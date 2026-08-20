//! Горячие клавиши: раскладко-независимое разрешение.
//!
//! Slint отдаёт в `KeyEvent.text` *логический* символ (зависит от раскладки ОС):
//! на русской раскладке «v» превращается в «м», физическая «/» — в «.», и т.д.
//! Поэтому хит-тест шортката невозможен в `.slint` — он перенесён сюда.
//!
//! [`normalize_layout`] отображает символы ЙЦУКЕН обратно в QWERTY по физическим
//! позициям клавиш, после чего матчинг идёт по каноническому ключу + модификаторам.

use serde::{Deserialize, Serialize};

/// Идентификатор действия (единый для UI, палитры и таблицы настроек).
pub mod action {
    pub const SELECT: &str = "select";
    pub const PAN: &str = "pan";
    pub const RECTANGLE: &str = "rectangle";
    pub const ELLIPSE: &str = "ellipse";
    pub const LINE: &str = "line";
    pub const FRAME: &str = "frame";
    pub const TEXT: &str = "text";
    pub const GRID: &str = "grid";
    pub const SNAP: &str = "snap";
    pub const UNDO: &str = "undo";
    pub const REDO: &str = "redo";
    pub const DELETE: &str = "delete";
    pub const ESCAPE: &str = "escape";
    pub const SAVE: &str = "save";
    pub const OPEN: &str = "open";
    pub const NEW: &str = "new";
    pub const RESET_VIEW: &str = "reset-view";
    pub const ZOOM_IN: &str = "zoom-in";
    pub const ZOOM_OUT: &str = "zoom-out";
    pub const PALETTE: &str = "palette";
    pub const HELP: &str = "help";
    pub const FIT_ALL: &str = "fit-all";
    pub const ZOOM_TO_SELECTION: &str = "zoom-to-selection";
    pub const RENAME: &str = "rename";
    /// Зажат пробел (временный Pan) — состояние, а не действие.
    pub const SPACE: &str = "space";
    // Сдвиг выделения на 1 px (стрелки) / на шаг сетки (Shift+стрелки).
    pub const NUDGE_LEFT: &str = "nudge-left";
    pub const NUDGE_RIGHT: &str = "nudge-right";
    pub const NUDGE_UP: &str = "nudge-up";
    pub const NUDGE_DOWN: &str = "nudge-down";
    pub const NUDGE_FAR_LEFT: &str = "nudge-far-left";
    pub const NUDGE_FAR_RIGHT: &str = "nudge-far-right";
    pub const NUDGE_FAR_UP: &str = "nudge-far-up";
    pub const NUDGE_FAR_DOWN: &str = "nudge-far-down";
    pub const COPY: &str = "copy";
    pub const CUT: &str = "cut";
    pub const PASTE: &str = "paste";
    pub const PASTE_IN_PLACE: &str = "paste-in-place";
    pub const DUPLICATE: &str = "duplicate";
    pub const WRAP_IN_FRAME: &str = "wrap-in-frame";
}

/// Комбинация клавиш. `key` — канонический символ в нижнем регистре
/// (латиница) либо имя именованной клавиши (`"escape"`, `"delete"`, ...).
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Shortcut {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub key: String,
}

impl Shortcut {
    pub fn plain(key: &str) -> Self {
        Self { ctrl: false, shift: false, alt: false, key: key.to_string() }
    }
    pub fn ctrl(key: &str) -> Self {
        Self { ctrl: true, shift: false, alt: false, key: key.to_string() }
    }
    pub fn shift(key: &str) -> Self {
        Self { ctrl: false, shift: true, alt: false, key: key.to_string() }
    }
    pub fn ctrl_shift(key: &str) -> Self {
        Self { ctrl: true, shift: true, alt: false, key: key.to_string() }
    }
}

/// Действие → одна или несколько комбинаций.
pub type ShortcutMap = Vec<(String, Vec<Shortcut>)>;

