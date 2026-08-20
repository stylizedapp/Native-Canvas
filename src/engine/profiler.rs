//! Профилировщик кадра: скользящее окно метрик рендера + рабочий набор RAM.
//!
//! `sync` записывает сюда метрики каждого кадра; кнопка Debug в UI делает
//! `take_snapshot()` для оверлея/лога. RAM читается лениво при вызове снимка.

use std::collections::VecDeque;
use std::time::Instant;

/// Постадийные замеры одного кадра рендера (в микросекундах).
#[derive(Clone, Copy, Debug, Default)]
pub struct FrameMetrics {
    /// Построение сцены: обход дерева + запись draw-calls (vello::Scene).
    pub scene_build_us: u128,
    /// Отправка на GPU: render_to_texture + readback-копия + submit.
    pub gpu_encode_us: u128,
    /// Получение кадра с GPU: poll + копия замапленного буфера в `out`.
    pub gpu_readback_us: u128,
    /// Полное время кадра (замеряется вызывающим, вкл. всё остальное).
    pub total_us: u128,
}

/// Скользящее окно `FrameMetrics` (последние `capacity` кадров) + FPS/RAM.
pub struct FrameProfiler {
    window: VecDeque<(Instant, FrameMetrics)>,
    capacity: usize,
}

impl Default for FrameProfiler {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameProfiler {
    pub fn new() -> Self {
        Self { window: VecDeque::with_capacity(60), capacity: 60 }
    }

    /// Добавляет метрики кадра в окно (после полного рендера).
    pub fn record(&mut self, m: FrameMetrics) {
        self.window.push_back((Instant::now(), m));
        while self.window.len() > self.capacity {
            self.window.pop_front();
        }
    }

    pub fn len(&self) -> usize {
        self.window.len()
    }

    pub fn is_empty(&self) -> bool {
        self.window.is_empty()
    }

    fn avg_us(&self, f: impl Fn(&FrameMetrics) -> u128) -> f64 {
        let n = self.window.len();
        if n == 0 {
            return 0.0;
        }
        let sum: u128 = self.window.iter().map(|(_, m)| f(m)).sum();
        sum as f64 / n as f64
    }

    pub fn avg_scene_build_ms(&self) -> f64 {
        self.avg_us(|m| m.scene_build_us) / 1000.0
    }

    pub fn avg_gpu_encode_ms(&self) -> f64 {
        self.avg_us(|m| m.gpu_encode_us) / 1000.0
    }

    pub fn avg_gpu_readback_ms(&self) -> f64 {
        self.avg_us(|m| m.gpu_readback_us) / 1000.0
    }

    pub fn avg_total_ms(&self) -> f64 {
        self.avg_us(|m| m.total_us) / 1000.0
    }

    /// Кадров в секунду по временным меткам окна (вкл. время простоя между ними).
    pub fn fps(&self) -> f64 {
        if self.window.len() < 2 {
            return 0.0;
        }
        let newest = self.window.back().unwrap().0;
        let oldest = self.window.front().unwrap().0;
        let dt = newest.duration_since(oldest).as_secs_f64();
        if dt <= 0.0 {
            return 0.0;
        }
        self.window.len() as f64 / dt
    }

    /// Рабочий набор процесса (RAM), МБ. Читается при каждом вызове (дёшево).
    pub fn ram_allocated_mb(&self) -> f64 {
        process_working_set_mb()
    }

    /// Снимок метрик для оверлея/лога (кнопка Debug). `extra` дописывается
    /// в конец (например, GL-презентация канваса из `CanvasState`).
    pub fn take_snapshot(&self, extra: &str) -> String {
        format!(
            "FPS {:.0}  |  render {:.2} ms (scene {:.2} + encode {:.2} + readback {:.2})\n\
             RAM {:.0} MB  |  window {} frames\n\
             {extra}",
            self.fps(),
            self.avg_total_ms(),
            self.avg_scene_build_ms(),
            self.avg_gpu_encode_ms(),
            self.avg_gpu_readback_ms(),
            self.ram_allocated_mb(),
            self.window.len(),
        )
    }
}

/// Рабочий набор текущего процесса через GetProcessMemoryInfo.
#[cfg(windows)]
fn process_working_set_mb() -> f64 {
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    let mut pmc = PROCESS_MEMORY_COUNTERS {
        cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
        PageFaultCount: 0,
        PeakWorkingSetSize: 0,
        WorkingSetSize: 0,
        QuotaPeakPagedPoolUsage: 0,
        QuotaPagedPoolUsage: 0,
        QuotaPeakNonPagedPoolUsage: 0,
        QuotaNonPagedPoolUsage: 0,
        PagefileUsage: 0,
        PeakPagefileUsage: 0,
    };
    let ok = unsafe {
        GetProcessMemoryInfo(
            GetCurrentProcess(),
            &mut pmc,
            std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
        )
    };
    if ok != 0 {
        pmc.WorkingSetSize as f64 / (1024.0 * 1024.0)
    } else {
        0.0
    }
}

#[cfg(not(windows))]
fn process_working_set_mb() -> f64 {
    0.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metrics(total: u128) -> FrameMetrics {
        FrameMetrics { scene_build_us: total / 3, gpu_encode_us: total / 3, gpu_readback_us: total / 3, total_us: total }
    }

    #[test]
    fn rolling_window_caps_at_capacity() {
        let mut p = FrameProfiler::new();
        for i in 0..200u128 {
            p.record(metrics(1000 + i));
        }
        assert_eq!(p.len(), 60);
    }

    #[test]
    fn averages_over_window() {
        let mut p = FrameProfiler::new();
        for _ in 0..10 {
            p.record(metrics(3000));
        }
        assert!((p.avg_total_ms() - 3.0).abs() < 1e-9);
        assert!((p.avg_scene_build_ms() - 1.0).abs() < 1e-9);
        assert!((p.avg_gpu_encode_ms() - 1.0).abs() < 1e-9);
        assert!((p.avg_gpu_readback_ms() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn snapshot_is_multiline() {
        let mut p = FrameProfiler::new();
        p.record(metrics(2000));
        p.record(metrics(2000));
        let s = p.take_snapshot("GL: test");
        assert!(s.contains("FPS"));
        assert!(s.contains("RAM"));
        assert!(s.contains("render"));
    }
}