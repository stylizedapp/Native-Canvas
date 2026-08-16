use super::{
    rects_intersect, world_bbox, Renderer, CANVAS_BG, GRID, ORIGIN, PAGE_BG, PAGE_BORDER,
    PREVIEW_FILL, PREVIEW_STROKE, SELECTION,
};
use super::super::grid::GridConfig;
use super::super::scene::{NodeKind, Scene, SceneNode, NodeId, PAGE_SIZE};
use super::super::transform::Camera;
use crate::engine::controller::Preview;
use glam::{Affine2, Vec2};
use tiny_skia::{Color, FillRule, Paint, PathBuilder, PixmapMut, Rect, Stroke as SkStroke, Transform};

/// Бэкенд на tiny-skia (CPU-растеризация) — фолбэк, когда GPU недоступен.
pub struct TinySkiaRenderer;

impl Renderer for TinySkiaRenderer {
    fn name(&self) -> &'static str {
        "tiny-skia (CPU)"
    }

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
    ) -> bool {
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
        let mut stack: Vec<(NodeId, Affine2)> = scene
            .roots
            .iter()
            .rev()
            .map(|&id| (id, Affine2::IDENTITY))
            .collect();

        while let Some((id, acc)) = stack.pop() {
            let Some(node) = scene.get(id) else { continue };
            if !node.visible {
                continue;
            }
            let world = acc * node.transform;
            let (mn, mx) = world_bbox(&node.kind, world);
            if rects_intersect(mn, mx, view_min, view_max) {
                draw_node(node, world, cam_ts, &mut pixmap);
            }
            stack.extend(node.children.iter().rev().map(|&ch| (ch, world)));
        }

        // Подсветка выделенных узлов (обводка ограничивающей рамки).
        for id in selected {
            if let Some((mn, mx)) = scene.world_bbox(*id) {
                draw_bbox_highlight(&mut pixmap, cam_ts, mn, mx);
            }
        }

        // Маркер начала координат (0,0) — для визуальной проверки пана/зума.
        draw_origin(&mut pixmap, cam_ts, camera.zoom);

        // Live-превью создаваемой фигуры.
        if let Some(p) = preview {
            draw_preview(&mut pixmap, cam_ts, p);
        }
        true
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

fn draw_node(node: &SceneNode, world: Affine2, cam_ts: Transform, pixmap: &mut PixmapMut) {
    let combined = cam_ts.pre_concat(Transform::from_row(
        world.matrix2.x_axis.x,
        world.matrix2.x_axis.y,
        world.matrix2.y_axis.x,
        world.matrix2.y_axis.y,
        world.translation.x,
        world.translation.y,
    ));

    let mut pb = PathBuilder::new();
    match node.kind {
        NodeKind::Frame { w, h } | NodeKind::Rectangle { w, h } => {
            if let Some(rect) = Rect::from_xywh(0.0, 0.0, w, h) {
                pb.push_rect(rect);
            }
        }
        NodeKind::Ellipse { w, h } => {
            if let Some(rect) = Rect::from_xywh(0.0, 0.0, w, h) {
                pb.push_oval(rect);
            }
        }
        NodeKind::Line { x2, y2 } => {
            pb.move_to(0.0, 0.0);
            pb.line_to(x2, y2);
        }
        NodeKind::Group | NodeKind::Vector => return,
    }

    let Some(path) = pb.finish() else { return };

    // Заливка.
    if let Some(f) = node.fill {
        if node.opacity > 0.0 {
            let a = (f.color[3] as f32 * node.opacity).round() as u8;
            let mut paint = Paint::default();
            paint.set_color_rgba8(f.color[0], f.color[1], f.color[2], a);
            paint.anti_alias = true;
            pixmap.fill_path(&path, &paint, FillRule::Winding, combined, None);
        }
    }
    // Обводка.
    if let Some(st) = node.stroke {
        if st.width > 0.0 {
            let scale = world.matrix2.x_axis.length().max(world.matrix2.y_axis.length());
            let mut sp = Paint::default();
            sp.set_color_rgba8(st.color[0], st.color[1], st.color[2], st.color[3]);
            sp.anti_alias = true;
            let sk = SkStroke {
                width: st.width * scale,
                miter_limit: 4.0,
                line_cap: tiny_skia::LineCap::Round,
                line_join: tiny_skia::LineJoin::Round,
                dash: None,
            };
            pixmap.stroke_path(&path, &sp, &sk, combined, None);
        }
    }
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

fn draw_preview(pixmap: &mut PixmapMut, cam_ts: Transform, p: Preview) {
    let min = Vec2::new(p.a.x.min(p.b.x), p.a.y.min(p.b.y));
    let max = Vec2::new(p.a.x.max(p.b.x), p.a.y.max(p.b.y));
    let size = max - min;

    let mut pb = PathBuilder::new();
    match p.kind {
        NodeKind::Rectangle { .. } | NodeKind::Frame { .. } => {
            if let Some(rect) = Rect::from_xywh(min.x, min.y, size.x, size.y) {
                pb.push_rect(rect);
            }
        }
        NodeKind::Ellipse { .. } => {
            if let Some(rect) = Rect::from_xywh(min.x, min.y, size.x, size.y) {
                pb.push_oval(rect);
            }
        }
        NodeKind::Line { .. } => {
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