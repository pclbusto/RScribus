use relm4::prelude::*;
use adw::prelude::*;
use gtk::gdk;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use crate::document::{Document, Item, ItemContent, ItemType};
use crate::text_box::{TextBox, KeyAction, AttrValue, AttrSnapshot, TextAlign, TextAttribute};
use crate::image_box::{FitMode, ImageBox};
use crate::svg_box::SvgBox;
use crate::persistence::PersistenceManager;

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
    svg_handles: HashMap<String, Rc<rsvg::SvgHandle>>,
    svg_editor_path: Option<String>,
    page_layout: PageLayout,
    last_save_path: Option<String>,
    editing_flag: Rc<RefCell<bool>>,
    cursor_snapshot: AttrSnapshot,
    cursor_alignment: TextAlign,
    /// Monotone counter bumped on every text edit; used to discard stale reflow timeouts.
    reflow_version: u64,
    /// Counter for undo-commit debounce (typing grouping).
    undo_version: u64,
    /// True while the user is in an active typing run (keys after the first don't push).
    typing_run_active: bool,
    /// Active canvas drag-to-link operation.
    link_drag_active: bool,
    link_drag_source_id: Option<String>,
    /// Arrow start in screen px (bottom-right corner of source frame).
    link_drag_start: Option<(f64, f64)>,
    /// Current mouse position in screen px during link drag.
    link_drag_current: Option<(f64, f64)>,
}

impl AppModel {
    fn scale(&self) -> f64 {
        SCALE * self.zoom
    }

    fn refresh_cursor_snapshot(&mut self) {
        if self.selected_item_type() == Some(ItemType::TextFrame) {
            if let Some(id) = self.selected_item_id.clone() {
                if let Some((_, item)) = self.find_item(&id) {
                    if let ItemContent::Text(ref tb) = item.content {
                        let pos = if self.is_editing {
                            tb.selection_range().map(|(s, _)| s).unwrap_or(tb.cursor_pos)
                        } else {
                            0
                        };
                        self.cursor_snapshot = tb.effective_snapshot_at(pos);
                        self.cursor_alignment = tb.get_alignment();
                    }
                }
            }
        } else {
            self.cursor_snapshot = AttrSnapshot::default();
            self.cursor_alignment = TextAlign::default();
        }
    }

    /// Closes any active typing run and pushes the current state as an undo checkpoint.
    /// Call BEFORE a format change, paste, or cut so that undo can return to this state.
    fn flush_history(&mut self) {
        self.typing_run_active = false;
        if let Some(id) = self.selected_item_id.clone() {
            if let Some((_, item)) = self.find_item_mut(&id) {
                if let ItemContent::Text(ref mut tb) = item.content {
                    tb.push_history();
                }
            }
        }
    }

