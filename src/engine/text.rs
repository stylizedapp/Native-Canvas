//! Текст-движок для текстовых узлов: измерения и контуры глифов.
//!
//! Рендер строится на ВЕКТОРНЫХ контурах глифов (а не на растеризации):
//! оба бэкенда (tiny-skia и vello) умеют заливать пути, поэтому текстовый
//! узел рисуется одинаково везде. Шрифт — встроенный Inter
//! (`assets/fonts/inter.ttf`), загружается один раз через `OnceLock`.
//!
//! v1: только режим AutoWidth (перенос строк по `\n`, без word-wrap);
//! Fixed/AutoHeight ведут себя как AutoWidth. Шрифт единственный — Inter,
//! `font_family`/`font_weight` хранятся в модели, но пока рендер — regular.

use super::model::nodes::NodeKind;
use super::model::types::TextAlign;
use ab_glyph::{Font, FontArc, GlyphId, Outline, OutlineCurve, PxScale, ScaleFont};
use glam::Vec2;
use kurbo::BezPath;
use std::sync::OnceLock;

/// Встроенный шрифт (Inter). Держим рядом с UI-ассетом — один файл на всё.
const FONT_BYTES: &[u8] = include_bytes!("../../assets/fonts/inter.ttf");

/// Строка раскладки: глифы уже в АБСОЛЮТНЫХ локальных координатах узла
/// (с учётом выравнивания).
pub struct LayoutLine {
    pub glyphs: Vec<BezPath>,
}

/// Один экземпляр шрифта на процесс.
pub fn font() -> &'static FontArc {
    static FONT: OnceLock<FontArc> = OnceLock::new();
    FONT.get_or_init(|| {
        FontArc::try_from_vec(FONT_BYTES.to_vec()).expect("embedded Inter font must parse")
    })
}

/// Параметры текста, извлечённые из узла (с безопасными дефолтами).
fn params(kind: &NodeKind) -> (&str, f32, f32, f32, TextAlign) {
    let (content, font_size, line_height, letter_spacing, align) = match kind {
        NodeKind::Text {
            content,
            font_size,
            line_height,
            letter_spacing,
            align,
            ..
        } => (content.as_str(), *font_size, *line_height, *letter_spacing, *align),
        _ => return ("", 16.0, 1.2, 0.0, TextAlign::Left),
    };
    (
        content,
        font_size.max(1.0),
        if line_height > 0.0 { line_height } else { 1.2 },
        letter_spacing,
        align,
    )
}

/// Измеряет все строки: возвращает их ширины в порядке следования.
fn line_widths(content: &str, fs: f32, ls: f32) -> (Vec<f32>, f32) {
    let font = font();
    let sf = font.as_scaled(PxScale::from(fs));
    let mut widths: Vec<f32> = Vec::new();
    let mut w = 0.0f32;
    let mut prev: Option<GlyphId> = None;
    for c in content.chars() {
        if c == '\n' {
            widths.push(w);
            w = 0.0;
            prev = None;
            continue;
        }
        let id = sf.glyph_id(c);
        let mut adv = sf.h_advance(id);
        if let Some(p) = prev {
            adv += sf.kern(p, id);
        }
        if w > 0.0 {
            adv += ls;
        }
        w += adv.max(0.0);
        prev = Some(id);
    }
    widths.push(w);
    let max_w = widths.iter().cloned().fold(0.0f32, f32::max);
    (widths, max_w)
}

/// Локальные размеры текстового узла (ширина = самая широкая строка).
pub fn measure(kind: &NodeKind) -> Vec2 {
    let (content, fs, lh, ls, _) = params(kind);
    let (_, max_w) = line_widths(content, fs, ls);
    let lines = content.chars().filter(|&c| c == '\n').count() + 1;
    Vec2::new(max_w, lines as f32 * fs * lh)
}

