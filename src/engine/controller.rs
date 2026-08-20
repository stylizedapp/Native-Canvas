use super::gizmo::{self, Handle};
use super::grid::GridConfig;
use super::history::History;
use super::model::document::Document;
use super::model::nodes::{NodeKey, NodeKind, ShapeKind};
use super::model::scene::{SceneGraph, SceneNode};
use super::model::types::{
    AutoLayoutConfig, Color, Constraints, LayoutAlign, LayoutDirection, LayoutJustify, Paint, Stroke,
    TextAlign, TextSizeMode,
};
use super::tool::Tool;
use super::transform::{pick, pick_stack, Camera};
use crate::engine::serialize::{load_json, save_json};
use glam::{Affine2, Vec2};
use std::time::{Duration, Instant};

/// Live-превью создаваемой фигуры (в мировых координатах).
#[derive(Clone, Debug)]
pub struct Preview {
    pub a: Vec2,
    pub b: Vec2,
    pub kind: NodeKind,
}

/// Режим перетаскивания (Select tool и не только).
#[derive(Clone, PartialEq)]
enum DragMode {
    Pan,
    /// Перемещение захваченного узла.
    Move { key: NodeKey },
    /// Перемещение всего выделения (мультивыбор).
    MoveMany { keys: Vec<NodeKey> },
    /// Ресайз через хэндл гизмо.
    Resize { key: NodeKey, handle: Handle },
    /// Создание фигуры (Rect/Ellipse/Line/Frame).
    Create(Tool),
    /// Рамочное выделение.
    Marquee,
}

/// Промежуточное состояние перетаскивания (координаты экранные).
struct Drag {
    tool: Tool,
    mode: DragMode,
    anchor_screen: Vec2,
    current_screen: Vec2,
    /// Последняя мировая позиция курсора (для корректного перемещения).
    last_world: Vec2,
    /// Мировая позиция курсора в момент захвата (Move).
    grab_world: Vec2,
    /// Позиция узла в момент захвата (Move, для снапа без дрейфа).
    start_pos: Vec2,
    /// Стартовые позиции всех узлов мультивыбора (MoveMany).
    start_positions: Vec<Vec2>,
    /// Исходное мировое AABB в момент захвата (Resize).
    start_mn: Vec2,
    start_mx: Vec2,
    /// Мировая точка старта рамки (Marquee).
    start_world: Vec2,
}

impl Drag {
    fn new(tool: Tool, mode: DragMode, screen: Vec2, world: Vec2) -> Self {
        Self {
            tool,
            mode,
            anchor_screen: screen,
            current_screen: screen,
            last_world: world,
            grab_world: world,
            start_pos: Vec2::ZERO,
            start_positions: Vec::new(),
            start_mn: Vec2::ZERO,
            start_mx: Vec2::ZERO,
            start_world: world,
        }
    }
}

/// Трансформация с сохранённым поворотом/масштабом и новой трансляцией:
/// перенос/позиционирование не сбрасывает rotation/scale узла.
fn translate_keep_rotation(cur: Affine2, t: Vec2) -> Affine2 {
    Affine2::from_cols(cur.matrix2.x_axis, cur.matrix2.y_axis, t)
}

/// Контроллер холста: связывает граф сцены, камеру, инструменты и историю.
pub struct CanvasController {
    pub scene: SceneGraph,
    pub camera: Camera,
    history: History,
    pub tool: Tool,
    drag: Option<Drag>,
    pub preview: Option<Preview>,
    /// Текущая рамка марки-выделения (мировые a/b), рисуется оверлеем.
    pub marquee: Option<(Vec2, Vec2)>,
    /// Флаг «состояние изменилось» — движок рендера подхватывает на следующем тике.
    pub dirty: bool,
    /// Счётчик всех мутаций (для debug-инварианта «мутация без dirty»).
    pub ops: u64,
    /// Ревизия структуры сцены — для пересборки дерева слоёв в UI.
    pub revision: u64,
    /// Показывать сетку, привязка, шаг.
    pub grid: GridConfig,
    /// Якорь диапазонного выделения в панели слоёв (Shift+click).
    pub layer_anchor: Option<NodeKey>,
    /// Внутренний буфер обмена (копии выделенных поддеревьев).
    pub clipboard: Option<Clipboard>,
    /// Фрейм-цель под курсором во время перетаскивания (подсветка + drop).
    pub hovered_frame: Option<NodeKey>,
    /// Последняя flex-конфигурация фрейма — сохраняется при выключении, чтобы
    /// повторное включение не сбрасывало направление/выравнивание.
    last_layout: Option<AutoLayoutConfig>,
    /// Текстовый узел, который сейчас редактируется инлайн-оверлеем.
    pub editing: Option<NodeKey>,
    /// (время, ключ) последнего клика в Select — для детекта двойного клика.
    last_pick: Option<(Instant, NodeKey)>,
}

