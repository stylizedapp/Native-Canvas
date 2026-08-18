//! Bounding-box gizmo: 8 ресайз-хэндлов, вычисление нового AABB, масштабная
//! трансформация для «не-size» видов и пересечение рамок (для марки).
//!
//! Координаты — мировые; экранный размер хэндлов достигается делением на zoom.

use super::model::nodes::NodeKind;
use glam::{Affine2, Vec2};

/// Один из 8 хэндлов рамки выделения.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Handle {
    Nw,
    N,
    Ne,
    E,
    Se,
    S,
    Sw,
    W,
}

impl Handle {
    /// Строковый идентификатор для UI (маппинг курсора в canvas_view.slint).
    pub fn name(self) -> &'static str {
        match self {
            Handle::Nw => "nw",
            Handle::N => "n",
            Handle::Ne => "ne",
            Handle::E => "e",
            Handle::Se => "se",
            Handle::S => "s",
            Handle::Sw => "sw",
            Handle::W => "w",
        }
    }
}

/// Экранный размер стороны квадрата-хэндла (в мировых координатах = /zoom).
pub const HANDLE_SIZE: f32 = 10.0;

/// Возвращает 8 хэндлов в мировых координатах (AABB `mn..mx`).
pub fn handle_rects(mn: Vec2, mx: Vec2, zoom: f32) -> [(Handle, (Vec2, Vec2)); 8] {
    let h = HANDLE_SIZE / zoom;
    let cx = (mn.x + mx.x) / 2.0;
    let cy = (mn.y + mx.y) / 2.0;
    let centers = [
        (Handle::Nw, Vec2::new(mn.x, mn.y)),
        (Handle::N, Vec2::new(cx, mn.y)),
        (Handle::Ne, Vec2::new(mx.x, mn.y)),
        (Handle::E, Vec2::new(mx.x, cy)),
        (Handle::Se, Vec2::new(mx.x, mx.y)),
        (Handle::S, Vec2::new(cx, mx.y)),
        (Handle::Sw, Vec2::new(mn.x, mx.y)),
        (Handle::W, Vec2::new(mn.x, cy)),
    ];
    centers.map(|(hnd, c)| (hnd, (c - Vec2::splat(h / 2.0), c + Vec2::splat(h / 2.0))))
}

/// Хит-тест хэндла по мировой точке (хэндлы построены в мировых координатах
/// от AABB `mn..mx`; экранная точка должна быть сконвертирована в мировую).
/// Среди всех хэндлов, содержащих точку,
/// выбирается ближайший к её центру: у угловых хэндлов центр дальше от середины
/// стороны, поэтому у края побеждает боковой, а в углу — угловой.
pub fn handle_at(screen: Vec2, mn: Vec2, mx: Vec2, zoom: f32) -> Option<Handle> {
    let mut best: Option<(f32, Handle)> = None;
    for (hnd, (a, b)) in handle_rects(mn, mx, zoom) {
        if screen.x >= a.x && screen.x <= b.x && screen.y >= a.y && screen.y <= b.y {
            let center = (a + b) / 2.0;
            let d = screen.distance_squared(center);
            if best.map(|(bd, _)| d < bd).unwrap_or(true) {
                best = Some((d, hnd));
            }
        }
    }
    best.map(|(_, h)| h)
}

