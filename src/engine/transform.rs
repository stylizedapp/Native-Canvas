use crate::engine::model::nodes::{NodeKey, NodeKind, ShapeKind};
use crate::engine::model::scene::SceneGraph;
use crate::engine::spatial::paint_rank;
use glam::{Affine2, Vec2};

/// Камера холста: панорамирование и масштабирование.
#[derive(Clone, Copy, Debug)]
pub struct Camera {
    pub pan: Vec2,
    pub zoom: f32,
}

impl Camera {
    pub fn new() -> Self {
        Self { pan: Vec2::ZERO, zoom: 1.0 }
    }

    /// Экранные -> мировые координаты (без учёта визуального центра окна,
    /// предполагаем левый верхний угол холста).
    pub fn screen_to_world(&self, screen: Vec2) -> Vec2 {
        (screen - self.pan) / self.zoom
    }

    pub fn world_to_screen(&self, world: Vec2) -> Vec2 {
        (world * self.zoom) + self.pan
    }

    /// Матрица камеры: screen = world * zoom + pan.
    pub fn to_affine(&self) -> Affine2 {
        Affine2::from_scale_angle_translation(Vec2::splat(self.zoom), 0.0, self.pan)
    }

    pub fn zoom_at(&mut self, factor: f32, screen_pivot: Vec2) {
        let new_zoom = (self.zoom * factor).clamp(0.05, 50.0);
        // Держим мировую точку под курсором неподвижной.
        self.pan = screen_pivot - (screen_pivot - self.pan) * (new_zoom / self.zoom);
        self.zoom = new_zoom;
    }

    pub fn pan_by(&mut self, delta: Vec2) {
        self.pan += delta;
    }

    /// Камера для рендера в буфер меньшего размера (кэп разрешения).
    /// `s` — коэффициент масштаба буфера относительно логического размера области.
    /// Ввод/хит-тест остаются в логических координатах; меняется только растеризация.
    pub fn for_render_scale(&self, s: f32) -> Camera {
        Camera { pan: self.pan * s, zoom: self.zoom * s }
    }
}

impl Default for Camera {
    fn default() -> Self {
        Self::new()
    }
}

/// Поиск узла под точкой в мировых координатах.
/// Spatial hash grid сужает кандидатов до окрестности точки (O(k), а не O(N)),
/// затем — точная проверка по контуру примитива. Возвращается наименьший по
/// площади видимый узел среди попавших (при равенстве — верхний по стеку).
pub fn pick(scene: &mut SceneGraph, world: Vec2) -> Option<NodeKey> {
    // Хит-тест читает кэшированные мировые трансформации.
    scene.flush_transforms();
    let mut ranked: Vec<(Vec<u32>, NodeKey)> = scene
        .spatial_query_point(world)
        .into_iter()
        .filter_map(|k| paint_rank(scene, k).map(|r| (r, k)))
        .collect();
    // Топ-мост первым: больший ранг позже нарисован и лежит выше по стеку.
    ranked.sort_by(|a, b| b.0.cmp(&a.0));
    let mut best: Option<(f32, NodeKey)> = None;
    for (_, key) in ranked {
        let node = match scene.get(key) {
            Some(n) => n,
            None => continue,
        };
        if !node.is_visible {
            continue;
        }
        let Some(bbox) = scene.world_bbox(key) else { continue };
        if point_in_bbox(world, bbox.0, bbox.1) && precise_hit(scene, key, world) {
            // Считаем площадь как приоритет: меньшие объекты поверх.
            let (mn, mx) = bbox;
            let area = (mx.x - mn.x) * (mx.y - mn.y);
            if best.map(|(a, _)| area < a).unwrap_or(true) {
                best = Some((area, key));
            }
        }
    }
    best.map(|(_, key)| key)
}

/// Стек узлов под точкой сверху вниз (для Ctrl+клик deep-select):
/// все видимые узлы, чей контур содержит `world`.
pub fn pick_stack(scene: &mut SceneGraph, world: Vec2) -> Vec<NodeKey> {
    scene.flush_transforms();
    let mut ranked: Vec<(Vec<u32>, NodeKey)> = scene
        .spatial_query_point(world)
        .into_iter()
        .filter_map(|k| paint_rank(scene, k).map(|r| (r, k)))
        .collect();
    ranked.sort_by(|a, b| b.0.cmp(&a.0));
    ranked
        .into_iter()
        .filter(|(_, key)| {
            scene.get(*key).map(|n| n.is_visible).unwrap_or(false)
                && scene
                    .world_bbox(*key)
                    .map(|(mn, mx)| point_in_bbox(world, mn, mx))
                    .unwrap_or(false)
                && precise_hit(scene, *key, world)
        })
        .map(|(_, key)| key)
        .collect()
}

