//! Import connections from DBeaver, `~/.pgpass` or a connection URL (FR-C8).

use super::{Column, LABEL_W, W, add_field, cancel_button, hint, muted, rest, right, table, text_field};
use crate::cx::AppContext;
use egui::{Id, RichText, Ui};
use egui_phosphor::regular as icon;
use std::cell::Cell;
use std::path::PathBuf;
use ternarygrid_config::ImportResult;
use ternarygrid_config::import::{DuplicatePolicy, merge_into_store};
use ternarygrid_core::{DbResult, tr, trf};
use ternarygrid_ui::kit;
use ternarygrid_ui::parts::{self, Dialog};
use ternarygrid_ui::style;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    Dbeaver,
    Pgpass,
    Url,
}

pub struct ImportDialog {
    source: Source,
    workspaces: Vec<PathBuf>,
    workspace: Option<PathBuf>,
    pgpass_path: String,
    pgpass_store_passwords: bool,
    url: String,
    url_name: String,
    result: Option<ImportResult>,
    selected: Vec<bool>,
    policy: DuplicatePolicy,
    keep_shell_commands: bool,
    folder_prefix: String,
    error: Option<String>,
}

impl Default for ImportDialog {
    fn default() -> Self {
        let workspaces = ternarygrid_config::import::dbeaver::find_workspaces();
        let pgpass_path = ternarygrid_config::import::pgpass::PgPass::default_path().map(|p| p.display().to_string()).unwrap_or_default();
        ImportDialog {
            source: Source::Dbeaver,
            workspace: workspaces.first().cloned(),
            workspaces,
            pgpass_path,
            pgpass_store_passwords: false,
            url: String::new(),
            url_name: String::new(),
            result: None,
            selected: Vec::new(),
            policy: DuplicatePolicy::Skip,
            keep_shell_commands: false,
            folder_prefix: String::new(),
            error: None,
        }
    }
}

impl ImportDialog {
    fn load(&mut self) {
        self.error = None;
        let res: DbResult<ImportResult> = match self.source {
            Source::Dbeaver => match &self.workspace {
                Some(ws) => ternarygrid_config::import::dbeaver::import_workspace(ws),
                None => Err(ternarygrid_core::DbError::config(tr("Choose a DBeaver workspace folder."))),
            },
            Source::Pgpass => ternarygrid_config::import::pgpass::import_pgpass_with(std::path::Path::new(self.pgpass_path.trim()), self.pgpass_store_passwords),
            Source::Url => ternarygrid_config::parse_connection_url(self.url.trim()).map(|(mut cfg, password)| {
                if !self.url_name.trim().is_empty() {
                    cfg.name = self.url_name.trim().to_string();
                }
                let mut r = ImportResult::default();
                if let Some(pw) = password {
                    cfg.save_password = true;
                    r.secrets.push((ternarygrid_core::config::secret_key(&cfg.id, ternarygrid_core::config::SecretSlot::Password), pw));
                }
                r.connections.push(cfg);
                r
            }),
        };
        match res {
            Ok(r) => {
                self.selected = vec![true; r.connections.len()];
                self.result = Some(r);
            }
            Err(e) => {
                self.result = None;
                self.error = Some(e.full_text());
            }
        }
    }

    fn apply(&mut self, cx: &mut AppContext) -> bool {
        let Some(mut result) = self.result.take() else { return false };
        let mut keep = self.selected.iter();
        result.connections.retain(|_| *keep.next().unwrap_or(&false));
        if !self.keep_shell_commands {
            for c in &mut result.connections {
                c.init.shell_before_connect = None;
                c.init.shell_after_disconnect = None;
            }
        }
        if !self.folder_prefix.trim().is_empty() {
            result.prefix_folders(self.folder_prefix.trim());
        }
        let warnings = result.warnings.clone();
        let summary = merge_into_store(&mut cx.conns.store, result, self.policy);
        if let Err(e) = cx.services.secrets.read().store_all(&summary.secrets) {
            cx.toasts.error(tr("Cannot store the imported passwords"), Some(e.to_string()));
        }
        cx.conns.dirty = true;
        let detail = if warnings.is_empty() { None } else { Some(warnings.join("\n")) };
        cx.toasts.success(trf("Imported {} connection(s), replaced {}, skipped {}", &[&summary.added, &summary.replaced, &summary.skipped]));
        if let Some(d) = detail {
            cx.toasts.warning(tr("Import notes"), Some(d));
        }
        true
    }

