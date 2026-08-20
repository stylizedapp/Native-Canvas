//! Микро-бенчмарки ядра (Спринт 6): запуск `native_canvas --bench`.
//!
//! Измеряют узкие места из аудита: рендер N узлов, mouse-move цикл, время
//! flush/pick, память undo. Числа печатаются в stdout для сверки.

use std::time::Instant;

use glam::{Affine2, Vec2};

use crate::engine::grid::GridConfig;
use crate::engine::model::nodes::{NodeKey, NodeKind, ShapeKind};
use crate::engine::model::scene::SceneGraph;
use crate::engine::model::types::{Color, Paint};
use crate::engine::renderers::Renderer;
use crate::engine::transform::{pick, Camera};

fn rect() -> NodeKind {
    NodeKind::Shape(ShapeKind::Rectangle { size: Vec2::new(120.0, 80.0), corner_radii: [0.0; 4] })
}

fn fill(n: &mut crate::engine::model::scene::SceneNode) {
    n.fills.push(Paint::Solid(Color::from_rgba8(91, 140, 255, 255)));
}

/// Сцена из `n` прямоугольников в сетке 20 колонок.
fn make_scene(n: usize) -> SceneGraph {
    let mut s = SceneGraph::new();
    let cols = 20usize;
    for i in 0..n {
        let k = s.insert_root(&format!("n{i}"), rect());
        if let Some(node) = s.get_mut(k) {
            let x = (i % cols) as f32 * 130.0;
            let y = (i / cols) as f32 * 90.0;
            node.local_transform = Affine2::from_translation(Vec2::new(x, y));
            fill(node);
        }
        s.mark_subtree_dirty(k);
    }
    s.flush_transforms();
    s
}

fn fmt_us(label: &str, us: f64, per: &str) {
    println!("{label:<34} {us:>10.0} us  ({per})");
}

pub fn run() {
    println!("=== native_canvas micro-benchmarks ===");
    for n in [100usize, 500, 1000] {
        let mut s = make_scene(n);
        let cam = Camera::new();

        // Рендер кадра (tiny-skia, 1280x800).
        let (w, h) = (1280u32, 800u32);
        let mut out = vec![0u8; (w * h * 4) as usize];
        let mut renderer = crate::engine::renderers::TinySkiaRenderer;
        let t = Instant::now();
        renderer.render(&s, &cam, w, h, &[], GridConfig::new(), None, None, None, &mut out);
        fmt_us(&format!("render tiny-skia {n} nodes"), t.elapsed().as_micros() as f64, "первый кадр");

        // Стабильный кадр (повторный рендер той же сцены).
        let t = Instant::now();
        for _ in 0..10 {
            renderer.render(&s, &cam, w, h, &[], GridConfig::new(), None, None, None, &mut out);
        }
        fmt_us(&format!("render tiny-skia {n} nodes"), t.elapsed().as_micros() as f64 / 10.0, "средний");

        // flush: перемещение одного узла -> пересчёт поддерева.
        let keys: Vec<NodeKey> = s.roots().to_vec();
        if let Some(k) = keys.first() {
            let t = Instant::now();
            for i in 0..100 {
                if let Some(node) = s.get_mut(*k) {
                    node.local_transform =
                        Affine2::from_translation(Vec2::new(i as f32 % 500.0, (i as f32 / 500.0) % 400.0));
                }
                s.mark_subtree_dirty(*k);
                s.flush_transforms();
            }
            fmt_us(&format!("flush_transforms {n} nodes"), t.elapsed().as_micros() as f64 / 100.0, "1 перемещение");
        }

        // pick: 200 точек по сцене.
        let t = Instant::now();
        for i in 0..200 {
            let p = Vec2::new((i as f32 * 37.0) % 2600.0, (i as f32 * 53.0) % 1800.0);
            let _ = pick(&mut s, p);
        }
        fmt_us(&format!("pick {n} nodes"), t.elapsed().as_micros() as f64 / 200.0, "1 точка");

        // mouse-move цикл: transform + flush + pick_stack + рендер.
        let t = Instant::now();
        for _ in 0..10 {
            if let Some(k) = keys.first() {
                if let Some(node) = s.get_mut(*k) {
                    node.local_transform = Affine2::from_translation(Vec2::new(700.0, 400.0));
                }
                s.mark_subtree_dirty(*k);
            }
            s.flush_transforms();
            let _ = pick(&mut s, Vec2::new(700.0, 400.0));
            renderer.render(&s, &cam, w, h, &[], GridConfig::new(), None, None, None, &mut out);
        }
        fmt_us(&format!("mouse-move cycle {n} nodes"), t.elapsed().as_micros() as f64 / 10.0, "движение + кадр");

        // Память undo: оценка снапшота (узел + имя + заливка + children).
        let per_node = std::mem::size_of::<crate::engine::model::scene::SceneNode>()
            + std::mem::size_of::<Paint>()
            + std::mem::size_of::<crate::engine::model::types::Color>();
        let snap_bytes = per_node * n;
        println!(
            "{:<34} {:>10} KB  ({} байт/узел)",
            format!("undo snapshot {n} nodes"),
            (snap_bytes as f64 / 1024.0).round() as i64,
            per_node
        );
        println!();
    }
    println!("(значения — ориентир для сверки с аудитом; на реальном железе зависят от загрузки)");
}