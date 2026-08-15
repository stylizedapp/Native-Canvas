mod engine;

use engine::controller::{CanvasController, Tool};
use engine::renderer::{Renderer, TinySkiaRenderer};
use engine::scene::{NodeKind, SceneNode};
use slint::{Image, ModelRc, Rgba8Pixel, SharedPixelBuffer, VecModel};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

slint::include_modules!();

type Controller = Rc<RefCell<CanvasController>>;
type RendererRef = Rc<RefCell<TinySkiaRenderer>>;

/// Максимальный размер длинной стороны буфера рендера (кэп разрешения).
/// Снижает нагрузку CPU-растеризации на больших окнах; ввод остаётся точным
/// (координаты логические, растеризация масштабируется отдельно).
const MAX_RENDER_DIM: f32 = 1920.0;

/// Переиспользуемые буферы холста, статистика и кэш UI-строк (чтобы не
/// переустанавливать неизменные свойства Slint на каждом кадре).
struct CanvasState {
    image: SharedPixelBuffer<Rgba8Pixel>,
    w: u32,
    h: u32,
    last_revision: u64,
    // FPS
    fps_frames: u64,
    fps_window: Instant,
    fps: f64,
    frame_count: u64,
    last_render_us: u128,
    // Кэш UI (установка только при изменении).
    last_sel_id: Option<u64>,
    last_zoom_text: String,
    last_fps_text: String,
    last_tool: String,
}

impl CanvasState {
    fn new() -> Self {
        Self {
            image: SharedPixelBuffer::new(1, 1),
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
        }
    }

    /// Обновляет размер буфера. Возвращает true, если размер изменился.
    fn ensure(&mut self, w: u32, h: u32) -> bool {
        let w = w.max(1);
        let h = h.max(1);
        if w != self.w || h != self.h {
            self.w = w;
            self.h = h;
            self.image = SharedPixelBuffer::new(w, h);
            true
        } else {
            false
        }
    }
}

fn main() -> Result<(), slint::PlatformError> {
    let window = AppWindow::new()?;
    let controller: Controller = Rc::new(RefCell::new(CanvasController::new()));
    let renderer: RendererRef = Rc::new(RefCell::new(TinySkiaRenderer));
    let state = Rc::new(RefCell::new(CanvasState::new()));

    // --- Цикл рендера: coalescing через таймер ~60 FPS ---
    let _render_timer = {
        let window = window.as_weak();
        let controller = controller.clone();
        let renderer = renderer.clone();
        let state = state.clone();
        let timer = slint::Timer::default();
        timer.start(slint::TimerMode::Repeated, Duration::from_millis(16), move || {
            sync(&window, &controller, &renderer, &state);
        });
        timer
    };

    // --- Колбэки инструментов / файлов ---
    {
        let controller = controller.clone();
        window.on_tool_changed(move |name| {
            controller.borrow_mut().set_tool(Tool::from_name(name.as_str()));
        });
    }
    {
        let controller = controller.clone();
        window.on_undo(move || { controller.borrow_mut().undo(); });
    }
    {
        let controller = controller.clone();
        window.on_redo(move || { controller.borrow_mut().redo(); });
    }
    {
        let controller = controller.clone();
        window.on_delete_selection(move || { controller.borrow_mut().delete_selection(); });
    }
    {
        let controller = controller.clone();
        window.on_new_doc(move || { controller.borrow_mut().clear(); });
    }
    {
        let weak = window.as_weak();
        let controller = controller.clone();
        window.on_save_doc(move || {
            if let Some(path) = rfd::FileDialog::new().set_file_name("document.json").save_file() {
                let result = controller.borrow().save();
                match result {
                    Ok(data) => {
                        let _ = std::fs::write(&path, data);
                        if let Some(w) = weak.upgrade() {
                            w.set_status_text(format!("Saved: {}", path.display()).into());
                        }
                    }
                    Err(e) => {
                        if let Some(w) = weak.upgrade() {
                            w.set_status_text(format!("Save error: {}", e).into());
                        }
                    }
                }
            }
        });
    }
    {
        let weak = window.as_weak();
        let controller = controller.clone();
        window.on_open_doc(move || {
            if let Some(path) = rfd::FileDialog::new().pick_file() {
                if let Ok(data) = std::fs::read_to_string(&path) {
                    if let Some(w) = weak.upgrade() {
                        let result = controller.borrow_mut().load(&data);
                        match result {
                            Ok(()) => w.set_status_text(format!("Opened: {}", path.display()).into()),
                            Err(e) => w.set_status_text(format!("Open error: {}", e).into()),
                        }
                    }
                }
            }
        });
    }

    // --- Колбэки холста (ЭКРАННЫЕ координаты — конверсию делает контроллер) ---
    {
        let controller = controller.clone();
        window.on_pointer_down(move |button, x, y| {
            controller.borrow_mut().pointer_down(glam::Vec2::new(x, y), button as u8);
        });
    }
    {
        let controller = controller.clone();
        window.on_pointer_move(move |_button, x, y| {
            controller.borrow_mut().pointer_move(glam::Vec2::new(x, y));
        });
    }
    {
        let controller = controller.clone();
        window.on_pointer_up(move |_button, x, y| {
            controller.borrow_mut().pointer_up(glam::Vec2::new(x, y));
        });
    }
    {
        let controller = controller.clone();
        window.on_scroll(move |delta, x, y| {
            controller.borrow_mut().zoom(delta, glam::Vec2::new(x, y));
        });
    }
    {
        let controller = controller.clone();
        window.on_select_layer(move |id| {
            controller.borrow_mut().select(id as u64);
        });
    }

    // --- Сетка / снап ---
    {
        let weak = window.as_weak();
        let controller = controller.clone();
        window.on_toggle_grid(move || {
            let on = {
                let mut c = controller.borrow_mut();
                c.toggle_grid();
                c.grid_visible
            };
            if let Some(w) = weak.upgrade() {
                w.set_grid_on(on);
            }
        });
    }
    {
        let weak = window.as_weak();
        let controller = controller.clone();
        window.on_toggle_snap(move || {
            let on = {
                let mut c = controller.borrow_mut();
                c.toggle_snap();
                c.snap_enabled
            };
            if let Some(w) = weak.upgrade() {
                w.set_snap_on(on);
            }
        });
    }

    // --- Дебагер ---
    {
        let weak = window.as_weak();
        window.on_toggle_debug(move || {
            if let Some(w) = weak.upgrade() {
                w.set_debug_show(!w.get_debug_show());
            }
        });
    }

    // --- Инспектор ---
    register_inspector_callbacks(&window, &controller);

    window.run()
}

