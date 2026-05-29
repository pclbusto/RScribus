use gtk4::gdk;
use gtk4::pango;
use gtk4::prelude::DisplayExt;
use serde::{Deserialize, Serialize};
use crate::text_flow::TextFlowProvider;

const DEFAULT_FONT: &str = "Sans 11";
const DEFAULT_PADDING: f64 = 4.0 / 3.0;

// ── Public types ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub enum TextAlign {
    #[default]
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AttrValue {
    Family(String),
    /// Font size in points.
    Size(f64),
    Bold(bool),
    Italic(bool),
    Underline(bool),
    /// RGB foreground color, 0–255 per channel.
    Color([u8; 3]),
    /// RGB background color, 0–255 per channel.
    BgColor([u8; 3]),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextAttribute {
    pub start: u32,
    pub end: u32,
    pub value: AttrValue,
}

/// One entry in the undo/redo history of a text frame.
#[derive(Debug, Clone)]
pub struct HistoryEntry {
    pub text: String,
    pub attributes: Vec<TextAttribute>,
    pub alignment: TextAlign,
    pub cursor_pos: usize,
    pub selection_anchor: Option<usize>,
    pub scroll_y: f64,
}

/// Snapshot of all format properties active at a single cursor position.
#[derive(Debug, Clone, Default)]
pub struct AttrSnapshot {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub family: Option<String>,
    pub size_pt: Option<f64>,
    pub color: Option<[u8; 3]>,
    pub bg_color: Option<[u8; 3]>,
}

// ── KeyAction ─────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum KeyAction {
    Handled,
    ExitEdit,
    RequestPaste,
    RequestCut,
    MoveVertical { up: bool, extend: bool },
    FormatBold,
    FormatItalic,
    FormatUnderline,
    Undo,
    Redo,
}

// ── TextBox ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextBox {
    pub text: String,
    pub font_description: String,
    pub padding: f64,
    pub line_spacing: f64,
    /// Per-span formatting applied on top of the base font.
    #[serde(default)]
    pub attributes: Vec<TextAttribute>,
    /// Paragraph-level alignment.
    #[serde(default)]
    pub alignment: TextAlign,
    /// ID of the next frame in the text chain.
    #[serde(default)]
    pub next_frame_id: Option<String>,
    /// ID of the previous frame in the text chain.
    #[serde(default)]
    pub prev_frame_id: Option<String>,
    /// Byte offset of this frame's text within the chain's global text.
    #[serde(default)]
    pub text_offset: usize,
    /// Cached overflow flag set by reflow_chain. Avoids repeated text_capacity calls during rendering.
    /// Only meaningful for the last frame in a chain; always false for standalone frames.
    #[serde(skip)]
    pub overflow_hint: bool,
    #[serde(skip)]
    pub cursor_pos: usize,
    #[serde(skip)]
    pub selection_anchor: Option<usize>,
    #[serde(skip)]
    pub scroll_y: f64,
    #[serde(skip)]
    pub undo_stack: Vec<HistoryEntry>,
    #[serde(skip)]
    pub redo_stack: Vec<HistoryEntry>,
}

impl Default for TextBox {
    fn default() -> Self {
        Self {
            text: String::new(),
            font_description: DEFAULT_FONT.to_string(),
            padding: DEFAULT_PADDING,
            line_spacing: 1.0,
            attributes: Vec::new(),
            alignment: TextAlign::default(),
            next_frame_id: None,
            prev_frame_id: None,
            text_offset: 0,
            overflow_hint: false,
            cursor_pos: 0,
            selection_anchor: None,
            scroll_y: 0.0,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
        }
    }
}

// ── Formatting API ────────────────────────────────────────────────────────────

impl TextBox {
    pub fn new(text: String) -> Self {
        Self { text, ..Default::default() }
    }

    /// Converts stored attributes to a `pango::AttrList` for layout rendering.
    pub fn build_attr_list(&self) -> pango::AttrList {
        let list = pango::AttrList::new();
        for attr in &self.attributes {
            let mut pa: pango::Attribute = match &attr.value {
                // AttrFamily passes the name directly to Pango/fontconfig without going
                // through FontDescription::from_string, which would mis-parse a trailing
                // number in the family name (e.g. "EB Garamond 12") as the point size,
                // stripping it and causing fontconfig to resolve the wrong optical variant.
                AttrValue::Family(f) => pango::AttrString::new_family(f).into(),
                AttrValue::Size(pt) => {
                    pango::AttrSize::new((*pt * pango::SCALE as f64) as i32).into()
                }
                AttrValue::Bold(b) => pango::AttrInt::new_weight(
                    if *b { pango::Weight::Bold } else { pango::Weight::Normal },
                ).into(),
                AttrValue::Italic(b) => pango::AttrInt::new_style(
                    if *b { pango::Style::Italic } else { pango::Style::Normal },
                ).into(),
                AttrValue::Underline(b) => pango::AttrInt::new_underline(
                    if *b { pango::Underline::Single } else { pango::Underline::None },
                ).into(),
                AttrValue::Color([r, g, b]) => {
                    let x = |v: u8| v as u16 * 257;
                    pango::AttrColor::new_foreground(x(*r), x(*g), x(*b)).into()
                }
                AttrValue::BgColor([r, g, b]) => {
                    let x = |v: u8| v as u16 * 257;
                    pango::AttrColor::new_background(x(*r), x(*g), x(*b)).into()
                }
            };
            pa.set_start_index(attr.start);
            pa.set_end_index(attr.end);
            list.insert(pa);
        }
        list
    }

