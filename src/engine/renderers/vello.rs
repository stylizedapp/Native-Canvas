use super::{
    clips_children, rects_intersect, RenderOutcome, Renderer, CANVAS_BG, DROP_FILL, DROP_STROKE,
    GIZMO_HANDLE_FILL, GIZMO_HANDLE_STROKE, GRID, MARQUEE_FILL, MARQUEE_STROKE, ORIGIN, PAGE_BG,
    PAGE_BORDER, PREVIEW_FILL, PREVIEW_STROKE, SELECTION, PAGE_SIZE,
};
use super::geom::{geom_hash, grid_key, text_glyphs, GridKey};
use super::super::grid::GridConfig;
use super::super::model::nodes::{NodeKey, NodeKind, ShapeKind};
use super::super::model::scene::{SceneGraph, SceneNode};
use super::super::model::types::{Paint as ModelPaint, Stroke as ModelStroke};
use super::super::profiler::FrameMetrics;
use super::super::transform::Camera;
use crate::engine::controller::Preview;
use crate::engine::gizmo;
use glam::{Affine2, Vec2};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;
use vello::kurbo::{self, Cap, Join, Stroke};
use vello::peniko::{Brush, Color, Fill};
use vello::wgpu;
use vello::{AaConfig, AaSupport, RenderParams, Renderer as VelloCore, RendererOptions, Scene as VelloScene};

/// Слот readback-буфера (staging) с флагом готовности данных.
struct StagingSlot {
    width: u32,
    height: u32,
    buffer: wgpu::Buffer,
    /// Устанавливается колбэком map_async — буфер замаплен, данные готовы.
    ready: Arc<AtomicBool>,
    /// map_async вызван, буфер ещё не возвращён (переиспользовать нельзя).
    in_flight: bool,
}

/// Бэкенд на vello (GPU, wgpu). Рендерит офскрин в текстуру и делает readback
/// в тот же буфер, что и CPU-бэкенд, — ядро и UI не знают разницы.
///
/// Readback неблокирующий, с двойным буфером: каждый вызов `render` сначала
/// забирает готовый кадр из предыдущего сабмита, затем отправляет новый на GPU.
/// UI-поток никогда не ждёт завершения GPU-работы.
pub struct VelloRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    renderer: VelloCore,
    /// Переиспользуемый буфер сцены: `reset()` вместо аллокации каждый кадр.
    scene: VelloScene,
    target: Option<(u32, u32, wgpu::Texture)>,
    staging: [Option<StagingSlot>; 2],
    /// Хотя бы раз показали кадр (буфер не пустой мусор).
    has_image: bool,
    /// Кэш раскладки текста: NodeKey -> (хэш геометрии, слитый путь глифов).
    /// Снимает шейпинг/outline каждого кадра; при статичном узле — 1 draw-call.
    text: HashMap<NodeKey, (u64, kurbo::BezPath)>,
    /// Длина графа на последнем рендере: структурное изменение чистит `text`.
    text_len: usize,
    /// Кэш пути сетки: переиспользуется, пока камера/шаг не изменились.
    grid: Option<(GridKey, kurbo::BezPath)>,
}

