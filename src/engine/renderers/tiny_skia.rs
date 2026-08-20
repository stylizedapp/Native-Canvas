use super::{
    clips_children, rects_intersect, RenderOutcome, Renderer, CANVAS_BG, DROP_FILL, DROP_STROKE,
    GIZMO_HANDLE_FILL, GIZMO_HANDLE_STROKE, GRID, MARQUEE_FILL, MARQUEE_STROKE, ORIGIN, PAGE_BG,
    PAGE_BORDER, PREVIEW_FILL, PREVIEW_STROKE, SELECTION, PAGE_SIZE,
};
use super::geom::{geom_hash, grid_key, GridKey};
use super::super::grid::GridConfig;
use super::super::model::nodes::{NodeKey, NodeKind, ShapeKind};
use super::super::model::scene::{SceneGraph, SceneNode};
use super::super::model::types::Paint as ModelPaint;
use super::super::profiler::FrameMetrics;
use super::super::transform::Camera;
use crate::engine::controller::Preview;
use crate::engine::gizmo;
use glam::{Affine2, Vec2};
use std::collections::HashMap;
use std::time::Instant;
use tiny_skia::{
    Color, FillRule, Paint, PathBuilder, PixmapMut, Rect, Stroke as SkStroke, StrokeDash, Transform,
};

/// Бэкенд на tiny-skia (CPU-растеризация) — фолбэк, когда GPU недоступен.
pub struct TinySkiaRenderer {
    /// Кэш контуров узлов: NodeKey -> (хэш геометрии, Path). Для скруглённых
    /// прямоугольников, эллипсов и текста; обычные прямоугольники идут через
    /// `fill_rect` без построения Path.
    paths: HashMap<NodeKey, (u64, tiny_skia::Path)>,
    /// Длина графа на последнем рендере: структурное изменение чистит `paths`.
    last_len: usize,
    /// Кэш пути сетки (переиспользуется при статичной камере).
    grid: Option<(GridKey, tiny_skia::Path)>,
}

impl Default for TinySkiaRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl TinySkiaRenderer {
    pub fn new() -> Self {
        Self {
            paths: HashMap::new(),
            last_len: 0,
            grid: None,
        }
    }
}

impl Renderer for TinySkiaRenderer {
    fn name(&self) -> &'static str {
        "tiny-skia (CPU)"
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
        let t0 = Instant::now();
        let mut pixmap =
            PixmapMut::from_bytes(out, width, height).expect("pixmap");
        pixmap.fill(Color::from_rgba8(CANVAS_BG[0], CANVAS_BG[1], CANVAS_BG[2], CANVAS_BG[3]));

        // Структурное изменение графа (вставка/удаление/загрузка) — кэш устарел.
        if scene.len() != self.last_len {
            self.paths.clear();
            self.last_len = scene.len();
        }

        let cam = camera.to_affine();
        let cam_ts = Transform::from_row(
            cam.matrix2.x_axis.x,
            cam.matrix2.x_axis.y,
            cam.matrix2.y_axis.x,
            cam.matrix2.y_axis.y,
            cam.translation.x,
            cam.translation.y,
        );

        // Видимая область в мировых координатах (для отсечения).
        let view_min = camera.screen_to_world(Vec2::ZERO);
        let view_max = camera.screen_to_world(Vec2::new(width as f32, height as f32));

        // Страница (границы холста) — один clipped rect, до нод.
        draw_page(&mut pixmap, cam_ts, camera, view_min, view_max);

        // Сетка — один draw-call, только видимая область. Путь кэшируется
        // (стабильный при статичной камере — драг узла).
        if grid.visible {
            draw_grid(
                &mut pixmap,
                cam_ts,
                camera,
                view_min,
                view_max,
                grid.step,
                &mut self.grid,
            );
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
            // Кэшированная мировая рамка (flush_transforms) — без повторного
            // measure текста и конвертации углов.
            let (mn, mx) = scene.world_bbox(key).unwrap_or((Vec2::ZERO, Vec2::ZERO));
            let visible = rects_intersect(mn, mx, view_min, view_max);
            if visible {
                draw_node(&mut pixmap, &mut self.paths, key, node, world, cam_ts, camera.zoom);
            }
            // Иерархический culling: у обрезанного контейнера вне вьюпорта детей
            // рисовать незачем; у остальных нод (вырожденный bbox / без clip)
            // спускаемся, чтобы не потерять видимых детей.
            let cull_children = clips_children(node) && !visible;
            if !cull_children {
                stack.extend(node.children.iter().rev().copied());
            }
        }

