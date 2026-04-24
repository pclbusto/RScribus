use gtk4::gdk;
use gtk4::pango;
use gtk4::prelude::DisplayExt;
use serde::{Deserialize, Serialize};

const DEFAULT_FONT: &str = "Sans 11";
const DEFAULT_PADDING: f64 = 4.0;

#[derive(Debug)]
pub enum KeyAction {
    Handled,
    ExitEdit,
    RequestPaste,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextBox {
    pub text: String,
    pub font_description: String,
    pub padding: f64,
    pub line_spacing: f64,
    #[serde(skip)]
    pub cursor_pos: usize,
    #[serde(skip)]
    pub selection_anchor: Option<usize>,
}

impl Default for TextBox {
    fn default() -> Self {
        Self {
            text: String::new(),
            font_description: DEFAULT_FONT.to_string(),
            padding: DEFAULT_PADDING,
            line_spacing: 1.0,
            cursor_pos: 0,
            selection_anchor: None,
        }
    }
}

impl TextBox {
    pub fn new(text: String) -> Self {
        Self { text, ..Default::default() }
    }

    pub fn selection_range(&self) -> Option<(usize, usize)> {
        let anchor = self.selection_anchor?;
        if anchor == self.cursor_pos { return None; }
        Some((anchor.min(self.cursor_pos), anchor.max(self.cursor_pos)))
    }

    pub fn delete_selection(&mut self) -> bool {
        if let Some((start, end)) = self.selection_range() {
            self.text.drain(start..end);
            self.cursor_pos = start;
            self.selection_anchor = None;
            true
        } else {
            false
        }
    }

    pub fn select_all(&mut self) {
        self.selection_anchor = Some(0);
        self.cursor_pos = self.text.len();
    }

    /// Selects the word containing `pos`. If `pos` is on whitespace, just places the cursor.
    pub fn select_word_at(&mut self, pos: usize) {
        let pos = pos.min(self.text.len());

        let start = {
            let mut i = pos;
            while i > 0 {
                let prev = prev_char_boundary(&self.text, i);
                let ch = self.text[prev..i].chars().next().unwrap_or(' ');
                if ch.is_whitespace() { break; }
                i = prev;
            }
            i
        };

        let end = {
            let mut i = pos;
            while i < self.text.len() {
                let next = next_char_boundary(&self.text, i);
                let ch = self.text[i..next].chars().next().unwrap_or(' ');
                if ch.is_whitespace() { break; }
                i = next;
            }
            i
        };

        if start < end {
            self.selection_anchor = Some(start);
            self.cursor_pos = end;
        } else {
            self.cursor_pos = pos;
            self.selection_anchor = None;
        }
    }

    pub fn insert_char(&mut self, ch: char) {
        self.delete_selection();
        self.text.insert(self.cursor_pos, ch);
        self.cursor_pos += ch.len_utf8();
    }

    pub fn insert_text(&mut self, s: &str) {
        self.delete_selection();
        self.text.insert_str(self.cursor_pos, s);
        self.cursor_pos += s.len();
    }

    pub fn delete_backward(&mut self) {
        if !self.delete_selection() && self.cursor_pos > 0 {
            let prev = prev_char_boundary(&self.text, self.cursor_pos);
            self.text.drain(prev..self.cursor_pos);
            self.cursor_pos = prev;
        }
    }

    pub fn delete_forward(&mut self) {
        if !self.delete_selection() && self.cursor_pos < self.text.len() {
            let next = next_char_boundary(&self.text, self.cursor_pos);
            self.text.drain(self.cursor_pos..next);
        }
    }

    pub fn copy_selection(&self) {
        if let Some((start, end)) = self.selection_range() {
            let selected = self.text[start..end].to_string();
            gdk::Display::default()
                .expect("no display")
                .clipboard()
                .set_text(&selected);
        }
    }

    pub fn cut_selection(&mut self) {
        self.copy_selection();
        self.delete_selection();
    }