fn point_in_bbox(p: Vec2, min: Vec2, max: Vec2) -> bool {
    p.x >= min.x && p.x <= max.x && p.y >= min.y && p.y <= max.y
}

/// Точная проверка попадания. Для вертикального среза используем AABB самого
/// примитива (уже трансформированного); контурная проверка (ray-casting / Безье)
/// будет добавлена на этапе Vector Networks.
fn precise_hit(scene: &SceneGraph, key: NodeKey, world: Vec2) -> bool {
    let node = match scene.get(key) {
        Some(n) => n,
        None => return false,
    };
    match &node.kind {
        NodeKind::Frame { .. } | NodeKind::Shape(ShapeKind::Rectangle { .. }) => {
            scene.world_bbox(key).map(|(mn, mx)| point_in_bbox(world, mn, mx)).unwrap_or(false)
        }
        NodeKind::Shape(ShapeKind::Ellipse { radii, .. }) => hit_ellipse(scene, key, world, *radii),
        NodeKind::Shape(ShapeKind::Line { start, end }) => hit_line(scene, key, world, *start, *end),
        _ => false,
    }
}

fn hit_ellipse(scene: &SceneGraph, key: NodeKey, world: Vec2, size: Vec2) -> bool {
    if size.x <= 0.0 || size.y <= 0.0 {
        return false;
    }
    let Some(t) = scene.world_transform(key) else { return false };
    // Локальные координаты.
    let local = t.inverse().transform_point2(world);
    let cx = size.x / 2.0;
    let cy = size.y / 2.0;
    let dx = (local.x - cx) / (size.x / 2.0);
    let dy = (local.y - cy) / (size.y / 2.0);
    dx * dx + dy * dy <= 1.0
}

