use crate::document::Document;
use crate::document::ItemContent;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use zip::write::SimpleFileOptions;
use serde_json;

pub struct PersistenceManager;

impl PersistenceManager {
    /// Saves the document and its assets into a .rsp (zip) file.
    pub fn save_project(document: &Document, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
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
    pub fn load_project(path: &Path, temp_dir: &Path) -> Result<(Document, Vec<(String, PathBuf)>), Box<dyn std::error::Error>> {
        let file = File::open(path)?;
        let mut archive = zip::ZipArchive::new(file)?;
        
        // 1. Read document.json
        let mut document: Document = {
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

#[cfg(test)]
mod tests {
    use std::fs;

    use crate::document::{Document, Item, ItemContent, Page};
    use crate::image_box::ImageBox;
    use crate::svg_box::SvgBox;

    use super::PersistenceManager;

    fn temp_path(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("rscribus_test_{}_{}", label, uuid::Uuid::new_v4()))
    }

    #[test]
    fn saves_and_loads_document_with_packaged_assets() {
        let work_dir = temp_path("workspace");
        let extract_dir = temp_path("extract");
        fs::create_dir_all(&work_dir).unwrap();
        fs::create_dir_all(&extract_dir).unwrap();

        let image_path = work_dir.join("sample-image.bin");
        let svg_path = work_dir.join("sample.svg");
        fs::write(&image_path, b"image-bytes").unwrap();
        fs::write(&svg_path, "<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>").unwrap();

        let document = Document {
            title: "Test document".to_string(),
            width: 210.0,
            height: 297.0,
            pages: vec![Page {
                items: vec![
                    Item {
                        id: "image-1".to_string(),
                        x: 10.0,
                        y: 10.0,
                        width: 30.0,
                        height: 20.0,
                        rotation: 0.0,
                        show_border: true,
                        content: ItemContent::Image(ImageBox {
                            image_path: Some(image_path.to_string_lossy().to_string()),
                            ..Default::default()
                        }),
                    },
                    Item {
                        id: "svg-1".to_string(),
                        x: 40.0,
                        y: 20.0,
                        width: 25.0,
                        height: 25.0,
                        rotation: 0.0,
                        show_border: true,
                        content: ItemContent::Svg(SvgBox {
                            svg_path: svg_path.to_string_lossy().to_string(),
                            ..Default::default()
                        }),
                    },
                ],
            }],
        };

        let project_path = work_dir.join("project.rsp");
        PersistenceManager::save_project(&document, &project_path).unwrap();

        let (loaded, extracted) = PersistenceManager::load_project(&project_path, &extract_dir).unwrap();

        assert_eq!(loaded.title, document.title);
        assert_eq!(loaded.pages.len(), 1);
        assert_eq!(extracted.len(), 2);

        let loaded_image = match &loaded.pages[0].items[0].content {
            ItemContent::Image(image) => image.image_path.as_ref().unwrap(),
            _ => panic!("expected image item"),
        };
        let loaded_svg = match &loaded.pages[0].items[1].content {
            ItemContent::Svg(svg) => &svg.svg_path,
            _ => panic!("expected svg item"),
        };

        assert!(loaded_image.starts_with(extract_dir.to_string_lossy().as_ref()));
        assert!(loaded_svg.starts_with(extract_dir.to_string_lossy().as_ref()));
        assert!(fs::metadata(loaded_image).is_ok());
        assert!(fs::metadata(loaded_svg).is_ok());

        let _ = fs::remove_dir_all(&work_dir);
        let _ = fs::remove_dir_all(&extract_dir);
    }

    #[test]
    fn document_json_roundtrip_preserves_basic_fields() {
        let document = Document::default();
        let json = serde_json::to_string(&document).unwrap();
        let decoded: Document = serde_json::from_str(&json).unwrap();

        assert_eq!(decoded.title, document.title);
        assert_eq!(decoded.width, document.width);
        assert_eq!(decoded.height, document.height);
        assert_eq!(decoded.pages.len(), document.pages.len());
    }
}
