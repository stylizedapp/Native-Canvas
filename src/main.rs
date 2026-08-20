mod engine;
mod ui;
mod app;
mod config;
mod crash;
mod bench;
mod gl_canvas;

fn main() -> Result<(), slint::PlatformError> {
    // Микро-бенчмарки ядра: `native_canvas --bench` (без запуска GUI).
    if std::env::args().nth(1).as_deref() == Some("--bench") {
        bench::run();
        return Ok(());
    }
    crash::install();
    app::run()
}