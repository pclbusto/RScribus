use relm4::prelude::*;
use adw::prelude::*;
use gtk::gdk;
use std::collections::HashMap;
use std::rc::Rc;
use crate::document::{Document, Item, ItemType};

const SCALE: f64 = 3.0;
const TEXT_PAD: f64 = 4.0;

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
    cursor_pos: usize,
    selection_anchor: Option<usize>,
    just_started_editing: bool,
    zoom: f64,
    create_frame_type: ItemType,
    image_surfaces: HashMap<String, Rc<cairo::ImageSurface>>,
}

impl AppModel {
    fn scale(&self) -> f64 {
        SCALE * self.zoom
    }

    fn get_editing_text(&self) -> String {
        if let Some(id) = &self.selected_item_id {
            if let Some(page) = self.document.pages.get(self.current_page) {
                if let Some(item) = page.items.iter().find(|i| &i.id == id) {
                    return item.text.clone();
                }
            }
        }
        String::new()
    }

    fn set_editing_text(&mut self, text: String) {
        let id = self.selected_item_id.clone();
        if let Some(id) = id {
            if let Some(page) = self.document.pages.get_mut(self.current_page) {
                if let Some(item) = page.items.iter_mut().find(|i| i.id == id) {
                    item.text = text;
                }
            }
        }
    }

    fn selection_range(&self) -> Option<(usize, usize)> {
        let anchor = self.selection_anchor?;
        if anchor == self.cursor_pos {
            return None;
        }
        Some((anchor.min(self.cursor_pos), anchor.max(self.cursor_pos)))
    }

    fn delete_selection(&mut self) -> bool {
        if let Some((start, end)) = self.selection_range() {
            let mut text = self.get_editing_text();
            text.drain(start..end);
            self.cursor_pos = start;
            self.selection_anchor = None;
            self.set_editing_text(text);
            true
        } else {
            false
        }
    }