    /// Draw the dialog; returns false when it was closed.
    pub fn show(&mut self, ctx: &egui::Context, cx: &mut AppContext) -> bool {
        let mut open = true;
        let import = Cell::new(false);
        let cancel = Cell::new(false);
        let selected = self.selected.iter().filter(|s| **s).count();
        let has_result = self.result.as_ref().is_some_and(|r| !r.connections.is_empty());
        let label = trf("Import {} connection(s)", &[&selected]);
        let (resp, _) = Dialog::new(tr("Import connections")).icon(icon::DOWNLOAD_SIMPLE).width(760.0).show_window(
            ctx,
            Id::new("ternarygrid_import_connections"),
            &mut open,
            None,
            |ui| self.body(ui),
            Some(&mut |ui: &mut Ui| {
                right(ui, |ui| {
                    if has_result && ui.add_enabled_ui(selected > 0, |ui| kit::primary_icon(ui, icon::DOWNLOAD_SIMPLE, &label)).inner.clicked() {
                        import.set(true);
                    }
                    if cancel_button(ui).clicked() {
                        cancel.set(true);
                    }
                });
            }),
        );
        let done = import.get() && self.apply(cx);
        open && !done && !cancel.get() && !crate::widgets::escape_closes(ctx, resp.layer)
    }