/// Все действия в порядке отображения (настройки / справка).
pub const ALL_ACTIONS: &[&str] = &[
    action::SELECT,
    action::PAN,
    action::RECTANGLE,
    action::ELLIPSE,
    action::LINE,
    action::FRAME,
    action::TEXT,
    action::GRID,
    action::SNAP,
    action::UNDO,
    action::REDO,
    action::DELETE,
    action::ESCAPE,
    action::SAVE,
    action::OPEN,
    action::NEW,
    action::RESET_VIEW,
    action::ZOOM_IN,
    action::ZOOM_OUT,
    action::PALETTE,
    action::HELP,
    action::WRAP_IN_FRAME,
];

/// Строка комбинаций действия (для настроек/справки): «Ctrl+Shift+Z».
pub fn combo_text(map: &ShortcutMap, action: &str) -> String {
    let defaults = default_shortcuts();
    let list = map
        .iter()
        .find(|(n, _)| n == action)
        .or_else(|| defaults.iter().find(|(n, _)| n == action))
        .map(|(_, l)| l);
    match list {
        Some(l) => l.iter().map(format_shortcut).collect::<Vec<_>>().join(" / "),
        None => String::new(),
    }
}

/// Парсит текстовую комбинацию («Ctrl+Shift+Z», «z», «Escape») в `Shortcut`.
pub fn parse_combo(s: &str) -> Option<Shortcut> {
    let (mut ctrl, mut shift, mut alt) = (false, false, false);
    let mut key: Option<String> = None;
    for part in s.split('+') {
        let part = part.trim();
        if part.is_empty() {
            return None;
        }
        match part.to_lowercase().as_str() {
            "ctrl" | "control" => ctrl = true,
            "shift" => shift = true,
            "alt" => alt = true,
            _ => {
                if key.is_some() {
                    return None; // больше одной клавиши — недопустимо
                }
                key = Some(match part.to_lowercase().as_str() {
                    "space" => " ".to_string(),
                    "esc" => "escape".to_string(),
                    "del" | "delete" => "delete".to_string(),
                    "backspace" => "backspace".to_string(),
                    other => other.to_lowercase(),
                });
            }
        }
    }
    let key = key?;
    if key.is_empty() {
        return None;
    }
    Some(Shortcut { ctrl, shift, alt, key })
}

/// Дефолтная таблица шорткатов (порядок важен: действие с совпавшей
/// комбинацией побеждает, поэтому конкретные комбинации не дублируются).
pub fn default_shortcuts() -> ShortcutMap {
    vec![
        (action::SELECT.to_string(), vec![Shortcut::plain("v"), Shortcut::plain("z")]),
        (action::PAN.to_string(), vec![Shortcut::plain("p"), Shortcut::plain("x")]),
        (action::RECTANGLE.to_string(), vec![Shortcut::plain("r")]),
        (action::ELLIPSE.to_string(), vec![Shortcut::plain("o")]),
        (action::LINE.to_string(), vec![Shortcut::plain("l")]),
        (action::FRAME.to_string(), vec![Shortcut::plain("f")]),
        (action::TEXT.to_string(), vec![Shortcut::plain("t")]),
        (action::GRID.to_string(), vec![Shortcut::plain("g")]),
        (action::SNAP.to_string(), vec![Shortcut::shift("g")]),
        (action::UNDO.to_string(), vec![Shortcut::ctrl("z")]),
        (action::REDO.to_string(), vec![Shortcut::ctrl("y"), Shortcut::ctrl_shift("z")]),
        (action::DELETE.to_string(), vec![Shortcut::plain("delete"), Shortcut::plain("backspace")]),
        (action::ESCAPE.to_string(), vec![Shortcut::plain("escape")]),
        (action::SAVE.to_string(), vec![Shortcut::ctrl("s")]),
        (action::OPEN.to_string(), vec![Shortcut::ctrl("o")]),
        (action::NEW.to_string(), vec![Shortcut::ctrl("n")]),
        (action::RESET_VIEW.to_string(), vec![Shortcut::ctrl("0")]),
        (action::FIT_ALL.to_string(), vec![Shortcut::shift("1")]),
        (action::ZOOM_TO_SELECTION.to_string(), vec![Shortcut::shift("2")]),
        (action::RENAME.to_string(), vec![Shortcut::ctrl("r"), Shortcut::plain("f2")]),
        (action::SPACE.to_string(), vec![Shortcut::plain(" ")]),
        (action::COPY.to_string(), vec![Shortcut::ctrl("c")]),
        (action::CUT.to_string(), vec![Shortcut::ctrl("x")]),
        (action::PASTE.to_string(), vec![Shortcut::ctrl("v")]),
        (action::PASTE_IN_PLACE.to_string(), vec![Shortcut::shift("v")]),
        (action::DUPLICATE.to_string(), vec![Shortcut::ctrl("d")]),
        (action::WRAP_IN_FRAME.to_string(), vec![Shortcut { ctrl: true, shift: false, alt: true, key: "g".into() }]),
        (action::NUDGE_LEFT.to_string(), vec![Shortcut::plain("arrowleft")]),
        (action::NUDGE_RIGHT.to_string(), vec![Shortcut::plain("arrowright")]),
        (action::NUDGE_UP.to_string(), vec![Shortcut::plain("arrowup")]),
        (action::NUDGE_DOWN.to_string(), vec![Shortcut::plain("arrowdown")]),
        (action::NUDGE_FAR_LEFT.to_string(), vec![Shortcut::shift("arrowleft")]),
        (action::NUDGE_FAR_RIGHT.to_string(), vec![Shortcut::shift("arrowright")]),
        (action::NUDGE_FAR_UP.to_string(), vec![Shortcut::shift("arrowup")]),
        (action::NUDGE_FAR_DOWN.to_string(), vec![Shortcut::shift("arrowdown")]),
        (action::ZOOM_IN.to_string(), vec![Shortcut::ctrl("="), Shortcut::ctrl("+")]),
        (action::ZOOM_OUT.to_string(), vec![Shortcut::ctrl("-"), Shortcut::ctrl("_")]),
        (action::PALETTE.to_string(), vec![Shortcut::ctrl("k"), Shortcut::shift("/")]),
        (action::HELP.to_string(), vec![Shortcut::ctrl("/")]),
    ]
}

