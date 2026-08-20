//! UI-слой: тип обёртки Slint-генераций, состояние холста, sync-цикл,
//! колбэки инспектора и построитель дерева слоёв.

use crate::engine::color;
use crate::engine::expr;
use crate::engine::controller::CanvasController;
use crate::engine::model::nodes::{NodeKey, NodeKind, ShapeKind};
use crate::engine::model::scene::SceneNode;
use crate::engine::model::types::{Color, Paint, TextAlign};
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
    /// Кол-во мутаций на последнем отрисованном кадре (debug-инвариант).
    pub last_rendered_ops: u64,
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
    pub last_sel_x: String,
    pub last_sel_y: String,
    pub last_sel_w: String,
    pub last_sel_h: String,
    pub last_sel_rot: String,
    pub last_sel_opacity: String,
    pub last_sel_fill: String,
    pub last_sel_stroke: String,
    pub last_sel_stroke_width: String,
    pub last_sel_corner_tl: String,
    pub last_sel_corner_tr: String,
    pub last_sel_corner_br: String,
    pub last_sel_corner_bl: String,
    pub last_sel_corner_text: String,
    pub last_sel_corner_expanded: bool,
    pub last_layout_gap: String,
    pub last_layout_padding: String,
    pub last_font_size: String,
    pub last_text_align: i32,
    // Момент последнего ввода пользователя в каждое поле (защита от затирания).
    pub sel_edited_at: [Option<Instant>; 17],
    /// Кэш текста-плейсхолдера инспектора (мультивыделение).
    pub last_sel_hint: String,
    /// Узел, который пользователь перетаскивает в панели слоёв (drag-reorder).
    pub dragged_layer: Option<NodeKey>,
    /// Узел под ПКМ в панели слоёв (для «Rename» из контекстного меню).
    pub context_target: Option<NodeKey>,
    /// Узел, текст которого редактируется инлайн-оверлеем (для детекта старта).
    pub editing_key: Option<NodeKey>,
    /// Зажат пробел (Space+Drag = временный Pan).
    pub space_held: bool,
}

/// Индексы полей инспектора в `sel_edited_at` и диф-кэше.
#[derive(Clone, Copy, PartialEq)]
enum SelField {
    X = 0,
    Y = 1,
    W = 2,
    H = 3,
    Rotation = 4,
    Opacity = 5,
    Fill = 6,
    Stroke = 7,
    StrokeWidth = 8,
    CornerTL = 9,
    CornerTR = 10,
    CornerBR = 11,
    CornerBL = 12,
    LayoutGap = 13,
    LayoutPadding = 14,
    CornerUniform = 15,
    FontSize = 16,
}

