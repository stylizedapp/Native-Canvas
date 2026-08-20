//! Spatial hash grid для быстрого hit-теста.
//!
//! Хранит мировые AABB узлов по ячейкам фиксированного размера. Поддерживает
//! точечные и прямоугольные запросы за O(ячеек запроса), а не O(N) обход дерева.
//! Обновляется инкрементально: при пересчёте мировой трансформации узел
//! перерегистрируется из старых ячеек в новые.

use crate::engine::model::nodes::NodeKey;
use crate::engine::model::scene::SceneGraph;
use glam::Vec2;
use std::collections::{HashMap, HashSet};

/// Размер ячейки сетки в мировых координатах.
pub const CELL_SIZE: f32 = 256.0;

/// Индекс узлов по мировым ограничивающим рамкам.
#[derive(Clone, Debug, Default)]
pub struct SpatialIndex {
    /// координата ячейки -> ключи узлов, чья рамка её покрывает
    cells: HashMap<(i32, i32), Vec<NodeKey>>,
    /// узел -> ячейки, которые он сейчас занимает (для перерегистрации)
    node_cells: HashMap<NodeKey, Vec<(i32, i32)>>,
}

/// Список ячеек, покрывающих прямоугольник `mn..mx`.
fn cell_range(mn: Vec2, mx: Vec2, cs: f32) -> Vec<(i32, i32)> {
    let x0 = (mn.x / cs).floor() as i32;
    let x1 = (mx.x / cs).floor() as i32;
    let y0 = (mn.y / cs).floor() as i32;
    let y1 = (mx.y / cs).floor() as i32;
    let mut out = Vec::with_capacity(((x1 - x0 + 1) * (y1 - y0 + 1)).max(1) as usize);
    for cy in y0..=y1 {
        for cx in x0..=x1 {
            out.push((cx, cy));
        }
    }
    out
}

impl SpatialIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Регистрирует узел по его мировому bbox.
    pub fn insert(&mut self, key: NodeKey, mn: Vec2, mx: Vec2) {
        let cells = cell_range(mn, mx, CELL_SIZE);
        for c in &cells {
            self.cells.entry(*c).or_default().push(key);
        }
        self.node_cells.insert(key, cells);
    }

    /// Перерегистрация после перемещения: убирает из старых ячеек, кладёт в новые.
    pub fn update(&mut self, key: NodeKey, old_mn: Vec2, old_mx: Vec2, new_mn: Vec2, new_mx: Vec2) {
        let old = cell_range(old_mn, old_mx, CELL_SIZE);
        let new = cell_range(new_mn, new_mx, CELL_SIZE);
        let old_set: HashSet<(i32, i32)> = old.iter().copied().collect();
        let new_set: HashSet<(i32, i32)> = new.iter().copied().collect();

        let mut empty = Vec::new();
        for c in &old_set {
            if !new_set.contains(c) {
                if let Some(v) = self.cells.get_mut(c) {
                    v.retain(|&k| k != key);
                    if v.is_empty() {
                        empty.push(*c);
                    }
                }
            }
        }
        for c in empty {
            self.cells.remove(&c);
        }
        for c in &new_set {
            if !old_set.contains(c) {
                self.cells.entry(*c).or_default().push(key);
            }
        }
        self.node_cells.insert(key, new_set.into_iter().collect());
    }

    /// Убирает узел из индекса.
    pub fn remove(&mut self, key: NodeKey) {
        if let Some(cells) = self.node_cells.remove(&key) {
            for c in cells {
                if let Some(v) = self.cells.get_mut(&c) {
                    v.retain(|&k| k != key);
                    if v.is_empty() {
                        self.cells.remove(&c);
                    }
                }
            }
        }
    }

    /// Кандидаты под точкой: ключи узлов, чей bbox покрывает ячейку точки.
    pub fn query_point(&self, p: Vec2) -> Vec<NodeKey> {
        let cx = (p.x / CELL_SIZE).floor() as i32;
        let cy = (p.y / CELL_SIZE).floor() as i32;
        self.cells.get(&(cx, cy)).cloned().unwrap_or_default()
    }

    /// Кандидаты, чей bbox пересекает прямоугольник (без дубликатов).
    pub fn query_rect(&self, mn: Vec2, mx: Vec2) -> Vec<NodeKey> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for c in cell_range(mn, mx, CELL_SIZE) {
            if let Some(v) = self.cells.get(&c) {
                for &k in v {
                    if seen.insert(k) {
                        out.push(k);
                    }
                }
            }
        }
        out
    }
}

