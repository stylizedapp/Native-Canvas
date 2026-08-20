//! Структура сцены: `SceneNode` и граф `SceneGraph` на арене `SlotMap`.
//!
//! `SceneNode` кэширует мировую трансформацию (`world_transform`) с флагом
//! `is_world_dirty`. Все мутации, влияющие на геометрию, помечают поддерево
//! грязным; перед чтением `world_transform`/`world_bbox` нужно вызвать
//! [`SceneGraph::flush_transforms`] (обычно — раз в кадр перед рендером).

use crate::engine::model::nodes::{NodeKey, NodeKind, ShapeKind};
use crate::engine::model::types::{BlendMode, Effect, Paint, Stroke};
use crate::engine::spatial::SpatialIndex;
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
    /// Кэш мировой ограничивающей рамки (обновляется в `flush_transforms`).
    #[serde(skip)]
    pub cached_world_bbox: Option<(Vec2, Vec2)>,

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
            cached_world_bbox: None,
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

/// Мировая ограничивающая рамка по локальной геометрии и трансформации.
fn kind_world_bbox(kind: &NodeKind, world: Affine2) -> (Vec2, Vec2) {
    let (lmin, lmax) = kind.local_bbox();
    let corners = [
        world.transform_point2(lmin),
        world.transform_point2(Vec2::new(lmax.x, lmin.y)),
        world.transform_point2(Vec2::new(lmin.x, lmax.y)),
        world.transform_point2(lmax),
    ];
    let min = corners.iter().copied().reduce(Vec2::min).unwrap_or(lmin);
    let max = corners.iter().copied().reduce(Vec2::max).unwrap_or(lmax);
    (min, max)
}

/// Иерархический граф сцены на арене `SlotMap` с кэшем мировых трансформаций.
#[derive(Debug, Serialize, Deserialize)]
pub struct SceneGraph {
    nodes: SlotMap<NodeKey, SceneNode>,
    roots: Vec<NodeKey>,
    selection: Vec<NodeKey>,
    /// Сколько узлов помечено как требующие пересчёта мировой трансформации.
    /// Позволяет `flush_transforms` делать O(1) early-exit, когда мир чист.
    #[serde(default)]
    dirty_transforms_count: usize,
    /// Требуется ли пересчёт auto-layout при следующем `flush_transforms`.
    #[serde(default)]
    is_layout_dirty: bool,
    /// Spatial hash grid для hit-теста (пересобирается при необходимости).
    #[serde(skip)]
    spatial: Option<SpatialIndex>,
    /// Scratch-буферы раскладки — переиспользуются, не сериализуются.
    #[serde(skip)]
    layout_frames: Vec<NodeKey>,
    #[serde(skip)]
    layout_children: Vec<NodeKey>,
    #[serde(skip)]
    layout_sizes: Vec<Vec2>,
    #[serde(skip)]
    layout_main: Vec<f32>,
}

impl Clone for SceneGraph {
    /// Клон без дорогих кэшей: spatial-индекс и scratch-буферы отбрасываются
    /// (пересобираются лениво), кэши мировых bbox копируются вместе с нодами.
    fn clone(&self) -> Self {
        Self {
            nodes: self.nodes.clone(),
            roots: self.roots.clone(),
            selection: self.selection.clone(),
            dirty_transforms_count: self.dirty_transforms_count,
            is_layout_dirty: self.is_layout_dirty,
            spatial: None,
            layout_frames: Vec::new(),
            layout_children: Vec::new(),
            layout_sizes: Vec::new(),
            layout_main: Vec::new(),
        }
    }
}

