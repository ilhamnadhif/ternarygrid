//! Transient notifications: cards above the status bar with an icon, a title,
//! the detail, a link and ×.

use egui::{Align2, Context, CornerRadius, Id, Margin, RichText, Stroke};
use egui_phosphor::regular as icon;
use std::time::{Duration, Instant};
use ternarygrid_core::tr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Debug, Clone)]
pub struct Toast {
    pub kind: ToastKind,
    pub title: String,
    pub detail: Option<String>,
    /// A file the toast offers to show in the Finder.
    pub reveal: Option<std::path::PathBuf>,
    created: Instant,
    duration: Duration,
}

#[derive(Default)]
pub struct Toasts {
    items: Vec<Toast>,
    /// Every toast since the last [`Toasts::take_log`] (bounded), for tests.
    log: Vec<Toast>,
}

impl Toasts {
    pub fn push(&mut self, kind: ToastKind, title: impl Into<String>, detail: Option<String>) {
        let duration = match kind {
            ToastKind::Error => Duration::from_secs(10),
            ToastKind::Warning => Duration::from_secs(7),
            _ => Duration::from_secs(4),
        };
        let toast = Toast { kind, title: title.into(), detail, reveal: None, created: Instant::now(), duration };
        if self.log.len() < 500 {
            self.log.push(toast.clone());
        }
        self.items.push(toast);
        if self.items.len() > 6 {
            self.items.remove(0);
        }
    }

    /// Dismiss every visible toast (the log keeps them).
    pub fn clear(&mut self) {
        self.items.clear();
    }

    /// Takes the toasts pushed since the previous call.
    pub fn take_log(&mut self) -> Vec<Toast> {
        std::mem::take(&mut self.log)
    }

    pub fn info(&mut self, title: impl Into<String>) {
        self.push(ToastKind::Info, title, None);
    }
    pub fn success(&mut self, title: impl Into<String>) {
        self.push(ToastKind::Success, title, None);
    }
    /// Success with a file (an export, a backup): its name and "Show in Finder".
    pub fn success_file(&mut self, title: impl Into<String>, path: std::path::PathBuf) {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
        self.push(ToastKind::Success, title, name);
        for t in [self.items.last_mut(), self.log.last_mut()].into_iter().flatten() {
            t.reveal = Some(path.clone());
            t.duration = Duration::from_secs(8);
        }
    }
    pub fn warning(&mut self, title: impl Into<String>, detail: Option<String>) {
        let title = title.into();
        crate::applog::push(tracing::Level::WARN, "ternarygrid::ui", log_text(&title, detail.as_deref()));
        self.push(ToastKind::Warning, title, detail);
    }
    pub fn error(&mut self, title: impl Into<String>, detail: Option<String>) {
        let title = title.into();
        crate::applog::push(tracing::Level::ERROR, "ternarygrid::ui", log_text(&title, detail.as_deref()));
        self.push(ToastKind::Error, title, detail);
    }

    pub fn show(&mut self, ctx: &Context) {
        let now = Instant::now();
        self.items.retain(|t| now.duration_since(t.created) < t.duration);
        if self.items.is_empty() {
            return;
        }
        let dark = ctx.theme() == egui::Theme::Dark;
        let p = ternarygrid_ui::palette(dark);
        let mut close: Option<usize> = None;
        // Above the status bar and the result toolbar, about the middle (where the design has
        // them): not over the side panels' buttons (Value: Apply, Set NULL) or the fetch
        // buttons at the right. The text is 380 points wide.
        egui::Area::new(Id::new("ternarygrid_toasts")).anchor(Align2::CENTER_BOTTOM, egui::vec2(14.0, -76.0)).order(egui::Order::Foreground).show(ctx, |ui| {
            ui.set_width(408.0);
            ui.spacing_mut().item_spacing.y = 8.0;
            for (i, t) in self.items.iter().enumerate() {
                let (glyph, color) = match t.kind {
                    ToastKind::Info => (icon::INFO, p.accent_text),
                    ToastKind::Success => (icon::CHECK_CIRCLE, p.ok),
                    ToastKind::Warning => (icon::WARNING, p.warn),
                    ToastKind::Error => (icon::X_CIRCLE, p.err),
                };
                // A card: the icon, the title, the detail, a link, ×.
                let card = egui::Frame::new()
                    .fill(p.surface)
                    .stroke(Stroke::new(1.0, p.border))
                    .corner_radius(CornerRadius::same(ternarygrid_ui::style::RADIUS_LG))
                    .shadow(ui.visuals().popup_shadow)
                    .inner_margin(Margin { left: 14, right: 12, top: 12, bottom: 12 });
                card.show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal_top(|ui| {
                        ui.spacing_mut().item_spacing.x = 10.0;
                        ui.label(RichText::new(glyph).font(ternarygrid_ui::style::icon_font(17.0)).color(color));
                        let w = (ui.available_width() - 32.0).max(120.0);
                        ui.allocate_ui_with_layout(egui::vec2(w, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                            ui.set_width(w);
                            ui.spacing_mut().item_spacing.y = 3.0;
                            crate::dialogs::semibold_text(ui, &t.title, 13.0, p.text, None, false);
                            if let Some(d) = &t.detail {
                                let short: String = d.chars().take(600).collect();
                                ui.add(egui::Label::new(RichText::new(short).size(12.0).color(p.text_muted)).wrap());
                            }
                            if let Some(path) = &t.reveal {
                                let label = if cfg!(target_os = "macos") { tr("Show in Finder") } else { tr("Show in folder") };
                                if ui.add(egui::Link::new(RichText::new(label).size(12.0).underline().color(p.accent_text))).clicked() {
                                    reveal(path);
                                }
                            }
                        });
                        if ternarygrid_ui::kit::icon_button_sized(ui, icon::X, tr("Dismiss"), false, 22.0, 11.0).clicked() {
                            close = Some(i);
                        }
                    });
                });
            }
        });
        if let Some(i) = close {
            self.items.remove(i);
        }
        // Wake up again when the next toast expires (no busy repainting).
        if let Some(next) = self.items.iter().map(|t| t.duration.saturating_sub(now.duration_since(t.created))).min() {
            ctx.request_repaint_after(next + Duration::from_millis(20));
        }
    }
}

/// Show `path` selected in the Finder (its folder elsewhere).
fn reveal(path: &std::path::Path) {
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("/usr/bin/open").arg("-R").arg(path).spawn();
    #[cfg(not(target_os = "macos"))]
    let _ = open::that_detached(path.parent().unwrap_or(path));
}

/// Toast text for the application log.
fn log_text(title: &str, detail: Option<&str>) -> String {
    match detail.filter(|d| !d.is_empty()) {
        Some(d) => format!("{title}: {d}"),
        None => title.to_string(),
    }
}
