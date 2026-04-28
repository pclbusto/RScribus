use serde::{Deserialize, Serialize};
use crate::text_box::TextBox;
use crate::image_box::ImageBox;
use crate::svg_box::SvgBox;

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
    #[serde(default)]
    pub rotation: f64,
    #[serde(default = "default_show_border")]
    pub show_border: bool,
    pub content: ItemContent,
}

fn default_show_border() -> bool { true }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ItemContent {
    Text(TextBox),
    Image(ImageBox),
    Svg(SvgBox),
    Shape,
}

impl ItemContent {
    pub fn item_type(&self) -> ItemType {
        match self {
            ItemContent::Text(_) => ItemType::TextFrame,
            ItemContent::Image(_) => ItemType::ImageFrame,
            ItemContent::Svg(_) => ItemType::SvgFrame,
            ItemContent::Shape => ItemType::Shape,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum ItemType {
    TextFrame,
    ImageFrame,
    SvgFrame,
    Shape,
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
                            show_border: true,
                            content: ItemContent::Text(TextBox::new("Sample text for the first frame.".to_string())),
                        }
                    ],
                }
            ],
            width: 210.0,
            height: 297.0,
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