        // Подсветка выделенных узлов (обводка ограничивающей рамки).
        // Хэндлы гизмо рисуем только при ровно одном выделенном узле.
        for key in selected {
            if let Some((mn, mx)) = scene.world_bbox(*key) {
                draw_bbox_highlight(&mut pixmap, cam_ts, mn, mx);
                if selected.len() == 1 {
                    let resizable = scene
                        .get(*key)
                        .map(|n| gizmo::resizable(&n.kind))
                        .unwrap_or(false);
                    if resizable {
                        draw_gizmo_handles(&mut pixmap, cam_ts, mn, mx, camera.zoom);
                    }
                }
            }
        }

        // Подсветка фрейма-цели при перетаскивании (drop-таргет).
        if let Some(h) = hovered {
            if let Some((mn, mx)) = scene.world_bbox(h) {
                draw_drop_highlight(&mut pixmap, cam_ts, mn, mx, camera.zoom);
            }
        }

        // Рамка марки-выделения.
        if let Some((a, b)) = marquee {
            draw_marquee(&mut pixmap, cam_ts, a, b, camera.zoom);
        }

        // Маркер начала координат (0,0) — для визуальной проверки пана/зума.
        draw_origin(&mut pixmap, cam_ts, camera.zoom);

        // Live-превью создаваемой фигуры.
        if let Some(p) = preview {
            draw_preview(&mut pixmap, cam_ts, p);
        }

        // CPU-бэкенд: весь кадр — построение сцены; GPU-стадий нет.
        let us = t0.elapsed().as_micros();
        RenderOutcome {
            submitted: true,
            metrics: FrameMetrics { scene_build_us: us, gpu_encode_us: 0, gpu_readback_us: 0, total_us: us },
        }
    }
}

/// Границы страницы (размер PAGE_SIZE). Заливка + рамка, отсечено по viewport.
fn draw_page(pixmap: &mut PixmapMut, cam_ts: Transform, camera: &Camera, view_min: Vec2, view_max: Vec2) {
    if !rects_intersect(Vec2::ZERO, PAGE_SIZE, view_min, view_max) {
        return;
    }
    let Some(rect) = Rect::from_xywh(0.0, 0.0, PAGE_SIZE.x, PAGE_SIZE.y) else { return };

    let mut fp = Paint::default();
    fp.set_color_rgba8(PAGE_BG[0], PAGE_BG[1], PAGE_BG[2], PAGE_BG[3]);
    fp.anti_alias = false;
    pixmap.fill_rect(rect, &fp, cam_ts, None);

    let mut pb = PathBuilder::new();
    pb.push_rect(rect);
    let Some(path) = pb.finish() else { return };
    let mut sp = Paint::default();
    sp.set_color_rgba8(PAGE_BORDER[0], PAGE_BORDER[1], PAGE_BORDER[2], PAGE_BORDER[3]);
    sp.anti_alias = false;
    let sk = SkStroke { width: 1.0 / camera.zoom, ..Default::default() };
    pixmap.stroke_path(&path, &sp, &sk, cam_ts, None);
}

/// Сетка в мировых координатах, адаптивный шаг (×2, пока шаг на экране < 8px).
/// Все линии в одном пути — один draw-call. Путь кэшируется до смены камеры/шага.
#[allow(clippy::too_many_arguments)]
fn draw_grid(
    pixmap: &mut PixmapMut,
    cam_ts: Transform,
    camera: &Camera,
    view_min: Vec2,
    view_max: Vec2,
    step: f32,
    cache: &mut Option<(GridKey, tiny_skia::Path)>,
) {
    let mut s = step.max(1.0);
    while s * camera.zoom < 8.0 {
        s *= 2.0;
    }

    let x0 = (view_min.x / s).floor();
    let x1 = (view_max.x / s).ceil();
    let y0 = (view_min.y / s).floor();
    let y1 = (view_max.y / s).ceil();
    let gk = grid_key(s, x0, x1, y0, y1, view_min.x, view_min.y, view_max.x, view_max.y);

    let cached = cache.as_ref().map(|(k, _)| *k == gk).unwrap_or(false);
    if !cached {
        let mut pb = PathBuilder::new();
        let mut x = x0;
        while x <= x1 {
            let wx = x * s;
            pb.move_to(wx, view_min.y);
            pb.line_to(wx, view_max.y);
            x += 1.0;
        }
        let mut y = y0;
        while y <= y1 {
            let wy = y * s;
            pb.move_to(view_min.x, wy);
            pb.line_to(view_max.x, wy);
            y += 1.0;
        }
        let Some(path) = pb.finish() else { return };
        *cache = Some((gk, path));
    }
    let path = &cache.as_ref().unwrap().1;

    let mut sp = Paint::default();
    sp.set_color_rgba8(GRID[0], GRID[1], GRID[2], GRID[3]);
    sp.anti_alias = false;
    let sk = SkStroke { width: 1.0 / camera.zoom, ..Default::default() };
    pixmap.stroke_path(path, &sp, &sk, cam_ts, None);
}