/// Буфер обмена: временный граф с копиями выбранных поддеревьев.
#[derive(Default)]
pub struct Clipboard {
    pub graph: SceneGraph,
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
            marquee: None,
            dirty: true,
            ops: 0,
            revision: 0,
            grid: GridConfig::new(),
            layer_anchor: None,
            clipboard: None,
            hovered_frame: None,
            last_layout: None,
            editing: None,
            last_pick: None,
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
        self.touch();
    }

    /// Помечает состояние «изменилось» и увеличивает счётчик мутаций.
    fn touch(&mut self) {
        self.ops += 1;
        self.dirty = true;
    }

    pub fn undo(&mut self) {
        if self.history.undo(&mut self.scene) {
            self.scene.clear_selection();
            self.bump_revision();
        }
    }

    pub fn redo(&mut self) {
        if self.history.redo(&mut self.scene) {
            self.scene.clear_selection();
            self.bump_revision();
        }
    }

    // --- Инструменты и действия ---

    pub fn set_tool(&mut self, tool: Tool) {
        self.tool = tool;
        self.drag = None;
        self.preview = None;
        self.marquee = None;
        self.editing = None;
        self.touch();
    }

    // --- Инлайн-редактирование текста ---

    /// Открывает редактирование текстового узла (возвращает false, если узел
    /// не текст). Оверлей рисует UI-слой (app.rs) по данным [`CanvasController::editing_view`].
    pub fn begin_text_edit(&mut self, key: NodeKey) -> bool {
        let is_text = self
            .scene
            .get(key)
            .map(|n| matches!(n.kind, NodeKind::Text { .. }))
            .unwrap_or(false);
        if !is_text {
            return false;
        }
        self.scene.set_selection(vec![key]);
        self.editing = Some(key);
        self.touch();
        true
    }

    /// Завершает редактирование (фокус ушёл с оверлея или Esc).
    pub fn end_text_edit(&mut self) {
        if self.editing.take().is_some() {
            self.record();
            self.touch();
        }
    }

    /// Обновляет контент текстового узла в процессе редактирования (live).
    pub fn set_editing_text(&mut self, content: &str) {
        let Some(key) = self.editing else { return };
        let Some(n) = self.scene.get_mut(key) else { return };
        if let NodeKind::Text { content: c, .. } = &mut n.kind {
            if *c != content {
                *c = content.to_string();
                self.scene.mark_subtree_dirty(key);
                self.touch();
            }
        }
    }

    /// Данные для позиционирования инлайн-оверлея в экранных координатах:
    /// (контент, позиция, размер, размер шрифта в px экрана, цвет заливки).
    pub fn editing_view(&self) -> Option<(String, Vec2, Vec2, f32, Color)> {
        let key = self.editing?;
        let node = self.scene.get(key)?;
        let NodeKind::Text { content, font_size, .. } = &node.kind else {
            return None;
        };
        let (mn, mx) = self.scene.world_bbox(key)?;
        let tl = self.camera.world_to_screen(mn);
        let br = self.camera.world_to_screen(mx);
        let color = node
            .fills
            .iter()
            .find_map(|p| match p {
                Paint::Solid(c) => Some(*c),
                _ => None,
            })
            .unwrap_or(Color::from_rgba8(232, 232, 232, 255));
        Some((
            content.clone(),
            tl,
            Vec2::new((br.x - tl.x).max(40.0), (br.y - tl.y).max(8.0)),
            *font_size * self.camera.zoom,
            color,
        ))
    }

    // --- Сетка / снап ---

    pub fn toggle_grid(&mut self) {
        self.grid.visible = !self.grid.visible;
        self.touch();
    }

    pub fn set_grid_visible(&mut self, v: bool) {
        self.grid.visible = v;
        self.touch();
    }

    pub fn toggle_snap(&mut self) {
        self.grid.snap = !self.grid.snap;
        self.touch();
    }

    pub fn set_snap(&mut self, v: bool) {
        self.grid.snap = v;
        self.touch();
    }

    /// Устанавливает шаг сетки (в мировых px).
    pub fn set_grid_step(&mut self, step: f32) {
        self.grid.step = step.clamp(1.0, 256.0);
        self.touch();
    }

    /// Сбрасывает камеру (zoom = 100%, без панорамирования).
    pub fn reset_view(&mut self) {
        self.camera = Camera::new();
        self.touch();
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
            Tool::Text => "Text",
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
            Tool::Text => NodeKind::Text {
                content: "Text".into(),
                font_family: "Inter".into(),
                font_size: 16.0,
                font_weight: 400,
                line_height: 1.2,
                letter_spacing: 0.0,
                align: TextAlign::Left,
                size_mode: TextSizeMode::AutoWidth,
            },
            _ => return NodeKey::default(),
        };

        let key = self.scene.insert_root(name, kind);
        if let Some(n) = self.scene.get_mut(key) {
            // Линия позиционируется от точки старта (a), а не от min: иначе при
            // перетаскивании в отрицательном направлении она смещалась бы на (min - a).
            // Текст (AutoWidth) — от точки клика; размер высчитывается по контенту.
            n.local_transform = Affine2::from_translation(if tool == Tool::Line || tool == Tool::Text {
                a
            } else {
                min
            });
            if tool == Tool::Text {
                n.fills = vec![Paint::Solid(Color::from_rgba8(232, 232, 232, 255))];
                n.strokes.clear();
            } else {
                n.fills = vec![Paint::Solid(Color::from_rgba8(122, 170, 233, 255))];
                n.strokes = vec![Stroke::solid(Color::from_rgba8(0, 0, 0, 200), 1.0)];
            }
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

    /// Дублирует всё выделение; копии появляются сиблингами (+16 px вниз-вправо)
    /// и становятся новым выделением.
    pub fn duplicate_selection(&mut self) {
        let sel = self.scene.selection().to_vec();
        if sel.is_empty() {
            return;
        }
        self.record();
        let mut new_keys = Vec::new();
        for key in sel {
            if let Some(nk) = self.scene.duplicate(key) {
                new_keys.push(nk);
            }
        }
        if new_keys.is_empty() {
            return;
        }
        // Копии смещаем, чтобы они не лежали ровно поверх оригинала.
        let offset = Vec2::new(16.0, 16.0);
        for k in &new_keys {
            if let Some(n) = self.scene.get_mut(*k) {
                let t = n.local_transform;
                n.local_transform = Affine2::from_translation(offset) * t;
            }
            self.scene.mark_subtree_dirty(*k);
        }
        self.scene.flush_transforms();
        self.scene.set_selection(new_keys);
        self.bump_revision();
    }

    /// Переносит верхнеуровневые узлы выделения в целевой фрейм (или в корни
    /// при `None`) с сохранением мировой позиции. Узлы, уже находящиеся
    /// в целевом фрейме, пропускаются.
    pub fn reparent_selection(&mut self, target: Option<NodeKey>) {
        if let Some(t) = target {
            if !self.scene.contains(t) {
                return;
            }
        }
        let sel: Vec<NodeKey> = top_level_selected(&self.scene)
            .into_iter()
            .filter(|k| self.scene.get(*k).and_then(|n| n.parent) != target)
            .collect();
        if sel.is_empty() {
            return;
        }
        if let Some(t) = target {
            if sel.contains(&t) {
                return;
            }
        }
        self.record();
        for key in &sel {
            self.scene.reparent_preserve_world(*key, target);
        }
        self.scene.flush_transforms();
        self.scene.set_selection(sel);
        self.bump_revision();
    }

    /// Обёртывает выделение во Frame («Frame selection», как Ctrl+Alt+G в Figma):
    /// создаёт фрейм по общей рамке выделения и переносит узлы внутрь.
    pub fn wrap_in_frame(&mut self) {
        let sel = top_level_selected(&self.scene);
        if sel.is_empty() {
            return;
        }
        self.scene.flush_transforms();
        let mut mn = Vec2::splat(f32::INFINITY);
        let mut mx = Vec2::splat(f32::NEG_INFINITY);
        for &k in &sel {
            if let Some((a, b)) = self.scene.world_bbox(k) {
                mn = mn.min(a);
                mx = mx.max(b);
            }
        }
        let pad = 8.0;
        let size = (mx - mn + Vec2::splat(pad * 2.0)).max(Vec2::new(1.0, 1.0));
        self.record();
        let frame_key = self.scene.insert_root(
            "Frame",
            NodeKind::Frame {
                size,
                clip_content: false,
                corner_radii: [0.0; 4],
                auto_layout: None,
                constraints: Constraints::default(),
            },
        );
        if let Some(n) = self.scene.get_mut(frame_key) {
            n.local_transform = Affine2::from_translation(mn - Vec2::splat(pad));
        }
        self.scene.mark_subtree_dirty(frame_key);
        self.scene.flush_transforms();
        for key in &sel {
            self.scene.reparent_preserve_world(*key, Some(frame_key));
        }
        self.scene.flush_transforms();
        self.scene.set_selection(vec![frame_key]);
        self.bump_revision();
    }

    /// Верхний по стеку видимый Frame под мировой точкой, который не входит
    /// в перемещаемый набор (и не является его потомком).
    fn drop_frame_at(&mut self, world: Vec2, moving: &[NodeKey]) -> Option<NodeKey> {
        let stack = pick_stack(&mut self.scene, world);
        for k in stack {
            let is_moving = moving
                .iter()
                .any(|&m| k == m || self.scene.is_descendant(k, m));
            if is_moving {
                continue;
            }
            let is_frame = self
                .scene
                .get(k)
                .map(|n| matches!(n.kind, NodeKind::Frame { .. }))
                .unwrap_or(false);
            if is_frame {
                return Some(k);
            }
        }
        None
    }

    /// Копирует выделенные поддеревья во внутренний буфер обмена.
    pub fn copy_selection(&mut self) {
        let sel = top_level_selected(&self.scene);
        if sel.is_empty() {
            return;
        }
        let mut temp = SceneGraph::new();
        for key in &sel {
            if let Some(nk) = self.scene.clone_into(&mut temp, *key) {
                temp.add_root(nk);
            }
        }
        self.clipboard = Some(Clipboard { graph: temp });
    }

    /// Вырезает выделение в буфер обмена.
    pub fn cut_selection(&mut self) {
        self.copy_selection();
        self.delete_selection();
    }

    /// Вставляет копию из буфера обмена со смещением +16 (Ctrl+V).
    pub fn paste(&mut self) {
        self.paste_inner(true);
    }

    /// Вставляет копию без смещения — в исходное место (Shift+V).
    pub fn paste_in_place(&mut self) {
        self.paste_inner(false);
    }

    fn paste_inner(&mut self, offset: bool) {
        let temp = match &self.clipboard {
            Some(cb) => cb.graph.clone(),
            None => return,
        };
        if temp.roots().is_empty() {
            return;
        }
        self.record();
        let mut new_keys = Vec::new();
        for root in temp.roots().to_vec() {
            if let Some(nk) = self.scene.insert_clone_from(&temp, root, None) {
                new_keys.push(nk);
            }
        }
        if new_keys.is_empty() {
            return;
        }
        if offset {
            let off = Vec2::new(16.0, 16.0);
            for k in &new_keys {
                if let Some(n) = self.scene.get_mut(*k) {
                    let t = n.local_transform;
                    n.local_transform = Affine2::from_translation(off) * t;
                }
                self.scene.mark_subtree_dirty(*k);
            }
        }
        self.scene.flush_transforms();
        self.scene.set_selection(new_keys);
        self.bump_revision();
    }

    /// Переключает блокировку всего выделения (одно состояние для всех).
    pub fn toggle_lock_selection(&mut self) {
        let keys = self.scene.selection().to_vec();
        if keys.is_empty() {
            return;
        }
        self.record();
        let any_unlocked = keys
            .iter()
            .any(|k| self.scene.get(*k).map(|n| !n.is_locked).unwrap_or(false));
        for k in &keys {
            if let Some(n) = self.scene.get_mut(*k) {
                n.is_locked = any_unlocked;
            }
        }
        self.bump_revision();
    }

    /// Переключает видимость всего выделения.
    pub fn toggle_hide_selection(&mut self) {
        let keys = self.scene.selection().to_vec();
        if keys.is_empty() {
            return;
        }
        self.record();
        let any_visible = keys
            .iter()
            .any(|k| self.scene.get(*k).map(|n| n.is_visible).unwrap_or(false));
        for k in &keys {
            if let Some(n) = self.scene.get_mut(*k) {
                n.is_visible = !any_visible;
            }
        }
        self.bump_revision();
    }

    /// Выбирает все узлы сцены.
    pub fn select_all(&mut self) {
        let all = self.scene.walk();
        if all.is_empty() {
            return;
        }
        self.scene.set_selection(all);
        self.bump_revision();
    }

    /// Центрирует камеру на рамке выделения (~70% вьюпорта).
    pub fn zoom_to_selection(&mut self) {
        self.scene.flush_transforms();
        let Some((mn, mx)) = self.selection_bbox() else {
            return;
        };
        let size = mx - mn;
        let center = (mn + mx) * 0.5;
        let target = (700.0 / size.y.max(1.0))
            .min(1000.0 / size.x.max(1.0))
            .clamp(0.05, 50.0)
            * 0.8;
        self.camera.zoom = target;
        self.camera.pan = Vec2::new(500.0, 350.0) - center * target;
        self.touch();
    }

    /// Центрирует камеру на всём содержимом сцены (Shift+1, fit all).
    pub fn fit_to_content(&mut self) {
        self.scene.flush_transforms();
        let keys = self.scene.walk();
        let Some(mn) = keys
            .iter()
            .filter_map(|k| self.scene.world_bbox(*k).map(|(a, _)| a))
            .reduce(|a, b| a.min(b))
        else {
            self.reset_view();
            return;
        };
        let Some(mx) = keys
            .iter()
            .filter_map(|k| self.scene.world_bbox(*k).map(|(_, b)| b))
            .reduce(|a, b| a.max(b))
        else {
            self.reset_view();
            return;
        };
        let size = mx - mn;
        let center = (mn + mx) * 0.5;
        let target = (700.0 / size.y.max(1.0))
            .min(1000.0 / size.x.max(1.0))
            .clamp(0.05, 50.0)
            * 0.85;
        self.camera.zoom = target;
        self.camera.pan = Vec2::new(500.0, 350.0) - center * target;
        self.touch();
    }

    /// Сдвигает выделение на дельту в мировых координатах (nudge стрелками).
    /// Работает и с мультивыделением (общий сдвиг группы).
    pub fn nudge(&mut self, dx: f32, dy: f32) {
        if self.scene.selection().is_empty() {
            return;
        }
        if dx == 0.0 && dy == 0.0 {
            return;
        }
        self.apply_position_delta_live(dx, dy);
        self.bump_revision();
    }

    /// Выбирает узел по ключу (из дерева слоёв).
    pub fn select(&mut self, id: NodeKey) {
        if self.scene.contains(id) {
            self.scene.set_selection(vec![id]);
            self.bump_revision();
        }
    }

    /// Выбор из панели слоёв с модификаторами.
    /// ctrl — переключает узел в выделении; shift — диапазон от якоря до id.
    pub fn select_layer(&mut self, id: NodeKey, ctrl: bool, shift: bool) {
        if !self.scene.contains(id) {
            return;
        }
        if shift {
            let anchor = self
                .layer_anchor
                .filter(|a| self.scene.contains(*a))
                .unwrap_or(id);
            let order = self.scene.walk();
            let (Some(pa), Some(pb)) = (
                order.iter().position(|k| *k == anchor),
                order.iter().position(|k| *k == id),
            ) else {
                return;
            };
            // Диапазон в порядке списка слоёв, включая оба конца.
            let (lo, hi) = (pa.min(pb), pa.max(pb));
            self.scene.set_selection(order[lo..=hi].to_vec());
            self.bump_revision();
        } else if ctrl {
            self.scene.toggle_selection(id);
            self.layer_anchor = Some(id);
            self.bump_revision();
        } else {
            self.scene.set_selection(vec![id]);
            self.layer_anchor = Some(id);
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
        self.pointer_down_mod(screen, button, false, false);
    }

    /// `pointer_down` с модификаторами: `ctrl` — deep-select (Ctrl+клик),
    /// `space` — временный Pan (Space+Drag) независимо от инструмента.
    pub fn pointer_down_mod(&mut self, screen: Vec2, button: u8, ctrl: bool, space: bool) {
        self.pointer_down_full(screen, button, ctrl, false, space);
    }

    /// Полная версия с `alt` (Alt+Drag — дублирование при перетаскивании).
    pub fn pointer_down_full(&mut self, screen: Vec2, button: u8, ctrl: bool, alt: bool, space: bool) {
        // Клик по холсту во время редактирования текста завершает его
        // (оверлей-TextInput лежит поверх TouchArea и свои клики не пропускает).
        if self.editing.is_some() {
            self.end_text_edit();
        }
        // Мировые трансформации актуальны для хит-теста хэндлов и pick.
        self.scene.flush_transforms();

        // Средняя кнопка, инструмент Pan или зажатый пробел — панорамирование.
        if button == 2 || self.tool == Tool::Pan || space {
            let world = self.camera.screen_to_world(screen);
            self.drag = Some(Drag::new(Tool::Pan, DragMode::Pan, screen, world));
            self.touch();
            return;
        }

        // ПКМ: только подбираем узел под курсор (меню открывается на up),
        // без начала перетаскивания.
        if button == 3 {
            let world = self.camera.screen_to_world(screen);
            if let Some(key) = pick(&mut self.scene, world) {
                if !self.scene.selection().contains(&key) {
                    self.scene.set_selection(vec![key]);
                    self.layer_anchor = Some(key);
                    self.bump_revision();
                }
            }
            return;
        }

        // Ctrl+клик (Select): deep-select — берём следующий ниже по стеку узел,
        // ещё не входящий в выделение.
        if ctrl && button == 1 && self.tool == Tool::Select {
            let world = self.camera.screen_to_world(screen);
            let stack = pick_stack(&mut self.scene, world);
            if let Some(key) = stack.into_iter().find(|k| !self.scene.selection().contains(k)) {
                self.scene.set_selection(vec![key]);
                self.layer_anchor = Some(key);
                self.bump_revision();
                return;
            }
        }

        // Alt+Drag (Select): дублируем узлы под курсором и тащим копии.
        if alt && button == 1 && self.tool == Tool::Select {
            let world = self.camera.screen_to_world(screen);
            if let Some(key) = pick(&mut self.scene, world) {
                self.record();
                let keys = if self.scene.selection().contains(&key) {
                    self.scene.selection().to_vec()
                } else {
                    vec![key]
                };
                let mut copies = Vec::new();
                for k in keys {
                    if let Some(nk) = self.scene.duplicate(k) {
                        copies.push(nk);
                    }
                }
                if copies.is_empty() {
                    return;
                }
                self.scene.set_selection(copies.clone());
                self.layer_anchor = Some(copies[0]);
                let start_positions = copies
                    .iter()
                    .map(|k| {
                        self.scene
                            .get(*k)
                            .map(|n| n.local_transform.translation)
                            .unwrap_or(Vec2::ZERO)
                    })
                    .collect();
                self.drag = Some(Drag {
                    mode: DragMode::MoveMany { keys: copies },
                    start_positions,
                    ..Drag::new(Tool::Select, DragMode::Move { key }, screen, world)
                });
                self.bump_revision();
                return;
            }
        }

        match self.tool {
            Tool::Select => {
                let world = self.camera.screen_to_world(screen);
                // 1. Попадание в ресайз-хэндл гизмо (ровно 1 выделенный, ресайзабельный).
                if self.scene.selection().len() == 1 {
                    let key = self.scene.selection()[0];
                    let resizable = self
                        .scene
                        .get(key)
                        .map(|n| gizmo::resizable(&n.kind))
                        .unwrap_or(false);
                    if resizable {
                        if let Some((mn, mx)) = self.scene.world_bbox(key) {
                            if let Some(handle) = gizmo::handle_at(world, mn, mx, self.camera.zoom) {
                                self.record();
                                self.drag = Some(Drag {
                                    mode: DragMode::Resize { key, handle },
                                    start_mn: mn,
                                    start_mx: mx,
                                    ..Drag::new(Tool::Select, DragMode::Move { key }, screen, world)
                                });
                                self.touch();
                                return;
                            }
                        }
                    }
                }

                match pick(&mut self.scene, world) {
                    Some(key) => {
                        // Двойной клик по текстовому узлу — инлайн-редактирование
                        // (первый клик запоминаем, второй в окне 400 мс открывает оверлей).
                        if self
                            .scene
                            .get(key)
                            .map(|n| matches!(n.kind, NodeKind::Text { .. }))
                            .unwrap_or(false)
                        {
                            let dbl = self
                                .last_pick
                                .map(|(t, k)| k == key && t.elapsed() < Duration::from_millis(400))
                                .unwrap_or(false);
                            self.last_pick = Some((Instant::now(), key));
                            if dbl {
                                self.begin_text_edit(key);
                                return;
                            }
                        }
                        let in_selection = self.scene.selection().contains(&key);
                        if in_selection && self.scene.selection().len() > 1 {
                            // Захват для перемещения всего выделения.
                            self.record();
                            let sel = self.scene.selection().to_vec();
                            let start_positions = sel
                                .iter()
                                .map(|&k| {
                                    self.scene
                                        .get(k)
                                        .map(|n| n.local_transform.translation)
                                        .unwrap_or(Vec2::ZERO)
                                })
                                .collect();
                            self.drag = Some(Drag {
                                mode: DragMode::MoveMany { keys: sel },
                                start_positions,
                                ..Drag::new(Tool::Select, DragMode::Move { key }, screen, world)
                            });
                            self.touch();
                        } else {
                            // Захват для перемещения одного узла.
                            self.record();
                            let start_pos = self
                                .scene
                                .get(key)
                                .map(|n| n.local_transform.translation)
                                .unwrap_or(Vec2::ZERO);
                            self.scene.clear_selection();
                            self.scene.add_to_selection(key);
                            self.layer_anchor = Some(key);
                            self.drag = Some(Drag {
                                mode: DragMode::Move { key },
                                start_pos,
                                ..Drag::new(Tool::Select, DragMode::Move { key }, screen, world)
                            });
                            self.bump_revision();
                        }
                    }
                    None => {
                        // Рамочное выделение (решение принимается на pointer_up).
                        self.drag = Some(Drag::new(Tool::Select, DragMode::Marquee, screen, world));
                        self.touch();
                    }
                }
            }
            other => {
                // Начало создания фигуры.
                let world = self.camera.screen_to_world(screen);
                self.drag = Some(Drag::new(other, DragMode::Create(other), screen, world));
                self.touch();
            }
        }
    }

    pub fn pointer_move(&mut self, screen: Vec2) {
        let Some(mut d) = self.drag.take() else { return };
        d.current_screen = screen;

        match &d.mode {
            DragMode::Pan => {
                let delta = d.current_screen - d.anchor_screen;
                self.camera.pan_by(delta);
                d.anchor_screen = d.current_screen;
                self.touch();
            }
            DragMode::Move { key } => {
                let world = self.camera.screen_to_world(screen);
                d.last_world = world;
                // Снап к итоговой позиции: без дрейфа, т.к. база — start_pos.
                let target = self.snap_point(d.start_pos + (world - d.grab_world));
                // Перенос сохраняет поворот/масштаб узла (не сбрасывает matrix2).
                if let Some(n) = self.scene.get_mut(*key) {
                    n.local_transform = translate_keep_rotation(n.local_transform, target);
                }
                self.scene.mark_subtree_dirty(*key);
                // Подсветка фрейма-цели (reparent по отпусканию).
                self.hovered_frame = self.drop_frame_at(world, &[*key]);
                self.touch();
            }
            DragMode::MoveMany { keys } => {
                let world = self.camera.screen_to_world(screen);
                d.last_world = world;
                // Снап по ДЕЛЬТЕ (к опорной точке захвата), чтобы сохранить
                // взаимные смещения узлов мультивыбора.
                let delta = self.snap_point(world) - d.grab_world;
                for (i, &k) in keys.iter().enumerate() {
                    let target = d.start_positions[i] + delta;
                    if let Some(n) = self.scene.get_mut(k) {
                        n.local_transform = translate_keep_rotation(n.local_transform, target);
                    }
                    self.scene.mark_subtree_dirty(k);
                }
                // Подсветка фрейма-цели (reparent по отпусканию).
                self.hovered_frame = self.drop_frame_at(world, keys);
                self.touch();
            }
            DragMode::Resize { key, handle } => {
                let world = self.camera.screen_to_world(screen);
                let corner = self.snap_point(world);
                let (mn, mx) = gizmo::resize_rect(*handle, d.start_mn, d.start_mx, corner);
                self.apply_resize_live(*key, mn, mx);
            }
            DragMode::Create(_) => {
                // Превью создаваемой фигуры (привязка к сетке/пикселям).
                let a = self.snap_point(self.camera.screen_to_world(d.anchor_screen));
                let b = self.snap_point(self.camera.screen_to_world(screen));
                self.preview = Some(Preview {
                    a,
                    b,
                    kind: kind_for_tool(d.tool),
                });
                self.touch();
            }
            DragMode::Marquee => {
                let world = self.camera.screen_to_world(screen);
                let a = self.snap_point(d.start_world);
                let b = self.snap_point(world);
                self.marquee = Some((a, b));
                self.touch();
            }
        }
        self.drag = Some(d);
    }

    pub fn pointer_up(&mut self, screen: Vec2) {
        let Some(d) = self.drag.take() else { return };
        self.preview = None;
        self.marquee = None;

        // Reparent по отпусканию: если тащили узел(ы) и есть фрейм-цель.
        let drop_target = match &d.mode {
            DragMode::Move { key } => self.hovered_frame.map(|f| (*key, f)),
            DragMode::MoveMany { keys } => self.hovered_frame.map(|f| {
                let k = keys.first().copied().unwrap_or_default();
                (k, f)
            }),
            _ => None,
        };
        self.hovered_frame = None;

        match &d.mode {
            DragMode::Create(tool) => {
                if let Tool::Rectangle | Tool::Ellipse | Tool::Line | Tool::Frame = tool {
                    let a = self.snap_point(self.camera.screen_to_world(d.anchor_screen));
                    let b = self.snap_point(self.camera.screen_to_world(screen));
                    let screen_dist = (d.current_screen - d.anchor_screen).length();
                    if screen_dist >= 3.0 {
                        self.record();
                        let key = self.add_root_node(*tool, a, b);
                        self.scene.set_selection(vec![key]);
                    }
                } else if *tool == Tool::Text {
                    // Клик с Text-инструментом: создаём узел в точке клика и сразу
                    // открываем инлайн-редактирование (как в Figma).
                    let a = self.snap_point(self.camera.screen_to_world(d.anchor_screen));
                    self.record();
                    let key = self.add_root_node(Tool::Text, a, a);
                    self.scene.set_selection(vec![key]);
                    self.begin_text_edit(key);
                }
            }
            DragMode::Marquee => {
                let screen_dist = (d.current_screen - d.anchor_screen).length();
                if screen_dist < 3.0 {
                    self.scene.clear_selection();
                } else {
                    let a = self.snap_point(d.start_world);
                    let b = self.snap_point(self.camera.screen_to_world(screen));
                    let mn = Vec2::new(a.x.min(b.x), a.y.min(b.y));
                    let mx = Vec2::new(a.x.max(b.x), a.y.max(b.y));
                    self.scene.flush_transforms();
                    let mut keys: Vec<NodeKey> = self
                        .scene
                        .spatial_query_rect(mn, mx)
                        .into_iter()
                        .filter(|k| {
                            let visible = self.scene.get(*k).map(|n| n.is_visible).unwrap_or(false);
                            visible
                                && self
                                    .scene
                                    .world_bbox(*k)
                                    .map(|(n0, n1)| gizmo::aabb_intersect(mn, mx, n0, n1))
                                    .unwrap_or(false)
                        })
                        .collect();
                    // Сохраняем порядок отрисовки (как в прежнем обходе walk()).
                    keys.sort_by_cached_key(|k| crate::engine::spatial::paint_rank(&self.scene, *k));
                    self.scene.set_selection(keys);
                }
                self.bump_revision();
            }
            DragMode::Pan | DragMode::Resize { .. } => {}
            DragMode::Move { .. } | DragMode::MoveMany { .. } => {
                match drop_target {
                    Some((_, frame)) => {
                        // Фрейм-цель может оказаться потомком перемещаемого узла
                        // (например, тащим фрейм, а под курсором его дочерний фрейм) —
                        // reparent сам отклонит цикл.
                        self.reparent_selection(Some(frame));
                    }
                    None => {
                        // Бросок на пустое место — вытаскиваем из родительского
                        // фрейма в корни (как Figma).
                        let inside_frame = self
                            .scene
                            .selection()
                            .iter()
                            .any(|k| self.scene.get(*k).and_then(|n| n.parent).is_some());
                        if inside_frame {
                            self.reparent_selection(None);
                        }
                    }
                }
            }
        }
        self.touch();
    }

    // --- Зум ---

    pub fn zoom(&mut self, delta_y: f32, screen: Vec2) {
        let factor = if delta_y > 0.0 { 1.1 } else { 1.0 / 1.1 };
        self.camera.zoom_at(factor, screen);
        self.touch();
    }

    /// Зум от центра холста (для хоткеев +/-).
    pub fn zoom_at_center(&mut self, factor: f32, center: Vec2) {
        self.camera.zoom_at(factor, center);
        self.touch();
    }

    /// Снимает выделение.
    pub fn deselect(&mut self) {
        if !self.scene.selection().is_empty() {
            self.scene.clear_selection();
            self.bump_revision();
        }
    }

    // --- Свойства выделенного узла ---

    pub fn selected_id(&self) -> Option<NodeKey> {
        self.scene.selection().first().copied()
    }

    pub fn selected(&self) -> Option<&SceneNode> {
        self.selected_id().and_then(|key| self.scene.get(key))
    }

    /// Применяет мутацию к выделенному узлу. `record` — записать undo-снапшот
    /// ДО изменения (для коммитов); live-правки идут с `record = false`, а
    /// undo-база фиксируется заранее вызовом `begin_edit`.
    fn apply_selected(&mut self, record: bool, f: impl FnOnce(&mut SceneNode)) {
        let Some(id) = self.selected_id() else { return };
        if record {
            self.record();
        }
        if let Some(n) = self.scene.get_mut(id) {
            f(n);
        }
        // Прямое изменение может затронуть геометрию/трансформацию — инвалидируем.
        self.scene.mark_subtree_dirty(id);
        self.touch();
    }

    /// Применяет замыкание ко ВСЕМ узлам выделения (live, без undo-снапшота).
    fn apply_all_selected(&mut self, mut f: impl FnMut(&mut SceneNode)) {
        let keys = self.scene.selection().to_vec();
        if keys.is_empty() {
            return;
        }
        for key in keys {
            if let Some(n) = self.scene.get_mut(key) {
                f(n);
            }
            self.scene.mark_subtree_dirty(key);
        }
        self.touch();
    }

    /// Фиксирует undo-снапшот текущего состояния (до первой live-правки сессии).
    pub fn begin_edit(&mut self) {
        self.record();
    }

    pub fn apply_position_live(&mut self, x: f32, y: f32) {
        self.apply_position(x, y);
    }

    fn apply_position(&mut self, x: f32, y: f32) {
        let snapped = self.snap_point(Vec2::new(x, y));
        self.apply_selected(false, |n| {
            n.local_transform = translate_keep_rotation(n.local_transform, snapped);
        });
    }

    pub fn apply_size_live(&mut self, w: f32, h: f32) {
        self.apply_size(w, h);
    }

    fn apply_size(&mut self, w: f32, h: f32) {
        let snapped = self.snap_size(Vec2::new(w.max(1.0), h.max(1.0)));
        let id = self.selected_id();
        self.apply_selected(false, |n| match &mut n.kind {
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
        if let Some(id) = id {
            if self.scene.affects_auto_layout(id) {
                self.scene.mark_layout_dirty();
            }
        }
    }

    pub fn apply_opacity_live(&mut self, v: f32) {
        let v = v.clamp(0.0, 1.0);
        self.apply_all_selected(move |n| n.opacity = v);
    }

    // --- Текст: размер и выравнивание ---

    /// Live-правка размера шрифта выбранных текстовых узлов (AutoWidth
    /// пересчитает размеры узла при следующем рендере через `text::measure`).
    pub fn apply_font_size_live(&mut self, fs: f32) {
        let fs = fs.max(1.0);
        self.apply_all_selected(move |n| {
            if let NodeKind::Text { font_size, .. } = &mut n.kind {
                *font_size = fs;
            }
        });
    }

    /// Устанавливает выравнивание выбранных текстовых узлов.
    pub fn set_text_align(&mut self, align: TextAlign) {
        self.apply_selected(true, move |n| {
            if let NodeKind::Text { align: a, .. } = &mut n.kind {
                *a = align;
            }
        });
    }

    pub fn apply_name_live(&mut self, name: &str) {
        let name = name.to_string();
        self.apply_all_selected(move |n| n.name = name.clone());
        self.bump_revision();
    }

    pub fn apply_fill_hex_live(&mut self, hex: &str) {
        if let Some(color) = crate::engine::color::parse_hex(hex) {
            self.apply_fill_color_live(color);
        }
    }

    /// Применяет сплошную заливку к выделению (live, без undo-снапшота).
    pub fn apply_fill_color_live(&mut self, color: Color) {
        self.apply_all_selected(move |n| n.fills = vec![Paint::Solid(color)]);
    }

    /// Текущий поворот выделенного узла в градусах (0, если нет выделения).
    pub fn rotation_degrees(&self) -> f32 {
        self.selected()
            .map(|n| {
                let m = n.local_transform.matrix2;
                m.x_axis.y.atan2(m.x_axis.x).to_degrees()
            })
            .unwrap_or(0.0)
    }

    /// Ширина обводки выделенного узла (0, если нет обводки).
    pub fn stroke_width(&self) -> f32 {
        self.selected()
            .and_then(|n| n.strokes.first())
            .map(|st| st.width)
            .unwrap_or(0.0)
    }

    /// Цвет обводки выделенного узла (сплошная первая обводка).
    pub fn stroke_color(&self) -> Option<Color> {
        self.selected()
            .and_then(|n| n.strokes.first())
            .and_then(|st| match &st.paint {
                Paint::Solid(c) => Some(*c),
                _ => None,
            })
    }

    /// Есть ли пунктир у обводки выделенного узла.
    pub fn stroke_dashed(&self) -> bool {
        self.selected()
            .and_then(|n| n.strokes.first())
            .map(|st| !st.dash_pattern.is_empty())
            .unwrap_or(false)
    }

    /// Живое изменение ширины обводки выделения (создаёт обводку, если её нет).
    pub fn apply_stroke_width_live(&mut self, width: f32) {
        let w = width.max(0.0);
        self.apply_all_selected(move |n| {
            if let Some(st) = n.strokes.first_mut() {
                st.width = w;
            } else {
                n.strokes.push(Stroke::solid(Color::BLACK, w));
            }
        });
    }

    /// Живое изменение цвета обводки выделения (создаёт обводку, если её нет).
    pub fn apply_stroke_color_live(&mut self, color: Color) {
        self.apply_all_selected(move |n| {
            if let Some(st) = n.strokes.first_mut() {
                st.paint = Paint::Solid(color);
            } else {
                n.strokes.push(Stroke::solid(color, 1.0));
            }
        });
    }

    /// Живое переключение пунктира обводки выделения.
    pub fn apply_stroke_dash_live(&mut self, dash: bool) {
        self.apply_all_selected(move |n| {
            if let Some(st) = n.strokes.first_mut() {
                st.dash_pattern = if dash { vec![4.0, 4.0] } else { Vec::new() };
            }
        });
    }

    /// Радиусы скругления выделенного узла ([tl,tr,br,bl]); None для видов без
    /// скругления.
    pub fn corners(&self) -> Option<[f32; 4]> {
        self.selected().and_then(|n| match &n.kind {
            NodeKind::Frame { corner_radii, .. }
            | NodeKind::Shape(ShapeKind::Rectangle { corner_radii, .. }) => Some(*corner_radii),
            _ => None,
        })
    }

    /// Живое изменение радиуса угла `i` (0..4, порядок tl,tr,br,bl) для всего
    /// выделения.
    pub fn apply_corner_radius_live(&mut self, i: usize, v: f32) {
        let r = v.max(0.0);
        self.apply_all_selected(move |n| match &mut n.kind {
            NodeKind::Frame { corner_radii, .. }
            | NodeKind::Shape(ShapeKind::Rectangle { corner_radii, .. }) => corner_radii[i.min(3)] = r,
            _ => {}
        });
    }

    /// Равномерный радиус скругления всех углов для всего выделения.
    pub fn apply_corner_radius_uniform(&mut self, v: f32) {
        let r = v.max(0.0);
        self.apply_all_selected(move |n| match &mut n.kind {
            NodeKind::Frame { corner_radii, .. }
            | NodeKind::Shape(ShapeKind::Rectangle { corner_radii, .. }) => {
                *corner_radii = [r, r, r, r];
            }
            _ => {}
        });
    }

    /// База процента для радиуса: меньшая сторона фигуры (как в Figma).
    pub fn corner_percent_base(&self) -> Option<f32> {
        self.selected().and_then(|n| match &n.kind {
            NodeKind::Frame { size, .. }
            | NodeKind::Shape(ShapeKind::Rectangle { size, .. }) => Some(size.x.min(size.y)),
            _ => None,
        })
    }

    /// Размер родительского фрейма выбранного узла (база % для W/H).
    pub fn parent_frame_size(&self) -> Option<Vec2> {
        let key = self.selected_id()?;
        let parent = self.scene.get(key)?.parent?;
        self.scene.get(parent).and_then(|n| match &n.kind {
            NodeKind::Frame { size, .. } => Some(*size),
            _ => None,
        })
    }

    /// Auto-layout выбранного узла (только для Frame).
    pub fn frame_auto_layout(&self) -> Option<AutoLayoutConfig> {
        self.selected().and_then(|n| match &n.kind {
            NodeKind::Frame { auto_layout, .. } => *auto_layout,
            _ => None,
        })
    }

    /// Режим auto-layout выбранного фрейма: 0 = off, 1 = Flex.
    /// Включая Flex, создаём конфиг с текущими отступами и направлением;
    /// выключая — `None`.
    pub fn set_auto_layout_mode(&mut self, mode: u8) {
        self.record();
        let had_config = self.frame_auto_layout().is_some() || self.last_layout.is_some();
        let mut cur = self.frame_auto_layout().or(self.last_layout).unwrap_or_default();
        // Включаем flex на «свежем» фрейме — дети НЕ растягиваются (как в Figma).
        if mode != 0 && !had_config {
            cur.align_items = LayoutAlign::Min;
        }
        self.apply_all_selected(move |n| {
            if let NodeKind::Frame { auto_layout, .. } = &mut n.kind {
                *auto_layout = match mode {
                    0 => None,
                    _ => Some(cur),
                };
            }
        });
        self.scene.mark_layout_dirty();
        if mode == 1 {
            self.last_layout = self.frame_auto_layout();
        }
        self.bump_revision();
    }

    /// Направление flex-раскладки выбранного фрейма: 0 = Row, 1 = Column.
    pub fn set_auto_layout_direction(&mut self, direction: u8) {
        self.record();
        let dir = if direction == 0 { LayoutDirection::Horizontal } else { LayoutDirection::Vertical };
        self.apply_all_selected(move |n| {
            if let NodeKind::Frame { auto_layout: Some(cfg), .. } = &mut n.kind {
                cfg.direction = dir;
            }
        });
        self.scene.mark_layout_dirty();
        self.last_layout = self.frame_auto_layout();
        self.bump_revision();
    }

    /// Выравнивание поперёк оси (0=Stretch, 1=Min, 2=Center, 3=Max).
    pub fn apply_auto_layout_align(&mut self, align: u8) {
        self.record();
        let a = match align {
            1 => LayoutAlign::Min,
            2 => LayoutAlign::Center,
            3 => LayoutAlign::Max,
            _ => LayoutAlign::Stretch,
        };
        self.apply_all_selected(move |n| {
            if let NodeKind::Frame { auto_layout: Some(cfg), .. } = &mut n.kind {
                cfg.align_items = a;
            }
        });
        self.scene.mark_layout_dirty();
        self.last_layout = self.frame_auto_layout();
        self.bump_revision();
    }

    /// Распределение вдоль оси (0=Min, 1=Center, 2=Max, 3=SpaceBetween).
    pub fn apply_auto_layout_justify(&mut self, justify: u8) {
        self.record();
        let j = match justify {
            1 => LayoutJustify::Center,
            2 => LayoutJustify::Max,
            3 => LayoutJustify::SpaceBetween,
            _ => LayoutJustify::Min,
        };
        self.apply_all_selected(move |n| {
            if let NodeKind::Frame { auto_layout: Some(cfg), .. } = &mut n.kind {
                cfg.justify_content = j;
            }
        });
        self.scene.mark_layout_dirty();
        self.last_layout = self.frame_auto_layout();
        self.bump_revision();
    }

    /// Отступ между детьми auto-layout фрейма (live).
    pub fn apply_auto_layout_spacing_live(&mut self, v: f32) {
        let v = v.max(0.0);
        self.apply_all_selected(move |n| {
            if let NodeKind::Frame { auto_layout: Some(cfg), .. } = &mut n.kind {
                cfg.spacing = v;
            }
        });
        self.scene.mark_layout_dirty();
        self.last_layout = self.frame_auto_layout();
        self.bump_revision();
    }

    /// Внутренний отступ auto-layout фрейма со всех сторон (live).
    pub fn apply_auto_layout_padding_live(&mut self, v: f32) {
        let v = v.max(0.0);
        self.apply_all_selected(move |n| {
            if let NodeKind::Frame { auto_layout: Some(cfg), .. } = &mut n.kind {
                cfg.padding = [v, v, v, v];
            }
        });
        self.scene.mark_layout_dirty();
        self.last_layout = self.frame_auto_layout();
        self.bump_revision();
    }

    /// Общая мировая рамка выделения (для мультивыбора — группа целиком).
    pub fn selection_bbox(&self) -> Option<(Vec2, Vec2)> {
        let mut it = self
            .scene
            .selection()
            .iter()
            .filter_map(|&k| self.scene.world_bbox(k));
        let (mut mn, mut mx) = it.next()?;
        for (a, b) in it {
            mn = mn.min(a);
            mx = mx.max(b);
        }
        Some((mn, mx))
    }

    /// Сдвиг всего выделения на дельту в мировых координатах (мультивыбор:
    /// поле X/Y интерпретируется как дельта от общей рамки).
    pub fn apply_position_delta_live(&mut self, dx: f32, dy: f32) {
        if dx == 0.0 && dy == 0.0 {
            return;
        }
        self.scene.flush_transforms();
        let keys = self.scene.selection().to_vec();
        for key in keys {
            let delta_local = self
                .scene
                .get(key)
                .and_then(|n| n.parent)
                .and_then(|p| self.scene.world_transform(p))
                .unwrap_or(Affine2::IDENTITY)
                .inverse()
                .transform_vector2(Vec2::new(dx, dy));
            if let Some(n) = self.scene.get_mut(key) {
                n.local_transform = n.local_transform * Affine2::from_translation(delta_local);
            }
            self.scene.mark_subtree_dirty(key);
        }
        self.touch();
    }

    /// Масштаб выделения как группы: новые W/H общей рамки; каждая нода
    /// масштабируется относительно общего центра рамки.
    pub fn apply_size_scale_live(&mut self, w: f32, h: f32) {
        self.scene.flush_transforms();
        let Some((mn, mx)) = self.selection_bbox() else { return };
        let size = mx - mn;
        if size.x <= 1e-6 || size.y <= 1e-6 {
            return;
        }
        let scale = Vec2::new(w.max(1.0) / size.x, h.max(1.0) / size.y);
        let center = (mn + mx) * 0.5;
        let keys = self.scene.selection().to_vec();
        for key in keys {
            let world = self.scene.world_transform(key).unwrap_or(Affine2::IDENTITY);
            let new_world = Affine2::from_translation(center)
                * Affine2::from_scale(scale)
                * Affine2::from_translation(-center)
                * world;
            let parent_world = self
                .scene
                .get(key)
                .and_then(|n| n.parent)
                .and_then(|p| self.scene.world_transform(p))
                .unwrap_or(Affine2::IDENTITY);
            if let Some(n) = self.scene.get_mut(key) {
                n.local_transform = parent_world.inverse() * new_world;
            }
            self.scene.mark_subtree_dirty(key);
        }
        self.touch();
    }

    /// Переименование конкретного узла (из панели слоёв).
    pub fn rename(&mut self, key: NodeKey, name: &str) {
        if !self.scene.contains(key) {
            return;
        }
        self.record();
        if let Some(n) = self.scene.get_mut(key) {
            n.name = name.to_string();
        }
        self.bump_revision();
    }

    /// Drag-reorder в панели слоёв: сдвиг узла на `offset` позиций среди его
    /// сиблингов (вверх — отрицательный, вниз — положительный).
    pub fn move_layer(&mut self, key: NodeKey, offset: i32) {
        let siblings = self.scene.siblings_of(key);
        let Some(i) = siblings.iter().position(|&k| k == key) else { return };
        let ni = (i as i32 + offset).clamp(0, siblings.len() as i32 - 1) as usize;
        if ni == i {
            return;
        }
        self.record();
        if self.scene.move_to_index(key, ni) {
            self.scene.mark_subtree_dirty(key);
            self.touch();
        }
    }

    /// Figma-стиль drop в панели слоёв. `zone`: 0 = вставить ПЕРЕД `target`
    /// (тот же уровень), 1 = вложить ВНУТРЬ `target`, 2 = вставить ПОСЛЕ.
    /// При переносе между уровнями мировая позиция сохраняется.
    pub fn drop_layer_at(&mut self, dragged: NodeKey, target: NodeKey, zone: u8) {
        if dragged == target || !self.scene.contains(dragged) || !self.scene.contains(target) {
            return;
        }
        // Цель (или её родитель) не может быть потомком переносимого узла —
        // иначе получится цикл.
        if self.scene.is_descendant(target, dragged) {
            return;
        }
        self.record();
        if zone == 1 {
            // Вложить внутрь target.
            self.scene.reparent_preserve_world(dragged, Some(target));
            self.scene.flush_transforms();
            self.bump_revision();
            return;
        }
        // Переместить в список сиблингов target и вставить до/после него.
        let parent = self.scene.get(target).and_then(|n| n.parent);
        if let Some(p) = parent {
            if self.scene.is_descendant(p, dragged) {
                return;
            }
        }
        if self.scene.get(dragged).and_then(|n| n.parent) != parent {
            self.scene.reparent_preserve_world(dragged, parent);
            self.scene.flush_transforms();
        }
        // Оба теперь в одном списке: позиция target после удаления dragged.
        let siblings = self.scene.siblings_of(target);
        let mut ti = siblings.iter().position(|&k| k == target).unwrap_or(0);
        if let Some(di) = siblings.iter().position(|&k| k == dragged) {
            if di < ti {
                ti -= 1;
            }
        }
        let ni = if zone == 0 { ti } else { ti + 1 };
        if self.scene.move_to_index(dragged, ni) {
            self.scene.mark_subtree_dirty(dragged);
        }
        self.bump_revision();
    }

    /// Сдвигает всё выделение на одну позицию вверх по z-order (поверх).
    /// Идём сверху вниз, чтобы более ранние сдвиги не сбивали позиции.
    pub fn bring_forward_selection(&mut self) {
        let sel = self.scene.selection().to_vec();
        if sel.is_empty() {
            return;
        }
        self.record();
        for key in sel.iter().rev() {
            let siblings = self.scene.siblings_of(*key);
            if let Some(i) = siblings.iter().position(|&k| k == *key) {
                if i + 1 < siblings.len() {
                    self.scene.move_to_index(*key, i + 1);
                    self.scene.mark_subtree_dirty(*key);
                }
            }
        }
        self.bump_revision();
    }

    /// Сдвигает всё выделение на одну позицию вниз по z-order (назад).
    /// Идём снизу вверх, чтобы более ранние сдвиги не сбивали позиции.
    pub fn send_backward_selection(&mut self) {
        let sel = self.scene.selection().to_vec();
        if sel.is_empty() {
            return;
        }
        self.record();
        for key in &sel {
            let siblings = self.scene.siblings_of(*key);
            if let Some(i) = siblings.iter().position(|&k| k == *key) {
                if i > 0 {
                    self.scene.move_to_index(*key, i - 1);
                    self.scene.mark_subtree_dirty(*key);
                }
            }
        }
        self.bump_revision();
    }

    /// Применяет поворот к выделению (live) вокруг центра каждого узла,
    /// сохраняя масштаб и позицию. Новая мировая трансформация
    /// `T(center)·R(angle)·T(−center)·W` затем пересчитывается в локальные
    /// координаты через мировую трансформацию родителя (работает и для
    /// вложенных узлов).
    pub fn apply_rotation_live(&mut self, deg: f32) {
        self.scene.flush_transforms();
        let keys = self.scene.selection().to_vec();
        if keys.is_empty() {
            return;
        }
        let rot = Affine2::from_angle(deg.to_radians());
        for key in keys {
            let Some((mn, mx)) = self.scene.world_bbox(key) else { continue };
            let center = (mn + mx) * 0.5;
            let world = self.scene.world_transform(key).unwrap_or(Affine2::IDENTITY);
            let new_world =
                Affine2::from_translation(center) * rot * Affine2::from_translation(-center) * world;
            let parent_world = self
                .scene
                .get(key)
                .and_then(|n| n.parent)
                .and_then(|p| self.scene.world_transform(p))
                .unwrap_or(Affine2::IDENTITY);
            if let Some(n) = self.scene.get_mut(key) {
                n.local_transform = parent_world.inverse() * new_world;
            }
            self.scene.mark_subtree_dirty(key);
        }
        self.touch();
    }

    /// Ресайз узла хэндлом гизмо: новое мировое AABB `mn..mx` применяется либо
    /// мутацией size/radii (Frame/Rectangle/Ellipse — обводка не масштабируется),
    /// либо масштабной трансформацией (остальные виды).
    fn apply_resize_live(&mut self, key: NodeKey, mn: Vec2, mx: Vec2) {
        let size = self.snap_size((mx - mn).max(Vec2::splat(1.0)));
        let Some(n) = self.scene.get_mut(key) else { return };
        let (lmin, lmax) = n.kind.local_bbox();
        let is_size_kind = match &mut n.kind {
            NodeKind::Frame { size: s, .. }
            | NodeKind::Shape(ShapeKind::Rectangle { size: s, .. })
            | NodeKind::Shape(ShapeKind::Ellipse { radii: s, .. }) => {
                *s = size;
                true
            }
            _ => false,
        };
        if is_size_kind {
            n.local_transform = translate_keep_rotation(n.local_transform, mn);
        } else {
            n.local_transform = gizmo::scale_transform(mn, mn + size, lmin, lmax);
        }
        self.scene.mark_subtree_dirty(key);
        if self.scene.affects_auto_layout(key) {
            self.scene.mark_layout_dirty();
        }
        self.touch();
    }

    /// Элемент гизмо под экранной точкой (для смены курсора): имя ресайз-хэндла.
    /// Только Select tool, без активного драга и при ровно одном выделенном узле.
    pub fn hover_gizmo(&mut self, screen: Vec2) -> String {
        self.scene.flush_transforms();
        if self.drag.is_some() || self.tool != Tool::Select || self.scene.selection().len() != 1 {
            return String::new();
        }
        let key = self.scene.selection()[0];
        let resizable = self
            .scene
            .get(key)
            .map(|n| gizmo::resizable(&n.kind))
            .unwrap_or(false);
        if !resizable {
            return String::new();
        }
        let Some((mn, mx)) = self.scene.world_bbox(key) else { return String::new() };
        let world = self.camera.screen_to_world(screen);
        gizmo::handle_at(world, mn, mx, self.camera.zoom)
            .map(|h| h.name().to_string())
            .unwrap_or_default()
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
        // Мир и кэши могут быть в любом состоянии — помечаем всё грязным,
        // чтобы первый же flush пересчитал трансформации и spatial-индекс.
        self.scene.invalidate_all();
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
        Tool::Text => NodeKind::Text {
            content: "Text".into(),
            font_family: "Inter".into(),
            font_size: 16.0,
            font_weight: 400,
            line_height: 1.2,
            letter_spacing: 0.0,
            align: TextAlign::Left,
            size_mode: TextSizeMode::AutoWidth,
        },
        _ => NodeKind::Shape(ShapeKind::Rectangle { size: Vec2::ONE, corner_radii: [0.0; 4] }),
    }
}

/// Верхнеуровневые узлы выделения: ключи, чей родитель не входит в выделение
/// (для copy/paste не дублируем узел вместе с его выбранным родителем).
fn top_level_selected(scene: &SceneGraph) -> Vec<NodeKey> {
    let sel = scene.selection();
    sel.iter()
        .copied()
        .filter(|k| {
            scene
                .get(*k)
                .and_then(|n| n.parent)
                .map(|p| !sel.contains(&p))
                .unwrap_or(true)
        })
        .collect()
}

impl Default for CanvasController {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Регресс-тест: хэндл гизмо должен находиться в МИРОВЫХ координатах.
    /// При сдвинутой камере (pan/zoom) хит-тест по экранной точке ломался
    /// (screen != world), и ресайз переставал работать после пана/зума.
    #[test]
    fn resize_handle_hit_works_after_pan_zoom() {
        let mut c = CanvasController::new();
        // Демо-контент: первый корневой узел — Frame (80,80)..(480,400).
        let key = c.scene.roots()[0];
        c.select(key);

        // Сдвигаем и масштабируем камеру (типичный сценарий пана/зума).
        c.camera.pan_by(Vec2::new(50.0, -30.0));
        c.camera.zoom_at(2.0, Vec2::new(300.0, 200.0));
        // Мировые трансформации должны быть пересчитаны до чтения bbox.
        c.scene.flush_transforms();

        let (mn, mx) = c.scene.world_bbox(key).unwrap();
        // SE-хэндл сидит в правом нижнем углу рамки. Позиция на экране:
        let se_screen = c.camera.world_to_screen(mx);
        c.pointer_down(se_screen, 1);

        let d = c.drag.as_ref().expect("drag должен начаться");
        match &d.mode {
            DragMode::Resize { key: k, handle } => {
                assert_eq!(*k, key);
                assert_eq!(*handle, Handle::Se);
            }
            _ => panic!("клик по экранной позиции SE-хэндла должен начать ресайз, а не move/pick"),
        }
        // Позиция записи рамки — исходная мировая AABB.
        assert_eq!(d.start_mn, mn);
        assert_eq!(d.start_mx, mx);
    }

    #[test]
    fn hover_handle_after_pan_zoom() {
        let mut c = CanvasController::new();
        let key = c.scene.roots()[0];
        c.select(key);
        c.camera.pan_by(Vec2::new(-120.0, 60.0));
        c.camera.zoom_at(0.5, Vec2::new(400.0, 300.0));
        c.scene.flush_transforms();

        let (_, mx) = c.scene.world_bbox(key).unwrap();
        let se_screen = c.camera.world_to_screen(mx);
        let handle = c.hover_gizmo(se_screen);
        assert_eq!(handle, "se");
    }

    #[test]
    fn rotation_keeps_center_and_scale() {
        let mut c = CanvasController::new();
        c.grid.snap = false;
        c.scene.flush_transforms();
        // Первый корень — Frame: два угла (80,80) и (400,320), т.е. размер 320×240.
        let key = c.scene.roots()[0];
        c.select(key);
        let (mn0, mx0) = c.scene.world_bbox(key).unwrap();
        let center_before = (mn0 + mx0) * 0.5;
        let (w0, h0) = ((mx0.x - mn0.x), (mx0.y - mn0.y));
        c.apply_rotation_live(90.0);
        c.scene.flush_transforms();
        let (mn, mx) = c.scene.world_bbox(key).unwrap();
        let center_after = (mn + mx) * 0.5;
        assert!(
            (center_after - center_before).length() < 1e-3,
            "центр не должен смещаться: {center_before:?} -> {center_after:?}"
        );
        // Поворот на 90° меняет местами ширину/высоту, размер сохраняется.
        let s = mx - mn;
        assert!((s.x - h0).abs() < 1e-3 && (s.y - w0).abs() < 1e-3, "размер после 90°: {s:?}");
    }

    #[test]
    fn corner_radius_applies_to_all_selected() {
        let mut c = CanvasController::new();
        c.select(c.scene.roots()[0]);
        c.select_layer(c.scene.roots()[1], true, false); // +Rectangle (мультивыбор)
        assert_eq!(c.scene.selection().len(), 2);
        c.apply_corner_radius_live(0, 12.0);
        for key in c.scene.selection().to_vec() {
            let r = match &c.scene.get(key).unwrap().kind {
                NodeKind::Frame { corner_radii, .. }
                | NodeKind::Shape(ShapeKind::Rectangle { corner_radii, .. }) => *corner_radii,
                _ => panic!("неизвестный вид"),
            };
            assert_eq!(r[0], 12.0);
        }
    }

    #[test]
    fn uniform_corner_sets_all_radii() {
        let mut c = CanvasController::new();
        c.select(c.scene.roots()[1]); // Rectangle
        c.apply_corner_radius_uniform(8.5);
        let r = c.corners().unwrap();
        assert_eq!(r, [8.5, 8.5, 8.5, 8.5]);
        c.apply_corner_radius_uniform(-3.0);
        assert_eq!(c.corners().unwrap(), [0.0; 4]);
    }

    #[test]
    fn corner_percent_base_is_min_side() {
        let mut c = CanvasController::new();
        // Первый корень — Frame 320×240: база = 240.
        c.select(c.scene.roots()[0]);
        assert_eq!(c.corner_percent_base(), Some(240.0));
        // Ellipse без скругления — базы нет.
        c.select(c.scene.roots()[2]);
        assert_eq!(c.corner_percent_base(), None);
    }

    #[test]
    fn parent_frame_size_axis() {
        let mut c = CanvasController::new();
        // Rectangle/Frame/… на корневом уровне — родителя-фрейма нет.
        assert_eq!(c.parent_frame_size(), None);
        // Вложим Rectangle в корневой Frame: база = размер фрейма (320×240).
        let rect = c.scene.roots()[1];
        c.scene.reparent_preserve_world(rect, Some(c.scene.roots()[0]));
        c.select(rect);
        assert_eq!(c.parent_frame_size(), Some(Vec2::new(320.0, 240.0)));
    }

    #[test]
    fn multi_select_delta_position() {
        let mut c = CanvasController::new();
        c.grid.snap = false;
        let a = c.scene.roots()[0];
        let b = c.scene.roots()[1];
        c.select(a);
        c.select_layer(b, true, false);
        c.scene.flush_transforms();
        let (mna, mxa) = c.scene.world_bbox(a).unwrap();
        let (mnb, mxb) = c.scene.world_bbox(b).unwrap();
        c.apply_position_delta_live(10.0, -5.0);
        c.scene.flush_transforms();
        let (mna2, _) = c.scene.world_bbox(a).unwrap();
        let (mnb2, _) = c.scene.world_bbox(b).unwrap();
        assert!((mna2 - mna - Vec2::new(10.0, -5.0)).length() < 1e-3, "a: {mna:?} -> {mna2:?}");
        assert!((mnb2 - mnb - Vec2::new(10.0, -5.0)).length() < 1e-3, "b: {mnb:?} -> {mnb2:?}");
        // Ширина и центр общей рамки группы сохраняются при сдвиге.
        let (gmn, gmx) = c.selection_bbox().unwrap();
        let mn = mna.min(mnb);
        let mx = mxa.max(mxb);
        assert!((gmx.x - gmn.x - (mx.x - mn.x)).abs() < 1e-3, "ширина группы изменилась");
        let gcenter = (gmn + gmx) * 0.5;
        let expected_center = (mn + mx) * 0.5 + Vec2::new(10.0, -5.0);
        assert!((gcenter - expected_center).length() < 1e-3, "центр группы: {gcenter:?} != {expected_center:?}");
    }

    #[test]
    fn multi_select_size_scales_group() {
        let mut c = CanvasController::new();
        c.grid.snap = false;
        let a = c.scene.roots()[0];
        let b = c.scene.roots()[1];
        c.select(a);
        c.select_layer(b, true, false);
        c.scene.flush_transforms();
        let (mn, mx) = c.selection_bbox().unwrap();
        let center = (mn + mx) * 0.5;
        let width = mx.x - mn.x;
        c.apply_size_scale_live(width * 2.0, mx.y - mn.y);
        c.scene.flush_transforms();
        let (mn2, mx2) = c.selection_bbox().unwrap();
        assert!((mx2.x - mn2.x - width * 2.0).abs() < 1e-2, "ширина группы: {}", mx2.x - mn2.x);
        let center2 = (mn2 + mx2) * 0.5;
        assert!((center2 - center).length() < 1e-2, "центр не смещается: {center:?} -> {center2:?}");
    }

    #[test]
    fn rename_layer_updates_name() {
        let mut c = CanvasController::new();
        let key = c.scene.roots()[0];
        c.rename(key, "My Frame");
        assert_eq!(c.scene.get(key).unwrap().name, "My Frame");
    }

    #[test]
    fn move_layer_reorders_siblings() {
        let mut c = CanvasController::new();
        let roots = c.scene.roots();
        assert_eq!(roots.len(), 3);
        let key = roots[1];
        c.select(key);

        c.move_layer(key, 1);
        let roots = c.scene.roots();
        assert_eq!(roots[2], key, "узел должен уйти вниз на одну позицию");
        assert!(c.scene.selection().contains(&key), "выделение сохраняется");

        c.move_layer(key, -2);
        let roots = c.scene.roots();
        assert_eq!(roots[0], key, "узел должен подняться наверх");

        // Сдвиг на 0 ничего не меняет.
        let before: Vec<NodeKey> = c.scene.roots().to_vec();
        c.move_layer(key, 0);
        assert_eq!(c.scene.roots(), before);
    }

    #[test]
    fn stroke_width_color_dash() {
        let mut c = CanvasController::new();
        let key = c.scene.roots()[0];
        c.select(key);
        assert_eq!(c.stroke_width(), 1.0);
        assert!(!c.stroke_dashed());

        c.apply_stroke_width_live(3.0);
        c.apply_stroke_color_live(Color::rgb(1.0, 0.0, 0.0));
        assert_eq!(c.stroke_width(), 3.0);
        assert_eq!(c.stroke_color(), Some(Color::rgb(1.0, 0.0, 0.0)));

        c.apply_stroke_dash_live(true);
        assert!(c.stroke_dashed());
        c.apply_stroke_dash_live(false);
        assert!(!c.stroke_dashed());
    }

    #[test]
    fn apply_size_clamps_min() {
        let mut c = CanvasController::new();
        let key = c.scene.roots()[1];
        c.select(key);
        c.apply_size_live(0.1, -5.0);
        let n = c.scene.get(key).unwrap();
        let (w, h) = match &n.kind {
            NodeKind::Shape(ShapeKind::Rectangle { size, .. }) => (size.x, size.y),
            _ => panic!("ожидался прямоугольник"),
        };
        assert_eq!(w, 1.0);
        assert_eq!(h, 1.0);
    }

    #[test]
    fn move_many_moves_whole_selection() {
        let mut c = CanvasController::new();
        c.grid.snap = false; // отключаем сетку, чтобы дельты были точными.
        c.scene.flush_transforms();

        // Frame(80,80) и Rect(120,130) — мультивыбор; точка (85,85) лежит
        // только внутри Frame, так что pick вернёт узел из выделения.
        let a = c.scene.roots()[0];
        let b = c.scene.roots()[1];
        c.scene.set_selection(vec![a, b]);

        let start = c.camera.world_to_screen(Vec2::new(85.0, 85.0));
        let end = start + Vec2::new(40.0, 30.0);
        c.pointer_down(start, 1);
        assert!(
            matches!(&c.drag.as_ref().unwrap().mode, DragMode::MoveMany { .. }),
            "драг по выделенному узлу при мультивыборе должен быть MoveMany"
        );

        c.pointer_move(end);
        let ta = c.scene.get(a).unwrap().local_transform.translation;
        let tb = c.scene.get(b).unwrap().local_transform.translation;
        assert!((ta.x - 120.0).abs() < 0.001 && (ta.y - 110.0).abs() < 0.001, "Frame: {ta:?}");
        assert!((tb.x - 160.0).abs() < 0.001 && (tb.y - 160.0).abs() < 0.001, "Rect: {tb:?}");

        c.pointer_up(end);
    }

    #[test]
    fn select_layer_ctrl_toggle_and_shift_range() {
        let mut c = CanvasController::new();
        let a = c.scene.roots()[0];
        let b = c.scene.roots()[1];
        let e = c.scene.roots()[2];

        c.select_layer(a, true, false);
        c.select_layer(b, true, false);
        assert_eq!(c.scene.selection().len(), 2);
        c.select_layer(a, true, false);
        assert_eq!(c.scene.selection().to_vec(), vec![b]);

        // Shift: диапазон от якоря (a, последний клик) до e в порядке walk() → [a, b, e].
        c.select_layer(e, false, true);
        let sel = c.scene.selection().to_vec();
        assert_eq!(sel, vec![a, b, e]);
    }

    #[test]
    fn duplicate_selection_inserts_siblings() {
        let mut c = CanvasController::new();
        let a = c.scene.roots()[0];
        let b = c.scene.roots()[1];
        c.select(a);
        c.select_layer(b, true, false);
        let before = c.scene.roots().len();
        c.duplicate_selection();
        let roots = c.scene.roots();
        assert_eq!(roots.len(), before + 2, "копия каждого выделенного — сиблинг");
        assert_eq!(c.scene.selection().len(), 2, "копии становятся выделением");
        // Копии смещены на (16, 16) относительно оригинала (все в корне).
        for (i, orig) in [a, b].iter().enumerate() {
            let orig_t = c.scene.get(*orig).unwrap().local_transform.translation;
            // Ищем копию: сиблинг, идущий сразу после оригинала.
            let idx = roots.iter().position(|k| k == orig).unwrap();
            let copy = roots[idx + 1];
            let copy_t = c.scene.get(copy).unwrap().local_transform.translation;
            assert!(
                (copy_t - orig_t - Vec2::new(16.0, 16.0)).length() < 1e-3,
                "копия #{i}: {orig_t:?} -> {copy_t:?}"
            );
            assert_eq!(
                c.scene.get(copy).unwrap().name,
                c.scene.get(*orig).unwrap().name,
                "имя копируется"
            );
        }
    }

    #[test]
    fn duplicate_preserves_children() {
        let mut c = CanvasController::new();
        let parent = c.scene.roots()[0];
        let child = c.scene.insert_child(parent, "Child", kind_for_tool(Tool::Rectangle)).unwrap();
        c.select(parent);
        c.duplicate_selection();
        let roots = c.scene.roots();
        let copy = roots[roots.iter().position(|k| k == &parent).unwrap() + 1];
        let copy_children = c.scene.get(copy).unwrap().children.clone();
        assert_eq!(copy_children.len(), 1, "копия фрейма несёт копию ребёнка");
        assert!(copy_children.contains(&child) == false, "ребёнок перемаплен на новый ключ");
        let copy_child = copy_children[0];
        assert_eq!(
            c.scene.get(copy_child).unwrap().name,
            "Child",
            "внутри копии — копия ребёнка с тем же именем"
        );
        assert_eq!(c.scene.get(copy_child).unwrap().parent, Some(copy));
    }

    #[test]
    fn toggle_lock_hide_selection() {
        let mut c = CanvasController::new();
        let a = c.scene.roots()[0];
        let b = c.scene.roots()[1];
        c.select(a);
        c.select_layer(b, true, false);
        c.toggle_lock_selection();
        for k in c.scene.selection().to_vec() {
            assert!(c.scene.get(k).unwrap().is_locked);
        }
        c.toggle_lock_selection();
        for k in c.scene.selection().to_vec() {
            assert!(!c.scene.get(k).unwrap().is_locked);
        }
        c.toggle_hide_selection();
        for k in c.scene.selection().to_vec() {
            assert!(!c.scene.get(k).unwrap().is_visible);
        }
        c.toggle_hide_selection();
        for k in c.scene.selection().to_vec() {
            assert!(c.scene.get(k).unwrap().is_visible);
        }
    }

    #[test]
    fn select_all_and_zoom_to_selection() {
        let mut c = CanvasController::new();
        c.select_all();
        assert_eq!(c.scene.selection().len(), c.scene.walk().len());
        let zoom_before = c.camera.zoom;
        c.zoom_to_selection();
        c.scene.flush_transforms();
        let (mn, mx) = c.selection_bbox().unwrap();
        // Центр рамки должен оказаться в центре вьюпорта (500, 350).
        let center_screen = c.camera.world_to_screen((mn + mx) * 0.5);
        assert!((center_screen - Vec2::new(500.0, 350.0)).length() < 1e-2);
        assert_ne!(c.camera.zoom, zoom_before);
    }

    #[test]
    fn bring_send_forward_selection() {
        let mut c = CanvasController::new();
        let a = c.scene.roots()[0];
        let b = c.scene.roots()[1];
        c.select(a);
        c.select_layer(b, true, false);
        // Оба в конце списка: bring forward не может поднять b (уже верхний).
        c.bring_forward_selection();
        let roots: Vec<_> = c.scene.roots().iter().copied().collect();
        assert_eq!(roots.last(), Some(&b));
        c.send_backward_selection();
        let roots: Vec<_> = c.scene.roots().iter().copied().collect();
        assert_eq!(roots[0], a, "a ушёл наверх списка");
        assert_eq!(roots[1], b);
    }

    #[test]
    fn fit_to_content_centers_content() {
        let mut c = CanvasController::new();
        // Все три корневых узла: границы мира [80,80]..[440,380].
        c.fit_to_content();
        c.scene.flush_transforms();
        let keys = c.scene.walk();
        let mn = keys
            .iter()
            .filter_map(|k| c.scene.world_bbox(*k).map(|(a, _)| a))
            .reduce(|a, b| a.min(b))
            .unwrap();
        let mx = keys
            .iter()
            .filter_map(|k| c.scene.world_bbox(*k).map(|(_, b)| b))
            .reduce(|a, b| a.max(b))
            .unwrap();
        let center_screen = c.camera.world_to_screen((mn + mx) * 0.5);
        assert!((center_screen - Vec2::new(500.0, 350.0)).length() < 1e-2);
        // Весь контент видим во вьюпорте 1000x700.
        let tl = c.camera.world_to_screen(mn);
        let br = c.camera.world_to_screen(mx);
        assert!(tl.x >= 0.0 && tl.y >= 0.0 && br.x <= 1000.0 && br.y <= 700.0);
    }

    #[test]
    fn nudge_moves_selection() {
        let mut c = CanvasController::new();
        c.grid.snap = false;
        let a = c.scene.roots()[0];
        let b = c.scene.roots()[1];
        c.select(a);
        c.select_layer(b, true, false);
        let ta0 = c.scene.get(a).unwrap().local_transform.translation;
        let tb0 = c.scene.get(b).unwrap().local_transform.translation;
        c.nudge(5.0, -3.0);
        let ta1 = c.scene.get(a).unwrap().local_transform.translation;
        let tb1 = c.scene.get(b).unwrap().local_transform.translation;
        assert!((ta1 - ta0 - Vec2::new(5.0, -3.0)).length() < 1e-3);
        assert!((tb1 - tb0 - Vec2::new(5.0, -3.0)).length() < 1e-3);
    }

    #[test]
    fn deep_select_picks_below_top() {
        let mut c = CanvasController::new();
        c.grid.snap = false;
        // Ellipse (260,200)-(400,290) перекрывает Rectangle (120,130)-(440,380).
        let rect = c.scene.roots()[1];
        let ellipse = c.scene.roots()[2];
        // Точка внутри обоих. Обычный pick вернёт меньший (Ellipse, сверху).
        let world = Vec2::new(320.0, 240.0);
        let top = pick(&mut c.scene, world).unwrap();
        assert_eq!(top, ellipse);
        let screen = c.camera.world_to_screen(world);
        // Обычный клик выбирает верхний узел.
        c.pointer_down_mod(screen, 1, false, false);
        c.pointer_up(screen);
        assert_eq!(c.scene.selection().to_vec(), vec![ellipse]);
        // Ctrl+клик при выбранном верхнем узле выбирает следующий ниже по стеку.
        c.pointer_down_mod(screen, 1, true, false);
        c.pointer_up(screen);
        assert_eq!(c.scene.selection().to_vec(), vec![rect]);
    }

    #[test]
    fn space_pan_ignores_tool() {
        let mut c = CanvasController::new();
        // Инструмент Select, но с зажатым пробелом — панорамирование.
        c.pointer_down_mod(Vec2::new(100.0, 100.0), 1, false, true);
        assert!(matches!(c.drag.as_ref().unwrap().mode, DragMode::Pan));
    }

    #[test]
    fn copy_paste_roundtrip() {
        let mut c = CanvasController::new();
        let a = c.scene.roots()[0];
        let _child = c.scene.insert_child(a, "Child", kind_for_tool(Tool::Rectangle)).unwrap();
        c.select(a);
        let before = c.scene.roots().len();
        c.copy_selection();
        assert!(c.clipboard.is_some());
        // Ctrl+V: копия как новый корень со смещением +16, выделяется.
        c.paste();
        assert_eq!(c.scene.roots().len(), before + 1);
        assert_eq!(c.scene.selection().len(), 1);
        let copy = c.scene.selection()[0];
        assert_ne!(copy, a);
        // Копия несёт ребёнка, имя совпадает.
        let copy_children = c.scene.get(copy).unwrap().children.clone();
        assert_eq!(copy_children.len(), 1);
        let cc = copy_children[0];
        assert_eq!(c.scene.get(cc).unwrap().name, "Child");
        assert_eq!(c.scene.get(cc).unwrap().parent, Some(copy));
        // Смещение +16 относительно оригинала.
        let ta = c.scene.get(a).unwrap().local_transform.translation;
        let tc = c.scene.get(copy).unwrap().local_transform.translation;
        assert!((tc - ta - Vec2::new(16.0, 16.0)).length() < 1e-3);
    }

    #[test]
    fn paste_in_place_keeps_position() {
        let mut c = CanvasController::new();
        let a = c.scene.roots()[0];
        c.select(a);
        c.copy_selection();
        c.paste_in_place();
        let copy = c.scene.selection()[0];
        let ta = c.scene.get(a).unwrap().local_transform.translation;
        let tc = c.scene.get(copy).unwrap().local_transform.translation;
        assert!((ta - tc).length() < 1e-3, "Shift+V без смещения");
    }

    #[test]
    fn cut_removes_original_and_pastes() {
        let mut c = CanvasController::new();
        let a = c.scene.roots()[0];
        c.select(a);
        let before = c.scene.roots().len();
        c.cut_selection();
        assert_eq!(c.scene.roots().len(), before - 1);
        assert!(!c.scene.contains(a));
        c.paste_in_place();
        assert_eq!(c.scene.roots().len(), before);
        assert_eq!(c.scene.selection().len(), 1);
    }

    #[test]
    fn alt_drag_duplicates_on_pointer_down() {
        let mut c = CanvasController::new();
        c.grid.snap = false;
        let a = c.scene.roots()[0];
        let world = Vec2::new(120.0, 130.0); // внутри Frame
        let screen = c.camera.world_to_screen(world);
        let before = c.scene.roots().len();
        c.pointer_down_full(screen, 1, false, true, false);
        // Создана копия-сиблинг, драг начался для копий.
        assert_eq!(c.scene.roots().len(), before + 1);
        assert_eq!(c.scene.selection().len(), 1);
        assert_ne!(c.scene.selection()[0], a);
        assert!(matches!(c.drag.as_ref().unwrap().mode, DragMode::MoveMany { .. }));
        // Тянем копию в сторону.
        c.pointer_move(screen + Vec2::new(30.0, 20.0));
        c.pointer_up(screen + Vec2::new(30.0, 20.0));
        let ta = c.scene.get(a).unwrap().local_transform.translation;
        let tc = c.scene.get(c.scene.selection()[0]).unwrap().local_transform.translation;
        // Оригинал не сдвинулся, копия уехала от него.
        assert_eq!(ta, Vec2::new(80.0, 80.0));
        assert!((tc - Vec2::new(80.0, 80.0)).length() > 10.0);
    }

    #[test]
    fn reparent_selection_moves_into_frame_preserving_world() {
        let mut c = CanvasController::new();
        c.grid.snap = false;
        let frame = c.scene.roots()[0];
        let rect = c.scene.roots()[1];
        c.select(rect);
        c.scene.flush_transforms();
        let (mn, mx) = c.scene.world_bbox(rect).unwrap();

        c.reparent_selection(Some(frame));

        assert_eq!(c.scene.get(rect).unwrap().parent, Some(frame));
        c.scene.flush_transforms();
        let (mn2, mx2) = c.scene.world_bbox(rect).unwrap();
        assert!((mn - mn2).length() < 0.01);
        assert!((mx - mx2).length() < 0.01);
        // Узел больше не в корнях.
        assert!(!c.scene.roots().contains(&rect));
    }

    #[test]
    fn reparent_selection_to_root_removes_from_frame() {
        let mut c = CanvasController::new();
        let frame = c.scene.roots()[0];
        let rect = c.scene.roots()[1];
        c.select(rect);
        c.reparent_selection(Some(frame));
        assert_eq!(c.scene.get(rect).unwrap().parent, Some(frame));

        c.reparent_selection(None);
        assert_eq!(c.scene.get(rect).unwrap().parent, None);
        assert!(c.scene.roots().contains(&rect));
    }

    #[test]
    fn reparent_selection_same_parent_is_noop() {
        let mut c = CanvasController::new();
        let frame = c.scene.roots()[0];
        let rect = c.scene.roots()[1];
        c.select(rect);
        c.reparent_selection(Some(frame));
        // Повторный перенос в тот же родитель ничего не меняет (не переупорядочивает).
        let before: Vec<NodeKey> = c.scene.get(frame).unwrap().children.clone();
        c.reparent_selection(Some(frame));
        let after: Vec<NodeKey> = c.scene.get(frame).unwrap().children.clone();
        assert_eq!(before, after);
    }

    #[test]
    fn wrap_in_frame_creates_parent_for_selection() {
        let mut c = CanvasController::new();
        c.grid.snap = false;
        let rect = c.scene.roots()[1];
        let ellipse = c.scene.roots()[2];
        c.scene.set_selection(vec![rect, ellipse]);
        c.scene.flush_transforms();
        let (rmn, _rmx) = c.scene.world_bbox(rect).unwrap();
        let (_emn, emx) = c.scene.world_bbox(ellipse).unwrap();

        c.wrap_in_frame();

        assert_eq!(c.scene.selection().len(), 1);
        let fk = c.scene.selection()[0];
        assert!(matches!(c.scene.get(fk).unwrap().kind, NodeKind::Frame { .. }));
        assert_eq!(c.scene.get(rect).unwrap().parent, Some(fk));
        assert_eq!(c.scene.get(ellipse).unwrap().parent, Some(fk));
        // Фрейм охватывает оба узла; мировые позиции детей сохранены.
        let (fmn, fmx) = c.scene.world_bbox(fk).unwrap();
        assert!(fmn.x <= rmn.x && fmn.y <= rmn.y);
        assert!(fmx.x >= emx.x && fmx.y >= emx.y);
        c.scene.flush_transforms();
        assert!((c.scene.world_bbox(rect).unwrap().0 - rmn).length() < 0.01);
        assert!((c.scene.world_bbox(ellipse).unwrap().1 - emx).length() < 0.01);
    }

    #[test]
    fn drag_into_frame_reparents_on_pointer_up() {
        let mut c = CanvasController::new();
        c.grid.snap = false;
        let frame = c.scene.roots()[0];
        let ellipse = c.scene.roots()[2];
        let world = Vec2::new(330.0, 245.0); // центр эллипса
        let screen = c.camera.world_to_screen(world);

        c.pointer_down_full(screen, 1, false, false, false);
        assert!(matches!(c.drag.as_ref().unwrap().mode, DragMode::Move { key } if key == ellipse));

        // Тащим в угол фрейма (внутрь Frame, но мимо Rect/Ellipse).
        let drop_world = Vec2::new(100.0, 300.0);
        let drop_screen = c.camera.world_to_screen(drop_world);
        c.pointer_move(drop_screen);
        assert_eq!(c.hovered_frame, Some(frame));

        c.pointer_up(drop_screen);
        assert_eq!(c.scene.get(ellipse).unwrap().parent, Some(frame));
        assert!(c.hovered_frame.is_none());
    }

    #[test]
    fn drag_outside_any_frame_does_not_reparent() {
        let mut c = CanvasController::new();
        c.grid.snap = false;
        let ellipse = c.scene.roots()[2];
        let world = Vec2::new(330.0, 245.0);
        let screen = c.camera.world_to_screen(world);
        c.pointer_down_full(screen, 1, false, false, false);

        // Точка вне всех фреймов.
        let drop_screen = c.camera.world_to_screen(Vec2::new(500.0, 100.0));
        c.pointer_move(drop_screen);
        assert_eq!(c.hovered_frame, None);
        c.pointer_up(drop_screen);
        assert_eq!(c.scene.get(ellipse).unwrap().parent, None);
    }

    #[test]
    fn drop_frame_at_skips_moving_subtree() {
        let mut c = CanvasController::new();
        let frame = c.scene.roots()[0];
        // Тащим сам фрейм — цель не должна указывать на него (или его потомков).
        let world = Vec2::new(100.0, 100.0);
        assert_ne!(c.drop_frame_at(world, &[frame]), Some(frame));
    }

    #[test]
    fn flex_mode_preserves_direction_and_resets_off() {
        let mut c = CanvasController::new();
        let frame = c.scene.roots()[0];
        c.select(frame);

        c.set_auto_layout_mode(1);
        assert_eq!(c.frame_auto_layout().map(|cfg| cfg.direction), Some(LayoutDirection::Horizontal));

        c.set_auto_layout_direction(1);
        assert_eq!(c.frame_auto_layout().map(|cfg| cfg.direction), Some(LayoutDirection::Vertical));

        // Выключение → None; повторное включение сохраняет направление.
        c.set_auto_layout_mode(0);
        assert_eq!(c.frame_auto_layout(), None);
        c.set_auto_layout_mode(1);
        assert_eq!(c.frame_auto_layout().map(|cfg| cfg.direction), Some(LayoutDirection::Vertical));
    }

    #[test]
    fn flex_align_and_justify_setters() {
        let mut c = CanvasController::new();
        let frame = c.scene.roots()[0];
        c.select(frame);
        c.set_auto_layout_mode(1);

        c.apply_auto_layout_align(2);
        assert_eq!(c.frame_auto_layout().map(|cfg| cfg.align_items), Some(LayoutAlign::Center));
        c.apply_auto_layout_justify(3);
        assert_eq!(c.frame_auto_layout().map(|cfg| cfg.justify_content), Some(LayoutJustify::SpaceBetween));
        c.apply_auto_layout_align(0);
        assert_eq!(c.frame_auto_layout().map(|cfg| cfg.align_items), Some(LayoutAlign::Stretch));
        c.apply_auto_layout_justify(1);
        assert_eq!(c.frame_auto_layout().map(|cfg| cfg.justify_content), Some(LayoutJustify::Center));
    }

    #[test]
    fn drag_out_of_frame_goes_to_root_on_empty_drop() {
        let mut c = CanvasController::new();
        c.grid.snap = false;
        let frame = c.scene.roots()[0];
        let rect = c.scene.roots()[1];
        c.select(rect);
        c.reparent_selection(Some(frame));
        assert_eq!(c.scene.get(rect).unwrap().parent, Some(frame));

        // Тащим ребёнка на пустое место (вне всех фреймов) и отпускаем.
        let screen = c.camera.world_to_screen(Vec2::new(200.0, 180.0));
        c.pointer_down_full(screen, 1, false, false, false);
        let drop_screen = c.camera.world_to_screen(Vec2::new(900.0, 700.0));
        c.pointer_move(drop_screen);
        assert_eq!(c.hovered_frame, None);
        c.pointer_up(drop_screen);
        assert_eq!(c.scene.get(rect).unwrap().parent, None);
        assert!(c.scene.roots().contains(&rect));
    }

    #[test]
    fn enabling_flex_does_not_stretch_children() {
        let mut c = CanvasController::new();
        let frame = c.scene.roots()[0];
        let r1 = c.scene.insert_child(frame, "R1", NodeKind::Shape(ShapeKind::Rectangle { size: Vec2::new(50.0, 20.0), corner_radii: [0.0; 4] })).unwrap();
        let r2 = c.scene.insert_child(frame, "R2", NodeKind::Shape(ShapeKind::Rectangle { size: Vec2::new(50.0, 20.0), corner_radii: [0.0; 4] })).unwrap();
        c.select(frame);
        c.set_auto_layout_mode(1);
        c.scene.flush_transforms();
        // Дети НЕ растянуты по высоте фрейма (align=Min, а не Stretch).
        let (a, b) = c.scene.get(r1).unwrap().kind.local_bbox();
        assert!((b.y - a.y - 20.0).abs() < 0.01, "r1 высота={}", b.y - a.y);
        assert_eq!(c.frame_auto_layout().map(|cfg| cfg.align_items), Some(LayoutAlign::Min));
        let _ = r2;
    }

    #[test]
    fn flex_align_center_positions_children() {
        let mut c = CanvasController::new();
        let frame = c.scene.roots()[0];
        // Кладём два прямоугольника во фрейм как детей.
        let r1 = c.scene.insert_child(frame, "R1", NodeKind::Shape(ShapeKind::Rectangle { size: Vec2::new(50.0, 20.0), corner_radii: [0.0; 4] })).unwrap();
        let r2 = c.scene.insert_child(frame, "R2", NodeKind::Shape(ShapeKind::Rectangle { size: Vec2::new(50.0, 20.0), corner_radii: [0.0; 4] })).unwrap();
        // Фрейм без трансформации, flex Row, align Center, justify Center, gap 10.
        if let Some(n) = c.scene.get_mut(frame) {
            if let NodeKind::Frame { size, auto_layout, .. } = &mut n.kind {
                *size = Vec2::new(300.0, 100.0);
                *auto_layout = Some(AutoLayoutConfig {
                    direction: LayoutDirection::Horizontal,
                    spacing: 10.0,
                    padding: [0.0; 4],
                    align_items: LayoutAlign::Center,
                    justify_content: LayoutJustify::Center,
                });
            }
        }
        c.scene.flush_transforms();
        let t1 = c.scene.world_transform(r1).unwrap().translation;
        let t2 = c.scene.world_transform(r2).unwrap().translation;
        // Фрейм в (80,80); ряд: суммарная ширина 110 + gap 10, свободные 180 →
        // начало в 80+95=175; второй в 80+155=235.
        assert!((t1.x - 175.0).abs() < 0.01, "t1.x={}", t1.x);
        assert!((t2.x - 235.0).abs() < 0.01, "t2.x={}", t2.x);
        // Центр по Y: 80 + (100-20)/2 = 120.
        assert!((t1.y - 120.0).abs() < 0.01, "t1.y={}", t1.y);
        assert!((t2.y - 120.0).abs() < 0.01, "t2.y={}", t2.y);
    }

    // --- Текстовые узлы ---

    #[test]
    fn text_tool_click_creates_and_edits() {
        let mut c = CanvasController::new();
        c.set_tool(Tool::Text);
        c.grid.snap = false;
        // Клик (без драга): создание узла в точке клика + сразу редактирование.
        c.pointer_down(Vec2::new(50.0, 60.0), 1);
        c.pointer_up(Vec2::new(50.0, 60.0));
        let key = c.scene.selection()[0];
        assert!(matches!(c.scene.get(key).unwrap().kind, NodeKind::Text { .. }));
        // Позиция узла = точка клика (AutoWidth).
        assert_eq!(c.scene.get(key).unwrap().local_transform.translation, Vec2::new(50.0, 60.0));
        assert_eq!(c.editing, Some(key));
        c.end_text_edit();
        assert_eq!(c.editing, None);
    }

    #[test]
    fn text_bbox_measured_from_content() {
        let mut c = CanvasController::new();
        let key = c.scene.insert_root(
            "Text",
            NodeKind::Text {
                content: "Hello".into(),
                font_family: "Inter".into(),
                font_size: 16.0,
                font_weight: 400,
                line_height: 1.2,
                letter_spacing: 0.0,
                align: TextAlign::Left,
                size_mode: TextSizeMode::AutoWidth,
            },
        );
        c.scene.flush_transforms();
        let (mn, mx) = c.scene.world_bbox(key).unwrap();
        assert!(mx.x > 0.0);
        assert!((mx.y - 16.0 * 1.2).abs() < 0.01);
        assert_eq!(mn, Vec2::ZERO);
    }

    #[test]
    fn text_edit_live_updates_content() {
        let mut c = CanvasController::new();
        let key = c.scene.insert_root(
            "Text",
            NodeKind::Text {
                content: "Hello".into(),
                font_family: "Inter".into(),
                font_size: 16.0,
                font_weight: 400,
                line_height: 1.2,
                letter_spacing: 0.0,
                align: TextAlign::Left,
                size_mode: TextSizeMode::AutoWidth,
            },
        );
        c.begin_text_edit(key);
        c.set_editing_text("World!");
        c.end_text_edit();
        let n = c.scene.get(key).unwrap();
        if let NodeKind::Text { content, .. } = &n.kind {
            assert_eq!(content, "World!");
        } else {
            panic!("не Text");
        }
    }

    #[test]
    fn text_align_and_font_size_apply() {
        let mut c = CanvasController::new();
        let key = c.scene.insert_root(
            "Text",
            NodeKind::Text {
                content: "Hi".into(),
                font_family: "Inter".into(),
                font_size: 16.0,
                font_weight: 400,
                line_height: 1.2,
                letter_spacing: 0.0,
                align: TextAlign::Left,
                size_mode: TextSizeMode::AutoWidth,
            },
        );
        c.scene.set_selection(vec![key]);
        c.set_text_align(TextAlign::Center);
        c.apply_font_size_live(24.0);
        let n = c.scene.get(key).unwrap();
        if let NodeKind::Text { font_size, align, .. } = &n.kind {
            assert_eq!(*align, TextAlign::Center);
            assert_eq!(*font_size, 24.0);
        } else {
            panic!("не Text");
        }
    }

    // --- Поворот сохраняется при переносе/ресайзе/вводе позиции ---

    fn rotation_of(c: &CanvasController, key: NodeKey) -> f32 {
        let n = c.scene.get(key).unwrap();
        n.local_transform.matrix2.x_axis.y.atan2(n.local_transform.matrix2.x_axis.x).to_degrees()
    }

    #[test]
    fn move_drag_preserves_rotation() {
        let mut c = CanvasController::new();
        c.grid.snap = false;
        // Свой узел без пересечений с демо-контентом (демо-фрейм перекрыт
        // прямоугольником/эллипсом, и центр его AABB попадает в них).
        let key = c.scene.insert_root(
            "R",
            NodeKind::Shape(ShapeKind::Rectangle {
                size: Vec2::new(100.0, 100.0),
                corner_radii: [0.0; 4],
            }),
        );
        c.scene.set_selection(vec![key]);
        c.scene.flush_transforms();
        c.apply_rotation_live(30.0);
        c.scene.flush_transforms();
        assert!((rotation_of(&c, key) - 30.0).abs() < 1e-3);

        // Драг за центр узла: позиция меняется, поворот сохраняется.
        let start = c.camera.world_to_screen(Vec2::new(50.0, 50.0));
        let end = start + Vec2::new(40.0, 25.0);
        let t0 = c.scene.get(key).unwrap().local_transform.translation;
        c.pointer_down(start, 1);
        assert!(
            matches!(&c.drag.as_ref().unwrap().mode, DragMode::Move { .. }),
            "драг должен быть Move"
        );
        c.pointer_move(end);
        c.scene.flush_transforms();
        let n = c.scene.get(key).unwrap();
        assert!(
            (rotation_of(&c, key) - 30.0).abs() < 1e-3,
            "поворот сброшен при Move: {}",
            rotation_of(&c, key)
        );
        let delta = n.local_transform.translation - t0;
        // snap_point всегда округляет до целых пикселей (даже без сетки).
        assert!(
            (delta.x - 40.0).abs() < 0.6 && (delta.y - 25.0).abs() < 0.6,
            "сдвиг: {delta:?}"
        );
        c.pointer_up(end);
    }

    #[test]
    fn move_many_preserves_rotation() {
        let mut c = CanvasController::new();
        c.grid.snap = false;
        c.scene.flush_transforms();
        let a = c.scene.roots()[0];
        let b = c.scene.roots()[1];
        c.scene.set_selection(vec![a, b]);
        c.apply_rotation_live(45.0);
        c.scene.flush_transforms();

        // Точка (100,100) лежит только в a — драг начнётся как MoveMany.
        let start = c.camera.world_to_screen(Vec2::new(100.0, 100.0));
        let end = start + Vec2::new(30.0, 20.0);
        c.pointer_down(start, 1);
        assert!(
            matches!(&c.drag.as_ref().unwrap().mode, DragMode::MoveMany { .. }),
            "драг по выделенному узлу при мультивыборе должен быть MoveMany"
        );
        c.pointer_move(end);
        c.scene.flush_transforms();
        for k in [a, b] {
            assert!(
                (rotation_of(&c, k) - 45.0).abs() < 1e-3,
                "поворот сброшен при MoveMany: {}",
                rotation_of(&c, k)
            );
        }
        c.pointer_up(end);
    }

    #[test]
    fn resize_drag_preserves_rotation() {
        let mut c = CanvasController::new();
        c.grid.snap = false;
        c.scene.flush_transforms();
        let key = c.scene.roots()[0]; // Frame — size-kind.
        c.select(key);
        c.apply_rotation_live(90.0);
        c.scene.flush_transforms();

        // Драг за SE-хэндол: размер меняется, поворот сохраняется.
        let (_, mx) = c.scene.world_bbox(key).unwrap();
        let se = c.camera.world_to_screen(mx);
        c.pointer_down(se, 1);
        assert!(
            matches!(&c.drag.as_ref().unwrap().mode, DragMode::Resize { handle: Handle::Se, .. }),
            "SE-хэндол должен начать ресайз"
        );
        let size_before = match &c.scene.get(key).unwrap().kind {
            NodeKind::Frame { size, .. } => *size,
            _ => panic!("ожидался фрейм"),
        };
        c.pointer_move(se + Vec2::new(30.0, 20.0));
        c.scene.flush_transforms();
        let n = c.scene.get(key).unwrap();
        assert!(
            (rotation_of(&c, key) - 90.0).abs() < 1e-3,
            "поворот сброшен при ресайзе: {}",
            rotation_of(&c, key)
        );
        let size_after = match &n.kind {
            NodeKind::Frame { size, .. } => *size,
            _ => panic!("ожидался фрейм"),
        };
        assert!(size_after != size_before, "размер должен измениться");
    }

    #[test]
    fn position_field_preserves_rotation() {
        let mut c = CanvasController::new();
        c.grid.snap = false;
        let key = c.scene.roots()[0];
        c.select(key);
        c.apply_rotation_live(60.0);
        c.scene.flush_transforms();
        c.apply_position_live(50.0, 70.0);
        let n = c.scene.get(key).unwrap();
        assert!(
            (rotation_of(&c, key) - 60.0).abs() < 1e-3,
            "поворот сброшен при вводе позиции: {}",
            rotation_of(&c, key)
        );
        assert!((n.local_transform.translation.x - 50.0).abs() < 1e-3);
        assert!((n.local_transform.translation.y - 70.0).abs() < 1e-3);
    }

    // --- Drag&drop в панели слоёв ---

    #[test]
    fn drop_layer_at_nests_into_frame() {
        let mut c = CanvasController::new();
        let frame = c.scene.roots()[0];
        let rect = c.scene.roots()[1];
        c.drop_layer_at(rect, frame, 1);
        assert_eq!(c.scene.get(rect).unwrap().parent, Some(frame));
        c.scene.flush_transforms();
        // Мировая позиция сохраняется при вложении (пересчёт в локальную).
        let (mn, _) = c.scene.world_bbox(rect).unwrap();
        assert!((mn.x - 120.0).abs() < 1e-3 && (mn.y - 130.0).abs() < 1e-3, "mn: {mn:?}");
    }

    #[test]
    fn drop_layer_at_reorders_before_after() {
        let mut c = CanvasController::new();
        let a = c.scene.roots()[0];
        let b = c.scene.roots()[1];
        let ell = c.scene.roots()[2];
        // Зона 0 — вставка ДО цели: ell уходит наверх.
        c.drop_layer_at(ell, a, 0);
        let roots = c.scene.roots();
        assert_eq!(roots, vec![ell, a, b]);
        // Зона 2 — вставка ПОСЛЕ цели: b встаёт сразу за ell.
        c.drop_layer_at(b, ell, 2);
        let roots = c.scene.roots();
        assert_eq!(roots, vec![ell, b, a]);
    }

    #[test]
    fn drop_layer_at_rejects_cycle() {
        let mut c = CanvasController::new();
        let frame = c.scene.roots()[0];
        let rect = c.scene.roots()[1];
        c.drop_layer_at(rect, frame, 1);
        assert_eq!(c.scene.get(rect).unwrap().parent, Some(frame));
        // Фрейм — предок rect: вложение и вставка рядом с ним создали бы цикл.
        c.drop_layer_at(frame, rect, 1);
        assert_eq!(c.scene.get(frame).unwrap().parent, None, "цикл должен быть отклонён");
        c.drop_layer_at(frame, rect, 0);
        assert_eq!(c.scene.get(frame).unwrap().parent, None);
    }
}