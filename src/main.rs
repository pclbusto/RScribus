mod text_box;
mod text_flow;
mod image_box;
mod svg_box;
mod document;
mod persistence;
mod app_dialogs;
mod app_io;
mod app;

use relm4::RelmApp;
use app::AppModel;

fn main() {
    let app = RelmApp::new("org.rscribus.RScribus");
    app.run::<AppModel>(());
}
