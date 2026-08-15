use super::scene::Scene;

/// История операций на основе снапшотов персистентного графа сцены.
///
/// Клон `Scene` дешёв (im-rs разделяет память), поэтому каждый шаг хранит только
/// изменённые ветки дерева — история почти не нагружает память.
#[derive(Default)]
pub struct History {
    undo: Vec<Scene>,
    redo: Vec<Scene>,
    /// Ограничение глубины истории.
    limit: usize,
}

impl History {
    pub fn new(limit: usize) -> Self {
        Self { undo: Vec::new(), redo: Vec::new(), limit }
    }

    /// Вызывается ПОСЛЕ внесения изменения: сохраняет состояние до изменения.
    pub fn record(&mut self, before: Scene) {
        if self.undo.len() >= self.limit && self.limit > 0 {
            self.undo.remove(0);
        }
        self.undo.push(before);
        // Новое изменение инвалидирует redo-ветку.
        self.redo.clear();
    }

    pub fn undo(&mut self, current: &Scene) -> Option<Scene> {
        if let Some(prev) = self.undo.pop() {
            self.redo.push(current.clone());
            Some(prev)
        } else {
            None
        }
    }

    pub fn redo(&mut self, current: &Scene) -> Option<Scene> {
        if let Some(next) = self.redo.pop() {
            self.undo.push(current.clone());
            Some(next)
        } else {
            None
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
}