fn register_inspector_callbacks(window: &AppWindow, controller: &Controller) {
    let c = controller.clone();
    let weak = window.as_weak();
    window.on_set_name(move |name| {
        c.borrow_mut().set_name(name.as_str());
        refresh_sel(&weak, &c);
    });

    let c = controller.clone();
    let weak = window.as_weak();
    window.on_set_x(move |v| {
        if let Ok(x) = v.trim().parse::<f32>() {
            let y = current_y(&c);
            c.borrow_mut().set_position(x, y);
            refresh_sel(&weak, &c);
        }
    });

    let c = controller.clone();
    let weak = window.as_weak();
    window.on_set_y(move |v| {
        if let Ok(y) = v.trim().parse::<f32>() {
            let x = current_x(&c);
            c.borrow_mut().set_position(x, y);
            refresh_sel(&weak, &c);
        }
    });

    let c = controller.clone();
    let weak = window.as_weak();
    window.on_set_w(move |v| {
        if let Ok(w) = v.trim().parse::<f32>() {
            let h = current_h(&c);
            c.borrow_mut().set_size(w, h);
            refresh_sel(&weak, &c);
        }
    });

    let c = controller.clone();
    let weak = window.as_weak();
    window.on_set_h(move |v| {
        if let Ok(h) = v.trim().parse::<f32>() {
            let w = current_w(&c);
            c.borrow_mut().set_size(w, h);
            refresh_sel(&weak, &c);
        }
    });

    let c = controller.clone();
    let weak = window.as_weak();
    window.on_set_opacity(move |v| {
        if let Ok(o) = v.trim().parse::<f32>() {
            c.borrow_mut().set_opacity(o / 100.0);
            refresh_sel(&weak, &c);
        }
    });

    let c = controller.clone();
    let weak = window.as_weak();
    window.on_set_fill(move |v| {
        c.borrow_mut().set_fill_hex(v.as_str());
        refresh_sel(&weak, &c);
    });
}

fn current_x(c: &Controller) -> f32 {
    c.borrow().selected().map(|n| n.transform.translation.x).unwrap_or(0.0)
}
fn current_y(c: &Controller) -> f32 {
    c.borrow().selected().map(|n| n.transform.translation.y).unwrap_or(0.0)
}
fn current_w(c: &Controller) -> f32 {
    c.borrow().selected().map(|n| dims(&n.kind).0).unwrap_or(0.0)
}
fn current_h(c: &Controller) -> f32 {
    c.borrow().selected().map(|n| dims(&n.kind).1).unwrap_or(0.0)
}