impl VelloRenderer {
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            flags: wgpu::InstanceFlags::default(),
            memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
            backend_options: wgpu::BackendOptions::default(),
            display: None,
        });
        let adapter = pollster::block_on(instance.request_adapter(
            &wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                ..Default::default()
            },
        ))
        .map_err(|_| "no GPU adapter")?;
        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("native_canvas"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                ..Default::default()
            },
        ))?;
        let renderer = VelloCore::new(
            &device,
            RendererOptions {
                use_cpu: false,
                antialiasing_support: AaSupport::all(),
                num_init_threads: None,
                pipeline_cache: None,
            },
        )?;
        Ok(Self {
            device,
            queue,
            renderer,
            scene: VelloScene::new(),
            target: None,
            staging: [None, None],
            has_image: false,
            text: HashMap::new(),
            text_len: 0,
            grid: None,
        })
    }

    /// Кэш целевой текстуры под текущий размер буфера (хэндл — дешёвый клон).
    fn ensure_target(&mut self, width: u32, height: u32) -> wgpu::Texture {
        if self.target.as_ref().map(|(w, h, _)| *w != width || *h != height).unwrap_or(true) {
            let tex = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("vello-canvas"),
                size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::STORAGE_BINDING
                    | wgpu::TextureUsages::COPY_SRC
                    | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            self.target = Some((width, height, tex));
        }
        self.target.as_ref().unwrap().2.clone()
    }

    /// Выровненный до 256 байт размер строки (требование wgpu copy).
    fn aligned_bytes_per_row(width: u32) -> u32 {
        let raw = 4 * width;
        (raw + 255) & !255
    }

    /// Сбрасывает слоты под текущий размер, отбрасывая устаревшие pending-кадры.
    fn reset_staging(&mut self, width: u32, height: u32) {
        for slot in self.staging.iter_mut() {
            if let Some(s) = slot {
                if s.width != width || s.height != height {
                    if s.ready.load(Ordering::Relaxed) {
                        s.buffer.unmap();
                    }
                    s.buffer = create_staging(&self.device, width, height);
                    s.width = width;
                    s.height = height;
                    s.ready.store(false, Ordering::Relaxed);
                    s.in_flight = false;
                }
            }
        }
    }

    /// Копирует готовый замапленный слот в сплошной RGBA8-буфер `out`.
    fn collect_slot(slot: &StagingSlot, out: &mut [u8]) {
        let data = slot.buffer.slice(..).get_mapped_range();
        let row_bytes = (slot.width as usize) * 4;
        let bytes_per_row = Self::aligned_bytes_per_row(slot.width);
        for row in 0..slot.height as usize {
            let src = &data[row * bytes_per_row as usize..row * bytes_per_row as usize + row_bytes];
            let dst = &mut out[row * row_bytes..(row + 1) * row_bytes];
            dst.copy_from_slice(src);
        }
    }
}