/// Разрешает шорткат в действие. `text` — `event.text` как есть; модификаторы
/// — из `event.modifiers`. Пользовательские переопределения (`user`) имеют
/// приоритет над дефолтными и заменяют их для тех же действий.
pub fn resolve(
    user: &ShortcutMap,
    text: &str,
    ctrl: bool,
    shift: bool,
    alt: bool,
) -> Option<String> {
    let key = normalize_layout(text).to_lowercase();
    if key.is_empty() {
        return None;
    }
    let defaults = default_shortcuts();
    // Порядок имён: сначала пользовательские переопределения, затем дефолтные.
    let mut names: Vec<&str> = Vec::new();
    for (n, _) in user.iter().chain(defaults.iter()) {
        if !names.contains(&n.as_str()) {
            names.push(n.as_str());
        }
    }
    for name in names {
        let list = user
            .iter()
            .find(|(n, _)| n == name)
            .or_else(|| defaults.iter().find(|(n, _)| n == name))
            .map(|(_, l)| l)
            .expect("имя найдено");
        for s in list {
            if s.ctrl == ctrl && s.shift == shift && s.alt == alt && s.key == key {
                return Some(name.to_string());
            }
        }
    }
    None
}

/// Нормализация текста события по физическим позициям клавиш.
///
/// Русская ЙЦУКЕН на тех же физических клавишах даёт другие символы; маппим их
/// обратно в QWERTY. Регистр сохраняется (сравнение всё равно по нижнему).
/// Дополнительно: «.» на RU — это физическая клавиша «/» (→ «/»), «?» — Shift+7.
pub fn normalize_layout(text: &str) -> String {
    text.chars().map(map_char).collect()
}

