//! Ключи арен (`slotmap`) и разновидности нод (`NodeKind`, `ShapeKind`).
//!
//! `NodeKey`/`PageKey`/`ComponentKey` — лёгкие копируемые ключи арен SlotMap.
//! Ключи сериализуются как `{ idx, version }` (см. slotmap::new_key_type! с фичей `serde`).

use crate::engine::model::types::{
    AutoLayoutConfig, Color, Constraints, ExportPreset, FillRule, PropertyValue, TextAlign,
    TextSizeMode,
};
use glam::Vec2;
use kurbo::Shape;
use serde::{Deserialize, Serialize};
use slotmap::new_key_type;
use std::collections::HashMap;
use std::sync::Arc;

new_key_type! {
    /// Ключ узла графа сцены.
    pub struct NodeKey;
    /// Ключ страницы документа.
    pub struct PageKey;
    /// Ключ мастер-компонента.
    pub struct ComponentKey;
}

/// Геометрический примитив (собственная геометрия в локальных координатах).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ShapeKind {
    Rectangle {
        size: Vec2,
        /// Радиусы скругления углов [top-left, top-right, bottom-right, bottom-left].
        corner_radii: [f32; 4],
    },
    Ellipse {
        radii: Vec2,
        /// Начало дуги в радианах (0 = ось +X).
        start_angle: f32,
        end_angle: f32,
        /// Для кольца/сектора: отношение внутреннего радиуса к внешнему (0..1).
        inner_radius_ratio: f32,
    },
    Polygon {
        radius: f32,
        point_count: u32,
    },
    Star {
        radius: f32,
        inner_radius_ratio: f32,
        point_count: u32,
    },
    Line {
        start: Vec2,
        end: Vec2,
    },
}

/// Операция булевой группы.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BooleanOpKind {
    Union,
    Subtract,
    Intersect,
    Exclude,
}

/// Режим масштабирования растрового изображения.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImageScaleMode {
    Fill,
    Fit,
    Crop,
    Tile,
}

/// Разновидность узла сцены (аналог нод Figma).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum NodeKind {
    Section {
        size: Vec2,
        background_color: Color,
    },
    Frame {
        size: Vec2,
        clip_content: bool,
        corner_radii: [f32; 4],
        auto_layout: Option<AutoLayoutConfig>,
        constraints: Constraints,
    },
    Component {
        master_id: ComponentKey,
        overrides: HashMap<String, PropertyValue>,
    },
    Group,
    Shape(ShapeKind),
    VectorPath {
        path: kurbo::BezPath,
        fill_rule: FillRule,
    },
    BooleanGroup {
        op: BooleanOpKind,
    },
    Text {
        content: String,
        font_family: String,
        font_size: f32,
        font_weight: u16,
        line_height: f32,
        letter_spacing: f32,
        align: TextAlign,
        size_mode: TextSizeMode,
    },
    Image {
        data: Arc<[u8]>,
        size: Vec2,
        scale_mode: ImageScaleMode,
        crop_rect: Option<kurbo::Rect>,
    },
    Slice {
        size: Vec2,
        presets: Vec<ExportPreset>,
    },
}

impl NodeKind {
    /// Локальная ограничивающая рамка (до трансформации).
    ///
    /// Для `Text` — грубая оценка по длине строки и кеглю; точный лимит будет
    /// вычисляться шейпером шрифта на этапе рендера.
    pub fn local_bbox(&self) -> (Vec2, Vec2) {
        match self {
            NodeKind::Section { size, .. }
            | NodeKind::Frame { size, .. }
            | NodeKind::Image { size, .. }
            | NodeKind::Slice { size, .. }
            | NodeKind::Shape(ShapeKind::Rectangle { size, .. })
            | NodeKind::Shape(ShapeKind::Ellipse { radii: size, .. }) => (Vec2::ZERO, *size),
            NodeKind::Shape(ShapeKind::Line { start, end }) => (
                Vec2::new(start.x.min(end.x), start.y.min(end.y)),
                Vec2::new(start.x.max(end.x), start.y.max(end.y)),
            ),
            NodeKind::Shape(ShapeKind::Polygon { radius, .. })
            | NodeKind::Shape(ShapeKind::Star { radius, .. }) => {
                (Vec2::new(-radius, -radius), Vec2::new(*radius, *radius))
            }
            NodeKind::VectorPath { path, .. } => {
                let r = path.bounding_box();
                (
                    Vec2::new(r.min_x() as f32, r.min_y() as f32),
                    Vec2::new(r.max_x() as f32, r.max_y() as f32),
                )
            }
            NodeKind::Text { .. } => {
                let size = crate::engine::text::measure(self);
                (Vec2::ZERO, size)
            }
            NodeKind::Group | NodeKind::Component { .. } | NodeKind::BooleanGroup { .. } => {
                (Vec2::ZERO, Vec2::ZERO)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_copiable_and_distinct() {
        let mut map = slotmap::SlotMap::<NodeKey, i32>::with_key();
        let a = map.insert(1);
        let b = map.insert(2);
        assert_ne!(a, b);
        let copy = a;
        assert_eq!(copy, a);
        assert_eq!(map[a], 1);
    }

    #[test]
    fn rect_bbox() {
        let kind = NodeKind::Shape(ShapeKind::Rectangle {
            size: Vec2::new(10.0, 20.0),
            corner_radii: [0.0; 4],
        });
        assert_eq!(kind.local_bbox(), (Vec2::ZERO, Vec2::new(10.0, 20.0)));
    }

    #[test]
    fn line_bbox_negative() {
        let kind = NodeKind::Shape(ShapeKind::Line {
            start: Vec2::new(5.0, 5.0),
            end: Vec2::new(-5.0, -3.0),
        });
        assert_eq!(kind.local_bbox(), (Vec2::new(-5.0, -3.0), Vec2::new(5.0, 5.0)));
    }

    #[test]
    fn star_bbox() {
        let kind = NodeKind::Shape(ShapeKind::Star {
            radius: 50.0,
            inner_radius_ratio: 0.5,
            point_count: 5,
        });
        assert_eq!(kind.local_bbox(), (Vec2::splat(-50.0), Vec2::splat(50.0)));
    }

    #[test]
    fn vector_path_bbox() {
        let mut path = kurbo::BezPath::new();
        path.move_to((10.0, 20.0));
        path.line_to((30.0, 80.0));
        path.close_path();
        let kind = NodeKind::VectorPath { path, fill_rule: FillRule::NonZero };
        assert_eq!(kind.local_bbox(), (Vec2::new(10.0, 20.0), Vec2::new(30.0, 80.0)));
    }

    #[test]
    fn node_key_serialize_roundtrip() {
        let key = NodeKey::default();
        let json = serde_json::to_string(&key).unwrap();
        let back: NodeKey = serde_json::from_str(&json).unwrap();
        assert_eq!(back, key);
    }
}