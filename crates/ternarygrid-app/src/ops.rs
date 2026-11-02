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
use ternarygrid_ui::kit;
use ternarygrid_ui::parts::{self, Dialog};
use ternarygrid_ui::style;
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

    /// The operations window beside the rail's button (`at`: its bottom-left
    /// corner); clears [`Operations::panel_open`] when it is closed.
    pub fn window(&mut self, ctx: &egui::Context, at: egui::Pos2) {
        if !self.panel_open {
            return;
        }
        let title = tr("Background operations");
        let shown = egui::Window::new(title)
            .id(egui::Id::new("ternarygrid_operations"))
            .title_bar(false)
            .resizable(false)
            .collapsible(false)
            .frame(crate::dialogs::window_frame(ctx))
            .pivot(egui::Align2::LEFT_BOTTOM)
            .default_pos(at)
            .show(ctx, |ui| Dialog::new(title).icon(icon::CLOCK_COUNTER_CLOCKWISE).width(420.0).body_padding(egui::Margin::ZERO).contents(ui, |ui| self.ui(ui), None).0);
        let closed = shown.as_ref().and_then(|s| s.inner).unwrap_or(false);
        if closed || crate::widgets::escape_closes(ctx, shown.map(|s| s.response.layer_id)) {
            self.panel_open = false;
        }
    }

    /// Operations panel content: the number running, then one row per operation (the
    /// running ones first).
    pub fn ui(&mut self, ui: &mut Ui) {
        self.collect();
        let p = style::pal(ui);
        let line = if ui.visuals().dark_mode { egui::Color32::from_rgb(0x33, 0x35, 0x3e) } else { egui::Color32::from_rgb(0xed, 0xed, 0xf2) };
        ui.spacing_mut().item_spacing.y = 0.0;
        if self.list.is_empty() {
            egui::Frame::new().inner_margin(egui::Margin::symmetric(16, 18)).show(ui, |ui| {
                ui.label(RichText::new(tr("No background operations.")).color(p.text_muted));
            });
            return;
        }
        let mut clear = false;
        egui::Frame::new().inner_margin(egui::Margin::symmetric(12, 10)).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.set_min_height(26.0);
                ui.label(RichText::new(trf("{} running", &[&self.running()])).font(style::semibold(ui.ctx(), 13.0)).color(p.text));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if kit::ghost(ui, None, tr("Clear finished")).clicked() {
                        clear = true;
                    }
                });
            });
        });
        parts::hline(ui);
        let order: Vec<usize> = (0..self.list.len()).filter(|i| self.list[*i].is_running()).chain((0..self.list.len()).filter(|i| !self.list[*i].is_running())).collect();
        egui::ScrollArea::vertical().max_height(360.0).auto_shrink([false, true]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for i in order {
                let op = &self.list[i];
                let running = matches!(op.state, OpState::Running);
                let (glyph, color, txt) = match &op.state {
                    OpState::Running => (icon::CLOCK_COUNTER_CLOCKWISE, p.accent_text, String::new()),
                    OpState::Succeeded(m) => (icon::CHECK_CIRCLE, p.ok, m.clone()),
                    OpState::Failed(m) => (icon::X_CIRCLE, p.err, m.clone()),
                    OpState::Cancelled => (icon::PROHIBIT, p.text_faint, tr("Cancelled").to_string()),
                };
                let row = egui::Frame::new().inner_margin(egui::Margin::symmetric(12, 10)).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal_top(|ui| {
                        ui.spacing_mut().item_spacing.x = 10.0;
                        let (r, _) = ui.allocate_exact_size(egui::vec2(16.0, 17.0), egui::Sense::hover());
                        ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, glyph, style::icon_font(16.0), color);
                        let cancel_w = if running { 34.0 } else { 0.0 };
                        let w = (ui.available_width() - cancel_w).max(80.0);
                        ui.allocate_ui_with_layout(egui::vec2(w, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                            ui.set_width(w);
                            ui.spacing_mut().item_spacing.y = 5.0;
                            // Rows as tall as their text (the title, the progress line).
                            ui.spacing_mut().interact_size.y = 0.0;
                            let elapsed = op.finished.unwrap_or_else(Instant::now).duration_since(op.started);
                            ui.horizontal(|ui| {
                                let time = format!("{:.1}s", elapsed.as_secs_f32());
                                let time_w = ui.painter().layout_no_wrap(time.clone(), egui::FontId::proportional(11.5), p.text_faint).size().x;
                                let title_w = (ui.available_width() - time_w - 8.0).max(40.0);
                                ui.allocate_ui_with_layout(egui::vec2(title_w, 0.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                                    ui.set_min_width(title_w);
                                    ui.add(egui::Label::new(RichText::new(&op.title).font(style::semibold(ui.ctx(), 13.0)).color(p.text)).truncate());
                                });
                                ui.label(RichText::new(time).size(11.5).color(p.text_faint));
                            });
                            let faint = |ui: &mut Ui, s: &str| ui.add(egui::Label::new(RichText::new(s).size(11.5).color(p.text_faint)).wrap());
                            if running {
                                let done = op.progress.done.load(Ordering::Relaxed);
                                let total = op.progress.total.load(Ordering::Relaxed);
                                let msg = op.progress.message.lock().clone();
                                let mut detail = if total > 0 {
                                    format!("{} / {}", group(done), group(total))
                                } else if done > 0 {
                                    group(done)
                                } else {
                                    String::new()
                                };
                                if !msg.is_empty() {
                                    if !detail.is_empty() {
                                        detail.push_str(" · ");
                                    }
                                    detail.push_str(&msg);
                                }
                                // The bar takes what the detail leaves (empty while the total is unknown).
                                ui.horizontal(|ui| {
                                    ui.spacing_mut().item_spacing.x = 8.0;
                                    let g = ui.painter().layout_no_wrap(detail.clone(), egui::FontId::proportional(11.5), p.text_faint);
                                    let bar_w = (ui.available_width() - g.size().x - 8.0).max(60.0);
                                    let frac = if total > 0 { (done as f32 / total as f32).clamp(0.0, 1.0) } else { 0.0 };
                                    parts::progress(ui, frac, bar_w);
                                    if !detail.is_empty() {
                                        ui.add(egui::Label::new(RichText::new(detail).size(11.5).color(p.text_faint)).truncate());
                                    }
                                });
                            } else if !txt.is_empty() {
                                faint(ui, &txt);
                            }
                        });
                        if running && kit::icon_button_ex(ui, icon::STOP, tr("Cancel"), false, 24.0).clicked() {
                            op.cancel.cancel();
                        }
                    });
                });
                let r = row.response.rect;
                ui.painter().hline(r.x_range(), r.bottom() + 0.5, egui::Stroke::new(1.0, line));
                ui.add_space(1.0);
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

/// `48210` as `48,210`.
fn group(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grouped_counts() {
        assert_eq!(group(0), "0");
        assert_eq!(group(999), "999");
        assert_eq!(group(48_210), "48,210");
        assert_eq!(group(1_234_567), "1,234,567");
    }

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
