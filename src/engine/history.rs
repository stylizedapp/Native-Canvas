use super::model::scene::SceneGraph;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::model::nodes::{NodeKind, ShapeKind};
    use glam::Vec2;

    fn rect() -> NodeKind {
        NodeKind::Shape(ShapeKind::Rectangle { size: Vec2::new(100.0, 60.0), corner_radii: [0.0; 4] })
    }

    #[test]
    fn snapshot_clone_cost_on_large_scene() {
        // 1000 узлов: дерево глубиной ~3. Замер полного клона (как в record/undo).
        let mut g = SceneGraph::new();
        let mut parents = Vec::new();
        let root = g.insert_root("r", NodeKind::Frame { size: Vec2::new(2000.0, 2000.0), auto_layout: None, clip_content: false, corner_radii: [0.0; 4], constraints: crate::engine::model::types::Constraints::default() });
        parents.push(root);
        for i in 0..20 {
            let p = g.insert_child(parents[i % parents.len()], &format!("p{i}"), NodeKind::Group).unwrap();
            parents.push(p);
        }
        let mut leaves = Vec::new();
        for i in 0..1000 {
            let p = parents[i % parents.len()];
            leaves.push(g.insert_child(p, &format!("n{i}"), rect()).unwrap());
        }
        g.flush_transforms();

        // Холодный клон.
        let t0 = std::time::Instant::now();
        let mut total = 0usize;
        for _ in 0..100 {
            let clone = g.clone();
            total += clone.len();
        }
        let per_clone = t0.elapsed().as_micros() as f64 / 100.0;
        eprintln!("[bench] scene={} nodes, full clone = {per_clone:.0} us, 100 clones touched {total} nodes", g.len());
        // Только диагностика: порог не ставим (тайминги флакают под нагрузкой CI).
    }

    #[test]
    fn undo_redo_roundtrip_preserves_states() {
        // Семантика как у старых снапшотов: record(до) -> мутация -> undo -> redo.
        fn make(n: &str, x: f32) -> SceneGraph {
            let mut g = SceneGraph::new();
            let k = g.insert_root(n, rect());
            if let Some(node) = g.get_mut(k) {
                node.local_transform = glam::Affine2::from_translation(Vec2::new(x, 0.0));
            }
            g.mark_subtree_dirty(k);
            g.flush_transforms();
            g
        }
        let mut h = History::new(10);
        let mut live = make("a", 1.0);
        // Операция 1: record(до) -> мутация live.
        h.record(live.clone());
        live = make("b", 2.0);
        h.record(live.clone());
        live = make("c", 3.0);

        // undo -> вернулись к "b".
        assert!(h.undo(&mut live));
        let name = live.get(live.roots()[0]).map(|n| n.name.as_str()).unwrap_or("");
        assert_eq!(name, "b");
        assert!(h.can_redo());

        // undo -> "a".
        assert!(h.undo(&mut live));
        assert_eq!(live.get(live.roots()[0]).map(|n| n.name.as_str()).unwrap_or(""), "a");
        assert!(!h.can_undo());

        // redo -> "b", redo -> "c".
        assert!(h.redo(&mut live));
        assert_eq!(live.get(live.roots()[0]).map(|n| n.name.as_str()).unwrap_or(""), "b");
        assert!(h.redo(&mut live));
        assert_eq!(live.get(live.roots()[0]).map(|n| n.name.as_str()).unwrap_or(""), "c");
        assert!(!h.can_redo());

        // Новое изменение чистит redo-ветку.
        h.record(live.clone());
        live = make("d", 4.0);
        assert!(!h.can_redo());
        assert!(h.undo(&mut live));
        assert_eq!(live.get(live.roots()[0]).map(|n| n.name.as_str()).unwrap_or(""), "c");
    }

    #[test]
    fn undo_redo_no_op_when_empty() {
        let mut h = History::new(10);
        let mut live = SceneGraph::new();
        assert!(!h.undo(&mut live));
        assert!(!h.redo(&mut live));
    }
}

/// История операций на основе снапшотов графа сцены.
///
/// `record()` принимает клон состояния ДО изменения (один клон на операцию —
/// неустранимо при снапшотном подходе). Откат/повтор (`undo`/`redo`) обменивают
/// состояния через `mem::swap` — без клонирования и аллокаций: цена отката
/// O(1), а не O(N) как при полном клоне. Память ограничена глубиной `limit`
/// (redo-ветка инвалидируется при новом изменении).
#[derive(Default)]
pub struct History {
    undo: Vec<SceneGraph>,
    redo: Vec<SceneGraph>,
    /// Ограничение глубины истории.
    limit: usize,
}

impl History {
    pub fn new(limit: usize) -> Self {
        Self { undo: Vec::new(), redo: Vec::new(), limit }
    }

    /// Вызывается ДО внесения изменения: сохраняет состояние до изменения.
    pub fn record(&mut self, before: SceneGraph) {
        if self.undo.len() >= self.limit && self.limit > 0 {
            self.undo.remove(0);
        }
        self.undo.push(before);
        // Новое изменение инвалидирует redo-ветку.
        self.redo.clear();
    }

    /// Откат: `current` заменяется предыдущим состоянием (swap, без клона).
    /// Возвращает `true`, если откат произошёл.
    pub fn undo(&mut self, current: &mut SceneGraph) -> bool {
        let Some(mut prev) = self.undo.pop() else { return false };
        std::mem::swap(current, &mut prev);
        self.redo.push(prev);
        true
    }

    /// Повтор: `current` заменяется следующим состоянием (swap, без клона).
    /// Возвращает `true`, если повтор произошёл.
    pub fn redo(&mut self, current: &mut SceneGraph) -> bool {
        let Some(mut next) = self.redo.pop() else { return false };
        std::mem::swap(current, &mut next);
        self.undo.push(next);
        true
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
}