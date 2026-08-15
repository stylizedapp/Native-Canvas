use glam::{Affine2, Vec2};
use im::{HashMap, Vector};

/// Уникальный идентификатор узла. Составной GUID (sessionId, localId) реализуем позже;
/// для ядра редактора используем монотонно возрастающий u64.
pub type NodeId = u64;

/// Размер страницы/холста (границы области рисования), в мировых координатах.
/// f32-точности достаточно: на 20000 точность ~0.002px.
pub const PAGE_SIZE: Vec2 = Vec2::new(20000.0, 20000.0);

/// Заливка узла.
#[derive(Clone, Copy, Debug)]
pub struct Fill {
    pub color: [u8; 4], // RGBA
}

impl Fill {
    pub fn solid(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { color: [r, g, b, a] }
    }
}

/// Обводка узла.
#[derive(Clone, Copy, Debug)]
pub struct Stroke {
    pub color: [u8; 4],
    pub width: f32,
    pub inside: bool,
    pub center: bool,
    pub outside: bool,
}

/// Геометрический вид узла.
#[derive(Clone, Debug)]
pub enum NodeKind {
    Frame { w: f32, h: f32 },
    Group,
    Rectangle { w: f32, h: f32 },
    Ellipse { w: f32, h: f32 },
    Line { x2: f32, y2: f32 },
    Vector,
}

impl NodeKind {
    /// Локальная ограничивающая рамка (до трансформации).
    pub fn local_bbox(&self) -> (Vec2, Vec2) {
        match *self {
            NodeKind::Frame { w, h }
            | NodeKind::Rectangle { w, h }
            | NodeKind::Ellipse { w, h } => (Vec2::ZERO, Vec2::new(w, h)),
            NodeKind::Group => (Vec2::ZERO, Vec2::ZERO),
            NodeKind::Line { x2, y2 } => {
                let min = Vec2::new(x2.min(0.0), y2.min(0.0));
                let max = Vec2::new(x2.max(0.0), y2.max(0.0));
                (min, max)
            }
            NodeKind::Vector => (Vec2::ZERO, Vec2::ZERO),
        }
    }
}

/// Узел дерева сцены. children — персистентный вектор, поэтому клонирование узла
/// (и всей сцены) дёшево за счёт разделения памяти (im-rs).
#[derive(Clone, Debug)]
pub struct SceneNode {
    pub id: NodeId,
    pub name: String,
    pub parent: Option<NodeId>,
    pub children: Vector<NodeId>,
    /// Локальная аффинная трансформация относительно родителя (Matrix2x3).
    pub transform: Affine2,
    pub kind: NodeKind,
    pub fill: Option<Fill>,
    pub stroke: Option<Stroke>,
    pub opacity: f32,
    pub visible: bool,
}

impl SceneNode {
    pub fn new(id: NodeId, name: &str, kind: NodeKind) -> Self {
        Self {
            id,
            name: name.to_string(),
            parent: None,
            children: Vector::new(),
            transform: Affine2::from_translation(Vec2::ZERO),
            kind,
            fill: None,
            stroke: None,
            opacity: 1.0,
            visible: true,
        }
    }
}

/// Иерархический граф сцены на персистентных структурах.
///
/// Клонирование `Scene` используется как снапшот для Undo/Redo: поскольку `HashMap`
/// и `Vector` из im-rs персистентны, клон разделяет неизменённые ветви с текущим
/// состоянием — стоимость истории почти бесплатна.
#[derive(Clone, Debug)]
pub struct Scene {
    pub nodes: HashMap<NodeId, SceneNode>,
    pub roots: Vector<NodeId>,
    next_id: NodeId,
    pub selection: Vec<NodeId>,
}

impl Scene {
    pub fn new() -> Self {
        Self {
            nodes: HashMap::new(),
            roots: Vector::new(),
            next_id: 1,
            selection: Vec::new(),
        }
    }

    pub fn alloc_id(&mut self) -> NodeId {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Гарантирует, что счётчик id будет не меньше `min` (для загруженных документов).
    pub fn ensure_next_id(&mut self, min: NodeId) {
        if self.next_id < min {
            self.next_id = min;
        }
    }

    pub fn get(&self, id: NodeId) -> Option<&SceneNode> {
        self.nodes.get(&id)
    }

    pub fn get_mut(&mut self, id: NodeId) -> Option<&mut SceneNode> {
        self.nodes.get_mut(&id)
    }

    pub fn iter(&self) -> impl Iterator<Item = &SceneNode> {
        self.nodes.values()
    }

    /// Добавляет узел корнем сцены.
    pub fn add_root(&mut self, node: SceneNode) -> NodeId {
        let id = node.id;
        self.nodes.insert(id, node);
        self.roots.push_back(id);
        id
    }

    /// Добавляет узел дочерним к `parent`.
    pub fn add_child(&mut self, parent: NodeId, mut node: SceneNode) -> NodeId {
        let id = node.id;
        node.parent = Some(parent);
        self.nodes.insert(id, node);
        if let Some(p) = self.nodes.get_mut(&parent) {
            p.children.push_back(id);
        }
        id
    }

    /// Удаляет узел и всех его потомков рекурсивно.
    pub fn remove(&mut self, id: NodeId) {
        let parent = self.nodes.get(&id).and_then(|n| n.parent);
        match parent {
            Some(p) => {
                if let Some(pn) = self.nodes.get_mut(&p) {
                    pn.children.retain(|&c| c != id);
                }
            }
            None => self.roots.retain(|&r| r != id),
        }

        // Рекурсивно собираем потомков.
        let mut stack = vec![id];
        while let Some(cur) = stack.pop() {
            let children = self.nodes.get(&cur).map(|n| n.children.clone()).unwrap_or_default();
            stack.extend(children.iter().copied());
            self.nodes.remove(&cur);
        }

        self.selection.retain(|&s| s != id);
    }

    /// Мировая трансформация узла (перемножение матриц предков).
    pub fn world_transform(&self, id: NodeId) -> Affine2 {
        let mut affine = Affine2::IDENTITY;
        let mut cur = Some(id);
        while let Some(cid) = cur {
            if let Some(node) = self.nodes.get(&cid) {
                affine = affine * node.transform;
                cur = node.parent;
            } else {
                break;
            }
        }
        affine
    }

    /// Мировая ограничивающая рамка узла.
    pub fn world_bbox(&self, id: NodeId) -> Option<(Vec2, Vec2)> {
        let node = self.nodes.get(&id)?;
        let (lmin, lmax) = node.kind.local_bbox();
        let t = self.world_transform(id);
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

    /// Возвращает узел (и его родителя для координат) по id.
    pub fn node_with_parent(&self, id: NodeId) -> Option<(&SceneNode, Option<&SceneNode>)> {
        let node = self.nodes.get(&id)?;
        let parent = node.parent.and_then(|p| self.nodes.get(&p));
        Some((node, parent))
    }

    /// Префиксный обход дерева в порядке отрисовки.
    pub fn walk(&self) -> Vec<NodeId> {
        let mut out = Vec::with_capacity(self.nodes.len());
        let mut stack: Vec<NodeId> = self.roots.iter().rev().copied().collect();
        while let Some(id) = stack.pop() {
            out.push(id);
            if let Some(node) = self.nodes.get(&id) {
                stack.extend(node.children.iter().rev().copied());
            }
        }
        out
    }
}

impl Default for Scene {
    fn default() -> Self {
        Self::new()
    }
}