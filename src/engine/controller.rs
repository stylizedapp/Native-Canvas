use super::history::History;
use super::scene::{Fill, NodeKind, NodeId, Scene, SceneNode, Stroke};
use super::transform::{pick, Camera};
use crate::engine::serialize::{load_json, save_json};
use glam::{Affine2, Vec2};

/// Активный инструмент.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Tool {
    Select,
    Pan,
    Rectangle,
    Ellipse,
    Line,
    Frame,
}

impl Tool {
    pub fn from_name(name: &str) -> Self {
        match name {
            "pan" => Tool::Pan,
            "rectangle" => Tool::Rectangle,
            "ellipse" => Tool::Ellipse,
            "line" => Tool::Line,
            "frame" => Tool::Frame,
            _ => Tool::Select,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Tool::Select => "select",
            Tool::Pan => "pan",
            Tool::Rectangle => "rectangle",
            Tool::Ellipse => "ellipse",
            Tool::Line => "line",
            Tool::Frame => "frame",
        }
    }
}

/// Live-превью создаваемой фигуры (в мировых координатах).
#[derive(Clone, Debug)]
pub struct Preview {
    pub a: Vec2,
    pub b: Vec2,
    pub kind: NodeKind,
}

/// Промежуточное состояние перетаскивания (координаты экранные).
struct Drag {
    tool: Tool,
    anchor_screen: Vec2,
    current_screen: Vec2,
    /// Захваченный узел при перемещении (Select tool).
    grabbed: Option<NodeId>,
    /// Последняя мировая позиция курсора (для корректного перемещения).
    last_world: Vec2,
    /// Мировая позиция курсора в момент захвата (Select tool).
    grab_world: Vec2,
    /// Позиция узла в момент захвата (для снапа без дрейфа).
    start_pos: Vec2,
}

/// Контроллер холста: связывает граф сцены, камеру, инструменты и историю.
pub struct CanvasController {
    pub scene: Scene,
    pub camera: Camera,
    history: History,
    pub tool: Tool,
    drag: Option<Drag>,
    pub preview: Option<Preview>,
    /// Флаг «состояние изменилось» — движок рендера подхватывает на следующем тике.
    pub dirty: bool,
    /// Ревизия структуры сцены — для пересборки дерева слоёв в UI.
    pub revision: u64,
    /// Показывать сетку.
    pub grid_visible: bool,
    /// Привязка к сетке (плюс всегда к целым пикселям).
    pub snap_enabled: bool,
    /// Шаг сетки (в мировых px).
    pub grid_step: f32,
}

impl CanvasController {
    pub fn new() -> Self {
        let mut c = Self {
            scene: Scene::new(),
            camera: Camera::new(),
            history: History::new(100),
            tool: Tool::Select,
            drag: None,
            preview: None,
            dirty: true,
            revision: 0,
            grid_visible: true,
            snap_enabled: true,
            grid_step: 8.0,
        };
        // Стартовое демо-содержимое.
        c.add_root_node(Tool::Frame, Vec2::new(80.0, 80.0), Vec2::new(400.0, 320.0));
        c.add_root_node(Tool::Rectangle, Vec2::new(120.0, 130.0), Vec2::new(320.0, 250.0));
        c.add_root_node(Tool::Ellipse, Vec2::new(260.0, 200.0), Vec2::new(400.0, 290.0));
        c
    }

    // --- Снапшоты / история ---

    fn record(&mut self) {
        self.history.record(self.scene.clone());
    }

    fn bump_revision(&mut self) {
        self.revision += 1;
        self.dirty = true;
    }

    pub fn undo(&mut self) {
        if let Some(prev) = self.history.undo(&self.scene) {
            self.scene = prev;
            self.scene.selection.clear();
            self.bump_revision();
        }
    }

    pub fn redo(&mut self) {
        if let Some(next) = self.history.redo(&self.scene) {
            self.scene = next;
            self.scene.selection.clear();
            self.bump_revision();
        }
    }

    // --- Инструменты и действия ---

    pub fn set_tool(&mut self, tool: Tool) {
        self.tool = tool;
        self.drag = None;
        self.preview = None;
        self.dirty = true;
    }

    // --- Сетка / снап ---

    pub fn toggle_grid(&mut self) {
        self.grid_visible = !self.grid_visible;
        self.dirty = true;
    }

    pub fn toggle_snap(&mut self) {
        self.snap_enabled = !self.snap_enabled;
        self.dirty = true;
    }

    /// Привязка точки к сетке (если включена) и всегда к целым пикселям.
    pub fn snap_point(&self, p: Vec2) -> Vec2 {
        if self.snap_enabled {
            let s = self.grid_step.max(1.0);
            Vec2::new((p.x / s).round() * s, (p.y / s).round() * s)
        } else {
            Vec2::new(p.x.round(), p.y.round())
        }
    }

