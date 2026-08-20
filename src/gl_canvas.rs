//! Персистентная GL-текстура канваса через `RenderingNotifier`.
//!
//! Проблема: `Image::from_rgba8` на каждый кадр создаёт новую GL-текстуру
//! (`ImageCacheKey::Invalid` -> `new_from_image` -> `glTexImage2D` + upload) и
//! удаляет старую, плюс `PartialEq` делает 8 МБ `memcmp` при каждом
//! `set_canvas_texture`.
//!
//! Решение: tiny-skia рендерит в CPU-буфер (двойной буфер `CanvasState.images`),
//! а `BeforeRendering` заливает его в одну переиспользуемую GL-текстуру через
//! `glTexSubImage2D`. Slint рисует её как `BorrowedOpenGLTexture` (обёртка над
//! существующей текстурой, `owned:false` — без delete; PartialEq по texture_id —
//! без memcmp). Текстура создаётся/пересоздаётся только при изменении размера.

use crate::ui::{AppWindow, CanvasState};
use glow::HasContext;
use slint::{ComponentHandle, GraphicsAPI, RenderingState};
use std::cell::RefCell;
use std::num::NonZeroU32;
use std::rc::Rc;
use std::time::Instant;

/// Регистрирует `RenderingNotifier`: создание/upload/удаление персистентной
/// текстуры и установку свойства `canvas-texture` (через `BorrowedOpenGLTexture`).
pub fn install(window: &AppWindow, state: &Rc<RefCell<CanvasState>>) {
    let weak = window.as_weak();
    let state = state.clone();
    let glow = Rc::new(RefCell::new(None::<glow::Context>));
    let mut texture: Option<glow::NativeTexture> = None;
    let mut tw: u32 = 0;
    let mut th: u32 = 0;
    let mut need_set = false;
    let mut frame_start = Instant::now();

    let _ = window.window().set_rendering_notifier(move |event, api| match event {
        RenderingState::RenderingSetup => {
            if let GraphicsAPI::NativeOpenGL { get_proc_address } = api {
                // Только с GL-контекстом можно использовать BorrowedOpenGLTexture;
                // при другом бэкенде остаётся fallback в sync() (from_rgba8).
                unsafe {
                    *glow.borrow_mut() =
                        Some(glow::Context::from_loader_function_cstr(get_proc_address));
                }
            }
        }
        RenderingState::BeforeRendering => {
            let now = Instant::now();
            let frame_gap_us = now.duration_since(frame_start).as_micros();
            frame_start = now;
            let t0 = Instant::now();

            let gl_opt = glow.borrow();
            let Some(gl) = gl_opt.as_ref() else {
                return;
            };
            let mut st = state.borrow_mut();
            let (w, h) = (st.w, st.h);
            if w < 1 || h < 1 {
                return;
            }

            // Пересоздание текстуры при изменении размера буфера (ресайз окна).
            if let Some(tex) = texture {
                if tw != w || th != h {
                    unsafe {
                        gl.delete_texture(tex);
                    }
                    texture = None;
                }
            }

            if texture.is_none() {
                unsafe {
                    let t = gl
                        .create_texture()
                        .expect("glGenTextures failed for canvas texture");
                    gl.bind_texture(glow::TEXTURE_2D, Some(t));
                    gl.tex_image_2d(
                        glow::TEXTURE_2D,
                        0,
                        glow::RGBA as i32,
                        w as i32,
                        h as i32,
                        0,
                        glow::RGBA,
                        glow::UNSIGNED_BYTE,
                        glow::PixelUnpackData::Slice(None),
                    );
                    gl.tex_parameter_i32(
                        glow::TEXTURE_2D,
                        glow::TEXTURE_MIN_FILTER,
                        glow::LINEAR as i32,
                    );
                    gl.tex_parameter_i32(
                        glow::TEXTURE_2D,
                        glow::TEXTURE_MAG_FILTER,
                        glow::LINEAR as i32,
                    );
                    gl.tex_parameter_i32(
                        glow::TEXTURE_2D,
                        glow::TEXTURE_WRAP_S,
                        glow::CLAMP_TO_EDGE as i32,
                    );
                    gl.tex_parameter_i32(
                        glow::TEXTURE_2D,
                        glow::TEXTURE_WRAP_T,
                        glow::CLAMP_TO_EDGE as i32,
                    );
                    gl.bind_texture(glow::TEXTURE_2D, None);
                    texture = Some(t);
                    tw = w;
                    th = h;
                    need_set = true;
                }
            }

            if st.present_pending {
                if let Some(tex) = texture {
                    unsafe {
                        upload(gl, tex, &st.images[st.cur]);
                    }
                    st.present_pending = false;
                }
            }

            st.gl_frame_gap_us = frame_gap_us;
            st.gl_present_us = t0.elapsed().as_micros();
        }
        RenderingState::AfterRendering => {
            // Свойство ставится после отрисовки сцены (без риска re-entrancy в
            // итераторе элементов). Только при создании/пересоздании текстуры —
            // иначе равное значение не вызовет dirty-цикла.
            if need_set {
                need_set = false;
                if let Some(w) = weak.upgrade() {
                    let Some(tex) = texture else { return };
                    let mut st = state.borrow_mut();
                    let size = st.images[st.cur].size();
                    st.gl_ready = true;
                    drop(st);
                    let img = unsafe {
                        slint::BorrowedOpenGLTextureBuilder::new_gl_2d_rgba_texture(tex.0, size)
                            .origin(slint::BorrowedOpenGLTextureOrigin::TopLeft)
                            .build()
                    };
                    w.set_canvas_texture(img);
                }
            }
            let mut st = state.borrow_mut();
            st.gl_frame_us = frame_start.elapsed().as_micros();
        }
        RenderingState::RenderingTeardown => {
            if let Some(gl) = glow.borrow().as_ref() {
                if let Some(tex) = texture.take() {
                    unsafe {
                        gl.delete_texture(tex);
                    }
                }
            }
            texture = None;
        }
        _ => {}
    });
}

/// `glTexSubImage2D` буфера в текстуру с сохранением/восстановлением GL-состояния
/// (документированное требование к колбэкам RenderingNotifier).
unsafe fn upload(
    gl: &glow::Context,
    tex: glow::NativeTexture,
    buf: &slint::SharedPixelBuffer<slint::Rgba8Pixel>,
) {
    let (w, h) = (buf.width() as i32, buf.height() as i32);
    let data = buf.as_bytes();
    let active_unit = gl.get_parameter_i32(glow::ACTIVE_TEXTURE);
    let bound = gl.get_parameter_i32(glow::TEXTURE_BINDING_2D);
    let unpack_align = gl.get_parameter_i32(glow::UNPACK_ALIGNMENT);

    gl.active_texture(glow::TEXTURE0);
    gl.bind_texture(glow::TEXTURE_2D, Some(tex));
    gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
    gl.tex_sub_image_2d(
        glow::TEXTURE_2D,
        0,
        0,
        0,
        w,
        h,
        glow::RGBA,
        glow::UNSIGNED_BYTE,
        glow::PixelUnpackData::Slice(Some(data)),
    );

    gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, unpack_align);
    gl.bind_texture(
        glow::TEXTURE_2D,
        (bound != 0).then(|| glow::NativeTexture(NonZeroU32::new(bound as u32).unwrap())),
    );
    gl.active_texture(active_unit as u32);
}