impl Renderer for VelloRenderer {
    fn name(&self) -> &'static str {
        "vello (GPU)"
    }

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
    ) -> RenderOutcome {
        let t_total = Instant::now();
        let mut metrics = FrameMetrics::default();

        let bg = CANVAS_BG;
        let t_build = Instant::now();
        self.scene.reset();
        // Структурное изменение графа (вставка/удаление/загрузка) — кэш устарел.
        if scene.len() != self.text_len {
            self.text.clear();
            self.text_len = scene.len();
        }
        {
            let mut vs = &mut self.scene;

            let cam_affine = camera.to_affine();
        let cam_t = to_affine(cam_affine);
        let zoom = camera.zoom;

        let view_min = camera.screen_to_world(Vec2::ZERO);
        let view_max = camera.screen_to_world(Vec2::new(width as f32, height as f32));

        // Страница.
        if rects_intersect(Vec2::ZERO, PAGE_SIZE, view_min, view_max) {
            let page = kurbo::Rect::new(0.0, 0.0, PAGE_SIZE.x as f64, PAGE_SIZE.y as f64);
            fill_shape(&mut vs, cam_t, rgba8(PAGE_BG, 1.0), &page);
            stroke_shape(&mut vs, cam_t, rgba8(PAGE_BORDER, 1.0), (1.0 / zoom) as f64, &page);
        }

        // Сетка — один path, один draw-call, адаптивный шаг. Путь кэшируется:
        // пока камера и шаг не меняются (драг узла), перестраивать его не нужно.
        if grid.visible {
            let mut s = grid.step.max(1.0);
            while s * zoom < 8.0 {
                s *= 2.0;
            }
            let x0 = (view_min.x / s).floor();
            let x1 = (view_max.x / s).ceil();
            let y0 = (view_min.y / s).floor();
            let y1 = (view_max.y / s).ceil();
            let gk = grid_key(s, x0, x1, y0, y1, view_min.x, view_min.y, view_max.x, view_max.y);
            let pb = match &self.grid {
                Some((k, p)) if *k == gk => p,
                _ => {
                    let mut path = kurbo::BezPath::new();
                    let mut x = x0;
                    while x <= x1 {
                        let wx = x * s;
                        path.move_to((wx as f64, view_min.y as f64));
                        path.line_to((wx as f64, view_max.y as f64));
                        x += 1.0;
                    }
                    let mut y = y0;
                    while y <= y1 {
                        let wy = y * s;
                        path.move_to((view_min.x as f64, wy as f64));
                        path.line_to((view_max.x as f64, wy as f64));
                        y += 1.0;
                    }
                    self.grid = Some((gk, path));
                    &self.grid.as_ref().unwrap().1
                }
            };
            stroke_shape(&mut vs, cam_t, rgba8(GRID, 1.0), (1.0 / zoom) as f64, pb);
        }

        // Однопроходный обход дерева по кэшированным мировым трансформациям (O(n)).
        // Стек хранит только ключи: трансформации уже посчитаны в `flush_transforms`.
        let mut stack: Vec<NodeKey> = scene.roots().iter().rev().copied().collect();
        while let Some(key) = stack.pop() {
            let Some(node) = scene.get(key) else { continue };
            if !node.is_visible {
                continue;
            }
            let world = node.world_transform;
            // Кэшированная мировая рамка (уже посчитана в flush_transforms) —
            // без повторного measure текста и конвертации углов.
            let (mn, mx) = scene.world_bbox(key).unwrap_or((Vec2::ZERO, Vec2::ZERO));
            let visible = rects_intersect(mn, mx, view_min, view_max);
            if visible {
                let screen = to_affine(cam_affine * world);
                draw_node(&mut vs, &mut self.text, key, node, screen);
            }
            // Иерархический culling: у обрезанного контейнера вне вьюпорта детей
            // рисовать незачем; у остальных нод (вырожденный bbox / без clip)
            // спускаемся, чтобы не потерять видимых детей.
            let cull_children = clips_children(node) && !visible;
            if !cull_children {
                stack.extend(node.children.iter().rev().copied());
            }
        }

        // Подсветка выделенных узлов. Хэндлы гизмо — при ровно одном узле.
        for key in selected {
            if let Some((mn, mx)) = scene.world_bbox(*key) {
                let rect = kurbo::Rect::new(mn.x as f64, mn.y as f64, mx.x as f64, mx.y as f64);
                stroke_shape(&mut vs, cam_t, rgba8(SELECTION, 1.0), (1.5 / zoom) as f64, &rect);
                if selected.len() == 1 {
                    let resizable = scene
                        .get(*key)
                        .map(|n| gizmo::resizable(&n.kind))
                        .unwrap_or(false);
                    if resizable {
                        draw_gizmo_handles(&mut vs, cam_t, mn, mx, zoom);
                    }
                }
            }
        }

        // Подсветка фрейма-цели при перетаскивании (drop-таргет).
        if let Some(h) = hovered {
            if let Some((mn, mx)) = scene.world_bbox(h) {
                let rect = kurbo::Rect::new(mn.x as f64, mn.y as f64, mx.x as f64, mx.y as f64);
                fill_shape(&mut vs, cam_t, rgba8(DROP_FILL, 1.0), &rect);
                stroke_shape(&mut vs, cam_t, rgba8(DROP_STROKE, 1.0), (2.0 / zoom) as f64, &rect);
            }
        }

        // Рамка марки-выделения.
        if let Some((a, b)) = marquee {
            draw_marquee(&mut vs, cam_t, a, b, zoom);
        }

        // Маркер начала координат (0,0) — для визуальной проверки пана/зума.
        draw_origin(&mut vs, cam_t, (1.0 / zoom) as f64);

        // Live-превью.
        if let Some(p) = preview {
            draw_preview(&mut vs, cam_t, &p, (1.0 / zoom) as f64);
        }
        } // конец заимствования self.scene: дальше работаем только с GPU-ресурсами

        metrics.scene_build_us = t_build.elapsed().as_micros();

        // --- Неблокирующий пайплайн: собрать готовый кадр, отправить новый ---

        // Запускает отложенные колбэки map_async (без ожидания GPU).
        let t_readback = Instant::now();
        let _ = self.device.poll(wgpu::PollType::Poll);
        self.reset_staging(width, height);

        // Забираем готовый кадр из предыдущего сабмита (если такой есть).
        let mut presented = false;
        for slot in self.staging.iter_mut() {
            if let Some(s) = slot {
                if s.ready.load(Ordering::Relaxed) {
                    Self::collect_slot(s, out);
                    s.buffer.unmap();
                    s.ready.store(false, Ordering::Relaxed);
                    s.in_flight = false;
                    presented = true;
                }
            }
        }
        // Первый кадр: пока нет данных readback, заполняем фоном.
        if !presented && !self.has_image {
            for px in out.chunks_exact_mut(4) {
                px.copy_from_slice(&bg);
            }
            self.has_image = true;
        }
        metrics.gpu_readback_us = t_readback.elapsed().as_micros();

        // Свободный слот под новый кадр; если свободного нет — GPU не успевает,
        // кадр пропускаем (состояние останется грязным, дорисуем в следующий тик).
        let Some(idx) = self
            .staging
            .iter()
            .position(|s| match s {
                Some(slot) => !slot.in_flight,
                None => true,
            })
        else {
            metrics.total_us = t_total.elapsed().as_micros();
            return RenderOutcome { submitted: false, metrics };
        };

        // Рендер в текстуру (сабмит без ожидания) + readback-копия.
        let t_encode = Instant::now();
        let target = self.ensure_target(width, height);
        let view = target.create_view(&wgpu::TextureViewDescriptor {
            label: Some("vello-canvas-view"),
            format: None,
            dimension: None,
            usage: Some(wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::STORAGE_BINDING),
            aspect: wgpu::TextureAspect::All,
            base_mip_level: 0,
            mip_level_count: None,
            base_array_layer: 0,
            array_layer_count: None,
        });
        if let Err(e) = self.renderer.render_to_texture(
            &self.device,
            &self.queue,
            &self.scene,
            &view,
            &RenderParams {
                base_color: Color::from_rgb8(bg[0], bg[1], bg[2]),
                width,
                height,
                antialiasing_method: AaConfig::Area,
            },
        ) {
            eprintln!("vello render error: {e}");
            return RenderOutcome { submitted: false, metrics };
        }

        if self.staging[idx].is_none() {
            let buf = create_staging(&self.device, width, height);
            self.staging[idx] = Some(StagingSlot {
                width,
                height,
                buffer: buf,
                ready: Arc::new(AtomicBool::new(false)),
                in_flight: false,
            });
        }
        let slot = self.staging[idx].as_mut().unwrap();
        debug_assert!(!slot.in_flight);

        let bytes_per_row = Self::aligned_bytes_per_row(width);
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("vello-readback"),
        });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &target,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &slot.buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(bytes_per_row),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        );
        self.queue.submit([encoder.finish()]);

        let flag = slot.ready.clone();
        slot.buffer.slice(..).map_async(wgpu::MapMode::Read, move |_| {
            flag.store(true, Ordering::Relaxed);
        });
        slot.in_flight = true;
        metrics.gpu_encode_us = t_encode.elapsed().as_micros();
        metrics.total_us = t_total.elapsed().as_micros();

        RenderOutcome { submitted: true, metrics }
    }
}

