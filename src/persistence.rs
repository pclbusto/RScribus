use serde::{Deserialize, Serialize};
use crate::document::ItemContent;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use zip::write::SimpleFileOptions;
use serde_json;

pub struct PersistenceManager;

impl PersistenceManager {
    /// Saves the document and its assets into a .rsp (zip) file.
    pub fn save_project(document: &crate::document::Document, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
        let file = File::create(path)?;
        let mut zip = zip::ZipWriter::new(file);
        let options = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);

        // 1. Prepare a modified document with relative paths for assets
        let mut doc_to_save = document.clone();
        for page in &mut doc_to_save.pages {
            for item in &mut page.items {
                match item.content {
                    ItemContent::Image(ref mut ib) => {
                        if let Some(ref original_path) = ib.image_path {
                            let original_path = Path::new(original_path);
                            if let Some(file_name) = original_path.file_name() {
                                let internal_path = format!("images/{}", file_name.to_string_lossy());
                                if let Ok(mut img_file) = File::open(original_path) {
                                    let mut buffer = Vec::new();
                                    if img_file.read_to_end(&mut buffer).is_ok() {
                                        let _ = zip.start_file(&internal_path, options);
                                        let _ = zip.write_all(&buffer);
                                        ib.image_path = Some(internal_path);
                                    }
                                }
                            }
                        }
                    }
                    ItemContent::Svg(ref mut sb) => {
                        if !sb.svg_path.is_empty() {
                            let original_path = Path::new(&sb.svg_path);
                            if let Some(file_name) = original_path.file_name() {
                                let internal_path = format!("svgs/{}", file_name.to_string_lossy());
                                if let Ok(mut svg_file) = File::open(original_path) {
                                    let mut buffer = Vec::new();
                                    if svg_file.read_to_end(&mut buffer).is_ok() {
                                        let _ = zip.start_file(&internal_path, options);
                                        let _ = zip.write_all(&buffer);
                                        sb.svg_path = internal_path;
                                    }
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        // 3. Serialize document to JSON
        let json = serde_json::to_string_pretty(&doc_to_save)?;
        zip.start_file("document.json", options)?;
        zip.write_all(json.as_bytes())?;

        zip.finish()?;
        Ok(())
    }

    /// Loads a project from a .rsp file.
    /// Returns the Document and a list of extracted image paths (temp locations).
    pub fn load_project(path: &Path, temp_dir: &Path) -> Result<(crate::document::Document, Vec<(String, PathBuf)>), Box<dyn std::error::Error>> {
        let file = File::open(path)?;
        let mut archive = zip::ZipArchive::new(file)?;
        
        // 1. Read document.json
        let mut document: crate::document::Document = {
            let mut doc_file = archive.by_name("document.json")?;
            let mut json_contents = String::new();
            doc_file.read_to_string(&mut json_contents)?;
            serde_json::from_str(&json_contents)?
        };

        let mut extracted_assets = Vec::new();

        // 2. Extract assets to temp directory and update paths
        for i in 0..archive.len() {
            let mut file = archive.by_index(i)?;
            if (file.name().starts_with("images/") || file.name().starts_with("svgs/")) && !file.is_dir() {
                let outpath = temp_dir.join(file.name());
                if let Some(p) = outpath.parent() {
                    std::fs::create_dir_all(p)?;
                }
                let mut outfile = File::create(&outpath)?;
                std::io::copy(&mut file, &mut outfile)?;
                
                extracted_assets.push((file.name().to_string(), outpath.clone()));
            }
        }

        // 3. Update document paths to point to temp files
        for page in &mut document.pages {
            for item in &mut page.items {
                match item.content {
                    ItemContent::Image(ref mut ib) => {
                        if let Some(ref internal_path) = ib.image_path {
                            if let Some((_, temp_path)) = extracted_assets.iter().find(|(p, _)| p == internal_path) {
                                ib.image_path = Some(temp_path.to_string_lossy().to_string());
                            }
                        }
                    }
                    ItemContent::Svg(ref mut sb) => {
                        if !sb.svg_path.is_empty() {
                            if let Some((_, temp_path)) = extracted_assets.iter().find(|(p, _)| p == &sb.svg_path) {
                                sb.svg_path = temp_path.to_string_lossy().to_string();
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        Ok((document, extracted_assets))
    }
}
