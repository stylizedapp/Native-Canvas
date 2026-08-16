//! Вспомогательные типы модели: цвета, заливки, обводки, эффекты, автолейаут,
//! типографика и пресеты экспорта.
//!
//! Все типы — `#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]`.

use glam::Vec2;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Цвет, каналы в диапазоне 0.0..=1.0.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Color {
    pub const TRANSPARENT: Color = Color::rgba(0.0, 0.0, 0.0, 0.0);
    pub const BLACK: Color = Color::rgb(0.0, 0.0, 0.0);
    pub const WHITE: Color = Color::rgb(1.0, 1.0, 1.0);

    pub const fn rgba(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self { r, g, b, a }
    }

    pub const fn rgb(r: f32, g: f32, b: f32) -> Self {
        Self { r, g, b, a: 1.0 }
    }

    /// Из u8 RGBA (совместимо с прототипом: `[u8; 4]`).
    pub fn from_rgba8(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self::rgba(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, a as f32 / 255.0)
    }

    /// В u8 RGBA.
    pub fn to_rgba8(self) -> [u8; 4] {
        [
            (self.r.clamp(0.0, 1.0) * 255.0).round() as u8,
            (self.g.clamp(0.0, 1.0) * 255.0).round() as u8,
            (self.b.clamp(0.0, 1.0) * 255.0).round() as u8,
            (self.a.clamp(0.0, 1.0) * 255.0).round() as u8,
        ]
    }

    /// Непрозрачный ли цвет (`a == 1.0`).
    pub fn is_opaque(self) -> bool {
        self.a >= 1.0
    }
}

/// Одна точка градиента: позиция 0.0..=1.0 и цвет.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct GradientStop {
    pub position: f32,
    pub color: Color,
}

/// Чем заливается узел (аналог `Paint` в Figma).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Paint {
    Solid(Color),
    LinearGradient {
        start: Vec2,
        end: Vec2,
        stops: Vec<GradientStop>,
    },
    RadialGradient {
        center: Vec2,
        radius: f32,
        stops: Vec<GradientStop>,
    },
    Image {
        data: Arc<[u8]>,
        width: u32,
        height: u32,
    },
}

impl Paint {
    pub fn solid(color: Color) -> Self {
        Paint::Solid(color)
    }
}

/// Смещение обводки относительно контура фигуры.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StrokeAlign {
    Inside,
    Center,
    Outside,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StrokeCap {
    Butt,
    Round,
    Square,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StrokeJoin {
    Miter,
    Round,
    Bevel,
}

/// Обводка узла.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Stroke {
    pub paint: Paint,
    pub width: f32,
    pub align: StrokeAlign,
    pub cap: StrokeCap,
    pub join: StrokeJoin,
    pub dash_pattern: Vec<f32>,
}

impl Stroke {
    /// Сплошная обводка по центру контура (как в прототипе).
    pub fn solid(color: Color, width: f32) -> Self {
        Self {
            paint: Paint::Solid(color),
            width,
            align: StrokeAlign::Center,
            cap: StrokeCap::Round,
            join: StrokeJoin::Round,
            dash_pattern: Vec::new(),
        }
    }
}

/// Режимы смешивания (набор Figma/SVG).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlendMode {
    Normal,
    Multiply,
    Screen,
    Overlay,
    Darken,
    Lighten,
    ColorDodge,
    ColorBurn,
    HardLight,
    SoftLight,
    Difference,
    Exclusion,
    Hue,
    Saturation,
    Color,
    Luminosity,
}

/// Эффект узла (тень/размытие).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Effect {
    DropShadow {
        offset: Vec2,
        blur: f32,
        spread: f32,
        color: Color,
    },
    InnerShadow {
        offset: Vec2,
        blur: f32,
        spread: f32,
        color: Color,
    },
    LayerBlur {
        radius: f32,
    },
    BackgroundBlur {
        radius: f32,
    },
}

impl Effect {
    pub fn drop_shadow(offset: Vec2, blur: f32, color: Color) -> Self {
        Effect::DropShadow { offset, blur, spread: 0.0, color }
    }
}

/// Горизонтальное ограничение (родитель/контейнер).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConstraintH {
    Left,
    Right,
    Center,
    LeftRight,
    Scale,
}