fn dims(kind: &NodeKind) -> (f32, f32) {
    match *kind {
        NodeKind::Frame { w, h } | NodeKind::Rectangle { w, h } | NodeKind::Ellipse { w, h } => (w, h),
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

/// Заполняет поля инспектора из узла.
fn set_sel_texts(w: &AppWindow, n: &SceneNode) {
    w.set_sel_name(n.name.clone().into());
    w.set_sel_x_text(fmt_num(n.transform.translation.x).into());
    w.set_sel_y_text(fmt_num(n.transform.translation.y).into());
    let (dw, dh) = dims(&n.kind);
    w.set_sel_w_text(fmt_num(dw).into());
    w.set_sel_h_text(fmt_num(dh).into());
    w.set_sel_opacity_text(fmt_pct(n.opacity).into());
    let (r, g, b) = n.fill.map(|f| (f.color[0], f.color[1], f.color[2])).unwrap_or((0, 0, 0));
    w.set_sel_fill(format!("#{:02X}{:02X}{:02X}", r, g, b).into());
}

/// Обновляет поля после commit (значения нормализованы контроллером).
fn refresh_sel(win: &slint::Weak<AppWindow>, c: &Controller) {
    if let Some(w) = win.upgrade() {
        if let Some(n) = c.borrow().selected() {
            set_sel_texts(&w, n);
        }
    }
}

/// Один тик цикла рендера: рендерит и обновляет UI только если состояние изменилось.
fn sync(
    window: &slint::Weak<AppWindow>,
    controller: &Controller,
    renderer: &RendererRef,
    state: &Rc<RefCell<CanvasState>>,
) {
    let Some(w) = window.upgrade() else { return };

    // Не рендерим, если окно свёрнуто/скрыто.
    if !w.window().is_visible() {
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
    let size_changed = st.ensure(bw, bh);

    // Ничего не изменилось — ничего не делаем.
    if !c.dirty && !size_changed {
        return;
    }
    c.dirty = false;

    // Рендер прямо в переиспользуемый буфер.
    let render_cam = c.camera.for_render_scale(scale);
    let t0 = Instant::now();
    {
        let (rw, rh) = (st.w, st.h);
        let bytes = st.image.make_mut_bytes();
        renderer.borrow_mut().render(
            &c.scene,
            &render_cam,
            rw,
            rh,
            &c.scene.selection,
            c.grid_visible,
            c.preview.clone(),
            bytes,
        );
    }
    st.last_render_us = t0.elapsed().as_micros();
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

    w.set_canvas_texture(Image::from_rgba8(st.image.clone()));

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

    // Дерево слоёв — только при изменении структуры.
    if c.revision != st.last_revision {
        st.last_revision = c.revision;
        let items: Vec<LayerItem> = build_layers(&c);
        let model: ModelRc<LayerItem> = Rc::new(VecModel::from(items)).into();
        w.set_layers(model);
    }
    w.set_selected_layer(c.scene.selection.first().map(|&i| i as i32).unwrap_or(-1));

    // Инспектор — только при смене выделения (поля не затирают ввод).
    let sel_changed = c.selected_id() != st.last_sel_id;
    st.last_sel_id = c.selected_id();
    match c.selected() {
        Some(n) => {
            w.set_has_selection(true);
            if sel_changed {
                set_sel_texts(&w, n);
            }
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
             Mode: CPU (tiny-skia)",
            st.fps,
            st.last_render_us as f64 / 1000.0,
            c.scene.nodes.len(),
            bw,
            bh,
            scale * 100.0,
            c.camera.pan.x,
            c.camera.pan.y,
            c.camera.zoom,
            c.tool.name(),
            c.revision,
            if c.grid_visible { "on" } else { "off" },
            if c.snap_enabled { "on" } else { "off" },
            c.grid_step,
        )
        .into(),
    );
}

fn build_layers(c: &CanvasController) -> Vec<LayerItem> {
    let mut out = Vec::new();
    let mut stack: Vec<(u64, i32)> = c.scene.roots.iter().rev().map(|&id| (id, 0)).collect();
    while let Some((id, depth)) = stack.pop() {
        if let Some(node) = c.scene.get(id) {
            out.push(LayerItem {
                id: id as i32,
                name: node.name.clone().into(),
                depth,
            });
            stack.extend(node.children.iter().rev().map(|&ch| (ch, depth + 1)));
        }
    }
    out
}