    /// Schedules a debounced history commit 400 ms after typing stops.
    fn schedule_history_commit(&mut self, sender: &relm4::ComponentSender<Self>) {
        self.undo_version = self.undo_version.wrapping_add(1);
        let version = self.undo_version;
        let s = sender.clone();
        gtk::glib::timeout_add_local(
            std::time::Duration::from_millis(400),
            move || {
                s.input(AppInput::MaybeCommitHistory(version));
                gtk::glib::ControlFlow::Break
            },
        );
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

    fn selected_has_link(&self) -> bool {
        self.selected_item_id.as_ref()
            .and_then(|id| self.find_item(id))
            .map(|(_, item)| {
                if let ItemContent::Text(tb) = &item.content {
                    tb.next_frame_id.is_some() || tb.prev_frame_id.is_some()
                } else { false }
            })
            .unwrap_or(false)
    }

    fn selected_item_type(&self) -> Option<ItemType> {
        let id = self.selected_item_id.as_ref()?;
        self.find_item(id).map(|(_, item)| item.content.item_type())
    }

    fn get_editing_text_box_mut(&mut self) -> Option<&mut TextBox> {
        let id = self.selected_item_id.clone()?;
        let (_, item) = self.find_item_mut(&id)?;
        match &mut item.content {
            ItemContent::Text(tb) => Some(tb),
            _ => None,
        }
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
    ImportSvg,
    SvgLoaded(String),
    ScrollText(f64),
    SetSvgFitMode(crate::svg_box::FitMode),
    FitFrameToSvg,
    RefreshSvg,
    ChooseSvgEditor,
    SvgEditorChosen(String),
    OpenExternalEditor,
    BringToFront,
    SendToBack,
    BringForward,
    SendBackward,
    DeleteItem,
    SetPageLayout(PageLayout),
    ShowPreferences,
    SaveProject,
    OpenProject,
    ExportPdf,
    ProjectSaved(String),
    ProjectLoaded(Document, String),
    SetBold(bool),
    SetItalic(bool),
    SetUnderline(bool),
    SetFontFamily(String),
    SetFontSize(f64),
    SetTextAlign(TextAlign),
    ClearFormat,
    LinkTo(String),
    UnlinkFrame,
    /// Fired by the debounce timer; ignored if `version` no longer matches.
    MaybeReflow(u64),
    Undo,
    Redo,
    CutText,
    /// Fired by the undo-commit debounce; ignored if version doesn't match.
    MaybeCommitHistory(u64),
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
                    pack_start = &gtk::Button {
                        set_icon_name: "document-open-symbolic",
                        set_tooltip_text: Some("Open Project"),
                        connect_clicked => AppInput::OpenProject,
                    },
                    pack_start = &gtk::Button {
                        set_icon_name: "document-save-symbolic",
                        set_tooltip_text: Some("Save Project"),
                        connect_clicked => AppInput::SaveProject,
                    },
                    pack_start = &gtk::Button {
                        set_icon_name: "printer-symbolic",
                        set_tooltip_text: Some("Export to PDF"),
                        connect_clicked => AppInput::ExportPdf,
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
                            gtk::ToggleButton {
                                set_label: "SVG",
                                #[watch]
                                set_active: model.create_frame_type == ItemType::SvgFrame,
                                connect_toggled[sender] => move |btn| {
                                    if btn.is_active() {
                                        sender.input(AppInput::SetCreateFrameType(ItemType::SvgFrame));
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

                        // ── Text formatting panel ─────────────────────────
                        gtk::Separator {
                            #[watch]
                            set_visible: model.selected_item_type() == Some(ItemType::TextFrame),
                        },
                        gtk::Label {
                            set_label: "Carácter",
                            add_css_class: "heading",
                            set_xalign: 0.0,
                            #[watch]
                            set_visible: model.selected_item_type() == Some(ItemType::TextFrame),
                        },
                        gtk::Box {
                            set_orientation: gtk::Orientation::Horizontal,
                            set_spacing: 4,
                            #[watch]
                            set_visible: model.selected_item_type() == Some(ItemType::TextFrame),
                            gtk::Box {
                                add_css_class: "linked",
                                set_orientation: gtk::Orientation::Horizontal,
                                gtk::ToggleButton {
                                    set_label: "B",
                                    set_tooltip_text: Some("Negrita (Ctrl+B)"),
                                    #[watch]
                                    set_active: model.cursor_snapshot.bold,
                                    connect_toggled[sender] => move |btn| {
                                        sender.input(AppInput::SetBold(btn.is_active()));
                                    },
                                },
                                gtk::ToggleButton {
                                    set_label: "I",
                                    set_tooltip_text: Some("Cursiva (Ctrl+I)"),
                                    #[watch]
                                    set_active: model.cursor_snapshot.italic,
                                    connect_toggled[sender] => move |btn| {
                                        sender.input(AppInput::SetItalic(btn.is_active()));
                                    },
                                },
                                gtk::ToggleButton {
                                    set_label: "U",
                                    set_tooltip_text: Some("Subrayado (Ctrl+U)"),
                                    #[watch]
                                    set_active: model.cursor_snapshot.underline,
                                    connect_toggled[sender] => move |btn| {
                                        sender.input(AppInput::SetUnderline(btn.is_active()));
                                    },
                                },
                            },
                            gtk::Button {
                                set_icon_name: "edit-clear-symbolic",
                                set_tooltip_text: Some("Limpiar formato"),
                                add_css_class: "flat",
                                connect_clicked[sender] => move |_| {
                                    sender.input(AppInput::ClearFormat);
                                },
                            },
                        },
                        gtk::Entry {
                            set_placeholder_text: Some("Fuente"),
                            set_tooltip_text: Some("Familia de fuente (Enter para aplicar)"),
                            #[watch]
                            set_visible: model.selected_item_type() == Some(ItemType::TextFrame),
                            #[watch]
                            set_text: model.cursor_snapshot.family.as_deref().unwrap_or(""),
                            connect_activate[sender] => move |entry| {
                                let f = entry.text().to_string();
                                if !f.is_empty() {
                                    sender.input(AppInput::SetFontFamily(f));
                                }
                            },
                        },
                        gtk::Box {
                            set_orientation: gtk::Orientation::Horizontal,
                            set_spacing: 6,
                            #[watch]
                            set_visible: model.selected_item_type() == Some(ItemType::TextFrame),
                            gtk::Label {
                                set_label: "Tamaño:",
                            },
                            gtk::SpinButton {
                                set_climb_rate: 1.0,
                                set_digits: 1,
                                set_range: (1.0, 200.0),
                                set_increments: (1.0, 10.0),
                                set_hexpand: true,
                                set_tooltip_text: Some("Tamaño de fuente (pt)"),
                                #[watch]
                                set_value: model.cursor_snapshot.size_pt.unwrap_or(11.0),
                                connect_value_changed[sender] => move |spin| {
                                    sender.input(AppInput::SetFontSize(spin.value()));
                                },
                            },
                        },
                        gtk::Label {
                            set_label: "Alineación",
                            add_css_class: "heading",
                            set_xalign: 0.0,
                            #[watch]
                            set_visible: model.selected_item_type() == Some(ItemType::TextFrame),
                        },
                        gtk::Box {
                            set_orientation: gtk::Orientation::Horizontal,
                            add_css_class: "linked",
                            #[watch]
                            set_visible: model.selected_item_type() == Some(ItemType::TextFrame),
                            gtk::ToggleButton {
                                set_icon_name: "format-justify-left-symbolic",
                                set_tooltip_text: Some("Alinear izquierda"),
                                #[watch]
                                set_active: matches!(model.cursor_alignment, TextAlign::Left),
                                connect_clicked[sender] => move |_| {
                                    sender.input(AppInput::SetTextAlign(TextAlign::Left));
                                },
                            },
                            gtk::ToggleButton {
                                set_icon_name: "format-justify-center-symbolic",
                                set_tooltip_text: Some("Centrar"),
                                #[watch]
                                set_active: matches!(model.cursor_alignment, TextAlign::Center),
                                connect_clicked[sender] => move |_| {
                                    sender.input(AppInput::SetTextAlign(TextAlign::Center));
                                },
                            },
                            gtk::ToggleButton {
                                set_icon_name: "format-justify-right-symbolic",
                                set_tooltip_text: Some("Alinear derecha"),
                                #[watch]
                                set_active: matches!(model.cursor_alignment, TextAlign::Right),
                                connect_clicked[sender] => move |_| {
                                    sender.input(AppInput::SetTextAlign(TextAlign::Right));
                                },
                            },
                        },

                        // ── Text chain panel ──────────────────────────────
                        gtk::Separator {
                            #[watch]
                            set_visible: model.selected_item_type() == Some(ItemType::TextFrame) && model.selected_has_link(),
                        },
                        gtk::Button {
                            set_label: "Desencadenar",
                            #[watch]
                            set_visible: model.selected_item_type() == Some(ItemType::TextFrame) && model.selected_has_link(),
                            connect_clicked => AppInput::UnlinkFrame,
                        },

                        gtk::Button {
                            set_label: "Import Image",
                            #[watch]
                            set_visible: model.selected_item_type() == Some(ItemType::ImageFrame),
                            connect_clicked => AppInput::ImportImage,
                        },
                        gtk::Button {
                            set_label: "Import SVG",
                            #[watch]
                            set_visible: model.selected_item_type() == Some(ItemType::SvgFrame),
                            connect_clicked => AppInput::ImportSvg,
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
                                        let svgs = model.svg_handles.clone();
                                        let link_drag = if model.link_drag_active {
                                            model.link_drag_start.zip(model.link_drag_current)
                                        } else { None };
                                        move |_area, cr, _w, _h| {
                                            draw_canvas(cr, &doc, d_start, d_current, selected.clone(), editing, &images, &svgs, zoom, layout, link_drag);
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
                                        connect_scroll[sender, editing_flag = model.editing_flag.clone()] => move |ctrl, _dx, dy| {
                                            let state = ctrl.current_event().map(|e| e.modifier_state()).unwrap_or(gdk::ModifierType::empty());
                                            if state.contains(gdk::ModifierType::CONTROL_MASK) {
                                                sender.input(AppInput::Zoom(-dy));
                                                gtk::glib::Propagation::Stop
                                            } else if *editing_flag.borrow() {
                                                sender.input(AppInput::ScrollText(dy));
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

                                        // SvgFrame actions
                                        gtk::Button {
                                            set_label: "Import SVG",
                                            add_css_class: "suggested-action",
                                            #[watch]
                                            set_visible: is_selected_type(&model.document, &model.selected_item_id, &ItemType::SvgFrame),
                                            connect_clicked => AppInput::ImportSvg,
                                        },

                                        // SVG Fit Mode section
                                        gtk::Separator {
                                            #[watch]
                                            set_visible: is_selected_type(&model.document, &model.selected_item_id, &ItemType::SvgFrame),
                                        },
                                        gtk::Label {
                                            set_label: "SVG Fit Mode",
                                            add_css_class: "heading",
                                            set_xalign: 0.0,
                                            #[watch]
                                            set_visible: is_selected_type(&model.document, &model.selected_item_id, &ItemType::SvgFrame),
                                        },
                                        gtk::Box {
                                            set_orientation: gtk::Orientation::Horizontal,
                                            set_spacing: 4,
                                            add_css_class: "linked",
                                            #[watch]
                                            set_visible: is_selected_type(&model.document, &model.selected_item_id, &ItemType::SvgFrame),

                                            gtk::ToggleButton {
                                                set_label: "Prop.",
                                                #[watch]
                                                set_active: get_selected_svg_fit_mode(&model.document, &model.selected_item_id) == Some(crate::svg_box::FitMode::Proportional),
                                                connect_toggled[sender] => move |btn| {
                                                    if btn.is_active() {
                                                        sender.input(AppInput::SetSvgFitMode(crate::svg_box::FitMode::Proportional));
                                                    }
                                                },
                                            },
                                            gtk::ToggleButton {
                                                set_label: "Original",
                                                #[watch]
                                                set_active: get_selected_svg_fit_mode(&model.document, &model.selected_item_id) == Some(crate::svg_box::FitMode::Original),
                                                connect_toggled[sender] => move |btn| {
                                                    if btn.is_active() {
                                                        sender.input(AppInput::SetSvgFitMode(crate::svg_box::FitMode::Original));
                                                    }
                                                },
                                            },
                                            gtk::ToggleButton {
                                                set_label: "Stretch",
                                                #[watch]
                                                set_active: get_selected_svg_fit_mode(&model.document, &model.selected_item_id) == Some(crate::svg_box::FitMode::Stretch),
                                                connect_toggled[sender] => move |btn| {
                                                    if btn.is_active() {
                                                        sender.input(AppInput::SetSvgFitMode(crate::svg_box::FitMode::Stretch));
                                                    }
                                                },
                                            },
                                        },
                                        gtk::Button {
                                            set_label: "Fit Frame to SVG",
                                            #[watch]
                                            set_visible: is_selected_type(&model.document, &model.selected_item_id, &ItemType::SvgFrame),
                                            connect_clicked => AppInput::FitFrameToSvg,
                                        },
                                        gtk::Button {
                                            set_label: "Refresh SVG",
                                            #[watch]
                                            set_visible: is_selected_type(&model.document, &model.selected_item_id, &ItemType::SvgFrame),
                                            connect_clicked => AppInput::RefreshSvg,
                                        },
                                        gtk::Button {
                                            set_label: "Open in External Editor",
                                            #[watch]
                                            set_visible: is_selected_type(&model.document, &model.selected_item_id, &ItemType::SvgFrame),
                                            connect_clicked => AppInput::OpenExternalEditor,
                                        },
                                        gtk::Button {
                                            #[watch]
                                            set_label: if model.svg_editor_path.is_some() { "Change SVG Editor" } else { "Set SVG Editor Path" },
                                            #[watch]
                                            set_visible: is_selected_type(&model.document, &model.selected_item_id, &ItemType::SvgFrame),
                                            connect_clicked => AppInput::ChooseSvgEditor,
                                        },
                                        gtk::Label {
                                            #[watch]
                                            set_label: &get_svg_path_info(&model.document, &model.selected_item_id),
                                            set_ellipsize: gtk::pango::EllipsizeMode::Middle,
                                            set_max_width_chars: 40,
                                            add_css_class: "caption",
                                            #[watch]
                                            set_visible: is_selected_type(&model.document, &model.selected_item_id, &ItemType::SvgFrame),
                                        },

                                        // Z-Order section
                                        gtk::Separator {
                                            #[watch]
                                            set_visible: model.selected_item_id.is_some(),
                                        },
                                        gtk::Label {
                                            set_label: "Z-Order",
                                            add_css_class: "heading",
                                            set_xalign: 0.0,
                                            #[watch]
                                            set_visible: model.selected_item_id.is_some(),
                                        },
                                        gtk::Box {
                                            set_orientation: gtk::Orientation::Horizontal,
                                            set_spacing: 4,
                                            add_css_class: "linked",
                                            #[watch]
                                            set_visible: model.selected_item_id.is_some(),

                                            gtk::Button {
                                                set_icon_name: "go-top-symbolic",
                                                set_tooltip_text: Some("Bring to Front"),
                                                connect_clicked => AppInput::BringToFront,
                                            },
                                            gtk::Button {
                                                set_icon_name: "go-up-symbolic",
                                                set_tooltip_text: Some("Bring Forward"),
                                                connect_clicked => AppInput::BringForward,
                                            },
                                            gtk::Button {
                                                set_icon_name: "go-down-symbolic",
                                                set_tooltip_text: Some("Send Backward"),
                                                connect_clicked => AppInput::SendBackward,
                                            },
                                            gtk::Button {
                                                set_icon_name: "go-bottom-symbolic",
                                                set_tooltip_text: Some("Send to Back"),
                                                connect_clicked => AppInput::SendToBack,
                                            },
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
            svg_handles: HashMap::new(),
            svg_editor_path: None,
            page_layout: PageLayout::Vertical,
            last_save_path: None,
            editing_flag: Rc::new(RefCell::new(false)),
            cursor_snapshot: AttrSnapshot::default(),
            cursor_alignment: TextAlign::default(),
            reflow_version: 0,
            undo_version: 0,
            typing_run_active: false,
            link_drag_active: false,
            link_drag_source_id: None,
            link_drag_start: None,
            link_drag_current: None,
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
        self.refresh_cursor_snapshot();
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
                        tb.scroll_y = 0.0;
                    }
                    self.is_editing = true;
                    *self.editing_flag.borrow_mut() = true;
                    self.request_focus = true;
                    self.popover_visible = false;

                    let item_dims = self.selected_item_id.as_ref()
                        .and_then(|id| self.find_item(id))
                        .map(|(_, item)| (item.width * SCALE, item.height * SCALE));
                    if let Some((w, h)) = item_dims {
                        let pango_ctx = make_pango_ctx();
                        if let Some(tb) = self.get_editing_text_box_mut() {
                            let s = tb.ensure_cursor_visible(&pango_ctx, w, h, SCALE);
                            tb.scroll_y = s;
                        }
                    }
                }
            }
            AppInput::ExitEdit => {
                self.is_editing = false;
                *self.editing_flag.borrow_mut() = false;
                if let Some(tb) = self.get_editing_text_box_mut() {
                    tb.selection_anchor = None;
                    tb.scroll_y = 0.0;
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
                        if let ItemContent::Image(ib) = &mut item.content {
                            ib.image_path = Some(path);
                        }
                    }
                }
                self.popover_visible = false;
            }
            AppInput::FitFrameToImage => {
                let id = self.selected_item_id.clone();
                let item_data = id.as_ref().and_then(|id| self.find_item(id));

                if let Some((_page_idx, item)) = item_data {
                    if let ItemContent::Image(ib) = &item.content {
                        if let Some(path) = &ib.image_path {
                            if let Some(surface) = self.image_surfaces.get(path) {
                                let img_w = surface.width() as f64;
                                let img_h = surface.height() as f64;
                                let w_mm = img_w * 25.4 / 96.0;
                                let h_mm = img_h * 25.4 / 96.0;

                                if let Some((_, item_mut)) = self.find_item_mut(&item.id) {
                                    item_mut.width = w_mm;
                                    item_mut.height = h_mm;
                                    if let ItemContent::Image(ib_mut) = &mut item_mut.content {
                                        ib_mut.fit_mode = FitMode::FrameToImage;
                                    }
                                }
                            }
                        }
                    }
                }
                self.popover_visible = false;
            }
            AppInput::FitImageToFrame => {
                if let Some(id) = self.selected_item_id.clone() {
                    if let Some((_, item)) = self.find_item_mut(&id) {
                        if let ItemContent::Image(ib) = &mut item.content {
                            ib.fit_mode = FitMode::ImageToFrame;
                        }
                    }
                }
                self.popover_visible = false;
            }
            AppInput::SetImageFitMode(mode) => {
                if let Some(id) = self.selected_item_id.clone() {
                    let mut aspect_ratio = None;
                    
                    // 1. Get aspect ratio if we are switching to Proportional
                    if mode == FitMode::FrameToImage {
                        if let Some((_, item)) = self.find_item(&id) {
                            if let ItemContent::Image(ib) = &item.content {
                                if let Some(path) = &ib.image_path {
                                    if let Some(surface) = self.image_surfaces.get(path) {
                                        aspect_ratio = Some(surface.width() as f64 / surface.height() as f64);
                                    }
                                }
                            }
                        }
                    }

                    // 2. Apply mode and adjust dimensions if proportional
                    if let Some((_, item)) = self.find_item_mut(&id) {
                        if let ItemContent::Image(ib) = &mut item.content {
                            ib.fit_mode = mode;
                            if let Some(ratio) = aspect_ratio {
                                if ratio > 0.0 {
                                    item.height = item.width / ratio;
                                }
                            }
                        }
                    }
                }
            }
            AppInput::ImportSvg => {
                if self.selected_item_id.is_some() {
                    let dialog = gtk::FileDialog::new();
                    let filter = gtk::FileFilter::new();
                    filter.add_suffix("svg");
                    filter.set_name(Some("SVG Images"));
                    let filters = gtk::gio::ListStore::new::<gtk::FileFilter>();
                    filters.append(&filter);
                    dialog.set_filters(Some(&filters));
                    dialog.set_default_filter(Some(&filter));

                    let s = sender.clone();
                    let win = root.clone();
                    gtk::glib::MainContext::default().spawn_local(async move {
                        if let Ok(file) = dialog.open_future(Some(&win)).await {
                            if let Some(path) = file.path() {
                                s.input(AppInput::SvgLoaded(
                                    path.to_string_lossy().to_string(),
                                ));
                            }
                        }
                    });
                }
            }
            AppInput::SvgLoaded(path) => {
                match rsvg::Loader::new().read_path(&path) {
                    Ok(handle) => {
                        self.svg_handles.insert(path.clone(), Rc::new(handle));
                        if let Some(id) = self.selected_item_id.clone() {
                            if let Some((_, item)) = self.find_item_mut(&id) {
                                if let ItemContent::Svg(sb) = &mut item.content {
                                    sb.svg_path = path;
                                }
                            }
                        }
                    }
                    Err(e) => eprintln!("Failed to load SVG: {}", e),
                }
                self.popover_visible = false;
            }
            AppInput::SetSvgFitMode(mode) => {
                if let Some(id) = self.selected_item_id.clone() {
                    if let Some((_, item)) = self.find_item_mut(&id) {
                        if let ItemContent::Svg(sb) = &mut item.content {
                            sb.fit_mode = mode;
                        }
                    }
                }
            }
            AppInput::FitFrameToSvg => {
                let id = self.selected_item_id.clone();
                let item_data = id.as_ref().and_then(|id| self.find_item(id));

                if let Some((_page_idx, item)) = item_data {
                    if let ItemContent::Svg(sb) = &item.content {
                        if !sb.svg_path.is_empty() {
                            let (w_mm, h_mm) = crate::svg_box::SvgBox::intrinsic_size(&sb.svg_path);
                            if let Some((_, item_mut)) = self.find_item_mut(&item.id) {
                                item_mut.width = w_mm;
                                item_mut.height = h_mm;
                            }
                        }
                    }
                }
                self.popover_visible = false;
            }
            AppInput::RefreshSvg => {
                if let Some(id) = self.selected_item_id.clone() {
                    if let Some((_, item)) = self.find_item(&id) {
                        if let ItemContent::Svg(sb) = &item.content {
                            let path = sb.svg_path.clone();
                            if !path.is_empty() {
                                match rsvg::Loader::new().read_path(&path) {
                                    Ok(handle) => {
                                        self.svg_handles.insert(path, Rc::new(handle));
                                    }
                                    Err(e) => eprintln!("Failed to refresh SVG: {}", e),
                                }
                            }
                        }
                    }
                }
                self.popover_visible = false;
            }
            AppInput::ChooseSvgEditor => {
                let dialog = gtk::FileDialog::new();
                dialog.set_title("Choose SVG Editor Executable");
                let s = sender.clone();
                let win = root.clone();
                gtk::glib::MainContext::default().spawn_local(async move {
                    if let Ok(file) = dialog.open_future(Some(&win)).await {
                        if let Some(path) = file.path() {
                            s.input(AppInput::SvgEditorChosen(path.to_string_lossy().to_string()));
                        }
                    }
                });
            }
            AppInput::SvgEditorChosen(path) => {
                self.svg_editor_path = Some(path);
            }
            AppInput::OpenExternalEditor => {
                if let Some(id) = self.selected_item_id.clone() {
                    if let Some((_, item)) = self.find_item(&id) {
                        if let ItemContent::Svg(sb) = &item.content {
                            let path = sb.svg_path.clone();
                            if !path.is_empty() {
                                if let Some(editor) = &self.svg_editor_path {
                                    if let Err(e) = std::process::Command::new(editor).arg(&path).spawn() {
                                        eprintln!("Failed to open SVG with custom editor: {}", e);
                                    }
                                } else {
                                    if let Err(e) = open::that(&path) {
                                        eprintln!("Failed to open SVG in external editor: {}", e);
                                    }
                                }
                            }
                        }
                    }
                }
                self.popover_visible = false;
            }
            AppInput::BringToFront => {
                if let Some(id) = self.selected_item_id.clone() {
                    for page in &mut self.document.pages {
                        if let Some(pos) = page.items.iter().position(|i| i.id == id) {
                            let item = page.items.remove(pos);
                            page.items.push(item);
                            break;
                        }
                    }
                }
            }
            AppInput::SendToBack => {
                if let Some(id) = self.selected_item_id.clone() {
                    for page in &mut self.document.pages {
                        if let Some(pos) = page.items.iter().position(|i| i.id == id) {
                            let item = page.items.remove(pos);
                            page.items.insert(0, item);
                            break;
                        }
                    }
                }
            }
            AppInput::BringForward => {
                if let Some(id) = self.selected_item_id.clone() {
                    for page in &mut self.document.pages {
                        if let Some(pos) = page.items.iter().position(|i| i.id == id) {
                            if pos + 1 < page.items.len() {
                                page.items.swap(pos, pos + 1);
                            }
                            break;
                        }
                    }
                }
            }
            AppInput::SendBackward => {
                if let Some(id) = self.selected_item_id.clone() {
                    for page in &mut self.document.pages {
                        if let Some(pos) = page.items.iter().position(|i| i.id == id) {
                            if pos > 0 {
                                page.items.swap(pos, pos - 1);
                            }
                            break;
                        }
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
                self.flush_history(); // push pre-paste state, close typing run
                if let Some(tb) = self.get_editing_text_box_mut() {
                    tb.insert_text(&text);
                }
                if let Some(id) = self.selected_item_id.clone() {
                    if is_in_chain(&self.document, &id) {
                        self.reflow_version = self.reflow_version.wrapping_add(1);
                        let version = self.reflow_version;
                        let s = sender.clone();
                        gtk::glib::timeout_add_local(
                            std::time::Duration::from_millis(250),
                            move || {
                                s.input(AppInput::MaybeReflow(version));
                                gtk::glib::ControlFlow::Break
                            },
                        );
                    }
                }
                let item_dims = self.selected_item_id.as_ref()
                    .and_then(|id| self.find_item(id))
                    .map(|(_, item)| (item.width * SCALE, item.height * SCALE));
                if let Some((w, h)) = item_dims {
                    let pango_ctx = make_pango_ctx();
                    if let Some(tb) = self.get_editing_text_box_mut() {
                        let s = tb.ensure_cursor_visible(&pango_ctx, w, h, SCALE);
                        tb.scroll_y = s;
                    }
                }
            }
            AppInput::TextKeyPressed(key, state) => {
                if !self.is_editing {
                    if key == gdk::Key::Delete && self.selected_item_id.is_some() {
                        sender.input(AppInput::DeleteItem);
                    }
                    return;
                }

                // Classify whether this key will modify text BEFORE calling handle_key,
                // so we can push the pre-edit state to the undo stack first.
                let ctrl = state.contains(gdk::ModifierType::CONTROL_MASK);
                let will_modify = !ctrl && (
                    matches!(key,
                        gdk::Key::BackSpace | gdk::Key::Delete |
                        gdk::Key::Return | gdk::Key::KP_Enter
                    ) || key.to_unicode().map_or(false, |c| !c.is_control())
                );

                // Start of a new typing run: push "state before run" to undo stack.
                if will_modify && !self.typing_run_active {
                    self.typing_run_active = true;
                    if let Some(tb) = self.get_editing_text_box_mut() {
                        tb.push_history();
                    }
                }

                let action = if let Some(tb) = self.get_editing_text_box_mut() {
                    tb.handle_key(key, state)
                } else {
                    return;
                };

                match action {
                    KeyAction::ExitEdit => {
                        self.typing_run_active = false;
                        self.is_editing = false;
                        *self.editing_flag.borrow_mut() = false;
                        if let Some(tb) = self.get_editing_text_box_mut() {
                            tb.selection_anchor = None;
                            tb.scroll_y = 0.0;
                        }
                        return;
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
                    KeyAction::RequestCut => {
                        sender.input(AppInput::CutText);
                    }
                    KeyAction::MoveVertical { up, extend } => {
                        let frame_w_px = self.selected_item_id.as_ref()
                            .and_then(|id| self.find_item(id))
                            .map(|(_, item)| item.width * SCALE)
                            .unwrap_or(0.0);
                        if let Some(tb) = self.get_editing_text_box_mut() {
                            tb.move_cursor_vertical(up, extend, frame_w_px, SCALE);
                        }
                    }
                    KeyAction::FormatBold => {
                        sender.input(AppInput::SetBold(!self.cursor_snapshot.bold));
                    }
                    KeyAction::FormatItalic => {
                        sender.input(AppInput::SetItalic(!self.cursor_snapshot.italic));
                    }
                    KeyAction::FormatUnderline => {
                        sender.input(AppInput::SetUnderline(!self.cursor_snapshot.underline));
                    }
                    KeyAction::Undo => {
                        sender.input(AppInput::Undo);
                        return;
                    }
                    KeyAction::Redo => {
                        sender.input(AppInput::Redo);
                        return;
                    }
                    KeyAction::Handled => {}
                }

                if will_modify {
                    self.schedule_history_commit(&sender);
                }

                if let Some(id) = self.selected_item_id.clone() {
                    if is_in_chain(&self.document, &id) {
                        self.reflow_version = self.reflow_version.wrapping_add(1);
                        let version = self.reflow_version;
                        let s = sender.clone();
                        gtk::glib::timeout_add_local(
                            std::time::Duration::from_millis(250),
                            move || {
                                s.input(AppInput::MaybeReflow(version));
                                gtk::glib::ControlFlow::Break
                            },
                        );
                    }
                }
                let item_dims = self.selected_item_id.as_ref()
                    .and_then(|id| self.find_item(id))
                    .map(|(_, item)| (item.width * SCALE, item.height * SCALE));
                if let Some((w, h)) = item_dims {
                    let pango_ctx = make_pango_ctx();
                    if let Some(tb) = self.get_editing_text_box_mut() {
                        let s = tb.ensure_cursor_visible(&pango_ctx, w, h, SCALE);
                        tb.scroll_y = s;
                    }
                }
            }
            AppInput::Zoom(delta) => {
                self.zoom = (self.zoom + delta * 0.1).clamp(0.1, 5.0);
            }
            AppInput::ScrollText(dy) => {
                if !self.is_editing { return; }
                let item_dims = self.selected_item_id.as_ref()
                    .and_then(|id| self.find_item(id))
                    .map(|(_, item)| (item.width * SCALE, item.height * SCALE));
                if let Some((w, h)) = item_dims {
                    let pango_ctx = make_pango_ctx();
                    if let Some(tb) = self.get_editing_text_box_mut() {
                        let total_h = tb.required_height(&pango_ctx, w, SCALE);
                        let max_scroll = (total_h - h).max(0.0);
                        tb.scroll_y = (tb.scroll_y + dy * 40.0).clamp(0.0, max_scroll);
                    }
                }
            }
            AppInput::DragStart(x, y) => {
                let x_mm = x / self.scale();
                let y_mm = y / self.scale();

                // Check if the user clicked the link-out button on any text frame
                if !self.is_editing {
                    'link_check: for (page_idx, page) in self.document.pages.iter().enumerate() {
                        let (off_x, off_y) = self.get_page_offset(page_idx);
                        let local_x = x_mm - off_x;
                        let local_y = y_mm - off_y;
                        for item in &page.items {
                            if let ItemContent::Text(tb) = &item.content {
                                if tb.next_frame_id.is_some() { continue; }
                                let pango_ctx = make_pango_ctx();
                                let w_px = item.width * SCALE;
                                let h_px = item.height * SCALE;
                                if tb.required_height(&pango_ctx, w_px, SCALE) > h_px
                                    && hit_link_button_mm(item, local_x, local_y)
                                {
                                    self.link_drag_active = true;
                                    self.link_drag_source_id = Some(item.id.clone());
                                    self.selected_item_id = Some(item.id.clone());
                                    let sc = self.scale();
                                    let sx = (off_x + item.x + item.width) * sc;
                                    let sy = (off_y + item.y + item.height) * sc;
                                    self.link_drag_start = Some((sx, sy));
                                    self.link_drag_current = Some((x, y));
                                    return;
                                }
                            }
                        }
                    }
                }

                if self.is_editing {
                    if let Some((page_idx, item)) = self.hit_test_all_pages(x, y) {
                        if item.content.item_type() == ItemType::TextFrame && Some(item.id.clone()) == self.selected_item_id {
                            let (off_x, off_y) = self.get_page_offset(page_idx);
                            let local_x = x_mm - off_x;
                            let local_y = y_mm - off_y;
                            let pos = if let ItemContent::Text(ref tb) = item.content {
                                tb.hit_test(item.x, item.y, local_x, local_y, SCALE, item.width * SCALE)
                            } else { 0 };
                            
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
                    *self.editing_flag.borrow_mut() = false;
                    if let Some(tb) = self.get_editing_text_box_mut() {
                        tb.selection_anchor = None;
                        tb.scroll_y = 0.0;
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
                if self.link_drag_active {
                    if let Some((sx, sy)) = self.link_drag_start {
                        self.link_drag_current = Some((sx + offset_x, sy + offset_y));
                    }
                    return;
                }
                if self.text_drag_active {
                    if let Some((sx, sy)) = self.drag_start {
                        let x_mm = (sx + offset_x) / self.scale();
                        let y_mm = (sy + offset_y) / self.scale();

                        let hit_data = if let Some(id) = &self.selected_item_id {
                            self.find_item(id).map(|(page_idx, item)| {
                                let (off_x, off_y) = self.get_page_offset(page_idx);
                                let local_x = x_mm - off_x;
                                let local_y = y_mm - off_y;
                                let pos = if let ItemContent::Text(ref tb) = item.content {
                                    tb.hit_test(item.x, item.y, local_x, local_y, SCALE, item.width * SCALE)
                                } else { 0 };
                                (pos, item.width * SCALE, item.height * SCALE)
                            })
                        } else { None };

                        if let Some((pos, w, h)) = hit_data {
                            let pango_ctx = make_pango_ctx();
                            if let Some(tb) = self.get_editing_text_box_mut() {
                                tb.cursor_pos = pos;
                                let s = tb.ensure_cursor_visible(&pango_ctx, w, h, SCALE);
                                tb.scroll_y = s;
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
                            if let ItemContent::Image(ib) = &item.content {
                                if ib.fit_mode == FitMode::FrameToImage {
                                    ib.image_path.as_ref().and_then(|path| {
                                        self.image_surfaces.get(path).map(|surf| surf.width() as f64 / surf.height() as f64)
                                    })
                                } else { None }
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
                                // Only move once the pointer has dragged more than 5 canvas pixels.
                                // Prevents items from drifting on a click (which may fire drag_update
                                // with tiny jitter, especially on first gesture after a document load).
                                if offset_x * offset_x + offset_y * offset_y >= 25.0 {
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

                // Check for double click on handles
                if let Some(selected_id) = self.selected_item_id.clone() {
                    if let Some((page_idx, item)) = self.find_item(&selected_id) {
                        let (off_x, off_y) = self.get_page_offset(page_idx);
                        let local_x = x_mm - off_x;
                        let local_y = y_mm - off_y;
                        
                        let handles = get_handle_positions(&item);
                        // Handle 5 is bottom center
                        let (hx, hy) = handles[5]; 
                        
                        if (local_x - hx).abs() < 2.0 && (local_y - hy).abs() < 2.0 {
                            if let ItemContent::Text(ref tb) = item.content {
                                let font_map = pangocairo::FontMap::default();
                                let pango_ctx = font_map.create_context();
                                pangocairo::functions::context_set_resolution(&pango_ctx, 25.4 * SCALE);
                                
                                let required_height = tb.required_height(&pango_ctx, item.width * SCALE, SCALE);
                                
                                if let Some((_, item_mut)) = self.find_item_mut(&selected_id) {
                                    item_mut.height = required_height / SCALE;
                                }
                                return;
                            }
                        }
                    }
                }

                if let Some((page_idx, item)) = self.hit_test_all_pages(x, y) {
                    let (off_x, off_y) = self.get_page_offset(page_idx);
                    let local_x = x_mm - off_x;
                    let local_y = y_mm - off_y;
                    let click_pos = if let ItemContent::Text(ref tb) = item.content {
                        tb.hit_test(item.x, item.y, local_x, local_y, SCALE, item.width * SCALE)
                    } else { 0 };
                    let id = item.id.clone();
                    let item_type = item.content.item_type();

                    match item_type {
                        ItemType::TextFrame => {
                            let already_editing = self.is_editing
                                && self.selected_item_id.as_deref() == Some(&id);
                            self.selected_item_id = Some(id);
                            self.current_page = page_idx;
                            self.is_editing = true;
                            *self.editing_flag.borrow_mut() = true;
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
                        ItemType::SvgFrame => {
                            self.selected_item_id = Some(id);
                            self.current_page = page_idx;
                            sender.input(AppInput::OpenExternalEditor);
                        }
                        _ => {}
                    }
                }
            }
            AppInput::DragEnd => {
                if self.link_drag_active {
                    // Try to link to whichever text frame is under the cursor
                    if let Some((cx, cy)) = self.link_drag_current {
                        if let Some((_, target)) = self.hit_test_all_pages(cx, cy) {
                            if target.content.item_type() == ItemType::TextFrame
                                && self.link_drag_source_id.as_deref() != Some(&target.id)
                            {
                                let tid = target.id.clone();
                                sender.input(AppInput::LinkTo(tid));
                            }
                        }
                    }
                    self.link_drag_active = false;
                    self.link_drag_source_id = None;
                    self.link_drag_start = None;
                    self.link_drag_current = None;
                    return;
                }
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
                                    content: match self.create_frame_type {
                                        ItemType::TextFrame => ItemContent::Text(TextBox::default()),
                                        ItemType::ImageFrame => ItemContent::Image(ImageBox::default()),
                                        ItemType::SvgFrame => ItemContent::Svg(SvgBox::default()),
                                        ItemType::Shape => ItemContent::Shape,
                                    },
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
            AppInput::SaveProject => {
                let dialog = gtk::FileDialog::new();
                let filter = gtk::FileFilter::new();
                filter.add_pattern("*.rsp");
                filter.set_name(Some("RScribus Project"));
                let filters = gtk::gio::ListStore::new::<gtk::FileFilter>();
                filters.append(&filter);
                dialog.set_filters(Some(&filters));
                dialog.set_default_filter(Some(&filter));

                if let Some(path) = &self.last_save_path {
                    let file = gtk::gio::File::for_path(path);
                    dialog.set_initial_file(Some(&file));
                }

                let s = sender.clone();
                let win = root.clone();
                let doc = self.document.clone();
                gtk::glib::MainContext::default().spawn_local(async move {
                    if let Ok(file) = dialog.save_future(Some(&win)).await {
                        if let Some(path) = file.path() {
                            let mut path_str = path.to_string_lossy().to_string();
                            if !path_str.ends_with(".rsp") {
                                path_str.push_str(".rsp");
                            }
                            let save_path = std::path::Path::new(&path_str);
                            if let Ok(_) = PersistenceManager::save_project(&doc, save_path) {
                                s.input(AppInput::ProjectSaved(path_str));
                            }
                        }
                    }
                });
            }
            AppInput::ProjectSaved(path) => {
                self.last_save_path = Some(path);
                
                let dialog = adw::MessageDialog::builder()
                    .heading("Project Saved")
                    .body("The project has been successfully saved.")
                    .transient_for(root)
                    .build();
                dialog.add_response("ok", "OK");
                dialog.present();
            }
            AppInput::OpenProject => {
                let dialog = gtk::FileDialog::new();
                let filter = gtk::FileFilter::new();
                filter.add_pattern("*.rsp");
                filter.set_name(Some("RScribus Project"));
                let filters = gtk::gio::ListStore::new::<gtk::FileFilter>();
                filters.append(&filter);
                dialog.set_filters(Some(&filters));
                dialog.set_default_filter(Some(&filter));

                let s = sender.clone();
                let win = root.clone();
                gtk::glib::MainContext::default().spawn_local(async move {
                    if let Ok(file) = dialog.open_future(Some(&win)).await {
                        if let Some(path) = file.path() {
                            let temp_dir = std::env::temp_dir().join("rscribus_extracted");
                            if let Ok((doc, _)) = PersistenceManager::load_project(&path, &temp_dir) {
                                s.input(AppInput::ProjectLoaded(doc, path.to_string_lossy().to_string()));
                            }
                        }
                    }
                });
            }
            AppInput::ExportPdf => {
                let dialog = gtk::FileDialog::new();
                let filter = gtk::FileFilter::new();
                filter.add_pattern("*.pdf");
                filter.set_name(Some("PDF Document"));
                let filters = gtk::gio::ListStore::new::<gtk::FileFilter>();
                filters.append(&filter);
                dialog.set_filters(Some(&filters));
                dialog.set_default_filter(Some(&filter));

                let s = sender.clone();
                let win = root.clone();
                let doc = self.document.clone();
                let images = self.image_surfaces.clone();
                let svgs = self.svg_handles.clone();
                gtk::glib::MainContext::default().spawn_local(async move {
                    if let Ok(file) = dialog.save_future(Some(&win)).await {
                        if let Some(path) = file.path() {
                            let mut path_str = path.to_string_lossy().to_string();
                            if !path_str.ends_with(".pdf") {
                                path_str.push_str(".pdf");
                            }
                            if let Ok(_) = export_to_pdf(&doc, &images, &svgs, &path_str) {
                                let dialog = adw::MessageDialog::builder()
                                    .heading("Export Successful")
                                    .body("The document has been exported to PDF.")
                                    .transient_for(&win)
                                    .build();
                                dialog.add_response("ok", "OK");
                                dialog.present();
                            }
                        }
                    }
                });
            }
            AppInput::ProjectLoaded(doc, path) => {
                self.document = doc;
                self.last_save_path = Some(path);
                self.selected_item_id = None;
                self.is_editing = false;
                self.drag_start = None;
                self.drag_current = None;
                self.is_moving = false;
                self.active_handle = None;
                self.initial_item_rect = None;
                self.text_drag_active = false;
                self.link_drag_active = false;
                self.link_drag_source_id = None;
                self.link_drag_start = None;
                self.link_drag_current = None;
                self.current_page = 0;

                // Pre-load all images from the loaded document
                for page in &self.document.pages {
                    for item in &page.items {
                        if let ItemContent::Image(ib) = &item.content {
                            if let Some(path) = &ib.image_path {
                                if !self.image_surfaces.contains_key(path) {
                                    if let Some(surface) = ImageBox::load_surface(path) {
                                        self.image_surfaces.insert(path.clone(), Rc::new(surface));
                                    }
                                }
                            }
                        }
                        if let ItemContent::Svg(sb) = &item.content {
                            if !sb.svg_path.is_empty() && !self.svg_handles.contains_key(&sb.svg_path) {
                                if let Ok(handle) = rsvg::Loader::new().read_path(&sb.svg_path) {
                                    self.svg_handles.insert(sb.svg_path.clone(), Rc::new(handle));
                                }
                            }
                        }
                    }
                }
            }
            AppInput::SetBold(value) => {
                let editing = self.is_editing;
                self.flush_history(); // push pre-format state, close typing run
                let mut need_enter_edit = false;
                if let Some(id) = self.selected_item_id.clone() {
                    if let Some((_, item)) = self.find_item_mut(&id) {
                        if let ItemContent::Text(ref mut tb) = item.content {
                            if editing {
                                let pos = tb.selection_range().map(|(s, _)| s).unwrap_or(tb.cursor_pos);
                                if tb.get_attr_at(pos).bold != value {
                                    let (s, e) = tb.selection_or_word_range();
                                    if s < e { tb.apply_format(s, e, AttrValue::Bold(value)); }
                                }
                            } else {
                                let len = tb.text.len();
                                tb.apply_format(0, len, AttrValue::Bold(value));
                                need_enter_edit = true;
                            }
                        }
                    }
                }
                if need_enter_edit {
                    self.is_editing = true;
                    *self.editing_flag.borrow_mut() = true;
                }
                self.request_focus = true;
            }
            AppInput::SetItalic(value) => {
                let editing = self.is_editing;
                self.flush_history(); // push pre-format state, close typing run
                let mut need_enter_edit = false;
                if let Some(id) = self.selected_item_id.clone() {
                    if let Some((_, item)) = self.find_item_mut(&id) {
                        if let ItemContent::Text(ref mut tb) = item.content {
                            if editing {
                                let pos = tb.selection_range().map(|(s, _)| s).unwrap_or(tb.cursor_pos);
                                if tb.get_attr_at(pos).italic != value {
                                    let (s, e) = tb.selection_or_word_range();
                                    if s < e { tb.apply_format(s, e, AttrValue::Italic(value)); }
                                }
                            } else {
                                let len = tb.text.len();
                                tb.apply_format(0, len, AttrValue::Italic(value));
                                need_enter_edit = true;
                            }
                        }
                    }
                }
                if need_enter_edit {
                    self.is_editing = true;
                    *self.editing_flag.borrow_mut() = true;
                }
                self.request_focus = true;
            }
            AppInput::SetUnderline(value) => {
                let editing = self.is_editing;
                self.flush_history(); // push pre-format state, close typing run
                let mut need_enter_edit = false;
                if let Some(id) = self.selected_item_id.clone() {
                    if let Some((_, item)) = self.find_item_mut(&id) {
                        if let ItemContent::Text(ref mut tb) = item.content {
                            if editing {
                                let pos = tb.selection_range().map(|(s, _)| s).unwrap_or(tb.cursor_pos);
                                if tb.get_attr_at(pos).underline != value {
                                    let (s, e) = tb.selection_or_word_range();
                                    if s < e { tb.apply_format(s, e, AttrValue::Underline(value)); }
                                }
                            } else {
                                let len = tb.text.len();
                                tb.apply_format(0, len, AttrValue::Underline(value));
                                need_enter_edit = true;
                            }
                        }
                    }
                }
                if need_enter_edit {
                    self.is_editing = true;
                    *self.editing_flag.borrow_mut() = true;
                }
                self.request_focus = true;
            }
            AppInput::SetFontFamily(family) => {
                let editing = self.is_editing;
                self.flush_history(); // push pre-format state, close typing run
                if let Some(id) = self.selected_item_id.clone() {
                    if let Some((_, item)) = self.find_item_mut(&id) {
                        if let ItemContent::Text(ref mut tb) = item.content {
                            if editing {
                                let (s, e) = tb.selection_or_word_range();
                                if s < e {
                                    tb.apply_format(s, e, AttrValue::Family(family));
                                }
                            } else {
                                let len = tb.text.len();
                                tb.apply_format(0, len, AttrValue::Family(family));
                                self.is_editing = true;
                                *self.editing_flag.borrow_mut() = true;
                            }
                        }
                    }
                }
                self.request_focus = true;
            }
            AppInput::SetFontSize(size) => {
                let editing = self.is_editing;
                self.flush_history(); // push pre-format state, close typing run
                if let Some(id) = self.selected_item_id.clone() {
                    if let Some((_, item)) = self.find_item_mut(&id) {
                        if let ItemContent::Text(ref mut tb) = item.content {
                            if editing {
                                let pos = tb.selection_range().map(|(s, _)| s).unwrap_or(tb.cursor_pos);
                                let current = tb.effective_snapshot_at(pos).size_pt.unwrap_or(11.0);
                                if (size - current).abs() > 0.05 {
                                    let (s, e) = tb.selection_or_word_range();
                                    if s < e {
                                        tb.apply_format(s, e, AttrValue::Size(size));
                                    }
                                }
                            } else {
                                let len = tb.text.len();
                                tb.apply_format(0, len, AttrValue::Size(size));
                                self.is_editing = true;
                                *self.editing_flag.borrow_mut() = true;
                            }
                        }
                    }
                }
                self.request_focus = true;
            }
            AppInput::SetTextAlign(align) => {
                self.flush_history(); // push pre-format state, close typing run
                if let Some(id) = self.selected_item_id.clone() {
                    if let Some((_, item)) = self.find_item_mut(&id) {
                        if let ItemContent::Text(ref mut tb) = item.content {
                            tb.set_alignment(align);
                            if !self.is_editing {
                                self.is_editing = true;
                                *self.editing_flag.borrow_mut() = true;
                            }
                        }
                    }
                }
                self.request_focus = true;
            }
            AppInput::ClearFormat => {
                let editing = self.is_editing;
                self.flush_history(); // push pre-format state, close typing run
                if let Some(id) = self.selected_item_id.clone() {
                    if let Some((_, item)) = self.find_item_mut(&id) {
                        if let ItemContent::Text(ref mut tb) = item.content {
                            if editing {
                                let (s, e) = tb.selection_or_word_range();
                                if s < e {
                                    tb.clear_format(s, e);
                                }
                            } else {
                                let len = tb.text.len();
                                tb.clear_format(0, len);
                                self.is_editing = true;
                                *self.editing_flag.borrow_mut() = true;
                            }
                        }
                    }
                }
                self.request_focus = true;
            }
            AppInput::MaybeReflow(version) => {
                if version == self.reflow_version {
                    if let Some(id) = self.selected_item_id.clone() {
                        if is_in_chain(&self.document, &id) {
                            let sc = self.scale();
                            reflow_chain(&mut self.document, &id, sc);
                        }
                    }
                }
            }
            AppInput::MaybeCommitHistory(version) => {
                if version == self.undo_version {
                    self.typing_run_active = false;
                }
            }
            AppInput::CutText => {
                if !self.is_editing { return; }
                self.flush_history(); // push pre-cut state, close typing run
                if let Some(tb) = self.get_editing_text_box_mut() {
                    tb.delete_selection();
                }
                // trigger reflow if in chain
                if let Some(id) = self.selected_item_id.clone() {
                    if is_in_chain(&self.document, &id) {
                        let sc = self.scale();
                        reflow_chain(&mut self.document, &id, sc);
                    }
                }
            }
            AppInput::Undo => {
                if !self.is_editing { return; }
                self.typing_run_active = false;
                self.undo_version = self.undo_version.wrapping_add(1);
                if let Some(id) = self.selected_item_id.clone() {
                    if let Some((_, item)) = self.find_item_mut(&id) {
                        if let ItemContent::Text(ref mut tb) = item.content {
                            tb.undo();
                        }
                    }
                    if is_in_chain(&self.document, &id) {
                        let sc = self.scale();
                        reflow_chain(&mut self.document, &id, sc);
                    }
                }
            }
            AppInput::Redo => {
                if !self.is_editing { return; }
                self.typing_run_active = false;
                self.undo_version = self.undo_version.wrapping_add(1);
                if let Some(id) = self.selected_item_id.clone() {
                    if let Some((_, item)) = self.find_item_mut(&id) {
                        if let ItemContent::Text(ref mut tb) = item.content {
                            tb.redo();
                        }
                    }
                    if is_in_chain(&self.document, &id) {
                        let sc = self.scale();
                        reflow_chain(&mut self.document, &id, sc);
                    }
                }
            }
            AppInput::LinkTo(target_id) => {
                let source_id = self.link_drag_source_id.clone()
                    .or_else(|| self.selected_item_id.clone());

                if let Some(source_id) = source_id {
                    let target_already_chained = self.find_item(&target_id)
                        .map(|(_, item)| {
                            if let ItemContent::Text(tb) = &item.content {
                                tb.prev_frame_id.is_some()
                            } else { true }
                        })
                        .unwrap_or(true);

                    if !target_already_chained {
                        if let Some((_, item)) = self.find_item_mut(&source_id) {
                            if let ItemContent::Text(tb) = &mut item.content {
                                tb.next_frame_id = Some(target_id.clone());
                            }
                        }
                        if let Some((_, item)) = self.find_item_mut(&target_id) {
                            if let ItemContent::Text(tb) = &mut item.content {
                                tb.prev_frame_id = Some(source_id.clone());
                            }
                        }
                        let sc = self.scale();
                        reflow_chain(&mut self.document, &source_id, sc);
                    }
                }
            }
            AppInput::UnlinkFrame => {
                if let Some(id) = self.selected_item_id.clone() {
                    let (next_id, prev_id) = self.find_item(&id)
                        .and_then(|(_, item)| {
                            if let ItemContent::Text(tb) = &item.content {
                                Some((tb.next_frame_id.clone(), tb.prev_frame_id.clone()))
                            } else { None }
                        })
                        .unwrap_or((None, None));

                    // Disconnect selected frame from both neighbours
                    if let Some((_, item)) = self.find_item_mut(&id) {
                        if let ItemContent::Text(tb) = &mut item.content {
                            tb.next_frame_id = None;
                            tb.prev_frame_id = None;
                        }
                    }
                    if let Some(nid) = next_id {
                        if let Some((_, item)) = self.find_item_mut(&nid) {
                            if let ItemContent::Text(tb) = &mut item.content {
                                tb.prev_frame_id = None;
                            }
                        }
                    }
                    if let Some(pid) = prev_id {
                        if let Some((_, item)) = self.find_item_mut(&pid) {
                            if let ItemContent::Text(tb) = &mut item.content {
                                tb.next_frame_id = None;
                            }
                        }
                    }
                }
            }
        }
    }
}

// ── Link-button geometry ──────────────────────────────────────────────────────

// The overflow indicator occupies the bottom-right corner of the item:
//   size = 10 px, margin = 3 px  (all in SCALE-space pixels)
// The link button sits immediately to its left, separated by a 2 px gap.
const LINK_BTN_SIZE_PX: f64 = 10.0;
const OVERFLOW_SIZE_PX: f64 = 10.0;
const CORNER_MARGIN_PX: f64 = 3.0;
const LINK_GAP_PX: f64 = 2.0;

/// Returns the link-button rect in local page mm coordinates.
/// `item.{x,y,width,height}` are already in mm.
fn link_button_rect_mm(item: &Item) -> (f64, f64, f64, f64) {
    let size   = LINK_BTN_SIZE_PX  / SCALE;
    let margin = CORNER_MARGIN_PX  / SCALE;
    let gap    = LINK_GAP_PX       / SCALE;
    let ovf    = OVERFLOW_SIZE_PX  / SCALE;
    let x = item.x + item.width  - margin - ovf - gap - size;
    let y = item.y + item.height - margin - size;
    (x, y, size, size)
}

fn hit_link_button_mm(item: &Item, local_x_mm: f64, local_y_mm: f64) -> bool {
    let (x, y, w, h) = link_button_rect_mm(item);
    local_x_mm >= x && local_x_mm <= x + w && local_y_mm >= y && local_y_mm <= y + h
}

// ── Text-chain reflow ─────────────────────────────────────────────────────────

fn is_in_chain(doc: &Document, id: &str) -> bool {
    doc.pages.iter()
        .flat_map(|p| p.items.iter())
        .find(|i| i.id == id)
        .map(|item| {
            if let ItemContent::Text(tb) = &item.content {
                tb.next_frame_id.is_some() || tb.prev_frame_id.is_some()
            } else { false }
        })
        .unwrap_or(false)
}

fn find_chain_root(doc: &Document, start_id: &str) -> String {
    let mut current = start_id.to_string();
    let mut visited = std::collections::HashSet::new();
    loop {
        if !visited.insert(current.clone()) { break; } // cycle guard
        let prev = doc.pages.iter()
            .flat_map(|p| p.items.iter())
            .find(|i| i.id == current)
            .and_then(|i| {
                if let ItemContent::Text(tb) = &i.content { tb.prev_frame_id.clone() } else { None }
            });
        match prev {
            Some(p) => current = p,
            None => break,
        }
    }
    current
}

fn collect_chain(doc: &Document, root_id: &str) -> Vec<String> {
    let mut chain = vec![root_id.to_string()];
    let mut current = root_id.to_string();
    let mut visited = std::collections::HashSet::new();
    visited.insert(root_id.to_string());
    loop {
        let next = doc.pages.iter()
            .flat_map(|p| p.items.iter())
            .find(|i| i.id == current)
            .and_then(|i| {
                if let ItemContent::Text(tb) = &i.content { tb.next_frame_id.clone() } else { None }
            });
        match next {
            Some(n) if !visited.contains(&n) => {
                visited.insert(n.clone());
                chain.push(n.clone());
                current = n;
            }
            _ => break,
        }
    }
    chain
}

/// Returns attributes from `global_attrs` that overlap `[start, end)`,
/// clamped and shifted so positions are relative to `start`.
fn slice_attrs_for_range(
    global_attrs: &[TextAttribute],
    start: usize,
    end: usize,
) -> Vec<TextAttribute> {
    let s = start as u32;
    let e = end as u32;
    let mut out = Vec::new();
    for attr in global_attrs {
        if attr.end <= s || attr.start >= e { continue; }
        let clamped_start = attr.start.max(s) - s;
        let clamped_end   = attr.end.min(e)   - s;
        if clamped_end > clamped_start {
            out.push(TextAttribute { start: clamped_start, end: clamped_end, value: attr.value.clone() });
        }
    }
    out
}

/// Redistributes the chain's text across all linked frames.
fn reflow_chain(doc: &mut Document, any_id: &str, scale: f64) {
    let root_id = find_chain_root(doc, any_id);
    let chain = collect_chain(doc, &root_id);
    if chain.len() < 2 { return; }

    // Snapshot each frame's current data (text + attrs + dimensions)
    struct FrameSnap {
        id: String,
        w: f64,
        h: f64,
        tb: TextBox,
    }
    let snaps: Vec<FrameSnap> = chain.iter().map(|id| {
        doc.pages.iter()
            .flat_map(|p| p.items.iter())
            .find(|i| &i.id == id)
            .map(|item| {
                let tb = if let ItemContent::Text(tb) = &item.content { tb.clone() } else { TextBox::default() };
                FrameSnap { id: id.clone(), w: item.width, h: item.height, tb }
            })
            .unwrap_or_else(|| FrameSnap { id: id.clone(), w: 100.0, h: 100.0, tb: TextBox::default() })
    }).collect();

    // Build global text and attributes from per-frame local slices
    let mut global_text = String::new();
    let mut global_attrs: Vec<TextAttribute> = Vec::new();
    for snap in &snaps {
        let offset = global_text.len() as u32;
        global_text.push_str(&snap.tb.text);
        for attr in &snap.tb.attributes {
            global_attrs.push(TextAttribute {
                start: attr.start + offset,
                end:   attr.end   + offset,
                value: attr.value.clone(),
            });
        }
    }

    // Distribute text into each frame
    let mut offset = 0usize;
    let n = snaps.len();

    for (i, snap) in snaps.iter().enumerate() {
        let is_last = i == n - 1;
        let remaining = &global_text[offset..];

        let capacity = if is_last || remaining.is_empty() {
            remaining.len()
        } else {
            // Build a temp TextBox with the remaining text to measure capacity
            let mut temp = snap.tb.clone();
            temp.text = remaining.to_string();
            temp.attributes = slice_attrs_for_range(&global_attrs, offset, global_text.len())
                .into_iter()
                .map(|a| TextAttribute { start: a.start, end: a.end, value: a.value })
                .collect();
            temp.text_capacity(snap.w * scale, snap.h * scale, scale)
        };

        let frame_text  = global_text[offset..offset + capacity].to_string();
        let frame_attrs = slice_attrs_for_range(&global_attrs, offset, offset + capacity);

        if let Some(item) = doc.pages.iter_mut()
            .flat_map(|p| p.items.iter_mut())
            .find(|i| i.id == snap.id)
        {
            if let ItemContent::Text(tb) = &mut item.content {
                tb.text_offset = offset;
                tb.text        = frame_text;
                tb.attributes  = frame_attrs;
                if tb.cursor_pos > tb.text.len() { tb.cursor_pos = tb.text.len(); }
                if tb.selection_anchor.map_or(false, |a| a > tb.text.len()) {
                    tb.selection_anchor = None;
                }
            }
        }

        offset += capacity;
        if offset >= global_text.len() {
            // Clear any remaining frames
            for later in &chain[i + 1..] {
                if let Some(item) = doc.pages.iter_mut()
                    .flat_map(|p| p.items.iter_mut())
                    .find(|i| &i.id == later)
                {
                    if let ItemContent::Text(tb) = &mut item.content {
                        tb.text_offset = offset;
                        tb.text.clear();
                        tb.attributes.clear();
                        tb.cursor_pos = 0;
                        tb.selection_anchor = None;
                    }
                }
            }
            break;
        }
    }
}

fn make_pango_ctx() -> gtk::pango::Context {
    use gtk::pango::prelude::FontMapExt;
    let font_map = pangocairo::FontMap::default();
    let ctx = font_map.create_context();
    pangocairo::functions::context_set_resolution(&ctx, 25.4 * SCALE);
    ctx
}

fn is_selected_type(doc: &Document, selected_id: &Option<String>, ty: &ItemType) -> bool {
    selected_id.as_ref()
        .and_then(|id| {
            for page in &doc.pages {
                if let Some(item) = page.items.iter().find(|i| &i.id == id) {
                    return Some(&item.content.item_type() == ty);
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
                    return Some(matches!(&item.content, ItemContent::Image(ib) if ib.image_path.is_some()));
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
                if let ItemContent::Image(ib) = &item.content {
                    return Some(ib.fit_mode);
                }
                return None;
            }
        }
        None
    })
}

fn get_selected_svg_fit_mode(doc: &Document, selected_id: &Option<String>) -> Option<crate::svg_box::FitMode> {
    selected_id.as_ref().and_then(|id| {
        for page in &doc.pages {
            if let Some(item) = page.items.iter().find(|i| &i.id == id) {
                if let ItemContent::Svg(sb) = &item.content {
                    return Some(sb.fit_mode);
                }
                return None;
            }
        }
        None
    })
}

fn get_svg_path_info(doc: &Document, selected_id: &Option<String>) -> String {
    selected_id.as_ref().and_then(|id| {
        for page in &doc.pages {
            if let Some(item) = page.items.iter().find(|i| &i.id == id) {
                if let ItemContent::Svg(sb) = &item.content {
                    return Some(format!("Current path: {}", sb.svg_path));
                }
            }
        }
        None
    }).unwrap_or_default()
}

fn get_info_text(doc: &Document, selected_id: &Option<String>) -> String {
    if let Some(id) = selected_id {
        for page in &doc.pages {
            if let Some(item) = page.items.iter().find(|i| &i.id == id) {
                return match &item.content {
                    ItemContent::Text(tb) => {
                        let text = &tb.text;
                        let paragraphs = text.split('\n').filter(|s| !s.is_empty()).count();
                        let words = text.split_whitespace().count();
                        let characters = text.len();
                        let lines = text.lines().count();
                        format!(
                            "Type: Text Frame\nSize: {:.1}x{:.1}mm\nParagraphs: {}\nLines: {}\nWords: {}\nCharacters: {}",
                            item.width, item.height, paragraphs, lines, words, characters
                        )
                    }
                    ItemContent::Image(ib) => {
                        let image_info = ib.image_path.as_ref()
                            .and_then(|p| std::path::Path::new(p).file_name())
                            .map(|n| format!("\nFile: {}", n.to_string_lossy()))
                            .unwrap_or_else(|| "\nNo image loaded".to_string());
                        format!("Type: Image Frame\nSize: {:.1}x{:.1}mm{}", item.width, item.height, image_info)
                    }
                    ItemContent::Svg(sb) => {
                        let svg_info = if sb.svg_path.is_empty() {
                            "\nNo SVG loaded".to_string()
                        } else {
                            format!("\nFile: {}", std::path::Path::new(&sb.svg_path).file_name().unwrap_or_default().to_string_lossy())
                        };
                        format!("Type: SVG Frame\nSize: {:.1}x{:.1}mm{}", item.width, item.height, svg_info)
                    }
                    ItemContent::Shape => format!("Type: Shape\nSize: {:.1}x{:.1}mm", item.width, item.height),
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


fn draw_page_content(
    cr: &cairo::Context,
    pango_ctx: &gtk::pango::Context,
    page: &crate::document::Page,
    images: &HashMap<String, Rc<cairo::ImageSurface>>,
    svg_handles: &HashMap<String, Rc<rsvg::SvgHandle>>,
    selected_id: Option<&String>,
    is_editing: bool,
    draw_handles: bool,
    scale_factor: f64,
    chain_ids: &[String],
) {
    for item in &page.items {
        let is_selected = selected_id == Some(&item.id);
        let is_editing_this = is_selected && is_editing;

        cr.save().unwrap();
        cr.translate(item.x * scale_factor, item.y * scale_factor);
        cr.rotate(item.rotation.to_radians());

        let w = item.width * scale_factor;
        let h = item.height * scale_factor;
        match &item.content {
            ItemContent::Text(tb) => {
                tb.render(cr, pango_ctx, w, h, is_selected, is_editing_this, scale_factor);
            }
            ItemContent::Image(ib) => {
                let image = ib.image_path.as_ref()
                    .and_then(|p| images.get(p))
                    .map(|rc| rc.as_ref());
                ib.render(cr, w, h, is_selected, image);
            }
            ItemContent::Svg(sb) => {
                let handle = if sb.svg_path.is_empty() {
                    None
                } else {
                    svg_handles.get(&sb.svg_path).map(|rc| rc.as_ref())
                };
                sb.render(cr, w, h, is_selected, handle);
            }
            ItemContent::Shape => {}
        }

        cr.restore().unwrap();

        if draw_handles && is_selected && !is_editing_this {
            let handles = get_handle_positions(item);
            cr.set_source_rgb(1.0, 1.0, 1.0);
            for (hx, hy) in &handles {
                cr.rectangle(hx * scale_factor - 3.0, hy * scale_factor - 3.0, 6.0, 6.0);
                cr.fill().unwrap();
            }
            cr.set_source_rgb(0.0, 0.5, 1.0);
            cr.set_line_width(1.0);
            for (hx, hy) in &handles {
                cr.rectangle(hx * scale_factor - 3.0, hy * scale_factor - 3.0, 6.0, 6.0);
                cr.stroke().unwrap();
            }
        }

        // Chain sibling highlight: dashed border on every non-selected member of the chain
        if !chain_ids.is_empty() && selected_id != Some(&item.id)
            && chain_ids.iter().any(|cid| *cid == item.id)
        {
            let w = item.width  * scale_factor;
            let h = item.height * scale_factor;
            cr.save().unwrap();
            cr.translate(item.x * scale_factor, item.y * scale_factor);
            cr.set_source_rgba(0.25, 0.55, 1.0, 0.75);
            cr.set_line_width(1.5);
            cr.set_dash(&[5.0, 3.0], 0.0);
            cr.rectangle(1.0, 1.0, w - 2.0, h - 2.0);
            cr.stroke().unwrap();
            cr.set_dash(&[], 0.0);
            cr.restore().unwrap();
        }

        // Chain indicators and link-out button
        if let ItemContent::Text(tb) = &item.content {
            let sf = scale_factor;
            // "flows in" indicator: green right-triangle at top-left
            if tb.prev_frame_id.is_some() {
                let x = item.x * sf;
                let y = item.y * sf;
                let s = 10.0f64;
                cr.set_source_rgb(0.0, 0.72, 0.42);
                cr.move_to(x, y);
                cr.line_to(x + s, y);
                cr.line_to(x, y + s);
                cr.close_path();
                cr.fill().unwrap();
            }
            // "flows out" indicator or link button at bottom-right
            let w = item.width  * sf;
            let h = item.height * sf;
            let item_has_overflow = !tb.text.is_empty()
                && h >= 20.0
                && tb.required_height(pango_ctx, w, sf) > h;

            if tb.next_frame_id.is_some() {
                // Already linked → orange arrow indicator next to overflow spot
                let margin = CORNER_MARGIN_PX;
                let size   = OVERFLOW_SIZE_PX;
                let gap    = LINK_GAP_PX;
                let bx = item.x * sf + w - margin - size - gap - size;
                let by = item.y * sf + h - margin - size;
                cr.set_source_rgb(1.0, 0.55, 0.0);
                cr.rectangle(bx, by, size, size);
                cr.fill().unwrap();
                cr.set_source_rgb(1.0, 1.0, 1.0);
                cr.set_line_width(1.2);
                let my = by + size / 2.0;
                cr.move_to(bx + 2.0, my);
                cr.line_to(bx + size - 2.5, my);
                cr.move_to(bx + size - 4.5, my - 2.0);
                cr.line_to(bx + size - 2.5, my);
                cr.line_to(bx + size - 4.5, my + 2.0);
                cr.stroke().unwrap();
            } else if item_has_overflow {
                // Overflow, no successor → blue link button
                let margin = CORNER_MARGIN_PX;
                let size   = LINK_BTN_SIZE_PX;
                let gap    = LINK_GAP_PX;
                let ovf    = OVERFLOW_SIZE_PX;
                let bx = item.x * sf + w - margin - ovf - gap - size;
                let by = item.y * sf + h - margin - size;
                cr.set_source_rgb(0.15, 0.45, 0.95);
                cr.rectangle(bx, by, size, size);
                cr.fill().unwrap();
                // Chain icon: two small squares joined by a bar
                cr.set_source_rgb(1.0, 1.0, 1.0);
                cr.set_line_width(1.2);
                let cy = by + size / 2.0;
                cr.rectangle(bx + 1.5, cy - 1.5, 3.0, 3.0);
                cr.stroke().unwrap();
                cr.rectangle(bx + size - 4.5, cy - 1.5, 3.0, 3.0);
                cr.stroke().unwrap();
                cr.move_to(bx + 4.5, cy);
                cr.line_to(bx + size - 4.5, cy);
                cr.stroke().unwrap();
            }
        }
    }
}

fn draw_canvas(
    cr: &gtk::cairo::Context,
    doc: &Document,
    drag_start: Option<(f64, f64)>,
    drag_current: Option<(f64, f64)>,
    selected_id: Option<String>,
    is_editing: bool,
    images: &HashMap<String, Rc<cairo::ImageSurface>>,
    svg_handles: &HashMap<String, Rc<rsvg::SvgHandle>>,
    zoom: f64,
    layout: PageLayout,
    link_drag: Option<((f64, f64), (f64, f64))>,
) {
    cr.save().unwrap();
    cr.scale(zoom, zoom);

    // Set resolution for Pango to match our SCALE (3px = 1mm)
    // 25.4 mm/inch * 3.0 px/mm = 76.2 DPI
    let pango_ctx = pangocairo::functions::create_context(cr);
    pangocairo::functions::context_set_resolution(&pango_ctx, 25.4 * SCALE);

    // Collect chain siblings of the selected frame so they can be highlighted
    let chain_ids: Vec<String> = selected_id.as_ref()
        .filter(|id| is_in_chain(doc, id))
        .map(|id| {
            let root = find_chain_root(doc, id);
            collect_chain(doc, &root)
        })
        .unwrap_or_default();

    let page_gap = 20.0;
    for (page_idx, page) in doc.pages.iter().enumerate() {
        let (off_x, off_y) = match layout {
            PageLayout::Vertical => (0.0, page_idx as f64 * (doc.height + page_gap)),
            PageLayout::Horizontal => (page_idx as f64 * (doc.width + page_gap), 0.0),
        };

        cr.save().unwrap();
        cr.translate(off_x * SCALE, off_y * SCALE);

        // Page background
        cr.set_source_rgb(1.0, 1.0, 1.0);
        cr.rectangle(0.0, 0.0, doc.width * SCALE, doc.height * SCALE);
        cr.fill().unwrap();

        // Page border
        cr.set_source_rgb(0.8, 0.8, 0.8);
        cr.set_line_width(1.0);
        cr.rectangle(0.0, 0.0, doc.width * SCALE, doc.height * SCALE);
        cr.stroke().unwrap();

        // Margins (visual guide)
        cr.set_source_rgb(0.0, 0.5, 1.0);
        cr.set_dash(&[5.0, 5.0], 0.0);
        cr.rectangle(
            10.0 * SCALE, 10.0 * SCALE,
            (doc.width - 20.0) * SCALE, (doc.height - 20.0) * SCALE,
        );
        cr.stroke().unwrap();
        cr.set_dash(&[], 0.0);

        draw_page_content(cr, &pango_ctx, page, images, svg_handles, selected_id.as_ref(), is_editing, true, SCALE, &chain_ids);

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

    // Link-drag arrow (drawn in screen pixels, outside the zoom transform)
    if let Some(((sx, sy), (cx, cy))) = link_drag {
        let dx = cx - sx;
        let dy = cy - sy;
        let len = (dx * dx + dy * dy).sqrt();
        cr.set_source_rgba(0.15, 0.45, 0.95, 0.9);
        cr.set_line_width(2.0);
        cr.set_dash(&[6.0, 4.0], 0.0);
        cr.move_to(sx, sy);
        cr.line_to(cx, cy);
        cr.stroke().unwrap();
        cr.set_dash(&[], 0.0);
        // Arrowhead
        if len > 8.0 {
            let nx = dx / len;
            let ny = dy / len;
            let al = 12.0;
            let aw = 6.0;
            cr.move_to(cx, cy);
            cr.line_to(cx - nx * al - ny * aw, cy - ny * al + nx * aw);
            cr.line_to(cx - nx * al + ny * aw, cy - ny * al - nx * aw);
            cr.close_path();
            cr.fill().unwrap();
        }
    }
}

pub fn export_to_pdf(
    document: &Document,
    images: &HashMap<String, Rc<cairo::ImageSurface>>,
    svg_handles: &HashMap<String, Rc<rsvg::SvgHandle>>,
    path: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let mm_to_points = 72.0 / 25.4;
    let width_pt = document.width * mm_to_points;
    let height_pt = document.height * mm_to_points;

    let surface = cairo::PdfSurface::new(width_pt, height_pt, path)?;
    let cr = cairo::Context::new(&surface)?;

    // Set resolution for Pango to match our SCALE (3px = 1mm)
    // 1 inch = 25.4 mm, so 25.4 * SCALE gives us the correct DPI for our coordinate system.
    let pango_ctx = pangocairo::functions::create_context(&cr);
    pangocairo::functions::context_set_resolution(&pango_ctx, 25.4 * SCALE);

    for page in &document.pages {
        surface.set_size(width_pt, height_pt)?;
        cr.save()?;
        // Map our internal units (pixels at SCALE) to PDF points.
        // 1mm = SCALE pixels in our app.
        // 1mm = 72/25.4 points in PDF.
        // So 1 pixel = (72/25.4) / SCALE points.
        let pdf_scale = mm_to_points / SCALE;
        cr.scale(pdf_scale, pdf_scale);

        draw_page_content(&cr, &pango_ctx, page, images, svg_handles, None, false, false, SCALE, &[]);

        cr.restore()?;
        cr.show_page()?;
    }

    surface.finish();
    Ok(())
}

