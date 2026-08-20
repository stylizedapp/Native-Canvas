use super::grid::GridConfig;
use super::model::nodes::{NodeKey, NodeKind};
use super::model::scene::SceneGraph;
use super::profiler::FrameMetrics;
use super::transform::Camera;
use crate::engine::controller::Preview;
use glam::Vec2;
use ::vello::wgpu;

mod geom;
mod tiny_skia;
mod vello;

pub use tiny_skia::TinySkiaRenderer;
pub use vello::VelloRenderer;

/// Размер страницы/холста (границы области рисования), в мировых координатах.
/// f32-точности достаточно: на 20000 точность ~0.002px.
pub const PAGE_SIZE: Vec2 = Vec2::new(20000.0, 20000.0);

/// Палитра канваса. Эти цвета дублируются в `ui/theme.slint` (`Theme.canvas-bg`
/// и т.д.) — при смене стиля правьте оба места (см. `ui/STYLING.md`).
pub const CANVAS_BG: [u8; 4] = [0x16, 0x16, 0x18, 0xff];
pub const PAGE_BG: [u8; 4] = [0x1a, 0x1a, 0x1d, 0xff];
pub const PAGE_BORDER: [u8; 4] = [0x2c, 0x2c, 0x31, 0xff];
pub const GRID: [u8; 4] = [0x24, 0x24, 0x2a, 0xff];
pub const SELECTION: [u8; 4] = [0x5b, 0x8c, 0xff, 0xff];
pub const PREVIEW_FILL: [u8; 4] = [0x5b, 0x8c, 0xff, 60];
pub const PREVIEW_STROKE: [u8; 4] = [0x6e, 0x9a, 0xff, 0xff];
/// Gizmo: заливка хэндла и его обводка.
pub const GIZMO_HANDLE_FILL: [u8; 4] = [0xff, 0xff, 0xff, 0xff];
pub const GIZMO_HANDLE_STROKE: [u8; 4] = [0x5b, 0x8c, 0xff, 0xff];
/// Marquee: заливка и рамка.
pub const MARQUEE_FILL: [u8; 4] = [0x5b, 0x8c, 0xff, 26];
pub const MARQUEE_STROKE: [u8; 4] = [0x6e, 0x9a, 0xff, 0xff];
/// Подсветка фрейма-цели при перетаскивании (drop-таргет).
pub const DROP_FILL: [u8; 4] = [0x5b, 0x8c, 0xff, 40];
pub const DROP_STROKE: [u8; 4] = [0x5b, 0x8c, 0xff, 0xff];
/// Маркер начала координат (0,0) — для визуальной проверки пана/зума.
pub const ORIGIN: [u8; 4] = [0xff, 0x9f, 0x43, 0xff];

/// Результат кадра рендера: был ли принят GPU + постадийные метрики.
pub struct RenderOutcome {
    /// `true` — кадр принят (отправлен на растеризацию), можно сбросить dirty.
    /// `false` — GPU пропустил кадр (не успел): состояние остаётся грязным.
    pub submitted: bool,
    /// Метрики стадий (заполняются бэкендом; `total_us` — вызывающим).
    pub metrics: FrameMetrics,
}

/// Абстракция графического бэкенда. Сейчас — CPU (tiny-skia) и GPU (vello/wgpu).
/// Ядро и UI не знают о конкретном бэкенде.
pub trait Renderer {
    /// Имя активного бэкенда (для debug-оверлея).
    fn name(&self) -> &'static str;

    /// Рендерит сцену в переданный RGBA8-буфер.
    ///
    /// Сцена передаётся только для чтения: мировые трансформации уже
    /// пересчитаны вызывающим (`SceneGraph::flush_transforms`).
    ///
    /// Возвращает [`RenderOutcome`]: `submitted` — принят ли кадр бэкендом,
    /// `metrics` — постадийные замеры для профилировщика.
    fn render(
        &mut self,
        scene: &SceneGraph,
        camera: &Camera,
        width: u32,
        height: u32,
        selected: &[NodeKey],
        grid: GridConfig,
        preview: Option<Preview>,
        marquee: Option<(Vec2, Vec2)>,
        hovered: Option<NodeKey>,
        out: &mut [u8],
    ) -> RenderOutcome;
}