fn map_char(c: char) -> char {
    const PAIRS: &[(char, char)] = &[
        // ЙЦУКЕН → QWERTY (по физическим позициям), строчные.
        ('ё', '`'), ('й', 'q'), ('ц', 'w'), ('у', 'e'), ('к', 'r'), ('е', 't'),
        ('н', 'y'), ('г', 'u'), ('ш', 'i'), ('щ', 'o'), ('з', 'p'), ('х', '['),
        ('ъ', ']'), ('ф', 'a'), ('ы', 's'), ('в', 'd'), ('а', 'f'), ('п', 'g'),
        ('р', 'h'), ('о', 'j'), ('л', 'k'), ('д', 'l'), ('ж', ';'), ('э', '\''),
        ('я', 'z'), ('ч', 'x'), ('с', 'c'), ('м', 'v'), ('и', 'b'), ('т', 'n'),
        ('ь', 'm'), ('б', ','), ('ю', '.'),
        // Заглавные.
        ('Ё', '`'), ('Й', 'Q'), ('Ц', 'W'), ('У', 'E'), ('К', 'R'), ('Е', 'T'),
        ('Н', 'Y'), ('Г', 'U'), ('Ш', 'I'), ('Щ', 'O'), ('З', 'P'), ('Х', '['),
        ('Ъ', ']'), ('Ф', 'A'), ('Ы', 'S'), ('В', 'D'), ('А', 'F'), ('П', 'G'),
        ('Р', 'H'), ('О', 'J'), ('Л', 'K'), ('Д', 'L'), ('Ж', ';'), ('Э', '\''),
        ('Я', 'Z'), ('Ч', 'X'), ('С', 'C'), ('М', 'V'), ('И', 'B'), ('Т', 'N'),
        ('Ь', 'M'), ('Б', ','), ('Ю', '.'),
        // Символы: на RU «.» — физическая «/», «?» — Shift+7.
        ('.', '/'), ('?', '/'),
    ];
    for &(ru, us) in PAIRS {
        if c == ru {
            return us;
        }
    }
    c
}

