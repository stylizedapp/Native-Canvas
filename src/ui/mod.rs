//! UI-слой: тип обёртки Slint-генераций, состояние холста, sync-цикл,
//! колбэки инспектора и построитель дерева слоёв.

use crate::engine::controller::CanvasController;
use crate::engine::model::nodes::{NodeKey, NodeKind, ShapeKind};
use crate::engine::model::scene::SceneNode;
use crate::engine::model::types::Paint;
use crate::engine::profiler::FrameProfiler;
use crate::engine::renderers::Renderer;
use slint::{ComponentHandle, Image, ModelRc, Rgba8Pixel, SharedPixelBuffer, VecModel};
use slotmap::Key;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

slint::include_modules!();

pub type Controller = Rc<RefCell<CanvasController>>;
pub type RendererRef = Rc<RefCell<Box<dyn Renderer>>>;

/// Максимальный размер длинной стороны буфера рендера (кэп разрешения).
/// Снижает нагрузку растеризации на больших окнах; ввод остаётся точным
/// (координаты логические, растеризация масштабируется отдельно).
const MAX_RENDER_DIM: f32 = 1920.0;

/// Переиспользуемые буферы холста, статистика и кэш UI-строк (чтобы не
/// переустанавливать неизменные свойства Slint на каждом кадре).
pub struct CanvasState {
    /// Двойной буфер: пишем в слот, на который окно не ссылается (refcount=1),
    /// чтобы `make_mut_bytes` не делал COW-копию 5 МБ на каждом кадре.
    pub images: [SharedPixelBuffer<Rgba8Pixel>; 2],
    /// Индекс буфера, отданного окну на прошлом кадре.
    pub cur: usize,
    pub w: u32,
    pub h: u32,
    pub last_revision: u64,
    // FPS
    pub fps_frames: u64,
    pub fps_window: Instant,
    pub fps: f64,
    pub frame_count: u64,
    pub last_render_us: u128,
    // Кэш UI (установка только при изменении).
    pub last_sel_id: Option<NodeKey>,
    pub last_zoom_text: String,
    pub last_fps_text: String,
    pub last_tool: String,
    pub last_grid_step_text: String,
    // Кэш инспектора (диф против значений узла; поля не затирают ввод).
    pub last_sel_name: String,
    pub last_sel_x: String,
    pub last_sel_y: String,
    pub last_sel_w: String,
    pub last_sel_h: String,
    pub last_sel_opacity: String,
    pub last_sel_fill: String,
    // Момент последнего ввода пользователя в каждое поле (защита от затирания).
    pub sel_edited_at: [Option<Instant>; 7],
}

/// Индексы полей инспектора в `sel_edited_at` и диф-кэше.
#[derive(Clone, Copy, PartialEq)]
enum SelField {
    Name = 0,
    X = 1,
    Y = 2,
    W = 3,
    H = 4,
    Opacity = 5,
    Fill = 6,
}

impl CanvasState {
    pub fn new() -> Self {
        Self {
            images: [SharedPixelBuffer::new(1, 1), SharedPixelBuffer::new(1, 1)],
            cur: 0,
            w: 0,
            h: 0,
            last_revision: u64::MAX,
            fps_frames: 0,
            fps_window: Instant::now(),
            fps: 0.0,
            frame_count: 0,
            last_render_us: 0,
            last_sel_id: None,
            last_zoom_text: String::new(),
            last_fps_text: String::new(),
            last_tool: String::new(),
            last_grid_step_text: String::new(),
            last_sel_name: String::new(),
            last_sel_x: String::new(),
            last_sel_y: String::new(),
            last_sel_w: String::new(),
            last_sel_h: String::new(),
            last_sel_opacity: String::new(),
            last_sel_fill: String::new(),
            sel_edited_at: [None; 7],
        }
    }

    /// Обновляет размер буфера. Возвращает true, если размер изменился.
    pub fn ensure(&mut self, w: u32, h: u32) -> bool {
        let w = w.max(1);
        let h = h.max(1);
        if w != self.w || h != self.h {
            self.w = w;
            self.h = h;
            self.images = [SharedPixelBuffer::new(w, h), SharedPixelBuffer::new(w, h)];
            true
        } else {
            false
        }
    }
}

impl Default for CanvasState {
    fn default() -> Self {
        Self::new()
    }
}

