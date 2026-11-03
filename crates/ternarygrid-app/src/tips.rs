//! Tip of the day: short hints about features, shown on start (can be
//! turned off) and from Help › Tip of the day.

use egui::{Align, Context, Id, Layout, Margin, RichText};
use egui_phosphor::regular as icon;
use std::cell::Cell;
use ternarygrid_core::tr;
use ternarygrid_ui::kit::{self, Kind};
use ternarygrid_ui::parts::Dialog;
use ternarygrid_ui::style;

pub const TIPS: &[&str] = &[
    "Press Ctrl+Enter (⌘↩ on macOS) to run the statement under the cursor; select text to run only the selection.",
    "The search box in the top bar (⌘⇧P / Ctrl+Shift+P) runs any command and opens tables, views, routines and scripts by name.",
    "Click the connection or schema in the top bar to switch the SQL editor's, or to find it in the navigator.",
    "The status bar shows the transaction mode; in manual commit mode, click it to see the pending transactions.",
    // U+E1FE: the ⋯ of the More buttons (Phosphor's; the text font has no ⋯).
    "A table's filter runs in the database (WHERE) or over the rows already fetched (FILTER): switch it in the filter bar's \u{E1FE} menu.",
    "Open Database Object (⌘⇧D / Ctrl+Shift+D) finds a table or routine by name in every connected database.",
    "Use :name or ${name} in a script: TernaryGrid asks for the values when it runs.",
    "Double-click a cell in the results to edit it; changes are saved with ⌘S / Ctrl+S or the Save button.",
    "Right-click a column header to filter, hide, pin or highlight rows with a value.",
    "Click a column header to sort: ascending, descending, then back to the original order.",
    "Connection types (development, test, production) colour the connection and can require confirmation before data changes.",
    "In manual commit mode, Database › Pending transactions lists every connection with uncommitted changes.",
    "Navigator › Link with editor selects the table of the active tab in the tree.",
    "Fold long statements and block comments with the markers in the gutter; the SQL editor menu collapses or expands them all.",
    "Drag a .sql file onto the window to open it; drop a SQLite file to add it as a connection.",
    "Tools › Backup and Restore use pg_dump, mysqldump or sqlite3; Scope › All databases runs pg_dumpall.",
    "Export results to CSV, JSON, Excel, SQL INSERT statements, Markdown and more with Export in the results footer.",
    "The ER diagram of a schema opens from the navigator context menu; drag tables to arrange it.",
    "Compare schemas or the data of two tables from the Database menu and generate the synchronisation script.",
    "Settings › Keyboard shortcuts offers presets (TernaryGrid or DBeaver) or shortcuts of your own.",
    "View › Theme switches between the light and the dark theme, or follows the system.",
    "Help › Error log shows the errors and warnings of this session; Help › Collect diagnostic info saves them without any secret.",
    "Hold ⌘ / Ctrl and click a table name in the editor to open it.",
];

/// The tip window; `index` is the tip shown. Returns false when it is closed.
pub struct TipWindow {
    pub index: usize,
}

impl TipWindow {
    /// Show the tip; `show_on_start` is the setting. Returns false when closed.
    pub fn show(&mut self, ctx: &Context, show_on_start: &mut bool) -> bool {
        let mut open = true;
        let n = TIPS.len();
        let (close, step) = (Cell::new(false), Cell::new(0i32));
        let tip = tr(TIPS[self.index % n]);
        let position = format!("{}/{n}", self.index % n + 1);
        let (resp, _) = Dialog::new(tr("Tip of the day")).icon(icon::LIGHTBULB).width(480.0).body_padding(Margin::symmetric(22, 20)).show_window(
            ctx,
            Id::new("tip_of_the_day"),
            &mut open,
            None,
            |ui| {
                let p = style::pal(ui);
                crate::dialogs::label_lh(ui, RichText::new(tip).color(p.text), egui::FontId::proportional(14.5), 1.55, false);
            },
            Some(&mut |ui: &mut egui::Ui| {
                ternarygrid_ui::kit::checkbox(ui, show_on_start, tr("Show tips on start"));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if kit::button_ex(ui, Kind::Ghost, None, tr("Close"), false).clicked() {
                        close.set(true);
                    }
                    if kit::button_ex(ui, Kind::Secondary, Some(icon::ARROW_RIGHT), tr("Next tip"), false).clicked() {
                        step.set(1);
                    }
                    if kit::icon_button_sized(ui, icon::ARROW_LEFT, tr("Previous tip"), false, 28.0, 14.0).clicked() {
                        step.set(-1);
                    }
                    ui.label(RichText::new(&position).size(12.0).color(style::pal(ui).text_faint));
                });
            }),
        );
        crate::dialogs::dim_behind(ctx, resp.layer);
        match step.get() {
            1 => self.index = (self.index + 1) % n,
            -1 => self.index = (self.index + n - 1) % n,
            _ => {}
        }
        open && !close.get() && !crate::widgets::escape_closes(ctx, resp.layer)
    }
}
