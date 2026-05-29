use relm4::prelude::*;
use adw::prelude::*;
use gtk::gdk;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use crate::app_dialogs::{show_info_dialog, show_keyboard_shortcuts_window};
use crate::app_io::{export_pdf_dialog, open_project_dialog, save_project_dialog, show_preferences_dialog};
use crate::document::{Document, Item, ItemContent, ItemType, MasterPage};
use crate::text_box::{TextBox, KeyAction, AttrValue, AttrSnapshot, TextAlign, TextAttribute};
use crate::image_box::{FitMode, ImageBox, WrapMode};
use crate::svg_box::SvgBox;
use crate::text_flow::PrecomputedFlowProvider;

const SCALE: f64 = 3.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PageLayout {
    Vertical,
    Horizontal,
}

#[derive(Debug, Clone, Copy)]
enum AlignH { Left, Center, Right }

#[derive(Debug, Clone, Copy)]
enum AlignV { Top, Center, Bottom }

struct EditLayoutCache {
    item_id: String,
    frame_w_px: f64,
    padding_px: f64,
    layout: gtk::pango::Layout,
}

#[derive(Hash, Eq, PartialEq)]
struct LayoutCacheKey {
    item_id: String,
    w_bits: u64,
    h_bits: u64,
    display_len: usize,
    reflow_version: u64,
}

pub struct AppModel {
    document: Document,
    current_page: usize,
    drag_start: Option<(f64, f64)>,
    drag_current: Option<(f64, f64)>,
    drag_offset: (f64, f64),
    selected_item_ids: Vec<String>,
    initial_item_rects: std::collections::HashMap<String, (f64, f64, f64, f64)>,
    active_handle: Option<usize>,
    is_moving: bool,
    popover_pos: (f64, f64),
    popover_visible: bool,
    popover_page_idx: Option<usize>,
    /// Number of upcoming DragStart events to swallow.
    /// Menu actions can close the popover twice in practice
    /// (explicit state change + GTK closed signal), so this is a counter
    /// instead of a one-shot boolean.
    pending_drag_start_swallows: u8,
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
    /// False after a document load until the canvas size_allocate fires,
    /// so that DragStart is ignored while GTK's layout pass is in flight.
    canvas_ready: bool,
    /// Pre-computed text-flow providers, keyed by text-frame item ID.
    /// Rebuilt whenever an image's WrapMode, position, or size changes.
    flow_providers: HashMap<String, PrecomputedFlowProvider>,
    autoscroll_timer: Option<gtk::glib::SourceId>,
    show_sidebar: bool,
    dirty: bool,
    edit_layout_cache: Option<EditLayoutCache>,
    render_layout_cache: Rc<RefCell<HashMap<LayoutCacheKey, gtk::pango::Layout>>>,
    pending_wrap_rebuild: bool,
    /// Master page editing mode: when true, pages[0] temporarily holds a master page's items.
    master_page_mode: bool,
    /// Which master page is currently loaded into pages[0] for editing.
    selected_master_page_id: Option<String>,
    /// Saved page-0 items while editing a master page; restored on exit.
    saved_page_0_items: Option<Vec<Item>>,
    /// Alignment-reference item ID (None = page).
    alignment_ref_id: Option<String>,
    pick_alignment_ref: bool,
    alignment_mode: bool,
    fit_message: Option<String>,
    /// Text displayed in the font-family entry.  Updated by the font dialog or
    /// when the cursor moves to a text position with a different family.
    font_entry_value: String,
}

impl AppModel {
    fn scale(&self) -> f64 {
        SCALE * self.zoom
    }

    fn swallow_upcoming_drag_start(&mut self) {
        self.pending_drag_start_swallows = self.pending_drag_start_swallows.saturating_add(1);
    }

    fn reset_pointer_interaction(&mut self) {
        self.drag_start = None;
        self.drag_current = None;
        self.drag_offset = (0.0, 0.0);
        self.initial_item_rects.clear();
        self.active_handle = None;
        self.is_moving = false;
        self.text_drag_active = false;
        self.link_drag_active = false;
        self.link_drag_source_id = None;
        self.link_drag_start = None;
        self.link_drag_current = None;
    }

    fn reset_to_new_project(&mut self) {
        self.document = Document::default();
        self.current_page = 0;
        self.reset_pointer_interaction();
        self.popover_visible = false;
        self.popover_page_idx = None;
        self.selected_item_ids.clear();
        self.is_editing = false;
        *self.editing_flag.borrow_mut() = false;
        self.invalidate_edit_layout();
        self.image_surfaces.clear();
        self.svg_handles.clear();
        self.last_save_path = None;
        self.dirty = false;
        self.flow_providers.clear();
        self.pending_wrap_rebuild = false;
        self.rebuild_flow_providers();
    }

    fn close_popover_and_skip_drag(&mut self) {
        self.popover_visible = false;
        self.popover_page_idx = None;
        self.reset_pointer_interaction();
        self.swallow_upcoming_drag_start();
    }

    fn invalidate_edit_layout(&mut self) {
        self.edit_layout_cache = None;
        if let Some(id) = self.selected_item_ids.first() {
            let id = id.clone();
            self.render_layout_cache.borrow_mut().retain(|k, _| k.item_id != id);
        }
    }

    fn page_at_canvas_coords(&self, x_px: f64, y_px: f64) -> Option<usize> {
        let x_mm = x_px / self.scale();
        let y_mm = y_px / self.scale();

        for (page_idx, _page) in self.document.pages.iter().enumerate().rev() {
            let (off_x, off_y) = self.get_page_offset(page_idx);
            let local_x = x_mm - off_x;
            let local_y = y_mm - off_y;
            if local_x >= 0.0
                && local_x <= self.document.width
                && local_y >= 0.0
                && local_y <= self.document.height
            {
                return Some(page_idx);
            }
        }

        None
    }

    fn page_index_for_canvas_coords(&self, x_px: f64, y_px: f64) -> usize {
        let x_mm = x_px / self.scale();
        let y_mm = y_px / self.scale();
        let page_gap = 20.0;
        let idx = match self.page_layout {
            PageLayout::Vertical => (y_mm / (self.document.height + page_gap)).floor() as i32,
            PageLayout::Horizontal => (x_mm / (self.document.width + page_gap)).floor() as i32,
        };
        (idx.max(0) as usize).min(self.document.pages.len().saturating_sub(1))
    }

