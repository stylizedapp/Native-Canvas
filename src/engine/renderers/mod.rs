use super::grid::GridConfig;
use super::scene::{NodeKind, NodeId, Scene};
use super::transform::Camera;
use crate::engine::controller::Preview;
use glam::{Affine2, Vec2};

mod tiny_skia;
mod vello;

pub use tiny_skia::TinySkiaRenderer;
pub use vello::VelloRenderer;

/// Палитра канваса. Эти цвета дублируются в `ui/theme.slint` (`Theme.canvas-bg`
/// и т.д.) — при смене стиля правьте оба места (см. `ui/STYLING.md`).
pub const CANVAS_BG: [u8; 4] = [0x16, 0x16, 0x18, 0xff];
pub const PAGE_BG: [u8; 4] = [0x1a, 0x1a, 0x1d, 0xff];
pub const PAGE_BORDER: [u8; 4] = [0x2c, 0x2c, 0x31, 0xff];
pub const GRID: [u8; 4] = [0x24, 0x24, 0x2a, 0xff];
pub const SELECTION: [u8; 4] = [0x5b, 0x8c, 0xff, 0xff];
pub const PREVIEW_FILL: [u8; 4] = [0x5b, 0x8c, 0xff, 60];
pub const PREVIEW_STROKE: [u8; 4] = [0x6e, 0x9a, 0xff, 0xff];
/// Маркер начала координат (0,0) — для визуальной проверки пана/зума.
pub const ORIGIN: [u8; 4] = [0xff, 0x9f, 0x43, 0xff];

/// Абстракция графического бэкенда. Сейчас — CPU (tiny-skia) и GPU (vello/wgpu).
/// Ядро и UI не знают о конкретном бэкенде.
pub trait Renderer {
    /// Имя активного бэкенда (для debug-оверлея).
    fn name(&self) -> &'static str;

    /// Рендерит сцену в переданный RGBA8-буфер.
    ///
    /// Возвращает `true`, если кадр был принят (отправлен на растеризацию) —
    /// тогда вызывающий может сбросить dirty-флаг. GPU-бэкенд может временно
    /// пропустить кадр, если GPU не успевает (тогда вернётся `false` и состояние
    /// останется грязным для следующего тика).
    fn render(
        &mut self,
        scene: &Scene,
        camera: &Camera,
        width: u32,
        height: u32,
        selected: &[NodeId],
        grid: GridConfig,
        preview: Option<Preview>,
        out: &mut [u8],
    ) -> bool;
}

/// Создаёт бэкенд: пробует GPU (vello), при неудаче — CPU (tiny-skia).
pub fn create_renderer() -> Box<dyn Renderer> {
    match VelloRenderer::new() {
        Ok(r) => Box::new(r),
        Err(e) => {
            eprintln!("[renderer] vello (GPU) unavailable, using tiny-skia (CPU): {e}");
            Box::new(TinySkiaRenderer)
        }
    }
}

/// Мировая ограничивающая рамка по локальной геометрии и трансформации.
pub(crate) fn world_bbox(kind: &NodeKind, world: Affine2) -> (Vec2, Vec2) {
    let (lmin, lmax) = kind.local_bbox();
    let corners = [
        world.transform_point2(lmin),
        world.transform_point2(Vec2::new(lmax.x, lmin.y)),
        world.transform_point2(Vec2::new(lmin.x, lmax.y)),
        world.transform_point2(lmax),
    ];
    let min = corners.iter().copied().reduce(Vec2::min).unwrap_or(lmin);
    let max = corners.iter().copied().reduce(Vec2::max).unwrap_or(lmax);
    (min, max)
}

pub(crate) fn rects_intersect(a0: Vec2, a1: Vec2, b0: Vec2, b1: Vec2) -> bool {
    a0.x <= b1.x && a1.x >= b0.x && a0.y <= b1.y && a1.y >= b0.y
}