/// Один тик on-demand цикла рендера: рендерит только если есть изменения
/// (`dirty`) или изменился размер буфера. При чистом состоянии — мгновенный
/// возврат без касания GPU/VRAM (сон). Пауза также когда окно свёрнуто/скрыто.
pub fn sync(
    window: &slint::Weak<AppWindow>,
    controller: &Controller,
    renderer: &RendererRef,
    state: &Rc<RefCell<CanvasState>>,
    profiler: &Rc<RefCell<FrameProfiler>>,
) {
    let Some(w) = window.upgrade() else { return };

    // Пауза: окно свёрнуто или скрыто.
    if !w.window().is_visible() || w.window().is_minimized() {
        return;
    }

    let (area_w, area_h) = (
        w.get_canvas_width().round().max(1.0),
        w.get_canvas_height().round().max(1.0),
    );

    // Кэп разрешения: буфер = область * scale (ввод остаётся в логических px).
    let scale = (MAX_RENDER_DIM / area_w.max(area_h)).min(1.0);
    let (bw, bh) = (
        (area_w * scale).round().max(1.0) as u32,
        (area_h * scale).round().max(1.0) as u32,
    );

    let mut c = controller.borrow_mut();
    let mut st = state.borrow_mut();

    // On-demand: ничего не менялось и размер прежний — рендер не нужен.
    let size_changed = st.ensure(bw, bh);
    if !c.dirty && !size_changed {
        return;
    }

    // Пересчёт кэшированных мировых трансформаций (хит-тест, рамки выделения).
    c.scene.flush_transforms();

    // Рендер прямо в переиспользуемый буфер (не тот, что отдан окну).
    let render_cam = c.camera.for_render_scale(scale);
    let next = st.cur ^ 1;
    let t0 = Instant::now();
    let outcome = {
        let (rw, rh) = (st.w, st.h);
        let bytes = st.images[next].make_mut_bytes();
        renderer.borrow_mut().render(
            &c.scene,
            &render_cam,
            rw,
            rh,
            c.scene.selection(),
            c.grid,
            c.preview.clone(),
            bytes,
        )
    };
    let mut metrics = outcome.metrics;
    metrics.total_us = t0.elapsed().as_micros();
    profiler.borrow_mut().record(metrics);

    // Кадр принят бэкендом — сбрасываем dirty. Если GPU пропустил кадр,
    // оставляем dirty, чтобы дорисовать в следующий тик (сторож 200ms).
    let submitted = outcome.submitted;
    c.dirty = !submitted;
    st.last_render_us = metrics.total_us;
    st.frame_count += 1;

    // FPS (раз в секунду).
    st.fps_frames += 1;
    let now = Instant::now();
    let win_secs = now.duration_since(st.fps_window).as_secs_f64();
    if win_secs >= 1.0 {
        st.fps = st.fps_frames as f64 / win_secs;
        st.fps_frames = 0;
        st.fps_window = now;
    }

    w.set_canvas_texture(Image::from_rgba8(st.images[next].clone()));
    st.cur = next;

    // Верхняя плашка — только при изменении.
    let zoom = format!("{:.0}%", c.camera.zoom * 100.0);
    if zoom != st.last_zoom_text {
        st.last_zoom_text = zoom.clone();
        w.set_zoom_text(zoom.into());
    }
    let fps = format!("{:.0} FPS", st.fps);
    if fps != st.last_fps_text {
        st.last_fps_text = fps.clone();
        w.set_fps_text(fps.into());
    }
    let tool = c.tool.name().to_string();
    if tool != st.last_tool {
        st.last_tool = tool.clone();
        w.set_tool(tool.into());
    }
    let grid_step = fmt_num(c.grid.step);
    if grid_step != st.last_grid_step_text {
        st.last_grid_step_text = grid_step.clone();
        w.set_grid_step_text(grid_step.into());
    }

    // Дерево слоёв — только при изменении структуры.
    if c.revision != st.last_revision {
        st.last_revision = c.revision;
        let items: Vec<LayerItem> = build_layers(&c);
        let model: ModelRc<LayerItem> = Rc::new(VecModel::from(items)).into();
        w.set_layers(model);
    }
    w.set_selected_layer(c.scene.selection().first().map(|&k| k.data().as_ffi() as i32).unwrap_or(-1));

    // Инспектор: диф по значениям каждый кадр (живые правки при драге).
    // При смене узла сбрасываем кэш и cooldown — все поля обновятся принудительно.
    let sel_id = c.selected_id();
    if sel_id != st.last_sel_id {
        st.last_sel_id = sel_id;
        st.last_sel_name.clear();
        st.last_sel_x.clear();
        st.last_sel_y.clear();
        st.last_sel_w.clear();
        st.last_sel_h.clear();
        st.last_sel_opacity.clear();
        st.last_sel_fill.clear();
        st.sel_edited_at = [None; 7];
    }
    match c.selected() {
        Some(n) => {
            w.set_has_selection(true);
            update_sel(&w, n, &mut st);
        }
        None => w.set_has_selection(false),
    }

    // Дебагер.
    w.set_debug_text(
        format!(
            "FPS {:.0}  |  render {:.2} ms\n\
             Nodes {}  |  Buffer {}x{} (x{:.0}%)\n\
             Camera pan({:.0},{:.0}) zoom {:.2}\n\
             Tool: {}  |  Revision: {}\n\
             Grid: {}  |  Snap: {}  |  Step: {}\n\
             Backend: {}",
            st.fps,
            st.last_render_us as f64 / 1000.0,
            c.scene.len(),
            bw,
            bh,
            scale * 100.0,
            c.camera.pan.x,
            c.camera.pan.y,
            c.camera.zoom,
            c.tool.name(),
            c.revision,
            if c.grid.visible { "on" } else { "off" },
            if c.grid.snap { "on" } else { "off" },
            c.grid.step,
            renderer.borrow().name(),
        )
        .into(),
    );
}

