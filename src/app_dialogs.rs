use relm4::{adw, gtk};
use relm4::gtk::prelude::*;

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

pub fn show_keyboard_shortcuts_window(root: &adw::ApplicationWindow) {
    let general = gtk::ShortcutsGroup::builder()
        .title("General")
        .build();
    general.add_shortcut(&shortcut("New project", "<Control>N"));
    general.add_shortcut(&shortcut("Show keyboard shortcuts", "<Control>F1"));
    general.add_shortcut(&shortcut("Open project", "<Control>O"));
    general.add_shortcut(&shortcut("Save project", "<Control>S"));
    general.add_shortcut(&shortcut("Export PDF", "<Control>P"));
    general.add_shortcut(&shortcut("Toggle properties sidebar", "<Control>L"));
    general.add_shortcut(&shortcut("Add page", "<Control>plus"));

    let creation = gtk::ShortcutsGroup::builder()
        .title("Frame Creation")
        .build();
    creation.add_shortcut(&shortcut("Select text frame tool", "<Control>T"));
    creation.add_shortcut(&shortcut("Select image frame tool", "<Control>I"));
    creation.add_shortcut(&shortcut("Select SVG frame tool", "<Control>G"));

    let editing = gtk::ShortcutsGroup::builder()
        .title("Text Editing")
        .build();
    editing.add_shortcut(&shortcut("Bold", "<Control>B"));
    editing.add_shortcut(&shortcut("Italic", "<Control>I"));
    editing.add_shortcut(&shortcut("Underline", "<Control>U"));
    editing.add_shortcut(&shortcut("Undo", "<Control>Z"));
    editing.add_shortcut(&shortcut("Redo", "<Control><Shift>Z"));
    editing.add_shortcut(&shortcut("Exit text editing", "Escape"));

    let section = gtk::ShortcutsSection::builder()
        .title("RScribus Shortcuts")
        .section_name("shortcuts")
        .max_height(18)
        .build();
    section.add_group(&general);
    section.add_group(&creation);
    section.add_group(&editing);

    let window = gtk::ShortcutsWindow::builder()
        .transient_for(root)
        .modal(true)
        .hide_on_close(true)
        .default_width(720)
        .default_height(540)
        .title("Keyboard Shortcuts")
        .build();
    window.add_section(&section);
    window.present();
}

fn shortcut(title: &str, accelerator: &str) -> gtk::ShortcutsShortcut {
    gtk::ShortcutsShortcut::builder()
        .title(title)
        .accelerator(accelerator)
        .build()
}
