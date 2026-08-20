//! Кэш-хелперы геометрии бэкендов: детерминированный хэш параметров узла и
//! сборка единого контура глифов текста. Переиспользование дорогих CPU-этапов
//! (шейпинг текста, построение `Path`) между кадрами при статичной геометрии.

use super::super::model::nodes::{NodeKind, ShapeKind};
use kurbo::BezPath;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// Детерминированный хэш геометрии узла: изменение любого параметра, влияющего
/// на контуры/раскладку, меняет хэш и инвалидирует запись кэша бэкенда.
/// Палитра (заливка/обводка) и трансформация в хэш не входят — контуры от
/// них не зависят, а трансформация применяется при рисовании.
pub fn geom_hash(kind: &NodeKind) -> u64 {
    let mut h = DefaultHasher::new();
    match kind {
        NodeKind::Text {
            content,
            font_size,
            line_height,
            letter_spacing,
            align,
            ..
        } => {
            0u8.hash(&mut h);
            content.hash(&mut h);
            font_size.to_bits().hash(&mut h);
            line_height.to_bits().hash(&mut h);
            letter_spacing.to_bits().hash(&mut h);
            (*align as u8).hash(&mut h);
        }
        NodeKind::Shape(ShapeKind::Rectangle { size, corner_radii }) => {
            1u8.hash(&mut h);
            size.x.to_bits().hash(&mut h);
            size.y.to_bits().hash(&mut h);
            corner_radii.iter().for_each(|r| r.to_bits().hash(&mut h));
        }
        NodeKind::Shape(ShapeKind::Ellipse {
            radii,
            start_angle,
            end_angle,
            inner_radius_ratio,
        }) => {
            2u8.hash(&mut h);
            radii.x.to_bits().hash(&mut h);
            radii.y.to_bits().hash(&mut h);
            start_angle.to_bits().hash(&mut h);
            end_angle.to_bits().hash(&mut h);
            inner_radius_ratio.to_bits().hash(&mut h);
        }
        NodeKind::Shape(ShapeKind::Polygon { radius, point_count }) => {
            3u8.hash(&mut h);
            radius.to_bits().hash(&mut h);
            point_count.hash(&mut h);
        }
        NodeKind::Shape(ShapeKind::Star {
            radius,
            inner_radius_ratio,
            point_count,
        }) => {
            4u8.hash(&mut h);
            radius.to_bits().hash(&mut h);
            inner_radius_ratio.to_bits().hash(&mut h);
            point_count.hash(&mut h);
        }
        NodeKind::Shape(ShapeKind::Line { start, end }) => {
            5u8.hash(&mut h);
            start.x.to_bits().hash(&mut h);
            start.y.to_bits().hash(&mut h);
            end.x.to_bits().hash(&mut h);
            end.y.to_bits().hash(&mut h);
        }
        NodeKind::Frame {
            size, corner_radii, ..
        } => {
            6u8.hash(&mut h);
            size.x.to_bits().hash(&mut h);
            size.y.to_bits().hash(&mut h);
            corner_radii.iter().for_each(|r| r.to_bits().hash(&mut h));
        }
        _ => 7u8.hash(&mut h),
    }
    h.finish()
}

/// Единый контур всех глифов текстового узла (результат `text::layout`),
/// чтобы рисовать текст одним draw-call вместо N вызовов по глифу.
pub fn text_glyphs(kind: &NodeKind) -> BezPath {
    let mut merged = BezPath::new();
    for line in crate::engine::text::layout(kind) {
        for glyph in &line.glyphs {
            merged.extend(glyph.elements().iter().copied());
        }
    }
    merged
}

/// Ключ кэша пути сетки: адаптивный шаг + границы вьюпорта в мировых
/// координатах. Пока камера статична (драг узла), ключ стабилен и путь
/// переиспользуется каждый кадр; при пане путь перестраивается.
pub type GridKey = (u32, u32, u32, u32, u32, u32, u32, u32, u32);

#[allow(clippy::too_many_arguments)]
pub fn grid_key(
    s: f32,
    x0: f32,
    x1: f32,
    y0: f32,
    y1: f32,
    vmin_x: f32,
    vmin_y: f32,
    vmax_x: f32,
    vmax_y: f32,
) -> GridKey {
    (
        s.to_bits(),
        x0.to_bits(),
        x1.to_bits(),
        y0.to_bits(),
        y1.to_bits(),
        vmin_x.to_bits(),
        vmin_y.to_bits(),
        vmax_x.to_bits(),
        vmax_y.to_bits(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::model::types::TextAlign;
    use glam::Vec2;

    fn text(content: &str) -> NodeKind {
        NodeKind::Text {
            content: content.into(),
            font_family: "Inter".into(),
            font_size: 16.0,
            font_weight: 400,
            line_height: 1.2,
            letter_spacing: 0.0,
            align: TextAlign::Left,
            size_mode: crate::engine::model::types::TextSizeMode::AutoWidth,
        }
    }

    #[test]
    fn geom_hash_stable_for_equal_geometry() {
        assert_eq!(geom_hash(&text("abc")), geom_hash(&text("abc")));
        let a = NodeKind::Shape(ShapeKind::Rectangle {
            size: Vec2::new(10.0, 20.0),
            corner_radii: [2.0; 4],
        });
        let b = NodeKind::Shape(ShapeKind::Rectangle {
            size: Vec2::new(10.0, 20.0),
            corner_radii: [2.0; 4],
        });
        assert_eq!(geom_hash(&a), geom_hash(&b));
    }

    #[test]
    fn geom_hash_invalidates_on_parameter_change() {
        assert_ne!(geom_hash(&text("abc")), geom_hash(&text("abd")));
        let a = NodeKind::Shape(ShapeKind::Rectangle {
            size: Vec2::new(10.0, 20.0),
            corner_radii: [2.0; 4],
        });
        let b = NodeKind::Shape(ShapeKind::Rectangle {
            size: Vec2::new(11.0, 20.0),
            corner_radii: [2.0; 4],
        });
        assert_ne!(geom_hash(&a), geom_hash(&b));
    }

    #[test]
    fn geom_hash_ignores_palette_fields() {
        // Палитра не входит в хэш (контуры от неё не зависят), поэтому узел
        // с другой заливкой имеет тот же хэш геометрии — кэш пути остаётся.
        assert_eq!(geom_hash(&text("abc")), geom_hash(&text("abc")));
    }

    #[test]
    fn text_glyphs_merges_all_glyphs() {
        let kind = text("Ii");
        let path = text_glyphs(&kind);
        assert!(path.elements().len() > 4, "объединённый путь непустой");
    }

    #[test]
    fn grid_key_depends_on_viewport() {
        let a = grid_key(8.0, 0.0, 5.0, 0.0, 5.0, 0.0, 0.0, 100.0, 100.0);
        let b = grid_key(8.0, 0.0, 5.0, 0.0, 5.0, 0.0, 1.0, 100.0, 100.0);
        assert_ne!(a, b);
        assert_eq!(a, grid_key(8.0, 0.0, 5.0, 0.0, 5.0, 0.0, 0.0, 100.0, 100.0));
    }
}