impl SceneGraph {
    pub fn new() -> Self {
        Self {
            nodes: SlotMap::with_key(),
            roots: Vec::new(),
            selection: Vec::new(),
            dirty_transforms_count: 0,
            is_layout_dirty: false,
            spatial: None,
            layout_frames: Vec::new(),
            layout_children: Vec::new(),
            layout_sizes: Vec::new(),
            layout_main: Vec::new(),
        }
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
        self.dirty_transforms_count += 1;
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
        self.dirty_transforms_count += 1;
        self.is_layout_dirty = true;
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
            if self.nodes.get(*k).map(|n| n.is_world_dirty).unwrap_or(false) {
                self.dirty_transforms_count = self.dirty_transforms_count.saturating_sub(1);
            }
            if let Some(si) = self.spatial.as_mut() {
                si.remove(*k);
            }
            self.nodes.remove(*k);
        }
        self.selection.retain(|s| !subtree.contains(s));
        self.is_layout_dirty = true;
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
        }
        match new_parent {
            Some(p) => self.nodes.get_mut(p).expect("parent checked").children.push(key),
            None => self.roots.push(key),
        }
        self.is_layout_dirty = true;
        self.mark_subtree_dirty(key);
    }

    /// Переносит узел с сохранением МИРОВОЙ позиции: новый локальный
    /// трансформ пересчитывается из нового родителя, чтобы объект не прыгал.
    /// Сначала прогоняется `flush_transforms` (мировые актуальны).
    pub fn reparent_preserve_world(&mut self, key: NodeKey, new_parent: Option<NodeKey>) {
        if !self.nodes.contains_key(key) {
            return;
        }
        if let Some(p) = new_parent {
            if !self.nodes.contains_key(p) || self.is_descendant(p, key) {
                return;
            }
        }
        self.flush_transforms();
        let old_world = self.world_transform(key).unwrap_or(Affine2::IDENTITY);
        let parent_world = new_parent
            .and_then(|p| self.world_transform(p))
            .unwrap_or(Affine2::IDENTITY);
        self.reparent(key, new_parent);
        if let Some(n) = self.nodes.get_mut(key) {
            n.local_transform = parent_world.inverse() * old_world;
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
        self.is_layout_dirty = true;
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
        self.is_layout_dirty = true;
        Some(new_root)
    }

    /// Копирует поддерево `key` (вместе с потомством) во временный граф `out`
    /// и возвращает ключ копии в `out`. Итеративный обход — без рекурсии,
    /// чтобы глубина дерева не приводила к переполнению стека (release).
    pub fn clone_into(&self, out: &mut SceneGraph, key: NodeKey) -> Option<NodeKey> {
        if !self.nodes.contains_key(key) {
            return None;
        }
        let mut map: HashMap<NodeKey, NodeKey> = HashMap::new();
        // Предпорядок: родитель вставляется раньше потомков, поэтому его ключ
        // уже в `map`, когда обрабатываются дети.
        let mut stack = vec![key];
        let mut root = None;
        while let Some(cur) = stack.pop() {
            let Some(node) = self.nodes.get(cur) else { continue };
            let parent = node.parent.and_then(|p| map.get(&p)).copied();
            let mut copy = node.clone();
            copy.parent = parent;
            copy.children = Vec::new();
            copy.is_world_dirty = true;
            let nk = out.nodes.insert(copy);
            out.dirty_transforms_count += 1;
            if root.is_none() {
                root = Some(nk);
            }
            map.insert(cur, nk);
            // Дети обрабатываются в исходном порядке (стек LIFO + rev-пуша),
            // поэтому append в порядке обработки сохраняет z-order.
            if let Some(p) = parent {
                if let Some(pn) = out.nodes.get_mut(p) {
                    pn.children.push(nk);
                }
            }
            for child in node.children.iter().rev() {
                stack.push(*child);
            }
        }
        root
    }

    /// Вставляет глубокую копию поддерева из временного графа `src` (узел `key`)
    /// новым корнем (или дочерним к `parent`). Возвращает ключ копии.
    /// Итеративный обход — без рекурсии.
    pub fn insert_clone_from(
        &mut self,
        src: &SceneGraph,
        key: NodeKey,
        parent: Option<NodeKey>,
    ) -> Option<NodeKey> {
        if !src.nodes.contains_key(key) {
            return None;
        }
        let mut map: HashMap<NodeKey, NodeKey> = HashMap::new();
        let mut stack = vec![(key, parent)];
        let mut root = None;
        while let Some((cur, dst_parent)) = stack.pop() {
            let Some(node) = src.nodes.get(cur) else { continue };
            let mut copy = node.clone();
            copy.parent = dst_parent;
            copy.children = Vec::new();
            copy.is_world_dirty = true;
            let nk = self.nodes.insert(copy);
            self.dirty_transforms_count += 1;
            if root.is_none() {
                root = Some(nk);
            }
            map.insert(cur, nk);
            if let Some(p) = dst_parent {
                if let Some(pn) = self.nodes.get_mut(p) {
                    pn.children.push(nk);
                }
            }
            for child in node.children.iter().rev() {
                stack.push((*child, Some(nk)));
            }
        }
        if parent.is_none() {
            if let Some(k) = root {
                self.add_root(k);
            }
        }
        if root.is_some() {
            self.is_layout_dirty = true;
        }
        root
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
        self.mark_subtree_dirty(key);
    }

    /// Помечает поддерево (включая сам узел) как требующее пересчёта
    /// мировой трансформации. Вызывайте после прямых мутаций через `get_mut`.
    pub fn mark_subtree_dirty(&mut self, key: NodeKey) {
        let mut stack = vec![key];
        while let Some(k) = stack.pop() {
            if let Some(n) = self.nodes.get_mut(k) {
                n.is_world_dirty = true;
                self.dirty_transforms_count += 1;
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
        // Переиспользуемые буферы — без аллокаций на кадр.
        self.layout_frames.clear();
        for (k, n) in self.nodes.iter() {
            if matches!(n.kind, NodeKind::Frame { auto_layout: Some(_), .. }) {
                self.layout_frames.push(k);
            }
        }
        let n_frames = self.layout_frames.len();
        for i in 0..n_frames {
            let key = self.layout_frames[i];
            let Some(node) = self.nodes.get(key) else { continue };
            let Some((cfg, frame_size)) = (match &node.kind {
                NodeKind::Frame { size, auto_layout: Some(cfg), .. } => Some((*cfg, *size)),
                _ => None,
            }) else {
                continue;
            };
            if node.children.is_empty() {
                continue;
            }
            let [pt, pr, pb, pl] = cfg.padding;

            // Снимок детей фрейма и их размеров (scratch-буферы).
            self.layout_children.clear();
            self.layout_children.extend_from_slice(&node.children);
            self.layout_sizes.clear();
            self.layout_sizes.extend(
                self.layout_children
                    .iter()
                    .filter_map(|&ck| self.nodes.get(ck).map(|n| n.kind.local_bbox()))
                    .map(|(a, b)| b - a),
            );
            if self.layout_sizes.len() != self.layout_children.len() {
                continue;
            }
            let horizontal = cfg.direction == LayoutDirection::Horizontal;
            let main_of = |s: Vec2| if horizontal { s.x } else { s.y };
            let cross_of = |s: Vec2| if horizontal { s.y } else { s.x };
            let frame_main = if horizontal { frame_size.x } else { frame_size.y };
            let frame_cross = if horizontal { frame_size.y } else { frame_size.x };
            self.layout_main.clear();
            self.layout_main.extend(self.layout_sizes.iter().map(|&s| main_of(s)));
            let sum_main: f32 = self.layout_main.iter().sum();
            let n = self.layout_children.len() as f32;

            // Свободное место и эффективный межэлементный отступ. Для
            // SpaceBetween отступ вычисляется из остатка, поэтому разворачиваем
            // зависимость: сначала остаток без отступов, затем gap из него.
            let raw_free = frame_main - pl - pr - sum_main;
            let gap = match cfg.justify_content {
                LayoutJustify::SpaceBetween if n > 1.0 => (raw_free / (n - 1.0)).max(0.0),
                _ => cfg.spacing,
            };
            // Отступы занимают место: свободное пространство для распределения.
            let free = raw_free - gap * (n - 1.0);

            // Начало основной оси.
            let start_main = match cfg.justify_content {
                LayoutJustify::Min => pl,
                LayoutJustify::Center => pl + free * 0.5,
                LayoutJustify::Max => pl + free,
                LayoutJustify::SpaceBetween => pl,
            };

            let mut cursor = start_main;
            let n_children = self.layout_children.len();
            for ci in 0..n_children {
                let ck = self.layout_children[ci];
                let s = self.layout_sizes[ci];
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
                }
                // Ребёнок сдвинут — всё его поддерево требует пересчёта мира.
                self.mark_subtree_dirty(ck);
                cursor += self.layout_main[ci] + gap;
            }
        }
    }

    /// Пересчитывает `world_transform` для всех грязных узлов в порядке
    /// «родитель раньше потомка». Вызывайте после мутаций, перед рендером.
    /// При чистом мире (ничего не менялось с прошлого flush) — O(1).
    pub fn flush_transforms(&mut self) {
        if self.dirty_transforms_count == 0 && !self.is_layout_dirty {
            return;
        }
        if self.is_layout_dirty {
            self.is_layout_dirty = false;
            self.apply_auto_layouts();
        }
        if self.dirty_transforms_count == 0 {
            return;
        }
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
                let bbox = kind_world_bbox(&n.kind, n.world_transform);
                if let Some(si) = self.spatial.as_mut() {
                    match n.cached_world_bbox {
                        Some(old) => si.update(key, old.0, old.1, bbox.0, bbox.1),
                        None => si.insert(key, bbox.0, bbox.1),
                    }
                }
                n.cached_world_bbox = Some(bbox);
            }
        }
        self.dirty_transforms_count = 0;
    }

    /// Есть ли узлы с грязной мировой трансформацией или раскладка,
    /// требующая пересчёта (т.е. `flush_transforms` сделает реальную работу).
    pub fn has_dirty_transforms(&self) -> bool {
        self.dirty_transforms_count > 0 || self.is_layout_dirty
    }

    /// Отмечает, что раскладка auto-layout фреймов изменилась (размеры узлов,
    /// конфиг фрейма и т.п.) — при следующем flush пересчитаются позиции детей.
    pub fn mark_layout_dirty(&mut self) {
        self.is_layout_dirty = true;
    }

    /// Затронет ли изменение геометрии узла `key` раскладку какого-либо
    /// auto-layout фрейма: сам узел — фрейм с auto_layout, или узел вложен в него.
    pub fn affects_auto_layout(&self, key: NodeKey) -> bool {
        let mut cur = Some(key);
        while let Some(k) = cur {
            let Some(n) = self.nodes.get(k) else { return false };
            if matches!(n.kind, NodeKind::Frame { auto_layout: Some(_), .. }) {
                return true;
            }
            cur = n.parent;
        }
        false
    }

    /// Кэшированная мировая трансформация. Гарантии корректности только после
    /// [`SceneGraph::flush_transforms`].
    pub fn world_transform(&self, key: NodeKey) -> Option<Affine2> {
        self.nodes.get(key).map(|n| n.world_transform)
    }

    /// Мировая ограничивающая рамка узла (по кэшированной трансформации).
    /// Использует кэш из `flush_transforms`, если он есть, иначе считает на лету.
    pub fn world_bbox(&self, key: NodeKey) -> Option<(Vec2, Vec2)> {
        let node = self.nodes.get(key)?;
        if let Some(b) = node.cached_world_bbox {
            return Some(b);
        }
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

    /// Помечает всю сцену грязной (после загрузки внешних данных): мир и
    /// кэши bbox сбрасываются, spatial-индекс пересобирается при следующем
    /// запросе.
    pub fn invalidate_all(&mut self) {
        self.dirty_transforms_count = 0;
        for (_, n) in self.nodes.iter_mut() {
            n.is_world_dirty = true;
            n.cached_world_bbox = None;
            self.dirty_transforms_count += 1;
        }
        self.is_layout_dirty = true;
        self.spatial = None;
    }

    // --- Spatial hash grid ---

    fn ensure_spatial(&mut self) {
        if self.spatial.is_some() {
            return;
        }
        let mut si = SpatialIndex::new();
        for (k, n) in self.nodes.iter() {
            let bbox = match n.cached_world_bbox {
                Some(b) => b,
                None => kind_world_bbox(&n.kind, n.world_transform),
            };
            si.insert(k, bbox.0, bbox.1);
        }
        self.spatial = Some(si);
    }

    /// Кандидаты под мировой точкой (требует актуальных трансформаций —
    /// вызовите `flush_transforms` заранее).
    pub fn spatial_query_point(&mut self, p: Vec2) -> Vec<NodeKey> {
        self.ensure_spatial();
        self.spatial.as_ref().map(|si| si.query_point(p)).unwrap_or_default()
    }

    /// Кандидаты, чей bbox пересекает мировой прямоугольник.
    pub fn spatial_query_rect(&mut self, mn: Vec2, mx: Vec2) -> Vec<NodeKey> {
        self.ensure_spatial();
        self.spatial.as_ref().map(|si| si.query_rect(mn, mx)).unwrap_or_default()
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
    fn reparent_preserve_world_keeps_global_position() {
        let mut g = SceneGraph::new();
        let frame = g.insert_root("F", rect_kind(300.0, 300.0));
        let a = g.insert_child(frame, "A", rect_kind(10.0, 10.0)).unwrap();
        g.set_transform(frame, Affine2::from_translation(Vec2::new(50.0, 50.0)));
        g.set_transform(a, Affine2::from_translation(Vec2::new(20.0, 30.0)));
        g.flush_transforms();
        let old_world = g.world_transform(a).unwrap();

        // Перенос в другой корень с СИЛЬНО отличным положением.
        let b = g.insert_root("B", rect_kind(100.0, 100.0));
        g.set_transform(b, Affine2::from_translation(Vec2::new(1000.0, 2000.0)));
        g.reparent_preserve_world(a, Some(b));
        g.flush_transforms();

        let new_world = g.world_transform(a).unwrap();
        assert!(new_world.transform_point2(Vec2::ZERO).distance(old_world.transform_point2(Vec2::ZERO)) < 0.01);
        assert_eq!(g.get(a).unwrap().parent, Some(b));
    }

    #[test]
    fn reparent_preserve_world_to_root_and_back() {
        let mut g = SceneGraph::new();
        let frame = g.insert_root("F", rect_kind(100.0, 100.0));
        let a = g.insert_child(frame, "A", rect_kind(10.0, 10.0)).unwrap();
        g.set_transform(frame, Affine2::from_translation(Vec2::new(300.0, 300.0)));
        g.set_transform(a, Affine2::from_translation(Vec2::new(5.0, 6.0)));
        g.flush_transforms();
        let old_origin = g.world_transform(a).unwrap().transform_point2(Vec2::ZERO);

        // Наружу (в корни): локальный трансформ должен стать мировым.
        g.reparent_preserve_world(a, None);
        g.flush_transforms();
        let root_origin = g.world_transform(a).unwrap().transform_point2(Vec2::ZERO);
        assert!(root_origin.distance(old_origin) < 0.01);
        assert_eq!(g.get(a).unwrap().parent, None);
        assert!(g.roots.contains(&a));
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

    #[test]
    fn deep_tree_clone_iterative_no_overflow() {
        // Глубокое дерево (10k уровней) — рекурсивный клон раньше валил стек.
        let mut g = SceneGraph::new();
        let mut parent = g.insert_root("deep0", rect_kind(10.0, 10.0));
        for i in 1..10_000 {
            parent = g.insert_child(parent, &format!("deep{i}"), rect_kind(10.0, 10.0)).unwrap();
        }
        // Клонирование всего поддерева во временный граф.
        let mut temp = SceneGraph::new();
        let copy_root = g.clone_into(&mut temp, g.roots()[0]).expect("root cloned");
        assert!(temp.contains(copy_root));
        assert_eq!(temp.len(), 10_000);
        // Вставка обратно (другим корнем) — тоже итеративно.
        let back = g.insert_clone_from(&temp, copy_root, None).expect("inserted");
        assert!(g.contains(back));
        assert_eq!(g.len(), 20_000);
    }

    #[test]
    fn flush_early_exit_resets_counter() {
        let mut g = SceneGraph::new();
        let a = g.insert_root("A", rect_kind(10.0, 10.0));
        let b = g.insert_child(a, "B", rect_kind(5.0, 5.0)).unwrap();
        assert!(g.has_dirty_transforms());
        g.flush_transforms();
        assert!(!g.has_dirty_transforms());
        // Повторный flush — no-op (ранний выход), мировой трансформ не ломается.
        g.flush_transforms();
        assert!(!g.has_dirty_transforms());
        assert_eq!(g.world_transform(a).unwrap(), Affine2::IDENTITY);
        assert_eq!(g.world_transform(b).unwrap(), Affine2::IDENTITY);
    }

    #[test]
    fn new_nodes_are_flushed_despite_early_exit() {
        let mut g = SceneGraph::new();
        let a = g.insert_root("A", rect_kind(10.0, 10.0));
        g.flush_transforms();
        assert!(!g.has_dirty_transforms());
        // Вставка после флаша — узел обязан попасть в следующий пересчёт.
        let b = g.insert_child(a, "B", rect_kind(5.0, 5.0)).unwrap();
        g.set_transform(a, Affine2::from_translation(Vec2::new(30.0, 40.0)));
        g.flush_transforms();
        assert_eq!(g.world_transform(b).unwrap().transform_point2(Vec2::ZERO), Vec2::new(30.0, 40.0));
        assert!(!g.has_dirty_transforms());
    }

    #[test]
    fn get_mut_then_mark_dirty_roundtrip() {
        let mut g = SceneGraph::new();
        let a = g.insert_root("A", rect_kind(10.0, 10.0));
        let b = g.insert_child(a, "B", rect_kind(5.0, 5.0)).unwrap();
        g.flush_transforms();
        assert!(!g.has_dirty_transforms());
        g.get_mut(b).unwrap().local_transform = Affine2::from_translation(Vec2::new(7.0, 8.0));
        g.mark_subtree_dirty(b);
        assert!(g.has_dirty_transforms());
        g.flush_transforms();
        assert_eq!(g.world_transform(b).unwrap().transform_point2(Vec2::ZERO), Vec2::new(7.0, 8.0));
        assert!(!g.has_dirty_transforms());
    }

    #[test]
    fn layout_dirty_after_config_change_repositions() {
        use crate::engine::model::types::{AutoLayoutConfig, LayoutDirection};
        let mut g = SceneGraph::new();
        let frame = g.insert_root("F", NodeKind::Frame {
            size: Vec2::new(100.0, 50.0),
            clip_content: false,
            corner_radii: [0.0; 4],
            auto_layout: Some(AutoLayoutConfig {
                direction: LayoutDirection::Horizontal,
                spacing: 0.0,
                padding: [0.0, 0.0, 0.0, 0.0],
                ..AutoLayoutConfig::default()
            }),
            constraints: Default::default(),
        });
        let _a = g.insert_child(frame, "A", rect_kind(20.0, 10.0)).unwrap();
        let b = g.insert_child(frame, "B", rect_kind(20.0, 10.0)).unwrap();
        g.flush_transforms();
        assert!(!g.has_dirty_transforms());
        assert!((g.get(b).unwrap().local_transform.translation - Vec2::new(20.0, 0.0)).length() < 1e-3);

        // Смена отступа → layout-грязь → при flush дети пересчитаны.
        if let NodeKind::Frame { auto_layout: Some(cfg), .. } = &mut g.get_mut(frame).unwrap().kind {
            cfg.spacing = 10.0;
        }
        g.mark_layout_dirty();
        assert!(g.has_dirty_transforms());
        g.flush_transforms();
        assert!((g.get(b).unwrap().local_transform.translation - Vec2::new(30.0, 0.0)).length() < 1e-3);
        assert!(!g.has_dirty_transforms());
    }

    #[test]
    fn layout_repositions_descendant_world_transforms() {
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
        let child = g.insert_child(frame, "C", rect_kind(20.0, 20.0)).unwrap();
        let grandchild = g.insert_child(child, "GC", rect_kind(10.0, 10.0)).unwrap();
        g.flush_transforms();
        // Ребёнок смещён раскладкой в (5,5) → его потомок виден в мировой точке (5,5).
        let origin = g.world_transform(grandchild).unwrap().transform_point2(Vec2::ZERO);
        assert!((origin - Vec2::new(5.0, 5.0)).length() < 1e-3);
        assert!(!g.has_dirty_transforms());
    }

    #[test]
    fn affects_auto_layout_detects_nested() {
        use crate::engine::model::types::AutoLayoutConfig;
        let mut g = SceneGraph::new();
        let frame = g.insert_root("F", NodeKind::Frame {
            size: Vec2::new(100.0, 100.0),
            clip_content: false,
            corner_radii: [0.0; 4],
            auto_layout: Some(AutoLayoutConfig::default()),
            constraints: Default::default(),
        });
        let child = g.insert_child(frame, "C", rect_kind(10.0, 10.0)).unwrap();
        let plain = g.insert_root("P", rect_kind(10.0, 10.0));
        assert!(g.affects_auto_layout(frame));
        assert!(g.affects_auto_layout(child));
        assert!(!g.affects_auto_layout(plain));
    }
}