/// Вертикальное ограничение.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConstraintV {
    Top,
    Bottom,
    Center,
    TopBottom,
    Scale,
}

/// Ограничения узла внутри родителя.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Constraints {
    pub horizontal: ConstraintH,
    pub vertical: ConstraintV,
}

impl Default for Constraints {
    fn default() -> Self {
        Self { horizontal: ConstraintH::Left, vertical: ConstraintV::Top }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LayoutDirection {
    Horizontal,
    Vertical,
}

/// Выравнивание элементов поперёк основной оси.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LayoutAlign {
    Stretch,
    Min,
    Center,
    Max,
}

/// Распределение вдоль основной оси.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LayoutJustify {
    Min,
    Center,
    Max,
    SpaceBetween,
}

/// Конфигурация автолейаута фрейма.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct AutoLayoutConfig {
    pub direction: LayoutDirection,
    pub spacing: f32,
    /// [top, right, bottom, left].
    pub padding: [f32; 4],
    pub align_items: LayoutAlign,
    pub justify_content: LayoutJustify,
}

impl Default for AutoLayoutConfig {
    fn default() -> Self {
        Self {
            direction: LayoutDirection::Horizontal,
            spacing: 0.0,
            padding: [0.0; 4],
            align_items: LayoutAlign::Stretch,
            justify_content: LayoutJustify::Min,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TextAlign {
    Left,
    Center,
    Right,
    Justified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TextSizeMode {
    AutoWidth,
    AutoHeight,
    Fixed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExportFormat {
    Png,
    Svg,
    Pdf,
    Jpeg,
}

/// Пресет экспорта (Figma: формат + масштаб + суффикс файла).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExportPreset {
    pub format: ExportFormat,
    pub scale: f32,
    pub suffix: String,
}

impl ExportPreset {
    pub fn png(scale: f32) -> Self {
        Self { format: ExportFormat::Png, scale, suffix: String::new() }
    }
}

/// Правило заполнения контура. В kurbo 0.13 нет `FillRule` (он есть в
/// peniko/tiny-skia), поэтому определён собственный enum, совместимый с обоими.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FillRule {
    NonZero,
    EvenOdd,
}

/// Значение переопределяемого свойства для экземпляра компонента
/// (`Component { overrides }`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PropertyValue {
    Number(f32),
    Bool(bool),
    String(String),
    Color(Color),
    Paint(Paint),
    Fills(Vec<Paint>),
    Strokes(Vec<Stroke>),
    Transform(glam::Affine2),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_rgba8_roundtrip() {
        let c = Color::from_rgba8(0x5b, 0x8c, 0xff, 0x3c);
        assert_eq!(c.to_rgba8(), [0x5b, 0x8c, 0xff, 0x3c]);
        assert!(!c.is_opaque());
        assert!(Color::WHITE.is_opaque());
    }

    #[test]
    fn stroke_solid_defaults() {
        let s = Stroke::solid(Color::BLACK, 2.0);
        assert_eq!(s.align, StrokeAlign::Center);
        assert_eq!(s.cap, StrokeCap::Round);
        assert_eq!(s.join, StrokeJoin::Round);
        assert!(s.dash_pattern.is_empty());
    }

    #[test]
    fn constraints_default() {
        let c = Constraints::default();
        assert_eq!(c.horizontal, ConstraintH::Left);
        assert_eq!(c.vertical, ConstraintV::Top);
    }

    #[test]
    fn autolayout_default() {
        let a = AutoLayoutConfig::default();
        assert_eq!(a.direction, LayoutDirection::Horizontal);
        assert_eq!(a.spacing, 0.0);
        assert_eq!(a.padding, [0.0; 4]);
        assert_eq!(a.align_items, LayoutAlign::Stretch);
        assert_eq!(a.justify_content, LayoutJustify::Min);
    }

    #[test]
    fn serialize_color() {
        let json = serde_json::to_string(&Color::from_rgba8(255, 0, 128, 255)).unwrap();
        let back: Color = serde_json::from_str(&json).unwrap();
        assert_eq!(back, Color::from_rgba8(255, 0, 128, 255));
    }
}