    fn selected_item_type(&self) -> Option<&ItemType> {
        let id = self.selected_item_id.as_ref()?;
        let page = self.document.pages.get(self.current_page)?;
        page.items.iter().find(|i| &i.id == id).map(|i| &i.item_type)
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
                            set_sensitive: model.selected_item_type() == Some(&ItemType::TextFrame) && !model.is_editing,
                            #[watch]
                            set_visible: model.selected_item_type() == Some(&ItemType::TextFrame),
                            connect_clicked => AppInput::StartEdit,
                        },
                        gtk::Button {
                            set_label: "Import Image",
                            #[watch]
                            set_visible: model.selected_item_type() == Some(&ItemType::ImageFrame),
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
                                    set_content_width: (model.document.width * model.scale()) as i32,
                                    #[watch]
                                    set_content_height: {
                                        let page_gap = 20.0;
                                        let total_h = model.document.pages.len() as f64 * model.document.height + (model.document.pages.len().saturating_sub(1) as f64) * page_gap;
                                        (total_h * model.scale()) as i32
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
                                        let cursor = model.cursor_pos;
                                        let selection = model.selection_range();
                                        let zoom = model.zoom;
                                        let images = model.image_surfaces.clone();
                                        move |_area, cr, _w, _h| {
                                            draw_canvas(cr, &doc, d_start, d_current, selected.clone(), editing, cursor, selection, &images, zoom);
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
                                            set_label: &get_info_text(&model.document, model.current_page, &model.selected_item_id),
                                            set_xalign: 0.0,
                                        },

                                        // TextFrame actions
                                        gtk::Button {
                                            set_label: "Edit Text",
                                            add_css_class: "suggested-action",
                                            #[watch]
                                            set_visible: is_selected_type(&model.document, model.current_page, &model.selected_item_id, &ItemType::TextFrame),
                                            connect_clicked => AppInput::StartEdit,
                                        },

                                        // ImageFrame actions
                                        gtk::Button {
                                            set_label: "Import Image",
                                            add_css_class: "suggested-action",
                                            #[watch]
                                            set_visible: is_selected_type(&model.document, model.current_page, &model.selected_item_id, &ItemType::ImageFrame),
                                            connect_clicked => AppInput::ImportImage,
                                        },

                                        // Adjust Image section (only when ImageFrame has an image)
                                        gtk::Separator {
                                            #[watch]
                                            set_visible: selected_image_frame_has_image(&model.document, model.current_page, &model.selected_item_id),
                                        },
                                        gtk::Label {
                                            set_label: "Adjust Image",
                                            add_css_class: "heading",
                                            set_xalign: 0.0,
                                            #[watch]
                                            set_visible: selected_image_frame_has_image(&model.document, model.current_page, &model.selected_item_id),
                                        },
                                        gtk::Button {
                                            set_label: "Frame to Image",
                                            #[watch]
                                            set_visible: selected_image_frame_has_image(&model.document, model.current_page, &model.selected_item_id),
                                            connect_clicked => AppInput::FitFrameToImage,
                                        },
                                        gtk::Button {
                                            set_label: "Image to Frame",
                                            #[watch]
                                            set_visible: selected_image_frame_has_image(&model.document, model.current_page, &model.selected_item_id),
                                            connect_clicked => AppInput::FitImageToFrame,
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
            cursor_pos: 0,
            selection_anchor: None,
            just_started_editing: false,
            zoom: 1.0,
            create_frame_type: ItemType::TextFrame,
            image_surfaces: HashMap::new(),
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

        if self.just_started_editing {
            self.just_started_editing = false;
            widgets.canvas.grab_focus();
        }
    }

    fn update(&mut self, message: Self::Input, sender: ComponentSender<Self>, root: &Self::Root) {
        match message {
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
                if self.selected_item_type() == Some(&ItemType::TextFrame) {
                    let text_len = self.get_editing_text().len();
                    self.cursor_pos = text_len;
                    self.selection_anchor = None;
                    self.is_editing = true;
                    self.just_started_editing = true;
                    self.popover_visible = false;
                }
            }
            AppInput::ExitEdit => {
                self.is_editing = false;
                self.selection_anchor = None;
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
                if let Some(surface) = load_image_surface(&path) {
                    self.image_surfaces.insert(path.clone(), Rc::new(surface));
                }
                if let Some(id) = self.selected_item_id.clone() {
                    if let Some(page) = self.document.pages.get_mut(self.current_page) {
                        if let Some(item) = page.items.iter_mut().find(|i| i.id == id) {
                            item.image_path = Some(path);
                        }
                    }
                }
                self.popover_visible = false;
            }
            AppInput::FitFrameToImage => {
                let id = self.selected_item_id.clone();
                let path = id.as_ref().and_then(|id| {
                    self.document.pages.get(self.current_page)
                        .and_then(|p| p.items.iter().find(|i| &i.id == id))
                        .and_then(|i| i.image_path.clone())
                });
                if let (Some(id), Some(path)) = (id, path) {
                    if let Some(surface) = self.image_surfaces.get(&path) {
                        let img_w = surface.width() as f64;
                        let img_h = surface.height() as f64;
                        // Convert pixels → mm at 96 DPI
                        let w_mm = img_w * 25.4 / 96.0;
                        let h_mm = img_h * 25.4 / 96.0;
                        if let Some(page) = self.document.pages.get_mut(self.current_page) {
                            if let Some(item) = page.items.iter_mut().find(|i| i.id == id) {
                                item.width = w_mm;
                                item.height = h_mm;
                            }
                        }
                    }
                }
                self.popover_visible = false;
            }
            AppInput::FitImageToFrame => {
                // Image already scales to fill the frame by default; just close popover.
                self.popover_visible = false;
            }
            AppInput::PasteText(text) => {
                if !self.is_editing { return; }
                self.delete_selection();
                let mut current = self.get_editing_text();
                current.insert_str(self.cursor_pos, &text);
                self.cursor_pos += text.len();
                self.set_editing_text(current);
            }
            AppInput::TextKeyPressed(key, state) => {
                if !self.is_editing { return; }

                let ctrl  = state.contains(gdk::ModifierType::CONTROL_MASK);
                let shift = state.contains(gdk::ModifierType::SHIFT_MASK);

                if ctrl {
                    match key {
                        gdk::Key::a | gdk::Key::A => {
                            let len = self.get_editing_text().len();
                            self.selection_anchor = Some(0);
                            self.cursor_pos = len;
                        }
                        gdk::Key::c | gdk::Key::C => {
                            if let Some((start, end)) = self.selection_range() {
                                let text = self.get_editing_text();
                                let selected = text[start..end].to_string();
                                gtk::gdk::Display::default()
                                    .expect("no display")
                                    .clipboard()
                                    .set_text(&selected);
                            }
                        }
                        gdk::Key::x | gdk::Key::X => {
                            if let Some((start, end)) = self.selection_range() {
                                let text = self.get_editing_text();
                                let selected = text[start..end].to_string();
                                gtk::gdk::Display::default()
                                    .expect("no display")
                                    .clipboard()
                                    .set_text(&selected);
                                self.delete_selection();
                            }
                        }
                        gdk::Key::v | gdk::Key::V => {
                            let s = sender.clone();
                            let clipboard = gtk::gdk::Display::default()
                                .expect("no display")
                                .clipboard();
                            gtk::glib::MainContext::default().spawn_local(async move {
                                if let Ok(Some(text)) = clipboard.read_text_future().await {
                                    s.input(AppInput::PasteText(text.to_string()));
                                }
                            });
                        }
                        _ => {}
                    }
                    return;
                }

                let text = self.get_editing_text();
                match key {
                    gdk::Key::Escape => {
                        self.is_editing = false;
                        self.selection_anchor = None;
                    }
                    gdk::Key::Left => {
                        if shift {
                            if self.selection_anchor.is_none() {
                                self.selection_anchor = Some(self.cursor_pos);
                            }
                            if self.cursor_pos > 0 {
                                self.cursor_pos = prev_char_boundary(&text, self.cursor_pos);
                            }
                        } else {
                            if self.selection_range().is_some() {
                                let (start, _) = self.selection_range().unwrap();
                                self.cursor_pos = start;
                            } else if self.cursor_pos > 0 {
                                self.cursor_pos = prev_char_boundary(&text, self.cursor_pos);
                            }
                            self.selection_anchor = None;
                        }
                    }
                    gdk::Key::Right => {
                        if shift {
                            if self.selection_anchor.is_none() {
                                self.selection_anchor = Some(self.cursor_pos);
                            }
                            if self.cursor_pos < text.len() {
                                self.cursor_pos = next_char_boundary(&text, self.cursor_pos);
                            }
                        } else {
                            if self.selection_range().is_some() {
                                let (_, end) = self.selection_range().unwrap();
                                self.cursor_pos = end;
                            } else if self.cursor_pos < text.len() {
                                self.cursor_pos = next_char_boundary(&text, self.cursor_pos);
                            }
                            self.selection_anchor = None;
                        }
                    }
                    gdk::Key::Home => {
                        if shift && self.selection_anchor.is_none() {
                            self.selection_anchor = Some(self.cursor_pos);
                        } else if !shift {
                            self.selection_anchor = None;
                        }
                        let before = &text[..self.cursor_pos];
                        self.cursor_pos = before.rfind('\n').map(|i| i + 1).unwrap_or(0);
                    }
                    gdk::Key::End => {
                        if shift && self.selection_anchor.is_none() {
                            self.selection_anchor = Some(self.cursor_pos);
                        } else if !shift {
                            self.selection_anchor = None;
                        }
                        let after = &text[self.cursor_pos..];
                        self.cursor_pos = after.find('\n')
                            .map(|i| self.cursor_pos + i)
                            .unwrap_or(text.len());
                    }
                    gdk::Key::BackSpace => {
                        if !self.delete_selection() && self.cursor_pos > 0 {
                            let prev = prev_char_boundary(&text, self.cursor_pos);
                            let mut new_text = text;
                            new_text.drain(prev..self.cursor_pos);
                            self.cursor_pos = prev;
                            self.set_editing_text(new_text);
                        }
                    }
                    gdk::Key::Delete => {
                        if !self.delete_selection() && self.cursor_pos < text.len() {
                            let next = next_char_boundary(&text, self.cursor_pos);
                            let mut new_text = text;
                            new_text.drain(self.cursor_pos..next);
                            self.set_editing_text(new_text);
                        }
                    }
                    gdk::Key::Return | gdk::Key::KP_Enter => {
                        self.delete_selection();
                        let mut new_text = self.get_editing_text();
                        new_text.insert(self.cursor_pos, '\n');
                        self.cursor_pos += 1;
                        self.set_editing_text(new_text);
                    }
                    _ => {
                        if let Some(ch) = key.to_unicode() {
                            if !ch.is_control() {
                                self.delete_selection();
                                let byte_len = ch.len_utf8();
                                let mut new_text = self.get_editing_text();
                                new_text.insert(self.cursor_pos, ch);
                                self.cursor_pos += byte_len;
                                self.set_editing_text(new_text);
                            }
                        }
                    }
                }
            }
            AppInput::Zoom(delta) => {
                self.zoom = (self.zoom + delta * 0.1).clamp(0.1, 5.0);
            }
            AppInput::DragStart(x, y) => {
                let page_gap = 20.0;
                let total_page_h_mm = self.document.height + page_gap;
                let raw_y_mm = y / self.scale();
                let page_idx = (raw_y_mm / total_page_h_mm).floor() as usize;
                let page_idx = page_idx.min(self.document.pages.len().saturating_sub(1));
                self.current_page = page_idx;
                let x_mm = x / self.scale();
                let y_mm = raw_y_mm - (page_idx as f64 * total_page_h_mm);

                if self.is_editing {
                    let hit = self.selected_item_id.as_ref()
                        .and_then(|id| self.document.pages.get(self.current_page)
                            .and_then(|page| page.items.iter().find(|i| &i.id == id)))
                        .filter(|item| item.item_type == ItemType::TextFrame)
                        .filter(|item| {
                            x_mm >= item.x && x_mm <= item.x + item.width &&
                            y_mm >= item.y && y_mm <= item.y + item.height
                        })
                        .map(|item| hit_test_layout(item, x_mm, y_mm));

                    match hit {
                        Some(pos) => {
                            self.cursor_pos = pos;
                            self.selection_anchor = None;
                        }
                        None => {
                            self.is_editing = false;
                            self.selection_anchor = None;
                        }
                    }
                    return;
                }

                self.drag_start = Some((x, y));
                self.drag_current = Some((x, y));

                if let Some(selected_id) = &self.selected_item_id {
                    if let Some(page) = self.document.pages.get(self.current_page) {
                        if let Some(item) = page.items.iter().find(|i| &i.id == selected_id) {
                            let handles = get_handle_positions(item);
                            for (idx, (hx, hy)) in handles.iter().enumerate() {
                                if (x_mm - hx).abs() < 2.0 && (y_mm - hy).abs() < 2.0 {
                                    self.active_handle = Some(idx);
                                    self.initial_item_rect = Some((item.x, item.y, item.width, item.height));
                                    return;
                                }
                            }
                        }
                    }
                }

                let mut found_hit = None;
                if let Some(page) = self.document.pages.get(self.current_page) {
                    for item in page.items.iter().rev() {
                        if x_mm >= item.x && x_mm <= item.x + item.width &&
                           y_mm >= item.y && y_mm <= item.y + item.height
                        {
                            found_hit = Some((item.id.clone(), item.x, item.y, item.width, item.height));
                            break;
                        }
                    }
                }

                if let Some((id, ix, iy, iw, ih)) = found_hit {
                    self.selected_item_id = Some(id);
                    self.initial_item_rect = Some((ix, iy, iw, ih));
                    self.is_moving = true;
                } else {
                    self.selected_item_id = None;
                    self.initial_item_rect = None;
                    self.is_moving = false;
                    self.active_handle = None;
                }
            }
            AppInput::DragUpdate(offset_x, offset_y) => {
                if let Some((sx, sy)) = self.drag_start {
                    self.drag_current = Some((sx + offset_x, sy + offset_y));
                    let dx = offset_x / self.scale();
                    let dy = offset_y / self.scale();
                    if let (Some(id), Some(page), Some((ix, iy, iw, ih))) = (
                        &self.selected_item_id,
                        self.document.pages.get_mut(self.current_page),
                        self.initial_item_rect,
                    ) {
                        if let Some(item) = page.items.iter_mut().find(|i| &i.id == id) {
                            if let Some(handle_idx) = self.active_handle {
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
                                if item.width < 1.0 { item.width = 1.0; }
                                if item.height < 1.0 { item.height = 1.0; }
                            } else if self.is_moving {
                                item.x = ix + dx;
                                item.y = iy + dy;
                            }
                        }
                    }
                }
            }
            AppInput::RightClick(x, y) => {
                let page_gap = 20.0;
                let total_page_h_mm = self.document.height + page_gap;
                let raw_y_mm = y / self.scale();
                let page_idx = (raw_y_mm / total_page_h_mm).floor() as usize;
                let page_idx = page_idx.min(self.document.pages.len().saturating_sub(1));
                self.current_page = page_idx;
                let x_mm = x / self.scale();
                let y_mm = raw_y_mm - (page_idx as f64 * total_page_h_mm);
                let mut found_hit = None;
                if let Some(page) = self.document.pages.get(self.current_page) {
                    for item in page.items.iter().rev() {
                        if x_mm >= item.x && x_mm <= item.x + item.width &&
                           y_mm >= item.y && y_mm <= item.y + item.height
                        {
                            found_hit = Some(item.id.clone());
                            break;
                        }
                    }
                }
                if let Some(id) = found_hit {
                    self.selected_item_id = Some(id);
                    self.popover_pos = (x, y);
                    self.popover_visible = true;
                } else {
                    self.popover_visible = false;
                }
            }
            AppInput::DoubleClick(x, y) => {
                let page_gap = 20.0;
                let total_page_h_mm = self.document.height + page_gap;
                let raw_y_mm = y / self.scale();
                let page_idx = (raw_y_mm / total_page_h_mm).floor() as usize;
                let page_idx = page_idx.min(self.document.pages.len().saturating_sub(1));
                self.current_page = page_idx;
                let x_mm = x / self.scale();
                let y_mm = raw_y_mm - (page_idx as f64 * total_page_h_mm);
                if let Some(page) = self.document.pages.get(self.current_page) {
                    if let Some(item) = page.items.iter().find(|item| {
                        x_mm >= item.x && x_mm <= item.x + item.width &&
                        y_mm >= item.y && y_mm <= item.y + item.height
                    }) {
                        match item.item_type {
                            ItemType::TextFrame => {
                                let text_len = item.text.len();
                                self.selected_item_id = Some(item.id.clone());
                                self.cursor_pos = text_len;
                                self.selection_anchor = None;
                                self.is_editing = true;
                                self.just_started_editing = true;
                            }
                            ItemType::ImageFrame => {
                                self.selected_item_id = Some(item.id.clone());
                                // Double-click on image frame opens import dialog
                                sender.input(AppInput::ImportImage);
                            }
                            _ => {}
                        }
                    }
                }
            }
            AppInput::DragEnd => {
                if !self.is_moving && self.active_handle.is_none() {
                    if let (Some((sx, sy)), Some((cx, cy))) = (self.drag_start, self.drag_current) {
                        let page_gap = 20.0;
                        let total_page_h_mm = self.document.height + page_gap;
                        let page_y_offset_mm = self.current_page as f64 * total_page_h_mm;

                        let x = sx.min(cx) / self.scale();
                        let y = (sy.min(cy) / self.scale()) - page_y_offset_mm;
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
                                    text: String::new(),
                                    image_path: None,
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
            }
        }
    }
}

fn prev_char_boundary(s: &str, pos: usize) -> usize {
    if pos == 0 { return 0; }
    let mut i = pos - 1;
    while i > 0 && !s.is_char_boundary(i) { i -= 1; }
    i
}

fn next_char_boundary(s: &str, pos: usize) -> usize {
    let mut i = pos + 1;
    while i < s.len() && !s.is_char_boundary(i) { i += 1; }
    i.min(s.len())
}

fn is_selected_type(doc: &Document, page_idx: usize, selected_id: &Option<String>, ty: &ItemType) -> bool {
    selected_id.as_ref()
        .and_then(|id| doc.pages.get(page_idx)
            .and_then(|p| p.items.iter().find(|i| &i.id == id)))
        .map(|i| &i.item_type == ty)
        .unwrap_or(false)
}

fn selected_image_frame_has_image(doc: &Document, page_idx: usize, selected_id: &Option<String>) -> bool {
    selected_id.as_ref()
        .and_then(|id| doc.pages.get(page_idx)
            .and_then(|p| p.items.iter().find(|i| &i.id == id)))
        .map(|i| i.item_type == ItemType::ImageFrame && i.image_path.is_some())
        .unwrap_or(false)
}

fn get_info_text(doc: &Document, page_idx: usize, selected_id: &Option<String>) -> String {
    if let Some(id) = selected_id {
        if let Some(page) = doc.pages.get(page_idx) {
            if let Some(item) = page.items.iter().find(|i| &i.id == id) {
                return match &item.item_type {
                    ItemType::TextFrame => {
                        let text = &item.text;
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
                        let image_info = item.image_path.as_ref()
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

fn load_image_surface(path: &str) -> Option<cairo::ImageSurface> {
    let texture = gdk::Texture::from_filename(path).ok()?;
    let w = texture.width();
    let h = texture.height();
    let stride = w as usize * 4;
    let mut data = vec![0u8; stride * h as usize];
    texture.download(&mut data, stride);
    // GDK_MEMORY_DEFAULT is B8G8R8A8_PREMULTIPLIED, which matches Cairo ARGB32 on little-endian.
    cairo::ImageSurface::create_for_data(data, cairo::Format::ARgb32, w, h, stride as i32).ok()
}

fn draw_canvas(
    cr: &gtk::cairo::Context,
    doc: &Document,
    drag_start: Option<(f64, f64)>,
    drag_current: Option<(f64, f64)>,
    selected_id: Option<String>,
    is_editing: bool,
    cursor_pos: usize,
    selection: Option<(usize, usize)>,
    images: &HashMap<String, Rc<cairo::ImageSurface>>,
    zoom: f64,
) {
    cr.save().unwrap();
    cr.scale(zoom, zoom);

    let page_gap = 20.0;
    for (page_idx, page) in doc.pages.iter().enumerate() {
        let page_y_offset = (doc.height + page_gap) * page_idx as f64;

        cr.save().unwrap();
        cr.translate(0.0, page_y_offset * SCALE);

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
            let item_selection = if is_editing_this { selection } else { None };

            cr.save().unwrap();
            cr.translate(item.x * SCALE, item.y * SCALE);
            cr.rotate(item.rotation.to_radians());

            match item.item_type {
                ItemType::TextFrame => {
                    draw_text_frame(cr, item, is_selected, is_editing_this, cursor_pos, item_selection);
                }
                ItemType::ImageFrame => {
                    let image = item.image_path.as_ref()
                        .and_then(|p| images.get(p))
                        .map(|rc| rc.as_ref());
                    draw_image_frame(cr, item, is_selected, image);
                }
                ItemType::Shape => {}
            }

            cr.restore().unwrap();

            if is_selected && !is_editing {
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
    cursor_pos: usize,
    selection: Option<(usize, usize)>,
) {
    let w = item.width * SCALE;
    let h = item.height * SCALE;

    if is_editing {
        cr.set_source_rgb(0.97, 0.97, 1.0);
        cr.rectangle(0.0, 0.0, w, h);
        cr.fill().unwrap();
    }

    if is_selected {
        cr.set_source_rgb(0.0, 0.5, 1.0);
        cr.set_line_width(2.0);
    } else {
        cr.set_source_rgb(0.3, 0.3, 0.3);
        cr.set_line_width(1.0);
    }
    cr.rectangle(0.0, 0.0, w, h);
    cr.stroke().unwrap();

    cr.rectangle(1.0, 1.0, w - 2.0, h - 2.0);
    cr.clip();

    let pscale = gtk::pango::SCALE as f64;
    let layout = pangocairo::functions::create_layout(cr);
    layout.set_text(&item.text);
    let font_desc = gtk::pango::FontDescription::from_string("Sans 11");
    layout.set_font_description(Some(&font_desc));
    layout.set_width(((w - 2.0 * TEXT_PAD) * pscale) as i32);
    layout.set_wrap(gtk::pango::WrapMode::Word);

    if let Some((sel_start, sel_end)) = selection {
        if sel_start < sel_end && sel_end <= item.text.len() {
            let attrs = gtk::pango::AttrList::new();

            let mut bg: gtk::pango::Attribute =
                gtk::pango::AttrColor::new_background(0x3535, 0x8484, 0xe4e4).into();
            bg.set_start_index(sel_start as u32);
            bg.set_end_index(sel_end as u32);
            attrs.insert(bg);

            let mut fg: gtk::pango::Attribute =
                gtk::pango::AttrColor::new_foreground(0xffff, 0xffff, 0xffff).into();
            fg.set_start_index(sel_start as u32);
            fg.set_end_index(sel_end as u32);
            attrs.insert(fg);

            layout.set_attributes(Some(&attrs));
        }
    }

    cr.set_source_rgb(0.1, 0.1, 0.1);
    cr.move_to(TEXT_PAD, TEXT_PAD);
    pangocairo::functions::show_layout(cr, &layout);

    if is_editing && selection.is_none() {
        let byte_idx = cursor_pos.min(item.text.len()) as i32;
        let (strong, _) = layout.cursor_pos(byte_idx);
        let cx = TEXT_PAD + strong.x() as f64 / pscale;
        let cy = TEXT_PAD + strong.y() as f64 / pscale;
        let ch = strong.height() as f64 / pscale;

        cr.set_source_rgb(0.1, 0.1, 0.9);
        cr.set_line_width(1.5);
        cr.move_to(cx, cy + 1.0);
        cr.line_to(cx, cy + ch - 1.0);
        cr.stroke().unwrap();
    }
}

fn draw_image_frame(
    cr: &gtk::cairo::Context,
    item: &Item,
    is_selected: bool,
    image: Option<&cairo::ImageSurface>,
) {
    let w = item.width * SCALE;
    let h = item.height * SCALE;

    // Background
    cr.set_source_rgb(0.88, 0.88, 0.88);
    cr.rectangle(0.0, 0.0, w, h);
    cr.fill().unwrap();

    if let Some(surf) = image {
        cr.save().unwrap();
        cr.rectangle(0.0, 0.0, w, h);
        cr.clip();

        let img_w = surf.width() as f64;
        let img_h = surf.height() as f64;
        let sx = w / img_w;
        let sy = h / img_h;
        cr.scale(sx, sy);
        cr.set_source_surface(surf, 0.0, 0.0).unwrap();
        cr.paint().unwrap();
        cr.restore().unwrap();
    } else {
        // Placeholder: diagonal hatch
        cr.save().unwrap();
        cr.rectangle(0.0, 0.0, w, h);
        cr.clip();
        cr.set_source_rgb(0.75, 0.75, 0.75);
        cr.set_line_width(1.0);
        let step = 12.0_f64;
        let diag = w + h;
        let mut i = -h;
        while i < diag {
            cr.move_to(i, 0.0);
            cr.line_to(i + h, h);
            i += step;
        }
        cr.stroke().unwrap();
        cr.restore().unwrap();

        // Centre label
        let layout = pangocairo::functions::create_layout(cr);
        layout.set_text("Image");
        layout.set_font_description(Some(&gtk::pango::FontDescription::from_string("Sans 10")));
        let (pw, ph) = layout.pixel_size();
        if pw as f64 <= w && ph as f64 <= h {
            cr.set_source_rgb(0.45, 0.45, 0.45);
            cr.move_to((w - pw as f64) / 2.0, (h - ph as f64) / 2.0);
            pangocairo::functions::show_layout(cr, &layout);
        }
    }

    // Border
    if is_selected {
        cr.set_source_rgb(0.0, 0.5, 1.0);
        cr.set_line_width(2.0);
    } else {
        cr.set_source_rgb(0.3, 0.3, 0.3);
        cr.set_line_width(1.0);
    }
    cr.rectangle(0.0, 0.0, w, h);
    cr.stroke().unwrap();
}

fn hit_test_layout(item: &Item, click_x_mm: f64, click_y_mm: f64) -> usize {
    use gtk::pango::prelude::FontMapExt;

    let pscale = gtk::pango::SCALE as f64;
    let frame_w_px = item.width * SCALE;

    let font_map = pangocairo::FontMap::default();
    let context = font_map.create_context();
    let layout = gtk::pango::Layout::new(&context);
    layout.set_text(&item.text);
    layout.set_font_description(Some(&gtk::pango::FontDescription::from_string("Sans 11")));
    layout.set_width(((frame_w_px - 2.0 * TEXT_PAD) * pscale) as i32);
    layout.set_wrap(gtk::pango::WrapMode::Word);

    let rel_x = (click_x_mm - item.x) * SCALE - TEXT_PAD;
    let rel_y = (click_y_mm - item.y) * SCALE - TEXT_PAD;
    let x_pango = (rel_x * pscale).max(0.0) as i32;
    let y_pango = (rel_y * pscale).max(0.0) as i32;

    let (_inside, byte_idx, trailing) = layout.xy_to_index(x_pango, y_pango);
    let byte_idx = byte_idx as usize;

    if trailing > 0 && byte_idx < item.text.len() {
        next_char_boundary(&item.text, byte_idx)
    } else {
        byte_idx
    }
}
