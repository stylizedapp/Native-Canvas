//! Обвязка приложения: окно, контроллер, бэкенд рендера, on-demand цикл.
//! Вся UI-логика (sync, инспектор, дерево слоёв) — в `crate::ui`.

use crate::config::{self, AppConfig};
use crate::engine::controller::CanvasController;
use crate::engine::model::nodes::NodeKey;
use crate::engine::profiler::FrameProfiler;
use crate::engine::renderers::create_renderer;
use crate::engine::shortcuts::{self, Shortcut, ShortcutMap};
use crate::engine::tool::Tool;
use crate::engine::transform::pick;
use crate::ui::{
    register_inspector_callbacks, sync, AppWindow, CanvasState, ContextMenuEntry, Controller,
    PaletteCommand, RendererRef, ShortcutEditRow, ShortcutRow,
};
use glam::Vec2;
use slint::{ComponentHandle, ModelRc, VecModel};
use slotmap::{Key, KeyData};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// Троттлинг `render()` при движении мыши: позиция контроллера обновляется на
/// каждое событие, а отрисовка — не чаще ~60 Гц. Лишние кадры коалесцятся в
/// один по таймеру, поэтому финальная позиция курсора не теряется.
struct MoveThrottle {
    last_render: Instant,
    pending: bool,
    timer: slint::Timer,
}

impl MoveThrottle {
    fn new() -> Self {
        Self {
            last_render: Instant::now() - Duration::from_millis(100),
            pending: false,
            timer: slint::Timer::default(),
        }
    }

    fn request(&mut self, render: &Rc<dyn Fn()>) {
        let now = Instant::now();
        if now.duration_since(self.last_render) >= Duration::from_millis(16) {
            self.pending = false;
            self.timer.stop();
            self.last_render = now;
            render();
        } else if !self.pending {
            self.pending = true;
            self.timer.restart();
        }
    }
}

