mod document;
mod app;

use relm4::RelmApp;
use app::AppModel;

fn main() {
    let app = RelmApp::new("org.rscribus.RScribus");
    app.run::<AppModel>(());
}