    pub fn move_cursor_left(&mut self, extend: bool) {
        if extend {
            if self.selection_anchor.is_none() {
                self.selection_anchor = Some(self.cursor_pos);
            }
            if self.cursor_pos > 0 {
                self.cursor_pos = prev_char_boundary(&self.text, self.cursor_pos);
            }
        } else {
            if let Some((start, _)) = self.selection_range() {
                self.cursor_pos = start;
            } else if self.cursor_pos > 0 {
                self.cursor_pos = prev_char_boundary(&self.text, self.cursor_pos);
            }
            self.selection_anchor = None;
        }
    }

    pub fn move_cursor_right(&mut self, extend: bool) {
        if extend {
            if self.selection_anchor.is_none() {
                self.selection_anchor = Some(self.cursor_pos);
            }
            if self.cursor_pos < self.text.len() {
                self.cursor_pos = next_char_boundary(&self.text, self.cursor_pos);
            }
        } else {
            if let Some((_, end)) = self.selection_range() {
                self.cursor_pos = end;
            } else if self.cursor_pos < self.text.len() {
                self.cursor_pos = next_char_boundary(&self.text, self.cursor_pos);
            }
            self.selection_anchor = None;
        }
    }

    pub fn move_cursor_home(&mut self, extend: bool) {
        if extend && self.selection_anchor.is_none() {
            self.selection_anchor = Some(self.cursor_pos);
        } else if !extend {
            self.selection_anchor = None;
        }
        let before = &self.text[..self.cursor_pos];
        self.cursor_pos = before.rfind('\n').map(|i| i + 1).unwrap_or(0);
    }

    pub fn move_cursor_end(&mut self, extend: bool) {
        if extend && self.selection_anchor.is_none() {
            self.selection_anchor = Some(self.cursor_pos);
        } else if !extend {
            self.selection_anchor = None;
        }
        let after = &self.text[self.cursor_pos..];
        self.cursor_pos = after.find('\n')
            .map(|i| self.cursor_pos + i)
            .unwrap_or(self.text.len());
    }

    pub fn handle_key(&mut self, key: gdk::Key, state: gdk::ModifierType) -> KeyAction {
        let ctrl = state.contains(gdk::ModifierType::CONTROL_MASK);
        let shift = state.contains(gdk::ModifierType::SHIFT_MASK);

        if ctrl {
            return match key {
                gdk::Key::a | gdk::Key::A => { self.select_all(); KeyAction::Handled }
                gdk::Key::c | gdk::Key::C => { self.copy_selection(); KeyAction::Handled }
                gdk::Key::x | gdk::Key::X => { self.cut_selection(); KeyAction::Handled }
                gdk::Key::v | gdk::Key::V => KeyAction::RequestPaste,
                _ => KeyAction::Handled,
            };
        }

        match key {
            gdk::Key::Escape => KeyAction::ExitEdit,
            gdk::Key::Left => { self.move_cursor_left(shift); KeyAction::Handled }
            gdk::Key::Right => { self.move_cursor_right(shift); KeyAction::Handled }
            gdk::Key::Home => { self.move_cursor_home(shift); KeyAction::Handled }
            gdk::Key::End => { self.move_cursor_end(shift); KeyAction::Handled }
            gdk::Key::BackSpace => { self.delete_backward(); KeyAction::Handled }
            gdk::Key::Delete => { self.delete_forward(); KeyAction::Handled }
            gdk::Key::Return | gdk::Key::KP_Enter => { self.insert_char('\n'); KeyAction::Handled }
            _ => {
                if let Some(ch) = key.to_unicode() {
                    if !ch.is_control() {
                        self.insert_char(ch);
                    }
                }
                KeyAction::Handled
            }
        }
    }

