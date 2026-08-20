//! Глобальный panic-hook: пишет `crash.log` рядом с exe.
//!
//! Профиль release использует `panic = "abort"`, поэтому без хука паника
//! теряется (Windows: STATUS_STACK_BUFFER_OVERRUN). Хук срабатывает ДО abort и
//! сохраняет сообщение + backtrace на диск для диагностики.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;

/// Устанавливает глобальный panic-hook. Вызывается один раз в `main`.
pub fn install() {
    std::panic::set_hook(Box::new(|info| {
        let payload = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().map(|s| s.clone()))
            .unwrap_or_else(|| "unknown panic payload".to_string());

        let mut log = format!(
            "=== panic v{} ({} {})\n{payload}\n",
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS,
            std::env::consts::ARCH
        );
        if let Some(loc) = info.location() {
            log.push_str(&format!("at {}:{}\n", loc.file(), loc.line()));
        }
        // Символы в release вырезаны (strip = true), но адреса помогут отыскать
        // место по PDB/addr2line; в dev-сборке backtrace читаем напрямую.
        let bt = std::backtrace::Backtrace::force_capture();
        log.push_str(&format!("{bt}\n"));

        let mut path =
            std::env::current_exe().unwrap_or_else(|_| PathBuf::from("native_canvas.exe"));
        path.set_file_name("crash.log");
        if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&path) {
            let _ = writeln!(f, "{log}");
            let _ = f.flush();
        }
        eprintln!("{log}");
    }));
}
