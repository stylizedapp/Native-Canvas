mod engine;
mod ui;
mod app;
mod config;

fn main() -> Result<(), slint::PlatformError> {
    app::run()
}