/// Поле инспектора только что отредактировано пользователем — помечаем,
/// чтобы sync не затирал ввод в течение cooldown.
fn mark_edited(st: &Rc<RefCell<CanvasState>>, field: SelField) {
    st.borrow_mut().sel_edited_at[field as usize] = Some(Instant::now());
}

pub fn register_inspector_callbacks(
    window: &AppWindow,
    controller: &Controller,
    state: &Rc<RefCell<CanvasState>>,
    render: &Rc<dyn Fn()>,
) {
    let c = controller.clone();
    let weak = window.as_weak();
    let st = state.clone();
    let rr = Rc::clone(render);
    window.on_set_name(move |name| {
        mark_edited(&st, SelField::Name);
        c.borrow_mut().set_name(name.as_str());
        refresh_sel(&weak, &c);
        rr();
    });

    let c = controller.clone();
    let weak = window.as_weak();
    let st = state.clone();
    let rr = Rc::clone(render);
    window.on_set_x(move |v| {
        mark_edited(&st, SelField::X);
        if let Ok(x) = v.trim().parse::<f32>() {
            let y = current_y(&c);
            c.borrow_mut().set_position(x, y);
            refresh_sel(&weak, &c);
        }
        rr();
    });

    let c = controller.clone();
    let weak = window.as_weak();
    let st = state.clone();
    let rr = Rc::clone(render);
    window.on_set_y(move |v| {
        mark_edited(&st, SelField::Y);
        if let Ok(y) = v.trim().parse::<f32>() {
            let x = current_x(&c);
            c.borrow_mut().set_position(x, y);
            refresh_sel(&weak, &c);
        }
        rr();
    });

    let c = controller.clone();
    let weak = window.as_weak();
    let st = state.clone();
    let rr = Rc::clone(render);
    window.on_set_w(move |v| {
        mark_edited(&st, SelField::W);
        if let Ok(w) = v.trim().parse::<f32>() {
            let h = current_h(&c);
            c.borrow_mut().set_size(w, h);
            refresh_sel(&weak, &c);
        }
        rr();
    });

    let c = controller.clone();
    let weak = window.as_weak();
    let st = state.clone();
    let rr = Rc::clone(render);
    window.on_set_h(move |v| {
        mark_edited(&st, SelField::H);
        if let Ok(h) = v.trim().parse::<f32>() {
            let w = current_w(&c);
            c.borrow_mut().set_size(w, h);
            refresh_sel(&weak, &c);
        }
        rr();
    });

    let c = controller.clone();
    let weak = window.as_weak();
    let st = state.clone();
    let rr = Rc::clone(render);
    window.on_set_opacity(move |v| {
        mark_edited(&st, SelField::Opacity);
        if let Ok(o) = v.trim().parse::<f32>() {
            c.borrow_mut().set_opacity(o / 100.0);
            refresh_sel(&weak, &c);
        }
        rr();
    });

    let c = controller.clone();
    let weak = window.as_weak();
    let st = state.clone();
    let rr = Rc::clone(render);
    window.on_set_fill(move |v| {
        mark_edited(&st, SelField::Fill);
        c.borrow_mut().set_fill_hex(v.as_str());
        refresh_sel(&weak, &c);
        rr();
    });
}

fn current_x(c: &Controller) -> f32 {
    c.borrow().selected().map(|n| n.local_transform.translation.x).unwrap_or(0.0)
}
fn current_y(c: &Controller) -> f32 {
    c.borrow().selected().map(|n| n.local_transform.translation.y).unwrap_or(0.0)
}
fn current_w(c: &Controller) -> f32 {
    c.borrow().selected().map(|n| dims(&n.kind).0).unwrap_or(0.0)
}
fn current_h(c: &Controller) -> f32 {
    c.borrow().selected().map(|n| dims(&n.kind).1).unwrap_or(0.0)
}

fn dims(kind: &NodeKind) -> (f32, f32) {
    match kind {
        NodeKind::Frame { size, .. } => (size.x, size.y),
        NodeKind::Shape(ShapeKind::Rectangle { size, .. }) => (size.x, size.y),
        NodeKind::Shape(ShapeKind::Ellipse { radii, .. }) => (radii.x, radii.y),
        _ => (0.0, 0.0),
    }
}

