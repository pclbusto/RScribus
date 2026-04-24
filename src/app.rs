use relm4::prelude::*;
use adw::prelude::*;
use gtk::gdk;
use std::collections::HashMap;
use std::rc::Rc;
use crate::document::{Document, Item, ItemType};
use crate::text_box::{TextBox, KeyAction};
use crate::image_box::{FitMode, ImageBox};

const SCALE: f64 = 3.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PageLayout {
    Vertical,
    Horizontal,
}

pub struct AppModel {
    document: Document,
    current_page: usize,
    drag_start: Option<(f64, f64)>,
    drag_current: Option<(f64, f64)>,
    selected_item_id: Option<String>,
    initial_item_rect: Option<(f64, f64, f64, f64)>,
    active_handle: Option<usize>,
    is_moving: bool,
    popover_pos: (f64, f64),
    popover_visible: bool,
    is_editing: bool,
    text_drag_active: bool,
    request_focus: bool,
    zoom: f64,
    create_frame_type: ItemType,
    image_surfaces: HashMap<String, Rc<cairo::ImageSurface>>,
    page_layout: PageLayout,
}

impl AppModel {
    fn scale(&self) -> f64 {
        SCALE * self.zoom
    }

    fn find_item(&self, id: &str) -> Option<(usize, Item)> {
        for (page_idx, page) in self.document.pages.iter().enumerate() {
            if let Some(item) = page.items.iter().find(|i| i.id == id) {
                return Some((page_idx, item.clone()));
            }
        }
        None
    }

    fn find_item_mut(&mut self, id: &str) -> Option<(usize, &mut Item)> {
        for (page_idx, page) in self.document.pages.iter_mut().enumerate() {
            if let Some(item) = page.items.iter_mut().find(|i| i.id == id) {
                return Some((page_idx, item));
            }
        }
        None
    }

    fn selected_item_type(&self) -> Option<ItemType> {
        let id = self.selected_item_id.as_ref()?;
        self.find_item(id).map(|(_, item)| item.item_type)
    }

    fn get_editing_text_box_mut(&mut self) -> Option<&mut TextBox> {
        let id = self.selected_item_id.clone()?;
        let (_, item) = self.find_item_mut(&id)?;
        (item.item_type == ItemType::TextFrame).then_some(&mut item.text_box)
    }

    fn get_page_offset(&self, page_idx: usize) -> (f64, f64) {
        let page_gap = 20.0;
        match self.page_layout {
            PageLayout::Vertical => {
                let y = page_idx as f64 * (self.document.height + page_gap);
                (0.0, y)
            }
            PageLayout::Horizontal => {
                let x = page_idx as f64 * (self.document.width + page_gap);
                (x, 0.0)
            }
        }
    }

    fn hit_test_all_pages(&self, x: f64, y: f64) -> Option<(usize, Item)> {
        let x_mm = x / self.scale();
        let y_mm = y / self.scale();

        for (page_idx, page) in self.document.pages.iter().enumerate().rev() {
            let (off_x, off_y) = self.get_page_offset(page_idx);
            let local_x = x_mm - off_x;
            let local_y = y_mm - off_y;

            if local_x >= 0.0 && local_x <= self.document.width &&
               local_y >= 0.0 && local_y <= self.document.height {
                for item in page.items.iter().rev() {
                    if local_x >= item.x && local_x <= item.x + item.width &&
                       local_y >= item.y && local_y <= item.y + item.height
                    {
                        return Some((page_idx, item.clone()));
                    }
                }
            }
        }
        None
    }
}

#[derive(Debug)]
pub enum AppInput {
    AddPage,
    DragStart(f64, f64),
    DragUpdate(f64, f64),
    DragEnd,
    SelectNone,
    RightClick(f64, f64),
    DoubleClick(f64, f64),
    ClosePopover,
    StartEdit,
    ExitEdit,
    TextKeyPressed(gdk::Key, gdk::ModifierType),
    PasteText(String),
    Zoom(f64),
    SetCreateFrameType(ItemType),
    ImportImage,
    ImageLoaded(String),
    FitFrameToImage,
    FitImageToFrame,
    SetImageFitMode(crate::image_box::FitMode),
    DeleteItem,
    SetPageLayout(PageLayout),
    ShowPreferences,
}

#[derive(Debug)]
pub enum AppOutput {}

#[relm4::component(pub)]
impl Component for AppModel {
    type Init = ();
    type Input = AppInput;
    type Output = AppOutput;
    type CommandOutput = ();