/// Раскладка текста: строки с векторными контурами глифов.
pub fn layout(kind: &NodeKind) -> Vec<LayoutLine> {
    let (content, fs, lh, ls, align) = params(kind);
    let font = font();
    let sf = font.as_scaled(PxScale::from(fs));
    let hf = sf.h_scale_factor();
    let vf = sf.v_scale_factor();
    let ascent = sf.ascent();

    let (line_widths, max_w) = line_widths(content, fs, ls);
    let line_h = fs * lh;

    let mut out: Vec<LayoutLine> = Vec::new();
    let mut glyphs: Vec<BezPath> = Vec::new();
    let mut x = 0.0f32;
    let mut prev: Option<GlyphId> = None;
    let mut line_index = 0usize;

    for c in content.chars() {
        if c == '\n' {
            push_line(
                &mut out,
                &mut glyphs,
                &mut x,
                &mut prev,
                line_index,
                &line_widths,
                max_w,
                align,
            );
            line_index += 1;
            continue;
        }
        let id = sf.glyph_id(c);
        let mut adv = sf.h_advance(id);
        if let Some(p) = prev {
            adv += sf.kern(p, id);
        }
        if x > 0.0 {
            adv += ls;
        }

        let baseline_y = line_index as f32 * line_h + ascent;
        if let Some(outline) = font.outline(id) {
            glyphs.push(outline_to_bez(&outline, hf, vf, x, baseline_y));
        }
        x += adv.max(0.0);
        prev = Some(id);
    }
    push_line(
        &mut out,
        &mut glyphs,
        &mut x,
        &mut prev,
        line_index,
        &line_widths,
        max_w,
        align,
    );

    out
}

/// Финализирует строку: применяет выравнивание и помещает в результат.
#[allow(clippy::too_many_arguments)]
fn push_line(
    out: &mut Vec<LayoutLine>,
    glyphs: &mut Vec<BezPath>,
    x: &mut f32,
    prev: &mut Option<GlyphId>,
    line_index: usize,
    line_widths: &[f32],
    max_w: f32,
    align: TextAlign,
) {
    let width = line_widths.get(line_index).copied().unwrap_or(0.0);
    let offset_x = match align {
        TextAlign::Left | TextAlign::Justified => 0.0,
        TextAlign::Center => (max_w - width) * 0.5,
        TextAlign::Right => max_w - width,
    };
    if offset_x != 0.0 {
        for g in glyphs.iter_mut() {
            *g = shift_path(std::mem::take(g), offset_x);
        }
    }
    out.push(LayoutLine { glyphs: std::mem::take(glyphs) });
    *x = 0.0;
    *prev = None;
}

/// Перенос контура на dx.
fn shift_path(path: BezPath, dx: f32) -> BezPath {
    if dx == 0.0 {
        return path;
    }
    let dx = dx as f64;
    let mut shifted = BezPath::new();
    for el in path.elements() {
        match *el {
            kurbo::PathEl::MoveTo(p) => shifted.move_to((p.x + dx, p.y)),
            kurbo::PathEl::LineTo(p) => shifted.line_to((p.x + dx, p.y)),
            kurbo::PathEl::QuadTo(c, p) => shifted.quad_to((c.x + dx, c.y), (p.x + dx, p.y)),
            kurbo::PathEl::CurveTo(c1, c2, p) => {
                shifted.curve_to((c1.x + dx, c1.y), (c2.x + dx, c2.y), (p.x + dx, p.y))
            }
            kurbo::PathEl::ClosePath => shifted.close_path(),
        }
    }
    shifted
}

