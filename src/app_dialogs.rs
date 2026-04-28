use relm4::{adw, gtk};

pub fn show_info_dialog(root: &adw::ApplicationWindow, message: &str, detail: &str) {
    show_alert_dialog(root, message, detail);
}

pub fn show_error_dialog(root: &adw::ApplicationWindow, message: &str, detail: &str) {
    show_alert_dialog(root, message, detail);
}

fn show_alert_dialog(root: &adw::ApplicationWindow, message: &str, detail: &str) {
    let dialog = gtk::AlertDialog::builder()
        .modal(true)
        .message(message)
        .detail(detail)
        .buttons(["OK"])
        .cancel_button(0)
        .default_button(0)
        .build();
    dialog.show(Some(root));
}