pub fn run() -> Result<(), slint::PlatformError> {
    let window = crate::ui::AppWindow::new()?;
    let controller: Controller = Rc::new(RefCell::new(CanvasController::new()));
    let renderer: RendererRef = Rc::new(RefCell::new(create_renderer()));
    let state = Rc::new(RefCell::new(CanvasState::new()));
    let profiler = Rc::new(RefCell::new(FrameProfiler::new()));

    // Пользовательские настройки: загрузка конфига и применение при старте.
    let config: Rc<RefCell<AppConfig>> = Rc::new(RefCell::new(config::load()));
    let shortcut_map: Rc<RefCell<ShortcutMap>> = Rc::new(RefCell::new(Vec::new()));
    {
        let cfg = config.borrow();
        // Тема: меняем флаг — сам UI применит палитру (см. main.slint).
        window.set_theme_dark(cfg.dark_theme);
        {
            let mut c = controller.borrow_mut();
            c.grid.visible = cfg.grid_visible;
            c.grid.snap = cfg.snap_on;
            c.grid.step = cfg.grid_step;
        }
        shortcut_map.borrow_mut().extend(cfg.shortcuts.clone());
        window.set_grid_on(cfg.grid_visible);
        window.set_snap_on(cfg.snap_on);
        window.set_grid_step_text(fmt_step(cfg.grid_step).into());
    }
    rebuild_shortcut_rows(&window, &shortcut_map);

    // Немедленный on-demand рендер после мутаций. При чистом состоянии sync()
    // сам вернётся сразу (без GPU/VRAM) — вызов безопасен из любого колбэка.
    let render: Rc<dyn Fn()> = {
        let window = window.as_weak();
        let controller = controller.clone();
        let renderer = renderer.clone();
        let state = state.clone();
        let profiler = profiler.clone();
        Rc::new(move || sync(&window, &controller, &renderer, &state, &profiler))
    };

    // Сторож on-demand (200 мс): подхват ресайза, возврата из свёрнутого окна и
    // пропущенного GPU-кадра. При чистом dirty — мгновенный no-op (сон).
    let _watchdog = {
        let render = render.clone();
        let timer = slint::Timer::default();
        timer.start(slint::TimerMode::Repeated, Duration::from_millis(200), move || {
            render();
        });
        timer
    };

    // --- Колбэки инструментов / файлов ---
    {
        let weak = window.as_weak();
        let controller = controller.clone();
        let render = render.clone();
        window.on_tool_changed(move |name| {
            controller.borrow_mut().set_tool(Tool::from_name(name.as_str()));
            if let Some(w) = weak.upgrade() {
                w.set_hovered_handle("".into());
            }
            render();
        });
    }
    {
        let controller = controller.clone();
        let render = render.clone();
        window.on_undo(move || {
            controller.borrow_mut().undo();
            render();
        });
    }
    {
        let controller = controller.clone();
        let render = render.clone();
        window.on_redo(move || {
            controller.borrow_mut().redo();
            render();
        });
    }
    {
        let controller = controller.clone();
        let render = render.clone();
        window.on_delete_selection(move || {
            controller.borrow_mut().delete_selection();
            render();
        });
    }
    {
        let controller = controller.clone();
        let render = render.clone();
        window.on_new_doc(move || {
            controller.borrow_mut().clear();
            render();
        });
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
        let render = render.clone();
        window.on_open_doc(move || {
            if let Some(path) = rfd::FileDialog::new().pick_file() {
                if let Ok(data) = std::fs::read_to_string(&path) {
                    if let Some(w) = weak.upgrade() {
                        let result = controller.borrow_mut().load(&data);
                        match result {
                            Ok(()) => w.set_status_text(format!("Opened: {}", path.display()).into()),
                            Err(e) => w.set_status_text(format!("Open error: {}", e).into()),
                        }
                        render();
                    }
                }
            }
        });
    }

    // --- Колбэки холста (ЭКРАННЫЕ координаты — конверсию делает контроллер) ---
    {
        let controller = controller.clone();
        let state = state.clone();
        let render = render.clone();
        window.on_pointer_down(move |button, x, y, ctrl, alt| {
            let space = state.borrow().space_held;
            controller
                .borrow_mut()
                .pointer_down_full(Vec2::new(x, y), button as u8, ctrl, alt, space);
            render();
        });
    }
    {
        let weak = window.as_weak();
        let controller = controller.clone();
        let render = render.clone();
        // Троттлинг: рендер не чаще ~60 Гц, коалесцинг через одноразовый таймер.
        let throttle: Rc<RefCell<MoveThrottle>> = Rc::new(RefCell::new(MoveThrottle::new()));
        {
            let weak = Rc::downgrade(&throttle);
            let render = render.clone();
            throttle.borrow_mut().timer.start(
                slint::TimerMode::SingleShot,
                Duration::from_millis(8),
                move || {
                    if let Some(t) = weak.upgrade() {
                        let mut t = t.borrow_mut();
                        t.last_render = Instant::now();
                        t.pending = false;
                        render();
                    }
                },
            );
        }
        window.on_pointer_move(move |_button, x, y| {
            let screen = Vec2::new(x, y);
            controller.borrow_mut().pointer_move(screen);
            // Курсор над элементом гизмо (Select, 1 выделенный).
            let handle = controller
                .borrow_mut()
                .hover_gizmo(screen);
            if let Some(w) = weak.upgrade() {
                if w.get_hovered_handle().as_str() != handle {
                    w.set_hovered_handle(handle.into());
                }
            }
            throttle.borrow_mut().request(&render);
        });
    }
    {
        let weak = window.as_weak();
        let controller = controller.clone();
        let render = render.clone();
        window.on_pointer_up(move |_button, x, y| {
            controller.borrow_mut().pointer_up(Vec2::new(x, y));
            if let Some(w) = weak.upgrade() {
                w.set_hovered_handle("".into());
            }
            render();
        });
    }
    {
        let controller = controller.clone();
        let render = render.clone();
        window.on_scroll(move |delta, x, y| {
            controller.borrow_mut().zoom(delta, Vec2::new(x, y));
            render();
        });
    }
    {
        let controller = controller.clone();
        let render = render.clone();
        window.on_select_layer_mod(move |id, ctrl, shift| {
            let key = NodeKey::from(KeyData::from_ffi(id as u32 as u64));
            controller.borrow_mut().select_layer(key, ctrl, shift);
            render();
        });
    }
    {
        let state = state.clone();
        window.on_drag_start(move |id| {
            let key = NodeKey::from(KeyData::from_ffi(id as u32 as u64));
            state.borrow_mut().dragged_layer = Some(key);
        });
    }
    {
        let controller = controller.clone();
        let state = state.clone();
        let render = render.clone();
        window.on_drop_layer(move |id, zone| {
            let dragged = state.borrow_mut().dragged_layer.take();
            if let Some(d) = dragged {
                let target = NodeKey::from(KeyData::from_ffi(id as u32 as u64));
                controller.borrow_mut().drop_layer_at(d, target, zone as u8);
                render();
            }
        });
    }
    {
        // Изменение размера области холста: обязательный рендер (dirty может
        // быть чистым) — sync сам заметит size_changed и пересоздаст буфер.
        let render = render.clone();
        window.on_canvas_resized(move || render());
    }
    {
        // Переименование слоя (инлайн-поле в панели Layers).
        let controller = controller.clone();
        let render = render.clone();
        window.on_rename_layer(move |id, name| {
            let key = NodeKey::from(KeyData::from_ffi(id as u32 as u64));
            controller.borrow_mut().rename(key, name.as_str());
            render();
        });
    }
    {
        // Инлайн-редактирование текста: live-обновление модели на каждый ввод.
        let controller = controller.clone();
        let render = render.clone();
        window.on_edit_text_changed(move |t| {
            controller.borrow_mut().set_editing_text(t.as_str());
            render();
        });
    }
    {
        // Завершение редактирования (Enter/Esc в оверлее).
        let controller = controller.clone();
        let render = render.clone();
        window.on_edit_finished(move || {
            controller.borrow_mut().end_text_edit();
            render();
        });
    }

    // --- Контекстное меню (ПКМ) ---
    fn canvas_menu_entries(c: &CanvasController) -> Vec<ContextMenuEntry> {
        if c.scene.selection().is_empty() {
            vec![
                ContextMenuEntry {
                    id: "paste".into(),
                    text: "Paste".into(),
                    enabled: c.clipboard.is_some(),
                },
                ContextMenuEntry { id: "select-all".into(), text: "Select all".into(), enabled: true },
                ContextMenuEntry { id: "reset-view".into(), text: "Reset view".into(), enabled: true },
            ]
        } else {
            let locked = c
                .scene
                .selection()
                .iter()
                .all(|k| c.scene.get(*k).map(|n| n.is_locked).unwrap_or(false));
            let hidden = c
                .scene
                .selection()
                .iter()
                .all(|k| c.scene.get(*k).map(|n| !n.is_visible).unwrap_or(true));
            vec![
                ContextMenuEntry { id: "copy".into(), text: "Copy".into(), enabled: true },
                ContextMenuEntry { id: "duplicate".into(), text: "Duplicate".into(), enabled: true },
                ContextMenuEntry { id: "delete".into(), text: "Delete".into(), enabled: true },
                ContextMenuEntry { id: "bring-forward".into(), text: "Bring forward".into(), enabled: true },
                ContextMenuEntry { id: "send-backward".into(), text: "Send backward".into(), enabled: true },
                ContextMenuEntry {
                    id: "wrap-in-frame".into(),
                    text: "Frame selection".into(),
                    enabled: true,
                },
                ContextMenuEntry {
                    id: "unframe".into(),
                    text: "Move out of frame".into(),
                    enabled: c
                        .scene
                        .selection()
                        .iter()
                        .any(|k| c.scene.get(*k).and_then(|n| n.parent).is_some()),
                },
                ContextMenuEntry {
                    id: "lock".into(),
                    text: if locked { "Unlock".into() } else { "Lock".into() },
                    enabled: true,
                },
                ContextMenuEntry {
                    id: "hide".into(),
                    text: if hidden { "Show".into() } else { "Hide".into() },
                    enabled: true,
                },
            ]
        }
    }

    fn layer_menu_entries(c: &CanvasController, key: NodeKey) -> Vec<ContextMenuEntry> {
        let locked = c.scene.get(key).map(|n| n.is_locked).unwrap_or(false);
        let hidden = c.scene.get(key).map(|n| !n.is_visible).unwrap_or(true);
        vec![
            ContextMenuEntry { id: "rename".into(), text: "Rename".into(), enabled: true },
            ContextMenuEntry { id: "duplicate".into(), text: "Duplicate".into(), enabled: true },
            ContextMenuEntry { id: "delete".into(), text: "Delete".into(), enabled: true },
            ContextMenuEntry {
                id: "wrap-in-frame".into(),
                text: "Frame selection".into(),
                enabled: true,
            },
            ContextMenuEntry {
                id: "unframe".into(),
                text: "Move out of frame".into(),
                enabled: c.scene.get(key).and_then(|n| n.parent).is_some(),
            },
            ContextMenuEntry {
                id: "lock".into(),
                text: if locked { "Unlock".into() } else { "Lock".into() },
                enabled: true,
            },
            ContextMenuEntry {
                id: "hide".into(),
                text: if hidden { "Show".into() } else { "Hide".into() },
                enabled: true,
            },
        ]
    }

    // ПКМ на канвасе: подбираем узел, собираем меню, показываем.
    {
        let weak = window.as_weak();
        let controller = controller.clone();
        window.on_context_requested(move |x, y| {
            let Some(w) = weak.upgrade() else { return };
            let mut c = controller.borrow_mut();
            let world = c.camera.screen_to_world(Vec2::new(x, y));
            if let Some(key) = pick(&mut c.scene, world) {
                if !c.scene.selection().contains(&key) {
                    c.scene.set_selection(vec![key]);
                    c.layer_anchor = Some(key);
                }
            }
            let entries = canvas_menu_entries(&c);
            let model: ModelRc<ContextMenuEntry> = Rc::new(VecModel::from(entries)).into();
            w.set_context_entries(model);
            w.set_context_open(true);
        });
    }
    // ПКМ по строке в Layers.
    {
        let weak = window.as_weak();
        let controller = controller.clone();
        let state = state.clone();
        window.on_context_request_layer(move |id, _y| {
            let Some(w) = weak.upgrade() else { return };
            let key = NodeKey::from(KeyData::from_ffi(id as u32 as u64));
            let c = controller.borrow();
            if !c.scene.contains(key) {
                return;
            }
            state.borrow_mut().context_target = Some(key);
            let entries = layer_menu_entries(&c, key);
            let model: ModelRc<ContextMenuEntry> = Rc::new(VecModel::from(entries)).into();
            w.set_context_entries(model);
            w.set_context_open(true);
        });
    }
    // Выполнение действия из контекстного меню.
    {
        let weak = window.as_weak();
        let controller = controller.clone();
        let state = state.clone();
        let render = render.clone();
        window.on_context_run(move |action| {
            let Some(w) = weak.upgrade() else { return };
            let mut need_render = true;
            {
                let mut c = controller.borrow_mut();
                match action.as_str() {
                    "copy" => c.copy_selection(),
                    "paste" => c.paste(),
                    "duplicate" => c.duplicate_selection(),
                    "delete" => c.delete_selection(),
                    "bring-forward" => c.bring_forward_selection(),
                    "send-backward" => c.send_backward_selection(),
                    "wrap-in-frame" => {
                        if let Some(key) = state.borrow().context_target {
                            c.scene.set_selection(vec![key]);
                        }
                        c.wrap_in_frame();
                    }
                    "unframe" => c.reparent_selection(None),
                    "lock" => c.toggle_lock_selection(),
                    "hide" => c.toggle_hide_selection(),
                    "select-all" => c.select_all(),
                    "reset-view" => c.reset_view(),
                    "rename" => {
                        if let Some(key) = state.borrow().context_target {
                            let id = key.data().as_ffi() as u32 as i32;
                            w.set_layer_rename_id(id);
                        }
                        need_render = false;
                    }
                    _ => need_render = false,
                }
            }
            if need_render {
                render();
            }
        });
    }

    // --- Сетка / снап ---
    {
        let weak = window.as_weak();
        let controller = controller.clone();
        let render = render.clone();
        window.on_toggle_grid(move || {
            let on = {
                let mut c = controller.borrow_mut();
                c.toggle_grid();
                c.grid.visible
            };
            if let Some(w) = weak.upgrade() {
                w.set_grid_on(on);
            }
            render();
        });
    }
    {
        let weak = window.as_weak();
        let controller = controller.clone();
        let render = render.clone();
        window.on_toggle_snap(move || {
            let on = {
                let mut c = controller.borrow_mut();
                c.toggle_snap();
                c.grid.snap
            };
            if let Some(w) = weak.upgrade() {
                w.set_snap_on(on);
            }
            render();
        });
    }

    // --- Настройки: шаг сетки / сброс вида ---
    {
        let weak = window.as_weak();
        let controller = controller.clone();
        let render = render.clone();
        window.on_set_grid_step(move |v| {
            if let Ok(s) = v.trim().parse::<f32>() {
                controller.borrow_mut().set_grid_step(s);
                let step = controller.borrow().grid.step;
                if let Some(w) = weak.upgrade() {
                    w.set_grid_step_text(format!("{}", step as i64).into());
                }
                render();
            }
        });
    }
    {
        let controller = controller.clone();
        let render = render.clone();
        window.on_reset_view(move || {
            controller.borrow_mut().reset_view();
            render();
        });
    }

    // --- Горячие клавиши: раскладко-независимое разрешение и выполнение ---
    // Таблица переопределений (`shortcut_map`) заполняется из config.json
    // и редактора настроек; пустая = дефолтные шорткаты.
    {
        let map = shortcut_map.clone();
        window.on_shortcut(move |text, ctrl, shift, alt| {
            crate::engine::shortcuts::resolve(&map.borrow(), text.as_str(), ctrl, shift, alt)
                .unwrap_or_default()
                .into()
        });
    }
    {
        // Отпускание клавиш: снимаем временный Pan (Space).
        let state = state.clone();
        window.on_key_release(move |text| {
            if text.trim().is_empty() {
                state.borrow_mut().space_held = false;
            }
        });
    }
    {
        let weak = window.as_weak();
        let controller = controller.clone();
        let state = state.clone();
        let render = render.clone();
        window.on_shortcut_run(move |action| {
            use crate::engine::shortcuts::action as a;
            let Some(w) = weak.upgrade() else { return };
            match action.as_str() {
                a::SELECT => {
                    let _ = w.invoke_tool_changed("select".into());
                }
                a::PAN => {
                    let _ = w.invoke_tool_changed("pan".into());
                }
                a::RECTANGLE => {
                    let _ = w.invoke_tool_changed("rectangle".into());
                }
                a::ELLIPSE => {
                    let _ = w.invoke_tool_changed("ellipse".into());
                }
                a::LINE => {
                    let _ = w.invoke_tool_changed("line".into());
                }
                a::FRAME => {
                    let _ = w.invoke_tool_changed("frame".into());
                }
                a::GRID => {
                    let _ = w.invoke_toggle_grid();
                }
                a::SNAP => {
                    let _ = w.invoke_toggle_snap();
                }
                a::UNDO => {
                    let _ = w.invoke_undo();
                }
                a::REDO => {
                    let _ = w.invoke_redo();
                }
                a::DELETE => {
                    let _ = w.invoke_delete_selection();
                }
                a::ESCAPE => {
                    if w.get_palette_open() {
                        let _ = w.invoke_toggle_palette();
                    } else {
                        let _ = w.invoke_deselect();
                    }
                }
                a::SAVE => {
                    let _ = w.invoke_save_doc();
                }
                a::OPEN => {
                    let _ = w.invoke_open_doc();
                }
                a::NEW => {
                    let _ = w.invoke_new_doc();
                }
                a::RESET_VIEW => {
                    let _ = w.invoke_reset_view();
                }
                a::ZOOM_IN => {
                    let _ = w.invoke_zoom_in();
                }
                a::ZOOM_OUT => {
                    let _ = w.invoke_zoom_out();
                }
                a::PALETTE => {
                    let _ = w.invoke_toggle_palette();
                }
                a::HELP => {
                    let _ = w.invoke_toggle_help();
                }
                a::FIT_ALL => {
                    controller.borrow_mut().fit_to_content();
                    render();
                }
                a::ZOOM_TO_SELECTION => {
                    controller.borrow_mut().zoom_to_selection();
                    render();
                }
                a::RENAME => {
                    let c = controller.borrow();
                    if let Some(key) = c.scene.selection().first() {
                        let id = key.data().as_ffi() as u32 as i32;
                        w.set_layer_rename_id(id);
                    }
                }
                a::SPACE => {
                    state.borrow_mut().space_held = true;
                }
                a::NUDGE_LEFT => {
                    controller.borrow_mut().nudge(-1.0, 0.0);
                    render();
                }
                a::NUDGE_RIGHT => {
                    controller.borrow_mut().nudge(1.0, 0.0);
                    render();
                }
                a::NUDGE_UP => {
                    controller.borrow_mut().nudge(0.0, -1.0);
                    render();
                }
                a::NUDGE_DOWN => {
                    controller.borrow_mut().nudge(0.0, 1.0);
                    render();
                }
                a::NUDGE_FAR_LEFT => {
                    nudge_step(&controller, -1.0, 0.0);
                    render();
                }
                a::NUDGE_FAR_RIGHT => {
                    nudge_step(&controller, 1.0, 0.0);
                    render();
                }
                a::NUDGE_FAR_UP => {
                    nudge_step(&controller, 0.0, -1.0);
                    render();
                }
                a::NUDGE_FAR_DOWN => {
                    nudge_step(&controller, 0.0, 1.0);
                    render();
                }
                a::COPY => {
                    controller.borrow_mut().copy_selection();
                }
                a::CUT => {
                    controller.borrow_mut().cut_selection();
                    render();
                }
                a::PASTE => {
                    controller.borrow_mut().paste();
                    render();
                }
                a::PASTE_IN_PLACE => {
                    controller.borrow_mut().paste_in_place();
                    render();
                }
                a::DUPLICATE => {
                    controller.borrow_mut().duplicate_selection();
                    render();
                }
                a::WRAP_IN_FRAME => {
                    controller.borrow_mut().wrap_in_frame();
                    render();
                }
                _ => {}
            }
        });
    }

    // --- Горячие клавиши: снятие выделения и зум от центра холста ---
    {
        let weak = window.as_weak();
        let controller = controller.clone();
        let render = render.clone();
        window.on_deselect(move || {
            controller.borrow_mut().deselect();
            if let Some(w) = weak.upgrade() {
                w.set_hovered_handle("".into());
            }
            render();
        });
    }
    {
        let weak = window.as_weak();
        let controller = controller.clone();
        let render = render.clone();
        window.on_zoom_in(move || zoom_center(&weak, &controller, 1.1, &render));
    }
    {
        let weak = window.as_weak();
        let controller = controller.clone();
        let render = render.clone();
        window.on_zoom_out(move || zoom_center(&weak, &controller, 1.0 / 1.1, &render));
    }

    // --- Command palette / справка: видимость оверлеев ---
    {
        let weak = window.as_weak();
        window.on_toggle_palette(move || {
            if let Some(w) = weak.upgrade() {
                w.set_palette_open(!w.get_palette_open());
            }
        });
    }
    {
        let weak = window.as_weak();
        window.on_close_palette(move || {
            if let Some(w) = weak.upgrade() {
                w.set_palette_open(false);
            }
        });
    }
    {
        let weak = window.as_weak();
        window.on_toggle_help(move || {
            if let Some(w) = weak.upgrade() {
                w.set_help_open(!w.get_help_open());
            }
        });
    }
    {
        let weak = window.as_weak();
        window.on_close_help(move || {
            if let Some(w) = weak.upgrade() {
                w.set_help_open(false);
            }
        });
    }

    // --- Command palette: команды и их выполнение ---
    let commands = build_commands(&window);
    {
        let items: Vec<PaletteCommand> = commands
            .iter()
            .map(|c| PaletteCommand { title: c.title.into(), hint: c.hint.into() })
            .collect();
        window.set_palette_commands(Rc::new(VecModel::from(items)).into());
    }
    {
        let commands = commands.clone();
        let weak = window.as_weak();
        window.on_palette_run(move |title| {
            if let Some(cmd) = commands.iter().find(|c| title == c.title) {
                (cmd.run)();
            }
            if let Some(w) = weak.upgrade() {
                w.set_palette_open(false);
            }
        });
    }
    {
        let commands = commands.clone();
        let weak = window.as_weak();
        window.on_palette_accept(move |filter| {
            let f = filter.trim().to_lowercase();
            if !f.is_empty() {
                if let Some(cmd) = commands.iter().find(|c| {
                    c.title.to_lowercase().contains(&f) || c.hint.to_lowercase().contains(&f)
                }) {
                    (cmd.run)();
                }
            }
            if let Some(w) = weak.upgrade() {
                w.set_palette_open(false);
            }
        });
    }
    {
        let commands = commands.clone();
        let weak = window.as_weak();
        window.on_palette_filter_changed(move |filter| {
            let f = filter.trim().to_lowercase();
            let items: Vec<PaletteCommand> = commands
                .iter()
                .filter(|c| {
                    f.is_empty()
                        || c.title.to_lowercase().contains(&f)
                        || c.hint.to_lowercase().contains(&f)
                })
                .map(|c| PaletteCommand { title: c.title.into(), hint: c.hint.into() })
                .collect();
            if let Some(w) = weak.upgrade() {
                w.set_palette_commands(Rc::new(VecModel::from(items)).into());
            }
        });
    }

    // --- Справочник шорткатов (Ctrl+/) ---
    {
        let rows: Vec<ShortcutRow> = vec![
            ShortcutRow { keys: "V / R / O / L / F / P".into(), action: "Select / Rectangle / Ellipse / Line / Frame / Pan".into() },
            ShortcutRow { keys: "G".into(), action: "Toggle grid".into() },
            ShortcutRow { keys: "Shift+G".into(), action: "Toggle snap".into() },
            ShortcutRow { keys: "Ctrl+Z".into(), action: "Undo".into() },
            ShortcutRow { keys: "Ctrl+Y / Shift+Z".into(), action: "Redo".into() },
            ShortcutRow { keys: "Delete / Backspace".into(), action: "Delete selection".into() },
            ShortcutRow { keys: "Escape".into(), action: "Deselect / close palette".into() },
            ShortcutRow { keys: "Ctrl+S".into(), action: "Save".into() },
            ShortcutRow { keys: "Ctrl+O".into(), action: "Open".into() },
            ShortcutRow { keys: "Ctrl+N".into(), action: "New document".into() },
            ShortcutRow { keys: "Ctrl+0".into(), action: "Reset view".into() },
            ShortcutRow { keys: "Ctrl+= / Ctrl+-".into(), action: "Zoom in / out".into() },
            ShortcutRow { keys: "Ctrl+K / ?".into(), action: "Command palette".into() },
            ShortcutRow { keys: "Ctrl+/".into(), action: "This help".into() },
        ];
        window.set_shortcut_rows(Rc::new(VecModel::from(rows)).into());
    }

    // --- Дебагер: при включении снимаем метрики профайлера ---
    {
        let weak = window.as_weak();
        let profiler = profiler.clone();
        window.on_toggle_debug(move || {
            if let Some(w) = weak.upgrade() {
                let show = !w.get_debug_show();
                w.set_debug_show(show);
                if show {
                    let snapshot = profiler.borrow().take_snapshot();
                    eprintln!("[profile]\n{snapshot}");
                    w.set_debug_text(snapshot.into());
                }
            }
        });
    }

    // --- Главное меню (☰) и настройки ---
    {
        let weak = window.as_weak();
        window.on_menu_item(move |action| {
            if let Some(w) = weak.upgrade() {
                match action.as_str() {
                    "settings" => {
                        w.set_settings_open(!w.get_settings_open());
                    }
                    "debug" => {
                        let _ = w.invoke_toggle_debug();
                    }
                    "help" => {
                        let _ = w.invoke_toggle_help();
                    }
                    "reset-view" => {
                        let _ = w.invoke_reset_view();
                    }
                    _ => {}
                }
            }
        });
    }
    {
        let weak = window.as_weak();
        window.on_toggle_settings(move || {
            if let Some(w) = weak.upgrade() {
                w.set_settings_open(!w.get_settings_open());
            }
        });
    }
    {
        let weak = window.as_weak();
        window.on_close_settings(move || {
            if let Some(w) = weak.upgrade() {
                w.set_settings_open(false);
            }
        });
    }
    {
        let weak = window.as_weak();
        let config = config.clone();
        window.on_settings_set_dark(move |v| {
            if let Some(w) = weak.upgrade() {
                w.set_theme_dark(v);
                config.borrow_mut().dark_theme = v;
                config::save(&config.borrow());
            }
        });
    }
    {
        let weak = window.as_weak();
        let controller = controller.clone();
        let config = config.clone();
        let render = render.clone();
        window.on_settings_set_grid(move |v| {
            if let Some(w) = weak.upgrade() {
                controller.borrow_mut().set_grid_visible(v);
                w.set_grid_on(v);
                config.borrow_mut().grid_visible = v;
                config::save(&config.borrow());
                render();
            }
        });
    }
    {
        let weak = window.as_weak();
        let controller = controller.clone();
        let config = config.clone();
        let render = render.clone();
        window.on_settings_set_snap(move |v| {
            if let Some(w) = weak.upgrade() {
                controller.borrow_mut().set_snap(v);
                w.set_snap_on(v);
                config.borrow_mut().snap_on = v;
                config::save(&config.borrow());
                render();
            }
        });
    }
    {
        let weak = window.as_weak();
        let controller = controller.clone();
        let config = config.clone();
        let render = render.clone();
        window.on_settings_set_step(move |t| {
            if let Ok(s) = t.trim().parse::<f32>() {
                if let Some(w) = weak.upgrade() {
                    controller.borrow_mut().set_grid_step(s);
                    let step = controller.borrow().grid.step;
                    w.set_grid_step_text(fmt_step(step).into());
                    config.borrow_mut().grid_step = step;
                    config::save(&config.borrow());
                    render();
                }
            }
        });
    }
    {
        let map = shortcut_map.clone();
        let config = config.clone();
        window.on_settings_set_shortcut(move |action, combo| {
            if let Some(s) = shortcuts::parse_combo(combo.as_str()) {
                update_shortcut(&map, action.as_str(), s);
                config.borrow_mut().shortcuts = map.borrow().clone();
                config::save(&config.borrow());
            }
        });
    }
    {
        let weak = window.as_weak();
        let map = shortcut_map.clone();
        let config = config.clone();
        window.on_settings_commit_shortcut(move |action, combo| {
            if let Some(s) = shortcuts::parse_combo(combo.as_str()) {
                update_shortcut(&map, action.as_str(), s);
                config.borrow_mut().shortcuts = map.borrow().clone();
                config::save(&config.borrow());
                if let Some(w) = weak.upgrade() {
                    rebuild_shortcut_rows(&w, &map);
                }
            }
        });
    }
    {
        let weak = window.as_weak();
        let map = shortcut_map.clone();
        let config = config.clone();
        window.on_reset_shortcuts(move || {
            map.borrow_mut().clear();
            config.borrow_mut().shortcuts.clear();
            config::save(&config.borrow());
            if let Some(w) = weak.upgrade() {
                rebuild_shortcut_rows(&w, &map);
            }
        });
    }

    // --- Инспектор ---
    register_inspector_callbacks(&window, &controller, &state, &render);

    // --- GPU-презентация канваса: персистентная GL-текстура (fix 30 FPS) ---
    crate::gl_canvas::install(&window, &state);

    window.run()
}

