use super::grid::GridConfig;
use super::history::History;
use super::model::document::Document;
use super::model::nodes::{NodeKey, NodeKind, ShapeKind};
use super::model::scene::{SceneGraph, SceneNode};
use super::model::types::{Color, Constraints, Paint, Stroke};
use super::tool::Tool;
use super::transform::{pick, Camera};
use crate::engine::serialize::{load_json, save_json};
use glam::{Affine2, Vec2};

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
    grabbed: Option<NodeKey>,
    /// Последняя мировая позиция курсора (для корректного перемещения).
    last_world: Vec2,
    /// Мировая позиция курсора в момент захвата (Select tool).
    grab_world: Vec2,
    /// Позиция узла в момент захвата (для снапа без дрейфа).
    start_pos: Vec2,
}

/// Контроллер холста: связывает граф сцены, камеру, инструменты и историю.
pub struct CanvasController {
    pub scene: SceneGraph,
    pub camera: Camera,
    history: History,
    pub tool: Tool,
    drag: Option<Drag>,
    pub preview: Option<Preview>,
    /// Флаг «состояние изменилось» — движок рендера подхватывает на следующем тике.
    pub dirty: bool,
    /// Ревизия структуры сцены — для пересборки дерева слоёв в UI.
    pub revision: u64,
    /// Показывать сетку, привязка, шаг.
    pub grid: GridConfig,
}

impl CanvasController {
    pub fn new() -> Self {
        let mut c = Self {
            scene: SceneGraph::new(),
            camera: Camera::new(),
            history: History::new(100),
            tool: Tool::Select,
            drag: None,
            preview: None,
            dirty: true,
            revision: 0,
            grid: GridConfig::new(),
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
            self.scene.clear_selection();
            self.bump_revision();
        }
    }

    pub fn redo(&mut self) {
        if let Some(next) = self.history.redo(&self.scene) {
            self.scene = next;
            self.scene.clear_selection();
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
        self.grid.visible = !self.grid.visible;
        self.dirty = true;
    }

    pub fn toggle_snap(&mut self) {
        self.grid.snap = !self.grid.snap;
        self.dirty = true;
    }

    /// Устанавливает шаг сетки (в мировых px).
    pub fn set_grid_step(&mut self, step: f32) {
        self.grid.step = step.clamp(1.0, 256.0);
        self.dirty = true;
    }

    /// Сбрасывает камеру (zoom = 100%, без панорамирования).
    pub fn reset_view(&mut self) {
        self.camera = Camera::new();
        self.dirty = true;
    }

    /// Привязка точки к сетке (если включена) и всегда к целым пикселям.
    pub fn snap_point(&self, p: Vec2) -> Vec2 {
        self.grid.snap_point(p)
    }

    /// Привязка размера: только к целым пикселям (сетка на размеры не влияет).
    fn snap_size(&self, p: Vec2) -> Vec2 {
        self.grid.snap_size(p)
    }

    /// Создаёт корневой узел по инструменту из прямоугольника (anchor..current).
    fn add_root_node(&mut self, tool: Tool, a: Vec2, b: Vec2) -> NodeKey {
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
            Tool::Rectangle => {
                NodeKind::Shape(ShapeKind::Rectangle { size, corner_radii: [0.0; 4] })
            }
            Tool::Ellipse => NodeKind::Shape(ShapeKind::Ellipse {
                radii: size,
                start_angle: 0.0,
                end_angle: std::f32::consts::TAU,
                inner_radius_ratio: 0.0,
            }),
            // Линия локально идёт из (0,0) в (b-a); трансляция в `a`.
            Tool::Line => {
                NodeKind::Shape(ShapeKind::Line { start: Vec2::ZERO, end: b - a })
            }
            Tool::Frame => NodeKind::Frame {
                size,
                clip_content: false,
                corner_radii: [0.0; 4],
                auto_layout: None,
                constraints: Constraints::default(),
            },
            _ => return NodeKey::default(),
        };

        let key = self.scene.insert_root(name, kind);
        if let Some(n) = self.scene.get_mut(key) {
            // Линия позиционируется от точки старта (a), а не от min: иначе при
            // перетаскивании в отрицательном направлении она смещалась бы на (min - a).
            n.local_transform = Affine2::from_translation(if tool == Tool::Line { a } else { min });
            n.fills = vec![Paint::Solid(Color::from_rgba8(122, 170, 233, 255))];
            n.strokes = vec![Stroke::solid(Color::from_rgba8(0, 0, 0, 200), 1.0)];
        }
        self.bump_revision();
        key
    }

    pub fn delete_selection(&mut self) {
        if self.scene.selection().is_empty() {
            return;
        }
        self.record();
        let sel = self.scene.selection().to_vec();
        for key in sel {
            self.scene.remove(key);
        }
        self.bump_revision();
    }

