//! Pending transactions: every session with an open transaction (SQL
//! editors in manual commit mode, the navigator / data editor session of a
//! connection), with commit and rollback per session or for all.

use crate::backend::{Backend, Job};
use crate::conn::ConnectionManager;
use crate::conn::session::SessionHandle;
use crate::dialogs::{self, Column, W, table};
use crate::tabs::Tab;
use crate::toasts::Toasts;
use egui::RichText;
use egui_dock::DockState;
use egui_phosphor::regular as icon;
use std::cell::Cell;
use ternarygrid_core::{DbResult, tr, trf, trn};
use ternarygrid_ui::kit::{self, Kind};
use ternarygrid_ui::parts::Dialog;
use ternarygrid_ui::style;

/// One session with uncommitted work.
#[derive(Clone)]
pub struct PendingTx {
    pub conn: String,
    pub conn_name: String,
    /// The tab (or "Navigator / data editor") owning the session.
    pub owner: String,
    pub session: SessionHandle,
    /// Writing statements in the transaction (its reads are nothing to commit).
    pub changes: usize,
    pub since: Option<chrono::DateTime<chrono::Local>>,
}

/// The sessions with an open transaction.
pub fn collect(dock: &DockState<Tab>, conns: &ConnectionManager) -> Vec<PendingTx> {
    let mut out: Vec<PendingTx> = Vec::new();
    let mut push = |conn: &str, owner: String, s: SessionHandle| {
        if s.is_closed() || out.iter().any(|p| p.session.same(&s)) {
            return;
        }
        // Only reads (manual commit mode opens a transaction for a SELECT too): nothing to lose.
        if !s.has_uncommitted_writes() {
            return;
        }
        let tx = s.tx_state();
        let conn_name = conns.config(conn).map(|c| c.name.clone()).unwrap_or_else(|| conn.to_string());
        out.push(PendingTx { conn: conn.to_string(), conn_name, owner, changes: tx.pending_writes(), since: tx.log.first().map(|e| e.at), session: s });
    };
    for (_, tab) in dock.iter_all_tabs() {
        if let (Some(conn), Some(s)) = (tab.view.connection_id(), tab.view.transaction_session()) {
            push(conn, tab.view.title(), s);
        }
    }
    let mut ids = conns.live_ids();
    ids.sort();
    for id in ids {
        let Some(live) = conns.live(&id) else { continue };
        push(&id, tr("Navigator / data editor").to_string(), live.main.clone());
        for (db, s) in &live.db_sessions {
            if let Some(s) = s.get() {
                push(&id, format!("{} ({db})", tr("Navigator / data editor")), s.clone());
            }
        }
    }
    out
}

#[derive(Default)]
pub struct PendingTxDialog {
    pub open: bool,
    jobs: Vec<(String, bool, Job<DbResult<()>>)>,
}

impl PendingTxDialog {
    /// A commit or rollback is running.
    pub fn busy(&self) -> bool {
        !self.jobs.is_empty()
    }

    /// Commit every transaction of `items` (in the background; a failure is a toast and its
    /// transaction stays open).
    pub fn commit_all(&mut self, backend: &Backend, items: &[PendingTx]) {
        for p in items {
            self.end(backend, p, true);
        }
    }

    fn end(&mut self, backend: &Backend, p: &PendingTx, commit: bool) {
        let s = p.session.clone();
        let job = backend.spawn(async move { if commit { s.commit().await } else { s.rollback().await } });
        self.jobs.push((p.conn_name.clone(), commit, job));
    }

    /// Finish the commits and rollbacks that are done (errors become toasts).
    pub fn poll(&mut self, toasts: &mut Toasts) {
        self.jobs.retain_mut(|(name, commit, job)| match job.poll() {
            Some(Ok(())) => false,
            Some(Err(e)) => {
                let title = if *commit { trf("Commit failed: {}", &[name]) } else { trf("Rollback failed: {}", &[name]) };
                toasts.error(title, Some(e.full_text()));
                false
            }
            None => true,
        });
    }

