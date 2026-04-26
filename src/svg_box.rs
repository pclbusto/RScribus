use gtk4::pango;
use serde::{Deserialize, Serialize};

// 1 CSS pixel at 96 DPI expressed in millimetres
const PX_TO_MM: f64 = 25.4 / 96.0;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub enum FitMode {
    /// Scale to fit entirely inside the frame, preserving aspect ratio (letterbox).
    #[default]
    Proportional,
    /// Draw at the SVG's natural 1:1 size from the top-left; clip if larger than frame.
    Original,
    /// Stretch to fill the frame exactly, ignoring aspect ratio.
    Stretch,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SvgBox {
    pub svg_path: String,
    pub fit_mode: FitMode,
}

impl SvgBox {
    pub fn new(svg_path: String) -> Self {
        Self { svg_path, fit_mode: FitMode::default() }
    }

    pub fn path(&self) -> &str {
        &self.svg_path
    }

    pub fn fit_mode(&self) -> FitMode {
        self.fit_mode
    }

    pub fn set_fit_mode(&mut self, mode: FitMode) {
        self.fit_mode = mode;
    }

    /// Loads the SVG at `path` once to read its intrinsic dimensions.
    /// Returns (width_mm, height_mm). Falls back to 100×100 mm on any error.
    pub fn intrinsic_size(path: &str) -> (f64, f64) {
        let handle = match rsvg::Loader::new().read_path(path) {
            Ok(h) => h,
            Err(_) => return (100.0, 100.0),
        };
        let renderer = rsvg::CairoRenderer::new(&handle);
        renderer
            .intrinsic_size_in_pixels()
            .map(|(w, h)| (w * PX_TO_MM, h * PX_TO_MM))
            .unwrap_or((100.0, 100.0))
    }

    /// Returns true when (x, y) is inside the bounding rectangle [0, w] × [0, h].
    pub fn contains_point(&self, x: f64, y: f64, w: f64, h: f64) -> bool {
        x >= 0.0 && y >= 0.0 && x <= w && y <= h
    }

    /// Renders the SVG box into `cr`.
    /// The context must already be translated to the item's origin.
    /// Pass a pre-loaded `rsvg::SvgHandle`; `None` shows the placeholder.
    pub fn render(
        &self,
        cr: &cairo::Context,
        w: f64,
        h: f64,
        is_selected: bool,
        handle: Option<&rsvg::SvgHandle>,
    ) {
        cr.set_source_rgb(0.92, 0.92, 0.92);
        cr.rectangle(0.0, 0.0, w, h);
        cr.fill().unwrap();

        match handle {
            Some(svg) => self.draw_svg(cr, w, h, svg),
            None => self.draw_placeholder(cr, w, h),
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
    }

    fn draw_svg(&self, cr: &cairo::Context, w: f64, h: f64, handle: &rsvg::SvgHandle) {
        let renderer = rsvg::CairoRenderer::new(handle);

        cr.save().unwrap();
        cr.rectangle(0.0, 0.0, w, h);
        cr.clip();

        let rect = match self.fit_mode {
            FitMode::Stretch => cairo::Rectangle::new(0.0, 0.0, w, h),
            FitMode::Proportional => renderer
                .intrinsic_size_in_pixels()
                .map(|(sw, sh)| {
                    let scale = (w / sw).min(h / sh);
                    let dw = sw * scale;
                    let dh = sh * scale;
                    cairo::Rectangle::new((w - dw) / 2.0, (h - dh) / 2.0, dw, dh)
                })
                .unwrap_or_else(|| cairo::Rectangle::new(0.0, 0.0, w, h)),
            FitMode::Original => renderer
                .intrinsic_size_in_pixels()
                .map(|(sw, sh)| cairo::Rectangle::new(0.0, 0.0, sw, sh))
                .unwrap_or_else(|| cairo::Rectangle::new(0.0, 0.0, w, h)),
        };

        renderer.render_document(cr, &rect).ok();
        cr.restore().unwrap();
    }

    fn draw_placeholder(&self, cr: &cairo::Context, w: f64, h: f64) {
        cr.save().unwrap();
        cr.rectangle(0.0, 0.0, w, h);
        cr.clip();

        cr.set_source_rgb(0.75, 0.75, 0.75);
        cr.set_line_width(1.0);
        let step = 12.0_f64;
        let diag = w + h;
        let mut offset = -h;
        while offset < diag {
            cr.move_to(offset, 0.0);
            cr.line_to(offset + h, h);
            offset += step;
        }
        cr.stroke().unwrap();

        cr.restore().unwrap();

        let layout = pangocairo::functions::create_layout(cr);
        layout.set_text("SVG");
        layout.set_font_description(Some(&pango::FontDescription::from_string("Sans 10")));
        let (pw, ph) = layout.pixel_size();
        if pw as f64 <= w && ph as f64 <= h {
            cr.set_source_rgb(0.45, 0.45, 0.45);
            cr.move_to((w - pw as f64) / 2.0, (h - ph as f64) / 2.0);
            pangocairo::functions::show_layout(cr, &layout);
        }
    }
}