fn create_staging(device: &wgpu::Device, width: u32, height: u32) -> wgpu::Buffer {
    let bytes_per_row = VelloRenderer::aligned_bytes_per_row(width);
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("vello-readback"),
        size: (bytes_per_row as u64) * (height as u64),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    })
}

/// glam::Affine2 -> kurbo::Affine (x' = a*x + c*y + e; y' = b*x + d*y + f).
fn to_affine(a: Affine2) -> kurbo::Affine {
    let m = a.matrix2;
    kurbo::Affine::new([
        m.x_axis.x as f64,
        m.x_axis.y as f64,
        m.y_axis.x as f64,
        m.y_axis.y as f64,
        a.translation.x as f64,
        a.translation.y as f64,
    ])
}

fn fill_shape(scene: &mut VelloScene, t: kurbo::Affine, color: Color, shape: &impl kurbo::Shape) {
    scene.fill(Fill::NonZero, t, Brush::Solid(color), None, shape);
}

fn stroke_shape(scene: &mut VelloScene, t: kurbo::Affine, color: Color, width: f64, shape: &impl kurbo::Shape) {
    let stroke = Stroke {
        width,
        join: Join::Round,
        miter_limit: 4.0,
        start_cap: Cap::Round,
        end_cap: Cap::Round,
        dash_pattern: Default::default(),
        dash_offset: 0.0,
    };
    scene.stroke(&stroke, t, Brush::Solid(color), None, shape);
}