/// Новое AABB из хэндла, исходной рамки и текущей точки. Противоположный край/
/// угол остаётся опорным и не двигается; размер ограничен снизу 1px.
pub fn resize_rect(handle: Handle, start_mn: Vec2, start_mx: Vec2, point: Vec2) -> (Vec2, Vec2) {
    const MIN_S: f32 = 1.0;
    let (mut nmin, mut nmax) = (start_mn, start_mx);
    match handle {
        Handle::Nw => {
            nmin.x = point.x.min(nmax.x - MIN_S);
            nmin.y = point.y.min(nmax.y - MIN_S);
        }
        Handle::N => nmin.y = point.y.min(nmax.y - MIN_S),
        Handle::Ne => {
            nmax.x = point.x.max(nmin.x + MIN_S);
            nmin.y = point.y.min(nmax.y - MIN_S);
        }
        Handle::E => nmax.x = point.x.max(nmin.x + MIN_S),
        Handle::Se => {
            nmax.x = point.x.max(nmin.x + MIN_S);
            nmax.y = point.y.max(nmin.y + MIN_S);
        }
        Handle::S => nmax.y = point.y.max(nmin.y + MIN_S),
        Handle::Sw => {
            nmin.x = point.x.min(nmax.x - MIN_S);
            nmax.y = point.y.max(nmin.y + MIN_S);
        }
        Handle::W => nmin.x = point.x.min(nmax.x - MIN_S),
    }
    (nmin, nmax)
}

/// Трансформация, отображающая локальное AABB `local_mn..local_mx` в мировое
/// `new_mn..new_mx` (равномерно/неравномерно по осям). Для видов без
/// собственного размера (Star, Polygon, VectorPath, Text, Line, ...).
pub fn scale_transform(new_mn: Vec2, new_mx: Vec2, local_mn: Vec2, local_mx: Vec2) -> Affine2 {
    let local_size = (local_mx - local_mn).max(Vec2::splat(1e-6));
    let scale = (new_mx - new_mn) / local_size;
    Affine2::from_translation(new_mn) * Affine2::from_scale(scale) * Affine2::from_translation(-local_mn)
}

/// Пересекаются ли два AABB (для рамки марки).
pub fn aabb_intersect(a0: Vec2, a1: Vec2, b0: Vec2, b1: Vec2) -> bool {
    a0.x <= b1.x && a1.x >= b0.x && a0.y <= b1.y && a1.y >= b0.y
}

/// true, если у вида есть ненулевое локальное AABB (ресайз применим).
pub fn resizable(kind: &NodeKind) -> bool {
    let (lmin, lmax) = kind.local_bbox();
    let s = lmax - lmin;
    s.x.abs() > f32::EPSILON && s.y.abs() > f32::EPSILON
}

#[cfg(test)]
mod tests {
    use super::*;

    const MN: Vec2 = Vec2::new(100.0, 200.0);
    const MX: Vec2 = Vec2::new(300.0, 400.0);

    #[test]
    fn handle_rects_covers_corners() {
        let rects = handle_rects(MN, MX, 1.0);
        assert_eq!(rects.len(), 8);
        for (hnd, (a, b)) in rects {
            let contains = |p: Vec2| p.x >= a.x && p.x <= b.x && p.y >= a.y && p.y <= b.y;
            match hnd {
                Handle::Nw => assert!(contains(MN)),
                Handle::Se => assert!(contains(MX)),
                Handle::Ne => assert!(contains(Vec2::new(MX.x, MN.y))),
                Handle::Sw => assert!(contains(Vec2::new(MN.x, MX.y))),
                _ => {}
            }
        }
    }

    #[test]
    fn handle_at_corner_beats_edge() {
        // Маленький узел: все хэндлы накладываются — ближайший к точке центр.
        let mn = Vec2::new(0.0, 0.0);
        let mx = Vec2::new(4.0, 4.0);
        assert_eq!(handle_at(Vec2::new(0.0, 0.0), mn, mx, 1.0), Some(Handle::Nw));
        assert_eq!(handle_at(Vec2::new(4.0, 4.0), mn, mx, 1.0), Some(Handle::Se));
        assert_eq!(handle_at(Vec2::new(2.0, 0.0), mn, mx, 1.0), Some(Handle::N));
        assert_eq!(handle_at(Vec2::new(4.0, 2.0), mn, mx, 1.0), Some(Handle::E));
        // Крупный узел: боковые хэндлы не перекрываются угловыми.
        let big = Vec2::new(50.0, 50.0);
        assert_eq!(handle_at(Vec2::new(25.0, 0.0), Vec2::ZERO, big, 1.0), Some(Handle::N));
        assert_eq!(handle_at(Vec2::new(50.0, 25.0), Vec2::ZERO, big, 1.0), Some(Handle::E));
        assert_eq!(handle_at(Vec2::new(0.0, 25.0), Vec2::ZERO, big, 1.0), Some(Handle::W));
    }

