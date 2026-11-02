//! Long running background operations (exports, imports, compares, backups,
//! fetch-all ...). Every operation shows up in the operations panel with
//! progress and a cancel button (PRD §10: "every long running operation is
//! shown in the background task panel and can be cancelled").

use egui::{RichText, Ui};
use egui_phosphor::regular as icon;
use parking_lot::Mutex;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use ternarygrid_core::{tr, trf};
use tokio_util::sync::CancellationToken;

/// Progress shared between the background task and the UI.
#[derive(Default)]
pub struct OpProgress {
    pub done: AtomicU64,
    /// 0 = unknown
    pub total: AtomicU64,
    pub message: Mutex<String>,
}

impl OpProgress {
    pub fn set(&self, done: u64, total: Option<u64>) {
        self.done.store(done, Ordering::Relaxed);
        if let Some(t) = total {
            self.total.store(t, Ordering::Relaxed);
        }
    }
    pub fn set_message(&self, m: impl Into<String>) {
        *self.message.lock() = m.into();
    }
}

/// Handle given to the task performing the work.
#[derive(Clone)]
pub struct OpHandle {
    pub id: u64,
    pub progress: Arc<OpProgress>,
    pub cancel: CancellationToken,
    outcome: Arc<Mutex<Option<OpState>>>,
}

impl OpHandle {
    /// End the operation from the task that does it: the operations list takes
    /// the outcome by itself, also when the view that started it is gone. The
    /// first outcome counts.
    pub fn finish(&self, state: OpState) {
        let mut o = self.outcome.lock();
        if o.is_none() {
            *o = Some(state);
        }
    }
}

#[derive(Debug, Clone)]
pub enum OpState {
    Running,
    Succeeded(String),
    Failed(String),
    Cancelled,
}

pub struct Operation {
    pub id: u64,
    pub title: String,
    pub started: Instant,
    pub finished: Option<Instant>,
    pub progress: Arc<OpProgress>,
    pub cancel: CancellationToken,
    pub state: OpState,
    /// Set through [`OpHandle::finish`].
    outcome: Arc<Mutex<Option<OpState>>>,
}

impl Operation {
    fn is_running(&self) -> bool {
        matches!(self.state, OpState::Running) && self.outcome.lock().is_none()
    }
}

#[derive(Default)]
pub struct Operations {
    list: Vec<Operation>,
    next_id: u64,
    pub panel_open: bool,
}

impl Operations {
    pub fn start(&mut self, title: impl Into<String>) -> OpHandle {
        self.collect();
        self.next_id += 1;
        let progress = Arc::new(OpProgress::default());
        let cancel = CancellationToken::new();
        let outcome = Arc::new(Mutex::new(None));
        self.list.push(Operation {
            id: self.next_id,
            title: title.into(),
            started: Instant::now(),
            finished: None,
            progress: progress.clone(),
            cancel: cancel.clone(),
            state: OpState::Running,
            outcome: outcome.clone(),
        });
        if self.list.len() > 100 {
            // drop the oldest finished entries
            if let Some(pos) = self.list.iter().position(|o| !matches!(o.state, OpState::Running)) {
                self.list.remove(pos);
            }
        }
        OpHandle { id: self.next_id, progress, cancel, outcome }
    }

    /// Take the outcomes the tasks set ([`OpHandle::finish`]).
    pub fn collect(&mut self) {
        for op in &mut self.list {
            if matches!(op.state, OpState::Running)
                && let Some(state) = op.outcome.lock().take()
            {
                op.state = state;
                op.finished = Some(Instant::now());
            }
        }
    }

    pub fn finish(&mut self, id: u64, state: OpState) {
        if let Some(op) = self.list.iter_mut().find(|o| o.id == id) {
            op.state = state;
            op.finished = Some(Instant::now());
        }
    }

    /// State of the operation `id` (outcomes set by its task not yet [collected](Self::collect)).
    pub fn state(&self, id: u64) -> Option<&OpState> {
        self.list.iter().find(|o| o.id == id).map(|o| &o.state)
    }

    pub fn running(&self) -> usize {
        self.list.iter().filter(|o| o.is_running()).count()
    }

    pub fn cancel_all(&self) {
        for o in &self.list {
            if o.is_running() {
                o.cancel.cancel();
            }
        }
    }

    /// Operations panel content.
    pub fn ui(&mut self, ui: &mut Ui) {
        self.collect();
        if self.list.is_empty() {
            ui.weak(tr("No background operations."));
            return;
        }
        let mut clear = false;
        ui.horizontal(|ui| {
            ui.strong(trf("{} running", &[&self.running()]));
            if ui.small_button(tr("Clear finished")).clicked() {
                clear = true;
            }
        });
        ui.separator();
        egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
            for op in self.list.iter().rev() {
                ui.horizontal(|ui| {
                    let (glyph, txt) = match &op.state {
                        OpState::Running => (icon::HOURGLASS, String::new()),
                        OpState::Succeeded(m) => (icon::CHECK_CIRCLE, m.clone()),
                        OpState::Failed(m) => (icon::X_CIRCLE, m.clone()),
                        OpState::Cancelled => (icon::PROHIBIT, tr("Cancelled").to_string()),
                    };
                    ui.label(glyph);
                    ui.vertical(|ui| {
                        ui.label(RichText::new(&op.title).strong());
                        let elapsed = op.finished.unwrap_or_else(Instant::now).duration_since(op.started);
                        let done = op.progress.done.load(Ordering::Relaxed);
                        let total = op.progress.total.load(Ordering::Relaxed);
                        if matches!(op.state, OpState::Running) {
                            if total > 0 {
                                ui.add(egui::ProgressBar::new((done as f32 / total as f32).clamp(0.0, 1.0)).desired_width(220.0).text(format!("{done} / {total}")));
                            } else {
                                ui.horizontal(|ui| {
                                    ui.spinner();
                                    if done > 0 {
                                        ui.label(done.to_string());
                                    }
                                });
                            }
                            let msg = op.progress.message.lock().clone();
                            if !msg.is_empty() {
                                ui.small(msg);
                            }
                        } else if !txt.is_empty() {
                            ui.small(txt);
                        }
                        ui.small(format!("{:.1}s", elapsed.as_secs_f32()));
                    });
                    if matches!(op.state, OpState::Running) && ui.button(ternarygrid_icons::ic(icon::STOP)).on_hover_text(tr("Cancel")).clicked() {
                        op.cancel.cancel();
                    }
                });
                ui.separator();
            }
        });
        if clear {
            self.list.retain(|o| matches!(o.state, OpState::Running));
        }
        if self.running() > 0 {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_task_ends_its_operation() {
        let mut ops = Operations::default();
        let a = ops.start("a");
        let b = ops.start("b");
        assert_eq!(ops.running(), 2);
        // Ended by the task, nobody else polling it.
        a.finish(OpState::Succeeded("1 rows".into()));
        a.finish(OpState::Cancelled);
        assert_eq!(ops.running(), 1);
        ops.collect();
        assert!(matches!(ops.list[0].state, OpState::Succeeded(_)) && ops.list[0].finished.is_some());
        // Finished by the view first: the task's later outcome does not change it.
        ops.finish(b.id, OpState::Cancelled);
        b.finish(OpState::Succeeded("2 rows".into()));
        ops.collect();
        assert!(matches!(ops.list[1].state, OpState::Cancelled));
        assert_eq!(ops.running(), 0);
    }
}