    fn body(&mut self, ui: &mut Ui) {
        let p = style::pal(ui);
        super::form_spacing(ui, 12.0);
        let mut source = self.source;
        if parts::segmented(ui, &mut source, &[(Source::Dbeaver, "DBeaver"), (Source::Pgpass, ".pgpass"), (Source::Url, "URL")]) {
            self.source = source;
            self.result = None;
            self.error = None;
        }
        let find_w = kit::button_width(ui, Some(icon::MAGNIFYING_GLASS), tr("Find connections"), false);
        let browse_w = kit::button_width(ui, Some(icon::FOLDER_OPEN), tr("Browse…"), false);
        let mut find = false;
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 8.0;
            match self.source {
                Source::Dbeaver => {
                    muted(ui, tr("Workspace folder (contains General/.dbeaver/data-sources.json)"));
                    ui.horizontal(|ui| {
                        let current = self.workspace.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| tr("(none found)").to_string());
                        let w = rest(ui, find_w + browse_w + 16.0) - 12.0;
                        egui::ComboBox::from_id_salt("dbeaver_ws").icon(kit::combo_caret).selected_text(current).width(w).show_ui(ui, |ui| {
                            for w in &self.workspaces {
                                ui.selectable_value(&mut self.workspace, Some(w.clone()), w.display().to_string());
                            }
                        });
                        if kit::secondary(ui, Some(icon::FOLDER_OPEN), tr("Browse…")).clicked()
                            && let Some(p) = rfd::FileDialog::new().pick_folder()
                        {
                            self.workspaces.push(p.clone());
                            self.workspace = Some(p);
                        }
                        find |= kit::primary_icon(ui, icon::MAGNIFYING_GLASS, tr("Find connections")).clicked();
                    });
                    ui.label(RichText::new(tr("Passwords saved by DBeaver with the default key are imported into the secure store.")).size(12.0).color(p.text_faint));
                }
                Source::Pgpass => {
                    muted(ui, tr("File"));
                    ui.horizontal(|ui| {
                        let w = rest(ui, find_w + browse_w + 16.0);
                        add_field(ui, text_field(&mut self.pgpass_path, w).font(egui::TextStyle::Monospace));
                        if kit::secondary(ui, Some(icon::FOLDER_OPEN), tr("Browse…")).clicked()
                            && let Some(p) = rfd::FileDialog::new().pick_file()
                        {
                            self.pgpass_path = p.display().to_string();
                        }
                        find |= kit::primary_icon(ui, icon::MAGNIFYING_GLASS, tr("Find connections")).clicked();
                    });
                    ternarygrid_ui::kit::checkbox(ui, &mut self.pgpass_store_passwords, tr("Copy the passwords into the secure store (otherwise the connections read .pgpass)"));
                }
                Source::Url => {
                    parts::form_row(ui, tr("URL"), 80.0, |ui| {
                        let w = rest(ui, find_w + 8.0);
                        add_field(ui, text_field(&mut self.url, w).font(egui::TextStyle::Monospace).hint_text(hint(ui, "postgres://user@host:5432/db")));
                        find |= kit::primary_icon(ui, icon::MAGNIFYING_GLASS, tr("Find connections")).clicked();
                    });
                    parts::form_row(ui, tr("Name"), 80.0, |ui| add_field(ui, text_field(&mut self.url_name, 260.0)));
                    ui.label(
                        RichText::new(tr("Supported: postgres://, mysql://, mariadb://, sqlserver://, oracle://, clickhouse://, cassandra://, sqlite:, jdbc:…"))
                            .size(12.0)
                            .color(p.text_faint),
                    );
                }
            }
        });
        if find {
            self.load();
        }
        if let Some(e) = &self.error {
            crate::dialogs::error_note(ui, e);
        }
        let Some(r) = &self.result else { return };
        parts::hline(ui);
        if r.connections.is_empty() {
            muted(ui, tr("No connections found."));
            return;
        }
        ui.horizontal(|ui| {
            ui.label(RichText::new(trf("{} connection(s) found", &[&r.connections.len()])).font(style::semibold(ui.ctx(), 13.0)).color(p.text));
            if kit::ghost(ui, None, tr("Select all")).clicked() {
                self.selected.iter_mut().for_each(|s| *s = true);
            }
            if kit::ghost(ui, None, tr("Select none")).clicked() {
                self.selected.iter_mut().for_each(|s| *s = false);
            }
        });
        let cols = [
            Column::new("", W::Px(34.0)),
            Column::new(tr("Connection"), W::Fr(1.3)),
            Column::new(tr("Host"), W::Fr(1.6)),
            Column::new(tr("Folder"), W::Fr(1.0)),
            Column::new("", W::Px(40.0)),
        ];
        let max_h = (ui.ctx().content_rect().height() - 560.0).max(120.0);
        let selected = &mut self.selected;
        table(ui, "import_conns", &cols, r.connections.len(), 31.0, max_h, |ui, row, col| {
            let Some(row) = row else {
                // The header's box selects all or none.
                if col == 0 {
                    let mut all = !selected.is_empty() && selected.iter().all(|s| *s);
                    let partly = !all && selected.iter().any(|s| *s);
                    if ui.add(egui::Checkbox::without_text(&mut all).indeterminate(partly)).on_hover_text(tr("Select all")).changed() {
                        selected.iter_mut().for_each(|s| *s = all);
                    }
                }
                return;
            };
            let c = &r.connections[row];
            match col {
                0 => {
                    if let Some(s) = selected.get_mut(row) {
                        ui.add(egui::Checkbox::without_text(s));
                    }
                }
                1 => {
                    super::db_logo(ui, c.kind, 15.0);
                    ui.label(RichText::new(&c.name).font(style::medium(ui.ctx(), 13.0)).color(p.text));
                }
                2 => {
                    let target = if c.kind.is_file_based() { c.file_path.clone().unwrap_or_default() } else { format!("{}:{}", c.host, c.effective_port()) };
                    ui.label(RichText::new(target).monospace().size(12.0).color(p.text_muted));
                }
                3 => {
                    if let Some(f) = &c.folder {
                        ui.label(RichText::new(icon::FOLDER).size(13.0).color(p.text_faint));
                        ui.label(RichText::new(f).size(12.5).color(p.text_muted));
                    }
                }
                _ => {
                    if c.init.shell_before_connect.is_some() || c.init.shell_after_disconnect.is_some() {
                        ui.label(RichText::new(icon::TERMINAL).size(14.0).color(p.warn)).on_hover_text(tr("Runs a shell command when connecting or disconnecting."));
                    }
                }
            }
        });
        if !r.warnings.is_empty() && super::disclosure(ui, Id::new("import_notes"), &trf("{} note(s)", &[&r.warnings.len()]), false) {
            for w in &r.warnings {
                muted(ui, w);
            }
        }
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 10.0;
            parts::form_row(ui, tr("Existing connections"), LABEL_W, |ui| {
                ui.spacing_mut().item_spacing.x = 16.0;
                ternarygrid_ui::kit::radio_value(ui, &mut self.policy, DuplicatePolicy::Skip, tr("Skip"));
                ternarygrid_ui::kit::radio_value(ui, &mut self.policy, DuplicatePolicy::Replace, tr("Replace"));
                ternarygrid_ui::kit::radio_value(ui, &mut self.policy, DuplicatePolicy::KeepBoth, tr("Keep both"));
            });
            parts::form_row(ui, tr("Put into folder"), LABEL_W, |ui| add_field(ui, text_field(&mut self.folder_prefix, 260.0).hint_text(hint(ui, tr("(top level)")))));
            parts::form_row(ui, "", LABEL_W, |ui| ternarygrid_ui::kit::checkbox(ui, &mut self.keep_shell_commands, tr("Keep shell commands that run on connect / disconnect")));
        });
    }
}