/// Добавляет в PathBuilder скруглённый прямоугольник `0..size` с радиусами
/// `[tl,tr,br,bl]` (дуги — кубические аппроксимации). Заливка и обводка
/// используют один и тот же путь, поэтому обводка не срезает углы.
fn push_rounded_rect(pb: &mut PathBuilder, size: Vec2, radii: [f32; 4]) {
    let w = size.x.max(0.0);
    let h = size.y.max(0.0);
    let max_r = w.min(h) * 0.5;
    let r = [
        radii[0].max(0.0).min(max_r),
        radii[1].max(0.0).min(max_r),
        radii[2].max(0.0).min(max_r),
        radii[3].max(0.0).min(max_r),
    ];
    let k = 0.552_284_75; // константа кубической аппроксимации дуги 90°.
    // Старт: верхняя грань правее верхнего-левого угла.
    pb.move_to(r[0], 0.0);
    // Верхняя грань до верхнего-правого угла.
    pb.line_to(w - r[1], 0.0);
    // TR.
    if r[1] > 0.0 {
        pb.cubic_to(w - r[1] + r[1] * k, 0.0, w, r[1] * k, w, r[1]);
    } else {
        pb.line_to(w, 0.0);
    }
    // Правая грань до нижнего-правого угла.
    pb.line_to(w, h - r[2]);
    // BR.
    if r[2] > 0.0 {
        pb.cubic_to(w, h - r[2] + r[2] * k, w - r[2] + r[2] * k, h, w - r[2], h);
    } else {
        pb.line_to(w, h);
    }
    // Нижняя грань до нижнего-левого угла.
    pb.line_to(r[3], h);
    // BL.
    if r[3] > 0.0 {
        pb.cubic_to(r[3] - r[3] * k, h, 0.0, h - r[3] + r[3] * k, 0.0, h - r[3]);
    } else {
        pb.line_to(0.0, h);
    }
    // Левая грань до верхнего-левого угла.
    pb.line_to(0.0, r[0]);
    // TL.
    if r[0] > 0.0 {
        pb.cubic_to(0.0, r[0] - r[0] * k, r[0] - r[0] * k, 0.0, r[0], 0.0);
    }
    pb.close();
}

#[allow(clippy::too_many_arguments)]
fn draw_node(
    pixmap: &mut PixmapMut,
    paths: &mut HashMap<NodeKey, (u64, tiny_skia::Path)>,
    key: NodeKey,
    node: &SceneNode,
    world: Affine2,
    cam_ts: Transform,
    zoom: f32,
) {
    let combined = cam_ts.pre_concat(Transform::from_row(
        world.matrix2.x_axis.x,
        world.matrix2.x_axis.y,
        world.matrix2.y_axis.x,
        world.matrix2.y_axis.y,
        world.translation.x,
        world.translation.y,
    ));

    // Текст: заливка векторных контуров глифов (уже в локальных координатах
    // узла, мировая трансформация применится через `combined`). Контур всех
    // глифов кэшируется и рисуется одним fill_path.
    if let NodeKind::Text { .. } = &node.kind {
        if let Some(f) = solid_fill(node) {
            let a = (f[3] as f32 * node.opacity).round() as u8;
            if a > 0 {
                let mut paint = Paint::default();
                paint.set_color_rgba8(f[0], f[1], f[2], a);
                paint.anti_alias = true;
                if let Some(path) = cached_path(paths, key, &node.kind) {
                    pixmap.fill_path(path, &paint, FillRule::Winding, combined, None);
                }
            }
        }
        return;
    }

    // Обычный прямоугольник (Frame/Rectangle без скругления) заливаем через
    // `fill_rect` — без аллокаций PathBuilder/Path на каждый узел в кадре.
    let plain_rect: Option<Rect> = match &node.kind {
        NodeKind::Frame { size, corner_radii, .. }
        | NodeKind::Shape(ShapeKind::Rectangle { size, corner_radii }) => {
            if *corner_radii == [0.0; 4] {
                Rect::from_xywh(0.0, 0.0, size.x, size.y)
            } else {
                None
            }
        }
        _ => None,
    };

    // Заливка (первая сплошная; градиенты пока игнорируются).
    let fill_a = solid_fill(node)
        .map(|f| (f[3] as f32 * node.opacity).round() as u8)
        .unwrap_or(0);
    // Активная обводка: ширина > 0, сплошная и эффективная альфа > 0.
    let stroke_a = node
        .strokes
        .first()
        .and_then(|st| match &st.paint {
            ModelPaint::Solid(c) if st.width > 0.0 => {
                let sc8 = c.to_rgba8();
                Some((sc8[3] as f32 * node.opacity).round() as u8)
            }
            _ => None,
        })
        .unwrap_or(0);
    // Fill alpha 0 и нет активной обводки: узел полностью невидим (без контура).
    if fill_a == 0 && stroke_a == 0 {
        return;
    }
    if fill_a > 0 {
        let f = solid_fill(node).unwrap();
        let mut paint = Paint::default();
        paint.set_color_rgba8(f[0], f[1], f[2], fill_a);
        paint.anti_alias = true;
        match plain_rect {
            Some(r) => pixmap.fill_rect(r, &paint, combined, None),
            None => {
                if let Some(path) = cached_path(paths, key, &node.kind) {
                    pixmap.fill_path(path, &paint, FillRule::Winding, combined, None);
                }
            }
        }
    }
    // Обводка.
    if stroke_a > 0 {
        if let Some(st) = node.strokes.first() {
            if let ModelPaint::Solid(c) = &st.paint {
                let sc8 = c.to_rgba8();
                let scale = world.matrix2.x_axis.length().max(world.matrix2.y_axis.length());
                let mut sp = Paint::default();
                sp.set_color_rgba8(sc8[0], sc8[1], sc8[2], stroke_a);
                sp.anti_alias = true;
                let sk = SkStroke {
                    width: st.width * scale,
                    miter_limit: 4.0,
                    line_cap: tiny_skia::LineCap::Round,
                    line_join: tiny_skia::LineJoin::Round,
                    dash: if st.dash_pattern.is_empty() {
                        None
                    } else {
                        // Пунктир в экранных координатах — делим на zoom, чтобы штрих
                        // был постоянным на экране.
                        let d: Vec<f32> = st.dash_pattern.iter().map(|d| *d / zoom).collect();
                        StrokeDash::new(d, 0.0)
                    },
                };
                if let Some(path) = cached_path(paths, key, &node.kind) {
                    pixmap.stroke_path(path, &sp, &sk, combined, None);
                }
            }
        }
    }
}

