//! Персистентные настройки приложения (`config.json` в каталоге конфига ОС,
//! например `%APPDATA%\nativecanvas\config.json`).

use crate::engine::shortcuts::ShortcutMap;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Пользовательские настройки. `ShortcutMap` — переопределения хоткеев
/// (пустой = дефолтная таблица из `shortcuts::default_shortcuts`).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AppConfig {
    pub dark_theme: bool,
    pub grid_visible: bool,
    pub snap_on: bool,
    pub grid_step: f32,
    pub shortcuts: ShortcutMap,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            dark_theme: false,
            grid_visible: true,
            snap_on: true,
            grid_step: 8.0,
            shortcuts: Vec::new(),
        }
    }
}

/// Путь к файлу конфига.
pub fn config_path() -> PathBuf {
    dirs::config_dir()
        .map(|dir| dir.join("nativecanvas").join("config.json"))
        .unwrap_or_else(|| PathBuf::from("nativecanvas-config.json"))
}

/// Загружает конфиг; при отсутствии/ошибке — дефолтный.
pub fn load() -> AppConfig {
    let path = config_path();
    match std::fs::read_to_string(&path) {
        Ok(data) => serde_json::from_str(&data).unwrap_or_default(),
        Err(_) => AppConfig::default(),
    }
}

/// Сохраняет конфиг (создаёт каталог при необходимости). Ошибки игнорируются.
pub fn save(config: &AppConfig) {
    let path = config_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(data) = serde_json::to_string_pretty(config) {
        let _ = std::fs::write(&path, data);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_roundtrip() {
        let cfg = AppConfig::default();
        let json = serde_json::to_string(&cfg).unwrap();
        let back: AppConfig = serde_json::from_str(&json).unwrap();
        assert!(!back.dark_theme);
        assert_eq!(back.grid_step, 8.0);
        assert!(back.shortcuts.is_empty());
    }
}