    view! {
        adw::ApplicationWindow {
            set_default_size: (1000, 700),
            set_visible: true,

            #[wrap(Some)]
            set_content = &gtk::Box {
                set_orientation: gtk::Orientation::Vertical,

                adw::HeaderBar {
                    #[wrap(Some)]
                    set_title_widget = &adw::WindowTitle {
                        #[watch]
                        set_title: &model.document.title,
                        set_subtitle: "RScribus",
                    },
                    pack_start = &gtk::Button {
                        set_icon_name: "list-add-symbolic",
                        set_tooltip_text: Some("Add Page"),
                        connect_clicked => AppInput::AddPage,
                    },
                    pack_end = &gtk::MenuButton {
                        set_icon_name: "open-menu-symbolic",
                        set_tooltip_text: Some("Menu"),
                        #[wrap(Some)]
                        set_popover = &gtk::Popover {
                            gtk::Box {
                                set_orientation: gtk::Orientation::Vertical,
                                set_spacing: 0,
                                set_margin_all: 0,

                                gtk::Button {
                                    set_label: "Vertical Layout",
                                    set_has_frame: false,
                                    connect_clicked[sender] => move |btn| {
                                        sender.input(AppInput::SetPageLayout(PageLayout::Vertical));
                                        btn.ancestor(gtk::Popover::static_type()).and_then(|p| p.downcast::<gtk::Popover>().ok()).map(|p| p.popdown());
                                    },
                                },
                                gtk::Button {
                                    set_label: "Horizontal Layout",
                                    set_has_frame: false,
                                    connect_clicked[sender] => move |btn| {
                                        sender.input(AppInput::SetPageLayout(PageLayout::Horizontal));
                                        btn.ancestor(gtk::Popover::static_type()).and_then(|p| p.downcast::<gtk::Popover>().ok()).map(|p| p.popdown());
                                    },
                                },
                                gtk::Separator {},
                                gtk::Button {
                                    set_label: "Preferences",
                                    set_has_frame: false,
                                    connect_clicked[sender] => move |btn| {
                                        sender.input(AppInput::ShowPreferences);
                                        btn.ancestor(gtk::Popover::static_type()).and_then(|p| p.downcast::<gtk::Popover>().ok()).map(|p| p.popdown());
                                    },
                                },
                            }
                        }
                    }
                },

                adw::OverlaySplitView {
                    set_sidebar_position: gtk::PackType::End,
                    set_show_sidebar: true,

                    #[wrap(Some)]
                    set_sidebar = &gtk::Box {
                        set_width_request: 250,
                        set_orientation: gtk::Orientation::Vertical,
                        set_spacing: 10,
                        set_margin_all: 10,

                        gtk::Label {
                            set_label: "Properties",
                            add_css_class: "title-4",
                        },
                        gtk::Separator {},
                        gtk::Label {
                            #[watch]
                            set_label: &format!("Pages: {}", model.document.pages.len()),
                        },
                        gtk::Label {
                            #[watch]
                            set_label: &format!("Size: {}x{}mm", model.document.width, model.document.height),
                        },
                        gtk::Separator {},
                        gtk::Label {
                            set_label: "Create Frame",
                            add_css_class: "caption",
                            set_xalign: 0.0,
                        },
                        gtk::Box {
                            set_orientation: gtk::Orientation::Horizontal,
                            set_spacing: 4,
                            add_css_class: "linked",

                            gtk::ToggleButton {
                                set_label: "Text",
                                #[watch]
                                set_active: model.create_frame_type == ItemType::TextFrame,
                                connect_toggled[sender] => move |btn| {
                                    if btn.is_active() {
                                        sender.input(AppInput::SetCreateFrameType(ItemType::TextFrame));
                                    }
                                },
                            },
                            gtk::ToggleButton {
                                set_label: "Image",
                                #[watch]
                                set_active: model.create_frame_type == ItemType::ImageFrame,
                                connect_toggled[sender] => move |btn| {
                                    if btn.is_active() {
                                        sender.input(AppInput::SetCreateFrameType(ItemType::ImageFrame));
                                    }
                                },
                            },
                        },
                        gtk::Separator {},
                        gtk::Label {
                            #[watch]
                            set_label: &format!("Selected: {}", model.selected_item_id.as_deref().unwrap_or("None")),
                            set_ellipsize: gtk::pango::EllipsizeMode::End,
                            add_css_class: "caption",
                        },
                        gtk::Button {
                            set_label: "Edit Text",
                            #[watch]
                            set_sensitive: model.selected_item_type() == Some(ItemType::TextFrame) && !model.is_editing,
                            #[watch]
                            set_visible: model.selected_item_type() == Some(ItemType::TextFrame),
                            connect_clicked => AppInput::StartEdit,
                        },
                        gtk::Button {
                            set_label: "Import Image",
                            #[watch]
                            set_visible: model.selected_item_type() == Some(ItemType::ImageFrame),
                            connect_clicked => AppInput::ImportImage,
                        },
                    },

                    #[wrap(Some)]
                    set_content = &gtk::ScrolledWindow {
                        set_hexpand: true,
                        set_vexpand: true,
                        add_css_class: "canvas-area",

                        gtk::Box {
                            set_halign: gtk::Align::Center,
                            set_valign: gtk::Align::Center,
                            set_margin_all: 40,

                            gtk::Overlay {
                                #[name = "canvas"]
                                gtk::DrawingArea {
                                    #[watch]
                                    set_content_width: {
                                        let page_gap = 20.0;
                                        let w = match model.page_layout {
                                            PageLayout::Vertical => model.document.width,
                                            PageLayout::Horizontal => {
                                                model.document.pages.len() as f64 * model.document.width + (model.document.pages.len().saturating_sub(1) as f64) * page_gap
                                            }
                                        };
                                        (w * model.scale()) as i32
                                    },
                                    #[watch]
                                    set_content_height: {
                                        let page_gap = 20.0;
                                        let h = match model.page_layout {
                                            PageLayout::Vertical => {
                                                model.document.pages.len() as f64 * model.document.height + (model.document.pages.len().saturating_sub(1) as f64) * page_gap
                                            }
                                            PageLayout::Horizontal => model.document.height,
                                        };
                                        (h * model.scale()) as i32
                                    },
                                    set_focusable: true,
                                    add_css_class: "card",

                                    #[watch]
                                    set_draw_func: {
                                        let doc = model.document.clone();
                                        let d_start = model.drag_start;
                                        let d_current = model.drag_current;
                                        let selected = model.selected_item_id.clone();
                                        let editing = model.is_editing;
                                        let zoom = model.zoom;
                                        let layout = model.page_layout;
                                        let images = model.image_surfaces.clone();
                                        move |_area, cr, _w, _h| {
                                            draw_canvas(cr, &doc, d_start, d_current, selected.clone(), editing, &images, zoom, layout);
                                        }
                                    },

                                    add_controller = gtk::GestureClick {
                                        set_button: 0,
                                        connect_pressed[sender] => move |gesture, n_press, x, y| {
                                            let btn = gesture.current_button();
                                            if n_press == 2 && btn == 1 {
                                                sender.input(AppInput::DoubleClick(x, y));
                                            } else if btn == 3 {
                                                sender.input(AppInput::RightClick(x, y));
                                            }
                                        }
                                    },

                                    add_controller = gtk::GestureDrag {
                                        connect_drag_begin[sender] => move |_gesture, x, y| {
                                            sender.input(AppInput::DragStart(x, y));
                                        },
                                        connect_drag_update[sender] => move |_gesture, offset_x, offset_y| {
                                            sender.input(AppInput::DragUpdate(offset_x, offset_y));
                                        },
                                        connect_drag_end[sender] => move |_gesture, _offset_x, _offset_y| {
                                            sender.input(AppInput::DragEnd);
                                        },
                                    },

                                    add_controller = gtk::EventControllerKey {
                                        connect_key_pressed[sender] => move |_ctrl, keyval, _keycode, state| {
                                            sender.input(AppInput::TextKeyPressed(keyval, state));
                                            gtk::glib::Propagation::Stop
                                        },
                                    },

                                    add_controller = gtk::EventControllerScroll {
                                        set_flags: gtk::EventControllerScrollFlags::VERTICAL,
                                        connect_scroll[sender] => move |ctrl, _dx, dy| {
                                            let state = ctrl.current_event().map(|e| e.modifier_state()).unwrap_or(gdk::ModifierType::empty());
                                            if state.contains(gdk::ModifierType::CONTROL_MASK) {
                                                sender.input(AppInput::Zoom(-dy));
                                                gtk::glib::Propagation::Stop
                                            } else {
                                                gtk::glib::Propagation::Proceed
                                            }
                                        }
                                    },
                                },

                                add_overlay = &gtk::Popover {
                                    set_autohide: true,
                                    #[watch]
                                    set_visible: model.popover_visible,
                                    #[watch]
                                    set_pointing_to: Some(&gtk::gdk::Rectangle::new(
                                        model.popover_pos.0 as i32,
                                        model.popover_pos.1 as i32,
                                        1, 1,
                                    )),
                                    connect_closed[sender] => move |_| {
                                        sender.input(AppInput::ClosePopover);
                                    },

                                    gtk::Box {
                                        set_orientation: gtk::Orientation::Vertical,
                                        set_spacing: 6,
                                        set_margin_all: 10,

                                        gtk::Label {
                                            set_label: "Frame Information",
                                            add_css_class: "title-4",
                                        },
                                        gtk::Separator {},
                                        gtk::Label {
                                            #[watch]
                                            set_label: &get_info_text(&model.document, &model.selected_item_id),
                                            set_xalign: 0.0,
                                        },

                                        // TextFrame actions
                                        gtk::Button {
                                            set_label: "Edit Text",
                                            add_css_class: "suggested-action",
                                            #[watch]
                                            set_visible: is_selected_type(&model.document, &model.selected_item_id, &ItemType::TextFrame),
                                            connect_clicked => AppInput::StartEdit,
                                        },

                                        // ImageFrame actions
                                        gtk::Button {
                                            set_label: "Import Image",
                                            add_css_class: "suggested-action",
                                            #[watch]
                                            set_visible: is_selected_type(&model.document, &model.selected_item_id, &ItemType::ImageFrame),
                                            connect_clicked => AppInput::ImportImage,
                                        },

                                        // Adjust Image section (only when ImageFrame has an image)
                                        gtk::Separator {
                                            #[watch]
                                            set_visible: selected_image_frame_has_image(&model.document, &model.selected_item_id),
                                        },
                                        gtk::Label {
                                            set_label: "Adjust Image",
                                            add_css_class: "heading",
                                            set_xalign: 0.0,
                                            #[watch]
                                            set_visible: selected_image_frame_has_image(&model.document, &model.selected_item_id),
                                        },
                                        gtk::Box {
                                            set_orientation: gtk::Orientation::Horizontal,
                                            set_spacing: 4,
                                            add_css_class: "linked",
                                            #[watch]
                                            set_visible: selected_image_frame_has_image(&model.document, &model.selected_item_id),

                                            gtk::ToggleButton {
                                                set_label: "Stretch",
                                                #[watch]
                                                set_active: get_selected_fit_mode(&model.document, &model.selected_item_id) == Some(crate::image_box::FitMode::ImageToFrame),
                                                connect_toggled[sender] => move |btn| {
                                                    if btn.is_active() {
                                                        sender.input(AppInput::SetImageFitMode(crate::image_box::FitMode::ImageToFrame));
                                                    }
                                                },
                                            },
                                            gtk::ToggleButton {
                                                set_label: "Proportional",
                                                #[watch]
                                                set_active: get_selected_fit_mode(&model.document, &model.selected_item_id) == Some(crate::image_box::FitMode::FrameToImage),
                                                connect_toggled[sender] => move |btn| {
                                                    if btn.is_active() {
                                                        sender.input(AppInput::SetImageFitMode(crate::image_box::FitMode::FrameToImage));
                                                        sender.input(AppInput::FitFrameToImage);
                                                    }
                                                },
                                            },
                                        },
                                        gtk::Button {
                                            set_label: "Reset to Original Size",
                                            #[watch]
                                            set_visible: selected_image_frame_has_image(&model.document, &model.selected_item_id),
                                            connect_clicked => AppInput::FitFrameToImage,
                                        },
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    fn init(
        _init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let model = AppModel {
            document: Document::default(),
            current_page: 0,
            drag_start: None,
            drag_current: None,
            selected_item_id: None,
            initial_item_rect: None,
            active_handle: None,
            is_moving: false,
            popover_pos: (0.0, 0.0),
            popover_visible: false,
            is_editing: false,
            text_drag_active: false,
            request_focus: false,
            zoom: 1.0,
            create_frame_type: ItemType::TextFrame,
            image_surfaces: HashMap::new(),
            page_layout: PageLayout::Vertical,
        };

        let widgets = view_output!();
        ComponentParts { model, widgets }
    }

    fn update_with_view(
        &mut self,
        widgets: &mut Self::Widgets,
        message: Self::Input,
        sender: ComponentSender<Self>,
        root: &Self::Root,
    ) {
        self.update(message, sender.clone(), root);
        self.update_view(widgets, sender);

        if self.request_focus {
            self.request_focus = false;
            widgets.canvas.grab_focus();
        }
    }

    fn update(&mut self, message: Self::Input, sender: ComponentSender<Self>, root: &Self::Root) {
        match message {
            AppInput::SetPageLayout(layout) => {
                self.page_layout = layout;
            }
            AppInput::ShowPreferences => {
                let dialog = adw::MessageDialog::builder()
                    .heading("Preferences")
                    .body("RScribus Preferences\n\n(This is a placeholder for actual preferences settings)")
                    .transient_for(root)
                    .build();
                dialog.add_response("close", "Close");
                dialog.present();
            }
            AppInput::AddPage => {
                self.document.pages.push(crate::document::Page::default());
            }
            AppInput::SelectNone => {
                self.selected_item_id = None;
            }
            AppInput::ClosePopover => {
                self.popover_visible = false;
            }
            AppInput::SetCreateFrameType(ft) => {
                self.create_frame_type = ft;
            }
            AppInput::StartEdit => {
                if self.selected_item_type() == Some(ItemType::TextFrame) {
                    if let Some(tb) = self.get_editing_text_box_mut() {
                        tb.cursor_pos = tb.text.len();
                        tb.selection_anchor = None;
                    }
                    self.is_editing = true;
                    self.request_focus = true;
                    self.popover_visible = false;
                }
            }
            AppInput::ExitEdit => {
                self.is_editing = false;
                if let Some(tb) = self.get_editing_text_box_mut() {
                    tb.selection_anchor = None;
                }
            }
            AppInput::ImportImage => {
                if self.selected_item_id.is_some() {
                    let dialog = gtk::FileDialog::new();
                    let filter = gtk::FileFilter::new();
                    filter.add_mime_type("image/*");
                    filter.set_name(Some("Images"));
                    let filters = gtk::gio::ListStore::new::<gtk::FileFilter>();
                    filters.append(&filter);
                    dialog.set_filters(Some(&filters));
                    dialog.set_default_filter(Some(&filter));

                    let s = sender.clone();
                    let win = root.clone();
                    gtk::glib::MainContext::default().spawn_local(async move {
                        if let Ok(file) = dialog.open_future(Some(&win)).await {
                            if let Some(path) = file.path() {
                                s.input(AppInput::ImageLoaded(
                                    path.to_string_lossy().to_string(),
                                ));
                            }
                        }
                    });
                }
            }
            AppInput::ImageLoaded(path) => {
                if let Some(surface) = ImageBox::load_surface(&path) {
                    self.image_surfaces.insert(path.clone(), Rc::new(surface));
                }
                if let Some(id) = self.selected_item_id.clone() {
                    if let Some((_, item)) = self.find_item_mut(&id) {
                        item.image_box.image_path = Some(path);
                    }
                }
                self.popover_visible = false;
            }
            AppInput::FitFrameToImage => {
                let id = self.selected_item_id.clone();
                let item_data = id.as_ref().and_then(|id| self.find_item(id));
                
                if let Some((_page_idx, item)) = item_data {
                    if let Some(path) = &item.image_box.image_path {
                        if let Some(surface) = self.image_surfaces.get(path) {
                            let img_w = surface.width() as f64;
                            let img_h = surface.height() as f64;
                            // Convert pixels → mm at 96 DPI
                            let w_mm = img_w * 25.4 / 96.0;
                            let h_mm = img_h * 25.4 / 96.0;
                            
                            // Re-fetch mutably to update
                            if let Some((_, item_mut)) = self.find_item_mut(&item.id) {
                                item_mut.width = w_mm;
                                item_mut.height = h_mm;
                                item_mut.image_box.fit_mode = FitMode::FrameToImage;
                            }
                        }
                    }
                }
                self.popover_visible = false;
            }
            AppInput::FitImageToFrame => {
                if let Some(id) = self.selected_item_id.clone() {
                    if let Some((_, item)) = self.find_item_mut(&id) {
                        item.image_box.fit_mode = FitMode::ImageToFrame;
                    }
                }
                self.popover_visible = false;
            }
            AppInput::SetImageFitMode(mode) => {
                if let Some(id) = self.selected_item_id.clone() {
                    if let Some((_, item)) = self.find_item_mut(&id) {
                        item.image_box.fit_mode = mode;
                    }
                }
            }
            AppInput::DeleteItem => {
                if let Some(id) = self.selected_item_id.clone() {
                    let mut found = None;
                    for (idx, page) in self.document.pages.iter().enumerate() {
                        if page.items.iter().any(|i| i.id == id) {
                            found = Some(idx);
                            break;
                        }
                    }
                    if let Some(page_idx) = found {
                        if let Some(page) = self.document.pages.get_mut(page_idx) {
                            page.items.retain(|item| item.id != id);
                            self.selected_item_id = None;
                        }
                    }
                }
            }
            AppInput::PasteText(text) => {
                if !self.is_editing { return; }
                if let Some(tb) = self.get_editing_text_box_mut() {
                    tb.insert_text(&text);
                }
            }
            AppInput::TextKeyPressed(key, state) => {
                if !self.is_editing {
                    if key == gdk::Key::Delete && self.selected_item_id.is_some() {
                        sender.input(AppInput::DeleteItem);
                    }
                    return;
                }
                let action = if let Some(tb) = self.get_editing_text_box_mut() {
                    tb.handle_key(key, state)
                } else {
                    return;
                };
                match action {
                    KeyAction::ExitEdit => {
                        self.is_editing = false;
                    }
                    KeyAction::RequestPaste => {
                        let s = sender.clone();
                        let clipboard = gdk::Display::default()
                            .expect("no display")
                            .clipboard();
                        gtk::glib::MainContext::default().spawn_local(async move {
                            if let Ok(Some(text)) = clipboard.read_text_future().await {
                                s.input(AppInput::PasteText(text.to_string()));
                            }
                        });
                    }
                    KeyAction::Handled => {}
                }
            }
            AppInput::Zoom(delta) => {
                self.zoom = (self.zoom + delta * 0.1).clamp(0.1, 5.0);
            }
            AppInput::DragStart(x, y) => {
                let x_mm = x / self.scale();
                let y_mm = y / self.scale();

                if self.is_editing {
                    if let Some((page_idx, item)) = self.hit_test_all_pages(x, y) {
                        if item.item_type == ItemType::TextFrame && Some(item.id.clone()) == self.selected_item_id {
                            let (off_x, off_y) = self.get_page_offset(page_idx);
                            let local_x = x_mm - off_x;
                            let local_y = y_mm - off_y;
                            let pos = item.text_box.hit_test(item.x, item.y, local_x, local_y, SCALE, item.width * SCALE);
                            
                            if let Some(tb) = self.get_editing_text_box_mut() {
                                tb.cursor_pos = pos;
                                tb.selection_anchor = Some(pos);
                            }
                            self.drag_start = Some((x, y));
                            self.text_drag_active = true;
                            self.current_page = page_idx;
                            return;
                        }
                    }
                    self.is_editing = false;
                    if let Some(tb) = self.get_editing_text_box_mut() {
                        tb.selection_anchor = None;
                    }
                }

                self.drag_start = Some((x, y));
                self.drag_current = Some((x, y));

                let handle_hit = if let Some(selected_id) = &self.selected_item_id {
                    if let Some((page_idx, item)) = self.find_item(selected_id) {
                        let (off_x, off_y) = self.get_page_offset(page_idx);
                        let local_x = x_mm - off_x;
                        let local_y = y_mm - off_y;
                        
                        let handles = get_handle_positions(&item);
                        let mut hit = None;
                        for (idx, (hx, hy)) in handles.iter().enumerate() {
                            if (local_x - hx).abs() < 2.0 && (local_y - hy).abs() < 2.0 {
                                hit = Some((idx, item.x, item.y, item.width, item.height, page_idx));
                                break;
                            }
                        }
                        hit
                    } else { None }
                } else { None };

                if let Some((idx, ix, iy, iw, ih, p_idx)) = handle_hit {
                    self.active_handle = Some(idx);
                    self.initial_item_rect = Some((ix, iy, iw, ih));
                    self.current_page = p_idx;
                    self.request_focus = true;
                    return;
                }

                if let Some((page_idx, item)) = self.hit_test_all_pages(x, y) {
                    self.selected_item_id = Some(item.id);
                    self.initial_item_rect = Some((item.x, item.y, item.width, item.height));
                    self.is_moving = true;
                    self.current_page = page_idx;
                    self.request_focus = true;
                } else {
                    self.selected_item_id = None;
                    self.initial_item_rect = None;
                    self.is_moving = false;
                    self.active_handle = None;
                    self.request_focus = true;
                    
                    // Determine current page based on layout
                    let page_gap = 20.0;
                    match self.page_layout {
                        PageLayout::Vertical => {
                            self.current_page = (y_mm / (self.document.height + page_gap)).floor() as usize;
                        }
                        PageLayout::Horizontal => {
                            self.current_page = (x_mm / (self.document.width + page_gap)).floor() as usize;
                        }
                    }
                    self.current_page = self.current_page.min(self.document.pages.len().saturating_sub(1));
                }
            }
            AppInput::DragUpdate(offset_x, offset_y) => {
                if self.text_drag_active {
                    if let Some((sx, sy)) = self.drag_start {
                        let x_mm = (sx + offset_x) / self.scale();
                        let y_mm = (sy + offset_y) / self.scale();

                        let hit_data = if let Some(id) = &self.selected_item_id {
                            self.find_item(id).map(|(page_idx, item)| {
                                let (off_x, off_y) = self.get_page_offset(page_idx);
                                let local_x = x_mm - off_x;
                                let local_y = y_mm - off_y;
                                item.text_box.hit_test(item.x, item.y, local_x, local_y, SCALE, item.width * SCALE)
                            })
                        } else { None };

                        if let Some(pos) = hit_data {
                            if let Some(tb) = self.get_editing_text_box_mut() {
                                tb.cursor_pos = pos;
                            }
                        }
                    }
                    return;
                }

                if let Some((sx, sy)) = self.drag_start {
                    self.drag_current = Some((sx + offset_x, sy + offset_y));
                    let dx = offset_x / self.scale();
                    let dy = offset_y / self.scale();
                    
                    let active_handle = self.active_handle;
                    let is_moving = self.is_moving;

                    if let (Some(id), Some((ix, iy, iw, ih))) = (self.selected_item_id.clone(), self.initial_item_rect) {
                        let mut item_moved_to_new_page = None;
                        let layout = self.page_layout;
                        let doc_w = self.document.width;
                        let doc_h = self.document.height;

                        // Pre-calculate image ratio if needed to avoid borrow checker issues
                        let image_ratio = if let Some((_, item)) = self.find_item(&id) {
                            if item.item_type == ItemType::ImageFrame && item.image_box.fit_mode == FitMode::FrameToImage {
                                item.image_box.image_path.as_ref().and_then(|path| {
                                    self.image_surfaces.get(path).map(|surf| surf.width() as f64 / surf.height() as f64)
                                })
                            } else { None }
                        } else { None };
                        
                        // First find the item and update its position locally
                        if let Some((page_idx, item)) = self.find_item_mut(&id) {
                            if let Some(handle_idx) = active_handle {
                                match handle_idx {
                                    0 => { item.x = ix + dx; item.y = iy + dy; item.width = iw - dx; item.height = ih - dy; }
                                    1 => { item.y = iy + dy; item.height = ih - dy; }
                                    2 => { item.y = iy + dy; item.width = iw + dx; item.height = ih - dy; }
                                    3 => { item.width = iw + dx; }
                                    4 => { item.width = iw + dx; item.height = ih + dy; }
                                    5 => { item.height = ih + dy; }
                                    6 => { item.x = ix + dx; item.width = iw - dx; item.height = ih + dy; }
                                    7 => { item.x = ix + dx; item.width = iw - dx; }
                                    _ => {}
                                }

                                // Apply proportional constraint if in FrameToImage mode
                                if let Some(ratio) = image_ratio {
                                    match handle_idx {
                                        3 | 7 | 4 | 6 => { // Width-driven or corners
                                            item.height = item.width / ratio;
                                            if handle_idx == 6 || handle_idx == 0 { // Bottom-Left or Top-Left
                                                // y might need adjustment if we want to keep it centered or similar, 
                                                // but for now simple width-based height adjustment.
                                            }
                                        }
                                        1 | 5 => { // Height-driven
                                            item.width = item.height * ratio;
                                        }
                                        0 | 2 => { // Top corners
                                            item.width = item.height * ratio;
                                            // Re-adjust X to maintain anchor if necessary
                                        }
                                        _ => {}
                                    }
                                }

                                if item.width < 1.0 { item.width = 1.0; }
                                if item.height < 1.0 { item.height = 1.0; }
                            } else if is_moving {
                                item.x = ix + dx;
                                item.y = iy + dy;
                                
                                // Check if item should move to another page
                                let page_gap = 20.0;
                                let (off_x, off_y) = match layout {
                                    PageLayout::Vertical => (0.0, page_idx as f64 * (doc_h + page_gap)),
                                    PageLayout::Horizontal => (page_idx as f64 * (doc_w + page_gap), 0.0),
                                };
                                let abs_x_mm = off_x + item.x + item.width / 2.0;
                                let abs_y_mm = off_y + item.y + item.height / 2.0;

                                let target_page_idx = match layout {
                                    PageLayout::Vertical => (abs_y_mm / (doc_h + page_gap)).floor() as usize,
                                    PageLayout::Horizontal => (abs_x_mm / (doc_w + page_gap)).floor() as usize,
                                };
                                let target_page_idx = target_page_idx.min(self.document.pages.len().saturating_sub(1));
                                
                                if target_page_idx != page_idx {
                                    item_moved_to_new_page = Some((page_idx, target_page_idx));
                                }
                            }
                        }

                        // If page change is needed, handle it here
                        if let Some((old_idx, new_idx)) = item_moved_to_new_page {
                            if let Some(old_page) = self.document.pages.get_mut(old_idx) {
                                if let Some(pos) = old_page.items.iter().position(|i| i.id == id) {
                                    let mut item = old_page.items.remove(pos);
                                    
                                    let (old_off_x, old_off_y) = self.get_page_offset(old_idx);
                                    let (new_off_x, new_off_y) = self.get_page_offset(new_idx);

                                    let abs_x = old_off_x + item.x;
                                    let abs_y = old_off_y + item.y;
                                    item.x = abs_x - new_off_x;
                                    item.y = abs_y - new_off_y;
                                    
                                    if let Some(new_page) = self.document.pages.get_mut(new_idx) {
                                        new_page.items.push(item);
                                        self.current_page = new_idx;
                                        if let Some((ix_ref, iy_ref, _, _)) = &mut self.initial_item_rect {
                                            let abs_ix = old_off_x + *ix_ref;
                                            let abs_iy = old_off_y + *iy_ref;
                                            *ix_ref = abs_ix - new_off_x;
                                            *iy_ref = abs_iy - new_off_y;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            AppInput::RightClick(x, y) => {
                if let Some((page_idx, item)) = self.hit_test_all_pages(x, y) {
                    self.selected_item_id = Some(item.id.clone());
                    self.current_page = page_idx;
                    self.popover_pos = (x, y);
                    self.popover_visible = true;
                } else {
                    self.popover_visible = false;
                }
            }
            AppInput::DoubleClick(x, y) => {
                let x_mm = x / self.scale();
                let y_mm = y / self.scale();

                if let Some((page_idx, item)) = self.hit_test_all_pages(x, y) {
                    let (off_x, off_y) = self.get_page_offset(page_idx);
                    let local_x = x_mm - off_x;
                    let local_y = y_mm - off_y;
                    let click_pos = item.text_box.hit_test(item.x, item.y, local_x, local_y, SCALE, item.width * SCALE);
                    let id = item.id.clone();
                    let item_type = item.item_type.clone();

                    match item_type {
                        ItemType::TextFrame => {
                            let already_editing = self.is_editing
                                && self.selected_item_id.as_deref() == Some(&id);
                            self.selected_item_id = Some(id);
                            self.current_page = page_idx;
                            self.is_editing = true;
                            self.request_focus = true;

                            if already_editing {
                                if let Some(tb) = self.get_editing_text_box_mut() {
                                    tb.select_word_at(click_pos);
                                }
                            } else {
                                if let Some(tb) = self.get_editing_text_box_mut() {
                                    tb.cursor_pos = click_pos;
                                    tb.selection_anchor = None;
                                }
                            }
                        }
                        ItemType::ImageFrame => {
                            self.selected_item_id = Some(id);
                            self.current_page = page_idx;
                            sender.input(AppInput::ImportImage);
                        }
                        _ => {}
                    }
                }
            }
            AppInput::DragEnd => {
                if self.text_drag_active {
                    self.text_drag_active = false;
                    self.drag_start = None;
                    self.drag_current = None;
                    return;
                }

                if !self.is_moving && self.active_handle.is_none() {
                    if let (Some((sx, sy)), Some((cx, cy))) = (self.drag_start, self.drag_current) {
                        let (off_x, off_y) = self.get_page_offset(self.current_page);

                        let x = (sx.min(cx) / self.scale()) - off_x;
                        let y = (sy.min(cy) / self.scale()) - off_y;
                        let width = (sx - cx).abs() / self.scale();
                        let height = (sy - cy).abs() / self.scale();
                        if width > 1.0 && height > 1.0 {
                            if let Some(page) = self.document.pages.get_mut(self.current_page) {
                                let new_id = uuid::Uuid::new_v4().to_string();
                                page.items.push(Item {
                                    id: new_id.clone(),
                                    x, y, width, height,
                                    rotation: 0.0,
                                    item_type: self.create_frame_type.clone(),
                                    text_box: TextBox::default(),
                                    image_box: ImageBox::default(),
                                });
                                self.selected_item_id = Some(new_id);
                            }
                        }
                    }
                }
                self.drag_start = None;
                self.drag_current = None;
                self.initial_item_rect = None;
                self.active_handle = None;
                self.is_moving = false;
                self.request_focus = true;
            }
        }
    }
}

fn is_selected_type(doc: &Document, selected_id: &Option<String>, ty: &ItemType) -> bool {
    selected_id.as_ref()
        .and_then(|id| {
            for page in &doc.pages {
                if let Some(item) = page.items.iter().find(|i| &i.id == id) {
                    return Some(&item.item_type == ty);
                }
            }
            None
        })
        .unwrap_or(false)
}

fn selected_image_frame_has_image(doc: &Document, selected_id: &Option<String>) -> bool {
    selected_id.as_ref()
        .and_then(|id| {
            for page in &doc.pages {
                if let Some(item) = page.items.iter().find(|i| &i.id == id) {
                    return Some(item.item_type == ItemType::ImageFrame && item.image_box.image_path.is_some());
                }
            }
            None
        })
        .unwrap_or(false)
}

fn get_selected_fit_mode(doc: &Document, selected_id: &Option<String>) -> Option<crate::image_box::FitMode> {
    selected_id.as_ref().and_then(|id| {
        for page in &doc.pages {
            if let Some(item) = page.items.iter().find(|i| &i.id == id) {
                return Some(item.image_box.fit_mode);
            }
        }
        None
    })
}

fn get_info_text(doc: &Document, selected_id: &Option<String>) -> String {
    if let Some(id) = selected_id {
        for page in &doc.pages {
            if let Some(item) = page.items.iter().find(|i| &i.id == id) {
                return match &item.item_type {
                    ItemType::TextFrame => {
                        let text = &item.text_box.text;
                        let paragraphs = text.split('\n').filter(|s| !s.is_empty()).count();
                        let words = text.split_whitespace().count();
                        let characters = text.len();
                        let lines = text.lines().count();
                        format!(
                            "Type: Text Frame\nSize: {:.1}x{:.1}mm\nParagraphs: {}\nLines: {}\nWords: {}\nCharacters: {}",
                            item.width, item.height, paragraphs, lines, words, characters
                        )
                    }
                    ItemType::ImageFrame => {
                        let image_info = item.image_box.image_path.as_ref()
                            .and_then(|p| std::path::Path::new(p).file_name())
                            .map(|n| format!("\nFile: {}", n.to_string_lossy()))
                            .unwrap_or_else(|| "\nNo image loaded".to_string());
                        format!("Type: Image Frame\nSize: {:.1}x{:.1}mm{}", item.width, item.height, image_info)
                    }
                    ItemType::Shape => format!("Type: Shape\nSize: {:.1}x{:.1}mm", item.width, item.height),
                };
            }
        }
    }
    "No item selected".to_string()
}

fn get_handle_positions(item: &Item) -> [(f64, f64); 8] {
    let (x, y, w, h) = (item.x, item.y, item.width, item.height);
    [
        (x,         y        ),
        (x + w/2.0, y        ),
        (x + w,     y        ),
        (x + w,     y + h/2.0),
        (x + w,     y + h    ),
        (x + w/2.0, y + h    ),
        (x,         y + h    ),
        (x,         y + h/2.0),
    ]
}


fn draw_canvas(
    cr: &gtk::cairo::Context,
    doc: &Document,
    drag_start: Option<(f64, f64)>,
    drag_current: Option<(f64, f64)>,
    selected_id: Option<String>,
    is_editing: bool,
    images: &HashMap<String, Rc<cairo::ImageSurface>>,
    zoom: f64,
    layout: PageLayout,
) {
    cr.save().unwrap();
    cr.scale(zoom, zoom);

    let page_gap = 20.0;
    for (page_idx, page) in doc.pages.iter().enumerate() {
        let (off_x, off_y) = match layout {
            PageLayout::Vertical => (0.0, page_idx as f64 * (doc.height + page_gap)),
            PageLayout::Horizontal => (page_idx as f64 * (doc.width + page_gap), 0.0),
        };

        cr.save().unwrap();
        cr.translate(off_x * SCALE, off_y * SCALE);

        cr.set_source_rgb(1.0, 1.0, 1.0);
        cr.rectangle(0.0, 0.0, doc.width * SCALE, doc.height * SCALE);
        cr.fill().unwrap();

        cr.set_source_rgb(0.8, 0.8, 0.8);
        cr.set_line_width(1.0);
        cr.rectangle(0.0, 0.0, doc.width * SCALE, doc.height * SCALE);
        cr.stroke().unwrap();

        cr.set_source_rgb(0.0, 0.5, 1.0);
        cr.set_dash(&[5.0, 5.0], 0.0);
        cr.rectangle(
            10.0 * SCALE, 10.0 * SCALE,
            (doc.width - 20.0) * SCALE, (doc.height - 20.0) * SCALE,
        );
        cr.stroke().unwrap();
        cr.set_dash(&[], 0.0);

        for item in &page.items {
            let is_selected = selected_id.as_ref() == Some(&item.id);
            let is_editing_this = is_selected && is_editing;

            cr.save().unwrap();
            cr.translate(item.x * SCALE, item.y * SCALE);
            cr.rotate(item.rotation.to_radians());

            match item.item_type {
                ItemType::TextFrame => {
                    draw_text_frame(cr, item, is_selected, is_editing_this);
                }
                ItemType::ImageFrame => {
                    let image = item.image_box.image_path.as_ref()
                        .and_then(|p| images.get(p))
                        .map(|rc| rc.as_ref());
                    draw_image_frame(cr, item, is_selected, image);
                }
                ItemType::Shape => {}
            }

            cr.restore().unwrap();

            if is_selected && !is_editing_this {
                let handles = get_handle_positions(item);
                cr.set_source_rgb(1.0, 1.0, 1.0);
                for (hx, hy) in &handles {
                    cr.rectangle(hx * SCALE - 3.0, hy * SCALE - 3.0, 6.0, 6.0);
                    cr.fill().unwrap();
                }
                cr.set_source_rgb(0.0, 0.5, 1.0);
                cr.set_line_width(1.0);
                for (hx, hy) in &handles {
                    cr.rectangle(hx * SCALE - 3.0, hy * SCALE - 3.0, 6.0, 6.0);
                    cr.stroke().unwrap();
                }
            }
        }
        cr.restore().unwrap();
    }
    cr.restore().unwrap();

    if drag_start.is_some() && selected_id.is_none() {
        if let (Some((sx, sy)), Some((cx, cy))) = (drag_start, drag_current) {
            cr.set_source_rgba(0.0, 0.5, 1.0, 0.3);
            cr.rectangle(sx.min(cx), sy.min(cy), (sx - cx).abs(), (sy - cy).abs());
            cr.fill().unwrap();
            cr.set_source_rgb(0.0, 0.5, 1.0);
            cr.set_line_width(1.0);
            cr.rectangle(sx.min(cx), sy.min(cy), (sx - cx).abs(), (sy - cy).abs());
            cr.stroke().unwrap();
        }
    }
}

fn draw_text_frame(
    cr: &gtk::cairo::Context,
    item: &Item,
    is_selected: bool,
    is_editing: bool,
) {
    let w = item.width * SCALE;
    let h = item.height * SCALE;
    item.text_box.render(cr, w, h, is_selected, is_editing);
}

fn draw_image_frame(
    cr: &gtk::cairo::Context,
    item: &Item,
    is_selected: bool,
    image: Option<&cairo::ImageSurface>,
) {
    let w = item.width * SCALE;
    let h = item.height * SCALE;
    item.image_box.render(cr, w, h, is_selected, image);
}