/// Контур узла из кэша, либо построение и вставка. Хэш геометрии инвалидирует
/// запись при изменении параметров фигуры/текста. Возвращает ссылку на путь
/// в кэше (без клонирования на каждый кадр).
fn cached_path<'a>(
    paths: &'a mut HashMap<NodeKey, (u64, tiny_skia::Path)>,
    key: NodeKey,
    kind: &NodeKind,
) -> Option<&'a tiny_skia::Path> {
    let h = geom_hash(kind);
    if paths.get(&key).map(|(ph, _)| *ph != h).unwrap_or(true) {
        if let Some(p) = build_cached_path(kind) {
            paths.insert(key, (h, p));
        }
    }
    paths.get(&key).map(|(_, p)| p)
}

/// Строит контур узла для кэша: текст — все глифы одним путём; фигуры — как
/// `build_node_path` (обычные прямоугольники в кэш не попадают: они идут
/// через `fill_rect`).
fn build_cached_path(kind: &NodeKind) -> Option<tiny_skia::Path> {
    match kind {
        NodeKind::Text { .. } => {
            let mut pb = PathBuilder::new();
            for line in crate::engine::text::layout(kind) {
                for glyph in &line.glyphs {
                    push_kurbo_path(&mut pb, glyph);
                }
            }
            pb.finish()
        }
        _ => build_node_path(kind),
    }
}

/// Строит контур узла (для заливки/обводки невырожденных примитивов).
fn build_node_path(kind: &NodeKind) -> Option<tiny_skia::Path> {
    let mut pb = PathBuilder::new();
    match kind {
        NodeKind::Frame { size, corner_radii, .. }
        | NodeKind::Shape(ShapeKind::Rectangle { size, corner_radii }) => {
            push_rounded_rect(&mut pb, *size, *corner_radii);
        }
        NodeKind::Shape(ShapeKind::Ellipse { radii, .. }) => {
            let Some(rect) = Rect::from_xywh(0.0, 0.0, radii.x, radii.y) else { return None };
            pb.push_oval(rect);
        }
        NodeKind::Shape(ShapeKind::Line { start, end }) => {
            pb.move_to(start.x, start.y);
            pb.line_to(end.x, end.y);
        }
        _ => return None,
    }
    pb.finish()
}

/// Переносит kurbo-контур в tiny_skia PathBuilder.
fn push_kurbo_path(pb: &mut PathBuilder, path: &kurbo::BezPath) {
    for el in path.elements() {
        match el {
            kurbo::PathEl::MoveTo(p) => pb.move_to(p.x as f32, p.y as f32),
            kurbo::PathEl::LineTo(p) => pb.line_to(p.x as f32, p.y as f32),
            kurbo::PathEl::QuadTo(c, p) => {
                pb.quad_to(c.x as f32, c.y as f32, p.x as f32, p.y as f32)
            }
            kurbo::PathEl::CurveTo(c1, c2, p) => pb.cubic_to(
                c1.x as f32,
                c1.y as f32,
                c2.x as f32,
                c2.y as f32,
                p.x as f32,
                p.y as f32,
            ),
            kurbo::PathEl::ClosePath => {
                pb.close();
            }
        }
    }
}

