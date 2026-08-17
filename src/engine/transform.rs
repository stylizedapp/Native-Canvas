use crate::engine::model::nodes::{NodeKey, NodeKind, ShapeKind};
use crate::engine::model::scene::SceneGraph;
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
/// Сначала — быстрый отбор по AABB (снизу вверх по порядку отрисовки),
/// затем — точная проверка по контуру примитива.
pub fn pick(scene: &mut SceneGraph, world: Vec2) -> Option<NodeKey> {
    // Хит-тест читает кэшированные мировые трансформации.
    scene.flush_transforms();
    let mut best: Option<(f32, NodeKey)> = None;
    for key in scene.walk().into_iter().rev() {
        let node = scene.get(key)?;
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