/// Зум от центра области холста (хоткеи +/-).
fn zoom_center(
    weak: &slint::Weak<crate::ui::AppWindow>,
    controller: &Controller,
    factor: f32,
    render: &Rc<dyn Fn()>,
) {
    let center = weak
        .upgrade()
        .map(|w| Vec2::new(w.get_canvas_width() / 2.0, w.get_canvas_height() / 2.0))
        .unwrap_or(Vec2::ZERO);
    controller.borrow_mut().zoom_at_center(factor, center);
    render();
}

/// Шаг сетки для отображения (целое).
fn fmt_step(v: f32) -> String {
    format!("{}", v as i64)
}

/// Nudge на шаг сетки (Shift+стрелки).
fn nudge_step(controller: &Controller, sx: f32, sy: f32) {
    let mut c = controller.borrow_mut();
    let step = c.grid.step.max(1.0);
    c.nudge(sx * step, sy * step);
}

/// Заменяет комбинацию действия в таблице переопределений.
fn update_shortcut(map: &Rc<RefCell<ShortcutMap>>, action: &str, s: Shortcut) {
    let mut m = map.borrow_mut();
    if let Some((_, list)) = m.iter_mut().find(|(n, _)| n == action) {
        *list = vec![s];
    } else {
        m.push((action.to_string(), vec![s]));
    }
}