impl CanvasState {
    pub fn new() -> Self {
        Self {
            images: [SharedPixelBuffer::new(1, 1), SharedPixelBuffer::new(1, 1)],
            cur: 0,
            w: 0,
            h: 0,
            last_revision: u64::MAX,
            last_rendered_ops: 0,
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
            last_sel_x: String::new(),
            last_sel_y: String::new(),
            last_sel_w: String::new(),
            last_sel_h: String::new(),
            last_sel_rot: String::new(),
            last_sel_opacity: String::new(),
            last_sel_fill: String::new(),
            last_sel_stroke: String::new(),
            last_sel_stroke_width: String::new(),
            last_sel_corner_tl: String::new(),
            last_sel_corner_tr: String::new(),
            last_sel_corner_br: String::new(),
            last_sel_corner_bl: String::new(),
            last_sel_corner_text: String::new(),
            last_sel_corner_expanded: false,
            last_layout_gap: String::new(),
            last_layout_padding: String::new(),
            last_font_size: String::new(),
            last_text_align: 0,
            sel_edited_at: [None; 17],
            last_sel_hint: String::new(),
            dragged_layer: None,
            context_target: None,
            editing_key: None,
            space_held: false,
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

    // Инлайн-редактор текста: оверлей в экранных координатах (слой Slint).
    // Текст в поле пишется только при старте редактирования — дальше поле
    // содержит введённое пользователем, и перезаписывать его нельзя.
    match c.editing_view() {
        Some((content, pos, size, font_size, color)) => {
            if st.editing_key != c.editing {
                st.editing_key = c.editing;
                w.set_editing_text(true);
                w.set_edit_x(pos.x);
                w.set_edit_y(pos.y);
                w.set_edit_w(size.x.max(40.0));
                w.set_edit_h(size.y.max(8.0));
                w.set_edit_font_size(font_size);
                w.set_edit_color(slint_color(color.to_rgba8()));
                w.set_edit_text(content.into());
            }
            // Живая перестройка позиции/размера (AutoWidth растёт при вводе).
            w.set_edit_x(pos.x);
            w.set_edit_y(pos.y);
            w.set_edit_w(size.x.max(40.0));
            w.set_edit_h(size.y.max(8.0));
            w.set_edit_font_size(font_size);
        }
        None => {
            if st.editing_key.take().is_some() || w.get_editing_text() {
                w.set_editing_text(false);
            }
        }
    }

    // On-demand: ничего не менялось и размер прежний — рендер не нужен.
    let size_changed = st.ensure(bw, bh);

    // Debug-инвариант: мутация без взвода dirty — утерянное обновление канваса.
    if cfg!(debug_assertions) && c.ops != st.last_rendered_ops && !c.dirty && !size_changed {
        eprintln!(
            "[invariant] ops changed ({} -> {}) but dirty=false",
            st.last_rendered_ops, c.ops
        );
    }

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
            c.marquee,
            c.hovered_frame,
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
    st.last_rendered_ops = c.ops;

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

    // Инспектор: диф по значениям каждый кадр (живые правки при драге).
    // При смене узла/выделения сбрасываем кэш и cooldown — все поля обновятся
    // принудительно.
    let sel_id = c.selected_id();
    if sel_id != st.last_sel_id {
        st.last_sel_id = sel_id;
        st.last_sel_x.clear();
        st.last_sel_y.clear();
        st.last_sel_w.clear();
        st.last_sel_h.clear();
        st.last_sel_rot.clear();
        st.last_sel_opacity.clear();
        st.last_sel_fill.clear();
        st.last_sel_stroke.clear();
        st.last_sel_stroke_width.clear();
        st.last_sel_corner_tl.clear();
        st.last_sel_corner_tr.clear();
        st.last_sel_corner_br.clear();
        st.last_sel_corner_bl.clear();
        st.sel_edited_at = [None; 17];
    }
    // Одиночный выбор — обычные поля; мультивыбор — X/Y/W/H показывают общую
    // рамку группы, остальные свойства — от первого узла.
    let sel_len = c.scene.selection().len();
    match c.selected() {
        Some(n) if sel_len >= 1 => {
            let xywh = if sel_len > 1 {
                c.selection_bbox()
                    .map(|(mn, mx)| (mn.x, mn.y, mx.x - mn.x, mx.y - mn.y))
                    .unwrap_or((0.0, 0.0, 0.0, 0.0))
            } else {
                let (dw, dh) = dims(&n.kind);
                (
                    n.local_transform.translation.x,
                    n.local_transform.translation.y,
                    dw,
                    dh,
                )
            };
            w.set_has_selection(true);
            update_sel(&w, n, &mut st, xywh);
        }
        _ => w.set_has_selection(false),
    }
    let hint = if sel_len > 1 {
        format!("{} nodes selected", sel_len)
    } else {
        String::new()
    };
    if hint != st.last_sel_hint {
        st.last_sel_hint = hint.clone();
        w.set_sel_hint(hint.into());
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

/// Commit поля (Enter): сбрасываем cooldown, чтобы следующая правка началась
/// с новой undo-точки (подтверждённое значение больше не «склеивается»).
fn mark_committed(st: &Rc<RefCell<CanvasState>>, field: SelField) {
    st.borrow_mut().sel_edited_at[field as usize] = None;
}

/// Числовой ввод из инспектора: чистое число или арифметика «как в Figma».
fn parse_num(s: &str) -> Option<f32> {
    expr::eval(s)
}

/// Разрешает значение поля размера: `50%` — процент от размера родительского
/// фрейма по нужной оси (как в Figma); без родителя процент = 0.
fn resolve_w(v: &str, c: &Controller) -> Option<f32> {
    match expr::parse(v) {
        Some(expr::Value::Percent(f)) => {
            let base = c.borrow().parent_frame_size().map(|s| s.x).unwrap_or(0.0);
            Some(f * base)
        }
        Some(expr::Value::Plain(n)) => Some(n),
        None => None,
    }
}

fn resolve_h(v: &str, c: &Controller) -> Option<f32> {
    match expr::parse(v) {
        Some(expr::Value::Percent(f)) => {
            let base = c.borrow().parent_frame_size().map(|s| s.y).unwrap_or(0.0);
            Some(f * base)
        }
        Some(expr::Value::Plain(n)) => Some(n),
        None => None,
    }
}

/// Разрешает радиус скругления: `50%` — процент от меньшей стороны фигуры.
fn resolve_corner(v: &str, c: &Controller) -> Option<f32> {
    match expr::parse(v) {
        Some(expr::Value::Percent(f)) => {
            let base = c.borrow().corner_percent_base().unwrap_or(0.0);
            Some(f * base)
        }
        Some(expr::Value::Plain(n)) => Some(n),
        None => None,
    }
}

/// Undo-точка для live-правки: на первый ввод сессии поля, а для текстовых
/// полей — дополнительно на границе слова (введён пробел). Значение после
/// вызова применяется live (без record), поэтому снапшот фиксирует состояние
/// ДО этой правки.
fn record_edit_start(c: &Controller, st: &Rc<RefCell<CanvasState>>, field: SelField, value: &str, word_boundary: bool) {
    let first = st.borrow().sel_edited_at[field as usize].is_none();
    let new_word = word_boundary && value.ends_with(' ');
    if first || new_word {
        c.borrow_mut().begin_edit();
    }
    mark_edited(st, field);
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
    window.on_live_x(move |v| {
        if let Some(x) = parse_num(&v) {
            record_edit_start(&c, &st, SelField::X, v.as_str(), false);
            if c.borrow().scene.selection().len() > 1 {
                // Мультивыбор: поле X = дельта сдвига группы (как в Figma).
                if let Some((lx, _, _, _)) = sel_bbox_now(&c) {
                    c.borrow_mut().apply_position_delta_live(x - lx, 0.0);
                }
            } else {
                let y = current_y(&c);
                c.borrow_mut().apply_position_live(x, y);
            }
            refresh_sel(&weak, &c);
        }
        rr();
    });

    let c = controller.clone();
    let weak = window.as_weak();
    let st = state.clone();
    let rr = Rc::clone(render);
    window.on_live_y(move |v| {
        if let Some(y) = parse_num(&v) {
            record_edit_start(&c, &st, SelField::Y, v.as_str(), false);
            if c.borrow().scene.selection().len() > 1 {
                if let Some((_, ty, _, _)) = sel_bbox_now(&c) {
                    c.borrow_mut().apply_position_delta_live(0.0, y - ty);
                }
            } else {
                let x = current_x(&c);
                c.borrow_mut().apply_position_live(x, y);
            }
            refresh_sel(&weak, &c);
        }
        rr();
    });

    let c = controller.clone();
    let weak = window.as_weak();
    let st = state.clone();
    let rr = Rc::clone(render);
    window.on_live_w(move |v| {
        if let Some(w) = resolve_w(&v, &c) {
            record_edit_start(&c, &st, SelField::W, v.as_str(), false);
            if c.borrow().scene.selection().len() > 1 {
                // Мультивыбор: W = масштаб группы относительно её центра.
                if let Some((_, _, _, lh)) = sel_bbox_now(&c) {
                    c.borrow_mut().apply_size_scale_live(w, lh);
                }
            } else {
                let h = current_h(&c);
                c.borrow_mut().apply_size_live(w, h);
            }
            refresh_sel(&weak, &c);
        }
        rr();
    });

    let c = controller.clone();
    let weak = window.as_weak();
    let st = state.clone();
    let rr = Rc::clone(render);
    window.on_live_h(move |v| {
        if let Some(h) = resolve_h(&v, &c) {
            record_edit_start(&c, &st, SelField::H, v.as_str(), false);
            if c.borrow().scene.selection().len() > 1 {
                if let Some((_, _, lw, _)) = sel_bbox_now(&c) {
                    c.borrow_mut().apply_size_scale_live(lw, h);
                }
            } else {
                let w = current_w(&c);
                c.borrow_mut().apply_size_live(w, h);
            }
            refresh_sel(&weak, &c);
        }
        rr();
    });

    let c = controller.clone();
    let weak = window.as_weak();
    let st = state.clone();
    let rr = Rc::clone(render);
    window.on_live_rot(move |v| {
        if let Some(deg) = parse_num(&v) {
            record_edit_start(&c, &st, SelField::Rotation, v.as_str(), false);
            c.borrow_mut().apply_rotation_live(deg);
            refresh_sel(&weak, &c);
        }
        rr();
    });

    let c = controller.clone();
    let weak = window.as_weak();
    let st = state.clone();
    let rr = Rc::clone(render);
    window.on_live_opacity(move |v| {
        if let Some(o) = parse_num(&v) {
            record_edit_start(&c, &st, SelField::Opacity, v.as_str(), false);
            c.borrow_mut().apply_opacity_live(o / 100.0);
            refresh_sel(&weak, &c);
        }
        rr();
    });

    let c = controller.clone();
    let weak = window.as_weak();
    let st = state.clone();
    let rr = Rc::clone(render);
    window.on_live_fill(move |v| {
        record_edit_start(&c, &st, SelField::Fill, v.as_str(), false);
        c.borrow_mut().apply_fill_hex_live(v.as_str());
        refresh_sel(&weak, &c);
        rr();
    });

    {
        let c = controller.clone();
        let weak = window.as_weak();
        let st = state.clone();
        let rr = Rc::clone(render);
        window.on_live_stroke(move |v| {
            record_edit_start(&c, &st, SelField::Stroke, v.as_str(), false);
            if let Some(col) = color::parse_hex(v.as_str()) {
                c.borrow_mut().apply_stroke_color_live(col);
            }
            refresh_sel(&weak, &c);
            rr();
        });
    }
    {
        let c = controller.clone();
        let weak = window.as_weak();
        let st = state.clone();
        let rr = Rc::clone(render);
        window.on_live_stroke_width(move |v| {
            if let Some(w) = parse_num(&v) {
                record_edit_start(&c, &st, SelField::StrokeWidth, v.as_str(), false);
                c.borrow_mut().apply_stroke_width_live(w);
                refresh_sel(&weak, &c);
            }
            rr();
        });
    }
    {
        let c = controller.clone();
        let weak = window.as_weak();
        let st = state.clone();
        let rr = Rc::clone(render);
        window.on_live_corner(move |i, v| {
            if let Some(r) = resolve_corner(&v, &c) {
                let field = match i {
                    0 => SelField::CornerTL,
                    1 => SelField::CornerTR,
                    2 => SelField::CornerBR,
                    _ => SelField::CornerBL,
                };
                record_edit_start(&c, &st, field, v.as_str(), false);
                c.borrow_mut().apply_corner_radius_live(i as usize, r);
                refresh_sel(&weak, &c);
            }
            rr();
        });
    }
    {
        let c = controller.clone();
        let weak = window.as_weak();
        let st = state.clone();
        let rr = Rc::clone(render);
        window.on_live_corner_uniform(move |v| {
            if let Some(r) = resolve_corner(&v, &c) {
                record_edit_start(&c, &st, SelField::CornerUniform, v.as_str(), false);
                c.borrow_mut().apply_corner_radius_uniform(r);
                refresh_sel(&weak, &c);
            }
            rr();
        });
    }
    {
        let c = controller.clone();
        let weak = window.as_weak();
        let st = state.clone();
        let rr = Rc::clone(render);
        window.on_scrub_corner_uniform(move |delta| {
            if c.borrow().scene.selection().is_empty() {
                return;
            }
            record_edit_start(&c, &st, SelField::CornerUniform, "", false);
            let cur = c.borrow().corners().unwrap_or([0.0; 4]);
            c.borrow_mut().apply_corner_radius_uniform(cur[0].max(0.0) + delta);
            refresh_sel(&weak, &c);
            rr();
        });
    }
    {
        let weak = window.as_weak();
        let st = state.clone();
        let rr = Rc::clone(render);
        window.on_toggle_corner_expand(move || {
            let mut st = st.borrow_mut();
            st.last_sel_corner_expanded = !st.last_sel_corner_expanded;
            weak.upgrade()
                .map(|w| w.set_sel_corner_expanded(st.last_sel_corner_expanded));
            rr();
        });
    }
    {
        let c = controller.clone();
        let weak = window.as_weak();
        let st = state.clone();
        let rr = Rc::clone(render);
        window.on_commit_field(move |i| {
            let field = match i {
                0 => SelField::X,
                1 => SelField::Y,
                2 => SelField::W,
                3 => SelField::H,
                4 => SelField::Rotation,
                5 => SelField::Opacity,
                8 => SelField::StrokeWidth,
                9 => SelField::CornerTL,
                10 => SelField::CornerTR,
                11 => SelField::CornerBR,
                12 => SelField::CornerBL,
                13 => SelField::LayoutGap,
                14 => SelField::LayoutPadding,
                15 => SelField::CornerUniform,
                16 => SelField::FontSize,
                _ => return,
            };
            mark_committed(&st, field);
            if !c.borrow().scene.selection().is_empty() {
                refresh_sel(&weak, &c);
            }
            rr();
        });
    }
    {
        let c = controller.clone();
        let weak = window.as_weak();
        let rr = Rc::clone(render);
        window.on_toggle_stroke_dash(move || {
            let dash = c.borrow().stroke_dashed();
            c.borrow_mut().apply_stroke_dash_live(!dash);
            refresh_sel(&weak, &c);
            rr();
        });
    }

    // --- Скраббинг полей инспектора (drag по метке NumberField) ---
    // `delta` — смещение в px * step поля; применяем относительно текущего
    // значения (как в Figma), а не абсолютной.
    {
        let c = controller.clone();
        let weak = window.as_weak();
        let st = state.clone();
        let rr = Rc::clone(render);
        window.on_scrub_delta(move |field, delta| {
            let field = field as usize;
            let sel_field = match field {
                0 => Some(SelField::X),
                1 => Some(SelField::Y),
                2 => Some(SelField::Rotation),
                3 => Some(SelField::W),
                4 => Some(SelField::H),
                5 => Some(SelField::Opacity),
                6 => Some(SelField::StrokeWidth),
                7 => Some(SelField::CornerTL),
                8 => Some(SelField::CornerTR),
                9 => Some(SelField::CornerBR),
                _ => Some(SelField::CornerBL),
            };
            let Some(sf) = sel_field else { return };
            if c.borrow().scene.selection().is_empty() {
                return;
            }
            record_edit_start(&c, &st, sf, "", false);
            let multi = c.borrow().scene.selection().len() > 1;
            match field {
                0 => {
                    if multi {
                        c.borrow_mut().apply_position_delta_live(delta, 0.0);
                    } else {
                        let y = current_y(&c);
                        c.borrow_mut().apply_position_live(current_x(&c) + delta, y);
                    }
                }
                1 => {
                    if multi {
                        c.borrow_mut().apply_position_delta_live(0.0, delta);
                    } else {
                        let x = current_x(&c);
                        c.borrow_mut().apply_position_live(x, current_y(&c) + delta);
                    }
                }
                2 => {
                    let deg = c.borrow().rotation_degrees() + delta;
                    c.borrow_mut().apply_rotation_live(deg);
                }
                3 => {
                    if let Some((_, _, lw, lh)) = sel_bbox_now(&c) {
                        if multi {
                            c.borrow_mut().apply_size_scale_live(lw + delta, lh);
                        } else {
                            c.borrow_mut().apply_size_live(current_w(&c) + delta, lh);
                        }
                    }
                }
                4 => {
                    if let Some((_, _, lw, lh)) = sel_bbox_now(&c) {
                        if multi {
                            c.borrow_mut().apply_size_scale_live(lw, lh + delta);
                        } else {
                            c.borrow_mut().apply_size_live(lw, current_h(&c) + delta);
                        }
                    }
                }
                5 => {
                    let o = c.borrow().selected().map(|n| n.opacity).unwrap_or(0.0) * 100.0;
                    c.borrow_mut().apply_opacity_live((o + delta).clamp(0.0, 100.0) / 100.0);
                }
                6 => {
                    let w = c.borrow().stroke_width() + delta;
                    c.borrow_mut().apply_stroke_width_live(w);
                }
                _ => {
                    let idx = field - 7;
                    let cur = c.borrow().corners().unwrap_or([0.0; 4]);
                    c.borrow_mut().apply_corner_radius_live(idx, cur[idx.min(3)] + delta);
                }
            }
            refresh_sel(&weak, &c);
            rr();
        });
    }

    // --- Auto layout (Frame): режим, отступы ---
    {
        let c = controller.clone();
        let weak = window.as_weak();
        let rr = Rc::clone(render);
        window.on_set_layout_mode(move |mode| {
            let cur = c.borrow().frame_auto_layout().map(|_| 1).unwrap_or(0);
            if mode != cur {
                c.borrow_mut().set_auto_layout_mode(mode as u8);
            }
            refresh_sel(&weak, &c);
            rr();
        });
        let c = controller.clone();
        let weak = window.as_weak();
        let rr = Rc::clone(render);
        window.on_set_layout_direction(move |d| {
            let cur = c
                .borrow()
                .frame_auto_layout()
                .map(|cfg| match cfg.direction {
                    crate::engine::model::types::LayoutDirection::Horizontal => 0,
                    crate::engine::model::types::LayoutDirection::Vertical => 1,
                })
                .unwrap_or(0);
            if d != cur {
                c.borrow_mut().set_auto_layout_direction(d as u8);
            }
            refresh_sel(&weak, &c);
            rr();
        });
        let c = controller.clone();
        let weak = window.as_weak();
        let rr = Rc::clone(render);
        window.on_set_layout_align(move |a| {
            let cur = c
                .borrow()
                .frame_auto_layout()
                .map(|cfg| match cfg.align_items {
                    crate::engine::model::types::LayoutAlign::Stretch => 0,
                    crate::engine::model::types::LayoutAlign::Min => 1,
                    crate::engine::model::types::LayoutAlign::Center => 2,
                    crate::engine::model::types::LayoutAlign::Max => 3,
                })
                .unwrap_or(0);
            if a != cur {
                c.borrow_mut().apply_auto_layout_align(a as u8);
            }
            refresh_sel(&weak, &c);
            rr();
        });
        let c = controller.clone();
        let weak = window.as_weak();
        let rr = Rc::clone(render);
        window.on_set_layout_justify(move |j| {
            let cur = c
                .borrow()
                .frame_auto_layout()
                .map(|cfg| match cfg.justify_content {
                    crate::engine::model::types::LayoutJustify::Min => 0,
                    crate::engine::model::types::LayoutJustify::Center => 1,
                    crate::engine::model::types::LayoutJustify::Max => 2,
                    crate::engine::model::types::LayoutJustify::SpaceBetween => 3,
                })
                .unwrap_or(0);
            if j != cur {
                c.borrow_mut().apply_auto_layout_justify(j as u8);
            }
            refresh_sel(&weak, &c);
            rr();
        });
        let c = controller.clone();
        let weak = window.as_weak();
        let st = state.clone();
        let rr = Rc::clone(render);
        window.on_live_layout_gap(move |v| {
            if let Some(g) = parse_num(&v) {
                record_edit_start(&c, &st, SelField::LayoutGap, v.as_str(), false);
                c.borrow_mut().apply_auto_layout_spacing_live(g);
                refresh_sel(&weak, &c);
            }
            rr();
        });
        let c = controller.clone();
        let weak = window.as_weak();
        let st = state.clone();
        let rr = Rc::clone(render);
        window.on_live_layout_padding(move |v| {
            if let Some(p) = parse_num(&v) {
                record_edit_start(&c, &st, SelField::LayoutPadding, v.as_str(), false);
                c.borrow_mut().apply_auto_layout_padding_live(p);
                refresh_sel(&weak, &c);
            }
            rr();
        });
        // --- Текст: размер шрифта и выравнивание ---
        let c = controller.clone();
        let weak = window.as_weak();
        let st = state.clone();
        let rr = Rc::clone(render);
        window.on_live_font_size(move |v| {
            if let Some(fs) = parse_num(&v) {
                record_edit_start(&c, &st, SelField::FontSize, v.as_str(), false);
                c.borrow_mut().apply_font_size_live(fs);
                refresh_sel(&weak, &c);
            }
            rr();
        });
        let c = controller.clone();
        let rr = Rc::clone(render);
        window.on_set_text_align(move |a| {
            let align = match a {
                1 => TextAlign::Center,
                2 => TextAlign::Right,
                3 => TextAlign::Justified,
                _ => TextAlign::Left,
            };
            c.borrow_mut().set_text_align(align);
            rr();
        });
        let c = controller.clone();
        let weak = window.as_weak();
        let st = state.clone();
        let rr = Rc::clone(render);
        window.on_scrub_layout_gap(move |d| {
            let cur = c.borrow().frame_auto_layout().map(|cfg| cfg.spacing).unwrap_or(0.0);
            record_edit_start(&c, &st, SelField::LayoutGap, "", false);
            c.borrow_mut().apply_auto_layout_spacing_live(cur + d);
            refresh_sel(&weak, &c);
            rr();
        });
        let c = controller.clone();
        let weak = window.as_weak();
        let st = state.clone();
        let rr = Rc::clone(render);
        window.on_scrub_layout_padding(move |d| {
            let cur = c.borrow().frame_auto_layout().map(|cfg| cfg.padding[0]).unwrap_or(0.0);
            record_edit_start(&c, &st, SelField::LayoutPadding, "", false);
            c.borrow_mut().apply_auto_layout_padding_live(cur + d);
            refresh_sel(&weak, &c);
            rr();
        });
    }

    // --- Полный color picker ---
    // Состояние пикера — заливка выделенного узла: каждый ввод применяется
    // к модели (live), затем компоненты цвета пересчитываются и уходят в UI.
    {
        let c = controller.clone();
        let weak = window.as_weak();
        window.on_open_picker(move || {
            if let Some(w) = weak.upgrade() {
                if c.borrow().selected().is_some() {
                    w.set_picker_stroke(false);
                    open_picker(&w, &c);
                }
            }
        });
        let c = controller.clone();
        let weak = window.as_weak();
        window.on_open_stroke_picker(move || {
            if let Some(w) = weak.upgrade() {
                if c.borrow().selected().is_some() {
                    w.set_picker_stroke(true);
                    open_picker(&w, &c);
                }
            }
        });
    }
    {
        let c = controller.clone();
        let weak = window.as_weak();
        let rr = Rc::clone(render);
        window.on_picker_hue(move |h| {
            if let Some(w) = weak.upgrade() {
                let deg = h * 360.0;
                let cur = current_picker_color(&c, w.get_picker_stroke());
                let (_, s, v) = rgb_to_hsv8(cur);
                let [r, g, b, a] = hsv_rgb(deg, s, v, cur);
                let col = Color::from_rgba8(r, g, b, a);
                apply_picker_color(&c, &w, col);
                push_picker_color(&w, &c);
                rr();
            }
        });
    }
    {
        let c = controller.clone();
        let weak = window.as_weak();
        let rr = Rc::clone(render);
        window.on_picker_sv(move |s, v| {
            if let Some(w) = weak.upgrade() {
                let cur = current_picker_color(&c, w.get_picker_stroke());
                let (h, _, _) = rgb_to_hsv8(cur);
                let [r, g, b, a] = hsv_rgb(h, s, v, cur);
                let col = Color::from_rgba8(r, g, b, a);
                apply_picker_color(&c, &w, col);
                push_picker_color(&w, &c);
                rr();
            }
        });
    }
    {
        let c = controller.clone();
        let weak = window.as_weak();
        let rr = Rc::clone(render);
        window.on_picker_alpha(move |a| {
            if let Some(w) = weak.upgrade() {
                let cur = current_picker_color(&c, w.get_picker_stroke());
                let col = Color::rgba(cur.r, cur.g, cur.b, a);
                apply_picker_color(&c, &w, col);
                push_picker_color(&w, &c);
                rr();
            }
        });
    }
    {
        let c = controller.clone();
        let weak = window.as_weak();
        let rr = Rc::clone(render);
        window.on_picker_hex_commit(move |t| {
            if let Some(w) = weak.upgrade() {
                if let Some(col) = color::parse_hex(t.as_str()) {
                    apply_picker_color(&c, &w, col);
                    push_picker_color(&w, &c);
                    rr();
                }
            }
        });
    }
    {
        let c = controller.clone();
        let weak = window.as_weak();
        let rr = Rc::clone(render);
        window.on_picker_rgba(move |field, t| {
            if let Some(w) = weak.upgrade() {
                let cur = current_picker_color(&c, w.get_picker_stroke());
                let [r, g, b, a] = cur.to_rgba8();
                let mut col = Color::from_rgba8(r, g, b, a);
                if let Ok(v) = t.trim().parse::<u8>() {
                    match field {
                        0 => col.r = v as f32 / 255.0,
                        1 => col.g = v as f32 / 255.0,
                        2 => col.b = v as f32 / 255.0,
                        _ => col.a = v as f32 / 255.0,
                    }
                    apply_picker_color(&c, &w, col);
                    push_picker_color(&w, &c);
                    rr();
                }
            }
        });
    }
    {
        let c = controller.clone();
        let weak = window.as_weak();
        let rr = Rc::clone(render);
        window.on_picker_hsl(move |field, t| {
            if let Some(w) = weak.upgrade() {
                let cur = current_picker_color(&c, w.get_picker_stroke());
                let (ch, cs, cl) = rgb_to_hsl8(cur);
                let parsed = expr::eval(&t);
                let (deg, s, l) = match (field, parsed) {
                    (0, Some(h)) => (h.clamp(0.0, 360.0), cs, cl),
                    (1, Some(sv)) => (ch, sv.clamp(0.0, 100.0) / 100.0, cl),
                    (_, Some(lv)) => (ch, cs, lv.clamp(0.0, 100.0) / 100.0),
                    _ => (ch, cs, cl),
                };
                let (r, g, b) = color::hsl_to_rgb(deg, s, l);
                let col = Color::rgba(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, cur.a);
                apply_picker_color(&c, &w, col);
                push_picker_color(&w, &c);
                rr();
            }
        });
    }
    {
        let c = controller.clone();
        let weak = window.as_weak();
        let rr = Rc::clone(render);
        window.on_picker_swatch(move |i| {
            if let Some(w) = weak.upgrade() {
                if let Some(col) = SWATCHES.get(i as usize) {
                    let col = *col;
                    apply_picker_color(&c, &w, col);
                    push_picker_color(&w, &c);
                    rr();
                }
            }
        });
    }
    {
        let weak = window.as_weak();
        let weak2 = window.as_weak();
        window.on_picker_done(move || {
            if let Some(w) = weak.upgrade() {
                w.set_picker_open(false);
            }
        });
        window.on_picker_cancel(move || {
            if let Some(w) = weak2.upgrade() {
                w.set_picker_open(false);
            }
        });
    }
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

/// RGBA8 -> цвет Slint (порядок каналов в from_argb_u8: alpha, r, g, b).
fn slint_color(c: [u8; 4]) -> slint::Color {
    slint::Color::from_argb_u8(c[3], c[0], c[1], c[2])
}

fn slint_transparent() -> slint::Color {
    slint::Color::from_argb_u8(0, 0, 0, 0)
}

/// Палитра свачей color picker (первый — прозрачный).
const SWATCHES: [Color; 13] = [
    Color::rgba(0.0, 0.0, 0.0, 0.0),
    Color::rgb(1.0, 1.0, 1.0),
    Color::rgb(0.0, 0.0, 0.0),
    Color::rgb(0.23, 0.44, 0.99),
    Color::rgb(0.0, 0.56, 0.76),
    Color::rgb(0.0, 0.68, 0.5),
    Color::rgb(0.56, 0.77, 0.2),
    Color::rgb(1.0, 0.82, 0.2),
    Color::rgb(1.0, 0.58, 0.18),
    Color::rgb(1.0, 0.32, 0.24),
    Color::rgb(0.89, 0.24, 0.53),
    Color::rgb(0.58, 0.34, 0.78),
    Color::rgb(0.38, 0.38, 0.42),
];

/// Текущая сплошная заливка выделенного узла (или чёрная).
fn current_fill_color(c: &Controller) -> Color {
    c.borrow()
        .selected()
        .and_then(|n| {
            n.fills.iter().find_map(|p| match p {
                Paint::Solid(col) => Some(*col),
                _ => None,
            })
        })
        .unwrap_or(Color::rgb(0.0, 0.0, 0.0))
}

/// Текущий цвет цели пикера: заливка (stroke == false) или обводка.
fn current_picker_color(c: &Controller, stroke: bool) -> Color {
    if stroke {
        c.borrow().stroke_color().unwrap_or(Color::rgb(0.0, 0.0, 0.0))
    } else {
        current_fill_color(c)
    }
}

/// Применяет цвет к цели пикера (заливка или обводка — по picker-stroke окна).
fn apply_picker_color(c: &Controller, w: &AppWindow, col: Color) {
    if w.get_picker_stroke() {
        c.borrow_mut().apply_stroke_color_live(col);
    } else {
        c.borrow_mut().apply_fill_color_live(col);
    }
}

fn rgb_to_hsv8(c: Color) -> (f32, f32, f32) {
    let [r, g, b, _] = c.to_rgba8();
    color::rgb_to_hsv(r, g, b)
}

fn rgb_to_hsl8(c: Color) -> (f32, f32, f32) {
    let [r, g, b, _] = c.to_rgba8();
    color::rgb_to_hsl(r, g, b)
}

/// HSV в RGBA8, сохраняя альфу исходного цвета.
fn hsv_rgb(h: f32, s: f32, v: f32, c: Color) -> [u8; 4] {
    let (r, g, b) = color::hsv_to_rgb(h, s, v);
    let a = c.to_rgba8()[3];
    [r, g, b, a]
}

/// Открывает color picker на текущий цвет цели (заливка/обводка) выделения.
fn open_picker(w: &AppWindow, c: &Controller) {
    if c.borrow().selected().is_none() {
        return;
    }
    let sw: Vec<slint::Color> = SWATCHES
        .iter()
        .map(|col| {
            let [r, g, b, a] = col.to_rgba8();
            slint::Color::from_argb_u8(a, r, g, b)
        })
        .collect();
    let model: ModelRc<slint::Color> = Rc::new(VecModel::from(sw)).into();
    w.set_picker_swatches(model);
    w.set_picker_open(true);
    push_picker_color(w, c);
}

/// Рассылает текущий цвет цели пикера в props color picker'а.
fn push_picker_color(w: &AppWindow, c: &Controller) {
    let cur = current_picker_color(c, w.get_picker_stroke());    let [r, g, b, a] = cur.to_rgba8();
    let (h, s, v) = color::rgb_to_hsv(r, g, b);
    let (hc_r, hc_g, hc_b) = color::hsv_to_rgb(h, 1.0, 1.0);
    w.set_picker_r(r as i32);
    w.set_picker_g(g as i32);
    w.set_picker_b(b as i32);
    w.set_picker_a(a as i32);
    w.set_picker_hex(color::to_hex(cur).into());
    w.set_picker_h(h);
    w.set_picker_s(s);
    w.set_picker_v(v);
    w.set_picker_hue_color(slint::Color::from_rgb_u8(hc_r, hc_g, hc_b));
    w.set_picker_solid(slint::Color::from_rgb_u8(r, g, b));
    w.set_picker_preview(slint::Color::from_argb_u8(a, r, g, b));
}

fn dims(kind: &NodeKind) -> (f32, f32) {
    match kind {
        NodeKind::Frame { size, .. } => (size.x, size.y),
        NodeKind::Shape(ShapeKind::Rectangle { size, .. }) => (size.x, size.y),
        NodeKind::Shape(ShapeKind::Ellipse { radii, .. }) => (radii.x, radii.y),
        NodeKind::Text { .. } => {
            let s = crate::engine::text::measure(kind);
            (s.x, s.y)
        }
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
/// `xywh` — отображаемые X/Y/W/H (для мультивыбора — общая рамка группы).
fn update_sel(w: &AppWindow, n: &SceneNode, st: &mut CanvasState, xywh: (f32, f32, f32, f32)) {
    let now = Instant::now();
    upd_field(w, &mut st.last_sel_x, &mut st.sel_edited_at[SelField::X as usize], now, fmt_num(xywh.0), AppWindow::set_sel_x_text);
    upd_field(w, &mut st.last_sel_y, &mut st.sel_edited_at[SelField::Y as usize], now, fmt_num(xywh.1), AppWindow::set_sel_y_text);
    upd_field(w, &mut st.last_sel_w, &mut st.sel_edited_at[SelField::W as usize], now, fmt_num(xywh.2), AppWindow::set_sel_w_text);
    upd_field(w, &mut st.last_sel_h, &mut st.sel_edited_at[SelField::H as usize], now, fmt_num(xywh.3), AppWindow::set_sel_h_text);
    let rot = n.local_transform.matrix2.x_axis.y.atan2(n.local_transform.matrix2.x_axis.x).to_degrees();
    upd_field(w, &mut st.last_sel_rot, &mut st.sel_edited_at[SelField::Rotation as usize], now, fmt_rot(rot), AppWindow::set_sel_rot_text);
    upd_field(w, &mut st.last_sel_opacity, &mut st.sel_edited_at[SelField::Opacity as usize], now, fmt_pct(n.opacity), AppWindow::set_sel_opacity_text);
    upd_field(w, &mut st.last_sel_fill, &mut st.sel_edited_at[SelField::Fill as usize], now, sel_fill_hex(n), AppWindow::set_sel_fill);
    w.set_sel_fill_preview(sel_fill_color(n).map(|c| slint_color(c)).unwrap_or(slint_transparent()));
    upd_field(w, &mut st.last_sel_stroke, &mut st.sel_edited_at[SelField::Stroke as usize], now, sel_stroke_hex(n), AppWindow::set_sel_stroke);
    w.set_sel_stroke_preview(sel_stroke_color(n).map(|c| slint_color(c)).unwrap_or(slint_transparent()));
    w.set_sel_stroke_dash(sel_stroke_dashed(n));
    let sw = n.strokes.first().map(|s| s.width).unwrap_or(0.0);
    upd_field(w, &mut st.last_sel_stroke_width, &mut st.sel_edited_at[SelField::StrokeWidth as usize], now, fmt_num(sw), AppWindow::set_sel_stroke_width_text);
    if let Some(r) = sel_corners(n) {
        upd_field(w, &mut st.last_sel_corner_tl, &mut st.sel_edited_at[SelField::CornerTL as usize], now, fmt_num(r[0]), AppWindow::set_sel_corner_tl_text);
        upd_field(w, &mut st.last_sel_corner_tr, &mut st.sel_edited_at[SelField::CornerTR as usize], now, fmt_num(r[1]), AppWindow::set_sel_corner_tr_text);
        upd_field(w, &mut st.last_sel_corner_br, &mut st.sel_edited_at[SelField::CornerBR as usize], now, fmt_num(r[2]), AppWindow::set_sel_corner_br_text);
        upd_field(w, &mut st.last_sel_corner_bl, &mut st.sel_edited_at[SelField::CornerBL as usize], now, fmt_num(r[3]), AppWindow::set_sel_corner_bl_text);
        // Свёрнутое единое поле: значение = TL (как в Figma, при разных углах
        // Figma показывает среднее/—; показываем TL).
        let uniform = if r[0] == r[1] && r[1] == r[2] && r[2] == r[3] {
            fmt_num(r[0])
        } else {
            "—".into()
        };
        upd_field(w, &mut st.last_sel_corner_text, &mut st.sel_edited_at[SelField::CornerUniform as usize], now, uniform, AppWindow::set_sel_corner_text);
    } else {
        // Виды без скругления: показываем «—».
        upd_field(w, &mut st.last_sel_corner_tl, &mut st.sel_edited_at[SelField::CornerTL as usize], now, "—".into(), AppWindow::set_sel_corner_tl_text);
        upd_field(w, &mut st.last_sel_corner_tr, &mut st.sel_edited_at[SelField::CornerTR as usize], now, "—".into(), AppWindow::set_sel_corner_tr_text);
        upd_field(w, &mut st.last_sel_corner_br, &mut st.sel_edited_at[SelField::CornerBR as usize], now, "—".into(), AppWindow::set_sel_corner_br_text);
        upd_field(w, &mut st.last_sel_corner_bl, &mut st.sel_edited_at[SelField::CornerBL as usize], now, "—".into(), AppWindow::set_sel_corner_bl_text);
        upd_field(w, &mut st.last_sel_corner_text, &mut st.sel_edited_at[SelField::CornerUniform as usize], now, "—".into(), AppWindow::set_sel_corner_text);
    }
    w.set_sel_corner_expanded(st.last_sel_corner_expanded);
    // Auto layout (Frame).
    w.set_is_frame(matches!(n.kind, NodeKind::Frame { .. }));
    let (mode, dir, align, justify, gap, pad) = match &n.kind {
        NodeKind::Frame { auto_layout: Some(cfg), .. } => {
            let mode = 1;
            let dir = match cfg.direction {
                crate::engine::model::types::LayoutDirection::Horizontal => 0,
                crate::engine::model::types::LayoutDirection::Vertical => 1,
            };
            let align = match cfg.align_items {
                crate::engine::model::types::LayoutAlign::Stretch => 0,
                crate::engine::model::types::LayoutAlign::Min => 1,
                crate::engine::model::types::LayoutAlign::Center => 2,
                crate::engine::model::types::LayoutAlign::Max => 3,
            };
            let justify = match cfg.justify_content {
                crate::engine::model::types::LayoutJustify::Min => 0,
                crate::engine::model::types::LayoutJustify::Center => 1,
                crate::engine::model::types::LayoutJustify::Max => 2,
                crate::engine::model::types::LayoutJustify::SpaceBetween => 3,
            };
            (mode, dir, align, justify, fmt_num(cfg.spacing), fmt_num(cfg.padding[0]))
        }
        _ => (0, 0, 0, 0, fmt_num(0.0), fmt_num(0.0)),
    };
    w.set_layout_mode(mode);
    w.set_sel_layout_direction(dir);
    w.set_sel_layout_align(align);
    w.set_sel_layout_justify(justify);
    upd_field(w, &mut st.last_layout_gap, &mut st.sel_edited_at[SelField::LayoutGap as usize], now, gap, AppWindow::set_sel_layout_gap);
    upd_field(w, &mut st.last_layout_padding, &mut st.sel_edited_at[SelField::LayoutPadding as usize], now, pad, AppWindow::set_sel_layout_padding);
    // Текст (Text-узел).
    match &n.kind {
        NodeKind::Text { font_size, align, .. } => {
            w.set_is_text(true);
            upd_field(
                w,
                &mut st.last_font_size,
                &mut st.sel_edited_at[SelField::FontSize as usize],
                now,
                fmt_num(*font_size),
                AppWindow::set_sel_font_size_text,
            );
            let a = match align {
                TextAlign::Left => 0,
                TextAlign::Center => 1,
                TextAlign::Right => 2,
                TextAlign::Justified => 3,
            };
            if a != st.last_text_align {
                st.last_text_align = a;
                w.set_sel_align(a);
            }
        }
        _ => {
            w.set_is_text(false);
            w.set_sel_align(0);
            st.last_text_align = 0;
        }
    }
}

/// Заполняет поля инспектора из узла (после commit; `xywh` — отображаемые
/// X/Y/W/H, для мультивыбора — общая рамка группы).
fn set_sel_texts(w: &AppWindow, n: &SceneNode, xywh: (f32, f32, f32, f32)) {
    w.set_sel_x_text(fmt_num(xywh.0).into());
    w.set_sel_y_text(fmt_num(xywh.1).into());
    w.set_sel_w_text(fmt_num(xywh.2).into());
    w.set_sel_h_text(fmt_num(xywh.3).into());
    let rot = n.local_transform.matrix2.x_axis.y.atan2(n.local_transform.matrix2.x_axis.x).to_degrees();
    w.set_sel_rot_text(fmt_rot(rot).into());
    w.set_sel_opacity_text(fmt_pct(n.opacity).into());
    w.set_sel_fill(sel_fill_hex(n).into());
    w.set_sel_fill_preview(sel_fill_color(n).map(|c| slint_color(c)).unwrap_or(slint_transparent()));
    w.set_sel_stroke(sel_stroke_hex(n).into());
    w.set_sel_stroke_preview(sel_stroke_color(n).map(|c| slint_color(c)).unwrap_or(slint_transparent()));
    w.set_sel_stroke_dash(sel_stroke_dashed(n));
    let sw = n.strokes.first().map(|s| s.width).unwrap_or(0.0);
    w.set_sel_stroke_width_text(fmt_num(sw).into());
    if let Some(r) = sel_corners(n) {
        w.set_sel_corner_tl_text(fmt_num(r[0]).into());
        w.set_sel_corner_tr_text(fmt_num(r[1]).into());
        w.set_sel_corner_br_text(fmt_num(r[2]).into());
        w.set_sel_corner_bl_text(fmt_num(r[3]).into());
    } else {
        w.set_sel_corner_tl_text("—".into());
        w.set_sel_corner_tr_text("—".into());
        w.set_sel_corner_br_text("—".into());
        w.set_sel_corner_bl_text("—".into());
    }
    match &n.kind {
        NodeKind::Text { font_size, align, .. } => {
            w.set_is_text(true);
            w.set_sel_font_size_text(fmt_num(*font_size).into());
            let a = match align {
                TextAlign::Left => 0,
                TextAlign::Center => 1,
                TextAlign::Right => 2,
                TextAlign::Justified => 3,
            };
            w.set_sel_align(a);
        }
        _ => {
            w.set_is_text(false);
            w.set_sel_align(0);
        }
    }
}

/// Радиусы скругления узла ([tl,tr,br,bl]), если вид поддерживает скругление.
fn sel_corners(n: &SceneNode) -> Option<[f32; 4]> {
    match &n.kind {
        NodeKind::Frame { corner_radii, .. }
        | NodeKind::Shape(ShapeKind::Rectangle { corner_radii, .. }) => Some(*corner_radii),
        _ => None,
    }
}

/// Текущие X/Y/W/H для показа в инспекторе (мультивыбор — общая рамка).
/// Вызывает flush_transforms, чтобы рамки были актуальны.
fn sel_bbox_now(c: &Controller) -> Option<(f32, f32, f32, f32)> {
    c.borrow_mut().scene.flush_transforms();
    c.borrow()
        .selection_bbox()
        .map(|(mn, mx)| (mn.x, mn.y, mx.x - mn.x, mx.y - mn.y))
}

/// Градусы поворота (целые).
fn fmt_rot(deg: f32) -> String {
    format!("{}", deg.round() as i64)
}

/// Первая сплошная заливка узла как RGBA8 (для превью).
fn sel_fill_color(n: &SceneNode) -> Option<[u8; 4]> {
    n.fills.iter().find_map(|p| match p {
        Paint::Solid(c) => Some(c.to_rgba8()),
        _ => None,
    })
}

/// HEX-строка первой сплошной заливки узла ("#RRGGBBAA" — цвет и его альфа).
fn sel_fill_hex(n: &SceneNode) -> String {
    n.fills
        .iter()
        .find_map(|p| match p {
            Paint::Solid(c) => Some(c.to_rgba8()),
            _ => None,
        })
        .map(|c| format!("#{:02X}{:02X}{:02X}{:02X}", c[0], c[1], c[2], c[3]))
        .unwrap_or_else(|| "#00000000".into())
}

/// Первая сплошная обводка узла как RGBA8 (для превью).
fn sel_stroke_color(n: &SceneNode) -> Option<[u8; 4]> {
    n.strokes.first().and_then(|st| match &st.paint {
        Paint::Solid(c) => Some(c.to_rgba8()),
        _ => None,
    })
}

/// HEX-строка первой сплошной обводки узла ("#RRGGBB").
fn sel_stroke_hex(n: &SceneNode) -> String {
    sel_stroke_color(n)
        .map(|c| format!("#{:02X}{:02X}{:02X}", c[0], c[1], c[2]))
        .unwrap_or_else(|| "#000000".into())
}

/// Пунктирная ли обводка выделенного узла.
fn sel_stroke_dashed(n: &SceneNode) -> bool {
    n.strokes.first().map(|st| !st.dash_pattern.is_empty()).unwrap_or(false)
}

/// Обновляет поля после commit (значения нормализованы контроллером).
fn refresh_sel(win: &slint::Weak<AppWindow>, c: &Controller) {
    if let Some(w) = win.upgrade() {
        if let Some(n) = c.borrow().selected() {
            let sel_len = c.borrow().scene.selection().len();
            let xywh = if sel_len > 1 {
                sel_bbox_now(c).unwrap_or((0.0, 0.0, 0.0, 0.0))
            } else {
                let (dw, dh) = dims(&n.kind);
                (
                    n.local_transform.translation.x,
                    n.local_transform.translation.y,
                    dw,
                    dh,
                )
            };
            set_sel_texts(&w, n, xywh);
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
                selected: c.scene.selection().contains(&key),
            });
            stack.extend(node.children.iter().rev().map(|&ch| (ch, depth + 1)));
        }
    }
    out
}