    /// Applies `value` to the byte range `[start, end)`, replacing any
    /// existing attribute of the same kind that overlaps that range.
    pub fn apply_format(&mut self, start: usize, end: usize, value: AttrValue) {
        if start >= end { return; }
        let s = start as u32;
        let e = end as u32;
        let mut leftovers: Vec<TextAttribute> = Vec::new();

        self.attributes.retain(|attr| {
            if std::mem::discriminant(&attr.value) != std::mem::discriminant(&value) {
                return true;
            }
            if attr.end <= s || attr.start >= e {
                return true;
            }
            if attr.start < s {
                leftovers.push(TextAttribute { start: attr.start, end: s, value: attr.value.clone() });
            }
            if attr.end > e {
                leftovers.push(TextAttribute { start: e, end: attr.end, value: attr.value.clone() });
            }
            false
        });

        self.attributes.extend(leftovers);
        self.attributes.push(TextAttribute { start: s, end: e, value });
    }

    /// Removes all formatting from the byte range `[start, end)`.
    pub fn clear_format(&mut self, start: usize, end: usize) {
        if start >= end { return; }
        let s = start as u32;
        let e = end as u32;
        let mut leftovers: Vec<TextAttribute> = Vec::new();

        self.attributes.retain(|attr| {
            if attr.end <= s || attr.start >= e {
                return true;
            }
            if attr.start < s {
                leftovers.push(TextAttribute { start: attr.start, end: s, value: attr.value.clone() });
            }
            if attr.end > e {
                leftovers.push(TextAttribute { start: e, end: attr.end, value: attr.value.clone() });
            }
            false
        });

        self.attributes.extend(leftovers);
    }

    /// Returns a snapshot of all active formatting properties at byte `pos`.
    /// Later-stored attributes of the same kind override earlier ones.
    pub fn get_attr_at(&self, pos: usize) -> AttrSnapshot {
        let pos = pos as u32;
        let mut snap = AttrSnapshot::default();
        for attr in &self.attributes {
            if attr.start <= pos && attr.end > pos {
                match &attr.value {
                    AttrValue::Bold(b)      => snap.bold = *b,
                    AttrValue::Italic(b)    => snap.italic = *b,
                    AttrValue::Underline(b) => snap.underline = *b,
                    AttrValue::Family(f)    => snap.family = Some(f.clone()),
                    AttrValue::Size(s)      => snap.size_pt = Some(*s),
                    AttrValue::Color(c)     => snap.color = Some(*c),
                    AttrValue::BgColor(c)   => snap.bg_color = Some(*c),
                }
            }
        }
        snap
    }

    pub fn set_alignment(&mut self, align: TextAlign) { self.alignment = align; }
    pub fn get_alignment(&self) -> TextAlign { self.alignment }

    /// Selection range if active, otherwise the word boundaries around the cursor.
    pub fn selection_or_word_range(&self) -> (usize, usize) {
        if let Some(range) = self.selection_range() {
            return range;
        }
        let pos = self.cursor_pos.min(self.text.len());
        let start = {
            let mut i = pos;
            while i > 0 {
                let prev = prev_char_boundary(&self.text, i);
                if self.text[prev..i].chars().next().map_or(true, |c| c.is_whitespace()) { break; }
                i = prev;
            }
            i
        };
        let end = {
            let mut i = pos;
            while i < self.text.len() {
                let next = next_char_boundary(&self.text, i);
                if self.text[i..next].chars().next().map_or(true, |c| c.is_whitespace()) { break; }
                i = next;
            }
            i
        };
        (start, end)
    }

    /// Like `get_attr_at` but fills in family/size/bold/italic from `font_description`
    /// when no explicit attribute overrides them, so the result always reflects the
    /// effective (visible) format at that position.
    pub fn effective_snapshot_at(&self, pos: usize) -> AttrSnapshot {
        let mut snap = self.get_attr_at(pos);
        let fd = pango::FontDescription::from_string(&self.font_description);
        if snap.family.is_none() {
            snap.family = fd.family().map(|gs| gs.to_string());
        }
        if snap.size_pt.is_none() {
            let raw = fd.size() as f64 / pango::SCALE as f64;
            snap.size_pt = Some(if raw > 0.0 { raw } else { 11.0 });
        }
        let has_bold_attr = self.attributes.iter().any(|a|
            (a.start as usize) <= pos && (a.end as usize) > pos &&
            matches!(a.value, AttrValue::Bold(_))
        );
        if !has_bold_attr {
            snap.bold = fd.weight() == pango::Weight::Bold;
        }
        let has_italic_attr = self.attributes.iter().any(|a|
            (a.start as usize) <= pos && (a.end as usize) > pos &&
            matches!(a.value, AttrValue::Italic(_))
        );
        if !has_italic_attr {
            snap.italic = fd.style() == pango::Style::Italic;
        }
        snap
    }

}

// ── Undo / Redo ───────────────────────────────────────────────────────────────

const UNDO_LIMIT: usize = 30;

impl TextBox {
    fn snapshot(&self) -> HistoryEntry {
        HistoryEntry {
            text: self.text.clone(),
            attributes: self.attributes.clone(),
            alignment: self.alignment,
            cursor_pos: self.cursor_pos,
            selection_anchor: self.selection_anchor,
            scroll_y: self.scroll_y,
        }
    }

    fn restore(&mut self, entry: HistoryEntry) {
        self.text = entry.text;
        self.attributes = entry.attributes;
        self.alignment = entry.alignment;
        self.cursor_pos = entry.cursor_pos;
        self.selection_anchor = entry.selection_anchor;
        self.scroll_y = entry.scroll_y;
    }

    fn snapshot_differs_from_last(&self, snap: &HistoryEntry) -> bool {
        match self.undo_stack.last() {
            None => true,
            Some(last) => last.text != snap.text || last.alignment != snap.alignment
                || last.attributes.len() != snap.attributes.len(),
        }
    }

    /// Saves current state as an undo checkpoint and clears the redo stack.
    /// Must be called BEFORE the action that will change the text, so the
    /// stack holds the "state to return to" (not the already-modified state).
    pub fn push_history(&mut self) {
        let snap = self.snapshot();
        if !self.snapshot_differs_from_last(&snap) { return; }
        if self.undo_stack.len() >= UNDO_LIMIT {
            self.undo_stack.remove(0);
        }
        self.undo_stack.push(snap);
        self.redo_stack.clear();
    }