/// Человекочитаемое отображение комбинации (для таблицы настроек и справки).
pub fn format_shortcut(s: &Shortcut) -> String {
    let mut out = String::new();
    if s.ctrl {
        out.push_str("Ctrl+");
    }
    if s.alt {
        out.push_str("Alt+");
    }
    if s.shift {
        out.push_str("Shift+");
    }
    match s.key.as_str() {
        " " => out.push_str("Space"),
        "escape" => out.push_str("Esc"),
        "delete" => out.push_str("Del"),
        "backspace" => out.push_str("Backspace"),
        "arrowleft" => out.push_str("←"),
        "arrowright" => out.push_str("→"),
        "arrowup" => out.push_str("↑"),
        "arrowdown" => out.push_str("↓"),
        "f2" => out.push_str("F2"),
        key if key.len() == 1 => out.push_str(&key.to_uppercase()),
        key => out.push_str(key),
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ru_layout_maps_to_us() {
        assert_eq!(normalize_layout("м"), "v");
        assert_eq!(normalize_layout("к"), "r");
        assert_eq!(normalize_layout("я"), "z");
        assert_eq!(normalize_layout("щ"), "o");
        assert_eq!(normalize_layout("."), "/");
        assert_eq!(normalize_layout("?"), "/");
        // Заглавные.
        assert_eq!(normalize_layout("М"), "V");
        assert_eq!(normalize_layout("П"), "G");
    }

    #[test]
    fn us_layout_passes_through() {
        assert_eq!(normalize_layout("v"), "v");
        assert_eq!(normalize_layout("/"), "/");
        assert_eq!(normalize_layout("Escape"), "Escape");
    }

    #[test]
    fn resolve_select_on_ru() {
        let map: ShortcutMap = Vec::new();
        assert_eq!(resolve(&map, "м", false, false, false), Some(action::SELECT.to_string()));
        assert_eq!(resolve(&map, "V", false, false, false), Some(action::SELECT.to_string()));
    }

    #[test]
    fn resolve_redo_on_ru() {
        let map: ShortcutMap = Vec::new();
        assert_eq!(resolve(&map, "я", true, false, false), Some(action::UNDO.to_string()));
        assert_eq!(resolve(&map, "н", true, false, false), Some(action::REDO.to_string()));
    }

    #[test]
    fn resolve_help_slash_on_ru() {
        let map: ShortcutMap = Vec::new();
        assert_eq!(resolve(&map, ".", true, false, false), Some(action::HELP.to_string()));
    }

    #[test]
    fn resolve_palette_shift_slash() {
        let map: ShortcutMap = Vec::new();
        assert_eq!(resolve(&map, "?", false, true, false), Some(action::PALETTE.to_string()));
        assert_eq!(resolve(&map, "/", false, true, false), Some(action::PALETTE.to_string()));
    }

    #[test]
    fn named_keys() {
        let map: ShortcutMap = Vec::new();
        assert_eq!(resolve(&map, "Escape", false, false, false), Some(action::ESCAPE.to_string()));
        assert_eq!(resolve(&map, "Delete", false, false, false), Some(action::DELETE.to_string()));
        assert_eq!(resolve(&map, "Backspace", false, false, false), Some(action::DELETE.to_string()));
    }

    #[test]
    fn user_override_replaces_default() {
        let map: ShortcutMap =
            vec![(action::SELECT.to_string(), vec![Shortcut::ctrl("v")])];
        // Дефолтный «v» без Ctrl больше не срабатывает.
        assert_eq!(resolve(&map, "v", false, false, false), None);
        assert_eq!(resolve(&map, "v", true, false, false), Some(action::SELECT.to_string()));
    }

#[test]
    fn unknown_key_is_none() {
        let map: ShortcutMap = Vec::new();
        assert_eq!(resolve(&map, "q", false, false, false), None);
        assert_eq!(resolve(&map, "~", false, false, false), None);
    }

    #[test]
    fn zx_select_pan_and_c_free() {
        let map: ShortcutMap = Vec::new();
        // Z = Select (и старый V), X = Pan (и старый P).
        assert_eq!(resolve(&map, "z", false, false, false), Some(action::SELECT.to_string()));
        assert_eq!(resolve(&map, "x", false, false, false), Some(action::PAN.to_string()));
        // C остаётся свободной (не назначает никакое действие).
        assert_eq!(resolve(&map, "c", false, false, false), None);
    }

    #[test]
    fn arrows_and_fit_zoom() {
        let map: ShortcutMap = Vec::new();
        assert_eq!(resolve(&map, "ArrowLeft", false, false, false), Some(action::NUDGE_LEFT.to_string()));
        assert_eq!(resolve(&map, "ArrowRight", true, false, false), None);
        assert_eq!(resolve(&map, "1", false, true, false), Some(action::FIT_ALL.to_string()));
        assert_eq!(resolve(&map, "2", false, true, false), Some(action::ZOOM_TO_SELECTION.to_string()));
        assert_eq!(resolve(&map, "r", true, false, false), Some(action::RENAME.to_string()));
    }

    #[test]
    fn space_is_state_action() {
        let map: ShortcutMap = Vec::new();
        assert_eq!(resolve(&map, " ", false, false, false), Some(action::SPACE.to_string()));
    }

    #[test]
    fn format_display() {
        assert_eq!(format_shortcut(&Shortcut::ctrl_shift("z")), "Ctrl+Shift+Z");
        assert_eq!(format_shortcut(&Shortcut::plain("escape")), "Esc");
        assert_eq!(format_shortcut(&Shortcut::plain("delete")), "Del");
    }

    #[test]
    fn parse_combo_cases() {
        assert_eq!(parse_combo("Ctrl+Shift+Z"), Some(Shortcut::ctrl_shift("z")));
        assert_eq!(parse_combo("z"), Some(Shortcut::plain("z")));
        assert_eq!(parse_combo("Ctrl+/"), Some(Shortcut::ctrl("/")));
        assert_eq!(parse_combo("Escape"), Some(Shortcut::plain("escape")));
        assert_eq!(parse_combo("ctrl+s"), Some(Shortcut::ctrl("s")));
        assert_eq!(parse_combo(""), None);
        assert_eq!(parse_combo("Ctrl+Shift+Alt+Z"), Some(Shortcut { ctrl: true, shift: true, alt: true, key: "z".into() }));
        assert_eq!(parse_combo("a+b"), None);
    }

    #[test]
    fn combo_text_uses_defaults() {
        let map: ShortcutMap = Vec::new();
        assert_eq!(combo_text(&map, action::UNDO), "Ctrl+Z");
        assert_eq!(combo_text(&map, action::DELETE), "Del / Backspace");
    }
}