fn hit_line(scene: &SceneGraph, key: NodeKey, world: Vec2, start: Vec2, end: Vec2) -> bool {
    let Some(t) = scene.world_transform(key) else { return false };
    // Переводим точку в локальные координаты линии (она учитывает transform).
    let local = t.inverse().transform_point2(world);
    // Проверка на дистанцию от точки до отрезка.
    let ab = end - start;
    let ab2 = ab.length_squared();
    let t_param = if ab2 == 0.0 { 0.0 } else { ((local - start).dot(ab) / ab2).clamp(0.0, 1.0) };
    let closest = start + ab * t_param;
    let tolerance = scene
        .get(key)
        .and_then(|n| n.strokes.first())
        .map(|s| (s.width / 2.0) + 4.0)
        .unwrap_or(4.0);
    (local - closest).length() <= tolerance
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::model::nodes::ShapeKind;

    fn rect(_x: f32, _y: f32, w: f32, h: f32) -> NodeKind {
        NodeKind::Shape(ShapeKind::Rectangle {
            size: Vec2::new(w, h),
            corner_radii: [0.0; 4],
        })
    }

    fn place(scene: &mut SceneGraph, parent: Option<NodeKey>, name: &str, kind: NodeKind, x: f32, y: f32) -> NodeKey {
        let k = match parent {
            Some(p) => scene.insert_child(p, name, kind).unwrap(),
            None => scene.insert_root(name, kind),
        };
        if let Some(n) = scene.get_mut(k) {
            n.local_transform = Affine2::from_translation(Vec2::new(x, y));
        }
        scene.mark_subtree_dirty(k);
        k
    }

    #[test]
    fn pick_prefers_smaller_overlapping_node() {
        let mut s = SceneGraph::new();
        // Большой фон (0,0)-(100,100) и маленький поверх (30,30)-(50,50).
        let big = place(&mut s, None, "big", rect(0.0, 0.0, 100.0, 100.0), 0.0, 0.0);
        let small = place(&mut s, None, "small", rect(0.0, 0.0, 20.0, 20.0), 30.0, 30.0);
        s.flush_transforms();
        // Точка внутри малого: должен выиграть малый (меньше площадь).
        assert_eq!(pick(&mut s, Vec2::new(40.0, 40.0)), Some(small));
        // Точка только в большом.
        assert_eq!(pick(&mut s, Vec2::new(5.0, 5.0)), Some(big));
        // Пустое место.
        assert_eq!(pick(&mut s, Vec2::new(200.0, 200.0)), None);
    }

    #[test]
    fn pick_stack_returns_topmost_first() {
        let mut s = SceneGraph::new();
        let big = place(&mut s, None, "big", rect(0.0, 0.0, 100.0, 100.0), 0.0, 0.0);
        let small = place(&mut s, None, "small", rect(0.0, 0.0, 20.0, 20.0), 30.0, 30.0);
        s.flush_transforms();
        let stack = pick_stack(&mut s, Vec2::new(40.0, 40.0));
        // Топ-мост: малый (нарисован последним) идёт первым.
        assert_eq!(stack, vec![small, big]);
    }

    #[test]
    fn pick_works_inside_reparented_tree() {
        let mut s = SceneGraph::new();
        let frame = place(&mut s, None, "F", rect(0.0, 0.0, 200.0, 200.0), 50.0, 50.0);
        let child = place(&mut s, Some(frame), "C", rect(0.0, 0.0, 40.0, 40.0), 20.0, 20.0);
        s.flush_transforms();
        // Мировая точка внутри ребёнка (50+20=70, 70).
        assert_eq!(pick(&mut s, Vec2::new(75.0, 75.0)), Some(child));
        assert_eq!(pick(&mut s, Vec2::new(120.0, 120.0)), Some(frame));
    }

    /// Эталонная (прежняя) реализация: полный обход в порядке отрисовки.
    fn reference_pick(scene: &SceneGraph, world: Vec2) -> Option<NodeKey> {
        let mut best: Option<(f32, NodeKey)> = None;
        for key in scene.walk().into_iter().rev() {
            let node = scene.get(key)?;
            if !node.is_visible {
                continue;
            }
            let Some((mn, mx)) = scene.world_bbox(key) else { continue };
            if point_in_bbox(world, mn, mx) && precise_hit(scene, key, world) {
                let area = (mx.x - mn.x) * (mx.y - mn.y);
                if best.map(|(a, _)| area < a).unwrap_or(true) {
                    best = Some((area, key));
                }
            }
        }
        best.map(|(_, key)| key)
    }

    fn reference_pick_stack(scene: &SceneGraph, world: Vec2) -> Vec<NodeKey> {
        let mut hits = Vec::new();
        for key in scene.walk().into_iter().rev() {
            let Some(node) = scene.get(key) else { continue };
            if !node.is_visible {
                continue;
            }
            let Some((mn, mx)) = scene.world_bbox(key) else { continue };
            if point_in_bbox(world, mn, mx) && precise_hit(scene, key, world) {
                hits.push(key);
            }
        }
        hits
    }

    #[test]
    fn spatial_pick_matches_reference_on_complex_scene() {
        let mut s = SceneGraph::new();
        // Глубокое дерево с поворотами, мультивложением и скрытыми узлами.
        let a = place(&mut s, None, "A", rect(0.0, 0.0, 300.0, 300.0), 20.0, 20.0);
        let b = place(&mut s, Some(a), "B", rect(0.0, 0.0, 150.0, 150.0), 10.0, 10.0);
        let c = place(&mut s, Some(b), "C", rect(0.0, 0.0, 60.0, 60.0), 40.0, 40.0);
        let _d = place(&mut s, Some(c), "D", rect(0.0, 0.0, 15.0, 15.0), 5.0, 5.0);
        // Повёрнутый узел.
        let e = place(&mut s, Some(a), "E", rect(0.0, 0.0, 80.0, 20.0), 100.0, 5.0);
        if let Some(n) = s.get_mut(e) {
            let rot = Affine2::from_angle(0.7);
            let t = Affine2::from_translation(Vec2::new(100.0, 5.0));
            n.local_transform = t * rot;
        }
        s.mark_subtree_dirty(e);
        // Скрытый узел поверх видимого — не должен побеждать.
        let hidden = place(&mut s, None, "H", rect(0.0, 0.0, 10.0, 10.0), 25.0, 25.0);
        if let Some(n) = s.get_mut(hidden) {
            n.is_visible = false;
        }
        s.mark_subtree_dirty(hidden);
        // Сторонний узел далеко.
        let _far = place(&mut s, None, "FAR", rect(0.0, 0.0, 50.0, 50.0), 5000.0, 5000.0);
        s.flush_transforms();

        // Мелкая сетка точек по всей сцене + точки далеко вне сцены.
        let mut points = Vec::new();
        for x in (-10..320).step_by(7) {
            for y in (-10..320).step_by(7) {
                points.push(Vec2::new(x as f32, y as f32));
            }
        }
        points.push(Vec2::new(100000.0, 100000.0));
        points.push(Vec2::new(-500.0, -500.0));

        for p in points {
            assert_eq!(pick(&mut s, p), reference_pick(&s, p), "pick mismatch at {p:?}");
            assert_eq!(pick_stack(&mut s, p), reference_pick_stack(&s, p), "stack mismatch at {p:?}");
        }
    }
}