/// Маркер начала координат (0,0): крест + точка. Размер на экране постоянный
/// (умножается на 1/zoom), поэтому при зуме остаётся читаемым.
fn draw_origin(scene: &mut VelloScene, cam_t: kurbo::Affine, w: f64) {
    let color = rgba8(ORIGIN, 1.0);
    let arm = 6.0 * w;
    let cross_h = kurbo::Line::new((-arm, 0.0), (arm, 0.0));
    stroke_shape(scene, cam_t, color, w, &cross_h);
    let cross_v = kurbo::Line::new((0.0, -arm), (0.0, arm));
    stroke_shape(scene, cam_t, color, w, &cross_v);
    let dot = kurbo::Ellipse::new((0.0, 0.0), (2.5 * w, 2.5 * w), 0.0);
    fill_shape(scene, cam_t, color, &dot);
}

/// 8 квадратов-хэндлов гизмо (белая заливка, синяя обводка). Экранный размер
/// фиксированный — в мировых координатах это 10/zoom.
fn draw_gizmo_handles(scene: &mut VelloScene, cam_t: kurbo::Affine, mn: Vec2, mx: Vec2, zoom: f32) {
    let h = (gizmo::HANDLE_SIZE / zoom) as f64;
    let w = (h * 0.12).max((1.0 / zoom) as f64);
    for (_handle, (a, b)) in gizmo::handle_rects(mn, mx, zoom) {
        let rect = kurbo::Rect::new(a.x as f64, a.y as f64, b.x as f64, b.y as f64);
        fill_shape(scene, cam_t, rgba8(GIZMO_HANDLE_FILL, 1.0), &rect);
        stroke_shape(scene, cam_t, rgba8(GIZMO_HANDLE_STROKE, 1.0), w, &rect);
    }
}

/// Рамка марки + полупрозрачная заливка. Штрих и width делим на zoom, чтобы
/// оставались постоянными на экране (vello применяет dash в мировых координатах).
fn draw_marquee(scene: &mut VelloScene, cam_t: kurbo::Affine, a: Vec2, b: Vec2, zoom: f32) {
    let min = Vec2::new(a.x.min(b.x), a.y.min(b.y));
    let max = Vec2::new(a.x.max(b.x), a.y.max(b.y));
    let rect = kurbo::Rect::new(min.x as f64, min.y as f64, max.x as f64, max.y as f64);
    fill_shape(scene, cam_t, rgba8(MARQUEE_FILL, 1.0), &rect);
    let dash = (4.0 / zoom).max(1.0) as f64;
    let stroke = Stroke {
        width: (1.0 / zoom) as f64,
        join: Join::Round,
        miter_limit: 4.0,
        start_cap: Cap::Butt,
        end_cap: Cap::Butt,
        dash_pattern: vec![dash, dash].into(),
        dash_offset: 0.0,
    };
    scene.stroke(&stroke, cam_t, Brush::Solid(rgba8(MARQUEE_STROKE, 1.0)), None, &rect);
}