/// Первая сплошная заливка узла в виде RGBA8.
fn solid_fill(node: &SceneNode) -> Option<[u8; 4]> {
    node.fills.iter().find_map(|p| match p {
        ModelPaint::Solid(c) => Some(c.to_rgba8()),
        _ => None,
    })
}

/// Маркер начала координат (0,0): крест + точка. Размер на экране постоянный
/// (масштабируется на 1/zoom), поэтому при зуме остаётся читаемым.
fn draw_origin(pixmap: &mut PixmapMut, cam_ts: Transform, zoom: f32) {
    let w = 1.0 / zoom;

    let mut pb = PathBuilder::new();
    let arm = 6.0 * w;
    pb.move_to(-arm, 0.0);
    pb.line_to(arm, 0.0);
    pb.move_to(0.0, -arm);
    pb.line_to(0.0, arm);
    if let Some(path) = pb.finish() {
        let mut sp = Paint::default();
        sp.set_color_rgba8(ORIGIN[0], ORIGIN[1], ORIGIN[2], ORIGIN[3]);
        sp.anti_alias = true;
        let sk = SkStroke { width: w, ..Default::default() };
        pixmap.stroke_path(&path, &sp, &sk, cam_ts, None);
    }

    let Some(rect) = Rect::from_xywh(-2.5 * w, -2.5 * w, 5.0 * w, 5.0 * w) else { return };
    let mut fp = Paint::default();
    fp.set_color_rgba8(ORIGIN[0], ORIGIN[1], ORIGIN[2], ORIGIN[3]);
    fp.anti_alias = true;
    pixmap.fill_rect(rect, &fp, cam_ts, None);
}

fn draw_bbox_highlight(pixmap: &mut PixmapMut, cam_ts: Transform, mn: Vec2, mx: Vec2) {
    let w = (mx.x - mn.x).max(1.0);
    let h = (mx.y - mn.y).max(1.0);
    let mut pb = PathBuilder::new();
    if let Some(rect) = Rect::from_xywh(mn.x, mn.y, w, h) {
        pb.push_rect(rect);
    }
    if let Some(path) = pb.finish() {
        let mut sp = Paint::default();
        sp.set_color_rgba8(SELECTION[0], SELECTION[1], SELECTION[2], SELECTION[3]);
        sp.anti_alias = true;
        let sk = SkStroke { width: 1.5, ..Default::default() };
        pixmap.stroke_path(&path, &sp, &sk, cam_ts, None);
    }
}

/// Подсветка фрейма-цели: полупрозрачная заливка + рамка (как marquee, но толще).
fn draw_drop_highlight(pixmap: &mut PixmapMut, cam_ts: Transform, mn: Vec2, mx: Vec2, zoom: f32) {
    let w = (mx.x - mn.x).max(1.0);
    let h = (mx.y - mn.y).max(1.0);
    let Some(rect) = Rect::from_xywh(mn.x, mn.y, w, h) else { return };
    let mut fp = Paint::default();
    fp.set_color_rgba8(DROP_FILL[0], DROP_FILL[1], DROP_FILL[2], DROP_FILL[3]);
    fp.anti_alias = true;
    pixmap.fill_rect(rect, &fp, cam_ts, None);
    let mut pb = PathBuilder::new();
    pb.push_rect(rect);
    let Some(path) = pb.finish() else { return };
    let mut sp = Paint::default();
    sp.set_color_rgba8(DROP_STROKE[0], DROP_STROKE[1], DROP_STROKE[2], DROP_STROKE[3]);
    sp.anti_alias = true;
    let sk = SkStroke { width: (2.0 / zoom).max(1.0), ..Default::default() };
    pixmap.stroke_path(&path, &sp, &sk, cam_ts, None);
}