/// Создаёт бэкенд с auto-detect:
/// - `NATIVE_CANVAS_BACKEND=cpu|gpu` — явный выбор;
/// - иначе на интегрированном GPU используем tiny-skia (vello-буферы ~150MB
///   лежали бы в системной RAM, UMA), на дискретном — vello (фолбэк на CPU).
pub fn create_renderer() -> Box<dyn Renderer> {
    // Явный переключатель.
    match std::env::var("NATIVE_CANVAS_BACKEND").as_deref() {
        Ok("cpu") => {
            eprintln!("[renderer] tiny-skia (CPU): forced by NATIVE_CANVAS_BACKEND=cpu");
            return Box::new(TinySkiaRenderer::new());
        }
        Ok("gpu") => {
            return match VelloRenderer::new() {
                Ok(r) => {
                    eprintln!("[renderer] vello (GPU): forced by NATIVE_CANVAS_BACKEND=gpu");
                    Box::new(r)
                }
                Err(e) => {
                    eprintln!("[renderer] vello unavailable, using tiny-skia (CPU): {e}");
                    Box::new(TinySkiaRenderer::new())
                }
            };
        }
        _ => {}
    }

    // Авто-определение по типу адаптера.
    match gpu_adapter_info() {
        Ok(Some(info)) if info.device_type == wgpu::DeviceType::IntegratedGpu => {
            eprintln!(
                "[renderer] tiny-skia (CPU): integrated GPU ({}) — vello-буферы жили бы в RAM",
                info.name
            );
            Box::new(TinySkiaRenderer::new())
        }
        Ok(Some(info)) if info.device_type == wgpu::DeviceType::DiscreteGpu => {
            eprintln!("[renderer] vello (GPU): {} detected", info.name);
            match VelloRenderer::new() {
                Ok(r) => Box::new(r),
                Err(e) => {
                    eprintln!("[renderer] vello unavailable, using tiny-skia (CPU): {e}");
                    Box::new(TinySkiaRenderer::new())
                }
            }
        }
        _ => match VelloRenderer::new() {
            Ok(r) => {
                eprintln!("[renderer] vello (GPU): adapter type unknown, using GPU");
                Box::new(r)
            }
            Err(e) => {
                eprintln!("[renderer] vello unavailable, using tiny-skia (CPU): {e}");
                Box::new(TinySkiaRenderer::new())
            }
        },
    }
}

/// Тип и имя доступного GPU-адаптера (без создания устройства).
fn gpu_adapter_info() -> Result<Option<wgpu::AdapterInfo>, Box<dyn std::error::Error>> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::all(),
        flags: wgpu::InstanceFlags::default(),
        memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
        backend_options: wgpu::BackendOptions::default(),
        display: None,
    });
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        ..Default::default()
    }))?;
    Ok(Some(adapter.get_info()))
}

/// Мировая ограничивающая рамка по локальной геометрии и трансформации.
/// (Для рендера используется кэшированная `SceneGraph::world_bbox`.)
pub(crate) fn rects_intersect(a0: Vec2, a1: Vec2, b0: Vec2, b1: Vec2) -> bool {
    a0.x <= b1.x && a1.x >= b0.x && a0.y <= b1.y && a1.y >= b0.y
}

/// Контейнер, реально обрезающий своё содержимое: дети за его границами
/// не видны. Только такие узлы можно иерархически отсекать по viewport —
/// у Group/Component bbox вырожден, а Frame без `clip_content` может иметь
/// детей вне своих границ, поэтому их дети обходятся всегда.
pub(crate) fn clips_children(node: &super::model::scene::SceneNode) -> bool {
    matches!(node.kind, NodeKind::Frame { clip_content: true, .. })
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::model::nodes::ShapeKind;
    use super::super::model::scene::SceneNode;
    use super::super::model::types::Constraints;

    fn frame(clip: bool) -> SceneNode {
        SceneNode::new(
            "F",
            NodeKind::Frame {
                size: glam::Vec2::new(10.0, 10.0),
                clip_content: clip,
                corner_radii: [0.0; 4],
                auto_layout: None,
                constraints: Constraints::default(),
            },
        )
    }

    #[test]
    fn clips_children_only_for_clipped_frames() {
        assert!(clips_children(&frame(true)));
        assert!(!clips_children(&frame(false)));
        let group = SceneNode::new("G", NodeKind::Group);
        assert!(!clips_children(&group));
        let rect = SceneNode::new(
            "R",
            NodeKind::Shape(ShapeKind::Rectangle { size: glam::Vec2::new(5.0, 5.0), corner_radii: [0.0; 4] }),
        );
        assert!(!clips_children(&rect));
    }
}