    /// Creates a Pango layout tied to a Cairo context for rendering.
    pub fn prepare_layout(&self, cr: &cairo::Context, frame_w_px: f64) -> pango::Layout {
        let pscale = pango::SCALE as f64;
        let layout = pangocairo::functions::create_layout(cr);
        layout.set_text(&self.text);
        let font_desc = pango::FontDescription::from_string(&self.font_description);
        layout.set_font_description(Some(&font_desc));
        layout.set_width(((frame_w_px - 2.0 * self.padding) * pscale) as i32);
        layout.set_wrap(pango::WrapMode::Word);
        layout
    }

    /// Renders the text box (background, border, text, selection, cursor) into cr.
    /// The Cairo context must be translated to the item's origin before calling.
    pub fn render(
        &self,
        cr: &cairo::Context,
        w: f64,
        h: f64,
        is_selected: bool,
        is_editing: bool,
    ) {
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

        let layout = self.prepare_layout(cr, w);
        let pscale = pango::SCALE as f64;

        if is_editing {
            if let Some((sel_start, sel_end)) = self.selection_range() {
                if sel_start < sel_end && sel_end <= self.text.len() {
                    let attrs = pango::AttrList::new();

                    let mut bg: pango::Attribute =
                        pango::AttrColor::new_background(0x3535, 0x8484, 0xe4e4).into();
                    bg.set_start_index(sel_start as u32);
                    bg.set_end_index(sel_end as u32);
                    attrs.insert(bg);

                    let mut fg: pango::Attribute =
                        pango::AttrColor::new_foreground(0xffff, 0xffff, 0xffff).into();
                    fg.set_start_index(sel_start as u32);
                    fg.set_end_index(sel_end as u32);
                    attrs.insert(fg);

                    layout.set_attributes(Some(&attrs));
                }
            }
        }

        cr.set_source_rgb(0.1, 0.1, 0.1);
        cr.move_to(self.padding, self.padding);
        pangocairo::functions::show_layout(cr, &layout);

        if is_editing && self.selection_range().is_none() {
            let byte_idx = self.cursor_pos.min(self.text.len()) as i32;
            let (strong, _) = layout.cursor_pos(byte_idx);
            let cx = self.padding + strong.x() as f64 / pscale;
            let cy = self.padding + strong.y() as f64 / pscale;
            let ch = strong.height() as f64 / pscale;

            cr.set_source_rgb(0.1, 0.1, 0.9);
            cr.set_line_width(1.5);
            cr.move_to(cx, cy + 1.0);
            cr.line_to(cx, cy + ch - 1.0);
            cr.stroke().unwrap();
        }
    }

    /// Maps a click position (in mm) to a byte index in the text.
    /// `scale` is the mm-to-pixel ratio (SCALE constant); `frame_w_px` is item.width * scale.
    pub fn hit_test(
        &self,
        frame_x: f64,
        frame_y: f64,
        click_x_mm: f64,
        click_y_mm: f64,
        scale: f64,
        frame_w_px: f64,
    ) -> usize {
        use pango::prelude::FontMapExt;

        let pscale = pango::SCALE as f64;
        let font_map = pangocairo::FontMap::default();
        let context = font_map.create_context();
        let layout = pango::Layout::new(&context);
        layout.set_text(&self.text);
        let font_desc = pango::FontDescription::from_string(&self.font_description);
        layout.set_font_description(Some(&font_desc));
        layout.set_width(((frame_w_px - 2.0 * self.padding) * pscale) as i32);
        layout.set_wrap(pango::WrapMode::Word);

        let rel_x = (click_x_mm - frame_x) * scale - self.padding;
        let rel_y = (click_y_mm - frame_y) * scale - self.padding;
        let x_pango = (rel_x * pscale).max(0.0) as i32;
        let y_pango = (rel_y * pscale).max(0.0) as i32;

        let (_inside, byte_idx, trailing) = layout.xy_to_index(x_pango, y_pango);
        let byte_idx = byte_idx as usize;

        if trailing > 0 && byte_idx < self.text.len() {
            next_char_boundary(&self.text, byte_idx)
        } else {
            byte_idx
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