/// Первая сплошная заливка узла в виде RGBA8 (градиенты пока игнорируются).
fn solid_fill(node: &SceneNode) -> Option<[u8; 4]> {
    node.fills.iter().find_map(|p| match p {
        ModelPaint::Solid(c) => Some(c.to_rgba8()),
        _ => None,
    })
}

/// Заливает и обводит прямоугольник/эллипс (общий путь для Frame/Rect).
fn draw_filled(
    scene: &mut VelloScene,
    node: &SceneNode,
    screen: kurbo::Affine,
    shape: &impl kurbo::Shape,
) {
    // Эффективная альфа заливки и обводки (с учётом opacity узла).
    let fill_a = solid_fill(node)
        .map(|f| (f[3] as f32 * node.opacity).round().clamp(0.0, 255.0) as u8)
        .unwrap_or(0);
    let stroke_a = node
        .strokes
        .first()
        .and_then(|st| match &st.paint {
            ModelPaint::Solid(c) if st.width > 0.0 => Some(
                (c.to_rgba8()[3] as f32 * node.opacity).round().clamp(0.0, 255.0) as u8,
            ),
            _ => None,
        })
        .unwrap_or(0);
    // Fill alpha 0 и нет активной обводки: узел полностью невидим (без контура).
    if fill_a == 0 && stroke_a == 0 {
        return;
    }
    if fill_a > 0 {
        if let Some(f) = solid_fill(node) {
            fill_shape(scene, screen, rgba8(f, node.opacity), shape);
        }
    }
    if stroke_a > 0 {
        if let Some(st) = node.strokes.first() {
            if let ModelPaint::Solid(_) = &st.paint {
                stroke_node(scene, screen, st, node.opacity, shape);
            }
        }
    }
}

/// Обводка узла с учётом пунктира (dash в мировых координатах).
fn stroke_node(
    scene: &mut VelloScene,
    t: kurbo::Affine,
    st: &ModelStroke,
    opacity: f32,
    shape: &impl kurbo::Shape,
) {
    if st.width <= 0.0 {
        return;
    }
    let color = match &st.paint {
        ModelPaint::Solid(c) => rgba8(c.to_rgba8(), opacity),
        _ => return,
    };
    let mut stroke = Stroke {
        width: st.width as f64,
        join: Join::Round,
        miter_limit: 4.0,
        start_cap: Cap::Round,
        end_cap: Cap::Round,
        dash_pattern: Default::default(),
        dash_offset: 0.0,
    };
    if !st.dash_pattern.is_empty() {
        stroke.dash_pattern = st.dash_pattern.iter().map(|d| *d as f64).collect::<Vec<_>>().into();
    }
    scene.stroke(&stroke, t, Brush::Solid(color), None, shape);
}

