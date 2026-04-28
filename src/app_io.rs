use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;

use relm4::gtk::prelude::FileExt;
use relm4::prelude::ComponentSender;
use relm4::{adw, gtk};

use crate::app::{export_to_pdf, AppInput, AppModel};
use crate::app_dialogs::{show_error_dialog, show_info_dialog};
use crate::document::Document;
use crate::persistence::PersistenceManager;

pub fn show_preferences_dialog(root: &adw::ApplicationWindow) {
    show_info_dialog(
        root,
        "Preferences",
        "RScribus Preferences\n\n(This is a placeholder for actual preferences settings)",
    );
}

pub fn save_project_dialog(
    root: &adw::ApplicationWindow,
    sender: ComponentSender<AppModel>,
    document: Document,
    last_save_path: Option<&str>,
) {
    let dialog = gtk::FileDialog::new();
    let filter = project_filter();
    let filters = gtk::gio::ListStore::new::<gtk::FileFilter>();
    filters.append(&filter);
    dialog.set_filters(Some(&filters));
    dialog.set_default_filter(Some(&filter));

    if let Some(path) = last_save_path {
        let file = gtk::gio::File::for_path(path);
        dialog.set_initial_file(Some(&file));
    }

    let win = root.clone();
    gtk::glib::MainContext::default().spawn_local(async move {
        if let Ok(file) = dialog.save_future(Some(&win)).await {
            if let Some(path) = file.path() {
                let path_str = ensure_extension(path, "rsp");
                let save_path = Path::new(&path_str);
                match PersistenceManager::save_project(&document, save_path) {
                    Ok(_) => sender.input(AppInput::ProjectSaved(path_str)),
                    Err(err) => show_error_dialog(
                        &win,
                        "Error Saving Project",
                        &format!("Could not save project file:\n{}", err),
                    ),
                }
            }
        }
    });
}

pub fn open_project_dialog(root: &adw::ApplicationWindow, sender: ComponentSender<AppModel>) {
    let dialog = gtk::FileDialog::new();
    let filter = project_filter();
    let filters = gtk::gio::ListStore::new::<gtk::FileFilter>();
    filters.append(&filter);
    dialog.set_filters(Some(&filters));
    dialog.set_default_filter(Some(&filter));

    let win = root.clone();
    gtk::glib::MainContext::default().spawn_local(async move {
        if let Ok(file) = dialog.open_future(Some(&win)).await {
            if let Some(path) = file.path() {
                let project_name = path
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| "project".to_string());
                let temp_dir = std::env::temp_dir().join(format!("rscribus_{}", project_name));

                match PersistenceManager::load_project(&path, &temp_dir) {
                    Ok((doc, _)) => sender
                        .input(AppInput::ProjectLoaded(doc, path.to_string_lossy().to_string())),
                    Err(err) => show_error_dialog(
                        &win,
                        "Error Loading Project",
                        &format!("Could not open project file:\n{}", err),
                    ),
                }
            }
        }
    });
}

pub fn export_pdf_dialog(
    root: &adw::ApplicationWindow,
    document: Document,
    images: HashMap<String, Rc<cairo::ImageSurface>>,
    svgs: HashMap<String, Rc<rsvg::SvgHandle>>,
) {
    let dialog = gtk::FileDialog::new();
    let filter = gtk::FileFilter::new();
    filter.add_pattern("*.pdf");
    filter.set_name(Some("PDF Document"));
    let filters = gtk::gio::ListStore::new::<gtk::FileFilter>();
    filters.append(&filter);
    dialog.set_filters(Some(&filters));
    dialog.set_default_filter(Some(&filter));

    let win = root.clone();
    gtk::glib::MainContext::default().spawn_local(async move {
        if let Ok(file) = dialog.save_future(Some(&win)).await {
            if let Some(path) = file.path() {
                let path_str = ensure_extension(path, "pdf");
                match export_to_pdf(&document, &images, &svgs, &path_str) {
                    Ok(_) => show_info_dialog(
                        &win,
                        "Export Successful",
                        "The document has been exported to PDF.",
                    ),
                    Err(err) => show_error_dialog(
                        &win,
                        "Export Failed",
                        &format!("Could not export PDF:\n{}", err),
                    ),
                }
            }
        }
    });
}

fn project_filter() -> gtk::FileFilter {
    let filter = gtk::FileFilter::new();
    filter.add_pattern("*.rsp");
    filter.set_name(Some("RScribus Project"));
    filter
}

fn ensure_extension(path: std::path::PathBuf, extension: &str) -> String {
    let mut path_str = path.to_string_lossy().to_string();
    if !path_str.ends_with(&format!(".{}", extension)) {
        path_str.push('.');
        path_str.push_str(extension);
    }
    path_str
}