    pub fn undo(&mut self) -> bool {
        if let Some(prev) = self.undo_stack.pop() {
            let current = self.snapshot();
            self.redo_stack.push(current);
            self.restore(prev);
            true
        } else {
            false
        }
    }

    pub fn redo(&mut self) -> bool {
        if let Some(next) = self.redo_stack.pop() {
            let current = self.snapshot();
            self.undo_stack.push(current);
            self.restore(next);
            true
        } else {
            false
        }
    }
}

// ── Cursor & selection ────────────────────────────────────────────────────────

impl TextBox {
    pub fn selection_range(&self) -> Option<(usize, usize)> {
        let anchor = self.selection_anchor?;
        if anchor == self.cursor_pos { return None; }
        Some((anchor.min(self.cursor_pos), anchor.max(self.cursor_pos)))
    }

    pub fn delete_selection(&mut self) -> bool {
        if let Some((start, end)) = self.selection_range() {
            self.text.drain(start..end);
            shift_attrs_delete(&mut self.attributes, start, end - start);
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

    pub fn select_word_at(&mut self, pos: usize) {
        let pos = pos.min(self.text.len());

        let start = {
            let mut i = pos;
            while i > 0 {
                let prev = prev_char_boundary(&self.text, i);
                if self.text[prev..i].chars().next().unwrap_or(' ').is_whitespace() { break; }
                i = prev;
            }
            i
        };

        let end = {
            let mut i = pos;
            while i < self.text.len() {
                let next = next_char_boundary(&self.text, i);
                if self.text[i..next].chars().next().unwrap_or(' ').is_whitespace() { break; }
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
        let pos = self.cursor_pos;
        self.text.insert(self.cursor_pos, ch);
        let n = ch.len_utf8();
        shift_attrs_insert(&mut self.attributes, pos, n);
        self.cursor_pos += n;
    }

    pub fn insert_text(&mut self, s: &str) {
        self.delete_selection();
        let pos = self.cursor_pos;
        let n = s.len();
        self.text.insert_str(self.cursor_pos, s);
        shift_attrs_insert(&mut self.attributes, pos, n);
        self.cursor_pos += n;
    }

    pub fn delete_backward(&mut self) {
        if !self.delete_selection() && self.cursor_pos > 0 {
            let prev = prev_char_boundary(&self.text, self.cursor_pos);
            let n = self.cursor_pos - prev;
            self.text.drain(prev..self.cursor_pos);
            shift_attrs_delete(&mut self.attributes, prev, n);
            self.cursor_pos = prev;
        }
    }

    pub fn delete_forward(&mut self) {
        if !self.delete_selection() && self.cursor_pos < self.text.len() {
            let next = next_char_boundary(&self.text, self.cursor_pos);
            let n = next - self.cursor_pos;
            self.text.drain(self.cursor_pos..next);
            shift_attrs_delete(&mut self.attributes, self.cursor_pos, n);
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

}

// ── Page-number substitution ───────────────────────────────────────────────────

impl TextBox {
    /// Returns a copy with `{{page}}` and `{{numpages}}` replaced by the given values.
    pub fn substitute_page_numbers(&self, page_num: usize, total_pages: usize) -> Self {
        let mut tb = self.clone();
        tb.text = tb.text
            .replace("{{page}}", &page_num.to_string())
            .replace("{{numpages}}", &total_pages.to_string());
        tb
    }
}

// ── Cursor movement ───────────────────────────────────────────────────────────

impl TextBox {
    pub fn move_cursor_left(&mut self, extend: bool) {
        if extend {
            if self.selection_anchor.is_none() { self.selection_anchor = Some(self.cursor_pos); }
            if self.cursor_pos > 0 { self.cursor_pos = prev_char_boundary(&self.text, self.cursor_pos); }
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
            if self.selection_anchor.is_none() { self.selection_anchor = Some(self.cursor_pos); }
            if self.cursor_pos < self.text.len() { self.cursor_pos = next_char_boundary(&self.text, self.cursor_pos); }
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
        if extend && self.selection_anchor.is_none() { self.selection_anchor = Some(self.cursor_pos); }
        else if !extend { self.selection_anchor = None; }
        let before = &self.text[..self.cursor_pos];
        self.cursor_pos = before.rfind('\n').map(|i| i + 1).unwrap_or(0);
    }

    pub fn move_cursor_end(&mut self, extend: bool) {
        if extend && self.selection_anchor.is_none() { self.selection_anchor = Some(self.cursor_pos); }
        else if !extend { self.selection_anchor = None; }
        let after = &self.text[self.cursor_pos..];
        self.cursor_pos = after.find('\n')
            .map(|i| self.cursor_pos + i)
            .unwrap_or(self.text.len());
    }

    pub fn move_cursor_vertical(&mut self, up: bool, extend: bool, frame_w_px: f64, scale: f64) {
        let ctx = make_screen_pango_ctx(scale);
        let padding = self.padding * scale;
        let layout = self.prepare_layout(&ctx, frame_w_px, padding);

        let byte_idx = self.cursor_pos.min(self.text.len()) as i32;
        let (strong, _) = layout.cursor_pos(byte_idx);
        let cur_x = strong.x();
        let cur_y = strong.y();
        let line_h = strong.height();

        let new_y = if up { cur_y - line_h / 2 } else { cur_y + line_h + line_h / 2 };

        if extend && self.selection_anchor.is_none() { self.selection_anchor = Some(self.cursor_pos); }
        else if !extend { self.selection_anchor = None; }

        if new_y < 0 { self.cursor_pos = 0; return; }

        let (_inside, new_byte, trailing) = layout.xy_to_index(cur_x, new_y);
        let mut new_pos = new_byte as usize;
        if trailing > 0 && new_pos < self.text.len() {
            new_pos = next_char_boundary(&self.text, new_pos);
        }
        self.cursor_pos = new_pos;
    }

    pub fn handle_key(&mut self, key: gdk::Key, state: gdk::ModifierType) -> KeyAction {
        let ctrl = state.contains(gdk::ModifierType::CONTROL_MASK);
        let shift = state.contains(gdk::ModifierType::SHIFT_MASK);

        if ctrl {
            return match key {
                gdk::Key::a | gdk::Key::A => { self.select_all(); KeyAction::Handled }
                gdk::Key::c | gdk::Key::C => { self.copy_selection(); KeyAction::Handled }
                gdk::Key::x | gdk::Key::X => { self.copy_selection(); KeyAction::RequestCut }
                gdk::Key::v | gdk::Key::V => KeyAction::RequestPaste,
                gdk::Key::b | gdk::Key::B => KeyAction::FormatBold,
                gdk::Key::i | gdk::Key::I => KeyAction::FormatItalic,
                gdk::Key::u | gdk::Key::U => KeyAction::FormatUnderline,
                gdk::Key::z | gdk::Key::Z => {
                    if shift { KeyAction::Redo } else { KeyAction::Undo }
                }
                gdk::Key::y | gdk::Key::Y => KeyAction::Redo,
                _ => KeyAction::Handled,
            };
        }

        match key {
            gdk::Key::Escape => KeyAction::ExitEdit,
            gdk::Key::Left   => { self.move_cursor_left(shift); KeyAction::Handled }
            gdk::Key::Right  => { self.move_cursor_right(shift); KeyAction::Handled }
            gdk::Key::Up     => KeyAction::MoveVertical { up: true, extend: shift },
            gdk::Key::Down   => KeyAction::MoveVertical { up: false, extend: shift },
            gdk::Key::Home   => { self.move_cursor_home(shift); KeyAction::Handled }
            gdk::Key::End    => { self.move_cursor_end(shift); KeyAction::Handled }
            gdk::Key::BackSpace           => { self.delete_backward(); KeyAction::Handled }
            gdk::Key::Delete              => { self.delete_forward(); KeyAction::Handled }
            gdk::Key::Return | gdk::Key::KP_Enter => { self.insert_char('\n'); KeyAction::Handled }
            _ => {
                if let Some(ch) = key.to_unicode() {
                    if !ch.is_control() { self.insert_char(ch); }
                }
                KeyAction::Handled
            }
        }
    }
}

fn make_screen_pango_ctx(scale: f64) -> pango::Context {
    use pango::prelude::FontMapExt;
    let font_map = pangocairo::FontMap::default();
    let ctx = font_map.create_context();
    pangocairo::functions::context_set_resolution(&ctx, 25.4 * scale);
    if let Ok(mut opts) = cairo::FontOptions::new() {
        opts.set_hint_metrics(cairo::HintMetrics::Off);
        opts.set_hint_style(cairo::HintStyle::Slight);
        pangocairo::functions::context_set_font_options(&ctx, Some(&opts));
    }
    ctx
}

// ── Layout & rendering ────────────────────────────────────────────────────────

impl TextBox {
    /// Builds a Pango layout with font, width, wrapping, text attributes and alignment.
    pub fn prepare_layout(&self, pango_ctx: &pango::Context, frame_w: f64, padding: f64) -> pango::Layout {
        let started = std::time::Instant::now();
        let pscale = pango::SCALE as f64;
        let layout = pango::Layout::new(pango_ctx);

        // PERFORMANCE OPTIMIZATION: Any frame in a chain only shows a portion of the
        // text (the suffix model clips at frame bounds). We don't need Pango to layout
        // 100k characters if we only show 500. Apply the cap to all chained frames,
        // including the last one (prev_frame_id.is_some() without a next).
        let (display_text, display_attrs) = if (self.next_frame_id.is_some() || self.prev_frame_id.is_some()) && self.text.len() > 5000 {
            // Ensure we include the cursor if we are editing
            let limit = (self.cursor_pos + 1000).max(5000).min(self.text.len());
            let mut end = limit;
            while end < self.text.len() && !self.text.is_char_boundary(end) { end += 1; }
            
            let sliced_text = &self.text[..end];
            let sliced_attrs = build_attr_list_slice(&self.attributes, 0, end);
            (sliced_text, Some(sliced_attrs))
        } else {
            (self.text.as_str(), None)
        };

        layout.set_text(display_text);
        let font_desc = pango::FontDescription::from_string(&self.font_description);
        layout.set_font_description(Some(&font_desc));
        let resolution = pangocairo::functions::context_get_resolution(pango_ctx);
        let pt_per_px = if resolution > 0.0 { 72.0 / resolution } else { 1.0 };
        layout.set_width(((frame_w - 2.0 * padding) * pt_per_px * pscale) as i32);
        layout.set_wrap(pango::WrapMode::Word);
        
        if let Some(attrs) = display_attrs {
            layout.set_attributes(Some(&attrs));
        } else if !self.attributes.is_empty() {
            layout.set_attributes(Some(&self.build_attr_list()));
        }

        layout.set_alignment(match self.alignment {
            TextAlign::Left   => pango::Alignment::Left,
            TextAlign::Center => pango::Alignment::Center,
            TextAlign::Right  => pango::Alignment::Right,
        });
        if self.line_spacing != 1.0 {
            layout.set_line_spacing(self.line_spacing as f32);
        }

        let elapsed = started.elapsed();
        if display_text.len() > 2_000 || elapsed.as_millis() >= 8 {
            eprintln!(
                "[perf] prepare_layout text_len={} display_len={} attrs={} linked={} width_px={:.1} padding_px={:.1} took={}ms",
                self.text.len(),
                display_text.len(),
                self.attributes.len(),
                self.next_frame_id.is_some() || self.prev_frame_id.is_some(),
                frame_w,
                padding,
                elapsed.as_millis()
            );
        }
        layout
    }

    /// Like `prepare_layout` but uses `text` and `attrs` directly instead of `self.text`/`self.attributes`.
    /// Used by the render cache to build truncated-frame layouts without cloning the TextBox.
    pub fn prepare_layout_for_slice(
        &self,
        pango_ctx: &pango::Context,
        frame_w: f64,
        padding: f64,
        text: &str,
        attrs: &[TextAttribute],
    ) -> pango::Layout {
        let pscale = pango::SCALE as f64;
        let layout = pango::Layout::new(pango_ctx);
        layout.set_text(text);
        let fd = pango::FontDescription::from_string(&self.font_description);
        layout.set_font_description(Some(&fd));
        let resolution = pangocairo::functions::context_get_resolution(pango_ctx);
        let pt_per_px = if resolution > 0.0 { 72.0 / resolution } else { 1.0 };
        layout.set_width(((frame_w - 2.0 * padding) * pt_per_px * pscale) as i32);
        layout.set_wrap(pango::WrapMode::Word);
        if !attrs.is_empty() {
            layout.set_attributes(Some(&build_attr_list_slice(attrs, 0, text.len())));
        }
        layout.set_alignment(match self.alignment {
            TextAlign::Left => pango::Alignment::Left,
            TextAlign::Center => pango::Alignment::Center,
            TextAlign::Right => pango::Alignment::Right,
        });
        if self.line_spacing != 1.0 {
            layout.set_line_spacing(self.line_spacing as f32);
        }
        layout
    }

    /// Returns the `scroll_y` value that keeps the cursor inside `[0, h]`.
    pub fn ensure_cursor_visible(
        &self,
        pango_ctx: &pango::Context,
        w: f64,
        h: f64,
        scale_factor: f64,
    ) -> f64 {
        let padding = self.padding * scale_factor;
        let layout = self.prepare_layout(pango_ctx, w, padding);
        let pscale = pango::SCALE as f64;

        let byte_idx = self.cursor_pos.min(self.text.len()) as i32;
        let (strong, _) = layout.cursor_pos(byte_idx);
        let cursor_top = padding + strong.y() as f64 / pscale;
        let cursor_bot = cursor_top + strong.height() as f64 / pscale;

        let new_scroll = if cursor_bot > self.scroll_y + h {
            cursor_bot - h
        } else if cursor_top < self.scroll_y {
            cursor_top
        } else {
            return self.scroll_y;
        };

        let (_, ph) = layout.size();
        let total_h = ph as f64 / pscale + 2.0 * padding;
        new_scroll.clamp(0.0, (total_h - h).max(0.0))
    }

    /// Returns the height required to render all text at the given width.
    pub fn required_height(&self, pango_ctx: &pango::Context, w: f64, scale_factor: f64) -> f64 {
        let started = std::time::Instant::now();
        let padding = self.padding * scale_factor;
        let layout = self.prepare_layout(pango_ctx, w, padding);
        let (_, ph) = layout.size();
        let result = ph as f64 / pango::SCALE as f64 + 2.0 * padding;
        let elapsed = started.elapsed();
        if self.text.len() > 2_000 || elapsed.as_millis() >= 8 {
            eprintln!(
                "[perf] required_height text_len={} attrs={} linked={} width_px={:.1} result_px={:.1} took={}ms",
                self.text.len(),
                self.attributes.len(),
                self.next_frame_id.is_some() || self.prev_frame_id.is_some(),
                w,
                result,
                elapsed.as_millis()
            );
        }
        result
    }

    /// Fast overflow check for non-editing render paths.
    /// For very large texts we avoid measuring the full required height and instead
    /// ask how much of a safe prefix would fit in the frame.
    pub fn overflows_frame(&self, pango_ctx: &pango::Context, w: f64, h: f64, scale_factor: f64) -> bool {
        if self.text.is_empty() {
            return false;
        }

        if self.text.len() > 5_000 {
            let mut probe = self.clone();
            probe.cursor_pos = 0;
            if probe.next_frame_id.is_none() {
                probe.next_frame_id = Some("__overflow_probe__".to_string());
            }
            return probe.text_capacity(w, h, scale_factor) < self.text.len();
        }

        self.required_height(pango_ctx, w, scale_factor) > h
    }

    /// Returns how many bytes of `text` fit within the frame dimensions.
    /// Avoids allocating a TextBox instance — callers pass only the relevant fields.
    pub fn measure_capacity(
        pango_ctx: &pango::Context,
        text: &str,
        attrs: &[TextAttribute],
        font_desc: &str,
        padding: f64,
        line_spacing: f64,
        align: TextAlign,
        w_px: f64,
        h_px: f64,
        scale: f64,
    ) -> usize {
        if text.is_empty() {
            return 0;
        }
        let pscale = pango::SCALE as f64;
        let padding_px = padding * scale;

        let layout = pango::Layout::new(pango_ctx);
        layout.set_text(text);
        let fd = pango::FontDescription::from_string(font_desc);
        layout.set_font_description(Some(&fd));
        let resolution = pangocairo::functions::context_get_resolution(pango_ctx);
        let pt_per_px = if resolution > 0.0 { 72.0 / resolution } else { 1.0 };
        layout.set_width(((w_px - 2.0 * padding_px) * pt_per_px * pscale) as i32);
        layout.set_wrap(pango::WrapMode::Word);

        if !attrs.is_empty() {
            layout.set_attributes(Some(&build_attr_list_slice(attrs, 0, text.len())));
        }

        layout.set_alignment(match align {
            TextAlign::Left => pango::Alignment::Left,
            TextAlign::Center => pango::Alignment::Center,
            TextAlign::Right => pango::Alignment::Right,
        });

        if line_spacing != 1.0 {
            layout.set_line_spacing(line_spacing as f32);
        }

        let available_h_pango = ((h_px - 2.0 * padding_px) * pscale).max(0.0) as i32;
        let (_, total_h) = layout.size();

        if total_h <= available_h_pango {
            return text.len();
        }

        let n_lines = layout.line_count();
        let mut last_fit_end = 0usize;

        for i in 0..n_lines {
            if let Some(line) = layout.line(i) {
                let start_byte = line.start_index() as i32;
                let (strong, _) = layout.cursor_pos(start_byte);
                let line_bottom = strong.y() + strong.height();
                if line_bottom > available_h_pango {
                    break;
                }
                last_fit_end = (line.start_index() as usize + line.length() as usize)
                    .min(text.len());
            }
        }

        while last_fit_end > 0 && !text.is_char_boundary(last_fit_end) {
            last_fit_end -= 1;
        }

        last_fit_end
    }

    /// Returns how many bytes of `self.text` fit within the frame dimensions.
    /// Used during chain reflow to split text across linked frames.
    pub fn text_capacity(&self, w_px: f64, h_px: f64, scale: f64) -> usize {
        let started = std::time::Instant::now();
        if self.text.is_empty() {
            return 0;
        }
        let ctx = make_screen_pango_ctx(scale);
        let padding = self.padding * scale;
        let layout = self.prepare_layout(&ctx, w_px, padding);
        let pscale = pango::SCALE as f64;
        let available_h_pango = ((h_px - 2.0 * padding) * pscale).max(0.0) as i32;

        // Fast path: all text fits
        let (_, total_h) = layout.size();
        if total_h <= available_h_pango {
            let elapsed = started.elapsed();
            if self.text.len() > 2_000 || elapsed.as_millis() >= 8 {
                eprintln!(
                    "[perf] text_capacity text_len={} attrs={} width_px={:.1} height_px={:.1} fit_all=true capacity={} took={}ms",
                    self.text.len(),
                    self.attributes.len(),
                    w_px,
                    h_px,
                    self.text.len(),
                    elapsed.as_millis()
                );
            }
            return self.text.len();
        }

        let n_lines = layout.line_count();
        let mut last_fit_end = 0usize;

        for i in 0..n_lines {
            if let Some(line) = layout.line(i) {
                let start_byte = line.start_index() as i32;
                let (strong, _) = layout.cursor_pos(start_byte);
                let line_bottom = strong.y() + strong.height();
                if line_bottom > available_h_pango {
                    break;
                }
                last_fit_end = (line.start_index() as usize + line.length() as usize)
                    .min(self.text.len());
            }
        }

        // Ensure valid char boundary
        while last_fit_end > 0 && !self.text.is_char_boundary(last_fit_end) {
            last_fit_end -= 1;
        }

        let elapsed = started.elapsed();
        if self.text.len() > 2_000 || elapsed.as_millis() >= 8 {
            eprintln!(
                "[perf] text_capacity text_len={} attrs={} width_px={:.1} height_px={:.1} fit_all=false capacity={} lines={} took={}ms",
                self.text.len(),
                self.attributes.len(),
                w_px,
                h_px,
                last_fit_end,
                n_lines,
                elapsed.as_millis()
            );
        }

        last_fit_end
    }

    /// Renders the text box into `cr` (translated to item origin).
    /// When `flow` is provided the text is laid out around the obstacles
    /// described by the provider; otherwise the standard single-column
    /// layout is used.
    pub fn render(
        &self,
        cr: &cairo::Context,
        pango_ctx: &pango::Context,
        w: f64,
        h: f64,
        is_selected: bool,
        is_editing: bool,
        show_border: bool,
        scale_factor: f64,
        flow: Option<&dyn TextFlowProvider>,
        prepared_layout: Option<&pango::Layout>,
        is_export: bool,
    ) {
        let padding = self.padding * scale_factor;

        if is_editing {
            cr.set_source_rgb(0.97, 0.97, 1.0);
            cr.rectangle(0.0, 0.0, w, h);
            cr.fill().unwrap();
        }

        if (is_selected || show_border) && !is_export {
            if is_selected {
                cr.set_source_rgb(0.0, 0.5, 1.0);
                cr.set_line_width(2.0);
            } else {
                cr.set_source_rgb(0.3, 0.3, 0.3);
                cr.set_line_width(1.0);
            }
            cr.rectangle(0.5, 0.5, w - 1.0, h - 1.0);
            cr.stroke().unwrap();
        }

        if !is_export {
            if let Ok(mut fo) = cairo::FontOptions::new() {
                fo.set_antialias(cairo::Antialias::Good);
                fo.set_hint_style(cairo::HintStyle::Slight);
                fo.set_hint_metrics(cairo::HintMetrics::On);
                cr.set_font_options(&fo);
                pangocairo::functions::context_set_font_options(pango_ctx, Some(&fo));
            }
        }

        if let Some(provider) = flow {
            self.render_with_provider(cr, pango_ctx, w, h, scale_factor, provider);
            return;
        }

        cr.save().unwrap();
        cr.rectangle(1.0, 1.0, w - 2.0, h - 2.0);
        cr.clip();
        cr.translate(0.0, -self.scroll_y);

        let layout = prepared_layout.cloned()
            .unwrap_or_else(|| self.prepare_layout(pango_ctx, w, padding));
        let pscale = pango::SCALE as f64;

        if is_editing {
            if let Some((sel_start, sel_end)) = self.selection_range() {
                if sel_start < sel_end && sel_end <= self.text.len() {
                    // Rebuild attr list with selection highlight on top
                    let attrs = self.build_attr_list();
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
        cr.move_to(padding, padding);
        pangocairo::functions::show_layout(cr, &layout);

        if is_editing && self.selection_range().is_none() {
            let byte_idx = self.cursor_pos.min(self.text.len()) as i32;
            let (strong, _) = layout.cursor_pos(byte_idx);
            let cx = padding + strong.x() as f64 / pscale;
            let cy = padding + strong.y() as f64 / pscale;
            let ch = strong.height() as f64 / pscale;

            cr.set_source_rgb(0.1, 0.1, 0.9);
            cr.set_line_width(1.5);
            cr.move_to(cx, cy + 1.0);
            cr.line_to(cx, cy + ch - 1.0);
            cr.stroke().unwrap();
        }

        cr.restore().unwrap();

        if !is_export
            && !is_editing
            && !self.text.is_empty()
            && h >= 20.0
            && self.next_frame_id.is_none()
        {
            let overflows = if self.prev_frame_id.is_some() {
                self.overflow_hint
            } else {
                self.overflows_frame(pango_ctx, w, h, scale_factor)
            };
            if overflows {
                draw_overflow_indicator(cr, w, h);
            }
        }
    }

    /// Flows text through the intervals supplied by `provider`, line by line.
    fn render_with_provider(
        &self,
        cr: &cairo::Context,
        pango_ctx: &pango::Context,
        w: f64,
        h: f64,
        sf: f64,
        provider: &dyn TextFlowProvider,
    ) {
        let padding = self.padding * sf;
        let pscale  = pango::SCALE as f64;
        let fd      = pango::FontDescription::from_string(&self.font_description);

        // Compute ascent and line height from font metrics.
        // show_layout_line draws with the BASELINE at the current point, so we
        // must offset by `ascent` to align the top of the text with `y`.
        let (ascent, line_h) = {
            let tmp = pango::Layout::new(pango_ctx);
            tmp.set_font_description(Some(&fd));
            tmp.set_text("Hg");
            // line(0) extents: logical.y() is negative (top is above baseline)
            let (ascent_pu, descent_pu) = if let Some(ln) = tmp.line(0) {
                let (_, log) = ln.extents();
                let a = (-log.y())         .max(0);
                let d = (log.height() + log.y()).max(0);
                (a, d)
            } else {
                let (_, ph) = tmp.size();
                (ph * 3 / 4, ph / 4)
            };
            let a = ascent_pu  as f64 / pscale;
            let d = descent_pu as f64 / pscale;
            (a, ((a + d) * self.line_spacing).max(1.0))
        };

        cr.save().unwrap();
        cr.rectangle(1.0, 1.0, w - 2.0, h - 2.0);
        cr.clip();
        cr.set_source_rgb(0.1, 0.1, 0.1);

        let mut text_pos = 0usize;
        let mut y = padding;  // y = top of the current line slot

        while y + line_h <= h + 0.5 && text_pos < self.text.len() {
            let intervals = provider.intervals_for_line(y, line_h);
            let mut consumed_this_row = false;

            for iv in intervals {
                if text_pos >= self.text.len() { break; }

                // Shrink interval by the text-frame padding on both sides.
                let ix = iv.x + padding;
                let iw = iv.width - 2.0 * padding;
                if iw < 4.0 { continue; }

                // Build a layout for the remaining text at this interval's width.
                let layout = pango::Layout::new(pango_ctx);
                layout.set_text(&self.text[text_pos..]);
                layout.set_font_description(Some(&fd));
                let resolution = pangocairo::functions::context_get_resolution(pango_ctx);
                let pt_per_px = if resolution > 0.0 { 72.0 / resolution } else { 1.0 };
                layout.set_width((iw * pt_per_px * pscale) as i32);
                layout.set_wrap(pango::WrapMode::Word);
                layout.set_alignment(match self.alignment {
                    TextAlign::Left   => pango::Alignment::Left,
                    TextAlign::Center => pango::Alignment::Center,
                    TextAlign::Right  => pango::Alignment::Right,
                });
                if self.line_spacing != 1.0 {
                    layout.set_line_spacing(self.line_spacing as f32);
                }
                if !self.attributes.is_empty() {
                    layout.set_attributes(Some(&build_attr_list_slice(
                        &self.attributes, text_pos, self.text.len(),
                    )));
                }

                // Take only the first line that Pango calculated.
                let Some(line0) = layout.line(0) else { continue };
                // Use line(1).start_index() as the byte count — this reliably
                // includes the paragraph separator (\n) that line0.length() may omit.
                let bytes_used = match layout.line(1) {
                    Some(line1) => line1.start_index() as usize,
                    None        => self.text[text_pos..].len(),
                }.min(self.text[text_pos..].len());

                if bytes_used == 0 {
                    // Stuck on a bare paragraph separator — consume it directly.
                    if self.text[text_pos..].starts_with('\n') {
                        text_pos += 1;
                    }
                    continue;
                }

                // show_layout_line renders at the baseline → offset y by ascent.
                cr.move_to(ix, y + ascent);
                pangocairo::functions::show_layout_line(cr, &line0);

                let new_pos = text_pos + bytes_used;
                // Round down to a valid UTF-8 boundary.
                let new_pos = {
                    let mut p = new_pos.min(self.text.len());
                    while p > text_pos && !self.text.is_char_boundary(p) { p -= 1; }
                    p
                };
                if new_pos > text_pos {
                    text_pos = new_pos;
                    consumed_this_row = true;
                }
            }

            // Always advance Y so we never loop forever.
            let _ = consumed_this_row;
            y += line_h;
        }

        cr.restore().unwrap();
    }

    /// Maps a click position (mm) to a byte index in the text.
    pub fn hit_test(
        &self,
        frame_x: f64,
        frame_y: f64,
        click_x_mm: f64,
        click_y_mm: f64,
        scale: f64,
        frame_w_px: f64,
    ) -> usize {
        let ctx = make_screen_pango_ctx(scale);
        let pscale = pango::SCALE as f64;
        let padding = self.padding * scale;
        let layout = self.prepare_layout(&ctx, frame_w_px, padding);

        let rel_x = (click_x_mm - frame_x) * scale - padding;
        let rel_y = (click_y_mm - frame_y) * scale - padding + self.scroll_y;
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

// ── Attribute shift helpers ───────────────────────────────────────────────────

/// Shifts attribute ranges to account for `n` bytes inserted at `pos`.
fn shift_attrs_insert(attrs: &mut Vec<TextAttribute>, pos: usize, n: usize) {
    let pos = pos as u32;
    let n = n as u32;
    for attr in attrs.iter_mut() {
        if attr.start > pos { attr.start += n; }
        if attr.end   > pos { attr.end   += n; }
    }
}

/// Adjusts attribute ranges to account for `n` bytes deleted starting at `pos`.
fn shift_attrs_delete(attrs: &mut Vec<TextAttribute>, pos: usize, n: usize) {
    let pos = pos as u32;
    let n = n as u32;
    let del_end = pos + n;

    attrs.retain_mut(|attr| {
        if attr.end <= pos {
            return true; // entirely before deletion
        }
        if attr.start >= del_end {
            // entirely after: shift left
            attr.start -= n;
            attr.end   -= n;
            return true;
        }
        // overlaps deletion: clamp
        let new_start = attr.start.min(pos);
        let new_end   = if attr.end > del_end { attr.end - n } else { pos };
        if new_end > new_start {
            attr.start = new_start;
            attr.end   = new_end;
            true
        } else {
            false // zero-length after clamp: drop
        }
    });
}

// ── Visual helpers ────────────────────────────────────────────────────────────

fn draw_overflow_indicator(cr: &cairo::Context, w: f64, h: f64) {
    let size = 10.0;
    let margin = 3.0;
    let x = w - margin - size;
    let y = h - margin - size;

    cr.set_source_rgb(0.85, 0.1, 0.1);
    cr.rectangle(x, y, size, size);
    cr.fill().unwrap();

    cr.set_source_rgb(1.0, 1.0, 1.0);
    cr.set_line_width(1.5);
    let arm = size * 0.3;
    let cx = x + size / 2.0;
    let cy = y + size / 2.0;
    cr.move_to(cx - arm, cy);
    cr.line_to(cx + arm, cy);
    cr.stroke().unwrap();
    cr.move_to(cx, cy - arm);
    cr.line_to(cx, cy + arm);
    cr.stroke().unwrap();
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

/// Build a Pango attribute list for the byte slice `[start, end)` of `attrs`,
/// remapping all indices so they are relative to `start`.
pub(crate) fn build_attr_list_slice(
    attrs: &[TextAttribute],
    start: usize,
    end: usize,
) -> pango::AttrList {
    let list = pango::AttrList::new();
    let s = start as u32;
    let e = end as u32;
    for attr in attrs {
        if attr.end <= s || attr.start >= e { continue; }
        let adj_start = attr.start.max(s) - s;
        let adj_end   = attr.end.min(e)   - s;
        if adj_end <= adj_start { continue; }
        let mut pa: pango::Attribute = match &attr.value {
            AttrValue::Family(f) => pango::AttrString::new_family(f).into(),
            AttrValue::Size(pt)  => pango::AttrSize::new((*pt * pango::SCALE as f64) as i32).into(),
            AttrValue::Bold(b)   => pango::AttrInt::new_weight(
                if *b { pango::Weight::Bold } else { pango::Weight::Normal }).into(),
            AttrValue::Italic(b) => pango::AttrInt::new_style(
                if *b { pango::Style::Italic } else { pango::Style::Normal }).into(),
            AttrValue::Underline(b) => pango::AttrInt::new_underline(
                if *b { pango::Underline::Single } else { pango::Underline::None }).into(),
            AttrValue::Color([r, g, b]) => {
                let x = |v: u8| v as u16 * 257;
                pango::AttrColor::new_foreground(x(*r), x(*g), x(*b)).into()
            }
            AttrValue::BgColor([r, g, b]) => {
                let x = |v: u8| v as u16 * 257;
                pango::AttrColor::new_background(x(*r), x(*g), x(*b)).into()
            }
        };
        pa.set_start_index(adj_start);
        pa.set_end_index(adj_end);
        list.insert(pa);
    }
    list
}

#[cfg(test)]
mod tests {
    use super::{AttrValue, TextAlign, TextBox};

    #[test]
    fn apply_format_replaces_overlapping_spans_of_same_kind() {
        let mut tb = TextBox::new("hello world".to_string());
        tb.apply_format(0, 5, AttrValue::Bold(true));
        tb.apply_format(3, 8, AttrValue::Bold(false));

        assert_eq!(tb.attributes.len(), 2);
        assert!(matches!(tb.attributes[0].value, AttrValue::Bold(true)));
        assert_eq!((tb.attributes[0].start, tb.attributes[0].end), (0, 3));
        assert!(matches!(tb.attributes[1].value, AttrValue::Bold(false)));
        assert_eq!((tb.attributes[1].start, tb.attributes[1].end), (3, 8));
    }

    #[test]
    fn clear_format_splits_overlapping_ranges() {
        let mut tb = TextBox::new("hello world".to_string());
        tb.apply_format(0, tb.text.len(), AttrValue::Underline(true));
        tb.clear_format(2, 8);

        assert_eq!(tb.attributes.len(), 2);
        assert_eq!((tb.attributes[0].start, tb.attributes[0].end), (0, 2));
        assert_eq!((tb.attributes[1].start, tb.attributes[1].end), (8, tb.text.len() as u32));
    }

    #[test]
    fn effective_snapshot_falls_back_to_base_font_description() {
        let mut tb = TextBox::new("hello".to_string());
        tb.font_description = "Serif Bold Italic 14".to_string();
        let snap = tb.effective_snapshot_at(0);

        assert_eq!(snap.family.as_deref(), Some("Serif"));
        assert_eq!(snap.size_pt, Some(14.0));
        assert!(snap.bold);
        assert!(snap.italic);
    }

    #[test]
    fn undo_and_redo_restore_text_attributes_and_alignment() {
        let mut tb = TextBox::new("hello".to_string());
        tb.alignment = TextAlign::Left;
        tb.cursor_pos = tb.text.len();
        tb.push_history();
        tb.insert_text(" world");
        tb.apply_format(0, 5, AttrValue::Bold(true));
        tb.alignment = TextAlign::Center;

        assert!(tb.undo());
        assert_eq!(tb.text, "hello");
        assert!(tb.attributes.is_empty());
        assert_eq!(tb.alignment, TextAlign::Left);

        assert!(tb.redo());
        assert_eq!(tb.text, "hello world");
        assert_eq!(tb.alignment, TextAlign::Center);
        assert_eq!(tb.attributes.len(), 1);
    }
}
