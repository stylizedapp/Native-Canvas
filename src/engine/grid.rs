use glam::Vec2;

/// Настройки сетки и привязки к ней.
#[derive(Clone, Copy, Debug)]
pub struct GridConfig {
    /// Показывать сетку.
    pub visible: bool,
    /// Привязка точек к сетке (плюс всегда к целым пикселям).
    pub snap: bool,
    /// Шаг сетки (в мировых px).
    pub step: f32,
}

impl GridConfig {
    pub fn new() -> Self {
        Self { visible: true, snap: true, step: 8.0 }
    }

    /// Привязка точки: к сетке (если включена), всегда к целым пикселям.
    pub fn snap_point(&self, p: Vec2) -> Vec2 {
        if self.snap {
            let s = self.step.max(1.0);
            Vec2::new((p.x / s).round() * s, (p.y / s).round() * s)
        } else {
            Vec2::new(p.x.round(), p.y.round())
        }
    }

    /// Привязка размера: только к целым пикселям (сетка на размеры не влияет).
    pub fn snap_size(&self, p: Vec2) -> Vec2 {
        Vec2::new(p.x.round(), p.y.round())
    }
}

impl Default for GridConfig {
    fn default() -> Self {
        Self::new()
    }
}