/// Путь узла от корня: индексы в списках детей на каждом уровне (корень —
/// индекс в `roots`). Лексикографическое сравнение рангов даёт порядок
/// отрисовки: меньший ранг рисуется раньше, потомок всегда больше предка.
pub fn paint_rank(scene: &SceneGraph, key: NodeKey) -> Option<Vec<u32>> {
    let mut path = Vec::new();
    let mut cur = key;
    loop {
        let n = scene.get(cur)?;
        match n.parent {
            Some(p) => {
                let idx = scene.get(p)?.children.iter().position(|&c| c == cur)? as u32;
                path.push(idx);
                cur = p;
            }
            None => {
                let idx = scene.roots().iter().position(|&r| r == cur)? as u32;
                path.push(idx);
                break;
            }
        }
    }
    path.reverse();
    Some(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::model::nodes::{NodeKind, ShapeKind};

    fn rect_key(g: &mut SceneGraph, parent: Option<NodeKey>, x: f32, y: f32, w: f32, h: f32) -> NodeKey {
        let kind = NodeKind::Shape(ShapeKind::Rectangle {
            size: Vec2::new(w, h),
            corner_radii: [0.0; 4],
        });
        let k = match parent {
            Some(p) => g.insert_child(p, "R", kind).unwrap(),
            None => g.insert_root("R", kind),
        };
        if let Some(n) = g.get_mut(k) {
            n.local_transform = glam::Affine2::from_translation(Vec2::new(x, y));
        }
        g.mark_subtree_dirty(k);
        k
    }

    #[test]
    fn grid_query_point_finds_overlapping_node() {
        let mut g = SceneGraph::new();
        let a = rect_key(&mut g, None, 10.0, 10.0, 100.0, 100.0);
        g.flush_transforms();
        assert!(g.spatial_query_point(Vec2::new(50.0, 50.0)).contains(&a));
        // Сетка — грубый префильтр по ячейкам: точка в другой ячейке пуста.
        assert!(g.spatial_query_point(Vec2::new(2000.0, 2000.0)).is_empty());
    }

    #[test]
    fn grid_update_after_move() {
        let mut g = SceneGraph::new();
        let a = rect_key(&mut g, None, 10.0, 10.0, 10.0, 10.0);
        g.flush_transforms();
        // Точка за пределами исходного bbox.
        assert!(g.spatial_query_point(Vec2::new(1000.0, 1000.0)).is_empty());
        // Перемещаем узел в (1000,1000) — grid должен перерегистрироваться.
        if let Some(n) = g.get_mut(a) {
            n.local_transform = glam::Affine2::from_translation(Vec2::new(1000.0, 1000.0));
        }
        g.mark_subtree_dirty(a);
        g.flush_transforms();
        assert!(g.spatial_query_point(Vec2::new(1005.0, 1005.0)).contains(&a));
        assert!(g.spatial_query_point(Vec2::new(15.0, 15.0)).is_empty());
    }

    #[test]
    fn grid_query_rect_dedups() {
        let mut g = SceneGraph::new();
        let a = rect_key(&mut g, None, 0.0, 0.0, 600.0, 600.0);
        g.flush_transforms();
        let hits = g.spatial_query_rect(Vec2::new(0.0, 0.0), Vec2::new(600.0, 600.0));
        assert_eq!(hits.iter().filter(|&&k| k == a).count(), 1);
    }

    #[test]
    fn grid_remove_clears_cells() {
        let mut g = SceneGraph::new();
        let a = rect_key(&mut g, None, 0.0, 0.0, 50.0, 50.0);
        g.flush_transforms();
        assert!(g.spatial_query_point(Vec2::new(10.0, 10.0)).contains(&a));
        g.remove(a);
        assert!(g.spatial_query_point(Vec2::new(10.0, 10.0)).is_empty());
    }

    #[test]
    fn paint_rank_orders_paint_stack() {
        let mut g = SceneGraph::new();
        let a = g.insert_root("A", NodeKind::Group);
        let b = g.insert_child(a, "B", NodeKind::Group).unwrap();
        let d = g.insert_child(b, "D", rect_kind_local()).unwrap();
        let c = g.insert_child(a, "C", NodeKind::Group).unwrap();
        let e = g.insert_child(c, "E", rect_kind_local()).unwrap();

        let mut ranks: Vec<(Vec<u32>, NodeKey)> = [a, b, d, c, e]
            .into_iter()
            .map(|k| (paint_rank(&g, k).unwrap(), k))
            .collect();
        // Отрисовка: A, B, D, C, E. Топ-мост (последний нарисован) — E, C, D, B, A.
        ranks.sort_by(|x, y| y.0.cmp(&x.0));
        let order: Vec<NodeKey> = ranks.into_iter().map(|(_, k)| k).collect();
        assert_eq!(order, vec![e, c, d, b, a]);
    }

    fn rect_kind_local() -> NodeKind {
        NodeKind::Shape(ShapeKind::Rectangle {
            size: Vec2::new(5.0, 5.0),
            corner_radii: [0.0; 4],
        })
    }

    #[test]
    fn marquee_spatial_matches_reference_walk() {
        use crate::engine::gizmo::aabb_intersect;
        let mut g = SceneGraph::new();
        // Несколько узлов разных размеров/позиций, включая скрытый и выходящий
        // за марки (но пересекающий её bbox), и глубокий ребёнок.
        let bg = rect_key(&mut g, None, -50.0, -50.0, 400.0, 400.0);
        let a = rect_key(&mut g, None, 10.0, 10.0, 100.0, 100.0);
        let b = rect_key(&mut g, Some(a), 20.0, 20.0, 30.0, 30.0);
        let hidden = rect_key(&mut g, None, 0.0, 0.0, 20.0, 20.0);
        if let Some(n) = g.get_mut(hidden) {
            n.is_visible = false;
        }
        g.mark_subtree_dirty(hidden);
        let far = rect_key(&mut g, None, 5000.0, 5000.0, 10.0, 10.0);
        g.flush_transforms();

        let marquee = (Vec2::new(0.0, 0.0), Vec2::new(150.0, 150.0));

        // Логика контроллера: кандидаты из сетки + видимость + пересечение + сорт.
        let mut spatial: Vec<NodeKey> = g
            .spatial_query_rect(marquee.0, marquee.1)
            .into_iter()
            .filter(|k| {
                g.get(*k).map(|n| n.is_visible).unwrap_or(false)
                    && g.world_bbox(*k)
                        .map(|(n0, n1)| aabb_intersect(marquee.0, marquee.1, n0, n1))
                        .unwrap_or(false)
            })
            .collect();
        spatial.sort_by_cached_key(|k| paint_rank(&g, *k));

        // Эталон: полный обход в порядке отрисовки.
        let reference: Vec<NodeKey> = g
            .walk()
            .into_iter()
            .filter(|k| {
                g.get(*k).map(|n| n.is_visible).unwrap_or(false)
                    && g.world_bbox(*k)
                        .map(|(n0, n1)| aabb_intersect(marquee.0, marquee.1, n0, n1))
                        .unwrap_or(false)
            })
            .collect();

        assert_eq!(spatial, reference, "spatial marquee mismatch");
        // Скрытый и дальний не должны попадать.
        assert!(!spatial.contains(&hidden) && !spatial.contains(&far));
        assert!(spatial.contains(&a) && spatial.contains(&b) && spatial.contains(&bg));
    }
}