mod engine;
mod ui;
mod app;

fn main() -> Result<(), slint::PlatformError> {
    app::run()
}