    fn refresh_cursor_snapshot(&mut self) {
        if self.selected_item_type() == Some(ItemType::TextFrame) {
            if let Some(id) = self.selected_item_ids.first().cloned() {
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
        if let Some(id) = self.selected_item_ids.first().cloned() {
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
        self.selected_item_ids.first()
            .and_then(|id| self.find_item(id))
            .map(|(_, item)| {
                if let ItemContent::Text(tb) = &item.content {
                    tb.next_frame_id.is_some() || tb.prev_frame_id.is_some()
                } else { false }
            })
            .unwrap_or(false)
    }

    fn selected_item_type(&self) -> Option<ItemType> {
        let id = self.selected_item_ids.first()?;
        self.find_item(id).map(|(_, item)| item.content.item_type())
    }

    fn get_editing_text_box_mut(&mut self) -> Option<&mut TextBox> {
        let id = self.selected_item_ids.first()?.clone();
        let (_, item) = self.find_item_mut(&id)?;
        match &mut item.content {
            ItemContent::Text(tb) => Some(tb),
            _ => None,
        }
    }

    fn frame_visible_local_range(&self, id: &str) -> Option<(usize, usize)> {
        let (_, item) = self.find_item(id)?;
        let tb = match &item.content {
            ItemContent::Text(tb) => tb,
            _ => return None,
        };

        if !is_in_chain(&self.document, id) {
            return Some((0, tb.text.len()));
        }

        let chain = collect_chain(&self.document, &find_chain_root(&self.document, id));
        let idx = chain.iter().position(|cid| cid == id)?;
        let current_offset = tb.text_offset;
        let visible_end = if let Some(next_id) = chain.get(idx + 1) {
            self.find_item(next_id)
                .and_then(|(_, next_item)| {
                    if let ItemContent::Text(next_tb) = &next_item.content {
                        Some(next_tb.text_offset.saturating_sub(current_offset))
                    } else {
                        None
                    }
                })
                .unwrap_or(tb.text.len())
        } else {
            tb.text.len()
        };

        Some((0, visible_end.min(tb.text.len())))
    }

    fn selected_text_operation_range(&self, id: &str, editing: bool) -> Option<(usize, usize)> {
        let (_, item) = self.find_item(id)?;
        let tb = match &item.content {
            ItemContent::Text(tb) => tb,
            _ => return None,
        };

        if editing {
            Some(tb.selection_or_word_range())
        } else {
            self.frame_visible_local_range(id)
        }
    }

    fn selected_has_wrapped_image(&self) -> bool {
        self.selected_item_ids.iter().any(|id| {
            self.find_item(id)
                .map(|(_, item)| matches!(
                    item.content,
                    ItemContent::Image(ref ib) if ib.wrap_mode != WrapMode::Independent
                ))
                .unwrap_or(false)
        })
    }

    fn get_or_build_edit_layout(
        &mut self,
        id: &str,
        pango_ctx: &gtk::pango::Context,
        frame_w_px: f64,
        scale_factor: f64,
    ) -> Option<gtk::pango::Layout> {
        let (_, item) = self.find_item(id)?;
        let tb = match &item.content {
            ItemContent::Text(tb) => tb,
            _ => return None,
        };
        let padding_px = tb.padding * scale_factor;

        let cache_valid = self.edit_layout_cache.as_ref().map(|cache| {
            cache.item_id == id
                && (cache.frame_w_px - frame_w_px).abs() < 0.01
                && (cache.padding_px - padding_px).abs() < 0.01
        }).unwrap_or(false);

        if cache_valid {
            return self.edit_layout_cache.as_ref().map(|cache| cache.layout.clone());
        }

        let started = std::time::Instant::now();
        let layout = tb.prepare_layout(pango_ctx, frame_w_px, padding_px);
        let elapsed = started.elapsed();
        if tb.text.len() > 2_000 || elapsed.as_millis() >= 8 {
            eprintln!(
                "[perf] build_edit_layout id={} text_len={} attrs={} linked={} width_px={:.1} took={}ms",
                id,
                tb.text.len(),
                tb.attributes.len(),
                tb.next_frame_id.is_some() || tb.prev_frame_id.is_some(),
                frame_w_px,
                elapsed.as_millis()
            );
        }
        self.edit_layout_cache = Some(EditLayoutCache {
            item_id: id.to_string(),
            frame_w_px,
            padding_px,
            layout: layout.clone(),
        });
        Some(layout)
    }

    fn ensure_cursor_visible_cached(
        &mut self,
        id: &str,
        w: f64,
        h: f64,
        scale_factor: f64,
    ) -> Option<f64> {
        let (_, item) = self.find_item(id)?;
        let tb = match &item.content {
            ItemContent::Text(tb) => tb,
            _ => return None,
        };
        let pango_ctx = make_pango_ctx();
        let layout = self.get_or_build_edit_layout(id, &pango_ctx, w, scale_factor)?;
        let padding = tb.padding * scale_factor;
        let pscale = gtk::pango::SCALE as f64;

        let byte_idx = tb.cursor_pos.min(tb.text.len()) as i32;
        let (strong, _) = layout.cursor_pos(byte_idx);
        let cursor_top = padding + strong.y() as f64 / pscale;
        let cursor_bot = cursor_top + strong.height() as f64 / pscale;

        let new_scroll = if cursor_bot > tb.scroll_y + h {
            cursor_bot - h
        } else if cursor_top < tb.scroll_y {
            cursor_top
        } else {
            return Some(tb.scroll_y);
        };

        let (_, ph) = layout.size();
        let total_h = ph as f64 / pscale + 2.0 * padding;
        Some(new_scroll.clamp(0.0, (total_h - h).max(0.0)))
    }

    fn required_height_cached(&mut self, id: &str, w: f64, scale_factor: f64) -> Option<f64> {
        let (_, item) = self.find_item(id)?;
        let tb = match &item.content {
            ItemContent::Text(tb) => tb,
            _ => return None,
        };
        let pango_ctx = make_pango_ctx();
        let layout = self.get_or_build_edit_layout(id, &pango_ctx, w, scale_factor)?;
        let padding = tb.padding * scale_factor;
        let (_, ph) = layout.size();
        Some(ph as f64 / gtk::pango::SCALE as f64 + 2.0 * padding)
    }

    fn move_cursor_vertical_cached(
        &mut self,
        id: &str,
        up: bool,
        extend: bool,
        frame_w_px: f64,
        scale_factor: f64,
    ) {
        let Some((_, item)) = self.find_item(id) else { return; };
        let tb = match &item.content {
            ItemContent::Text(tb) => tb,
            _ => return,
        };
        let pango_ctx = make_pango_ctx();
        let Some(layout) = self.get_or_build_edit_layout(id, &pango_ctx, frame_w_px, scale_factor) else { return; };

        let byte_idx = tb.cursor_pos.min(tb.text.len()) as i32;
        let (strong, _) = layout.cursor_pos(byte_idx);
        let cur_x = strong.x();
        let cur_y = strong.y();
        let line_h = strong.height();
        let new_y = if up { cur_y - line_h / 2 } else { cur_y + line_h + line_h / 2 };

        let mut new_anchor = tb.selection_anchor;
        if extend && new_anchor.is_none() {
            new_anchor = Some(tb.cursor_pos);
        } else if !extend {
            new_anchor = None;
        }

        let new_pos = if new_y < 0 {
            0
        } else {
            let (_inside, new_byte, trailing) = layout.xy_to_index(cur_x, new_y);
            let mut pos = new_byte as usize;
            if trailing > 0 && pos < tb.text.len() {
                pos = next_char_boundary_local(&tb.text, pos);
            }
            pos
        };

        if let Some(tb_mut) = self.get_editing_text_box_mut() {
            tb_mut.selection_anchor = new_anchor;
            tb_mut.cursor_pos = new_pos;
        }
    }

    fn hit_test_cached(
        &mut self,
        id: &str,
        frame_x: f64,
        frame_y: f64,
        click_x_mm: f64,
        click_y_mm: f64,
        scale: f64,
        frame_w_px: f64,
    ) -> Option<usize> {
        let (_, item) = self.find_item(id)?;
        let tb = match &item.content {
            ItemContent::Text(tb) => tb,
            _ => return None,
        };
        let pango_ctx = make_pango_ctx();
        let layout = self.get_or_build_edit_layout(id, &pango_ctx, frame_w_px, scale)?;
        let pscale = gtk::pango::SCALE as f64;
        let padding = tb.padding * scale;

        let rel_x = (click_x_mm - frame_x) * scale - padding;
        let rel_y = (click_y_mm - frame_y) * scale - padding + tb.scroll_y;
        let x_pango = (rel_x * pscale).max(0.0) as i32;
        let y_pango = (rel_y * pscale).max(0.0) as i32;
        let (_inside, byte_idx, trailing) = layout.xy_to_index(x_pango, y_pango);
        let byte_idx = byte_idx as usize;

        Some(if trailing > 0 && byte_idx < tb.text.len() {
            next_char_boundary_local(&tb.text, byte_idx)
        } else {
            byte_idx
        })
    }

    fn process_drag(&mut self) {
        let (offset_x, offset_y) = self.drag_offset;

        if self.link_drag_active {
            if let Some((sx, sy)) = self.link_drag_start {
                self.link_drag_current = Some((sx + offset_x, sy + offset_y));
            }
            return;
        }

        if self.text_drag_active {
            if let Some((sx, sy)) = self.drag_start {
                self.drag_current = Some((sx + offset_x, sy + offset_y));
                let x_mm = (sx + offset_x) / self.scale();
                let y_mm = (sy + offset_y) / self.scale();

                let hit_data = if let Some(id) = self.selected_item_ids.first().cloned() {
                    if let Some((page_idx, item)) = self.find_item(&id) {
                        let (off_x, off_y) = self.get_page_offset(page_idx);
                        let local_x = x_mm - off_x;
                        let local_y = y_mm - off_y;
                        let pos = if matches!(item.content, ItemContent::Text(_)) {
                            self.hit_test_cached(&id, item.x, item.y, local_x, local_y, SCALE, item.width * SCALE)
                                .unwrap_or(0)
                        } else { 0 };
                        Some((pos, item.width * SCALE, item.height * SCALE))
                    } else {
                        None
                    }
                } else { None };

                if let Some((pos, w, h)) = hit_data {
                    if let Some(tb) = self.get_editing_text_box_mut() {
                        tb.cursor_pos = pos;
                    }
                    if let Some(id) = self.selected_item_ids.first().cloned() {
                        if let Some(s) = self.ensure_cursor_visible_cached(&id, w, h, SCALE) {
                            if let Some(tb) = self.get_editing_text_box_mut() {
                                tb.scroll_y = s;
                            }
                        }
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

            if active_handle.is_some() {
                if let Some(id) = self.selected_item_ids.first().cloned() {
                    if let Some(&(ix, iy, iw, ih)) = self.initial_item_rects.get(&id) {
                        // Pre-calculate image ratio if needed
                        let image_ratio = if let Some((_, item)) = self.find_item(&id) {
                            if let ItemContent::Image(ib) = &item.content {
                                if ib.fit_mode == FitMode::FrameToImage {
                                    ib.image_path.as_ref().and_then(|path| {
                                        self.image_surfaces.get(path).map(|surf| surf.width() as f64 / surf.height() as f64)
                                    })
                                } else { None }
                            } else { None }
                        } else { None };

                        if let Some((_page_idx, item)) = self.find_item_mut(&id) {
                            let handle_idx = active_handle.unwrap();
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

                            if let Some(ratio) = image_ratio {
                                match handle_idx {
                                    3 | 7 | 4 | 6 => { item.height = item.width / ratio; }
                                    1 | 5 => { item.width = item.height * ratio; }
                                    0 | 2 => { item.width = item.height * ratio; }
                                    _ => {}
                                }
                            }

                            if item.width < 1.0 { item.width = 1.0; }
                            if item.height < 1.0 { item.height = 1.0; }
                        }

                        if self.selected_has_wrapped_image() {
                            self.pending_wrap_rebuild = true;
                        }
                    }
                }
            } else if is_moving {
                // We don't check for offset_x*offset_x + offset_y*offset_y >= 100.0 here 
                // because autoscroll might move it by smaller increments. 
                // We should probably check it in the DragUpdate message handler instead.
                let mut moves = Vec::new();
                let ids = self.selected_item_ids.clone();
                for id in &ids {
                    if let Some(&(ix, iy, _iw, _ih)) = self.initial_item_rects.get(id) {
                        let page_layout = self.page_layout;
                        let doc_height = self.document.height;
                        let doc_width = self.document.width;

                        if let Some((page_idx, item)) = self.find_item_mut(id) {
                            item.x = ix + dx;
                            item.y = iy + dy;

                            // Check for page move
                            let page_gap = 20.0;
                            let (off_x, off_y) = match page_layout {
                                PageLayout::Vertical => (0.0, page_idx as f64 * (doc_height + page_gap)),
                                PageLayout::Horizontal => (page_idx as f64 * (doc_width + page_gap), 0.0),
                            };
                            let abs_x_mm = off_x + item.x + item.width / 2.0;
                            let abs_y_mm = off_y + item.y + item.height / 2.0;

                            let target_page_idx = match page_layout {
                                PageLayout::Vertical => (abs_y_mm / (doc_height + page_gap)).floor() as usize,
                                PageLayout::Horizontal => (abs_x_mm / (doc_width + page_gap)).floor() as usize,
                            };
                            let target_page_idx = target_page_idx.min(self.document.pages.len().saturating_sub(1));

                            if target_page_idx != page_idx {
                                moves.push((id.clone(), page_idx, target_page_idx));
                            }
                        }
                    }
                }

                for (id, old_idx, new_idx) in moves {
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
                                // Update initial rect to reflect new page origin
                                if let Some(rect) = self.initial_item_rects.get_mut(&id) {
                                    let abs_ix = old_off_x + rect.0;
                                    let abs_iy = old_off_y + rect.1;
                                    rect.0 = abs_ix - new_off_x;
                                    rect.1 = abs_iy - new_off_y;
                                }
                            }
                        }
                    }
                }

                if self.selected_has_wrapped_image() {
                    self.pending_wrap_rebuild = true;
                }
            }
            self.dirty = true;
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

#[derive(Debug, Clone)]
pub enum AppInput {
    AddPage,
    DragStart(f64, f64, gdk::ModifierType),
    DragUpdate(f64, f64),
    DragEnd,
    RightClick(f64, f64),
    DoubleClick(f64, f64),
    ClosePopover,
    StartEdit,
    TextKeyPressed(gdk::Key, gdk::ModifierType),
    PasteText(String),
    Zoom(f64),
    SetCreateFrameType(ItemType),
    ImportImage,
    ImageLoaded(String),
    FitFrameToImage,
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
    ShowKeyboardShortcuts,
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
    SplitChainHere,
    SetWrapMode(WrapMode),
    SetShowBorder(bool),
    Undo,
    Redo,
    CutText,
    /// Fired by the undo-commit debounce; ignored if version doesn't match.
    MaybeCommitHistory(u64),
    /// Fired by size_allocate after a document load; unlocks DragStart.
    CanvasReady,
    Autoscroll,
    AdjustDragOffset(f64, f64),
    ToggleSidebar,
    NewProject,
    NewProjectConfirmed,
    MaybeReflow(u64),
    DeletePage,
    CreateMasterPage,
    ApplyMasterPage(String),
    RenameMasterPage(String, String),
    RemoveMasterPageFromPage,
    DeleteMasterPage(String),
    EnterMasterPageMode(String),
    ExitMasterPageMode,
    NewMasterPage,
    AlignLeft,
    AlignCenterH,
    AlignRight,
    AlignTop,
    AlignCenterV,
    AlignBottom,
    StartPickAlignmentRef,
    SetAlignmentRef(String),
    EnterAlignmentMode,
    ExitAlignmentMode,
    FitPageToWindow,
    ClearFitMessage,
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

            add_controller = gtk::EventControllerKey {
                connect_key_pressed[sender] => move |_ctrl, keyval, _keycode, state| {
                    if state.contains(gdk::ModifierType::CONTROL_MASK) && keyval == gdk::Key::f {
                        sender.input(AppInput::FitPageToWindow);
                        return gtk::glib::Propagation::Stop;
                    }
                    gtk::glib::Propagation::Proceed
                },
            },

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
                        set_icon_name: "document-new-symbolic",
                        set_tooltip_text: Some("New Project"),
                        connect_clicked => AppInput::NewProject,
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
                    pack_end = &gtk::Button {
                        set_icon_name: "help-about-symbolic",
                        set_tooltip_text: Some("Toggle Properties"),
                        connect_clicked => AppInput::ToggleSidebar,
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
                                    set_label: "Keyboard Shortcuts",
                                    set_has_frame: false,
                                    connect_clicked[sender] => move |btn| {
                                        sender.input(AppInput::ShowKeyboardShortcuts);
                                        btn.ancestor(gtk::Popover::static_type()).and_then(|p| p.downcast::<gtk::Popover>().ok()).map(|p| p.popdown());
                                    },
                                },
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
                    #[watch]
                    set_show_sidebar: model.show_sidebar,

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
                            set_label: &{
                                if let Some(ref msg) = model.fit_message {
                                    msg.clone()
                                } else if model.master_page_mode {
                                    model.document.master_pages.iter()
                                        .find(|m| m.id == *model.selected_master_page_id.as_deref().unwrap_or(""))
                                        .map(|m| format!("Master: {}", m.name))
                                        .unwrap_or_else(|| "Master: —".to_string())
                                } else {
                                    format!("Pages: {}", model.effective_page_count())
                                }
                            },
                        },
                        gtk::Label {
                            #[watch]
                            set_label: &format!("Size: {}x{}mm", model.document.width, model.document.height),
                        },
                        gtk::Separator {},
                        gtk::Box {
                            set_orientation: gtk::Orientation::Horizontal,
                            set_spacing: 4,
                            add_css_class: "linked",
                            gtk::ToggleButton {
                                set_label: "Pages",
                                #[watch]
                                set_active: !model.master_page_mode,
                                connect_toggled[sender] => move |btn| {
                                    if btn.is_active() {
                                        sender.input(AppInput::ExitMasterPageMode);
                                    }
                                },
                            },
                            gtk::ToggleButton {
                                set_label: "Masters",
                                #[watch]
                                set_active: model.master_page_mode,
                                connect_toggled[sender] => move |btn| {
                                    if btn.is_active() {
                                        sender.input(AppInput::EnterMasterPageMode(String::new()));
                                    }
                                },
                            },
                        },
                        gtk::Separator {},
                        gtk::Label {
                            set_label: "Master Pages",
                            add_css_class: "heading",
                            set_xalign: 0.0,
                        },
                        gtk::Box {
                            set_orientation: gtk::Orientation::Horizontal,
                            set_spacing: 4,
                            gtk::Button {
                                set_label: "New",
                                set_tooltip_text: Some("Create blank master page"),
                                add_css_class: "flat",
                                connect_clicked => AppInput::NewMasterPage,
                            },
                            gtk::Button {
                                set_label: "From page",
                                set_tooltip_text: Some("Create master page from current page layout"),
                                add_css_class: "flat",
                                connect_clicked => AppInput::CreateMasterPage,
                            },
                        },
                        #[name = "master_pages_listbox"]
                        gtk::ListBox {
                            add_css_class: "rich-list",
                            set_selection_mode: gtk::SelectionMode::None,
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
                            set_label: &format!("Selected: {}", model.selected_item_ids.first().map(|s| s.as_str()).unwrap_or("None")),
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
                        gtk::Box {
                            set_orientation: gtk::Orientation::Horizontal,
                            set_spacing: 4,
                            #[watch]
                            set_visible: model.selected_item_type() == Some(ItemType::TextFrame),
                            gtk::Entry {
                                set_placeholder_text: Some("Font family"),
                                #[watch]
                                set_text: &model.font_entry_value,
                                set_hexpand: true,
                                connect_activate[sender] => move |entry| {
                                    let f = entry.text().to_string();
                                    if !f.is_empty() {
                                        sender.input(AppInput::SetFontFamily(f));
                                    }
                                },
                            },
                            gtk::Button {
                                set_icon_name: "preferences-desktop-font-symbolic",
                                set_tooltip_text: Some("Choose font..."),
                                add_css_class: "flat",
                                connect_clicked[sender, root] => move |_| {
                                    // Use our custom font dialog that can distinguish
                                    // optical-size variants (e.g. EB Garamond 12 vs 08)
                                    // which GTK4 FontDialog cannot expose.
                                    show_custom_font_dialog(&root, sender.clone());
                                },
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
                        gtk::Separator {
                            #[watch]
                            set_visible: model.alignment_mode,
                        },
                        gtk::Label {
                            set_label: "Alignment",
                            add_css_class: "title-4",
                            #[watch]
                            set_visible: model.alignment_mode,
                        },
                        gtk::Label {
                            #[watch]
                            set_label: &{
                                if let Some(ref ref_id) = model.alignment_ref_id {
                                    let mut found: Option<&crate::document::Item> = None;
                                    for page in &model.document.pages {
                                        if let Some(item) = page.items.iter().find(|i| &i.id == ref_id) {
                                            found = Some(item);
                                            break;
                                        }
                                    }
                                    if let Some(item) = found {
                                        format!("Reference: {}", item.id.chars().take(6).collect::<String>())
                                    } else {
                                        "Reference: Page".to_string()
                                    }
                                } else {
                                    "Reference: Page".to_string()
                                }
                            },
                            add_css_class: "caption",
                            set_xalign: 0.0,
                            set_ellipsize: gtk::pango::EllipsizeMode::End,
                            #[watch]
                            set_visible: model.alignment_mode,
                        },
                        gtk::Box {
                            set_orientation: gtk::Orientation::Horizontal,
                            set_spacing: 4,
                            #[watch]
                            set_visible: model.alignment_mode,
                            gtk::Button {
                                set_label: "Pick reference",
                                set_tooltip_text: Some("Click an item on the canvas to use as alignment reference"),
                                connect_clicked[sender] => move |_| {
                                    sender.input(AppInput::StartPickAlignmentRef);
                                },
                            },
                            gtk::Button {
                                set_label: "Page",
                                set_tooltip_text: Some("Use page as reference"),
                                connect_clicked => AppInput::SetAlignmentRef(String::new()),
                            },
                        },
                        gtk::Separator {
                            #[watch]
                            set_visible: model.alignment_mode,
                        },
                        gtk::Label {
                            set_label: "Horizontal",
                            add_css_class: "heading",
                            set_xalign: 0.0,
                            #[watch]
                            set_visible: model.alignment_mode,
                        },
                        gtk::Box {
                            set_orientation: gtk::Orientation::Horizontal,
                            set_spacing: 4,
                            add_css_class: "linked",
                            #[watch]
                            set_visible: model.alignment_mode,
                            gtk::Button {
                                set_label: "Left",
                                set_tooltip_text: Some("Align left edges"),
                                connect_clicked => AppInput::AlignLeft,
                            },
                            gtk::Button {
                                set_label: "Center",
                                set_tooltip_text: Some("Align horizontal centers"),
                                connect_clicked => AppInput::AlignCenterH,
                            },
                            gtk::Button {
                                set_label: "Right",
                                set_tooltip_text: Some("Align right edges"),
                                connect_clicked => AppInput::AlignRight,
                            },
                        },
                        gtk::Separator {
                            #[watch]
                            set_visible: model.alignment_mode,
                        },
                        gtk::Label {
                            set_label: "Vertical",
                            add_css_class: "heading",
                            set_xalign: 0.0,
                            #[watch]
                            set_visible: model.alignment_mode,
                        },
                        gtk::Box {
                            set_orientation: gtk::Orientation::Horizontal,
                            set_spacing: 4,
                            add_css_class: "linked",
                            #[watch]
                            set_visible: model.alignment_mode,
                            gtk::Button {
                                set_label: "Top",
                                set_tooltip_text: Some("Align top edges"),
                                connect_clicked => AppInput::AlignTop,
                            },
                            gtk::Button {
                                set_label: "Center",
                                set_tooltip_text: Some("Align vertical centers"),
                                connect_clicked => AppInput::AlignCenterV,
                            },
                            gtk::Button {
                                set_label: "Bottom",
                                set_tooltip_text: Some("Align bottom edges"),
                                connect_clicked => AppInput::AlignBottom,
                            },
                        },
                        gtk::Separator {
                            #[watch]
                            set_visible: model.alignment_mode,
                        },
                        gtk::Button {
                            set_label: "Close alignment",
                            set_tooltip_text: Some("Return to normal mode"),
                            #[watch]
                            set_visible: model.alignment_mode,
                            connect_clicked => AppInput::ExitAlignmentMode,
                                    },
                                },

                    #[name = "scrolled_window"]
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
                                    set_content_width: {
                                        let page_gap = 20.0;
                                        let w = match model.page_layout {
                                            PageLayout::Horizontal => {
                                                model.effective_page_count() as f64 * model.document.width + (model.effective_page_count().saturating_sub(1) as f64) * page_gap
                                            }
                                            PageLayout::Vertical => model.document.width,
                                        };
                                        (w * model.scale()) as i32
                                    },
                                    #[watch]
                                    set_content_height: {
                                        let page_gap = 20.0;
                                        let h = match model.page_layout {
                                            PageLayout::Vertical => {
                                                model.effective_page_count() as f64 * model.document.height + (model.effective_page_count().saturating_sub(1) as f64) * page_gap
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
                                        let selected = model.selected_item_ids.clone();
                                        let editing = model.is_editing;
                                        let zoom = model.zoom;
                                        let layout = model.page_layout;
                                        let images = model.image_surfaces.clone();
                                        let svgs = model.svg_handles.clone();
                                        let link_drag = if model.link_drag_active {
                                            model.link_drag_start.zip(model.link_drag_current)
                                        } else { None };
                                        let flow_providers = model.flow_providers.clone();
                                        let editing_layout = model.edit_layout_cache.as_ref()
                                            .map(|cache| (cache.item_id.clone(), cache.layout.clone()));
                                        let render_cache = Rc::clone(&model.render_layout_cache);
                                        let reflow_version = model.reflow_version;
                                        let master_mode = model.master_page_mode && model.selected_master_page_id.is_some();
                                        move |_area, cr, _w, _h| {
                                            draw_canvas(cr, &doc, d_start, d_current, &selected, editing, &images, &svgs, zoom, layout, link_drag, &flow_providers, editing_layout.as_ref(), &render_cache, reflow_version, master_mode);
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
                                        connect_drag_begin[sender] => move |gesture, x, y| {
                                            let state = gesture.current_event().map(|e| e.modifier_state()).unwrap_or(gdk::ModifierType::empty());
                                            sender.input(AppInput::DragStart(x, y, state));
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

                                        // Page actions
                                        gtk::Button {
                                            set_label: "Eliminar Página",
                                            add_css_class: "destructive-action",
                                            #[watch]
                                            set_visible: model.selected_item_ids.is_empty()
                                                && model.document.pages.len() > 1
                                                && model.popover_page_idx.is_some(),
                                            connect_clicked => AppInput::DeletePage,
                                        },
                                        gtk::Label {
                                            #[watch]
                                            set_label: &{
                                                let page_idx = model.popover_page_idx.unwrap_or(model.current_page);
                                                model.document.pages.get(page_idx)
                                                    .and_then(|p| p.master_page.as_deref())
                                                    .and_then(|mid| model.document.master_pages.iter().find(|m| m.id == *mid))
                                                    .map(|m| format!("Master: {}", m.name))
                                                    .unwrap_or_default()
                                            },
                                            #[watch]
                                            set_visible: model.selected_item_ids.is_empty()
                                                && model.popover_page_idx.is_some()
                                                && !model.document.master_pages.is_empty(),
                                            add_css_class: "caption",
                                            set_xalign: 0.0,
                                        },
                                        gtk::Button {
                                            set_label: "Convertir a página maestra",
                                            #[watch]
                                            set_visible: model.selected_item_ids.is_empty()
                                                && model.popover_page_idx.is_some(),
                                            connect_clicked => AppInput::CreateMasterPage,
                                        },
                                        gtk::Button {
                                            set_label: "Quitar página maestra",
                                            #[watch]
                                            set_visible: model.selected_item_ids.is_empty()
                                                && model.popover_page_idx.is_some()
                                                && {
                                                    let page_idx = model.popover_page_idx.unwrap_or(model.current_page);
                                                    model.document.pages.get(page_idx)
                                                        .map(|p| p.master_page.is_some())
                                                        .unwrap_or(false)
                                                },
                                            connect_clicked => AppInput::RemoveMasterPageFromPage,
                                        },
                                        gtk::Separator {
                                            #[watch]
                                            set_visible: model.selected_item_ids.is_empty()
                                                && model.document.pages.len() > 1
                                                && model.popover_page_idx.is_some(),
                                        },

                                        gtk::Label {
                                            set_label: "Frame Information",
                                            add_css_class: "title-4",
                                        },
                                        gtk::Separator {},
                                        gtk::Label {
                                            #[watch]
                                            set_label: &get_info_text(&model.document, &model.selected_item_ids),
                                            set_xalign: 0.0,
                                        },

                                        // TextFrame actions
                                        gtk::Button {
                                            set_label: "Edit Text",
                                            add_css_class: "suggested-action",
                                            #[watch]
                                            set_visible: is_selected_type(&model.document, &model.selected_item_ids, &ItemType::TextFrame),
                                            connect_clicked => AppInput::StartEdit,
                                        },
                                        gtk::Button {
                                            set_label: "Dividir cadena aquí",
                                            #[watch]
                                            set_visible: selected_text_frame_has_next_chain(&model.document, &model.selected_item_ids, model.is_editing),
                                            connect_clicked => AppInput::SplitChainHere,
                                        },

                                        // ImageFrame actions
                                        gtk::Button {
                                            set_label: "Import Image",
                                            add_css_class: "suggested-action",
                                            #[watch]
                                            set_visible: is_selected_type(&model.document, &model.selected_item_ids, &ItemType::ImageFrame),
                                            connect_clicked => AppInput::ImportImage,
                                        },

                                        // Adjust Image section (only when ImageFrame has an image)
                                        gtk::Separator {
                                            #[watch]
                                            set_visible: selected_image_frame_has_image(&model.document, &model.selected_item_ids),
                                        },
                                        gtk::Label {
                                            set_label: "Adjust Image",
                                            add_css_class: "heading",
                                            set_xalign: 0.0,
                                            #[watch]
                                            set_visible: selected_image_frame_has_image(&model.document, &model.selected_item_ids),
                                        },
                                        gtk::Box {
                                            set_orientation: gtk::Orientation::Horizontal,
                                            set_spacing: 4,
                                            add_css_class: "linked",
                                            #[watch]
                                            set_visible: selected_image_frame_has_image(&model.document, &model.selected_item_ids),

                                            gtk::ToggleButton {
                                                set_label: "Stretch",
                                                #[watch]
                                                set_active: get_selected_fit_mode(&model.document, &model.selected_item_ids) == Some(crate::image_box::FitMode::ImageToFrame),
                                                connect_toggled[sender] => move |btn| {
                                                    if btn.is_active() {
                                                        sender.input(AppInput::SetImageFitMode(crate::image_box::FitMode::ImageToFrame));
                                                    }
                                                },
                                            },
                                            gtk::ToggleButton {
                                                set_label: "Proportional",
                                                #[watch]
                                                set_active: get_selected_fit_mode(&model.document, &model.selected_item_ids) == Some(crate::image_box::FitMode::FrameToImage),
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
                                            set_visible: selected_image_frame_has_image(&model.document, &model.selected_item_ids),
                                            connect_clicked => AppInput::FitFrameToImage,
                                        },

                                        // Text-wrap mode
                                        gtk::Separator {
                                            #[watch]
                                            set_visible: is_selected_type(&model.document, &model.selected_item_ids, &ItemType::ImageFrame),
                                        },
                                        gtk::Label {
                                            set_label: "Ajuste de texto",
                                            add_css_class: "heading",
                                            set_xalign: 0.0,
                                            #[watch]
                                            set_visible: is_selected_type(&model.document, &model.selected_item_ids, &ItemType::ImageFrame),
                                        },
                                        gtk::Box {
                                            set_orientation: gtk::Orientation::Horizontal,
                                            set_spacing: 4,
                                            add_css_class: "linked",
                                            #[watch]
                                            set_visible: is_selected_type(&model.document, &model.selected_item_ids, &ItemType::ImageFrame),

                                            gtk::ToggleButton {
                                                set_label: "Libre",
                                                set_tooltip_text: Some("La imagen flota libre sin afectar el texto"),
                                                #[watch]
                                                set_active: get_selected_wrap_mode(&model.document, &model.selected_item_ids) == Some(WrapMode::Independent),
                                                connect_toggled[sender] => move |btn| {
                                                    if btn.is_active() { sender.input(AppInput::SetWrapMode(WrapMode::Independent)); }
                                                },
                                            },
                                            gtk::ToggleButton {
                                                set_label: "Bloque",
                                                set_tooltip_text: Some("El texto fluye solo arriba y abajo de la imagen"),
                                                #[watch]
                                                set_active: get_selected_wrap_mode(&model.document, &model.selected_item_ids) == Some(WrapMode::Block),
                                                connect_toggled[sender] => move |btn| {
                                                    if btn.is_active() { sender.input(AppInput::SetWrapMode(WrapMode::Block)); }
                                                },
                                            },
                                            gtk::ToggleButton {
                                                set_label: "Izq.",
                                                set_tooltip_text: Some("Imagen a la izquierda, texto a la derecha"),
                                                #[watch]
                                                set_active: get_selected_wrap_mode(&model.document, &model.selected_item_ids) == Some(WrapMode::WrapLeft),
                                                connect_toggled[sender] => move |btn| {
                                                    if btn.is_active() { sender.input(AppInput::SetWrapMode(WrapMode::WrapLeft)); }
                                                },
                                            },
                                            gtk::ToggleButton {
                                                set_label: "Der.",
                                                set_tooltip_text: Some("Imagen a la derecha, texto a la izquierda"),
                                                #[watch]
                                                set_active: get_selected_wrap_mode(&model.document, &model.selected_item_ids) == Some(WrapMode::WrapRight),
                                                connect_toggled[sender] => move |btn| {
                                                    if btn.is_active() { sender.input(AppInput::SetWrapMode(WrapMode::WrapRight)); }
                                                },
                                            },
                                        },

                                        // SvgFrame actions
                                        gtk::Button {
                                            set_label: "Import SVG",
                                            add_css_class: "suggested-action",
                                            #[watch]
                                            set_visible: is_selected_type(&model.document, &model.selected_item_ids, &ItemType::SvgFrame),
                                            connect_clicked => AppInput::ImportSvg,
                                        },

                                        // SVG Fit Mode section
                                        gtk::Separator {
                                            #[watch]
                                            set_visible: is_selected_type(&model.document, &model.selected_item_ids, &ItemType::SvgFrame),
                                        },
                                        gtk::Label {
                                            set_label: "SVG Fit Mode",
                                            add_css_class: "heading",
                                            set_xalign: 0.0,
                                            #[watch]
                                            set_visible: is_selected_type(&model.document, &model.selected_item_ids, &ItemType::SvgFrame),
                                        },
                                        gtk::Box {
                                            set_orientation: gtk::Orientation::Horizontal,
                                            set_spacing: 4,
                                            add_css_class: "linked",
                                            #[watch]
                                            set_visible: is_selected_type(&model.document, &model.selected_item_ids, &ItemType::SvgFrame),

                                            gtk::ToggleButton {
                                                set_label: "Prop.",
                                                #[watch]
                                                set_active: get_selected_svg_fit_mode(&model.document, &model.selected_item_ids) == Some(crate::svg_box::FitMode::Proportional),
                                                connect_toggled[sender] => move |btn| {
                                                    if btn.is_active() {
                                                        sender.input(AppInput::SetSvgFitMode(crate::svg_box::FitMode::Proportional));
                                                    }
                                                },
                                            },
                                            gtk::ToggleButton {
                                                set_label: "Original",
                                                #[watch]
                                                set_active: get_selected_svg_fit_mode(&model.document, &model.selected_item_ids) == Some(crate::svg_box::FitMode::Original),
                                                connect_toggled[sender] => move |btn| {
                                                    if btn.is_active() {
                                                        sender.input(AppInput::SetSvgFitMode(crate::svg_box::FitMode::Original));
                                                    }
                                                },
                                            },
                                            gtk::ToggleButton {
                                                set_label: "Stretch",
                                                #[watch]
                                                set_active: get_selected_svg_fit_mode(&model.document, &model.selected_item_ids) == Some(crate::svg_box::FitMode::Stretch),
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
                                            set_visible: is_selected_type(&model.document, &model.selected_item_ids, &ItemType::SvgFrame),
                                            connect_clicked => AppInput::FitFrameToSvg,
                                        },
                                        gtk::Button {
                                            set_label: "Refresh SVG",
                                            #[watch]
                                            set_visible: is_selected_type(&model.document, &model.selected_item_ids, &ItemType::SvgFrame),
                                            connect_clicked => AppInput::RefreshSvg,
                                        },
                                        gtk::Button {
                                            set_label: "Open in External Editor",
                                            #[watch]
                                            set_visible: is_selected_type(&model.document, &model.selected_item_ids, &ItemType::SvgFrame),
                                            connect_clicked => AppInput::OpenExternalEditor,
                                        },
                                        gtk::Button {
                                            #[watch]
                                            set_label: if model.svg_editor_path.is_some() { "Change SVG Editor" } else { "Set SVG Editor Path" },
                                            #[watch]
                                            set_visible: is_selected_type(&model.document, &model.selected_item_ids, &ItemType::SvgFrame),
                                            connect_clicked => AppInput::ChooseSvgEditor,
                                        },
                                        gtk::Label {
                                            #[watch]
                                            set_label: &get_svg_path_info(&model.document, &model.selected_item_ids),
                                            set_ellipsize: gtk::pango::EllipsizeMode::Middle,
                                            set_max_width_chars: 40,
                                            add_css_class: "caption",
                                            #[watch]
                                            set_visible: is_selected_type(&model.document, &model.selected_item_ids, &ItemType::SvgFrame),
                                        },

                                        gtk::Separator {},
                                        gtk::Label {
                                            set_label: "Ajustes de marco",
                                            add_css_class: "heading",
                                            set_xalign: 0.0,
                                        },
                                        gtk::CheckButton {
                                            set_label: Some("Mostrar borde"),
                                            #[watch]
                                            set_active: get_selected_show_border(&model.document, &model.selected_item_ids),
                                            connect_toggled[sender] => move |btn| {
                                                sender.input(AppInput::SetShowBorder(btn.is_active()));
                                            },
                                        },

                                        gtk::Separator {
                                            #[watch]
                                            set_visible: !model.selected_item_ids.is_empty(),
                                        },
                                        gtk::Button {
                                            set_label: "Align...",
                                            set_tooltip_text: Some("Open alignment panel in sidebar"),
                                            #[watch]
                                            set_visible: !model.selected_item_ids.is_empty(),
                                            connect_clicked => AppInput::EnterAlignmentMode,
                                        },
                                        gtk::Separator {
                                            #[watch]
                                            set_visible: !model.selected_item_ids.is_empty(),
                                        },
                                        gtk::Label {
                                            set_label: "Z-Order",
                                            add_css_class: "heading",
                                            set_xalign: 0.0,
                                            #[watch]
                                            set_visible: !model.selected_item_ids.is_empty(),
                                        },
                                        gtk::Box {
                                            set_orientation: gtk::Orientation::Horizontal,
                                            set_spacing: 4,
                                            add_css_class: "linked",
                                            #[watch]
                                            set_visible: !model.selected_item_ids.is_empty(),

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
            drag_offset: (0.0, 0.0),
            selected_item_ids: Vec::new(),
            initial_item_rects: std::collections::HashMap::new(),
            active_handle: None,
            is_moving: false,
            popover_pos: (0.0, 0.0),
            popover_visible: false,
            popover_page_idx: None,
            pending_drag_start_swallows: 0,
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
            canvas_ready: true,
            flow_providers: HashMap::new(),
            autoscroll_timer: None,
            show_sidebar: true,
            dirty: false,
            edit_layout_cache: None,
            render_layout_cache: Rc::new(RefCell::new(HashMap::new())),
            pending_wrap_rebuild: false,
            master_page_mode: false,
            selected_master_page_id: None,
            saved_page_0_items: None,
            alignment_ref_id: None,
            pick_alignment_ref: false,
            alignment_mode: false,
            fit_message: None,
            font_entry_value: String::new(),
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
        let is_project_loaded = matches!(message, AppInput::ProjectLoaded(..));

        let prev_snapshot_family = self.cursor_snapshot.family.clone();
        let prev_selected = self.selected_item_ids.clone();

        self.update(message.clone(), sender.clone(), root);
        self.refresh_cursor_snapshot();

        // Update the font entry whenever the cursor moves to a different family
        // or when the user switches text frames, but not when a format command
        // (SetFontFamily / SetFontSize / etc.) was just applied — those handlers
        // set font_entry_value explicitly themselves.
        let is_format_cmd = matches!(
            message,
            AppInput::SetFontFamily(_) |
            AppInput::SetFontSize(_) |
            AppInput::SetBold(_) |
            AppInput::SetItalic(_) |
            AppInput::SetUnderline(_) |
            AppInput::ClearFormat
        );
        if !is_format_cmd {
            let family_changed = self.cursor_snapshot.family != prev_snapshot_family;
            let selection_changed = self.selected_item_ids != prev_selected;
            if family_changed || selection_changed {
                self.font_entry_value = self.cursor_snapshot.family.clone().unwrap_or_default();
            }
        }

        self.update_view(widgets, sender.clone());
        rebuild_master_pages_ui(&self, &mut widgets.master_pages_listbox, sender.clone());

        // Autoscroll logic
        if matches!(message, AppInput::DragUpdate(..) | AppInput::Autoscroll) {
            let scroll_threshold = 50.0;
            let scroll_speed = 15.0;
            let mut delta_v = 0.0;
            let mut delta_h = 0.0;

            if (self.is_moving || self.active_handle.is_some()) && self.drag_current.is_some() {
                let vadj = widgets.scrolled_window.vadjustment();
                let hadj = widgets.scrolled_window.hadjustment();
                let (mx, my) = self.drag_current.unwrap();

                // Vertical scroll
                if my < vadj.value() + scroll_threshold {
                    delta_v = -scroll_speed;
                } else if my > vadj.value() + vadj.page_size() - scroll_threshold {
                    delta_v = scroll_speed;
                }

                // Horizontal scroll
                if mx < hadj.value() + scroll_threshold {
                    delta_h = -scroll_speed;
                } else if mx > hadj.value() + hadj.page_size() - scroll_threshold {
                    delta_h = scroll_speed;
                }

                if delta_v != 0.0 || delta_h != 0.0 {
                    let old_v = vadj.value();
                    let old_h = hadj.value();
                    vadj.set_value((old_v + delta_v).clamp(vadj.lower(), vadj.upper() - vadj.page_size()));
                    hadj.set_value((old_h + delta_h).clamp(hadj.lower(), hadj.upper() - hadj.page_size()));
                    
                    let actual_dv = vadj.value() - old_v;
                    let actual_dh = hadj.value() - old_h;

                    if actual_dv != 0.0 || actual_dh != 0.0 {
                        sender.input(AppInput::AdjustDragOffset(actual_dh, actual_dv));
                        
                        if self.autoscroll_timer.is_none() {
                            let s = sender.clone();
                            self.autoscroll_timer = Some(gtk::glib::timeout_add_local_once(
                                std::time::Duration::from_millis(20),
                                move || {
                                    s.input(AppInput::Autoscroll);
                                }
                            ));
                        }
                    } else {
                        self.autoscroll_timer = None;
                    }
                } else {
                    self.autoscroll_timer = None;
                }
            } else {
                self.autoscroll_timer = None;
            }
        } else if matches!(message, AppInput::DragEnd | AppInput::DragStart(..)) {
            self.autoscroll_timer = None;
        }

        if is_project_loaded {
            // Schedule CanvasReady one frame after the load.  The GTK layout
            // pass runs before this timeout fires, so by the time DragStart
            // is unblocked the canvas origin is correctly calibrated.
            let s = sender.clone();
            gtk::glib::timeout_add_local_once(
                std::time::Duration::from_millis(50),
                move || { s.input(AppInput::CanvasReady); },
            );
        }

        if self.request_focus {
            self.request_focus = false;
            let vadj = widgets.scrolled_window.vadjustment();
            let hadj = widgets.scrolled_window.hadjustment();
            let saved_v = vadj.value();
            let saved_h = hadj.value();
            widgets.canvas.grab_focus();
            vadj.set_value(saved_v);
            hadj.set_value(saved_h);
        }
    }

    fn update(&mut self, message: Self::Input, sender: ComponentSender<Self>, root: &Self::Root) {
        match message {
            AppInput::SetPageLayout(layout) => {
                self.page_layout = layout;
            }
            AppInput::ShowPreferences => {
                show_preferences_dialog(root);
            }
            AppInput::ShowKeyboardShortcuts => {
                show_keyboard_shortcuts_window(root);
            }
            AppInput::AddPage => {
                self.document.pages.push(crate::document::Page::default());
                self.dirty = true;
            }
            AppInput::ClosePopover => {
                self.close_popover_and_skip_drag();
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
                    self.close_popover_and_skip_drag();

                    let item_dims = self.selected_item_ids.first()
                        .and_then(|id| self.find_item(id))
                        .map(|(_, item)| (item.width * SCALE, item.height * SCALE));
                    if let Some((w, h)) = item_dims {
                        if let Some(id) = self.selected_item_ids.first().cloned() {
                            self.invalidate_edit_layout();
                            if let Some(s) = self.ensure_cursor_visible_cached(&id, w, h, SCALE) {
                                if let Some(tb) = self.get_editing_text_box_mut() {
                                    tb.scroll_y = s;
                                }
                            }
                        }
                    }
                }
            }
            AppInput::ImportImage => {
                if !self.selected_item_ids.is_empty() {
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
                if let Some(id) = self.selected_item_ids.first().cloned() {
                    if let Some((_, item)) = self.find_item_mut(&id) {
                        if let ItemContent::Image(ib) = &mut item.content {
                            ib.image_path = Some(path);
                        }
                    }
                }
                self.close_popover_and_skip_drag();
            }
            AppInput::FitFrameToImage => {
                let id = self.selected_item_ids.first().cloned();
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
                self.close_popover_and_skip_drag();
            }
            AppInput::SetImageFitMode(mode) => {
                if let Some(id) = self.selected_item_ids.first().cloned() {
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
                if !self.selected_item_ids.is_empty() {
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
                        if let Some(id) = self.selected_item_ids.first().cloned() {
                            if let Some((_, item)) = self.find_item_mut(&id) {
                                if let ItemContent::Svg(sb) = &mut item.content {
                                    sb.svg_path = path;
                                }
                            }
                        }
                    }
                    Err(e) => eprintln!("Failed to load SVG: {}", e),
                }
                self.close_popover_and_skip_drag();
            }
            AppInput::SetSvgFitMode(mode) => {
                if let Some(id) = self.selected_item_ids.first().cloned() {
                    if let Some((_, item)) = self.find_item_mut(&id) {
                        if let ItemContent::Svg(sb) = &mut item.content {
                            sb.fit_mode = mode;
                        }
                    }
                }
            }
            AppInput::FitFrameToSvg => {
                let id = self.selected_item_ids.first().cloned();
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
                self.close_popover_and_skip_drag();
            }
            AppInput::RefreshSvg => {
                if let Some(id) = self.selected_item_ids.first().cloned() {
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
                self.close_popover_and_skip_drag();
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
                if let Some(id) = self.selected_item_ids.first().cloned() {
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
                self.close_popover_and_skip_drag();
            }
            AppInput::BringToFront => {
                for id in self.selected_item_ids.clone() {
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
                for id in self.selected_item_ids.clone().into_iter().rev() {
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
                for id in self.selected_item_ids.clone().into_iter().rev() {
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
                for id in self.selected_item_ids.clone() {
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
                let to_delete: std::collections::HashSet<String> =
                    self.selected_item_ids.iter().cloned().collect();
                let chain_roots: std::collections::HashSet<String> = to_delete.iter()
                    .filter(|id| is_in_chain(&self.document, id))
                    .map(|id| find_chain_root(&self.document, id))
                    .collect();

                for root_id in &chain_roots {
                    let editing_id = if self.is_editing {
                        self.selected_item_ids.first()
                            .filter(|id| find_chain_root(&self.document, id) == *root_id)
                            .map(|id| id.as_str())
                    } else {
                        None
                    };
                    let sc = self.scale();
                    reflow_chain(&mut self.document, root_id, sc, editing_id);
                }

                for root_id in chain_roots {
                    let chain = collect_chain(&self.document, &root_id);
                    let remaining: Vec<String> = chain.iter()
                        .filter(|id| !to_delete.contains(*id))
                        .cloned()
                        .collect();
                    if remaining.is_empty() {
                        continue;
                    }

                    let root_tb = chain_frame_textbox(&self.document, &root_id).cloned();
                    if let Some(root_tb) = root_tb {
                        let new_root_id = remaining[0].clone();
                        set_chain_links(&mut self.document, &remaining);
                        set_chain_frame_content(
                            &mut self.document,
                            &new_root_id,
                            root_tb.text.clone(),
                            root_tb.attributes.clone(),
                            0,
                        );
                        if remaining.len() >= 2 {
                            let sc = self.scale();
                            reflow_chain(&mut self.document, &new_root_id, sc, None);
                        }
                    }
                }

                self.selected_item_ids.clear();
                for page in &mut self.document.pages {
                    page.items.retain(|item| !to_delete.contains(&item.id));
                }
                self.dirty = true;
            }
            AppInput::PasteText(text) => {
                if !self.is_editing { return; }
                self.flush_history(); // push pre-paste state, close typing run
                if let Some(tb) = self.get_editing_text_box_mut() {
                    tb.insert_text(&text);
                    self.dirty = true;
                }
                self.invalidate_edit_layout();

                if let Some(id) = self.selected_item_ids.first().cloned() {
                    if is_in_chain(&self.document, &id) {
                        let sc = self.scale();
                        reflow_chain(&mut self.document, &id, sc, Some(id.as_str()));
                    }
                }

                let item_dims = self.selected_item_ids.first()
                    .and_then(|id| self.find_item(id))
                    .map(|(_, item)| (item.width * SCALE, item.height * SCALE));
                if let Some((w, h)) = item_dims {
                    if let Some(id) = self.selected_item_ids.first().cloned() {
                        if let Some(s) = self.ensure_cursor_visible_cached(&id, w, h, SCALE) {
                            if let Some(tb) = self.get_editing_text_box_mut() {
                                tb.scroll_y = s;
                            }
                        }
                    }
                }
            }
            AppInput::TextKeyPressed(key, state) => {
                let ctrl = state.contains(gdk::ModifierType::CONTROL_MASK);

                if ctrl && key == gdk::Key::F1 {
                    sender.input(AppInput::ShowKeyboardShortcuts);
                    return;
                }

                if !self.is_editing {
                    if ctrl {
                        match key {
                            gdk::Key::plus | gdk::Key::KP_Add | gdk::Key::equal => {
                                sender.input(AppInput::AddPage);
                                return;
                            }
                            gdk::Key::f | gdk::Key::F => {
                                sender.input(AppInput::FitPageToWindow);
                                return;
                            }
                            gdk::Key::n | gdk::Key::N => {
                                sender.input(AppInput::NewProject);
                                return;
                            }
                            gdk::Key::o | gdk::Key::O => {
                                sender.input(AppInput::OpenProject);
                                return;
                            }
                            gdk::Key::s | gdk::Key::S => {
                                sender.input(AppInput::SaveProject);
                                return;
                            }
                            gdk::Key::p | gdk::Key::P => {
                                sender.input(AppInput::ExportPdf);
                                return;
                            }
                            gdk::Key::l | gdk::Key::L => {
                                sender.input(AppInput::ToggleSidebar);
                                return;
                            }
                            gdk::Key::t | gdk::Key::T => {
                                sender.input(AppInput::SetCreateFrameType(ItemType::TextFrame));
                                return;
                            }
                            gdk::Key::i | gdk::Key::I => {
                                sender.input(AppInput::SetCreateFrameType(ItemType::ImageFrame));
                                return;
                            }
                            gdk::Key::g | gdk::Key::G => {
                                sender.input(AppInput::SetCreateFrameType(ItemType::SvgFrame));
                                return;
                            }
                            _ => {}
                        }
                    }

                    if (key == gdk::Key::Delete || key == gdk::Key::BackSpace) && !self.selected_item_ids.is_empty() {
                        sender.input(AppInput::DeleteItem);
                    }
                    return;
                }

                // Global shortcuts that should work even when editing
                if ctrl {
                    match key {
                        gdk::Key::f | gdk::Key::F => {
                            sender.input(AppInput::FitPageToWindow);
                            return;
                        }
                        gdk::Key::n | gdk::Key::N => {
                            sender.input(AppInput::NewProject);
                            return;
                        }
                        gdk::Key::s | gdk::Key::S => {
                            sender.input(AppInput::SaveProject);
                            return;
                        }
                        gdk::Key::o | gdk::Key::O => {
                            sender.input(AppInput::OpenProject);
                            return;
                        }
                        gdk::Key::p | gdk::Key::P => {
                            sender.input(AppInput::ExportPdf);
                            return;
                        }
                        _ => {}
                    }
                }

                // Classify whether this key will modify text BEFORE calling handle_key,
                // so we can push the pre-edit state to the undo stack first.
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
                    let a = tb.handle_key(key, state);
                    if will_modify {
                        self.dirty = true;
                        self.invalidate_edit_layout();
                    }
                    a
                } else {
                    return;
                };

                match action {
                    KeyAction::ExitEdit => {
                        if let Some(id) = self.selected_item_ids.first().cloned() {
                            if is_in_chain(&self.document, &id) {
                                let sc = self.scale();
                                reflow_chain(&mut self.document, &id, sc, Some(id.as_str()));
                                self.reflow_version = self.reflow_version.wrapping_add(1);
                            }
                        }
                        self.typing_run_active = false;
                        self.is_editing = false;
                        *self.editing_flag.borrow_mut() = false;
                        self.invalidate_edit_layout();
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
                        let frame_w_px = self.selected_item_ids.first()
                            .and_then(|id| self.find_item(id))
                            .map(|(_, item)| item.width * SCALE)
                            .unwrap_or(0.0);
                        if let Some(id) = self.selected_item_ids.first().cloned() {
                            self.move_cursor_vertical_cached(&id, up, extend, frame_w_px, SCALE);
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
                    self.invalidate_edit_layout();

                    // Immediately reflow chained frames so downstream frames
                    // update live during typing (backspace, delete, typing, return).
                    let chained_id = self.selected_item_ids.first().cloned();
                    if let Some(ref id) = chained_id {
                        if is_in_chain(&self.document, id) {
                            let sc = self.scale();
                            reflow_chain(&mut self.document, id, sc, Some(id.as_str()));
                            self.reflow_version = self.reflow_version.wrapping_add(1);
                        }
                    }
                }

                let item_dims = self.selected_item_ids.first()
                    .and_then(|id| self.find_item(id))
                    .map(|(_, item)| (item.width * SCALE, item.height * SCALE));
                if let Some((w, h)) = item_dims {
                    if let Some(id) = self.selected_item_ids.first().cloned() {
                        if let Some(s) = self.ensure_cursor_visible_cached(&id, w, h, SCALE) {
                            if let Some(tb) = self.get_editing_text_box_mut() {
                                tb.scroll_y = s;
                            }
                        }
                    }
                }
            }
            AppInput::Zoom(delta) => {
                self.zoom = (self.zoom + delta * 0.1).clamp(0.1, 5.0);
            }
            AppInput::ScrollText(dy) => {
                if !self.is_editing { return; }
                let item_dims = self.selected_item_ids.first()
                    .and_then(|id| self.find_item(id))
                    .map(|(_, item)| (item.width * SCALE, item.height * SCALE));
                if let Some((w, h)) = item_dims {
                    if let Some(id) = self.selected_item_ids.first().cloned() {
                        if let Some(total_h) = self.required_height_cached(&id, w, SCALE) {
                            if let Some(tb) = self.get_editing_text_box_mut() {
                                let max_scroll = (total_h - h).max(0.0);
                                tb.scroll_y = (tb.scroll_y + dy * 40.0).clamp(0.0, max_scroll);
                            }
                        }
                    }
                }
            }
            AppInput::DragStart(x, y, state) => {
                if !self.canvas_ready { return; }

                if self.pick_alignment_ref {
                    if let Some((_, item)) = self.hit_test_all_pages(x, y) {
                        self.alignment_ref_id = Some(item.id.clone());
                    } else {
                        self.alignment_ref_id = None;
                    }
                    self.pick_alignment_ref = false;
                    return;
                }

                // Swallow the click that dismissed a popover so it doesn't
                // accidentally move items or change current_page.
                if self.pending_drag_start_swallows > 0 {
                    self.pending_drag_start_swallows -= 1;
                    return;
                }
                let is_ctrl = state.contains(gdk::ModifierType::CONTROL_MASK);
                let x_mm = x / self.scale();
                let y_mm = y / self.scale();

                self.drag_offset = (0.0, 0.0);

                // Check if the user clicked the link-out button on any text frame
                if !self.is_editing && !is_ctrl {
                    for (page_idx, page) in self.document.pages.iter().enumerate() {
                        let (off_x, off_y) = self.get_page_offset(page_idx);
                        let local_x = x_mm - off_x;
                        let local_y = y_mm - off_y;
                        for item in &page.items {
                            if let ItemContent::Text(tb) = &item.content {
                                if tb.next_frame_id.is_some() { continue; }
                                let pango_ctx = make_pango_ctx();
                                let w_px = item.width * SCALE;
                                let h_px = item.height * SCALE;
                                if tb.overflows_frame(&pango_ctx, w_px, h_px, SCALE)
                                    && hit_link_button_mm(item, local_x, local_y)
                                {
                                    self.link_drag_active = true;
                                    self.link_drag_source_id = Some(item.id.clone());
                                    self.selected_item_ids = vec![item.id.clone()];
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
                        if item.content.item_type() == ItemType::TextFrame && Some(item.id.clone()) == self.selected_item_ids.first().cloned() {
                            let (off_x, off_y) = self.get_page_offset(page_idx);
                            let local_x = x_mm - off_x;
                            let local_y = y_mm - off_y;
                            let pos = if matches!(item.content, ItemContent::Text(_)) {
                                self.hit_test_cached(&item.id, item.x, item.y, local_x, local_y, SCALE, item.width * SCALE)
                                    .unwrap_or(0)
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
                    if let Some(id) = self.selected_item_ids.first().cloned() {
                        if is_in_chain(&self.document, &id) {
                            let sc = self.scale();
                            reflow_chain(&mut self.document, &id, sc, Some(id.as_str()));
                            self.reflow_version = self.reflow_version.wrapping_add(1);
                        }
                    }
                    self.is_editing = false;
                    *self.editing_flag.borrow_mut() = false;
                    self.invalidate_edit_layout();
                    if let Some(tb) = self.get_editing_text_box_mut() {
                        tb.selection_anchor = None;
                        tb.scroll_y = 0.0;
                    }
                }

                self.drag_start = Some((x, y));
                self.drag_current = Some((x, y));

                let handle_hit = if let Some(selected_id) = self.selected_item_ids.first().cloned() {
                    if let Some((page_idx, item)) = self.find_item(&selected_id) {
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
                    self.initial_item_rects.clear();
                    self.initial_item_rects.insert(self.selected_item_ids[0].clone(), (ix, iy, iw, ih));
                    self.current_page = p_idx;
                    self.request_focus = true;
                    return;
                }

                if let Some((page_idx, item)) = self.hit_test_all_pages(x, y) {
                    if is_ctrl {
                        if let Some(pos) = self.selected_item_ids.iter().position(|id| id == &item.id) {
                            self.selected_item_ids.remove(pos);
                            self.is_moving = false;
                        } else {
                            self.selected_item_ids.push(item.id.clone());
                            self.is_moving = true;
                        }
                    } else {
                        if !self.selected_item_ids.contains(&item.id) {
                            self.selected_item_ids = vec![item.id.clone()];
                        }
                        self.is_moving = true;
                    }
                    
                    self.initial_item_rects.clear();
                    for id in &self.selected_item_ids {
                        if let Some((_, it)) = self.find_item(id) {
                            self.initial_item_rects.insert(id.clone(), (it.x, it.y, it.width, it.height));
                        }
                    }
                    self.current_page = page_idx;
                    self.request_focus = true;
                } else {
                    if !is_ctrl {
                        self.selected_item_ids.clear();
                    }
                    self.initial_item_rects.clear();
                    self.is_moving = false;
                    self.active_handle = None;
                    self.request_focus = true;
                }
            }
            AppInput::DragUpdate(offset_x, offset_y) => {
                self.drag_offset = (offset_x, offset_y);
                self.process_drag();
            }
            AppInput::AdjustDragOffset(dx, dy) => {
                self.drag_offset.0 += dx;
                self.drag_offset.1 += dy;
                self.process_drag();
            }
            AppInput::ToggleSidebar => {
                self.show_sidebar = !self.show_sidebar;
            }
            AppInput::NewProject => {
                crate::app_io::new_project_dialog(root, sender, self.dirty);
            }
            AppInput::NewProjectConfirmed => {
                self.reset_to_new_project();
            }
            AppInput::MaybeReflow(_version) => {}
            AppInput::DeletePage => {
                let page_idx = self.popover_page_idx.unwrap_or(self.current_page);
                if self.document.pages.len() > 1 && page_idx < self.document.pages.len() {
                    self.document.pages.remove(page_idx);
                    self.selected_item_ids.clear();
                    self.current_page = page_idx.min(self.document.pages.len().saturating_sub(1));
                    self.popover_page_idx = None;
                    self.popover_visible = false;
                    self.dirty = true;
                    self.rebuild_flow_providers();
                }
            }
            AppInput::CreateMasterPage => {
                let page = &self.document.pages[self.current_page];
                let name = format!("Master Page {}", self.document.master_pages.len() + 1);
                let mp = MasterPage::from_page(name, page);
                self.document.master_pages.push(mp);
                self.dirty = true;
                self.close_popover_and_skip_drag();
            }
            AppInput::ApplyMasterPage(mp_id) => {
                if let Some(page) = self.document.pages.get_mut(self.current_page) {
                    page.master_page = Some(mp_id);
                }
                self.dirty = true;
                self.close_popover_and_skip_drag();
            }
            AppInput::RenameMasterPage(mp_id, new_name) => {
                if let Some(mp) = self.document.master_pages.iter_mut().find(|m| m.id == mp_id) {
                    mp.name = new_name;
                }
                self.dirty = true;
            }
            AppInput::RemoveMasterPageFromPage => {
                if let Some(page) = self.document.pages.get_mut(self.current_page) {
                    page.master_page = None;
                }
                self.dirty = true;
                self.close_popover_and_skip_drag();
            }
            AppInput::DeleteMasterPage(mp_id) => {
                self.document.master_pages.retain(|m| m.id != mp_id);
                // Clear reference on all pages using this master
                for page in &mut self.document.pages {
                    if page.master_page.as_deref() == Some(&mp_id) {
                        page.master_page = None;
                    }
                }
                self.dirty = true;
                self.close_popover_and_skip_drag();
            }
            AppInput::EnterMasterPageMode(mp_id) => {
                let id = if mp_id.is_empty() {
                    self.document.master_pages.first().map(|m| m.id.clone()).unwrap_or_else(|| {
                        // No master pages exist yet — create one automatically
                        let new_id = uuid::Uuid::new_v4().to_string();
                        let name = format!("Master Page {}", self.document.master_pages.len() + 1);
                        self.document.master_pages.push(crate::document::MasterPage {
                            id: new_id.clone(),
                            name,
                            items: vec![],
                        });
                        self.dirty = true;
                        new_id
                    })
                } else {
                    mp_id
                };
                if !id.is_empty() {
                    self.enter_master_page_mode(id);
                }
                self.close_popover_and_skip_drag();
            }
            AppInput::ExitMasterPageMode => {
                self.exit_master_page_mode();
                self.close_popover_and_skip_drag();
            }
            AppInput::NewMasterPage => {
                let id = uuid::Uuid::new_v4().to_string();
                let name = format!("Master Page {}", self.document.master_pages.len() + 1);
                self.document.master_pages.push(crate::document::MasterPage {
                    id: id.clone(),
                    name,
                    items: vec![],
                });
                self.dirty = true;
                self.enter_master_page_mode(id);
                self.close_popover_and_skip_drag();
            }
            AppInput::AlignLeft => {
                self.align_selected_horizontal(AlignH::Left);
                self.close_popover_and_skip_drag();
            }
            AppInput::AlignCenterH => {
                self.align_selected_horizontal(AlignH::Center);
                self.close_popover_and_skip_drag();
            }
            AppInput::AlignRight => {
                self.align_selected_horizontal(AlignH::Right);
                self.close_popover_and_skip_drag();
            }
            AppInput::AlignTop => {
                self.align_selected_vertical(AlignV::Top);
                self.close_popover_and_skip_drag();
            }
            AppInput::AlignCenterV => {
                self.align_selected_vertical(AlignV::Center);
                self.close_popover_and_skip_drag();
            }
            AppInput::AlignBottom => {
                self.align_selected_vertical(AlignV::Bottom);
                self.close_popover_and_skip_drag();
            }
            AppInput::StartPickAlignmentRef => {
                self.pick_alignment_ref = true;
                // Don't close popover here — we're in sidebar alignment mode.
            }
            AppInput::SetAlignmentRef(ref_id) => {
                self.alignment_ref_id = if ref_id.is_empty() { None } else { Some(ref_id) };
                self.pick_alignment_ref = false;
                self.popover_visible = true;
            }
            AppInput::EnterAlignmentMode => {
                self.alignment_mode = true;
                self.pick_alignment_ref = false;
                self.popover_visible = false;
                self.close_popover_and_skip_drag();
            }
            AppInput::ExitAlignmentMode => {
                self.alignment_mode = false;
                self.pick_alignment_ref = false;
            }
            AppInput::FitPageToWindow => {
                let page_w = self.document.width * SCALE;
                let page_h = self.document.height * SCALE;
                if page_w > 0.0 && page_h > 0.0 {
                    let vw = root.width() as f64;
                    let vh = root.height() as f64;
                    self.zoom = (vw / page_w).min(vh / page_h).max(0.1).min(5.0);
                    let msg = format!("Zoom: {:.0}% (fit to window)", self.zoom * 100.0);
                    self.fit_message = Some(msg);
                    let s = sender.clone();
                    gtk::glib::timeout_add_local_once(
                        std::time::Duration::from_millis(3000),
                        move || { s.input(AppInput::ClearFitMessage); },
                    );
                }
                self.request_focus = true;
            }
            AppInput::ClearFitMessage => {
                self.fit_message = None;
            }
            AppInput::RightClick(x, y) => {
                if self.pick_alignment_ref {
                    if let Some((_page_idx, item)) = self.hit_test_all_pages(x, y) {
                        // Clicked on an item — set it as alignment reference
                        self.alignment_ref_id = Some(item.id.clone());
                    } else {
                        // Clicked on empty space — use page as reference
                        self.alignment_ref_id = None;
                    }
                    self.pick_alignment_ref = false;
                    self.popover_pos = (x, y);
                    self.popover_visible = true;
                    return;
                }
                if let Some((page_idx, item)) = self.hit_test_all_pages(x, y) {
                    if !self.selected_item_ids.contains(&item.id) {
                        self.selected_item_ids = vec![item.id.clone()];
                    }
                    self.current_page = page_idx;
                    self.popover_page_idx = Some(page_idx);
                    self.popover_pos = (x, y);
                    self.popover_visible = true;
                } else {
                    self.popover_page_idx = self.page_at_canvas_coords(x, y);
                    if let Some(page_idx) = self.popover_page_idx {
                        self.current_page = page_idx;
                    }
                    self.selected_item_ids.clear();
                    self.popover_pos = (x, y);
                    self.popover_visible = true;
                }
            }
            AppInput::DoubleClick(x, y) => {
                let x_mm = x / self.scale();
                let y_mm = y / self.scale();

                // Check for double click on handles
                if let Some(selected_id) = self.selected_item_ids.first().cloned().clone() {
                    if let Some((page_idx, item)) = self.find_item(&selected_id) {
                        let (off_x, off_y) = self.get_page_offset(page_idx);
                        let local_x = x_mm - off_x;
                        let local_y = y_mm - off_y;
                        
                        let handles = get_handle_positions(&item);
                        // Handle 5 is bottom center
                        let (hx, hy) = handles[5]; 
                        
                        if (local_x - hx).abs() < 2.0 && (local_y - hy).abs() < 2.0 {
                            if !is_in_chain(&self.document, &selected_id) {
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
                }

                if let Some((page_idx, item)) = self.hit_test_all_pages(x, y) {
                    let (off_x, off_y) = self.get_page_offset(page_idx);
                    let local_x = x_mm - off_x;
                    let local_y = y_mm - off_y;
                    let click_pos = if matches!(item.content, ItemContent::Text(_)) {
                        self.hit_test_cached(&item.id, item.x, item.y, local_x, local_y, SCALE, item.width * SCALE)
                            .unwrap_or(0)
                    } else { 0 };
                    let id = item.id.clone();
                    let item_type = item.content.item_type();

                    match item_type {
                        ItemType::TextFrame => {
                            let already_editing = self.is_editing
                                && self.selected_item_ids.first().as_deref() == Some(&id);
                            self.selected_item_ids = vec![id];
                            self.current_page = page_idx;
                            self.is_editing = true;
                            *self.editing_flag.borrow_mut() = true;
                            self.invalidate_edit_layout();
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
                            self.selected_item_ids = vec![id];
                            self.current_page = page_idx;
                            sender.input(AppInput::ImportImage);
                        }
                        ItemType::SvgFrame => {
                            self.selected_item_ids = vec![id];
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
                        let width = (sx - cx).abs() / self.scale();
                        let height = (sy - cy).abs() / self.scale();
                        if width > 1.0 && height > 1.0 {
                            let page_idx = self.page_index_for_canvas_coords(sx.min(cx), sy.min(cy));
                            let (off_x, off_y) = self.get_page_offset(page_idx);
                            let x = (sx.min(cx) / self.scale()) - off_x;
                            let y = (sy.min(cy) / self.scale()) - off_y;
                            if let Some(page) = self.document.pages.get_mut(page_idx) {
                                let new_id = uuid::Uuid::new_v4().to_string();
                                page.items.push(Item {
                                    id: new_id.clone(),
                                    x, y, width, height,
                                    rotation: 0.0,
                                    show_border: true,
                                    content: match self.create_frame_type {
                                        ItemType::TextFrame => ItemContent::Text(TextBox::default()),
                                        ItemType::ImageFrame => ItemContent::Image(ImageBox::default()),
                                        ItemType::SvgFrame => ItemContent::Svg(SvgBox::default()),
                                        ItemType::Shape => ItemContent::Shape,
                                    },
                                });
                                self.selected_item_ids = vec![new_id];
                                self.current_page = page_idx;
                            }
                        }
                    }
                }
                let was_resize = self.active_handle.is_some();
                self.drag_start = None;
                self.drag_current = None;
                self.initial_item_rects.clear();
                self.active_handle = None;
                self.is_moving = false;
                if was_resize {
                    let chained_id = self.selected_item_ids.first().cloned();
                    if let Some(ref id) = chained_id {
                        if is_in_chain(&self.document, id) {
                            let sc = self.scale();
                            reflow_chain(&mut self.document, id, sc, if self.is_editing { Some(id.as_str()) } else { None });
                            self.reflow_version = self.reflow_version.wrapping_add(1);
                        }
                    }
                }
                if self.pending_wrap_rebuild {
                    self.rebuild_flow_providers();
                    self.pending_wrap_rebuild = false;
                }
                self.request_focus = true;
            }
            AppInput::SaveProject => {
                let mut save_doc = self.document.clone();
                strip_chain_downstream_texts(&mut save_doc);
                save_project_dialog(
                    root,
                    sender.clone(),
                    save_doc,
                    self.last_save_path.as_deref(),
                );
            }
            AppInput::ProjectSaved(path) => {
                self.last_save_path = Some(path);
                self.dirty = false;
                show_info_dialog(root, "Project Saved", "The project has been successfully saved.");
            }
            AppInput::OpenProject => {
                open_project_dialog(root, sender.clone());
            }
            AppInput::ExportPdf => {
                export_pdf_dialog(
                    root,
                    self.document.clone(),
                    self.image_surfaces.clone(),
                    self.svg_handles.clone(),
                );
            }
            AppInput::ProjectLoaded(doc, path) => {
                self.document = doc;
                for root_id in collect_all_chain_roots(&self.document) {
                    reflow_chain(&mut self.document, &root_id, SCALE, None);
                }
                self.last_save_path = Some(path);
                self.dirty = false;
                self.selected_item_ids.clear();
                self.is_editing = false;
                *self.editing_flag.borrow_mut() = false;
                self.invalidate_edit_layout();
                self.pending_wrap_rebuild = false;
                
                // Reset all interaction states to prevent jumps on first click
                self.drag_start = None;
                self.drag_current = None;
                self.is_moving = false;
                self.active_handle = None;
                self.initial_item_rects.clear();
                self.text_drag_active = false;
                self.link_drag_active = false;
                self.link_drag_source_id = None;
                self.link_drag_start = None;
                self.link_drag_current = None;
                self.popover_visible = false;
                self.popover_page_idx = None;
                self.pending_drag_start_swallows = 0;
                self.current_page = 0;
                
                // Reset internal counters
                self.undo_version = 0;
                self.reflow_version = 0;
                self.typing_run_active = false;
                self.render_layout_cache.borrow_mut().clear();

                self.load_all_assets();
                self.rebuild_flow_providers();

                // Block drag input until the canvas layout pass fires (size_allocate
                // → CanvasReady).  This prevents the first click from jumping because
                // GestureDrag would capture a stale widget origin.
                self.canvas_ready = false;
                self.swallow_upcoming_drag_start();
            }
            AppInput::SetBold(value) => {
                if value == self.cursor_snapshot.bold { return; }
                let editing = self.is_editing;
                self.flush_history(); // push pre-format state, close typing run
                let mut need_enter_edit = false;
                if let Some(id) = self.selected_item_ids.first().cloned().clone() {
                    let range = self.selected_text_operation_range(&id, editing);
                    if let Some((_, item)) = self.find_item_mut(&id) {
                        if let ItemContent::Text(ref mut tb) = item.content {
                            if editing {
                                let pos = tb.selection_range().map(|(s, _)| s).unwrap_or(tb.cursor_pos);
                                if tb.get_attr_at(pos).bold != value {
                                    let (s, e) = range.unwrap_or_else(|| tb.selection_or_word_range());
                                    if s < e { tb.apply_format(s, e, AttrValue::Bold(value)); }
                                }
                            } else {
                                if let Some((s, e)) = range {
                                    if s < e {
                                        tb.apply_format(s, e, AttrValue::Bold(value));
                                        need_enter_edit = true;
                                    }
                                }
                            }
                        }
                    }
                    if is_in_chain(&self.document, &id) {
                        let sc = self.scale();
                        reflow_chain(&mut self.document, &id, sc, if self.is_editing { Some(id.as_str()) } else { None });
                    }
                }
                if need_enter_edit {
                    self.is_editing = true;
                    *self.editing_flag.borrow_mut() = true;
                }
                self.invalidate_edit_layout();
                self.request_focus = true;
            }
            AppInput::SetItalic(value) => {
                if value == self.cursor_snapshot.italic { return; }
                let editing = self.is_editing;
                self.flush_history(); // push pre-format state, close typing run
                let mut need_enter_edit = false;
                if let Some(id) = self.selected_item_ids.first().cloned().clone() {
                    let range = self.selected_text_operation_range(&id, editing);
                    if let Some((_, item)) = self.find_item_mut(&id) {
                        if let ItemContent::Text(ref mut tb) = item.content {
                            if editing {
                                let pos = tb.selection_range().map(|(s, _)| s).unwrap_or(tb.cursor_pos);
                                if tb.get_attr_at(pos).italic != value {
                                    let (s, e) = range.unwrap_or_else(|| tb.selection_or_word_range());
                                    if s < e { tb.apply_format(s, e, AttrValue::Italic(value)); }
                                }
                            } else {
                                if let Some((s, e)) = range {
                                    if s < e {
                                        tb.apply_format(s, e, AttrValue::Italic(value));
                                        need_enter_edit = true;
                                    }
                                }
                            }
                        }
                    }
                    if is_in_chain(&self.document, &id) {
                        let sc = self.scale();
                        reflow_chain(&mut self.document, &id, sc, if self.is_editing { Some(id.as_str()) } else { None });
                    }
                }
                if need_enter_edit {
                    self.is_editing = true;
                    *self.editing_flag.borrow_mut() = true;
                }
                self.invalidate_edit_layout();
                self.request_focus = true;
            }
            AppInput::SetUnderline(value) => {
                if value == self.cursor_snapshot.underline { return; }
                let editing = self.is_editing;
                self.flush_history(); // push pre-format state, close typing run
                let mut need_enter_edit = false;
                if let Some(id) = self.selected_item_ids.first().cloned().clone() {
                    let range = self.selected_text_operation_range(&id, editing);
                    if let Some((_, item)) = self.find_item_mut(&id) {
                        if let ItemContent::Text(ref mut tb) = item.content {
                            if editing {
                                let pos = tb.selection_range().map(|(s, _)| s).unwrap_or(tb.cursor_pos);
                                if tb.get_attr_at(pos).underline != value {
                                    let (s, e) = range.unwrap_or_else(|| tb.selection_or_word_range());
                                    if s < e { tb.apply_format(s, e, AttrValue::Underline(value)); }
                                }
                            } else {
                                if let Some((s, e)) = range {
                                    if s < e {
                                        tb.apply_format(s, e, AttrValue::Underline(value));
                                        need_enter_edit = true;
                                    }
                                }
                            }
                        }
                    }
                    if is_in_chain(&self.document, &id) {
                        let sc = self.scale();
                        reflow_chain(&mut self.document, &id, sc, if self.is_editing { Some(id.as_str()) } else { None });
                    }
                }
                if need_enter_edit {
                    self.is_editing = true;
                    *self.editing_flag.borrow_mut() = true;
                }
                self.invalidate_edit_layout();
                self.request_focus = true;
            }
            AppInput::SetFontFamily(family) => {
                let editing = self.is_editing;
                self.flush_history(); // push pre-format state, close typing run
                if let Some(id) = self.selected_item_ids.first().cloned().clone() {
                    let range = self.selected_text_operation_range(&id, editing);
                    if let Some((_, item)) = self.find_item_mut(&id) {
                        if let ItemContent::Text(ref mut tb) = item.content {
                            // Always update the base font description so that:
                            // 1) newly-typed text uses the chosen family,
                            // 2) export falls back to the right font when no per-span
                            //    attribute covers the current position.
                            // Use set_family() + to_str() so the exact family name is
                            // preserved (Pango never strips optical-size suffixes this
                            // way); the trailing comma that to_str() adds when size==0
                            // is harmless because from_string() parses it back
                            // correctly.
                            let mut fd = gtk::pango::FontDescription::from_string(&tb.font_description);
                            fd.set_family(&family);
                            tb.font_description = fd.to_str().to_string();

                            // Apply a per-span Family attribute to the text range so
                            // the existing content also uses the new font.
                            if editing {
                                let (s, e) = range.unwrap_or_else(|| tb.selection_or_word_range());
                                if s < e {
                                    tb.apply_format(s, e, AttrValue::Family(family.clone()));
                                }
                            } else if let Some((s, e)) = range {
                                if s < e {
                                    tb.apply_format(s, e, AttrValue::Family(family.clone()));
                                    self.is_editing = true;
                                    *self.editing_flag.borrow_mut() = true;
                                }
                            }
                        }
                    }
                    if is_in_chain(&self.document, &id) {
                        let sc = self.scale();
                        reflow_chain(&mut self.document, &id, sc, if self.is_editing { Some(id.as_str()) } else { None });
                    }
                }
                self.font_entry_value = family;
                self.invalidate_edit_layout();
                self.request_focus = true;
            }
            AppInput::SetFontSize(size) => {
                // Guard against the #[watch] set_value → value-changed feedback loop:
                // cursor_snapshot always reflects the effective size at the cursor, so a
                // matching value means the spin was updated programmatically, not by the user.
                if (size - self.cursor_snapshot.size_pt.unwrap_or(11.0)).abs() < 0.05 { return; }
                let editing = self.is_editing;
                self.flush_history(); // push pre-format state, close typing run
                if let Some(id) = self.selected_item_ids.first().cloned().clone() {
                    let range = self.selected_text_operation_range(&id, editing);
                    if let Some((_, item)) = self.find_item_mut(&id) {
                        if let ItemContent::Text(ref mut tb) = item.content {
                            if editing {
                                let pos = tb.selection_range().map(|(s, _)| s).unwrap_or(tb.cursor_pos);
                                let current = tb.effective_snapshot_at(pos).size_pt.unwrap_or(11.0);
                                if (size - current).abs() > 0.05 {
                                    let (s, e) = range.unwrap_or_else(|| tb.selection_or_word_range());
                                    if s < e {
                                        tb.apply_format(s, e, AttrValue::Size(size));
                                    }
                                }
                            } else {
                                if let Some((s, e)) = range {
                                    if s < e {
                                        tb.apply_format(s, e, AttrValue::Size(size));
                                        self.is_editing = true;
                                        *self.editing_flag.borrow_mut() = true;
                                    }
                                }
                            }
                        }
                    }
                    if is_in_chain(&self.document, &id) {
                        let sc = self.scale();
                        reflow_chain(&mut self.document, &id, sc, if self.is_editing { Some(id.as_str()) } else { None });
                    }
                }
                self.invalidate_edit_layout();
                self.request_focus = true;
            }
            AppInput::SetTextAlign(align) => {
                self.flush_history(); // push pre-format state, close typing run
                if let Some(id) = self.selected_item_ids.first().cloned().clone() {
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
                self.invalidate_edit_layout();
                self.request_focus = true;
            }
            AppInput::ClearFormat => {
                let editing = self.is_editing;
                self.flush_history(); // push pre-format state, close typing run
                if let Some(id) = self.selected_item_ids.first().cloned().clone() {
                    let range = self.selected_text_operation_range(&id, editing);
                    if let Some((_, item)) = self.find_item_mut(&id) {
                        if let ItemContent::Text(ref mut tb) = item.content {
                            if editing {
                                let (s, e) = range.unwrap_or_else(|| tb.selection_or_word_range());
                                if s < e {
                                    tb.clear_format(s, e);
                                }
                            } else {
                                if let Some((s, e)) = range {
                                    if s < e {
                                        tb.clear_format(s, e);
                                        self.is_editing = true;
                                        *self.editing_flag.borrow_mut() = true;
                                    }
                                }
                            }
                        }
                    }
                    if is_in_chain(&self.document, &id) {
                        let sc = self.scale();
                        reflow_chain(&mut self.document, &id, sc, if self.is_editing { Some(id.as_str()) } else { None });
                    }
                }
                self.invalidate_edit_layout();
                self.request_focus = true;
            }
            AppInput::MaybeCommitHistory(version) => {
                if version == self.undo_version {
                    self.typing_run_active = false;
                }
            }
            AppInput::CanvasReady => {
                self.canvas_ready = true;
            }
            AppInput::CutText => {
                if !self.is_editing { return; }
                self.flush_history(); // push pre-cut state, close typing run
                if let Some(tb) = self.get_editing_text_box_mut() {
                    tb.copy_selection();
                    tb.delete_selection();
                    self.dirty = true;
                }
                self.invalidate_edit_layout();

                if let Some(id) = self.selected_item_ids.first().cloned() {
                    if is_in_chain(&self.document, &id) {
                        let sc = self.scale();
                        reflow_chain(&mut self.document, &id, sc, Some(id.as_str()));
                    }
                }
            }
            AppInput::Undo => {
                if !self.is_editing { return; }
                self.typing_run_active = false;
                self.undo_version = self.undo_version.wrapping_add(1);
                if let Some(id) = self.selected_item_ids.first().cloned().clone() {
                    let sc = self.scale();
                    if is_in_chain(&self.document, &id) {
                        reflow_chain(&mut self.document, &id, sc, Some(id.as_str()));
                    }
                    if let Some((_, item)) = self.find_item_mut(&id) {
                        if let ItemContent::Text(ref mut tb) = item.content {
                            tb.undo();
                        }
                    }
                    self.invalidate_edit_layout();
                    if is_in_chain(&self.document, &id) {
                        reflow_chain(&mut self.document, &id, sc, Some(id.as_str()));
                    }
                }
            }
            AppInput::Redo => {
                if !self.is_editing { return; }
                self.typing_run_active = false;
                self.undo_version = self.undo_version.wrapping_add(1);
                if let Some(id) = self.selected_item_ids.first().cloned().clone() {
                    let sc = self.scale();
                    if is_in_chain(&self.document, &id) {
                        reflow_chain(&mut self.document, &id, sc, Some(id.as_str()));
                    }
                    if let Some((_, item)) = self.find_item_mut(&id) {
                        if let ItemContent::Text(ref mut tb) = item.content {
                            tb.redo();
                        }
                    }
                    self.invalidate_edit_layout();
                    if is_in_chain(&self.document, &id) {
                        reflow_chain(&mut self.document, &id, sc, Some(id.as_str()));
                    }
                }
            }
            AppInput::LinkTo(target_id) => {
                let source_id = self.link_drag_source_id.clone()
                    .or_else(|| self.selected_item_ids.first().cloned().clone());

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
                        reflow_chain(&mut self.document, &source_id, sc, None);
                    }
                }
            }
            AppInput::UnlinkFrame => {
                if let Some(id) = self.selected_item_ids.first().cloned().clone() {
                    let sc = self.scale();
                    if is_in_chain(&self.document, &id) {
                        let editing_id = if self.is_editing { Some(id.as_str()) } else { None };
                        reflow_chain(&mut self.document, &id, sc, editing_id);
                    }

                    let root_id = find_chain_root(&self.document, &id);
                    let chain = collect_chain(&self.document, &root_id);
                    let Some(idx) = chain.iter().position(|cid| cid == &id) else { return; };
                    if chain.len() < 2 { return; }

                    let root_tb = match chain_frame_textbox(&self.document, &root_id).cloned() {
                        Some(tb) => tb,
                        None => return,
                    };
                    let current_tb = match chain_frame_textbox(&self.document, &id).cloned() {
                        Some(tb) => tb,
                        None => return,
                    };

                    let start = current_tb.text_offset.min(root_tb.text.len());
                    let end = if let Some(next_id) = chain.get(idx + 1) {
                        chain_frame_textbox(&self.document, next_id)
                            .map(|tb| tb.text_offset.min(root_tb.text.len()))
                            .unwrap_or(root_tb.text.len())
                    } else {
                        root_tb.text.len()
                    };

                    let left_ids = chain[..idx].to_vec();
                    let right_ids = if idx + 1 < chain.len() {
                        chain[idx + 1..].to_vec()
                    } else {
                        Vec::new()
                    };

                    if !left_ids.is_empty() {
                        set_chain_links(&mut self.document, &left_ids);
                        set_chain_frame_content(
                            &mut self.document,
                            &left_ids[0],
                            root_tb.text[..start].to_string(),
                            slice_attrs_for_range(&root_tb.attributes, 0, start),
                            0,
                        );
                        if left_ids.len() >= 2 {
                            reflow_chain(&mut self.document, &left_ids[0], sc, None);
                        }
                    }

                    set_chain_links(&mut self.document, &[id.clone()]);
                    set_chain_frame_content(
                        &mut self.document,
                        &id,
                        root_tb.text[start..end].to_string(),
                        slice_attrs_for_range(&root_tb.attributes, start, end),
                        0,
                    );

                    if !right_ids.is_empty() {
                        set_chain_links(&mut self.document, &right_ids);
                        set_chain_frame_content(
                            &mut self.document,
                            &right_ids[0],
                            root_tb.text[end..].to_string(),
                            slice_attrs_for_range(&root_tb.attributes, end, root_tb.text.len()),
                            0,
                        );
                        if right_ids.len() >= 2 {
                            reflow_chain(&mut self.document, &right_ids[0], sc, None);
                        }
                    }
                }
            }
            AppInput::SplitChainHere => {
                let Some(id) = self.selected_item_ids.first().cloned() else { return; };
                if !self.is_editing { return; }

                let sc = self.scale();
                let root_id = find_chain_root(&self.document, &id);
                let chain = collect_chain(&self.document, &root_id);
                let Some(b_idx) = chain.iter().position(|cid| cid == &id) else { return; };
                // Only makes sense when there is at least one downstream frame.
                if b_idx + 1 >= chain.len() { return; }

                let root_tb = match chain_frame_textbox(&self.document, &root_id).cloned() {
                    Some(tb) => tb,
                    None => return,
                };
                let current_tb = match chain_frame_textbox(&self.document, &id).cloned() {
                    Some(tb) => tb,
                    None => return,
                };

                // Split point in global-text coordinates.
                let cursor_in_b = current_tb.cursor_pos.min(current_tb.text.len());
                let global_split = (current_tb.text_offset + cursor_in_b).min(root_tb.text.len());

                let chain_1_ids = chain[..=b_idx].to_vec();   // A … B (inclusive)
                let chain_2_ids = chain[b_idx + 1..].to_vec(); // C … (everything after)

                let global_text = root_tb.text.clone();
                let global_attrs = root_tb.attributes.clone();

                let text_1 = global_text[..global_split].to_string();
                let attrs_1 = slice_attrs_for_range(&global_attrs, 0, global_split);

                let text_2 = global_text[global_split..].to_string();
                let attrs_2 = slice_attrs_for_range(&global_attrs, global_split, global_text.len());

                // Rewrite link pointers: chain 1 ends at B, chain 2 starts at C.
                set_chain_links(&mut self.document, &chain_1_ids);
                set_chain_links(&mut self.document, &chain_2_ids);

                // Give each new root its portion of the global text.
                set_chain_frame_content(&mut self.document, &chain_1_ids[0], text_1, attrs_1, 0);
                set_chain_frame_content(&mut self.document, &chain_2_ids[0], text_2, attrs_2, 0);

                // Reflow each chain to distribute text across its frames.
                if chain_1_ids.len() > 1 {
                    reflow_chain(&mut self.document, &chain_1_ids[0], sc, None);
                }
                if chain_2_ids.len() > 1 {
                    reflow_chain(&mut self.document, &chain_2_ids[0], sc, None);
                }

                self.reflow_version = self.reflow_version.wrapping_add(1);
                self.render_layout_cache.borrow_mut().clear();
                self.is_editing = false;
                *self.editing_flag.borrow_mut() = false;
                self.typing_run_active = false;
                self.invalidate_edit_layout();
                self.popover_visible = false;
                self.dirty = true;
            }
            AppInput::SetWrapMode(mode) => {
                if let Some(id) = self.selected_item_ids.first().cloned() {
                    if let Some((_, item)) = self.find_item_mut(&id) {
                        if let ItemContent::Image(ib) = &mut item.content {
                            ib.wrap_mode = mode;
                        }
                    }
                    self.rebuild_flow_providers();
                }
            }
            AppInput::SetShowBorder(show) => {
                let ids = self.selected_item_ids.clone();
                for id in &ids {
                    if let Some((_, item)) = self.find_item_mut(id) {
                        item.show_border = show;
                    }
                }
            }
            AppInput::Autoscroll => {
                self.autoscroll_timer = None;
            }
        }
    }
}

// ── Master-pages sidebar builder ──────────────────────────────────────────────

fn rebuild_master_pages_ui(
    model: &AppModel,
    listbox: &mut gtk::ListBox,
    sender: ComponentSender<AppModel>,
) {
    while let Some(row) = listbox.first_child() {
        listbox.remove(&row);
    }

    let current_master = model.document.pages
        .get(model.current_page)
        .and_then(|p| p.master_page.as_deref());

    for mp in &model.document.master_pages {
        let is_applied = current_master == Some(mp.id.as_str());
        let page_count = model.document.pages.iter()
            .filter(|p| p.master_page.as_deref() == Some(mp.id.as_str()))
            .count();

        let row = gtk::ListBoxRow::new();
        let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        hbox.set_margin_all(4);

        let label_text = if page_count > 0 {
            format!("{} ({} pages)", mp.name, page_count)
        } else {
            mp.name.clone()
        };
        let name_label = gtk::Label::new(Some(&label_text));
        name_label.set_xalign(0.0);
        name_label.set_hexpand(true);
        if is_applied {
            name_label.set_markup(&format!("<b>{}</b> (current)", label_text));
        }
        hbox.append(&name_label);

        if !is_applied {
            let mp_id = mp.id.clone();
            let s = sender.clone();
            let apply_btn = gtk::Button::from_icon_name("emblem-ok-symbolic");
            apply_btn.set_tooltip_text(Some("Apply to current page"));
            apply_btn.add_css_class("flat");
            apply_btn.connect_clicked(move |_| {
                s.input(AppInput::ApplyMasterPage(mp_id.clone()));
            });
            hbox.append(&apply_btn);
        } else {
            let s = sender.clone();
            let remove_btn = gtk::Button::from_icon_name("edit-undo-symbolic");
            remove_btn.set_tooltip_text(Some("Remove from current page"));
            remove_btn.add_css_class("flat");
            remove_btn.connect_clicked(move |_| {
                s.input(AppInput::RemoveMasterPageFromPage);
            });
            hbox.append(&remove_btn);
        }

        {
            let mp_id = mp.id.clone();
            let s = sender.clone();
            let edit_btn = gtk::Button::from_icon_name("document-edit-symbolic");
            edit_btn.set_tooltip_text(Some("Edit master page"));
            edit_btn.add_css_class("flat");
            if model.is_master_page_mode() && model.selected_master_page_id.as_deref() == Some(mp.id.as_str()) {
                edit_btn.add_css_class("suggested-action");
            }
            edit_btn.connect_clicked(move |_| {
                s.input(AppInput::EnterMasterPageMode(mp_id.clone()));
            });
            hbox.append(&edit_btn);
        }

        {
            let mp_id = mp.id.clone();
            let mp_name = mp.name.clone();
            let s = sender.clone();
            let rename_btn = gtk::Button::from_icon_name("document-edit-symbolic");
            rename_btn.set_tooltip_text(Some("Rename"));
            rename_btn.add_css_class("flat");
            rename_btn.connect_clicked(move |_| {
                let w = gtk::Window::new();
                w.set_title(Some("Rename Master Page"));
                w.set_modal(true);
                w.set_default_size(350, -1);

                let vbox = gtk::Box::new(gtk::Orientation::Vertical, 12);
                vbox.set_margin_all(16);

                let label = gtk::Label::new(Some(&format!("Rename \"{}\":", mp_name)));
                label.set_xalign(0.0);
                vbox.append(&label);

                let entry = gtk::Entry::new();
                entry.set_text(&mp_name);
                entry.set_activates_default(true);
                vbox.append(&entry);

                let btn_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
                btn_box.set_halign(gtk::Align::End);
                btn_box.set_homogeneous(true);

                let cancel_btn = gtk::Button::with_label("Cancel");
                {
                    let w2 = w.downgrade();
                    cancel_btn.connect_clicked(move |_| {
                        if let Some(w) = w2.upgrade() { w.close(); }
                    });
                }
                btn_box.append(&cancel_btn);

                let ok_btn = gtk::Button::with_label("Rename");
                ok_btn.add_css_class("suggested-action");
                {
                    let mid = mp_id.clone();
                    let s = s.clone();
                    let entry = entry.clone();
                    let w_weak = w.downgrade();
                    ok_btn.connect_clicked(move |_| {
                        let n = entry.text().to_string();
                        if !n.is_empty() {
                            s.input(AppInput::RenameMasterPage(mid.clone(), n));
                        }
                        if let Some(w) = w_weak.upgrade() { w.close(); }
                    });
                }
                btn_box.append(&cancel_btn);

                let ok_btn = gtk::Button::with_label("Rename");
                ok_btn.add_css_class("suggested-action");
                {
                    let mid = mp_id.clone();
                    let s = s.clone();
                    let entry = entry.clone();
                    let w_weak = w.downgrade();
                    ok_btn.connect_clicked(move |_| {
                        let n = entry.text().to_string();
                        if !n.is_empty() {
                            s.input(AppInput::RenameMasterPage(mid.clone(), n));
                        }
                        if let Some(w) = w_weak.upgrade() { w.close(); }
                    });
                }
                btn_box.append(&ok_btn);

                vbox.append(&btn_box);
                w.set_child(Some(&vbox));
                w.present();
            });
            hbox.append(&rename_btn);
        }

        {
            let mp_id = mp.id.clone();
            let mp_name = mp.name.clone();
            let s = sender.clone();
            let delete_btn = gtk::Button::from_icon_name("edit-delete-symbolic");
            delete_btn.set_tooltip_text(Some("Delete master page"));
            delete_btn.add_css_class("flat");
            delete_btn.add_css_class("destructive-action");
            delete_btn.connect_clicked(move |_| {
                let dialog = gtk::AlertDialog::builder()
                    .modal(true)
                    .message("Delete Master Page")
                    .detail(&format!("Delete \"{}\"?\nPages using this master page will lose its items.", mp_name))
                    .buttons(["Cancel", "Delete"])
                    .cancel_button(0)
                    .default_button(0)
                    .build();

                let mid = mp_id.clone();
                let s2 = s.clone();
                dialog.choose(None::<&gtk::Window>, gtk::gio::Cancellable::NONE, move |result| {
                    if let Ok(response) = result {
                        if response == 1 {
                            s2.input(AppInput::DeleteMasterPage(mid.clone()));
                        }
                    }
                });
            });
            hbox.append(&delete_btn);
        }

        row.set_child(Some(&hbox));
        listbox.append(&row);
    }
}

// ── Text-flow provider management ────────────────────────────────────────────

impl AppModel {
    pub fn load_all_assets(&mut self) {
        self.image_surfaces.clear();
        self.svg_handles.clear();

        for page in &self.document.pages {
            for item in &page.items {
                match &item.content {
                    ItemContent::Image(ib) => {
                        if let Some(ref path) = ib.image_path {
                            if let Some(surface) = ImageBox::load_surface(path) {
                                self.image_surfaces.insert(path.clone(), Rc::new(surface));
                            }
                        }
                    }
                    ItemContent::Svg(sb) => {
                        if !sb.svg_path.is_empty() {
                            if let Ok(handle) = rsvg::Loader::new().read_path(&sb.svg_path) {
                                self.svg_handles.insert(sb.svg_path.clone(), Rc::new(handle));
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        for mp in &self.document.master_pages {
            for item in &mp.items {
                match &item.content {
                    ItemContent::Image(ib) => {
                        if let Some(ref path) = ib.image_path {
                            if !self.image_surfaces.contains_key(path.as_str()) {
                                if let Some(surface) = ImageBox::load_surface(path) {
                                    self.image_surfaces.insert(path.clone(), Rc::new(surface));
                                }
                            }
                        }
                    }
                    ItemContent::Svg(sb) => {
                        if !sb.svg_path.is_empty() && !self.svg_handles.contains_key(&sb.svg_path) {
                            if let Ok(handle) = rsvg::Loader::new().read_path(&sb.svg_path) {
                                self.svg_handles.insert(sb.svg_path.clone(), Rc::new(handle));
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    // ── Master page editing mode ────────────────────────────────────────────

    pub fn is_master_page_mode(&self) -> bool {
        self.master_page_mode && self.selected_master_page_id.is_some()
    }

    fn enter_master_page_mode(&mut self, mp_id: String) {
        if self.is_master_page_mode() && self.selected_master_page_id.as_deref() == Some(&mp_id) {
            return;
        }
        // Save edits back to outgoing master page, then exit previous mode
        if self.selected_master_page_id.is_some() {
            self.exit_master_page_mode_inner();
        }

        // Save original page-0 items so we can restore them on exit
        if self.saved_page_0_items.is_none() {
            self.saved_page_0_items = Some(self.document.pages[0].items.clone());
        }

        // Load master page items into page 0
        self.document.pages[0].items = self.clone_master_items(&mp_id);
        self.selected_master_page_id = Some(mp_id);
        self.master_page_mode = true;
        self.current_page = 0;
        self.selected_item_ids.clear();
        self.is_editing = false;
        *self.editing_flag.borrow_mut() = false;
        self.invalidate_edit_layout();
        self.dirty = true;
    }

    fn exit_master_page_mode(&mut self) {
        if !self.is_master_page_mode() { return; }
        self.exit_master_page_mode_inner();
        // Restore original page 0 items
        if let Some(items) = self.saved_page_0_items.take() {
            self.document.pages[0].items = items;
        }
        self.selected_master_page_id = None;
        self.master_page_mode = false;
        self.selected_item_ids.clear();
        self.is_editing = false;
        *self.editing_flag.borrow_mut() = false;
        self.invalidate_edit_layout();
        self.dirty = true;
    }

    fn exit_master_page_mode_inner(&mut self) {
        // Save edits back to the master page
        if let Some(ref mp_id) = self.selected_master_page_id {
            if let Some(mp) = self.document.master_pages.iter_mut().find(|m| m.id == *mp_id) {
                mp.items = self.document.pages[0].items.clone();
            }
        }
    }

    fn clone_master_items(&self, mp_id: &str) -> Vec<Item> {
        self.document.master_pages.iter()
            .find(|m| m.id == mp_id)
            .map(|m| m.items.clone())
            .unwrap_or_default()
    }

    fn effective_page_count(&self) -> usize {
        if self.is_master_page_mode() { 1 } else { self.document.pages.len() }
    }

    // ── End master page editing mode ────────────────────────────────────────

    // ── Alignment helpers ───────────────────────────────────────────────────

    fn get_alignment_ref_bounds(&self) -> (f64, f64, f64, f64) {
        if let Some(ref ref_id) = self.alignment_ref_id {
            if let Some((_, item)) = self.find_item(ref_id) {
                return (item.x, item.y, item.width, item.height);
            }
        }
        // Page reference (or ref item not found)
        (0.0, 0.0, self.document.width, self.document.height)
    }

    fn align_selected_horizontal(&mut self, align: AlignH) {
        let ref_bounds = self.get_alignment_ref_bounds();
        let ids: Vec<String> = self.selected_item_ids.clone();
        for id in &ids {
            if let Some((_, item)) = self.find_item_mut(id) {
                match align {
                    AlignH::Left   => item.x = ref_bounds.0,
                    AlignH::Center => item.x = ref_bounds.0 + (ref_bounds.2 - item.width) / 2.0,
                    AlignH::Right  => item.x = ref_bounds.0 + ref_bounds.2 - item.width,
                }
            }
        }
        self.dirty = true;
    }

    fn align_selected_vertical(&mut self, align: AlignV) {
        let ref_bounds = self.get_alignment_ref_bounds();
        let ids: Vec<String> = self.selected_item_ids.clone();
        for id in &ids {
            if let Some((_, item)) = self.find_item_mut(id) {
                match align {
                    AlignV::Top    => item.y = ref_bounds.1,
                    AlignV::Center => item.y = ref_bounds.1 + (ref_bounds.3 - item.height) / 2.0,
                    AlignV::Bottom => item.y = ref_bounds.1 + ref_bounds.3 - item.height,
                }
            }
        }
        self.dirty = true;
    }

    // ── End alignment helpers ───────────────────────────────────────────────

    /// Re-computes all flow providers for the current document.
    /// Called after any image WrapMode change, move, or resize.
    pub fn rebuild_flow_providers(&mut self) {
        self.flow_providers.clear();
        let scale = self.scale();
        const FLOW_PADDING_PX: f64 = 3.0;

        for page in &self.document.pages {
            let wrap_images: Vec<&Item> = page.items.iter()
                .filter(|it| matches!(&it.content, ItemContent::Image(ib) if ib.wrap_mode != WrapMode::Independent))
                .collect();
            if wrap_images.is_empty() { continue; }

            for tf in page.items.iter().filter(|it| matches!(it.content, ItemContent::Text(_))) {
                let fw_f = tf.width  * scale;
                let fh_f = tf.height * scale;
                let fw = fw_f as i32 + 1;
                let fh = fh_f as i32 + 1;

                // Collect images that overlap this text frame (with their frame-local coords).
                let overlapping: Vec<(&Item, f64, f64, f64, f64)> = wrap_images.iter()
                    .filter_map(|img| {
                        let rel_x = (img.x - tf.x) * scale;
                        let rel_y = (img.y - tf.y) * scale;
                        let iw    = img.width  * scale;
                        let ih    = img.height * scale;
                        if rel_x < fw_f && rel_x + iw > 0.0 && rel_y < fh_f && rel_y + ih > 0.0 {
                            Some((*img, rel_x, rel_y, iw, ih))
                        } else {
                            None
                        }
                    })
                    .collect();

                if overlapping.is_empty() { continue; }

                // Build an A8 mask surface: 255 = text can go here, 0 = blocked.
                // Start fully writable, then carve out each image using DestOut so
                // the image's own alpha channel defines the obstacle shape.
                let Ok(mut mask_surf) = cairo::ImageSurface::create(cairo::Format::A8, fw, fh)
                else { continue };

                {
                    let Ok(cr) = cairo::Context::new(&mask_surf) else { continue };

                    // Fill everything as writable (alpha = 1.0 → byte 255 in A8).
                    cr.set_source_rgba(0.0, 0.0, 0.0, 1.0);
                    cr.paint().unwrap();

                    // DestOut: dest_alpha *= (1 − src_alpha)
                    // → opaque image pixel → mask becomes 0 (blocked)
                    // → transparent image pixel → mask stays 255 (writable)
                    cr.set_operator(cairo::Operator::DestOut);

                    for (img, rel_x, rel_y, iw, ih) in &overlapping {
                        let wrap_mode = match &img.content {
                            ItemContent::Image(ib) => ib.wrap_mode,
                            _ => continue,
                        };

                        if wrap_mode == WrapMode::Block {
                            cr.set_source_rgba(0.0, 0.0, 0.0, 1.0);
                            cr.rectangle(0.0, *rel_y, fw_f, *ih);
                            cr.fill().unwrap();
                            continue;
                        }

                        let surf_opt = if let ItemContent::Image(ib) = &img.content {
                            ib.image_path.as_deref().and_then(|p| self.image_surfaces.get(p))
                        } else {
                            None
                        };

                        cr.save().unwrap();
                        cr.translate(*rel_x, *rel_y);

                        if let Some(surf_rc) = surf_opt {
                            let src_w = surf_rc.width()  as f64;
                            let src_h = surf_rc.height() as f64;
                            if src_w > 0.0 && src_h > 0.0 {
                                cr.scale(iw / src_w, ih / src_h);
                                cr.set_source_surface(&**surf_rc, 0.0, 0.0).unwrap();
                                cr.paint().unwrap();
                            }
                        } else {
                            // No surface loaded: fall back to opaque rectangle.
                            cr.set_source_rgba(0.0, 0.0, 0.0, 1.0);
                            cr.rectangle(0.0, 0.0, *iw, *ih);
                            cr.fill().unwrap();
                        }
                        cr.restore().unwrap();
                    }
                    // cr drops here, releasing Cairo's reference to mask_surf.
                }

                // Extract A8 bytes row-by-row (stride may be padded).
                let stride   = mask_surf.stride() as usize;
                let fw_usize = fw as usize;
                let fh_usize = fh as usize;
                let flat = {
                    let Ok(data) = mask_surf.data() else { continue };
                    let mut v = Vec::with_capacity(fw_usize * fh_usize);
                    for row in 0..fh_usize {
                        v.extend_from_slice(&data[row * stride..row * stride + fw_usize]);
                    }
                    v
                };

                let provider = PrecomputedFlowProvider::from_a8_mask(
                    &flat, fw_usize, fh_usize, FLOW_PADDING_PX,
                );
                self.flow_providers.insert(tf.id.clone(), provider);
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

fn collect_all_chain_roots(doc: &Document) -> Vec<String> {
    doc.pages.iter()
        .flat_map(|p| p.items.iter())
        .filter_map(|item| {
            if let ItemContent::Text(tb) = &item.content {
                if tb.prev_frame_id.is_none() && tb.next_frame_id.is_some() {
                    return Some(item.id.clone());
                }
            }
            None
        })
        .collect()
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

fn shift_attrs(attrs: &[TextAttribute], offset: usize) -> Vec<TextAttribute> {
    let offset = offset as u32;
    attrs.iter().map(|attr| TextAttribute {
        start: attr.start + offset,
        end: attr.end + offset,
        value: attr.value.clone(),
    }).collect()
}

fn chain_frame_textbox<'a>(doc: &'a Document, id: &str) -> Option<&'a TextBox> {
    doc.pages.iter()
        .flat_map(|p| p.items.iter())
        .find(|i| i.id == id)
        .and_then(|item| {
            if let ItemContent::Text(tb) = &item.content { Some(tb) } else { None }
        })
}

fn set_chain_frame_content(
    doc: &mut Document,
    id: &str,
    text: String,
    attributes: Vec<TextAttribute>,
    text_offset: usize,
) {
    if let Some(item) = doc.pages.iter_mut()
        .flat_map(|p| p.items.iter_mut())
        .find(|i| i.id == id)
    {
        if let ItemContent::Text(tb) = &mut item.content {
            tb.text = text;
            tb.attributes = attributes;
            tb.text_offset = text_offset;
            if tb.cursor_pos > tb.text.len() { tb.cursor_pos = tb.text.len(); }
            if tb.selection_anchor.map_or(false, |a| a > tb.text.len()) {
                tb.selection_anchor = None;
            }
        }
    }
}

fn set_chain_links(doc: &mut Document, ids: &[String]) {
    for (idx, id) in ids.iter().enumerate() {
        let prev = idx.checked_sub(1).and_then(|i| ids.get(i)).cloned();
        let next = ids.get(idx + 1).cloned();
        if let Some(item) = doc.pages.iter_mut()
            .flat_map(|p| p.items.iter_mut())
            .find(|i| i.id == *id)
        {
            if let ItemContent::Text(tb) = &mut item.content {
                tb.prev_frame_id = prev;
                tb.next_frame_id = next;
            }
        }
    }
}

fn strip_chain_downstream_texts(doc: &mut Document) {
    for root_id in collect_all_chain_roots(doc) {
        let chain = collect_chain(doc, &root_id);
        for id in chain.iter().skip(1) {
            set_chain_frame_content(doc, id, String::new(), Vec::new(), chain_frame_textbox(doc, id).map(|tb| tb.text_offset).unwrap_or(0));
        }
    }
}

fn collect_chain_visible_lengths(doc: &Document) -> HashMap<String, usize> {
    let mut visible_lengths = HashMap::new();

    for root_id in collect_all_chain_roots(doc) {
        let chain = collect_chain(doc, &root_id);
        for (idx, id) in chain.iter().enumerate() {
            let Some(tb) = chain_frame_textbox(doc, id) else { continue; };
            let visible_len = if let Some(next_id) = chain.get(idx + 1) {
                chain_frame_textbox(doc, next_id)
                    .map(|next_tb| next_tb.text_offset.saturating_sub(tb.text_offset).min(tb.text.len()))
                    .unwrap_or(tb.text.len())
            } else {
                tb.text.len()
            };
            visible_lengths.insert(id.clone(), visible_len);
        }
    }

    visible_lengths
}

/// Redistributes the chain's text across all linked frames.
fn reflow_chain(doc: &mut Document, any_id: &str, scale: f64, editing_id: Option<&str>) {
    use gtk::pango::prelude::FontMapExt;
    let started = std::time::Instant::now();
    let root_id = find_chain_root(doc, any_id);
    let chain = collect_chain(doc, &root_id);
    if chain.len() < 2 { return; }

    struct ReflowFrameSnap {
        id: String,
        w: f64,
        h: f64,
        text: String,
        attributes: Vec<TextAttribute>,
        text_offset: usize,
        font_description: String,
        padding: f64,
        line_spacing: f64,
        alignment: TextAlign,
    }
    let snaps: Vec<ReflowFrameSnap> = chain.iter().map(|id| {
        doc.pages.iter()
            .flat_map(|p| p.items.iter())
            .find(|i| &i.id == id)
            .map(|item| {
                if let ItemContent::Text(tb) = &item.content {
                    ReflowFrameSnap {
                        id: id.clone(),
                        w: item.width,
                        h: item.height,
                        text: tb.text.clone(),
                        attributes: tb.attributes.clone(),
                        text_offset: tb.text_offset,
                        font_description: tb.font_description.clone(),
                        padding: tb.padding,
                        line_spacing: tb.line_spacing,
                        alignment: tb.alignment,
                    }
                } else {
                    ReflowFrameSnap {
                        id: id.clone(),
                        w: item.width,
                        h: item.height,
                        text: String::new(),
                        attributes: vec![],
                        text_offset: 0,
                        font_description: String::from("Sans 11"),
                        padding: 0.0,
                        line_spacing: 1.0,
                        alignment: TextAlign::Left,
                    }
                }
            })
            .unwrap_or_else(|| ReflowFrameSnap {
                id: id.clone(),
                w: 100.0,
                h: 100.0,
                text: String::new(),
                attributes: vec![],
                text_offset: 0,
                font_description: String::from("Sans 11"),
                padding: 0.0,
                line_spacing: 1.0,
                alignment: TextAlign::Left,
            })
    }).collect();

    // Priority: editing_id branch FIRST, then legacy detection.
    // The legacy check fires whenever an edited downstream frame grows 1 byte beyond
    // root.text, which is the normal suffix-model state while typing. If we checked
    // legacy_layout first, it would concatenate all suffixes (N × ~500KB) and multiply
    // the global text by the number of frames on every keystroke.
    let (global_text, global_attrs) = if let Some(editing_id) = editing_id.filter(|id| chain.iter().any(|cid| cid == id)) {
        if editing_id == root_id {
            (snaps[0].text.clone(), snaps[0].attributes.clone())
        } else {
            let editing_snap = snaps.iter().find(|snap| snap.id == editing_id).unwrap();
            let prefix_end = editing_snap.text_offset.min(snaps[0].text.len());
            let mut text = snaps[0].text[..prefix_end].to_string();
            text.push_str(&editing_snap.text);

            let mut attrs = slice_attrs_for_range(&snaps[0].attributes, 0, prefix_end);
            attrs.extend(shift_attrs(&editing_snap.attributes, prefix_end));
            (text, attrs)
        }
    } else {
        // Legacy detection: old files stored slices instead of suffixes; detect by
        // checking if any downstream frame's text extends beyond the root's text length.
        let legacy_layout = snaps.iter().skip(1).any(|snap| {
            !snap.text.is_empty() && snaps[0].text.len() < snap.text_offset + snap.text.len()
        });
        if legacy_layout {
            let mut text = String::new();
            let mut attrs = Vec::new();
            for snap in &snaps {
                let offset = text.len();
                text.push_str(&snap.text);
                attrs.extend(shift_attrs(&snap.attributes, offset));
            }
            (text, attrs)
        } else {
            (snaps[0].text.clone(), snaps[0].attributes.clone())
        }
    };

    set_chain_frame_content(doc, &root_id, global_text.clone(), global_attrs.clone(), 0);

    // Distribute text into each frame
    // PROBE_LIMIT: cap for probe text — well above any single-frame capacity (~4KB for A4)
    // so measurements are accurate without allocating/copying large suffixes (500KB+).
    const PROBE_LIMIT: usize = 15_000;
    let pango_ctx = {
        let font_map = pangocairo::FontMap::default();
        let ctx = font_map.create_context();
        pangocairo::functions::context_set_resolution(&ctx, 25.4 * scale);
        ctx
    };
    let mut offset = 0usize;
    let n = snaps.len();
    let mut last_frame_overflows = false;

    for (i, snap) in snaps.iter().enumerate() {
        let is_last = i == n - 1;
        let remaining = &global_text[offset..];

        let capacity = if remaining.is_empty() {
            0
        } else if is_last {
            // For the last frame, also compute overflow_hint so draw_page_content
            // doesn't have to call text_capacity again on every render cycle.
            if remaining.len() > PROBE_LIMIT {
                last_frame_overflows = true;
            } else {
                let attrs = slice_attrs_for_range(&global_attrs, offset, global_text.len());
                let cap = TextBox::measure_capacity(
                    &pango_ctx,
                    remaining,
                    &attrs,
                    &snap.font_description,
                    snap.padding,
                    snap.line_spacing,
                    snap.alignment,
                    snap.w * scale,
                    snap.h * scale,
                    scale,
                );
                last_frame_overflows = cap < remaining.len();
            }
            remaining.len()
        } else {
            let probe_end = if remaining.len() <= PROBE_LIMIT {
                remaining.len()
            } else {
                let mut e = PROBE_LIMIT;
                while e > 0 && !remaining.is_char_boundary(e) { e -= 1; }
                e
            };
            let attrs = slice_attrs_for_range(&global_attrs, offset, offset + probe_end);
            TextBox::measure_capacity(
                &pango_ctx,
                &remaining[..probe_end],
                &attrs,
                &snap.font_description,
                snap.padding,
                snap.line_spacing,
                snap.alignment,
                snap.w * scale,
                snap.h * scale,
                scale,
            )
        };

        if remaining.len() > 2_000 {
            eprintln!(
                "[perf] reflow_chain frame id={} idx={}/{} remaining_len={} capacity={} size_mm={:.1}x{:.1}",
                snap.id,
                i + 1,
                n,
                remaining.len(),
                capacity,
                snap.w,
                snap.h
            );
        }

        let frame_text = global_text[offset..].to_string();
        let frame_attrs = slice_attrs_for_range(&global_attrs, offset, global_text.len());
        set_chain_frame_content(doc, &snap.id, frame_text, frame_attrs, offset);

        offset += capacity;
        if offset > global_text.len() {
            offset = global_text.len();
        }
    }

    // Persist overflow_hint on the last frame so draw_page_content reads it O(1).
    {
        let last_id = snaps[n - 1].id.clone();
        if let Some(item) = doc.pages.iter_mut()
            .flat_map(|p| p.items.iter_mut())
            .find(|i| i.id == last_id)
        {
            if let ItemContent::Text(tb) = &mut item.content {
                tb.overflow_hint = last_frame_overflows;
            }
        }
    }

    let elapsed = started.elapsed();
    if global_text.len() > 2_000 || elapsed.as_millis() >= 8 {
        eprintln!(
            "[perf] reflow_chain root={} frames={} global_len={} attrs={} editing_id={:?} took={}ms",
            root_id,
            chain.len(),
            global_text.len(),
            global_attrs.len(),
            editing_id,
            elapsed.as_millis()
        );
    }
}

fn make_pango_ctx_with_resolution(dpi: f64) -> gtk::pango::Context {
    use gtk::pango::prelude::FontMapExt;
    let font_map = pangocairo::FontMap::default();
    let ctx = font_map.create_context();
    pangocairo::functions::context_set_resolution(&ctx, dpi);
    ctx
}

fn make_pango_ctx() -> gtk::pango::Context {
    let ctx = make_pango_ctx_with_resolution(25.4 * SCALE);
    // At 76.2 DPI (= 25.4 mm/in * SCALE=3 px/mm), font sizes are non-integer
    // pixel counts (e.g. 12pt → 12.7 px).  With the default HintMetrics::On,
    // Pango rounds per-glyph advance heights to whole pixels, but they round
    // differently for each glyph shape, so letters on the same baseline end up
    // on different screen pixel rows — the "letters at different heights" effect.
    // HintMetrics::Off uses exact fractional metrics; HintStyle::Slight keeps
    // mild hinting for readability without aggressively snapping stroke widths.
    if let Ok(mut opts) = cairo::FontOptions::new() {
        opts.set_hint_metrics(cairo::HintMetrics::Off);
        opts.set_hint_style(cairo::HintStyle::Slight);
        pangocairo::functions::context_set_font_options(&ctx, Some(&opts));
    }
    ctx
}

fn selected_text_frame_has_next_chain(doc: &Document, selected_ids: &[String], is_editing: bool) -> bool {
    if !is_editing || selected_ids.len() != 1 { return false; }
    let id = &selected_ids[0];
    doc.pages.iter()
        .flat_map(|p| p.items.iter())
        .find(|i| &i.id == id)
        .map(|item| {
            if let ItemContent::Text(tb) = &item.content { tb.next_frame_id.is_some() } else { false }
        })
        .unwrap_or(false)
}

fn is_selected_type(doc: &Document, selected_ids: &[String], ty: &ItemType) -> bool {
    if selected_ids.len() != 1 { return false; }
    let id = &selected_ids[0];
    for page in &doc.pages {
        if let Some(item) = page.items.iter().find(|i| &i.id == id) {
            return &item.content.item_type() == ty;
        }
    }
    false
}

fn selected_image_frame_has_image(doc: &Document, selected_ids: &[String]) -> bool {
    if selected_ids.len() != 1 { return false; }
    let id = &selected_ids[0];
    for page in &doc.pages {
        if let Some(item) = page.items.iter().find(|i| &i.id == id) {
            return matches!(&item.content, ItemContent::Image(ib) if ib.image_path.is_some());
        }
    }
    false
}

fn next_char_boundary_local(s: &str, mut idx: usize) -> usize {
    idx = idx.min(s.len());
    while idx < s.len() && !s.is_char_boundary(idx) {
        idx += 1;
    }
    idx
}

fn get_selected_fit_mode(doc: &Document, selected_ids: &[String]) -> Option<crate::image_box::FitMode> {
    if selected_ids.len() != 1 { return None; }
    let id = &selected_ids[0];
    for page in &doc.pages {
        if let Some(item) = page.items.iter().find(|i| &i.id == id) {
            if let ItemContent::Image(ib) = &item.content {
                return Some(ib.fit_mode);
            }
            return None;
        }
    }
    None
}

fn get_selected_wrap_mode(doc: &Document, selected_ids: &[String]) -> Option<WrapMode> {
    if selected_ids.len() != 1 { return None; }
    let id = &selected_ids[0];
    for page in &doc.pages {
        if let Some(item) = page.items.iter().find(|i| &i.id == id) {
            if let ItemContent::Image(ib) = &item.content {
                return Some(ib.wrap_mode);
            }
        }
    }
    None
}

fn get_selected_svg_fit_mode(doc: &Document, selected_ids: &[String]) -> Option<crate::svg_box::FitMode> {
    if selected_ids.len() != 1 { return None; }
    let id = &selected_ids[0];
    for page in &doc.pages {
        if let Some(item) = page.items.iter().find(|i| &i.id == id) {
            if let ItemContent::Svg(sb) = &item.content {
                return Some(sb.fit_mode);
            }
            return None;
        }
    }
    None
}

fn get_svg_path_info(doc: &Document, selected_ids: &[String]) -> String {
    if selected_ids.len() != 1 { return String::new(); }
    let id = &selected_ids[0];
    for page in &doc.pages {
        if let Some(item) = page.items.iter().find(|i| &i.id == id) {
            if let ItemContent::Svg(sb) = &item.content {
                return format!("Current path: {}", sb.svg_path);
            }
        }
    }
    String::new()
}

fn get_selected_show_border(doc: &Document, selected_ids: &[String]) -> bool {
    if selected_ids.is_empty() { return false; }
    let id = &selected_ids[0];
    for page in &doc.pages {
        if let Some(item) = page.items.iter().find(|i| &i.id == id) {
            return item.show_border;
        }
    }
    false
}

fn get_info_text(doc: &Document, selected_ids: &[String]) -> String {
    if selected_ids.is_empty() {
        return "No item selected".to_string();
    }
    if selected_ids.len() > 1 {
        return format!("{} items selected", selected_ids.len());
    }
    let id = &selected_ids[0];
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
    selected_ids: &[String],
    is_editing: bool,
    draw_handles: bool,
    scale_factor: f64,
    chain_ids: &[String],
    visible_lengths: &HashMap<String, usize>,
    flow_providers: &HashMap<String, PrecomputedFlowProvider>,
    editing_layout: Option<&(String, gtk::pango::Layout)>,
    is_export: bool,
    render_cache: Option<&Rc<RefCell<HashMap<LayoutCacheKey, gtk::pango::Layout>>>>,
    reflow_version: u64,
) {
    for item in &page.items {
        let is_selected = selected_ids.contains(&item.id);
        let is_editing_this = is_selected && is_editing && selected_ids.first() == Some(&item.id);

        cr.save().unwrap();
        cr.translate(item.x * scale_factor, item.y * scale_factor);
        cr.rotate(item.rotation.to_radians());

        let w = item.width * scale_factor;
        let h = item.height * scale_factor;
        match &item.content {
            ItemContent::Text(tb) => {
                let flow = flow_providers.get(&item.id).map(|p| p as &dyn crate::text_flow::TextFlowProvider);
                let visible_len = visible_lengths.get(&item.id).copied().unwrap_or(tb.text.len()).min(tb.text.len());
                // Limit display to visible_len for ALL frames that have a successor (root + intermediate).
                // Without this, editing such a frame shows the full suffix—content belonging to later
                // frames—which looks like infinite repeating text. The editing case extends the window
                // just enough to keep the cursor visible when it's past the normal frame boundary.
                let display_len = if visible_len < tb.text.len() {
                    if is_editing_this {
                        let raw = tb.cursor_pos.saturating_add(200).min(tb.text.len()).max(visible_len);
                        // cursor_pos+200 may land mid-char; snap back to the nearest boundary.
                        let mut d = raw;
                        while d > visible_len && !tb.text.is_char_boundary(d) { d -= 1; }
                        d
                    } else {
                        visible_len
                    }
                } else {
                    tb.text.len()
                };

                if is_editing_this {
                    // Editing frame: use edit_layout_cache (managed separately), no render cache.
                    let editing_layout_ref = editing_layout
                        .and_then(|(id, layout)| if *id == item.id { Some(layout) } else { None });
                    if display_len < tb.text.len() {
                        let mut visible_tb = tb.clone();
                        visible_tb.text.truncate(display_len);
                        visible_tb.attributes = slice_attrs_for_range(&tb.attributes, 0, display_len);
                        visible_tb.render(cr, pango_ctx, w, h, is_selected, true, item.show_border, scale_factor, flow, None, is_export);
                    } else {
                        tb.render(cr, pango_ctx, w, h, is_selected, true, item.show_border, scale_factor, flow, editing_layout_ref, is_export);
                    }
                } else if flow.is_some() {
                    // Flow-provider path has its own layout logic — skip render cache.
                    tb.render(cr, pango_ctx, w, h, is_selected, false, item.show_border, scale_factor, flow, None, is_export);
                } else if let Some(cache) = render_cache {
                    // Non-editing frame: look up or populate the render layout cache.
                    let key = LayoutCacheKey {
                        item_id: item.id.clone(),
                        w_bits: w.to_bits(),
                        h_bits: h.to_bits(),
                        display_len,
                        reflow_version,
                    };
                    let cached = cache.borrow().get(&key).cloned();
                    let layout = cached.unwrap_or_else(|| {
                        let padding = tb.padding * scale_factor;
                        let l = if display_len < tb.text.len() {
                            let slice_text = &tb.text[..display_len];
                            let slice_attrs = slice_attrs_for_range(&tb.attributes, 0, display_len);
                            tb.prepare_layout_for_slice(pango_ctx, w, padding, slice_text, &slice_attrs)
                        } else {
                            tb.prepare_layout(pango_ctx, w, padding)
                        };
                        cache.borrow_mut().insert(key, l.clone());
                        l
                    });
                    tb.render(cr, pango_ctx, w, h, is_selected, false, item.show_border, scale_factor, None, Some(&layout), is_export);
                } else {
                    // Export path (no cache): direct render.
                    if display_len < tb.text.len() {
                        let mut visible_tb = tb.clone();
                        visible_tb.text.truncate(display_len);
                        visible_tb.attributes = slice_attrs_for_range(&tb.attributes, 0, display_len);
                        visible_tb.render(cr, pango_ctx, w, h, false, false, item.show_border, scale_factor, flow, None, is_export);
                    } else {
                        tb.render(cr, pango_ctx, w, h, false, false, item.show_border, scale_factor, flow, None, is_export);
                    }
                }
            }
            ItemContent::Image(ib) => {
                let image = ib.image_path.as_ref()
                    .and_then(|p| images.get(p))
                    .map(|rc| rc.as_ref());
                ib.render(cr, w, h, is_selected, item.show_border, image);
            }
            ItemContent::Svg(sb) => {
                let handle = if sb.svg_path.is_empty() {
                    None
                } else {
                    svg_handles.get(&sb.svg_path).map(|rc| rc.as_ref())
                };
                sb.render(cr, w, h, is_selected, item.show_border, handle);
            }
            ItemContent::Shape => {
                if !is_export && (is_selected || item.show_border) {
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
            }
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
        if !chain_ids.is_empty() && !selected_ids.contains(&item.id)
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
        if !is_export {
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
                let visible_len = visible_lengths.get(&item.id).copied().unwrap_or(tb.text.len()).min(tb.text.len());
                let item_has_overflow = tb.next_frame_id.is_none()
                    && visible_len > 0
                    && h >= 20.0
                    && if tb.prev_frame_id.is_some() {
                        // Last frame in a chain — reflow_chain cached this result; avoids
                        // calling text_capacity on every render cycle (was 6-8× per keypress).
                        tb.overflow_hint
                    } else if visible_len < tb.text.len() {
                        let mut visible_tb = tb.clone();
                        visible_tb.text.truncate(visible_len);
                        visible_tb.attributes = slice_attrs_for_range(&tb.attributes, 0, visible_len);
                        visible_tb.overflows_frame(pango_ctx, w, h, sf)
                    } else {
                        tb.overflows_frame(pango_ctx, w, h, sf)
                    };

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
}

fn draw_canvas(
    cr: &gtk::cairo::Context,
    doc: &Document,
    drag_start: Option<(f64, f64)>,
    drag_current: Option<(f64, f64)>,
    selected_ids: &[String],
    is_editing: bool,
    images: &HashMap<String, Rc<cairo::ImageSurface>>,
    svg_handles: &HashMap<String, Rc<rsvg::SvgHandle>>,
    zoom: f64,
    layout: PageLayout,
    link_drag: Option<((f64, f64), (f64, f64))>,
    flow_providers: &HashMap<String, PrecomputedFlowProvider>,
    editing_layout: Option<&(String, gtk::pango::Layout)>,
    render_cache: &Rc<RefCell<HashMap<LayoutCacheKey, gtk::pango::Layout>>>,
    reflow_version: u64,
    master_page_mode: bool,
) {
    cr.save().unwrap();
    cr.scale(zoom, zoom);

    // Build Pango layouts in document space, not from the zoomed Cairo context.
    // Zoom should scale the final paint output only; otherwise text metrics and
    // line breaks can shift as the viewport zoom changes.
    let pango_ctx = make_pango_ctx();

    // Collect chain siblings of the selected frame so they can be highlighted
    let chain_ids: Vec<String> = selected_ids.first()
        .filter(|id| is_in_chain(doc, id))
        .map(|id| {
            let root = find_chain_root(doc, id);
            collect_chain(doc, &root)
        })
        .unwrap_or_default();
    let visible_lengths = collect_chain_visible_lengths(doc);

    let page_gap = 20.0;
    let pages_to_show = if master_page_mode { 1 } else { doc.pages.len() };

    for (page_idx, page) in doc.pages.iter().enumerate().take(pages_to_show) {
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

        // Master page items (rendered as dimmed background layer)
        // Skip when in master-page editing mode (we ARE the master page)
        if !master_page_mode {
            if let Some(ref mp_id) = page.master_page {
            if let Some(mp) = doc.master_pages.iter().find(|m| m.id == *mp_id) {
                cr.save().unwrap();
                // Render all master items as a group at reduced opacity
                cr.push_group();
                for item in &mp.items {
                    cr.save().unwrap();
                    cr.translate(item.x * SCALE, item.y * SCALE);
                    cr.rotate(item.rotation.to_radians());

                    let w = item.width * SCALE;
                    let h = item.height * SCALE;

                    match &item.content {
                        ItemContent::Text(tb) => {
                            let page_num = page_idx + 1;
                            let total = doc.pages.len();
                            let subs = tb.substitute_page_numbers(page_num, total);
                            subs.render(cr, &pango_ctx, w, h, false, false, item.show_border, SCALE, None, None, false);
                        }
                        ItemContent::Image(ib) => {
                            let surface = ib.image_path.as_ref()
                                .and_then(|p| images.get(p.as_str()))
                                .map(|rc| rc.as_ref());
                            ib.render(cr, w, h, false, item.show_border, surface);
                        }
                        ItemContent::Svg(sb) => {
                            let handle = svg_handles.get(&sb.svg_path)
                                .map(|rc| rc.as_ref());
                            sb.render(cr, w, h, false, item.show_border, handle);
                        }
                        ItemContent::Shape => {
                            cr.set_source_rgba(0.85, 0.85, 0.95, 1.0);
                            cr.rectangle(0.0, 0.0, w, h);
                            cr.fill().unwrap();
                            cr.set_source_rgba(0.5, 0.5, 0.7, 1.0);
                            cr.set_line_width(1.0);
                            cr.rectangle(0.0, 0.0, w, h);
                            cr.stroke().unwrap();
                        }
                    }

                    cr.restore().unwrap();
                }
                cr.pop_group_to_source().unwrap();
                cr.paint_with_alpha(0.55).unwrap();
                cr.restore().unwrap();
            }
            }
        }

        draw_page_content(cr, &pango_ctx, page, images, svg_handles, selected_ids, is_editing, true, SCALE, &chain_ids, &visible_lengths, flow_providers, editing_layout, false, Some(render_cache), reflow_version);

        cr.restore().unwrap();
    }
    cr.restore().unwrap();

    if drag_start.is_some() && selected_ids.is_empty() {
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

    // Disable font hinting for PDF: hinting snaps glyph outlines to pixel
    // boundaries (making fonts appear bolder) and skews advance widths so
    // they don't match the embedded vector outlines, breaking spacing.
    let mut font_options = cairo::FontOptions::new()?;
    font_options.set_hint_style(cairo::HintStyle::None);
    font_options.set_hint_metrics(cairo::HintMetrics::Off);
    cr.set_font_options(&font_options);

    // create_context ties the Pango context to the Cairo PDF surface and calls
    // update_context internally, which makes Cairo's PDF backend choose CID composite
    // fonts (with ToUnicode CMap) instead of WinAnsi Type-1 encoding.
    let pango_ctx = pangocairo::functions::create_context(&cr);
    pangocairo::functions::context_set_font_options(&pango_ctx, Some(&font_options));
    pangocairo::functions::context_set_resolution(&pango_ctx, 72.0);
    let visible_lengths = collect_chain_visible_lengths(document);

    for (page_idx, page) in document.pages.iter().enumerate() {
        surface.set_size(width_pt, height_pt)?;
        cr.save()?;
        // Sync the Pango context with the current Cairo surface/transform state so
        // font metrics and glyph outlines stay accurate for each page.
        pangocairo::functions::update_context(&cr, &pango_ctx);

        // Rebuild flow providers for this page during export
        let mut page_flow_providers = HashMap::new();
        let scale = mm_to_points;
        const FLOW_PADDING_PX: f64 = 3.0;
        let wrap_images: Vec<&Item> = page.items.iter()
            .filter(|it| matches!(&it.content, ItemContent::Image(ib) if ib.wrap_mode != WrapMode::Independent))
            .collect();
        
        for item in &page.items {
            if let ItemContent::Text(_) = &item.content {
                // Check if any wrap_image overlaps with this text box
                let overlapping: Vec<&Item> = wrap_images.iter()
                    .filter(|img| {
                        let overlap_x = item.x < img.x + img.width && item.x + item.width > img.x;
                        let overlap_y = item.y < img.y + img.height && item.y + item.height > img.y;
                        overlap_x && overlap_y
                    })
                    .map(|img| *img)
                    .collect();

                if !overlapping.is_empty() {
                    let fw_f = item.width * scale;
                    let fh_f = item.height * scale;
                    let fw = fw_f as i32 + 1;
                    let fh = fh_f as i32 + 1;
                    let Ok(mut mask_surf) = cairo::ImageSurface::create(cairo::Format::A8, fw, fh)
                    else { continue };

                    {
                        let Ok(cr) = cairo::Context::new(&mask_surf) else { continue };
                        cr.set_source_rgba(0.0, 0.0, 0.0, 1.0);
                        cr.paint().unwrap();
                        cr.set_operator(cairo::Operator::DestOut);

                        for img in &overlapping {
                            let (wrap_mode, image_path) = match &img.content {
                                ItemContent::Image(ib) => (ib.wrap_mode, ib.image_path.as_deref()),
                                _ => continue,
                            };

                            let rel_x = (img.x - item.x) * scale;
                            let rel_y = (img.y - item.y) * scale;
                            let iw = img.width * scale;
                            let ih = img.height * scale;

                            if wrap_mode == WrapMode::Block {
                                cr.set_source_rgba(0.0, 0.0, 0.0, 1.0);
                                cr.rectangle(0.0, rel_y, fw_f, ih);
                                cr.fill().unwrap();
                                continue;
                            }

                            let surf_opt = image_path.and_then(|p| images.get(p));

                            cr.save().unwrap();
                            cr.translate(rel_x, rel_y);

                            if let Some(surf_rc) = surf_opt {
                                let src_w = surf_rc.width() as f64;
                                let src_h = surf_rc.height() as f64;
                                if src_w > 0.0 && src_h > 0.0 {
                                    cr.scale(iw / src_w, ih / src_h);
                                    cr.set_source_surface(&**surf_rc, 0.0, 0.0).unwrap();
                                    cr.paint().unwrap();
                                }
                            } else {
                                cr.set_source_rgba(0.0, 0.0, 0.0, 1.0);
                                cr.rectangle(0.0, 0.0, iw, ih);
                                cr.fill().unwrap();
                            }
                            cr.restore().unwrap();
                        }
                    }

                    let stride = mask_surf.stride() as usize;
                    let fw_usize = fw as usize;
                    let fh_usize = fh as usize;
                    let flat = {
                        let Ok(data) = mask_surf.data() else { continue };
                        let mut v = Vec::with_capacity(fw_usize * fh_usize);
                        for row in 0..fh_usize {
                            v.extend_from_slice(&data[row * stride..row * stride + fw_usize]);
                        }
                        v
                    };

                    let provider = crate::text_flow::PrecomputedFlowProvider::from_a8_mask(
                        &flat, fw_usize, fh_usize, FLOW_PADDING_PX,
                    );
                    page_flow_providers.insert(item.id.clone(), provider);
                }
            }
        }

        // Master page items (rendered at full opacity for PDF export)
        if let Some(ref mp_id) = page.master_page {
            if let Some(mp) = document.master_pages.iter().find(|m| m.id == *mp_id) {
                let page_num = page_idx + 1;
                let total = document.pages.len();
                for item in &mp.items {
                    cr.save()?;
                    cr.translate(item.x * mm_to_points, item.y * mm_to_points);
                    cr.rotate(item.rotation.to_radians());

                    let w = item.width * mm_to_points;
                    let h = item.height * mm_to_points;

                    match &item.content {
                        ItemContent::Text(tb) => {
                            let subs = tb.substitute_page_numbers(page_num, total);
                            subs.render(&cr, &pango_ctx, w, h, false, false, item.show_border, mm_to_points, None, None, true);
                        }
                        ItemContent::Image(ib) => {
                            let surface = ib.image_path.as_ref()
                                .and_then(|p| images.get(p.as_str()))
                                .map(|rc| rc.as_ref());
                            ib.render(&cr, w, h, false, item.show_border, surface);
                        }
                        ItemContent::Svg(sb) => {
                            let handle = svg_handles.get(&sb.svg_path)
                                .map(|rc| rc.as_ref());
                            sb.render(&cr, w, h, false, item.show_border, handle);
                        }
                        ItemContent::Shape => {
                            cr.set_source_rgba(0.85, 0.85, 0.95, 1.0);
                            cr.rectangle(0.0, 0.0, w, h);
                            cr.fill()?;
                            cr.set_source_rgba(0.5, 0.5, 0.7, 1.0);
                            cr.set_line_width(1.0);
                            cr.rectangle(0.0, 0.0, w, h);
                            cr.stroke()?;
                        }
                    }

                    cr.restore()?;
                }
            }
        }

        draw_page_content(&cr, &pango_ctx, page, images, svg_handles, &[], false, false, mm_to_points, &[], &visible_lengths, &page_flow_providers, None, true, None, 0);

        cr.restore()?;
        cr.show_page()?;
    }

    surface.finish();
    Ok(())
}

// ── Custom font dialog ────────────────────────────────────────────────────────

/// Opens a modal font picker that lists every Pango family / face and
/// reconstructs the exact family string needed for fontconfig (e.g.
/// "EB Garamond 12" instead of the generic "EB Garamond").
fn show_custom_font_dialog(
    parent: &adw::ApplicationWindow,
    sender: relm4::ComponentSender<AppModel>,
) {
    use pangocairo::prelude::{FontMapExt, FontFamilyExt, FontFaceExt};

    let dialog = gtk::Window::builder()
        .title("Choose Font")
        .modal(true)
        .transient_for(parent.upcast_ref::<gtk::Window>())
        .default_width(500)
        .default_height(400)
        .build();

    let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    hbox.set_margin_top(12);
    hbox.set_margin_bottom(12);
    hbox.set_margin_start(12);
    hbox.set_margin_end(12);

    // Left: family list
    let fam_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .width_request(200)
        .build();
    let fam_list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::Single)
        .build();
    fam_scroll.set_child(Some(&fam_list));

    // Right: face list
    let face_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .width_request(200)
        .build();
    let face_list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::Single)
        .build();
    face_scroll.set_child(Some(&face_list));

    hbox.append(&fam_scroll);
    hbox.append(&face_scroll);

    // Bottom: preview entry + buttons
    let vbox = gtk::Box::new(gtk::Orientation::Vertical, 12);
    vbox.append(&hbox);

    let preview_entry = gtk::Entry::builder()
        .placeholder_text("Selected font")
        .build();
    vbox.append(&preview_entry);

    let btn_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    btn_box.set_halign(gtk::Align::End);
    let ok_btn = gtk::Button::with_label("OK");
    let cancel_btn = gtk::Button::with_label("Cancel");
    btn_box.append(&ok_btn);
    btn_box.append(&cancel_btn);
    vbox.append(&btn_box);

    dialog.set_child(Some(&vbox));

    // Populate families
    let font_map = pangocairo::FontMap::default();
    let families: Vec<gtk::pango::FontFamily> = font_map.list_families();
    let mut family_data: Vec<(String, Vec<gtk::pango::FontFace>)> = Vec::new();
    for family in families {
        let name = family.name().to_string();
        let faces = family.list_faces();
        family_data.push((name, faces));
    }
    // Sort by name
    family_data.sort_by(|a, b| a.0.cmp(&b.0));

    let family_data_ref = std::rc::Rc::new(std::cell::RefCell::new(family_data));

    for (name, _) in family_data_ref.borrow().iter() {
        let row = gtk::ListBoxRow::new();
        let label = gtk::Label::new(Some(name));
        label.set_halign(gtk::Align::Start);
        label.set_margin_start(6);
        label.set_margin_end(6);
        label.set_margin_top(6);
        label.set_margin_bottom(6);
        row.set_child(Some(&label));
        fam_list.append(&row);
    }

    // Track which family is currently selected so the face callback can
    // reconstruct the exact name without parsing the preview entry text.
    let selected_family_idx: std::rc::Rc<std::cell::Cell<Option<usize>>> =
        std::rc::Rc::new(std::cell::Cell::new(None));

    // When family selected → populate faces
    let face_list_weak = face_list.downgrade();
    let preview_weak = preview_entry.downgrade();
    let family_data_clone = family_data_ref.clone();
    let sel_fam_idx_clone = selected_family_idx.clone();
    fam_list.connect_row_selected(move |_, row| {
        let Some(face_list) = face_list_weak.upgrade() else { return };
        let Some(preview) = preview_weak.upgrade() else { return };

        // Clear faces
        while let Some(child) = face_list.last_child() {
            face_list.remove(&child);
        }

        let Some(row) = row else {
            sel_fam_idx_clone.set(None);
            return;
        };
        let idx = row.index() as usize;
        sel_fam_idx_clone.set(Some(idx));

        let data = family_data_clone.borrow();
        let Some((family_name, faces)) = data.get(idx) else { return };

        for face in faces {
            let face_name = face.face_name().to_string();
            let row = gtk::ListBoxRow::new();
            let label = gtk::Label::new(Some(&face_name));
            label.set_halign(gtk::Align::Start);
            label.set_margin_start(6);
            label.set_margin_end(6);
            label.set_margin_top(6);
            label.set_margin_bottom(6);
            row.set_child(Some(&label));
            face_list.append(&row);
        }
        // Default preview = family base
        preview.set_text(family_name);
    });

    // When face selected → update preview with reconstructed exact name
    let preview_weak2 = preview_entry.downgrade();
    let family_data_clone2 = family_data_ref.clone();
    let sel_fam_idx_clone2 = selected_family_idx.clone();
    face_list.connect_row_selected(move |_, row| {
        let Some(preview) = preview_weak2.upgrade() else { return };
        let Some(row) = row else { return };

        let face_idx = row.index() as usize;
        let fam_idx = sel_fam_idx_clone2.get();
        let Some(fam_idx) = fam_idx else { return };

        let data = family_data_clone2.borrow();
        let Some((family_name, faces)) = data.get(fam_idx) else { return };
        let Some(face) = faces.get(face_idx) else { return };

        let face_name = face.face_name().to_string();
        // Reconstruct exact family name:
        // If face_name starts with digits (optical size), prepend them to family.
        let reconstructed = if let Some(first_token) = face_name.split_whitespace().next() {
            if first_token.chars().all(|c| c.is_ascii_digit()) {
                format!("{} {}", family_name, first_token)
            } else {
                family_name.clone()
            }
        } else {
            family_name.clone()
        };
        preview.set_text(&reconstructed);
    });

    // OK button
    let s = sender.clone();
    let dialog_weak = dialog.downgrade();
    ok_btn.connect_clicked(move |_| {
        let Some(dialog) = dialog_weak.upgrade() else { return };
        let font_text = preview_entry.text().to_string();
        if !font_text.is_empty() {
            s.input(AppInput::SetFontFamily(font_text));
        }
        dialog.close();
    });

    // Cancel button
    let dialog_for_cancel = dialog.clone();
    cancel_btn.connect_clicked(move |_| {
        dialog_for_cancel.close();
    });

    dialog.present();
}
