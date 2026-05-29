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
    #[serde(default)]
    pub master_pages: Vec<MasterPage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Page {
    pub items: Vec<Item>,
    #[serde(default)]
    pub master_page: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MasterPage {
    pub id: String,
    pub name: String,
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

impl Item {
    pub fn empty_clone(&self) -> Self {
        Item {
            id: String::new(),
            x: self.x,
            y: self.y,
            width: self.width,
            height: self.height,
            rotation: self.rotation,
            show_border: self.show_border,
            content: self.content.empty_clone(),
        }
    }
}

impl ItemContent {
    pub fn empty_clone(&self) -> Self {
        match self {
            ItemContent::Text(tb) => {
                let mut empty = TextBox::default();
                empty.font_description = tb.font_description.clone();
                empty.padding = tb.padding;
                empty.line_spacing = tb.line_spacing;
                empty.alignment = tb.alignment;
                ItemContent::Text(empty)
            }
            ItemContent::Image(_) => ItemContent::Image(ImageBox::default()),
            ItemContent::Svg(_) => ItemContent::Svg(SvgBox::default()),
            ItemContent::Shape => ItemContent::Shape,
        }
    }
}

impl MasterPage {
    pub fn from_page(name: String, page: &Page) -> Self {
        let items: Vec<Item> = page.items.iter()
            .map(|item| {
                let mut c = item.clone();
                c.id = uuid::Uuid::new_v4().to_string();
                c
            })
            .collect();
        MasterPage {
            id: uuid::Uuid::new_v4().to_string(),
            name,
            items,
        }
    }
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
                    master_page: None,
                }
            ],
            width: 210.0,
            height: 297.0,
            master_pages: Vec::new(),
        }
    }
}

impl Default for Page {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            master_page: None,
        }
    }
}
