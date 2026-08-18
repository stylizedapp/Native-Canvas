use super::{
    rects_intersect, world_bbox, RenderOutcome, Renderer, CANVAS_BG, GIZMO_HANDLE_FILL,
    GIZMO_HANDLE_STROKE, GRID, MARQUEE_FILL, MARQUEE_STROKE, ORIGIN, PAGE_BG, PAGE_BORDER,
    PREVIEW_FILL, PREVIEW_STROKE, SELECTION, PAGE_SIZE,
};
use super::super::grid::GridConfig;
use super::super::model::nodes::{NodeKey, NodeKind, ShapeKind};
use super::super::model::scene::{SceneGraph, SceneNode};
use super::super::model::types::Paint as ModelPaint;
use super::super::profiler::FrameMetrics;
use super::super::transform::Camera;
use crate::engine::controller::Preview;
use crate::engine::gizmo;
use glam::{Affine2, Vec2};
use std::time::Instant;
use tiny_skia::{
    Color, FillRule, Paint, PathBuilder, PixmapMut, Rect, Stroke as SkStroke, StrokeDash, Transform,
};

/// Бэкенд на tiny-skia (CPU-растеризация) — фолбэк, когда GPU недоступен.
pub struct TinySkiaRenderer;

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
        out: &mut [u8],
    ) -> RenderOutcome {
        let t0 = Instant::now();
        let mut pixmap =
            PixmapMut::from_bytes(out, width, height).expect("pixmap");
        pixmap.fill(Color::from_rgba8(CANVAS_BG[0], CANVAS_BG[1], CANVAS_BG[2], CANVAS_BG[3]));

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

        // Сетка — один draw-call, только видимая область.
        if grid.visible {
            draw_grid(&mut pixmap, cam_ts, camera, view_min, view_max, grid.step);
        }

        // Однопроходный обход дерева с накопленной трансформацией (O(n)).
        let mut stack: Vec<(NodeKey, Affine2)> = scene
            .roots()
            .iter()
            .rev()
            .map(|&key| (key, Affine2::IDENTITY))
            .collect();

        while let Some((key, acc)) = stack.pop() {
            let Some(node) = scene.get(key) else { continue };
            if !node.is_visible {
                continue;
            }
            let world = acc * node.local_transform;
            let (mn, mx) = world_bbox(&node.kind, world);
            if rects_intersect(mn, mx, view_min, view_max) {
                draw_node(node, world, cam_ts, camera.zoom, &mut pixmap);
            }
            stack.extend(node.children.iter().rev().map(|&ch| (ch, world)));
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
/// Все линии в одном пути — один draw-call.
fn draw_grid(pixmap: &mut PixmapMut, cam_ts: Transform, camera: &Camera, view_min: Vec2, view_max: Vec2, step: f32) {
    let mut s = step.max(1.0);
    while s * camera.zoom < 8.0 {
        s *= 2.0;
    }

    let x0 = (view_min.x / s).floor();
    let x1 = (view_max.x / s).ceil();
    let y0 = (view_min.y / s).floor();
    let y1 = (view_max.y / s).ceil();

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

    let mut sp = Paint::default();
    sp.set_color_rgba8(GRID[0], GRID[1], GRID[2], GRID[3]);
    sp.anti_alias = false;
    let sk = SkStroke { width: 1.0 / camera.zoom, ..Default::default() };
    pixmap.stroke_path(&path, &sp, &sk, cam_ts, None);
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

fn draw_node(node: &SceneNode, world: Affine2, cam_ts: Transform, zoom: f32, pixmap: &mut PixmapMut) {
    let combined = cam_ts.pre_concat(Transform::from_row(
        world.matrix2.x_axis.x,
        world.matrix2.x_axis.y,
        world.matrix2.y_axis.x,
        world.matrix2.y_axis.y,
        world.translation.x,
        world.translation.y,
    ));

    let mut pb = PathBuilder::new();
    match &node.kind {
        NodeKind::Frame { size, corner_radii, .. }
        | NodeKind::Shape(ShapeKind::Rectangle { size, corner_radii }) => {
            push_rounded_rect(&mut pb, *size, *corner_radii);
        }
        NodeKind::Shape(ShapeKind::Ellipse { radii, .. }) => {
            if let Some(rect) = Rect::from_xywh(0.0, 0.0, radii.x, radii.y) {
                pb.push_oval(rect);
            }
        }
        NodeKind::Shape(ShapeKind::Line { start, end }) => {
            pb.move_to(start.x, start.y);
            pb.line_to(end.x, end.y);
        }
        _ => return,
    }

    let Some(path) = pb.finish() else { return };

    // Заливка (первая сплошная; градиенты пока игнорируются).
    if let Some(f) = solid_fill(node) {
        if node.opacity > 0.0 {
            let a = (f[3] as f32 * node.opacity).round() as u8;
            let mut paint = Paint::default();
            paint.set_color_rgba8(f[0], f[1], f[2], a);
            paint.anti_alias = true;
            pixmap.fill_path(&path, &paint, FillRule::Winding, combined, None);
        }
    }
    // Обводка.
    if let Some(st) = node.strokes.first() {
        if st.width > 0.0 {
            if let ModelPaint::Solid(c) = &st.paint {
                let sc8 = c.to_rgba8();
                let scale = world.matrix2.x_axis.length().max(world.matrix2.y_axis.length());
                let mut sp = Paint::default();
                sp.set_color_rgba8(sc8[0], sc8[1], sc8[2], sc8[3]);
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
                pixmap.stroke_path(&path, &sp, &sk, combined, None);
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