/// Целое, если значение близко к целому, иначе два знака.
fn fmt_num(v: f32) -> String {
    let r = v.round();
    if (v - r).abs() < 1e-4 {
        format!("{}", r as i64)
    } else {
        format!("{:.2}", v)
    }
}

/// Прозрачность в процентах.
fn fmt_pct(opacity: f32) -> String {
    format!("{}", (opacity * 100.0).round() as i64)
}

/// Обновляет одно поле инспектора, если значение изменилось и поле не
/// редактируется пользователем (cooldown ~1 c после последнего ввода).
fn upd_field(
    w: &AppWindow,
    cache: &mut String,
    edited_at: &mut Option<Instant>,
    now: Instant,
    value: String,
    setter: impl FnOnce(&AppWindow, slint::SharedString),
) {
    if *cache == value {
        return;
    }
    let being_edited = match edited_at {
        Some(t) => now.duration_since(*t).as_secs_f64() < 1.0,
        None => false,
    };
    if being_edited {
        return;
    }
    *cache = value.clone();
    setter(w, value.into());
}

/// Диф-обновление полей инспектора из узла (живой отклик при драге,
/// без затирания полей, которые пользователь сейчас редактирует).
fn update_sel(w: &AppWindow, n: &SceneNode, st: &mut CanvasState) {
    let now = Instant::now();
    upd_field(w, &mut st.last_sel_name, &mut st.sel_edited_at[SelField::Name as usize], now, n.name.clone(), AppWindow::set_sel_name);
    upd_field(w, &mut st.last_sel_x, &mut st.sel_edited_at[SelField::X as usize], now, fmt_num(n.local_transform.translation.x), AppWindow::set_sel_x_text);
    upd_field(w, &mut st.last_sel_y, &mut st.sel_edited_at[SelField::Y as usize], now, fmt_num(n.local_transform.translation.y), AppWindow::set_sel_y_text);
    let (dw, dh) = dims(&n.kind);
    upd_field(w, &mut st.last_sel_w, &mut st.sel_edited_at[SelField::W as usize], now, fmt_num(dw), AppWindow::set_sel_w_text);
    upd_field(w, &mut st.last_sel_h, &mut st.sel_edited_at[SelField::H as usize], now, fmt_num(dh), AppWindow::set_sel_h_text);
    upd_field(w, &mut st.last_sel_opacity, &mut st.sel_edited_at[SelField::Opacity as usize], now, fmt_pct(n.opacity), AppWindow::set_sel_opacity_text);
    upd_field(w, &mut st.last_sel_fill, &mut st.sel_edited_at[SelField::Fill as usize], now, sel_fill_hex(n), AppWindow::set_sel_fill);
}

/// Заполняет поля инспектора из узла.
fn set_sel_texts(w: &AppWindow, n: &SceneNode) {
    w.set_sel_name(n.name.clone().into());
    w.set_sel_x_text(fmt_num(n.local_transform.translation.x).into());
    w.set_sel_y_text(fmt_num(n.local_transform.translation.y).into());
    let (dw, dh) = dims(&n.kind);
    w.set_sel_w_text(fmt_num(dw).into());
    w.set_sel_h_text(fmt_num(dh).into());
    w.set_sel_opacity_text(fmt_pct(n.opacity).into());
    w.set_sel_fill(sel_fill_hex(n).into());
}

/// HEX-строка первой сплошной заливки узла ("#RRGGBB").
fn sel_fill_hex(n: &SceneNode) -> String {
    n.fills
        .iter()
        .find_map(|p| match p {
            Paint::Solid(c) => Some(c.to_rgba8()),
            _ => None,
        })
        .map(|c| format!("#{:02X}{:02X}{:02X}", c[0], c[1], c[2]))
        .unwrap_or_else(|| "#000000".into())
}

/// Обновляет поля после commit (значения нормализованы контроллером).
fn refresh_sel(win: &slint::Weak<AppWindow>, c: &Controller) {
    if let Some(w) = win.upgrade() {
        if let Some(n) = c.borrow().selected() {
            set_sel_texts(&w, n);
        }
    }
}

pub fn build_layers(c: &CanvasController) -> Vec<LayerItem> {
    let mut out = Vec::new();
    let mut stack: Vec<(NodeKey, i32)> = c.scene.roots().iter().rev().map(|&k| (k, 0)).collect();
    while let Some((key, depth)) = stack.pop() {
        if let Some(node) = c.scene.get(key) {
            out.push(LayerItem {
                id: key.data().as_ffi() as i32,
                name: node.name.clone().into(),
                depth,
            });
            stack.extend(node.children.iter().rev().map(|&ch| (ch, depth + 1)));
        }
    }
    out
}