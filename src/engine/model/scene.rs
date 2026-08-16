//! Структура сцены: `SceneNode` и граф `SceneGraph` на арене `SlotMap`.
//!
//! `SceneNode` кэширует мировую трансформацию (`world_transform`) с флагом
//! `is_world_dirty`. Все мутации, влияющие на геометрию, помечают поддерево
//! грязным; перед чтением `world_transform`/`world_bbox` нужно вызвать
//! [`SceneGraph::flush_transforms`] (обычно — раз в кадр перед рендером).

use crate::engine::model::nodes::{NodeKey, NodeKind};
use crate::engine::model::types::{BlendMode, Effect, Paint, Stroke};
use glam::{Affine2, Vec2};
use serde::{Deserialize, Serialize};
use slotmap::SlotMap;

/// Узел дерева сцены. Идентификатор узла — его ключ в арене `SceneGraph.nodes`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SceneNode {
    pub name: String,
    pub parent: Option<NodeKey>,
    pub children: Vec<NodeKey>,

    /// Локальная трансформация относительно родителя.
    pub local_transform: Affine2,
    /// Кэшированная мировая трансформация (см. `flush_transforms`).
    pub world_transform: Affine2,
    /// Мировая трансформация требует пересчёта.
    pub is_world_dirty: bool,

    pub is_visible: bool,
    pub is_locked: bool,
    pub opacity: f32,
    pub blend_mode: BlendMode,

    pub fills: Vec<Paint>,
    pub strokes: Vec<Stroke>,
    pub effects: Vec<Effect>,

    pub kind: NodeKind,
}

impl SceneNode {
    pub fn new(name: &str, kind: NodeKind) -> Self {
        Self {
            name: name.to_string(),
            parent: None,
            children: Vec::new(),
            local_transform: Affine2::IDENTITY,
            world_transform: Affine2::IDENTITY,
            is_world_dirty: true,
            is_visible: true,
            is_locked: false,
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            fills: Vec::new(),
            strokes: Vec::new(),
            effects: Vec::new(),
            kind,
        }
    }

    /// «Контейнерный» узел: не имеет собственной геометрии.
    pub fn is_group_like(&self) -> bool {
        matches!(
            self.kind,
            NodeKind::Group | NodeKind::Component { .. } | NodeKind::BooleanGroup { .. }
        )
    }
}

/// Иерархический граф сцены на арене `SlotMap` с кэшем мировых трансформаций.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SceneGraph {
    nodes: SlotMap<NodeKey, SceneNode>,
    roots: Vec<NodeKey>,
    selection: Vec<NodeKey>,
}

impl SceneGraph {
    pub fn new() -> Self {
        Self { nodes: SlotMap::with_key(), roots: Vec::new(), selection: Vec::new() }
    }

    // --- Доступ ---

    pub fn get(&self, key: NodeKey) -> Option<&SceneNode> {
        self.nodes.get(key)
    }

    /// Прямой доступ на запись. Если меняется `local_transform` или связи,
    /// вызовите [`SceneGraph::mark_subtree_dirty`] после завершения мутации.
    pub fn get_mut(&mut self, key: NodeKey) -> Option<&mut SceneNode> {
        self.nodes.get_mut(key)
    }

