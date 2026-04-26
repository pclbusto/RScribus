use gtk4::gdk;
use gtk4::pango;
use gtk4::prelude::DisplayExt;
use serde::{Deserialize, Serialize};

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
    MoveVertical { up: bool, extend: bool },
    FormatBold,
    FormatItalic,
    FormatUnderline,
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
    #[serde(skip)]
    pub cursor_pos: usize,
    #[serde(skip)]
    pub selection_anchor: Option<usize>,
    #[serde(skip)]
    pub scroll_y: f64,
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
            cursor_pos: 0,
            selection_anchor: None,
            scroll_y: 0.0,
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

    pub fn set_line_spacing(&mut self, factor: f64) { self.line_spacing = factor; }
    pub fn get_line_spacing(&self) -> f64 { self.line_spacing }

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

    /// Generates an HTML fragment representing the text with inline formatting tags.
    pub fn to_html(&self) -> String {
        if self.text.is_empty() { return String::new(); }
        let mut boundaries: std::collections::BTreeSet<usize> = std::collections::BTreeSet::new();
        boundaries.insert(0);
        boundaries.insert(self.text.len());
        for attr in &self.attributes {
            let s = (attr.start as usize).min(self.text.len());
            let e = (attr.end as usize).min(self.text.len());
            if self.text.is_char_boundary(s) { boundaries.insert(s); }
            if self.text.is_char_boundary(e) { boundaries.insert(e); }
        }
        let boundaries: Vec<usize> = boundaries.into_iter().collect();
        let mut html = String::new();
        for w in boundaries.windows(2) {
            let (seg_s, seg_e) = (w[0], w[1]);
            if seg_s >= self.text.len() { break; }
            let snap = self.get_attr_at(seg_s);
            if snap.bold      { html.push_str("<b>"); }
            if snap.italic    { html.push_str("<i>"); }
            if snap.underline { html.push_str("<u>"); }
            let has_span = snap.size_pt.is_some() || snap.color.is_some() || snap.family.is_some();
            if has_span {
                html.push_str("<span style=\"");
                if let Some(f) = &snap.family { html.push_str(&format!("font-family:{};", f)); }
                if let Some(pt) = snap.size_pt { html.push_str(&format!("font-size:{:.1}pt;", pt)); }
                if let Some([r, g, b]) = snap.color { html.push_str(&format!("color:#{:02x}{:02x}{:02x};", r, g, b)); }
                html.push_str("\">");
            }
            for ch in self.text[seg_s..seg_e].chars() {
                match ch {
                    '<'  => html.push_str("&lt;"),
                    '>'  => html.push_str("&gt;"),
                    '&'  => html.push_str("&amp;"),
                    '\n' => html.push_str("<br>"),
                    c    => html.push(c),
                }
            }
            if has_span       { html.push_str("</span>"); }
            if snap.underline { html.push_str("</u>"); }
            if snap.italic    { html.push_str("</i>"); }
            if snap.bold      { html.push_str("</b>"); }
        }
        html
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

    pub fn cut_selection(&mut self) {
        self.copy_selection();
        self.delete_selection();
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
        use pango::prelude::FontMapExt;
        let font_map = pangocairo::FontMap::default();
        let ctx = font_map.create_context();
        pangocairo::functions::context_set_resolution(&ctx, 25.4 * scale);
        let padding = self.padding * scale;
        let layout = self.prepare_layout(&ctx, frame_w_px, padding);
        let pscale = pango::SCALE as f64;

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
                gdk::Key::x | gdk::Key::X => { self.cut_selection(); KeyAction::Handled }
                gdk::Key::v | gdk::Key::V => KeyAction::RequestPaste,
                gdk::Key::b | gdk::Key::B => KeyAction::FormatBold,
                gdk::Key::i | gdk::Key::I => KeyAction::FormatItalic,
                gdk::Key::u | gdk::Key::U => KeyAction::FormatUnderline,
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

// ── Layout & rendering ────────────────────────────────────────────────────────

impl TextBox {
    /// Builds a Pango layout with font, width, wrapping, text attributes and alignment.
    pub fn prepare_layout(&self, pango_ctx: &pango::Context, frame_w: f64, padding: f64) -> pango::Layout {
        let pscale = pango::SCALE as f64;
        let layout = pango::Layout::new(pango_ctx);
        layout.set_text(&self.text);
        let font_desc = pango::FontDescription::from_string(&self.font_description);
        layout.set_font_description(Some(&font_desc));
        layout.set_width(((frame_w - 2.0 * padding) * pscale) as i32);
        layout.set_wrap(pango::WrapMode::Word);
        if !self.attributes.is_empty() {
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
        let padding = self.padding * scale_factor;
        let layout = self.prepare_layout(pango_ctx, w, padding);
        let (_, ph) = layout.size();
        ph as f64 / pango::SCALE as f64 + 2.0 * padding
    }

    /// Returns how many bytes of `self.text` fit within the frame dimensions.
    /// Used during chain reflow to split text across linked frames.
    pub fn text_capacity(&self, w_px: f64, h_px: f64, scale: f64) -> usize {
        use pango::prelude::FontMapExt;
        if self.text.is_empty() {
            return 0;
        }
        let font_map = pangocairo::FontMap::default();
        let ctx = font_map.create_context();
        pangocairo::functions::context_set_resolution(&ctx, 25.4 * scale);
        let padding = self.padding * scale;
        let layout = self.prepare_layout(&ctx, w_px, padding);
        let pscale = pango::SCALE as f64;
        let available_h_pango = ((h_px - 2.0 * padding) * pscale).max(0.0) as i32;

        // Fast path: all text fits
        let (_, total_h) = layout.size();
        if total_h <= available_h_pango {
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

        last_fit_end
    }

    /// Renders the text box into `cr` (translated to item origin).
    pub fn render(
        &self,
        cr: &cairo::Context,
        pango_ctx: &pango::Context,
        w: f64,
        h: f64,
        is_selected: bool,
        is_editing: bool,
        scale_factor: f64,
    ) {
        let padding = self.padding * scale_factor;

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

        cr.save().unwrap();
        cr.rectangle(1.0, 1.0, w - 2.0, h - 2.0);
        cr.clip();
        cr.translate(0.0, -self.scroll_y);

        let layout = self.prepare_layout(pango_ctx, w, padding);
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

        if !is_editing && !self.text.is_empty() && h >= 20.0 {
            if self.required_height(pango_ctx, w, scale_factor) > h {
                draw_overflow_indicator(cr, w, h);
            }
        }
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
        use pango::prelude::FontMapExt;
        let font_map = pangocairo::FontMap::default();
        let ctx = font_map.create_context();
        pangocairo::functions::context_set_resolution(&ctx, 25.4 * scale);
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