    #[test]
    fn handle_at_outside_is_none() {
        assert_eq!(handle_at(Vec2::new(500.0, 500.0), MN, MX, 1.0), None);
    }

    #[test]
    fn resize_se_corner() {
        let (mn, mx) = resize_rect(Handle::Se, MN, MX, Vec2::new(340.0, 420.0));
        assert_eq!(mn, MN);
        assert_eq!(mx, Vec2::new(340.0, 420.0));
    }

    #[test]
    fn resize_nw_keeps_se_fixed() {
        let (mn, mx) = resize_rect(Handle::Nw, MN, MX, Vec2::new(80.0, 190.0));
        assert_eq!(mn, Vec2::new(80.0, 190.0));
        assert_eq!(mx, MX);
    }

    #[test]
    fn resize_n_moves_only_top() {
        let (mn, mx) = resize_rect(Handle::N, MN, MX, Vec2::new(123.0, 150.0));
        assert_eq!(mn, Vec2::new(MN.x, 150.0));
        assert_eq!(mx, MX);
    }

    #[test]
    fn resize_e_moves_only_right() {
        let (mn, mx) = resize_rect(Handle::E, MN, MX, Vec2::new(350.0, 250.0));
        assert_eq!(mn, MN);
        assert_eq!(mx, Vec2::new(350.0, MX.y));
    }

    #[test]
    fn resize_sw_moves_left_and_bottom() {
        let (mn, mx) = resize_rect(Handle::Sw, MN, MX, Vec2::new(70.0, 430.0));
        assert_eq!(mn, Vec2::new(70.0, MN.y));
        assert_eq!(mx, Vec2::new(MX.x, 430.0));
    }

    #[test]
    fn resize_clamps_min_size() {
        let (mn, mx) = resize_rect(Handle::Se, MN, MX, Vec2::new(100.5, 200.5));
        assert_eq!(mx.x, MN.x + 1.0);
        assert_eq!(mx.y, MN.y + 1.0);
        assert_eq!(mn, MN);
    }

    #[test]
    fn scale_transform_maps_local_bbox() {
        let local_mn = Vec2::new(-50.0, -30.0);
        let local_mx = Vec2::new(50.0, 30.0);
        let new_mn = Vec2::new(10.0, 20.0);
        let new_mx = Vec2::new(30.0, 50.0);
        let t = scale_transform(new_mn, new_mx, local_mn, local_mx);
        assert_eq!(t.transform_point2(local_mn), new_mn);
        assert_eq!(t.transform_point2(local_mx), new_mx);
    }

    #[test]
    fn aabb_intersect_cases() {
        assert!(aabb_intersect(Vec2::ZERO, Vec2::splat(10.0), Vec2::splat(5.0), Vec2::splat(15.0)));
        assert!(aabb_intersect(Vec2::ZERO, Vec2::splat(10.0), Vec2::splat(10.0), Vec2::splat(20.0)));
        assert!(!aabb_intersect(Vec2::ZERO, Vec2::splat(10.0), Vec2::splat(11.0), Vec2::splat(20.0)));
        assert!(!aabb_intersect(Vec2::ZERO, Vec2::splat(10.0), Vec2::new(0.0, 11.0), Vec2::new(20.0, 20.0)));
    }

    #[test]
    fn resizable_kinds() {
        use crate::engine::model::nodes::{NodeKind, ShapeKind};

        let rect = NodeKind::Shape(ShapeKind::Rectangle { size: Vec2::splat(50.0), corner_radii: [0.0; 4] });
        assert!(resizable(&rect));

        let star = NodeKind::Shape(ShapeKind::Star { radius: 20.0, inner_radius_ratio: 0.5, point_count: 5 });
        assert!(resizable(&star));

        let group = NodeKind::Group;
        assert!(!resizable(&group));
    }
}