/// 8 квадратов-хэндлов гизмо (белая заливка, синяя обводка). Экранный размер
/// фиксированный — в мировых координатах это 10/zoom.
fn draw_gizmo_handles(pixmap: &mut PixmapMut, cam_ts: Transform, mn: Vec2, mx: Vec2, zoom: f32) {
    let h = gizmo::HANDLE_SIZE / zoom;
    for (_handle, (a, b)) in gizmo::handle_rects(mn, mx, zoom) {
        let Some(rect) = Rect::from_xywh(a.x, a.y, b.x - a.x, b.y - a.y) else { continue };
        let mut fp = Paint::default();
        fp.set_color_rgba8(GIZMO_HANDLE_FILL[0], GIZMO_HANDLE_FILL[1], GIZMO_HANDLE_FILL[2], GIZMO_HANDLE_FILL[3]);
        fp.anti_alias = true;
        pixmap.fill_rect(rect, &fp, cam_ts, None);
        let mut pb = PathBuilder::new();
        pb.push_rect(rect);
        let Some(path) = pb.finish() else { continue };
        let mut sp = Paint::default();
        sp.set_color_rgba8(GIZMO_HANDLE_STROKE[0], GIZMO_HANDLE_STROKE[1], GIZMO_HANDLE_STROKE[2], GIZMO_HANDLE_STROKE[3]);
        sp.anti_alias = true;
        let sk = SkStroke { width: (h * 0.12).max(1.0 / zoom), ..Default::default() };
        pixmap.stroke_path(&path, &sp, &sk, cam_ts, None);
    }
}

/// Пунктирная рамка марки + полупрозрачная заливка. Dash в мировых координатах
/// делим на zoom, чтобы штрих был постоянным на экране.
fn draw_marquee(pixmap: &mut PixmapMut, cam_ts: Transform, a: Vec2, b: Vec2, zoom: f32) {
    let min = Vec2::new(a.x.min(b.x), a.y.min(b.y));
    let max = Vec2::new(a.x.max(b.x), a.y.max(b.y));
    let Some(rect) = Rect::from_xywh(min.x, min.y, (max.x - min.x).max(1.0), (max.y - min.y).max(1.0))
    else {
        return;
    };
    let mut fp = Paint::default();
    fp.set_color_rgba8(MARQUEE_FILL[0], MARQUEE_FILL[1], MARQUEE_FILL[2], MARQUEE_FILL[3]);
    fp.anti_alias = true;
    pixmap.fill_rect(rect, &fp, cam_ts, None);

    let mut pb = PathBuilder::new();
    pb.push_rect(rect);
    if let Some(path) = pb.finish() {
        let mut sp = Paint::default();
        sp.set_color_rgba8(MARQUEE_STROKE[0], MARQUEE_STROKE[1], MARQUEE_STROKE[2], MARQUEE_STROKE[3]);
        sp.anti_alias = true;
        let dash = (4.0 / zoom).max(1.0);
        let sk = SkStroke {
            width: 1.0 / zoom,
            dash: StrokeDash::new(vec![dash, dash], 0.0),
            ..Default::default()
        };
        pixmap.stroke_path(&path, &sp, &sk, cam_ts, None);
    }
}