    pub fn contains(&self, key: NodeKey) -> bool {
        self.nodes.contains_key(key)
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn roots(&self) -> &[NodeKey] {
        &self.roots
    }

    pub fn selection(&self) -> &[NodeKey] {
        &self.selection
    }

    pub fn iter(&self) -> impl Iterator<Item = (NodeKey, &SceneNode)> {
        self.nodes.iter()
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = (NodeKey, &mut SceneNode)> {
        self.nodes.iter_mut()
    }

    // --- Вставка / удаление ---

    /// Вставляет корневой узел.
    pub fn insert_root(&mut self, name: &str, kind: NodeKind) -> NodeKey {
        let key = self.nodes.insert(SceneNode::new(name, kind));
        self.roots.push(key);
        key
    }

    /// Вставляет узел дочерним к `parent`. Возвращает `None`, если родитель не найден.
    pub fn insert_child(&mut self, parent: NodeKey, name: &str, kind: NodeKind) -> Option<NodeKey> {
        if !self.nodes.contains_key(parent) {
            return None;
        }
        let mut node = SceneNode::new(name, kind);
        node.parent = Some(parent);
        let key = self.nodes.insert(node);
        self.nodes.get_mut(parent).expect("parent checked").children.push(key);
        Some(key)
    }

    /// Удаляет узел и всё его поддерево, восстанавливая связи родителей.
    pub fn remove(&mut self, key: NodeKey) {
        let Some(node) = self.nodes.get(key) else { return };
        // Собираем поддерево, прежде чем удалять.
        let mut stack = vec![key];
        let mut subtree = Vec::new();
        while let Some(k) = stack.pop() {
            subtree.push(k);
            if let Some(n) = self.nodes.get(k) {
                stack.extend(n.children.iter().copied());
            }
        }
        // Отвязать от родителя/корней.
        match node.parent {
            Some(p) => {
                if let Some(pn) = self.nodes.get_mut(p) {
                    pn.children.retain(|&c| c != key);
                }
            }
            None => self.roots.retain(|&r| r != key),
        }
        for k in &subtree {
            self.nodes.remove(*k);
        }
        self.selection.retain(|s| !subtree.contains(s));
    }

    /// Переносит узел в другое место (или в корни при `None`).
    /// Отвергает перенос в собственного потомка.
    pub fn reparent(&mut self, key: NodeKey, new_parent: Option<NodeKey>) {
        if !self.nodes.contains_key(key) {
            return;
        }
        if let Some(p) = new_parent {
            if !self.nodes.contains_key(p) || self.is_descendant(p, key) {
                return;
            }
        }
        let old_parent = self.nodes.get(key).and_then(|n| n.parent);
        match old_parent {
            Some(p) => {
                if let Some(pn) = self.nodes.get_mut(p) {
                    pn.children.retain(|&c| c != key);
                }
            }
            None => self.roots.retain(|&r| r != key),
        }
        if let Some(n) = self.nodes.get_mut(key) {
            n.parent = new_parent;
            n.is_world_dirty = true;
        }
        match new_parent {
            Some(p) => self.nodes.get_mut(p).expect("parent checked").children.push(key),
            None => self.roots.push(key),
        }
        self.mark_subtree_dirty(key);
    }

    /// Меняет порядок узла внутри списка своего родителя (z-order).
    /// Возвращает `false`, если узел не найден.
    pub fn move_to_index(&mut self, key: NodeKey, index: usize) -> bool {
        let Some(node) = self.nodes.get(key) else { return false };
        let parent = node.parent;
        let mut list = match parent {
            Some(p) => self.nodes.get(p).map(|n| n.children.clone()).unwrap_or_default(),
            None => self.roots.clone(),
        };
        let Some(old) = list.iter().position(|&k| k == key) else { return false };
        list.remove(old);
        let index = index.min(list.len());
        list.insert(index, key);
        match parent {
            Some(p) => {
                if let Some(n) = self.nodes.get_mut(p) {
                    n.children = list;
                }
            }
            None => self.roots = list,
        }
        true
    }

    // --- Мутации с инвалидацией кэша ---

    /// Устанавливает локальную трансформацию и помечает поддерево грязным.
    pub fn set_transform(&mut self, key: NodeKey, transform: Affine2) {
        let Some(n) = self.nodes.get_mut(key) else { return };
        n.local_transform = transform;
        n.is_world_dirty = true;
        self.mark_subtree_dirty(key);
    }

    /// Помечает поддерево (включая сам узел) как требующее пересчёта
    /// мировой трансформации. Вызывайте после прямых мутаций через `get_mut`.
    pub fn mark_subtree_dirty(&mut self, key: NodeKey) {
        let mut stack = vec![key];
        while let Some(k) = stack.pop() {
            if let Some(n) = self.nodes.get_mut(k) {
                n.is_world_dirty = true;
                stack.extend(n.children.iter().copied());
            }
        }
    }

    // --- Мировые трансформации ---

    /// Пересчитывает `world_transform` для всех грязных узлов в порядке
    /// «родитель раньше потомка». Вызывайте после мутаций, перед рендером.
    pub fn flush_transforms(&mut self) {
        let order = self.walk();
        for key in order {
            let dirty = self.nodes.get(key).map(|n| n.is_world_dirty).unwrap_or(false);
            if !dirty {
                continue;
            }
            let parent_world = self
                .nodes
                .get(key)
                .and_then(|n| n.parent)
                .and_then(|p| self.nodes.get(p))
                .map(|p| p.world_transform)
                .unwrap_or(Affine2::IDENTITY);
            let local = self.nodes.get(key).map(|n| n.local_transform).unwrap_or(Affine2::IDENTITY);
            if let Some(n) = self.nodes.get_mut(key) {
                n.world_transform = parent_world * local;
                n.is_world_dirty = false;
            }
        }
    }

    /// Кэшированная мировая трансформация. Гарантии корректности только после
    /// [`SceneGraph::flush_transforms`].
    pub fn world_transform(&self, key: NodeKey) -> Option<Affine2> {
        self.nodes.get(key).map(|n| n.world_transform)
    }

    /// Мировая ограничивающая рамка узла (по кэшированной трансформации).
    pub fn world_bbox(&self, key: NodeKey) -> Option<(Vec2, Vec2)> {
        let node = self.nodes.get(key)?;
        let (lmin, lmax) = node.kind.local_bbox();
        let t = node.world_transform;
        let corners = [
            t.transform_point2(lmin),
            t.transform_point2(Vec2::new(lmax.x, lmin.y)),
            t.transform_point2(Vec2::new(lmin.x, lmax.y)),
            t.transform_point2(lmax),
        ];
        let min = corners.iter().copied().reduce(Vec2::min)?;
        let max = corners.iter().copied().reduce(Vec2::max)?;
        Some((min, max))
    }

    // --- Обходы ---

    /// Префиксный обход в порядке отрисовки (корни сверху вниз, дети рекурсивно).
    pub fn walk(&self) -> Vec<NodeKey> {
        let mut out = Vec::with_capacity(self.nodes.len());
        let mut stack: Vec<NodeKey> = self.roots.iter().rev().copied().collect();
        while let Some(key) = stack.pop() {
            out.push(key);
            if let Some(n) = self.nodes.get(key) {
                stack.extend(n.children.iter().rev().copied());
            }
        }
        out
    }

    /// Глубина узла от корня (0 для корней).
    pub fn depth(&self, key: NodeKey) -> Option<u32> {
        let mut d = 0;
        let mut cur = key;
        loop {
            let Some(n) = self.nodes.get(cur) else { return None };
            match n.parent {
                Some(p) => {
                    d += 1;
                    cur = p;
                }
                None => return Some(d),
            }
        }
    }

    /// Является ли `child` потомком `ancestor`.
    pub fn is_descendant(&self, child: NodeKey, ancestor: NodeKey) -> bool {
        let mut cur = child;
        while let Some(n) = self.nodes.get(cur) {
            match n.parent {
                Some(p) if p == ancestor => return true,
                Some(p) => cur = p,
                None => return false,
            }
        }
        false
    }

    // --- Выделение ---

    pub fn clear_selection(&mut self) {
        self.selection.clear();
    }

    pub fn set_selection(&mut self, keys: Vec<NodeKey>) {
        self.selection = keys;
    }

    pub fn add_to_selection(&mut self, key: NodeKey) {
        if !self.selection.contains(&key) {
            self.selection.push(key);
        }
    }

    pub fn toggle_selection(&mut self, key: NodeKey) {
        if let Some(pos) = self.selection.iter().position(|&k| k == key) {
            self.selection.remove(pos);
        } else {
            self.selection.push(key);
        }
    }
}

impl Default for SceneGraph {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::model::nodes::ShapeKind;
    use crate::engine::model::types::Color;

    fn rect_kind(w: f32, h: f32) -> NodeKind {
        NodeKind::Shape(ShapeKind::Rectangle { size: Vec2::new(w, h), corner_radii: [0.0; 4] })
    }

    #[test]
    fn insert_and_query() {
        let mut g = SceneGraph::new();
        let a = g.insert_root("A", rect_kind(10.0, 10.0));
        let b = g.insert_child(a, "B", rect_kind(5.0, 5.0)).unwrap();
        assert_eq!(g.len(), 2);
        assert_eq!(g.get(a).unwrap().name, "A");
        assert_eq!(g.get(b).unwrap().parent, Some(a));
        assert_eq!(g.get(a).unwrap().children, vec![b]);
    }

    #[test]
    fn insert_child_missing_parent() {
        let mut g = SceneGraph::new();
        assert!(g.insert_child(NodeKey::default(), "X", rect_kind(1.0, 1.0)).is_none());
    }

    #[test]
    fn remove_subtree() {
        let mut g = SceneGraph::new();
        let a = g.insert_root("A", rect_kind(10.0, 10.0));
        let b = g.insert_child(a, "B", rect_kind(5.0, 5.0)).unwrap();
        let c = g.insert_child(b, "C", rect_kind(5.0, 5.0)).unwrap();
        g.remove(b);
        assert!(!g.contains(b));
        assert!(!g.contains(c));
        assert_eq!(g.get(a).unwrap().children, Vec::<NodeKey>::new());
        assert_eq!(g.len(), 1);
    }

    #[test]
    fn reparent_into_root_and_back() {
        let mut g = SceneGraph::new();
        let a = g.insert_root("A", rect_kind(10.0, 10.0));
        let b = g.insert_root("B", rect_kind(10.0, 10.0));
        let c = g.insert_child(b, "C", rect_kind(5.0, 5.0)).unwrap();
        g.reparent(c, Some(a));
        assert_eq!(g.get(c).unwrap().parent, Some(a));
        assert_eq!(g.get(a).unwrap().children, vec![c]);
        assert!(g.get(b).unwrap().children.is_empty());
        g.reparent(c, None);
        assert_eq!(g.get(c).unwrap().parent, None);
        assert!(g.roots.contains(&c));
    }

    #[test]
    fn reparent_rejects_cycle() {
        let mut g = SceneGraph::new();
        let a = g.insert_root("A", rect_kind(10.0, 10.0));
        let b = g.insert_child(a, "B", rect_kind(5.0, 5.0)).unwrap();
        g.reparent(a, Some(b));
        assert_eq!(g.get(a).unwrap().parent, None);
    }

    #[test]
    fn world_transform_nested() {
        let mut g = SceneGraph::new();
        let a = g.insert_root("A", rect_kind(10.0, 10.0));
        let b = g.insert_child(a, "B", rect_kind(5.0, 5.0)).unwrap();
        g.set_transform(a, Affine2::from_translation(Vec2::new(100.0, 50.0)));
        g.set_transform(b, Affine2::from_translation(Vec2::new(10.0, 20.0)));
        g.flush_transforms();
        let ta = g.world_transform(a).unwrap();
        let tb = g.world_transform(b).unwrap();
        assert_eq!(ta.transform_point2(Vec2::ZERO), Vec2::new(100.0, 50.0));
        assert_eq!(tb.transform_point2(Vec2::ZERO), Vec2::new(110.0, 70.0));
    }

    #[test]
    fn world_bbox_translated() {
        let mut g = SceneGraph::new();
        let a = g.insert_root("A", rect_kind(10.0, 20.0));
        g.set_transform(a, Affine2::from_translation(Vec2::new(5.0, 7.0)));
        g.flush_transforms();
        let (mn, mx) = g.world_bbox(a).unwrap();
        assert_eq!(mn, Vec2::new(5.0, 7.0));
        assert_eq!(mx, Vec2::new(15.0, 27.0));
    }

    #[test]
    fn walk_paint_order() {
        let mut g = SceneGraph::new();
        let a = g.insert_root("A", rect_kind(10.0, 10.0));
        let b = g.insert_root("B", rect_kind(10.0, 10.0));
        let b1 = g.insert_child(b, "B1", rect_kind(5.0, 5.0)).unwrap();
        let b2 = g.insert_child(b, "B2", rect_kind(5.0, 5.0)).unwrap();
        let order = g.walk();
        assert_eq!(order, vec![a, b, b1, b2]);
    }

    #[test]
    fn move_to_index_reorders() {
        let mut g = SceneGraph::new();
        let a = g.insert_root("A", rect_kind(10.0, 10.0));
        let b = g.insert_root("B", rect_kind(10.0, 10.0));
        g.move_to_index(b, 0);
        assert_eq!(g.roots(), &[b, a]);
    }

    #[test]
    fn selection_ops() {
        let mut g = SceneGraph::new();
        let a = g.insert_root("A", rect_kind(10.0, 10.0));
        g.add_to_selection(a);
        assert_eq!(g.selection(), &[a]);
        g.toggle_selection(a);
        assert!(g.selection().is_empty());
        g.set_selection(vec![a]);
        g.clear_selection();
        assert!(g.selection().is_empty());
    }

    #[test]
    fn serialize_scene() {
        let mut g = SceneGraph::new();
        let a = g.insert_root("A", rect_kind(10.0, 10.0));
        let _b = g.insert_child(a, "B", rect_kind(5.0, 5.0)).unwrap();
        let json = serde_json::to_string(&g).unwrap();
        let back: SceneGraph = serde_json::from_str(&json).unwrap();
        assert_eq!(back.len(), g.len());
        // Ключи переживают сериализацию: индекс/версия сохранены.
        let a2 = *back.roots().first().unwrap();
        assert_eq!(back.get(a2).unwrap().name, "A");
    }

    #[test]
    fn node_with_fill_and_stroke() {
        let mut g = SceneGraph::new();
        let a = g.insert_root("A", rect_kind(10.0, 10.0));
        g.get_mut(a).unwrap().fills.push(Paint::Solid(Color::from_rgba8(255, 0, 0, 255)));
        g.get_mut(a).unwrap().strokes.push(Stroke::solid(Color::BLACK, 1.0));
        let n = g.get(a).unwrap();
        assert_eq!(n.fills.len(), 1);
        assert_eq!(n.strokes.len(), 1);
    }
}