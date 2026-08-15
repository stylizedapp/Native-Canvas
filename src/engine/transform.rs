use crate::engine::scene::{Scene, SceneNode, NodeKind, NodeId};
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
pub fn pick(scene: &Scene, world: Vec2) -> Option<NodeId> {
    let mut best: Option<(f32, NodeId)> = None;
    for id in scene.walk().into_iter().rev() {
        let node = scene.get(id)?;
        if !node.visible {
            continue;
        }
        let Some(bbox) = scene.world_bbox(id) else { continue };
        if point_in_bbox(world, bbox.0, bbox.1) && precise_hit(scene, id, world) {
            // Считаем площадь как приоритет: меньшие объекты поверх.
            let (mn, mx) = bbox;
            let area = (mx.x - mn.x) * (mx.y - mn.y);
            if best.map(|(a, _)| area < a).unwrap_or(true) {
                best = Some((area, id));
            }
        }
    }
    best.map(|(_, id)| id)
}

fn point_in_bbox(p: Vec2, min: Vec2, max: Vec2) -> bool {
    p.x >= min.x && p.x <= max.x && p.y >= min.y && p.y <= max.y
}

/// Точная проверка попадания. Для вертикального среза используем AABB самого
/// примитива (уже трансформированного); контурная проверка (ray-casting / Безье)
/// будет добавлена на этапе Vector Networks.
fn precise_hit(scene: &Scene, id: NodeId, world: Vec2) -> bool {
    let node = scene.get(id);
    match node {
        Some(n) => match n.kind {
            NodeKind::Rectangle { .. } | NodeKind::Frame { .. } => {
                if let Some((mn, mx)) = scene.world_bbox(id) {
                    point_in_bbox(world, mn, mx)
                } else {
                    false
                }
            }
            NodeKind::Ellipse { .. } => {
                // Эллипс: проверка по эллиптическому уравнению в локальных координатах.
                hit_ellipse(scene, id, world)
            }
            NodeKind::Line { x2, y2 } => hit_line(n, world, x2, y2),
            NodeKind::Group => false,
            NodeKind::Vector => false,
        },
        None => false,
    }
}

fn hit_ellipse(scene: &Scene, id: NodeId, world: Vec2) -> bool {
    let Some(node) = scene.get(id) else { return false };
    let NodeKind::Ellipse { w, h } = node.kind else { return false };
    if w <= 0.0 || h <= 0.0 {
        return false;
    }
    // Локальные координаты.
    let t = scene.world_transform(id).inverse();
    let local = t.transform_point2(world);
    let cx = w / 2.0;
    let cy = h / 2.0;
    let dx = (local.x - cx) / (w / 2.0);
    let dy = (local.y - cy) / (h / 2.0);
    dx * dx + dy * dy <= 1.0
}

fn hit_line(node: &SceneNode, world: Vec2, x2: f32, y2: f32) -> bool {
    let a = Vec2::ZERO;
    let b = Vec2::new(x2, y2);
    // Проверка на дистанцию от точки до отрезка.
    let ab = b - a;
    let ab2 = ab.length_squared();
    let t = if ab2 == 0.0 { 0.0 } else { ((world - a).dot(ab) / ab2).clamp(0.0, 1.0) };
    let closest = a + ab * t;
    let tolerance = node.stroke.map(|s| (s.width / 2.0) + 4.0).unwrap_or(4.0);
    (world - closest).length() <= tolerance
}