fn draw_node(
    scene: &mut VelloScene,
    text: &mut HashMap<NodeKey, (u64, kurbo::BezPath)>,
    key: NodeKey,
    node: &SceneNode,
    screen: kurbo::Affine,
) {
    match &node.kind {
        NodeKind::Frame { size, corner_radii, .. }
        | NodeKind::Shape(ShapeKind::Rectangle { size, corner_radii }) => {
            if *corner_radii == [0.0; 4] {
                let rect = kurbo::Rect::new(0.0, 0.0, size.x as f64, size.y as f64);
                draw_filled(scene, node, screen, &rect);
            } else {
                // Обводка идёт по тому же скруглённому пути, что и заливка.
                let rr = kurbo::RoundedRect::new(
                    0.0,
                    0.0,
                    size.x as f64,
                    size.y as f64,
                    kurbo::RoundedRectRadii::new(
                        corner_radii[0] as f64,
                        corner_radii[1] as f64,
                        corner_radii[2] as f64,
                        corner_radii[3] as f64,
                    ),
                );
                draw_filled(scene, node, screen, &rr);
            }
        }
        NodeKind::Shape(ShapeKind::Ellipse { radii, .. }) => {
            let rx = radii.x as f64 / 2.0;
            let ry = radii.y as f64 / 2.0;
            let ell = kurbo::Ellipse::new((rx, ry), (rx, ry), 0.0);
            draw_filled(scene, node, screen, &ell);
        }
        NodeKind::Text { .. } => {
            if let Some(f) = solid_fill(node) {
                let a = (f[3] as f32 * node.opacity).round().clamp(0.0, 255.0) as u8;
                if a > 0 {
                    let brush = Brush::Solid(rgba8(f, node.opacity));
                    // Кэш: шейпинг + outline текста один раз, дальше — один
                    // draw-call слитым контуром глифов.
                    let h = geom_hash(&node.kind);
                    if text.get(&key).map(|(ph, _)| *ph != h).unwrap_or(true) {
                        text.insert(key, (h, text_glyphs(&node.kind)));
                    }
                    if let Some((_, path)) = text.get(&key) {
                        scene.fill(Fill::NonZero, screen, brush, None, path);
                    }
                }
            }
        }
        NodeKind::Shape(ShapeKind::Line { start, end }) => {
            if let Some(st) = node.strokes.first() {
                if st.width > 0.0 {
                    if let ModelPaint::Solid(c) = &st.paint {
                        let sa = (c.to_rgba8()[3] as f32 * node.opacity)
                            .round()
                            .clamp(0.0, 255.0) as u8;
                        if sa > 0 {
                            let line = kurbo::Line::new(
                                (start.x as f64, start.y as f64),
                                (end.x as f64, end.y as f64),
                            );
                            stroke_node(scene, screen, st, node.opacity, &line);
                        }
                    }
                }
            }
        }
        _ => {}
    }
}

fn draw_preview(scene: &mut VelloScene, cam_t: kurbo::Affine, p: &Preview, w: f64) {
    let min = Vec2::new(p.a.x.min(p.b.x), p.a.y.min(p.b.y));
    let max = Vec2::new(p.a.x.max(p.b.x), p.a.y.max(p.b.y));
    let size = max - min;
    let fill = rgba8(PREVIEW_FILL, 1.0);
    let stroke = rgba8(PREVIEW_STROKE, 1.0);
    match &p.kind {
        NodeKind::Frame { .. } | NodeKind::Shape(ShapeKind::Rectangle { .. }) => {
            let rect = kurbo::Rect::new(min.x as f64, min.y as f64, max.x as f64, max.y as f64);
            fill_shape(scene, cam_t, fill, &rect);
            stroke_shape(scene, cam_t, stroke, w, &rect);
        }
        NodeKind::Shape(ShapeKind::Ellipse { .. }) => {
            let ell = kurbo::Ellipse::new(
                ((min.x + size.x / 2.0) as f64, (min.y + size.y / 2.0) as f64),
                ((size.x / 2.0) as f64, (size.y / 2.0) as f64),
                0.0,
            );
            fill_shape(scene, cam_t, fill, &ell);
            stroke_shape(scene, cam_t, stroke, w, &ell);
        }
        NodeKind::Shape(ShapeKind::Line { .. }) => {
            let line = kurbo::Line::new((p.a.x as f64, p.a.y as f64), (p.b.x as f64, p.b.y as f64));
            stroke_shape(scene, cam_t, stroke, w, &line);
        }
        _ => {}
    }
}

fn rgba8(c: [u8; 4], opacity: f32) -> Color {
    let a = (c[3] as f32 * opacity).round().clamp(0.0, 255.0) as u8;
    Color::from_rgba8(c[0], c[1], c[2], a)
}