    /// Привязка размера: только к целым пикселям (сетка на размеры не влияет).
    fn snap_size(&self, p: Vec2) -> Vec2 {
        Vec2::new(p.x.round(), p.y.round())
    }

    /// Создаёт корневой узел по инструменту из прямоугольника (anchor..current).
    fn add_root_node(&mut self, tool: Tool, a: Vec2, b: Vec2) -> NodeId {
        let name = match tool {
            Tool::Rectangle => "Rectangle",
            Tool::Ellipse => "Ellipse",
            Tool::Line => "Line",
            Tool::Frame => "Frame",
            _ => "Node",
        };
        let min = Vec2::new(a.x.min(b.x), a.y.min(b.y));
        let max = Vec2::new(a.x.max(b.x), a.y.max(b.y));
        let size = max - min;

        let kind = match tool {
            Tool::Rectangle => NodeKind::Rectangle { w: size.x, h: size.y },
            Tool::Ellipse => NodeKind::Ellipse { w: size.x, h: size.y },
            Tool::Line => NodeKind::Line { x2: b.x - a.x, y2: b.y - a.y },
            Tool::Frame => NodeKind::Frame { w: size.x, h: size.y },
            _ => return 0,
        };

        let mut node = SceneNode::new(self.scene.alloc_id(), name, kind);
        let id = node.id;
        node.transform = Affine2::from_translation(min);
        node.fill = Some(Fill::solid(122, 170, 233, 255));
        node.stroke = Some(Stroke {
            color: [0, 0, 0, 200],
            width: 1.0,
            inside: false,
            center: true,
            outside: false,
        });
        self.scene.add_root(node);
        self.bump_revision();
        id
    }

    pub fn delete_selection(&mut self) {
        if self.scene.selection.is_empty() {
            return;
        }
        self.record();
        let sel = self.scene.selection.clone();
        for id in sel {
            self.scene.remove(id);
        }
        self.bump_revision();
    }

    /// Выбирает узел по id (из дерева слоёв).
    pub fn select(&mut self, id: NodeId) {
        if self.scene.get(id).is_some() {
            self.scene.selection = vec![id];
            self.bump_revision();
        }
    }

    /// Сбрасывает документ (новый пустой файл).
    pub fn clear(&mut self) {
        self.record();
        self.scene = Scene::new();
        self.bump_revision();
    }

    // --- Обработка указателя (ЭКРАННЫЕ координаты; кнопка: 1=ЛКМ, 2=средняя) ---

    pub fn pointer_down(&mut self, screen: Vec2, button: u8) {
        // Средняя кнопка или инструмент Pan — панорамирование.
        if button == 2 || self.tool == Tool::Pan {
            self.drag = Some(Drag {
                tool: Tool::Pan,
                anchor_screen: screen,
                current_screen: screen,
                grabbed: None,
                last_world: Vec2::ZERO,
                grab_world: Vec2::ZERO,
                start_pos: Vec2::ZERO,
            });
            self.dirty = true;
            return;
        }

        match self.tool {
            Tool::Select => {
                let world = self.camera.screen_to_world(screen);
                match pick(&self.scene, world) {
                    Some(id) => {
                        // Захват для перемещения.
                        self.record();
                        let start_pos = self
                            .scene
                            .get(id)
                            .map(|n| n.transform.translation)
                            .unwrap_or(Vec2::ZERO);
                        self.scene.selection.clear();
                        self.scene.selection.push(id);
                        self.drag = Some(Drag {
                            tool: Tool::Select,
                            anchor_screen: screen,
                            current_screen: screen,
                            grabbed: Some(id),
                            last_world: world,
                            grab_world: world,
                            start_pos,
                        });
                        self.bump_revision();
                    }
                    None => {
                        self.scene.selection.clear();
                        self.drag = None;
                        self.bump_revision();
                    }
                }
            }
            other => {
                // Начало создания фигуры.
                self.drag = Some(Drag {
                    tool: other,
                    anchor_screen: screen,
                    current_screen: screen,
                    grabbed: None,
                    last_world: self.camera.screen_to_world(screen),
                    grab_world: Vec2::ZERO,
                    start_pos: Vec2::ZERO,
                });
                self.dirty = true;
            }
        }
    }

    pub fn pointer_move(&mut self, screen: Vec2) {
        let Some(mut d) = self.drag.take() else { return };
        d.current_screen = screen;

        match d.tool {
            Tool::Pan => {
                let delta = d.current_screen - d.anchor_screen;
                self.camera.pan_by(delta);
                d.anchor_screen = d.current_screen;
                self.dirty = true;
            }
            Tool::Select => {
                if let Some(id) = d.grabbed {
                    let world = self.camera.screen_to_world(screen);
                    d.last_world = world;
                    // Снап к итоговой позиции: без дрейфа, т.к. база — start_pos.
                    let target = self.snap_point(d.start_pos + (world - d.grab_world));
                    if let Some(n) = self.scene.get_mut(id) {
                        n.transform = Affine2::from_translation(target);
                    }
                    self.dirty = true;
                }
            }
            _ => {
                // Превью создаваемой фигуры (привязка к сетке/пикселям).
                let a = self.snap_point(self.camera.screen_to_world(d.anchor_screen));
                let b = self.snap_point(self.camera.screen_to_world(screen));
                self.preview = Some(Preview {
                    a,
                    b,
                    kind: kind_for_tool(d.tool),
                });
                self.dirty = true;
            }
        }
        self.drag = Some(d);
    }

