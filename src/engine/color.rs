//! Преобразования цветовых пространств (RGB/HSV/HSL) и парсинг HEX-строк.
//!
//! Каналы RGB/HSV/HSL — f32 (0..1), оттенок — градусы (0..360).
//! Используется инспектором (color picker) и текстовым вводом HEX.

use crate::engine::model::types::Color;

/// Парсит "#RRGGBB" или "#RRGGBBAA" (символ '#' необязателен).
pub fn parse_hex(s: &str) -> Option<Color> {
    let s = s.trim().trim_start_matches('#');
    let rgb = match s.len() {
        6 => {
            let r = u8::from_str_radix(&s[0..2], 16).ok()?;
            let g = u8::from_str_radix(&s[2..4], 16).ok()?;
            let b = u8::from_str_radix(&s[4..6], 16).ok()?;
            [r, g, b, 255]
        }
        8 => {
            let r = u8::from_str_radix(&s[0..2], 16).ok()?;
            let g = u8::from_str_radix(&s[2..4], 16).ok()?;
            let b = u8::from_str_radix(&s[4..6], 16).ok()?;
            let a = u8::from_str_radix(&s[6..8], 16).ok()?;
            [r, g, b, a]
        }
        _ => return None,
    };
    Some(Color::from_rgba8(rgb[0], rgb[1], rgb[2], rgb[3]))
}

/// "#RRGGBBAA" из цвета (всегда 8 цифр, прозрачность явная).
pub fn to_hex(c: Color) -> String {
    let [r, g, b, a] = c.to_rgba8();
    format!("#{:02X}{:02X}{:02X}{:02X}", r, g, b, a)
}

/// Чистый оттенок (s=1, v=1) как цвет — для градиента SV-области.
pub fn hue_color(h_deg: f32) -> Color {
    let (r, g, b) = hsv_to_rgb(h_deg, 1.0, 1.0);
    Color::from_rgba8(r, g, b, 255)
}

/// HSV -> RGB (u8). `h` — градусы, `s`/`v` — 0..1.
pub fn hsv_to_rgb(h: f32, s: f32, v: f32) -> (u8, u8, u8) {
    let h = (h % 360.0 + 360.0) % 360.0;
    let s = s.clamp(0.0, 1.0);
    let v = v.clamp(0.0, 1.0);
    let c = v * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = v - c;
    let (r, g, b) = match (h / 60.0).floor() as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    (
        ((r + m) * 255.0).round() as u8,
        ((g + m) * 255.0).round() as u8,
        ((b + m) * 255.0).round() as u8,
    )
}

/// RGB (u8) -> HSV. Возвращает (h в градусах 0..360, s 0..1, v 0..1).
pub fn rgb_to_hsv(r: u8, g: u8, b: u8) -> (f32, f32, f32) {
    let r = r as f32 / 255.0;
    let g = g as f32 / 255.0;
    let b = b as f32 / 255.0;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    let h = if d == 0.0 {
        0.0
    } else if max == r {
        ((g - b) / d) % 6.0
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    let h = h * 60.0;
    let h = if h < 0.0 { h + 360.0 } else { h };
    let s = if max == 0.0 { 0.0 } else { d / max };
    (h, s, max)
}

/// HSL -> RGB (u8). `h` — градусы, `s`/`l` — 0..1.
pub fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (u8, u8, u8) {
    let h = (h % 360.0 + 360.0) % 360.0;
    let s = s.clamp(0.0, 1.0);
    let l = l.clamp(0.0, 1.0);
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = l - c / 2.0;
    let (r, g, b) = match (h / 60.0).floor() as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    (
        ((r + m) * 255.0).round() as u8,
        ((g + m) * 255.0).round() as u8,
        ((b + m) * 255.0).round() as u8,
    )
}

/// RGB (u8) -> HSL. Возвращает (h 0..360, s 0..1, l 0..1).
pub fn rgb_to_hsl(r: u8, g: u8, b: u8) -> (f32, f32, f32) {
    let r = r as f32 / 255.0;
    let g = g as f32 / 255.0;
    let b = b as f32 / 255.0;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    let d = max - min;
    if d == 0.0 {
        return (0.0, 0.0, l);
    }
    let s = if l > 0.5 { d / (2.0 - max - min) } else { d / (max + min) };
    let h = if max == r {
        ((g - b) / d) % 6.0
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    let h = h * 60.0;
    (if h < 0.0 { h + 360.0 } else { h }, s, l)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_roundtrip() {
        let c = parse_hex("#FF8000").unwrap();
        assert_eq!(to_hex(c), "#FF8000FF");
        assert_eq!(parse_hex("ff8000ff").unwrap().to_rgba8(), [255, 128, 0, 255]);
        assert_eq!(parse_hex("zzz"), None);
        assert_eq!(parse_hex("#12345"), None);
    }

    #[test]
    fn hsv_roundtrip() {
        // Красный.
        assert_eq!(hsv_to_rgb(0.0, 1.0, 1.0), (255, 0, 0));
        assert_eq!(rgb_to_hsv(255, 0, 0), (0.0, 1.0, 1.0));
        // Оранжевый (30°, s=1, v=1).
        assert_eq!(hsv_to_rgb(30.0, 1.0, 1.0), (255, 128, 0));
        // Серый: s=0, оттенок не важен.
        let (h, s, v) = rgb_to_hsv(128, 128, 128);
        assert_eq!(s, 0.0);
        assert!((v - 128.0 / 255.0).abs() < 0.001);
        assert!(h >= 0.0 && h < 360.0);
    }

    #[test]
    fn hsl_roundtrip() {
        assert_eq!(hsl_to_rgb(0.0, 1.0, 0.5), (255, 0, 0));
        let (h, s, l) = rgb_to_hsl(255, 128, 0);
        assert!((h - 30.0).abs() < 0.5, "h={h}");
        assert!((s - 1.0).abs() < 0.01);
        assert!((l - 0.5).abs() < 0.01);
    }
}