fn draw_preview(pixmap: &mut PixmapMut, cam_ts: Transform, p: Preview) {
    let min = Vec2::new(p.a.x.min(p.b.x), p.a.y.min(p.b.y));
    let max = Vec2::new(p.a.x.max(p.b.x), p.a.y.max(p.b.y));
    let size = max - min;

    let mut pb = PathBuilder::new();
    match &p.kind {
        NodeKind::Frame { .. } | NodeKind::Shape(ShapeKind::Rectangle { .. }) => {
            if let Some(rect) = Rect::from_xywh(min.x, min.y, size.x, size.y) {
                pb.push_rect(rect);
            }
        }
        NodeKind::Shape(ShapeKind::Ellipse { .. }) => {
            if let Some(rect) = Rect::from_xywh(min.x, min.y, size.x, size.y) {
                pb.push_oval(rect);
            }
        }
        NodeKind::Shape(ShapeKind::Line { .. }) => {
            pb.move_to(p.a.x, p.a.y);
            pb.line_to(p.b.x, p.b.y);
        }
        _ => {}
    }
    if let Some(path) = pb.finish() {
        let mut fp = Paint::default();
        fp.set_color_rgba8(PREVIEW_FILL[0], PREVIEW_FILL[1], PREVIEW_FILL[2], PREVIEW_FILL[3]);
        fp.anti_alias = true;
        pixmap.fill_path(&path, &fp, FillRule::Winding, cam_ts, None);

        let mut sp = Paint::default();
        sp.set_color_rgba8(PREVIEW_STROKE[0], PREVIEW_STROKE[1], PREVIEW_STROKE[2], PREVIEW_STROKE[3]);
        sp.anti_alias = true;
        let sk = SkStroke { width: 1.0, ..Default::default() };
        pixmap.stroke_path(&path, &sp, &sk, cam_ts, None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::model::types::Color as ModelColor;

    fn render_scene(scene: &SceneGraph) -> Vec<u8> {
        let (w, h) = (160u32, 120u32);
        let mut out = vec![0u8; (w * h * 4) as usize];
        TinySkiaRenderer::new().render(
                scene,
                &Camera::new(),
                w,
                h,
                &[],
                GridConfig::default(),
                None,
                None,
                None,
                &mut out,
            );
        out
    }

    fn pixel(buf: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * w + x) * 4) as usize;
        [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
    }

    #[test]
    fn plain_rect_renders_via_fill_rect_fast_path() {
        // Обычный прямоугольник без скругления: fill_rect без PathBuilder.
        let mut s = SceneGraph::new();
        let k = s.insert_root(
            "R",
            NodeKind::Shape(ShapeKind::Rectangle {
                size: Vec2::new(40.0, 30.0),
                corner_radii: [0.0; 4],
            }),
        );
        if let Some(n) = s.get_mut(k) {
            n.fills.push(ModelPaint::Solid(ModelColor::from_rgba8(255, 0, 0, 255)));
        }
        s.mark_subtree_dirty(k);
        s.flush_transforms();
        let buf = render_scene(&s);
        // Центр прямоугольника (20,15) — красный (маркер начала координат
        // перекрывает лишь x<6,y<6, так что центр чист).
        assert_eq!(pixel(&buf, 160, 20, 15), [255, 0, 0, 255]);
        // За пределами — страница (закрывает весь вьюпорт), не фон канваса.
        assert_eq!(pixel(&buf, 160, 100, 100), PAGE_BG);
    }

    #[test]
    fn rounded_rect_renders_via_path() {
        let mut s = SceneGraph::new();
        let k = s.insert_root(
            "R",
            NodeKind::Shape(ShapeKind::Rectangle {
                size: Vec2::new(40.0, 30.0),
                corner_radii: [8.0; 4],
            }),
        );
        if let Some(n) = s.get_mut(k) {
            n.fills.push(ModelPaint::Solid(ModelColor::from_rgba8(0, 255, 0, 255)));
        }
        s.mark_subtree_dirty(k);
        s.flush_transforms();
        let buf = render_scene(&s);
        // Центр — зелёный; точка ниже нижней кромки (вне узла, вне маркера) — страница.
        assert_eq!(pixel(&buf, 160, 20, 15), [0, 255, 0, 255]);
        assert_eq!(pixel(&buf, 160, 20, 31), PAGE_BG);
    }

    #[test]
    fn opacity_applied_to_fill() {
        let mut s = SceneGraph::new();
        let k = s.insert_root(
            "R",
            NodeKind::Shape(ShapeKind::Rectangle {
                size: Vec2::new(40.0, 30.0),
                corner_radii: [0.0; 4],
            }),
        );
        if let Some(n) = s.get_mut(k) {
            n.fills.push(ModelPaint::Solid(ModelColor::from_rgba8(255, 0, 0, 255)));
            n.opacity = 0.5;
        }
        s.mark_subtree_dirty(k);
        s.flush_transforms();
        let buf = render_scene(&s);
        // Красный при 50% поверх тёмной страницы: цвет — смесь, альфа буфера 255.
        let [r, g, b, a] = pixel(&buf, 160, 20, 15);
        assert_eq!(a, 255, "alpha = {a}");
        assert!((130..=160).contains(&r) && g < 30 && b < 30, "rgb = {r},{g},{b}");
    }

    #[test]
    fn text_node_renders_glyphs() {
        // Текстовый узел: буква "I" (Inter) рисуется жирным чёрным штрихом.
        let mut s = SceneGraph::new();
        let k = s.insert_root(
            "T",
            NodeKind::Text {
                content: "I".into(),
                font_family: "Inter".into(),
                font_size: 32.0,
                font_weight: 400,
                line_height: 1.2,
                letter_spacing: 0.0,
                align: crate::engine::model::types::TextAlign::Left,
                size_mode: crate::engine::model::types::TextSizeMode::AutoWidth,
            },
        );
        if let Some(n) = s.get_mut(k) {
            n.fills.push(ModelPaint::Solid(ModelColor::from_rgba8(0, 0, 0, 255)));
        }
        s.mark_subtree_dirty(k);
        s.flush_transforms();
        let buf = render_scene(&s);
        // "I" в Inter — вертикальный штрих ~ по центру x. Ищем его по колонке
        // в середине bbox: должны встретиться чёрные пиксели (глиф залит).
        let (_, mx) = s.world_bbox(k).unwrap();
        let cx = (mx.x * 0.5).round() as u32;
        let mut dark = 0;
        for y in 8..(mx.y.round() as u32) {
            let p = pixel(&buf, 160, cx, y);
            if p[0] < 40 && p[1] < 40 && p[2] < 40 {
                dark += 1;
            }
        }
        assert!(dark > 4, "тёмных пикселей в колонке глифа: {dark}");
    }

    #[test]
    fn transparent_fill_without_stroke_is_invisible() {
        // Заливка прозрачна (alpha 0) и обводки нет: узел не рисуется вовсе
        // (раньше такой узел рисовал невидимую фигуру, затирая то, что под ним).
        let mut s = SceneGraph::new();
        let k = s.insert_root(
            "R",
            NodeKind::Shape(ShapeKind::Rectangle {
                size: Vec2::new(40.0, 30.0),
                corner_radii: [0.0; 4],
            }),
        );
        if let Some(n) = s.get_mut(k) {
            n.fills.push(ModelPaint::Solid(ModelColor::from_rgba8(255, 0, 0, 0)));
        }
        s.mark_subtree_dirty(k);
        s.flush_transforms();
        let buf = render_scene(&s);
        assert_eq!(pixel(&buf, 160, 20, 15), PAGE_BG);
    }

    #[test]
    fn transparent_fill_still_shows_stroke() {
        // Прозрачная заливка + видимая обводка: узел рисуется, но только обводкой.
        let mut s = SceneGraph::new();
        let k = s.insert_root(
            "R",
            NodeKind::Shape(ShapeKind::Rectangle {
                size: Vec2::new(40.0, 30.0),
                corner_radii: [0.0; 4],
            }),
        );
        if let Some(n) = s.get_mut(k) {
            n.fills.push(ModelPaint::Solid(ModelColor::from_rgba8(255, 0, 0, 0)));
            n.strokes.push(crate::engine::model::types::Stroke::solid(ModelColor::BLACK, 8.0));
        }
        s.mark_subtree_dirty(k);
        s.flush_transforms();
        let buf = render_scene(&s);
        // Центр — прозрачная заливка → страница; у левой кромки — чёрная обводка.
        assert_eq!(pixel(&buf, 160, 20, 15), PAGE_BG);
        let [r, g, b, a] = pixel(&buf, 160, 2, 15);
        assert_eq!(a, 255);
        assert!(
            r < 26 && g < 26 && b < 26,
            "обводка должна затемнять страницу: {r},{g},{b}"
        );
    }

    #[test]
    fn stroke_alpha_respects_node_opacity() {
        // Alpha обводки умножается на opacity узла: 50% чёрного поверх тёмной
        // страницы — заметно темнее, но не чисто чёрная.
        let mut s = SceneGraph::new();
        let k = s.insert_root(
            "R",
            NodeKind::Shape(ShapeKind::Rectangle {
                size: Vec2::new(40.0, 30.0),
                corner_radii: [0.0; 4],
            }),
        );
        if let Some(n) = s.get_mut(k) {
            n.fills.push(ModelPaint::Solid(ModelColor::from_rgba8(255, 0, 0, 0)));
            n.strokes.push(crate::engine::model::types::Stroke::solid(ModelColor::BLACK, 8.0));
            n.opacity = 0.5;
        }
        s.mark_subtree_dirty(k);
        s.flush_transforms();
        let buf = render_scene(&s);
        let [r, g, b, a] = pixel(&buf, 160, 2, 15);
        assert_eq!(a, 255);
        assert!(
            r > 0 && r < 20 && g > 0 && g < 20 && b > 0 && b < 20,
            "rgb = {r},{g},{b}"
        );
    }

    #[test]
    fn path_cache_reused_and_invalidated_on_geometry_change() {
        let mut s = SceneGraph::new();
        let k = s.insert_root(
            "R",
            NodeKind::Shape(ShapeKind::Rectangle {
                size: Vec2::new(40.0, 30.0),
                corner_radii: [8.0; 4],
            }),
        );
        if let Some(n) = s.get_mut(k) {
            n.fills.push(ModelPaint::Solid(ModelColor::from_rgba8(255, 0, 0, 255)));
        }
        s.mark_subtree_dirty(k);
        s.flush_transforms();
        let mut r = TinySkiaRenderer::new();
        let (w, h) = (160u32, 120u32);
        let mut out = vec![0u8; (w * h * 4) as usize];
        r.render(&s, &Camera::new(), w, h, &[], GridConfig::default(), None, None, None, &mut out);
        let first = out.clone();
        // Повторный рендер без изменений: кэш попадает, вывод идентичен.
        r.render(&s, &Camera::new(), w, h, &[], GridConfig::default(), None, None, None, &mut out);
        assert_eq!(out, first);
        // Меняем геометрию (радиус скругления) — хэш меняется, кэш инвалидируется.
        if let Some(n) = s.get_mut(k) {
            n.kind = NodeKind::Shape(ShapeKind::Rectangle {
                size: Vec2::new(40.0, 30.0),
                corner_radii: [0.0; 4],
            });
        }
        s.mark_subtree_dirty(k);
        s.flush_transforms();
        r.render(&s, &Camera::new(), w, h, &[], GridConfig::default(), None, None, None, &mut out);
        assert_ne!(out, first);
    }
}