    pub fn pointer_up(&mut self, screen: Vec2) {
        let Some(d) = self.drag.take() else { return };
        self.preview = None;

        match d.tool {
            Tool::Rectangle | Tool::Ellipse | Tool::Line | Tool::Frame => {
                let a = self.snap_point(self.camera.screen_to_world(d.anchor_screen));
                let b = self.snap_point(self.camera.screen_to_world(screen));
                let screen_dist = (d.current_screen - d.anchor_screen).length();
                if screen_dist >= 3.0 {
                    self.record();
                    let id = self.add_root_node(d.tool, a, b);
                    self.scene.selection = vec![id];
                }
            }
            Tool::Select | Tool::Pan => {}
        }
        self.dirty = true;
    }

    // --- Зум ---

    pub fn zoom(&mut self, delta_y: f32, screen: Vec2) {
        let factor = if delta_y > 0.0 { 1.1 } else { 1.0 / 1.1 };
        self.camera.zoom_at(factor, screen);
        self.dirty = true;
    }

    // --- Свойства выделенного узла ---

    pub fn selected_id(&self) -> Option<NodeId> {
        self.scene.selection.first().copied()
    }

    pub fn selected(&self) -> Option<&SceneNode> {
        self.selected_id().and_then(|id| self.scene.get(id))
    }

    fn mutate_selected(&mut self, f: impl FnOnce(&mut SceneNode)) {
        let Some(id) = self.selected_id() else { return };
        self.record();
        if let Some(n) = self.scene.get_mut(id) {
            f(n);
        }
        self.dirty = true;
    }

    pub fn set_position(&mut self, x: f32, y: f32) {
        let snapped = self.snap_point(Vec2::new(x, y));
        self.mutate_selected(|n| {
            n.transform = Affine2::from_translation(snapped);
        });
    }

    pub fn set_size(&mut self, w: f32, h: f32) {
        let snapped = self.snap_size(Vec2::new(w.max(1.0), h.max(1.0)));
        self.mutate_selected(|n| match &mut n.kind {
            NodeKind::Frame { w: nw, h: nh }
            | NodeKind::Rectangle { w: nw, h: nh }
            | NodeKind::Ellipse { w: nw, h: nh } => {
                *nw = snapped.x;
                *nh = snapped.y;
            }
            _ => {}
        });
    }

    pub fn set_opacity(&mut self, v: f32) {
        self.mutate_selected(|n| n.opacity = v.clamp(0.0, 1.0));
    }

    pub fn set_name(&mut self, name: &str) {
        self.mutate_selected(|n| n.name = name.to_string());
        self.bump_revision();
    }

    pub fn set_fill_hex(&mut self, hex: &str) {
        if let Some(rgb) = parse_hex(hex) {
            self.mutate_selected(|n| n.fill = Some(Fill { color: [rgb[0], rgb[1], rgb[2], 255] }));
        }
    }

    // --- Сериализация ---

    pub fn save(&self) -> Result<String, String> {
        save_json(&self.scene).map_err(|e| e.to_string())
    }

    pub fn load(&mut self, data: &str) -> Result<(), String> {
        let scene = load_json(data).map_err(|e| e.to_string())?;
        self.record();
        self.scene = scene;
        self.bump_revision();
        Ok(())
    }
}

fn kind_for_tool(tool: Tool) -> NodeKind {
    match tool {
        Tool::Rectangle => NodeKind::Rectangle { w: 1.0, h: 1.0 },
        Tool::Ellipse => NodeKind::Ellipse { w: 1.0, h: 1.0 },
        Tool::Line => NodeKind::Line { x2: 1.0, y2: 1.0 },
        Tool::Frame => NodeKind::Frame { w: 1.0, h: 1.0 },
        _ => NodeKind::Rectangle { w: 1.0, h: 1.0 },
    }
}

/// Парсит шестнадцатеричный цвет вида "#RRGGBB" или "#RRGGBBAA".
fn parse_hex(s: &str) -> Option<[u8; 3]> {
    let s = s.trim_start_matches('#');
    if s.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&s[0..2], 16).ok()?;
    let g = u8::from_str_radix(&s[2..4], 16).ok()?;
    let b = u8::from_str_radix(&s[4..6], 16).ok()?;
    Some([r, g, b])
}

impl Default for CanvasController {
    fn default() -> Self {
        Self::new()
    }
}