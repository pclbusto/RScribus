use serde::{Deserialize, Serialize};
use crate::text_box::TextBox;
use crate::image_box::ImageBox;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Document {
    pub title: String,
    pub pages: Vec<Page>,
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Page {
    pub items: Vec<Item>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Item {
    pub id: String,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub rotation: f64,
    pub item_type: ItemType,
    pub text_box: TextBox,
    pub image_box: ImageBox,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ItemType {
    TextFrame,
    ImageFrame,
    Shape,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Layer {
    pub name: String,
    pub visible: bool,
    pub locked: bool,
}

impl Default for Document {
    fn default() -> Self {
        Self {
            title: String::from("New Document"),
            pages: vec![
                Page {
                    items: vec![
                        Item {
                            id: String::from("1"),
                            x: 20.0,
                            y: 20.0,
                            width: 170.0,
                            height: 50.0,
                            rotation: 0.0,
                            item_type: ItemType::TextFrame,
                            text_box: TextBox::new("Sample text for the first frame.".to_string()),
                            image_box: ImageBox::default(),
                        }
                    ],
                }
            ],
            width: 210.0,  // A4 width in mm
            height: 297.0, // A4 height in mm
        }
    }
}

impl Default for Page {
    fn default() -> Self {
        Self {
            items: Vec::new(),
        }
    }
}

impl Default for Layer {
    fn default() -> Self {
        Self {
            name: String::from("Layer 1"),
            visible: true,
            locked: false,
        }
    }
}
