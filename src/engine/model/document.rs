//! Документ: страницы, глобальные именованные стили и общий граф сцены.

use crate::engine::model::nodes::{NodeKey, PageKey};
use crate::engine::model::scene::SceneGraph;
use crate::engine::model::types::{Color, Effect, Paint};
use serde::{Deserialize, Serialize};
use slotmap::SlotMap;
use std::collections::HashMap;

/// Именованный текстовый стиль (шрифтовые атрибуты).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextStyle {
    pub font_family: String,
    pub font_size: f32,
    pub font_weight: u16,
    pub line_height: f32,
    pub letter_spacing: f32,
}

impl TextStyle {
    pub fn new(font_family: &str, font_size: f32) -> Self {
        Self {
            font_family: font_family.to_string(),
            font_size,
            font_weight: 400,
            line_height: font_size * 1.2,
            letter_spacing: 0.0,
        }
    }
}

/// Реестр глобальных именованных стилей (аналог Style Library в Figma).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GlobalStyles {
    pub paints: HashMap<String, Paint>,
    pub text: HashMap<String, TextStyle>,
    pub effects: HashMap<String, Vec<Effect>>,
}

impl GlobalStyles {
    pub fn add_paint(&mut self, name: &str, paint: Paint) {
        self.paints.insert(name.to_string(), paint);
    }

    pub fn get_paint(&self, name: &str) -> Option<&Paint> {
        self.paints.get(name)
    }

    pub fn add_text(&mut self, name: &str, style: TextStyle) {
        self.text.insert(name.to_string(), style);
    }

    pub fn add_effect(&mut self, name: &str, effects: Vec<Effect>) {
        self.effects.insert(name.to_string(), effects);
    }
}

/// Страница документа: именованный список корневых узлов (обычно Frame'ы).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Page {
    pub name: String,
    /// Корневые узлы страницы в графе `Document::scene`.
    pub top_level: Vec<NodeKey>,
    pub background_color: Color,
}

impl Page {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            top_level: Vec::new(),
            background_color: Color::rgb(1.0, 1.0, 1.0),
        }
    }

    pub fn add_node(&mut self, key: NodeKey) {
        self.top_level.push(key);
    }
}

/// Документ: страницы + общий граф сцены + глобальные стили.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Document {
    pub pages: SlotMap<PageKey, Page>,
    pub active_page: PageKey,
    pub scene: SceneGraph,
    pub styles: GlobalStyles,
}

impl Document {
    pub fn new() -> Self {
        let mut pages = SlotMap::with_key();
        let active_page = pages.insert(Page::new("Page 1"));
        Self { pages, active_page, scene: SceneGraph::new(), styles: GlobalStyles::default() }
    }

    // --- Страницы ---

    pub fn active_page(&self) -> Option<&Page> {
        self.pages.get(self.active_page)
    }

    pub fn active_page_mut(&mut self) -> Option<&mut Page> {
        self.pages.get_mut(self.active_page)
    }

    pub fn page(&self, key: PageKey) -> Option<&Page> {
        self.pages.get(key)
    }

    pub fn page_mut(&mut self, key: PageKey) -> Option<&mut Page> {
        self.pages.get_mut(key)
    }

    pub fn pages(&self) -> impl Iterator<Item = (PageKey, &Page)> {
        self.pages.iter()
    }

    /// Создаёт страницу и делает её активной.
    pub fn new_page(&mut self, name: &str) -> PageKey {
        let key = self.pages.insert(Page::new(name));
        self.active_page = key;
        key
    }

    /// Удаляет страницу. Активную удалить нельзя.
    pub fn remove_page(&mut self, key: PageKey) {
        if key != self.active_page {
            self.pages.remove(key);
        }
    }

    pub fn set_active_page(&mut self, key: PageKey) {
        if self.pages.contains_key(key) {
            self.active_page = key;
        }
    }
}

impl Default for Document {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::model::nodes::{NodeKind, ShapeKind};
    use glam::Vec2;

    fn rect_kind(w: f32, h: f32) -> NodeKind {
        NodeKind::Shape(ShapeKind::Rectangle { size: Vec2::new(w, h), corner_radii: [0.0; 4] })
    }

    #[test]
    fn new_has_default_page() {
        let doc = Document::new();
        assert_eq!(doc.pages.len(), 1);
        let page = doc.active_page().unwrap();
        assert_eq!(page.name, "Page 1");
        assert!(page.top_level.is_empty());
    }

    #[test]
    fn new_page_becomes_active() {
        let mut doc = Document::new();
        let k = doc.new_page("Artboard");
        assert_eq!(doc.active_page, k);
        assert_eq!(doc.active_page().unwrap().name, "Artboard");
    }

    #[test]
    fn cannot_remove_active_page() {
        let mut doc = Document::new();
        let first = doc.active_page;
        doc.remove_page(first);
        assert_eq!(doc.pages.len(), 1);
    }

    #[test]
    fn set_active_only_existing() {
        let mut doc = Document::new();
        let existing = doc.active_page;
        doc.set_active_page(existing);
        assert_eq!(doc.active_page, existing);
    }

    #[test]
    fn page_owns_root_nodes() {
        let mut doc = Document::new();
        let frame = doc.scene.insert_root("Frame", rect_kind(100.0, 100.0));
        doc.active_page_mut().unwrap().add_node(frame);
        assert_eq!(doc.active_page().unwrap().top_level, vec![frame]);
        assert_eq!(doc.scene.len(), 1);
    }

    #[test]
    fn global_styles_insert_and_get() {
        let mut styles = GlobalStyles::default();
        styles.add_paint("Primary", Paint::solid(Color::from_rgba8(0x5b, 0x8c, 0xff, 0xff)));
        styles.add_text("Body", TextStyle::new("Inter", 14.0));
        styles.add_effect("Shadow", vec![Effect::drop_shadow(Vec2::new(0.0, 2.0), 4.0, Color::BLACK)]);
        assert_eq!(
            styles.get_paint("Primary"),
            Some(&Paint::solid(Color::from_rgba8(0x5b, 0x8c, 0xff, 0xff)))
        );
        assert_eq!(styles.text["Body"].font_size, 14.0);
        assert_eq!(styles.effects["Shadow"].len(), 1);
    }

    #[test]
    fn serialize_document() {
        let mut doc = Document::new();
        let frame = doc.scene.insert_root("Frame", rect_kind(100.0, 100.0));
        doc.active_page_mut().unwrap().add_node(frame);
        doc.styles.add_paint("Bg", Paint::solid(Color::WHITE));
        let json = serde_json::to_string(&doc).unwrap();
        let back: Document = serde_json::from_str(&json).unwrap();
        assert_eq!(back.pages.len(), 1);
        assert_eq!(back.scene.len(), 1);
        assert!(back.styles.paints.contains_key("Bg"));
        let frame2 = *back.active_page().unwrap().top_level.first().unwrap();
        assert_eq!(back.scene.get(frame2).unwrap().name, "Frame");
    }
}