    pub fn show(&mut self, ctx: &egui::Context, backend: &Backend, items: &[PendingTx]) {
        if !self.open {
            return;
        }
        let mut open = true;
        let action: Cell<Option<(Option<usize>, bool)>> = Cell::new(None);
        let busy = self.busy();
        let changes: usize = items.iter().map(|p| p.changes).sum();
        let summary = trf("{} in {}", &[&trn(changes, "1 change", "{} changes"), &trn(items.len(), "1 transaction", "{} transactions")]);
        let (resp, _) = Dialog::new(tr("Pending transactions")).icon(icon::PAUSE).width(dialogs::WIDTH_L).body_padding(egui::Margin::symmetric(20, 16)).show_window(
            ctx,
            egui::Id::new("pending_transactions"),
            &mut open,
            None,
            |ui| {
                let p = style::pal(ui);
                if items.is_empty() {
                    dialogs::muted(ui, tr("No connection has uncommitted changes."));
                    return;
                }
                let cols = [
                    Column::new(tr("Connection"), W::Fr(1.6)),
                    Column::new(tr("Opened by"), W::Fr(1.0)),
                    Column::new(tr("Changes"), W::Fr(0.7)).right(),
                    Column::new(tr("Since"), W::Fr(0.7)),
                    Column::new("", W::Px(220.0)),
                ];
                let max_h = (ui.ctx().content_rect().height() - 300.0).max(120.0);
                // A long name is cut with …, the whole name in its tooltip.
                let cell = |ui: &mut egui::Ui, text: &str| {
                    let r = ui.add(egui::Label::new(RichText::new(text).color(p.text)).truncate());
                    if r.rect.width() + 1.0 >= ui.max_rect().width() {
                        r.on_hover_text(text);
                    }
                };
                table(ui, "pending_tx", &cols, items.len(), 40.0, max_h, |ui, row, col| {
                    let Some(i) = row else { return };
                    let tx = &items[i];
                    match col {
                        0 => cell(ui, &tx.conn_name),
                        1 => cell(ui, &tx.owner),
                        2 => {
                            ui.label(RichText::new(tx.changes.to_string()).color(p.text));
                        }
                        3 => {
                            ui.label(RichText::new(tx.since.map(|t| t.format("%H:%M:%S").to_string()).unwrap_or_else(|| "—".into())).color(p.text));
                        }
                        _ => {
                            ui.add_enabled_ui(!busy, |ui| {
                                if kit::button_ex(ui, Kind::Secondary, Some(icon::CHECK_CIRCLE), tr("Commit"), true).clicked() {
                                    action.set(Some((Some(i), true)));
                                }
                                if kit::button_ex(ui, Kind::Ghost, Some(icon::ARROW_COUNTER_CLOCKWISE), tr("Roll back"), true).clicked() {
                                    action.set(Some((Some(i), false)));
                                }
                            });
                        }
                    }
                });
            },
            Some(&mut |ui: &mut egui::Ui| {
                ui.add_enabled_ui(!busy && !items.is_empty(), |ui| {
                    if kit::secondary(ui, None, tr("Commit all")).clicked() {
                        action.set(Some((None, true)));
                    }
                    if kit::button_ex(ui, Kind::Ghost, None, tr("Roll back all"), false).clicked() {
                        action.set(Some((None, false)));
                    }
                });
                if busy {
                    ui.add(egui::Spinner::new().size(14.0));
                }
                if !items.is_empty() {
                    dialogs::right(ui, |ui| ui.label(RichText::new(&summary).size(12.0).color(style::pal(ui).text_muted)));
                }
            }),
        );
        if let Some((which, commit)) = action.get() {
            match which {
                Some(i) => self.end(backend, &items[i], commit),
                None => {
                    for p in items {
                        self.end(backend, p, commit);
                    }
                }
            }
        }
        if !self.jobs.is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
        dialogs::dim_behind(ctx, resp.layer);
        self.open = open && !crate::widgets::escape_closes(ctx, resp.layer);
    }
}