    /// Выбирает узел по ключу (из дерева слоёв).
    pub fn select(&mut self, id: NodeKey) {
        if self.scene.contains(id) {
            self.scene.set_selection(vec![id]);
            self.bump_revision();
        }
    }

    /// Сбрасывает документ (новый пустой файл).
    pub fn clear(&mut self) {
        self.record();
        self.scene = SceneGraph::new();
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
                match pick(&mut self.scene, world) {
                    Some(key) => {
                        // Захват для перемещения.
                        self.record();
                        let start_pos = self
                            .scene
                            .get(key)
                            .map(|n| n.local_transform.translation)
                            .unwrap_or(Vec2::ZERO);
                        self.scene.clear_selection();
                        self.scene.add_to_selection(key);
                        self.drag = Some(Drag {
                            tool: Tool::Select,
                            anchor_screen: screen,
                            current_screen: screen,
                            grabbed: Some(key),
                            last_world: world,
                            grab_world: world,
                            start_pos,
                        });
                        self.bump_revision();
                    }
                    None => {
                        self.scene.clear_selection();
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
                if let Some(key) = d.grabbed {
                    let world = self.camera.screen_to_world(screen);
                    d.last_world = world;
                    // Снап к итоговой позиции: без дрейфа, т.к. база — start_pos.
                    let target = self.snap_point(d.start_pos + (world - d.grab_world));
                    self.scene.set_transform(key, Affine2::from_translation(target));
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
                    let key = self.add_root_node(d.tool, a, b);
                    self.scene.set_selection(vec![key]);
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

    pub fn selected_id(&self) -> Option<NodeKey> {
        self.scene.selection().first().copied()
    }

    pub fn selected(&self) -> Option<&SceneNode> {
        self.selected_id().and_then(|key| self.scene.get(key))
    }

    fn mutate_selected(&mut self, f: impl FnOnce(&mut SceneNode)) {
        let Some(id) = self.selected_id() else { return };
        self.record();
        if let Some(n) = self.scene.get_mut(id) {
            f(n);
        }
        // Прямое изменение может затронуть геометрию/трансформацию — инвалидируем.
        self.scene.mark_subtree_dirty(id);
        self.dirty = true;
    }

    pub fn set_position(&mut self, x: f32, y: f32) {
        let snapped = self.snap_point(Vec2::new(x, y));
        self.mutate_selected(|n| {
            n.local_transform = Affine2::from_translation(snapped);
        });
    }

    pub fn set_size(&mut self, w: f32, h: f32) {
        let snapped = self.snap_size(Vec2::new(w.max(1.0), h.max(1.0)));
        self.mutate_selected(|n| match &mut n.kind {
            NodeKind::Frame { size, .. } => {
                *size = snapped;
            }
            NodeKind::Shape(ShapeKind::Rectangle { size, .. }) => {
                *size = snapped;
            }
            NodeKind::Shape(ShapeKind::Ellipse { radii, .. }) => {
                *radii = snapped;
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
            self.mutate_selected(|n| {
                n.fills = vec![Paint::Solid(Color::from_rgba8(rgb[0], rgb[1], rgb[2], 255))];
            });
        }
    }

    // --- Сериализация ---

    pub fn save(&self) -> Result<String, String> {
        // Сцена — основной контент; страницы/стили пока дефолтные.
        let mut doc = Document::new();
        let roots = self.scene.roots().to_vec();
        if let Some(page) = doc.active_page_mut() {
            page.top_level = roots;
        }
        doc.scene = self.scene.clone();
        save_json(&doc).map_err(|e| e.to_string())
    }

    pub fn load(&mut self, data: &str) -> Result<(), String> {
        let doc = load_json(data).map_err(|e| e.to_string())?;
        self.record();
        self.scene = doc.scene;
        self.bump_revision();
        Ok(())
    }
}

fn kind_for_tool(tool: Tool) -> NodeKind {
    match tool {
        Tool::Rectangle => {
            NodeKind::Shape(ShapeKind::Rectangle { size: Vec2::ONE, corner_radii: [0.0; 4] })
        }
        Tool::Ellipse => NodeKind::Shape(ShapeKind::Ellipse {
            radii: Vec2::ONE,
            start_angle: 0.0,
            end_angle: std::f32::consts::TAU,
            inner_radius_ratio: 0.0,
        }),
        Tool::Line => NodeKind::Shape(ShapeKind::Line { start: Vec2::ZERO, end: Vec2::ONE }),
        Tool::Frame => NodeKind::Frame {
            size: Vec2::ONE,
            clip_content: false,
            corner_radii: [0.0; 4],
            auto_layout: None,
            constraints: Constraints::default(),
        },
        _ => NodeKind::Shape(ShapeKind::Rectangle { size: Vec2::ONE, corner_radii: [0.0; 4] }),
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