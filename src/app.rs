//! Обвязка приложения: окно, контроллер, бэкенд рендера, таймер.
//! Вся UI-логика (sync, инспектор, дерево слоёв) — в `crate::ui`.

use crate::engine::controller::CanvasController;
use crate::engine::renderers::create_renderer;
use crate::engine::tool::Tool;
use crate::ui::{register_inspector_callbacks, sync, CanvasState, Controller, RendererRef};
use glam::Vec2;
use slint::ComponentHandle;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

pub fn run() -> Result<(), slint::PlatformError> {
    let window = crate::ui::AppWindow::new()?;
    let controller: Controller = Rc::new(RefCell::new(CanvasController::new()));
    let renderer: RendererRef = Rc::new(RefCell::new(create_renderer()));
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
            controller.borrow_mut().pointer_down(Vec2::new(x, y), button as u8);
        });
    }
    {
        let controller = controller.clone();
        window.on_pointer_move(move |_button, x, y| {
            controller.borrow_mut().pointer_move(Vec2::new(x, y));
        });
    }
    {
        let controller = controller.clone();
        window.on_pointer_up(move |_button, x, y| {
            controller.borrow_mut().pointer_up(Vec2::new(x, y));
        });
    }
    {
        let controller = controller.clone();
        window.on_scroll(move |delta, x, y| {
            controller.borrow_mut().zoom(delta, Vec2::new(x, y));
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
                c.grid.visible
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
                c.grid.snap
            };
            if let Some(w) = weak.upgrade() {
                w.set_snap_on(on);
            }
        });
    }

    // --- Настройки: шаг сетки / сброс вида ---
    {
        let weak = window.as_weak();
        let controller = controller.clone();
        window.on_set_grid_step(move |v| {
            if let Ok(s) = v.trim().parse::<f32>() {
                controller.borrow_mut().set_grid_step(s);
                let step = controller.borrow().grid.step;
                if let Some(w) = weak.upgrade() {
                    w.set_grid_step_text(format!("{}", step as i64).into());
                }
            }
        });
    }
    {
        let controller = controller.clone();
        window.on_reset_view(move || {
            controller.borrow_mut().reset_view();
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
    register_inspector_callbacks(&window, &controller, &state);

    window.run()
}