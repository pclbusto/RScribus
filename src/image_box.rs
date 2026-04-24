use gtk4::gdk;
use gtk4::pango;
use gtk4::prelude::{TextureExt, TextureExtManual};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub enum FitMode {
    #[default]
    ImageToFrame,
    FrameToImage,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ImageBox {
    pub image_path: Option<String>,
    pub fit_mode: FitMode,
}

impl ImageBox {
    pub fn fit_mode(&self) -> FitMode {
        self.fit_mode
    }
}

impl ImageBox {
    /// Loads a cairo ImageSurface from a file path.
    pub fn load_surface(path: &str) -> Option<cairo::ImageSurface> {
        let texture = gdk::Texture::from_filename(path).ok()?;
        let w = texture.width();
        let h = texture.height();
        let stride = w as usize * 4;
        let mut data = vec![0u8; stride * h as usize];
        texture.download(&mut data, stride);
        // GDK_MEMORY_DEFAULT is B8G8R8A8_PREMULTIPLIED, matches Cairo ARGB32 on little-endian.
        cairo::ImageSurface::create_for_data(data, cairo::Format::ARgb32, w, h, stride as i32).ok()
    }

    /// Renders the image box into cr. The Cairo context must be translated to the item's origin.
    pub fn render(
        &self,
        cr: &cairo::Context,
        w: f64,
        h: f64,
        is_selected: bool,
        image: Option<&cairo::ImageSurface>,
    ) {
        cr.set_source_rgb(0.88, 0.88, 0.88);
        cr.rectangle(0.0, 0.0, w, h);
        cr.fill().unwrap();

        if let Some(surf) = image {
            cr.save().unwrap();
            cr.rectangle(0.0, 0.0, w, h);
            cr.clip();

            let img_w = surf.width() as f64;
            let img_h = surf.height() as f64;
            cr.scale(w / img_w, h / img_h);
            cr.set_source_surface(surf, 0.0, 0.0).unwrap();
            cr.paint().unwrap();
            cr.restore().unwrap();
        } else {
            self.draw_placeholder(cr, w, h);
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

    fn draw_placeholder(&self, cr: &cairo::Context, w: f64, h: f64) {
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

        let layout = pangocairo::functions::create_layout(cr);
        layout.set_text("Image");
        layout.set_font_description(Some(&pango::FontDescription::from_string("Sans 10")));
        let (pw, ph) = layout.pixel_size();
        if pw as f64 <= w && ph as f64 <= h {
            cr.set_source_rgb(0.45, 0.45, 0.45);
            cr.move_to((w - pw as f64) / 2.0, (h - ph as f64) / 2.0);
            pangocairo::functions::show_layout(cr, &layout);
        }
    }
}
