//! Структура сцены: `SceneNode` и граф `SceneGraph` на арене `SlotMap`.
//!
//! `SceneNode` кэширует мировую трансформацию (`world_transform`) с флагом
//! `is_world_dirty`. Все мутации, влияющие на геометрию, помечают поддерево
//! грязным; перед чтением `world_transform`/`world_bbox` нужно вызвать
//! [`SceneGraph::flush_transforms`] (обычно — раз в кадр перед рендером).

use crate::engine::model::nodes::{NodeKey, NodeKind, ShapeKind};
use crate::engine::model::types::{BlendMode, Effect, Paint, Stroke};
use glam::{Affine2, Vec2};
use serde::{Deserialize, Serialize};
use slotmap::SlotMap;
use std::collections::HashMap;

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

    /// Добавляет уже существующий узел в корни (для временных графов).
    pub fn add_root(&mut self, key: NodeKey) {
        if !self.roots.contains(&key) {
            self.roots.push(key);
        }
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

    /// Глубоко клонирует поддерево `key` с новыми ключами и вставляет копию
    /// сиблингом сразу после исходного узла. Возвращает ключ копии.
    pub fn duplicate(&mut self, key: NodeKey) -> Option<NodeKey> {
        self.nodes.get(key)?;
        // Собираем поддерево в предпорядке (родитель раньше потомков).
        let mut stack = vec![key];
        let mut order = Vec::new();
        while let Some(k) = stack.pop() {
            order.push(k);
            if let Some(n) = self.nodes.get(k) {
                stack.extend(n.children.iter().copied());
            }
        }
        let mut map: HashMap<NodeKey, NodeKey> = HashMap::new();
        for k in &order {
            let parent = self.nodes.get(*k).and_then(|n| n.parent);
            let new_parent = parent.and_then(|p| map.get(&p)).copied();
            let mut copy = self.nodes.get(*k).cloned()?;
            copy.parent = new_parent;
            copy.is_world_dirty = true;
            map.insert(*k, self.nodes.insert(copy));
        }
        // Дочерние списки копий ссылаются на старые ключи — пересобираем.
        for (_, new) in &map {
            let remap = |c: &NodeKey| map.get(c).copied();
            if let Some(n) = self.nodes.get_mut(*new) {
                n.children = n.children.iter().filter_map(remap).collect();
            }
        }
        let new_root = map[&key];
        // Вставляем копию сразу после оригинала в списке сиблингов.
        let mut siblings = match self.nodes.get(key).and_then(|n| n.parent) {
            Some(p) => self.nodes.get(p).map(|n| n.children.clone()).unwrap_or_default(),
            None => self.roots.clone(),
        };
        if let Some(i) = siblings.iter().position(|&k| k == key) {
            siblings.insert(i + 1, new_root);
            match self.nodes.get(key).and_then(|n| n.parent) {
                Some(p) => {
                    if let Some(n) = self.nodes.get_mut(p) {
                        n.children = siblings;
                    }
                }
                None => self.roots = siblings,
            }
        }
        self.mark_subtree_dirty(new_root);
        Some(new_root)
    }

    /// Копирует поддерево `key` (вместе с потомством) во временный граф `out`
    /// и возвращает ключ копии в `out`. Идентичность между графами не нужна —
    /// дочерние ссылки пересобираются по рекурсии.
    pub fn clone_into(&self, out: &mut SceneGraph, key: NodeKey) -> Option<NodeKey> {
        fn rec(
            src: &SceneGraph,
            out: &mut SceneGraph,
            key: NodeKey,
            map: &mut HashMap<NodeKey, NodeKey>,
        ) -> Option<NodeKey> {
            let node = src.nodes.get(key)?;
            let parent = node.parent.and_then(|p| map.get(&p)).copied();
            let mut copy = node.clone();
            copy.parent = parent;
            copy.children = Vec::new();
            copy.is_world_dirty = true;
            let nk = out.nodes.insert(copy);
            map.insert(key, nk);
            for child in &node.children {
                if let Some(cnk) = rec(src, out, *child, map) {
                    out.nodes.get_mut(nk).expect("только что вставлен").children.push(cnk);
                }
            }
            Some(nk)
        }
        let mut map = HashMap::new();
        rec(self, out, key, &mut map)
    }

    /// Вставляет глубокую копию поддерева из временного графа `src` (узел `key`)
    /// новым корнем (или дочерним к `parent`). Возвращает ключ копии.
    pub fn insert_clone_from(
        &mut self,
        src: &SceneGraph,
        key: NodeKey,
        parent: Option<NodeKey>,
    ) -> Option<NodeKey> {
        fn rec(
            dst: &mut SceneGraph,
            src: &SceneGraph,
            key: NodeKey,
            parent: Option<NodeKey>,
            map: &mut HashMap<NodeKey, NodeKey>,
        ) -> Option<NodeKey> {
            let node = src.nodes.get(key)?;
            let mut copy = node.clone();
            copy.parent = parent;
            copy.children = Vec::new();
            copy.is_world_dirty = true;
            let nk = dst.nodes.insert(copy);
            map.insert(key, nk);
            for child in &node.children {
                if let Some(cnk) = rec(dst, src, *child, Some(nk), map) {
                    dst.nodes.get_mut(nk).expect("только что вставлен").children.push(cnk);
                }
            }
            Some(nk)
        }
        let mut map = HashMap::new();
        let nk = rec(self, src, key, parent, &mut map);
        if parent.is_none() {
            if let Some(k) = nk {
                self.add_root(k);
            }
        }
        nk
    }

    /// Список сиблингов узла в порядке z-order (узел входит в список).
    pub fn siblings_of(&self, key: NodeKey) -> Vec<NodeKey> {
        let Some(node) = self.nodes.get(key) else { return Vec::new() };
        match node.parent {
            Some(p) => self.nodes.get(p).map(|n| n.children.clone()).unwrap_or_default(),
            None => self.roots.clone(),
        }
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

    /// Раскладывает детей фреймов с `auto_layout` внутри родителя: позиции
    /// перезаписываются по направлению/отступу/выравниванию, дети помечаются
    /// грязными. Запускается перед пересчётом мировых трансформаций.
    fn apply_auto_layouts(&mut self) {
        use crate::engine::model::types::{LayoutAlign, LayoutDirection, LayoutJustify};
        let frames: Vec<NodeKey> = self
            .nodes
            .iter()
            .filter(|(_, n)| matches!(n.kind, NodeKind::Frame { auto_layout: Some(_), .. }))
            .map(|(k, _)| k)
            .collect();
        for key in frames {
            let Some(children) = self.nodes.get(key).map(|n| n.children.clone()) else {
                continue;
            };
            if children.is_empty() {
                continue;
            }
            let Some((cfg, frame_size)) = self.nodes.get(key).and_then(|n| match &n.kind {
                NodeKind::Frame { size, auto_layout: Some(cfg), .. } => Some((*cfg, *size)),
                _ => None,
            }) else {
                continue;
            };
            let [pt, pr, pb, pl] = cfg.padding;

            // Собственные размеры детей и их основная ось.
            let sizes: Vec<Vec2> = children
                .iter()
                .filter_map(|&ck| self.nodes.get(ck).map(|n| n.kind.local_bbox()))
                .map(|(a, b)| b - a)
                .collect();
            if sizes.len() != children.len() {
                continue;
            }
            let horizontal = cfg.direction == LayoutDirection::Horizontal;
            let main_of = |s: Vec2| if horizontal { s.x } else { s.y };
            let cross_of = |s: Vec2| if horizontal { s.y } else { s.x };
            let frame_main = if horizontal { frame_size.x } else { frame_size.y };
            let frame_cross = if horizontal { frame_size.y } else { frame_size.x };
            let main: Vec<f32> = sizes.iter().map(|&s| main_of(s)).collect();
            let sum_main: f32 = main.iter().sum();
            let n = children.len() as f32;
            let free = frame_main - pl - pr - sum_main;

            // Эффективный отступ между детьми.
            let spacing = match cfg.justify_content {
                LayoutJustify::SpaceBetween if n > 1.0 => (free / (n - 1.0)).max(0.0),
                _ => cfg.spacing,
            };

            // Начало основной оси.
            let start_main = match cfg.justify_content {
                LayoutJustify::Min => pl,
                LayoutJustify::Center => pl + free * 0.5,
                LayoutJustify::Max => pl + free,
                LayoutJustify::SpaceBetween => pl,
            };

            let mut cursor = start_main;
            for (i, &ck) in children.iter().enumerate() {
                let s = sizes[i];
                // Позиция поперёк основной оси.
                let cross = match cfg.align_items {
                    LayoutAlign::Stretch => pt,
                    LayoutAlign::Min => pt,
                    LayoutAlign::Center => pt + (frame_cross - cross_of(s)) * 0.5,
                    LayoutAlign::Max => pt + (frame_cross - cross_of(s)),
                };
                let pos = if horizontal {
                    Vec2::new(cursor, cross)
                } else {
                    Vec2::new(cross, cursor)
                };
                if let Some(n) = self.nodes.get_mut(ck) {
                    // Stretch растягивает ребёнка по поперечной оси.
                    if cfg.align_items == LayoutAlign::Stretch {
                        match &mut n.kind {
                            NodeKind::Frame { size, .. } => {
                                if horizontal {
                                    size.y = (frame_cross - pt - pb).max(0.0);
                                } else {
                                    size.x = (frame_cross - pt - pb).max(0.0);
                                }
                            }
                            NodeKind::Shape(ShapeKind::Rectangle { size, .. }) => {
                                if horizontal {
                                    size.y = (frame_cross - pt - pb).max(0.0);
                                } else {
                                    size.x = (frame_cross - pt - pb).max(0.0);
                                }
                            }
                            _ => {}
                        }
                    }
                    n.local_transform.translation = pos;
                    n.is_world_dirty = true;
                }
                cursor += main[i] + spacing;
            }
        }
    }

    /// Пересчитывает `world_transform` для всех грязных узлов в порядке
    /// «родитель раньше потомка». Вызывайте после мутаций, перед рендером.
    pub fn flush_transforms(&mut self) {
        self.apply_auto_layouts();
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

    #[test]
    fn auto_layout_row_positions_children() {
        use crate::engine::model::types::{AutoLayoutConfig, LayoutDirection};
        let mut g = SceneGraph::new();
        let frame = g.insert_root("F", NodeKind::Frame {
            size: Vec2::new(100.0, 100.0),
            clip_content: false,
            corner_radii: [0.0; 4],
            auto_layout: Some(AutoLayoutConfig {
                direction: LayoutDirection::Horizontal,
                spacing: 10.0,
                padding: [5.0, 5.0, 5.0, 5.0],
                ..AutoLayoutConfig::default()
            }),
            constraints: Default::default(),
        });
        let a = g.insert_child(frame, "A", rect_kind(20.0, 20.0)).unwrap();
        let b = g.insert_child(frame, "B", rect_kind(30.0, 30.0)).unwrap();
        g.flush_transforms();
        // A начинается с pl=5; B — через 20 + gap 10.
        let ta = g.get(a).unwrap().local_transform.translation;
        let tb = g.get(b).unwrap().local_transform.translation;
        assert!((ta - Vec2::new(5.0, 5.0)).length() < 1e-3);
        assert!((tb - Vec2::new(35.0, 5.0)).length() < 1e-3);
    }

    #[test]
    fn auto_layout_column_and_stretch() {
        use crate::engine::model::types::{AutoLayoutConfig, LayoutAlign, LayoutDirection, LayoutJustify};
        let mut g = SceneGraph::new();
        let frame = g.insert_root("F", NodeKind::Frame {
            size: Vec2::new(100.0, 200.0),
            clip_content: false,
            corner_radii: [0.0; 4],
            auto_layout: Some(AutoLayoutConfig {
                direction: LayoutDirection::Vertical,
                spacing: 0.0,
                padding: [0.0, 0.0, 0.0, 0.0],
                align_items: LayoutAlign::Stretch,
                justify_content: LayoutJustify::Min,
            }),
            constraints: Default::default(),
        });
        let a = g.insert_child(frame, "A", rect_kind(20.0, 40.0)).unwrap();
        let b = g.insert_child(frame, "B", rect_kind(20.0, 40.0)).unwrap();
        g.flush_transforms();
        let ta = g.get(a).unwrap().local_transform.translation;
        let tb = g.get(b).unwrap().local_transform.translation;
        assert!((ta - Vec2::new(0.0, 0.0)).length() < 1e-3);
        assert!((tb - Vec2::new(0.0, 40.0)).length() < 1e-3);
        // Stretch растягивает детей по ширине фрейма (100).
        let sa = g.get(a).unwrap().kind.local_bbox();
        assert_eq!(sa.1.x, 100.0);
    }
}