/// Конвертирует несмасштабированный контур глифа в kurbo-контур.
///
/// Координаты font-юнитов (ось Y вверх) переводятся в пиксельное пространство
/// (ось Y вниз) с базовой линией в `baseline_y`:
/// `x = p.x * hf + pen_x`, `y = -(p.y * vf) + baseline_y`.
fn outline_to_bez(outline: &Outline, hf: f32, vf: f32, pen_x: f32, baseline_y: f32) -> BezPath {
    let mut path = BezPath::new();
    let mut last: Option<(f64, f64)> = None;
    for curve in &outline.curves {
        let p0 = map_point(curve_start(curve), hf, vf, pen_x, baseline_y);
        let p1 = map_point(curve_end(curve), hf, vf, pen_x, baseline_y);
        let disconnected = match last {
            Some((lx, ly)) => (lx - p0.0).abs() > 0.05 || (ly - p0.1).abs() > 0.05,
            None => true,
        };
        if disconnected {
            path.move_to(p0);
        }
        match curve {
            OutlineCurve::Line(..) => path.line_to(p1),
            OutlineCurve::Quad(_, c, _) => {
                let c = map_point(c, hf, vf, pen_x, baseline_y);
                path.quad_to(c, p1);
            }
            OutlineCurve::Cubic(_, c1, c2, _) => {
                let c1 = map_point(c1, hf, vf, pen_x, baseline_y);
                let c2 = map_point(c2, hf, vf, pen_x, baseline_y);
                path.curve_to(c1, c2, p1);
            }
        }
        last = Some(p1);
    }
    if !path.is_empty() {
        path.close_path();
    }
    path
}

fn map_point(p: &ab_glyph::Point, hf: f32, vf: f32, pen_x: f32, baseline_y: f32) -> (f64, f64) {
    (p.x as f64 * hf as f64 + pen_x as f64, -(p.y as f64) * vf as f64 + baseline_y as f64)
}

fn curve_start(curve: &OutlineCurve) -> &ab_glyph::Point {
    match curve {
        OutlineCurve::Line(p0, _) | OutlineCurve::Quad(p0, ..) | OutlineCurve::Cubic(p0, ..) => p0,
    }
}

fn curve_end(curve: &OutlineCurve) -> &ab_glyph::Point {
    match curve {
        OutlineCurve::Line(_, p1) => p1,
        OutlineCurve::Quad(_, _, p1) => p1,
        OutlineCurve::Cubic(_, _, _, p1) => p1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::model::types::TextSizeMode;
    use kurbo::Shape;

    fn text_kind(content: &str) -> NodeKind {
        NodeKind::Text {
            content: content.into(),
            font_family: "Inter".into(),
            font_size: 16.0,
            font_weight: 400,
            line_height: 1.2,
            letter_spacing: 0.0,
            align: TextAlign::Left,
            size_mode: TextSizeMode::AutoWidth,
        }
    }

    #[test]
    fn measure_non_empty_positive() {
        let m = measure(&text_kind("Hello"));
        assert!(m.x > 0.0 && m.y > 0.0);
        assert!((m.y - 16.0 * 1.2).abs() < 0.01);
    }

    #[test]
    fn measure_empty_is_zero_width() {
        let m = measure(&text_kind(""));
        assert_eq!(m.x, 0.0);
        assert!((m.y - 16.0 * 1.2).abs() < 0.01);
    }

    #[test]
    fn newline_increases_height_and_keeps_max_width() {
        let single = measure(&text_kind("abc"));
        let multi = measure(&text_kind("abc\ndef"));
        assert!((multi.y - 2.0 * 16.0 * 1.2).abs() < 0.01);
        assert!((multi.x - single.x).abs() < 0.01);
    }

    #[test]
    fn layout_splits_lines_and_produces_glyphs() {
        let lines = layout(&text_kind("ab\ncd"));
        assert_eq!(lines.len(), 2);
        assert!(lines[0].glyphs.len() >= 1);
        assert!(lines[1].glyphs.len() >= 1);
    }

    #[test]
    fn center_alignment_shifts_short_line() {
        let left = layout(&text_kind("a\nbb"));
        let mut kind = text_kind("a\nbb");
        if let NodeKind::Text { align, .. } = &mut kind {
            *align = TextAlign::Center;
        }
        let center = layout(&kind);
        let left_min = line_min_x(&left[0]);
        let center_min = line_min_x(&center[0]);
        assert!(center_min > left_min);
    }

    fn line_min_x(line: &LayoutLine) -> f32 {
        line.glyphs
            .iter()
            .map(|g| g.bounding_box().min_x() as f32)
            .fold(f32::MAX, f32::min)
    }

    #[test]
    fn glyph_outline_is_non_empty() {
        let outline = font().outline(font().glyph_id('A'));
        assert!(outline.is_some());
        assert!(!outline.unwrap().curves.is_empty());
    }
}