/// Пересобирает модель строк редактора шорткатов в настройках.
fn rebuild_shortcut_rows(window: &AppWindow, map: &Rc<RefCell<ShortcutMap>>) {
    let rows: Vec<ShortcutEditRow> = shortcuts::ALL_ACTIONS
        .iter()
        .map(|a| ShortcutEditRow {
            action: a.to_string().into(),
            combo: shortcuts::combo_text(&map.borrow(), a).into(),
        })
        .collect();
    window.set_shortcut_edit_rows(Rc::new(VecModel::from(rows)).into());
}

/// Команда command palette: название + подсказка + действие.
#[derive(Clone)]
struct CommandSpec {
    title: &'static str,
    hint: &'static str,
    run: Rc<dyn Fn()>,
}

/// Строит список команд палитры. Действия переиспользуют уже
/// зарегистрированные колбэки окна через `invoke_*`.
fn build_commands(window: &crate::ui::AppWindow) -> Vec<CommandSpec> {
    let weak = window.as_weak();
    let mut out: Vec<CommandSpec> = Vec::new();

    macro_rules! invoke {
        ($title:expr, $hint:expr, $m:ident) => {{
            let w = weak.clone();
            out.push(CommandSpec {
                title: $title,
                hint: $hint,
                run: Rc::new(move || {
                    if let Some(w) = w.upgrade() {
                        let _ = w.$m();
                    }
                }),
            });
        }};
    }
    macro_rules! tool {
        ($title:expr, $hint:expr, $name:expr) => {{
            let w = weak.clone();
            out.push(CommandSpec {
                title: $title,
                hint: $hint,
                run: Rc::new(move || {
                    if let Some(w) = w.upgrade() {
                        let _ = w.invoke_tool_changed($name.into());
                    }
                }),
            });
        }};
    }

    tool!("Select", "V", "select");
    tool!("Rectangle", "R", "rectangle");
    tool!("Ellipse", "O", "ellipse");
    tool!("Line", "L", "line");
    tool!("Frame", "F", "frame");
    tool!("Pan", "P", "pan");
    invoke!("Toggle grid", "G", invoke_toggle_grid);
    invoke!("Toggle snap", "Shift+G", invoke_toggle_snap);
    invoke!("Undo", "Ctrl+Z", invoke_undo);
    invoke!("Redo", "Ctrl+Y", invoke_redo);
    invoke!("Delete selection", "Del", invoke_delete_selection);
    invoke!("New document", "Ctrl+N", invoke_new_doc);
    invoke!("Open document", "Ctrl+O", invoke_open_doc);
    invoke!("Save document", "Ctrl+S", invoke_save_doc);
    invoke!("Reset view", "Ctrl+0", invoke_reset_view);
    invoke!("Zoom in", "Ctrl+=", invoke_zoom_in);
    invoke!("Zoom out", "Ctrl+-", invoke_zoom_out);
    invoke!("Command palette", "Ctrl+K", invoke_toggle_palette);
    invoke!("Keyboard shortcuts", "Ctrl+/", invoke_toggle_help);

    out
}