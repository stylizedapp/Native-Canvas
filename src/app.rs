//! Обвязка приложения: окно, контроллер, бэкенд рендера, on-demand цикл.
//! Вся UI-логика (sync, инспектор, дерево слоёв) — в `crate::ui`.

use crate::engine::controller::CanvasController;
use crate::engine::model::nodes::NodeKey;
use crate::engine::profiler::FrameProfiler;
use crate::engine::renderers::create_renderer;
use crate::engine::tool::Tool;
use crate::ui::{register_inspector_callbacks, sync, CanvasState, Controller, RendererRef};
use glam::Vec2;
use slint::ComponentHandle;
use slotmap::KeyData;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

pub fn run() -> Result<(), slint::PlatformError> {
    let window = crate::ui::AppWindow::new()?;
    let controller: Controller = Rc::new(RefCell::new(CanvasController::new()));
    let renderer: RendererRef = Rc::new(RefCell::new(create_renderer()));
    let state = Rc::new(RefCell::new(CanvasState::new()));
    let profiler = Rc::new(RefCell::new(FrameProfiler::new()));

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
        let controller = controller.clone();
        let render = render.clone();
        window.on_tool_changed(move |name| {
            controller.borrow_mut().set_tool(Tool::from_name(name.as_str()));
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
        let render = render.clone();
        window.on_pointer_down(move |button, x, y| {
            controller.borrow_mut().pointer_down(Vec2::new(x, y), button as u8);
            render();
        });
    }
    {
        let controller = controller.clone();
        let render = render.clone();
        window.on_pointer_move(move |_button, x, y| {
            controller.borrow_mut().pointer_move(Vec2::new(x, y));
            render();
        });
    }
    {
        let controller = controller.clone();
        let render = render.clone();
        window.on_pointer_up(move |_button, x, y| {
            controller.borrow_mut().pointer_up(Vec2::new(x, y));
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
        window.on_select_layer(move |id| {
            controller.borrow_mut().select(NodeKey::from(KeyData::from_ffi(id as u32 as u64)));
            render();
        });
    }
    {
        // Изменение размера области холста: обязательный рендер (dirty может
        // быть чистым) — sync сам заметит size_changed и пересоздаст буфер.
        let render = render.clone();
        window.on_canvas_resized(move || render());
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

    // --- Инспектор ---
    register_inspector_callbacks(&window, &controller, &state, &render);

    window.run()
}