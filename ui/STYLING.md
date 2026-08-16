# Стили NativeCanvas (дизайн-система Bento)

Как устроены стили и где что менять. Правило №1: **не хардкодь цвета в .slint** —
используй токены из `Theme`. Исключение — только для холста (см. ниже).

## Слои стилей

| Слой | Файл | Что внутри |
|---|---|---|
| Токены | `ui/theme.slint` | global `Theme`: цвета, отступы, радиусы, шрифты |
| Примитивы | `ui/components/ds.slint` | Panel, SectionHeader, Divider, BentoGrid, BentoColumn, TopBar, ToolButton, ActionButton, Field, NumberField |
| Компоненты | `ui/components/*.slint` | toolbar, layers_panel, inspector, canvas_view |
| Композиция | `ui/main.slint` | раскладка окна (колонки, связи с Rust-колбэками) |
| Цвета холста | `src/engine/renderers/mod.rs` | константы `CANVAS_BG`, `PAGE_BG`, `PAGE_BORDER`, `GRID`, `SELECTION`, `PREVIEW_FILL`, `PREVIEW_STROKE` |

## Токены (`ui/theme.slint`)

- **Surfaces**: `bg`, `surface`, `surface-raised`, `border`, `border-strong` — фон окна, карточек, кнопок, рамки.
- **Text**: `text`, `text-muted`, `text-faint` — основной / приглушённый / тусклый текст.
- **Accent/states**: `accent` (акцентный цвет кнопок и выделения), `accent-hover`, `accent-contrast` (текст на акценте), `selection` (подсветка выбранной строки слоёв), `danger`.
- **Canvas**: `canvas-bg` — фон области холста (см. предупреждение ниже).
- **Spacing**: `space-0..5` (4px-сетка). **Radii**: `radius-sm/md/lg`. **Font**: `font-xs/sm/md/lg`.

## Композиция экрана (`ui/main.slint`)

```
TopBar (плашка сверху)
BentoGrid
 ├── BentoColumn (232px):  Layers + Debug
 ├── CanvasView (центр)    ← сюда же плавающая панель Tools (canvas_view.slint)
 └── BentoColumn (280px):  Inspector + Settings
```

Плавающая панель инструментов живёт внутри `canvas_view.slint` (`Toolbar { x: (root.width - 376px) / 2; y: 8px }`)
и позиционируется абсолютно поверх холста.

## ВАЖНО: цвета холста дублируются

Цвета, которые рисуются в RGBA-буфер (канвас), определены **дважды**:
1. В Slint — `Theme.canvas-bg` (`ui/theme.slint`).
2. В Rust — константы в `src/engine/renderers/mod.rs` (используются обоими бэкендами: vello/GPU и tiny-skia/CPU).

**При смене палитры меняй оба места**, иначе канвас будет отличаться от рамки-карточки.

## Как поменять стиль

1. **Акцентный цвет** (кнопки, рамка выделения, превью): `Theme.accent` в `ui/theme.slint` +
   константы `SELECTION`, `PREVIEW_FILL`, `PREVIEW_STROKE` в `src/engine/renderers/mod.rs`.
2. **Тёмный/светлый фон**: `Theme.bg` / `surface` / `canvas-bg` + `CANVAS_BG`, `PAGE_BG` в renderers.
3. **Шрифты**: `Theme.font-*`. **Скругления**: `Theme.radius-*`. **Отступы**: `Theme.space-*`.
4. **Форма кнопок/полей**: примитивы в `ui/components/ds.slint` (меняется глобально для всего UI).

## Пересборка

Slint-файлы компилируются в бинарник автоматически (`build.rs`), hot-reload нет.
После любой правки стилей:

```
cargo build
```

Запуск: `cargo run` или `.\target\debug\native_canvas.exe`.

## Пример: сменить акцентный цвет

Было `#5b8cff`, стало, например, `#7c5cff`:
1. `ui/theme.slint`: `accent: #7c5cff;` (и при желании `accent-hover`, `accent-contrast`).
2. `src/engine/renderers/mod.rs`:
   - `SELECTION` → `[0x7c, 0x5c, 0xff, 0xff]`
   - `PREVIEW_FILL` → `[0x7c, 0x5c, 0xff, 60]`
   - `PREVIEW_STROKE` → `[0x8e, 0x7a, 0xff, 0xff]`
3. `cargo build`.