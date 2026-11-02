//! Preferences tab: every section of `settings.json`, the keymap editor,
//! connection types, AI and secret storage. A side navigation (with a search
//! that finds sections by their settings) and, at the right, the section's
//! settings in titled groups.

use crate::command::AppCommand;
use crate::cx::AppContext;
use crate::keys::{Action, IS_MAC, Keymap, Shortcut};
use crate::tabs::TabView;
use crate::tabs::tasks::look;
use egui::{RichText, Ui};
use egui_phosphor::regular as icon;
use ternarygrid_config::settings::{AiEffort, AiProvider, BlankLineDelimiter, KeywordCase, NavigatorSort, ResultsLayout, RowDensity, SecretBackend, SortFilterMode, Theme};
use ternarygrid_config::{KeymapPreset, SecretsManager};
use ternarygrid_core::config::{ConnectionType, NavigatorView};
use ternarygrid_core::value::{BinaryFormat, BoolStyle};
use ternarygrid_core::{Secret, tr, trf};
use ternarygrid_ui::kit::{self, Kind};
use ternarygrid_ui::parts::{self, Tone};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    General,
    Editor,
    Results,
    Execution,
    History,
    Navigator,
    Keyboard,
    ConnectionTypes,
    NetworkProfiles,
    Ai,
    Security,
    Accessibility,
}

impl Section {
    const ALL: [Section; 12] = [
        Section::General,
        Section::Editor,
        Section::Results,
        Section::Execution,
        Section::History,
        Section::Navigator,
        Section::Keyboard,
        Section::ConnectionTypes,
        Section::NetworkProfiles,
        Section::Ai,
        Section::Security,
        Section::Accessibility,
    ];

    fn label(self) -> &'static str {
        match self {
            Section::General => tr("General"),
            Section::Editor => tr("SQL editor"),
            Section::Results => tr("Result sets"),
            Section::Execution => tr("Execution"),
            Section::History => tr("Query history"),
            Section::Navigator => tr("Navigator"),
            Section::Keyboard => tr("Keyboard shortcuts"),
            Section::ConnectionTypes => tr("Connection types"),
            Section::NetworkProfiles => tr("Network profiles"),
            Section::Ai => tr("AI assistant"),
            Section::Security => tr("Security"),
            Section::Accessibility => tr("Accessibility"),
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Section::General => icon::GEAR,
            Section::Editor => icon::FILE_CODE,
            Section::Results => icon::TABLE,
            Section::Execution => icon::PLAY,
            Section::History => icon::CLOCK,
            Section::Navigator => icon::TREE_STRUCTURE,
            Section::Keyboard => icon::KEYBOARD,
            Section::ConnectionTypes => icon::TAG,
            Section::NetworkProfiles => icon::GLOBE,
            Section::Ai => icon::SPARKLE,
            Section::Security => icon::LOCK,
            Section::Accessibility => icon::EYE,
        }
    }

    /// The settings of the section (what "Search settings" finds it by).
    fn keywords(self) -> &'static [&'static str] {
        match self {
            Section::General => &[
                "Appearance",
                "Theme",
                "Light",
                "Dark",
                "Font size",
                "Zoom",
                "Time zone",
                "Reopen the editors of the last session on start",
                "Ask before quitting with unsaved changes",
                "Check for updates automatically (at start and every few hours)",
            ],
            Section::Editor => &[
                "Font size",
                "Tab size",
                "Statement delimiter",
                "Blank line ends a statement",
                "Auto-save interval",
                "Insert spaces instead of tabs",
                "Show line numbers",
                "Code folding (statements, block comments)",
                "Highlight occurrences of the word under the cursor",
                "Highlight the current line",
                "Wrap long lines",
                "Close quotes automatically",
                "Close brackets automatically",
                "Check spelling in comments and strings",
                "Autocomplete",
                "Delay",
                "Minimum characters",
                "Maximum proposals",
                "Keyword case",
                "Formatter",
                "Indent width",
                "Lines between statements",
                "Maximum line length",
                "Indent with tabs",
                "Commas at the start of lines",
                "New line before AND / OR",
                "Parameters and variables",
            ],
            Section::Results => &[
                "Rows fetched per page",
                "Sorting and filtering",
                "Confirm \"fetch all\" above",
                "Auto-refresh interval",
                "Results position",
                "Row height",
                "Maximum result tabs",
                "Open each query in a new result tab",
                "Show row numbers",
                "Memory per result set",
                "Memory for all result sets",
                "Maximum text length",
                "NULL text",
                "Date format",
                "Time format",
                "Timestamp format",
                "Binary values",
                "Booleans",
                "Float decimals",
                "Show fractional seconds",
                "Thousands separator",
            ],
            Section::Execution => &[
                "Query timeout",
                "Stop the script on the first error",
                "Confirm DELETE / UPDATE without WHERE, DROP and TRUNCATE",
                "Smart commit: switch to manual commit before data changes",
            ],
            Section::History => &["Record executed queries", "Mask passwords in recorded statements", "Keep at most", "Keep for", "Clear the whole history"],
            Section::Navigator => {
                &["Order", "Default view of new connections", "Show system schemas and objects", "Show user@host next to connection names", "Expand a connection after connecting"]
            }
            Section::Keyboard => &["Preset", "Shortcut", "Reset all"],
            Section::ConnectionTypes => &["Colour", "Production", "Auto-commit by default", "Confirm statements that change data or schema", "Confirm saving data changes"],
            Section::NetworkProfiles => &["SSH", "Proxy", "Tunnel"],
            Section::Ai => &["Provider", "Model", "Base URL", "Maximum answer length", "Effort", "API key", "Send table and column names as context", "Allow sending sample rows"],
            Section::Security => &["Secret storage", "Lock the vault after", "Master password", "Keychain", "Encrypted file"],
            Section::Accessibility => &["High contrast", "Reduce motion", "Always show the keyboard focus", "Run scheduled tasks while TernaryGrid is open"],
        }
    }

    /// The section is found by `query` (lower case): its name or one of its settings.
    fn matches(self, query: &str) -> bool {
        query.is_empty() || self.label().to_lowercase().contains(query) || self.hits(query) > 0
    }

    /// How many of the section's settings `query` (lower case) finds.
    fn hits(self, query: &str) -> usize {
        if query.is_empty() {
            return 0;
        }
        self.keywords().iter().filter(|k| tr(k).to_lowercase().contains(query)).count()
    }

    /// A line under the section's title.
    fn intro(self) -> Option<&'static str> {
        match self {
            Section::NetworkProfiles => Some(tr("Reusable SSH tunnel and proxy settings; connections pick a profile on their SSH page.")),
            Section::Ai => Some(tr("The assistant sends your question, and optionally schema details, to the provider you configure. Nothing is sent while it is disabled.")),
            Section::Security => Some(tr("Passwords and API keys are never written to configuration files.")),
            _ => None,
        }
    }
}

pub struct SettingsTab {
    section: Section,
    /// "Search settings": sections shown in the navigation.
    search: String,
    keymap: Option<Keymap>,
    key_filter: String,
    recording: Option<Action>,
    ai_key: String,
    ai_key_loaded: Option<AiProvider>,
    ai_key_saved: bool,
    selected_type: usize,
    profile: Option<ternarygrid_core::config::NetworkProfile>,
    profile_ssh_password: String,
    profile_ssh_passphrase: String,
    profile_proxy_password: String,
    new_master: String,
    new_master2: String,
    old_master: String,
    message: Option<Result<String, String>>,
    /// Moving the secrets or changing the master password (Argon2 takes a moment, off
    /// the UI thread): the message and the storage now used.
    secrets_job: Option<crate::backend::Job<Result<(String, Option<SecretBackend>), String>>>,
    /// Asking whether to reset every setting.
    confirm_reset: bool,
    /// The network profile's SSH options the design leaves out are shown.
    ssh_more: bool,
}

/// Every setting back to its default, except the state of the update
/// checks (seen version, skipped release).
pub fn reset_settings(settings: &mut ternarygrid_config::settings::AppSettings) {
    let updates = settings.updates.clone();
    *settings = Default::default();
    settings.updates = updates;
}

impl Default for SettingsTab {
    fn default() -> Self {
        SettingsTab {
            section: Section::General,
            search: String::new(),
            keymap: None,
            key_filter: String::new(),
            recording: None,
            ai_key: String::new(),
            ai_key_loaded: None,
            ai_key_saved: false,
            selected_type: 0,
            profile: None,
            profile_ssh_password: String::new(),
            profile_ssh_passphrase: String::new(),
            profile_proxy_password: String::new(),
            new_master: String::new(),
            new_master2: String::new(),
            old_master: String::new(),
            message: None,
            secrets_job: None,
            confirm_reset: false,
            ssh_more: false,
        }
    }
}

/// Run `work` on a blocking thread (Argon2 key derivation) as a job.
fn blocking<T: Send + 'static>(cx: &AppContext, work: impl FnOnce() -> Result<T, String> + Send + 'static) -> crate::backend::Job<Result<T, String>> {
    cx.backend.spawn(async move { tokio::task::spawn_blocking(work).await.unwrap_or_else(|e| Err(e.to_string())) })
}

// ---------------------------------------------------------------------------------------------
// Form pieces
// ---------------------------------------------------------------------------------------------

/// Width of the label column: 220 points, less in a half-width group.
fn label_w(ui: &Ui) -> f32 {
    // In a half-width group the label column leaves the controls `narrow_controls()` points
    // (196: a three-option segmented control) instead of overflowing the group as the mockup
    // does.
    let w = ui.available_width();
    let controls = ui.data(|d| d.get_temp::<f32>(narrow_controls_id())).unwrap_or(196.0);
    if w < 480.0 { (w - 16.0 - controls).clamp(110.0, 165.0) } else { 220.0 }
}

/// The room a narrow group's rows keep for their controls (see [`label_w`]).
fn narrow_controls_id() -> egui::Id {
    egui::Id::new("settings_narrow_controls")
}

/// What "Search settings" looks for (lower case), kept for the frame in the ui's memory.
fn search_id() -> egui::Id {
    egui::Id::new("settings_search_query")
}

/// The content scrolls to the first setting found once after the search changed.
fn scroll_id() -> egui::Id {
    egui::Id::new("settings_search_scroll")
}

/// Draws a setting; one that "Search settings" finds gets a soft accent behind it.
fn found(ui: &mut Ui, label: &str, add: impl FnOnce(&mut Ui) -> bool) -> bool {
    let mut add = Some(add);
    found_dyn(ui, label, &mut |ui| add.take().is_some_and(|a| a(ui)))
}

// Not generic: compiled once for the hundred settings that use it.
fn found_dyn(ui: &mut Ui, label: &str, add: &mut dyn FnMut(&mut Ui) -> bool) -> bool {
    let query = ui.data(|d| d.get_temp::<String>(search_id())).unwrap_or_default();
    if query.is_empty() || label.is_empty() || !label.to_lowercase().contains(&query) {
        return add(ui);
    }
    let behind = ui.painter().add(egui::Shape::Noop);
    let r = ui.scope(add);
    let p = ternarygrid_ui::pal(ui);
    let rect = r.response.rect.expand2(egui::vec2(8.0, 3.0));
    ui.painter().set(behind, egui::Shape::rect_filled(rect, ternarygrid_ui::style::RADIUS, p.accent_soft));
    if ui.data_mut(|d| d.remove_temp::<bool>(scroll_id())).unwrap_or(false) {
        r.response.scroll_to_me(Some(egui::Align::Center));
    }
    r.inner
}

/// A setting: its label in the label column, then the control (30 points high) 16 points
/// further.
fn row(ui: &mut Ui, label: &str, add: impl FnOnce(&mut Ui) -> bool) -> bool {
    let mut add = Some(add);
    row_dyn(ui, label, None, look::FIELD_H, &mut |ui| add.take().is_some_and(|a| a(ui)))
}

/// [`row`] for a control `h` high (segmented 28, radio buttons 16): the row is as tall as
/// that, or as its label.
fn row_h(ui: &mut Ui, label: &str, h: f32, add: impl FnOnce(&mut Ui) -> bool) -> bool {
    let mut add = Some(add);
    row_dyn(ui, label, None, h, &mut |ui| add.take().is_some_and(|a| a(ui)))
}

/// [`row`] with a muted line of explanation under the control.
fn row_sub(ui: &mut Ui, label: &str, sub: &str, add: impl FnOnce(&mut Ui) -> bool) -> bool {
    let mut add = Some(add);
    row_dyn(ui, label, Some(sub), look::FIELD_H, &mut |ui| add.take().is_some_and(|a| a(ui)))
}

// Not generic: compiled once for the hundred settings that use it.
fn row_dyn(ui: &mut Ui, label: &str, sub: Option<&str>, h: f32, add: &mut dyn FnMut(&mut Ui) -> bool) -> bool {
    let w = label_w(ui);
    found_dyn(ui, label, &mut |ui| form_row(ui, label, w, sub, h, add))
}

/// The form row of the mockups: a grid of `label_w` and the rest, 16 points apart, the label
/// muted on the line of a 30 point control, the control's explanation 4 points under it.
fn form_row(ui: &mut Ui, label: &str, label_w: f32, sub: Option<&str>, h: f32, add: &mut dyn FnMut(&mut Ui) -> bool) -> bool {
    let p = ternarygrid_ui::pal(ui);
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 16.0;
        // The label 6 points down (on the line of a 30 point field), its cell as tall as that.
        ui.allocate_ui_with_layout(egui::vec2(label_w, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
            ui.set_min_width(label_w);
            ui.add_space(6.0);
            if !label.is_empty() {
                ui.add(egui::Label::new(RichText::new(label).size(13.0).color(p.text_muted)).truncate());
            }
        });
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(8.0, 4.0);
            // The controls centred on a line `h` high (not egui's 30 point rows).
            let w = ui.available_width();
            let c = ui.allocate_ui_with_layout(egui::vec2(w, h), egui::Layout::left_to_right(egui::Align::Center), |ui| add(ui)).inner;
            if let Some(sub) = sub {
                // 4 points under the control, on a line 1.4 times its size.
                ui.add_space(1.0);
                ui.add(egui::Label::new(RichText::new(sub).size(12.0).line_height(Some(16.8)).color(p.text_muted)).wrap());
            }
            c
        })
        .inner
    })
    .inner
}

/// Content indented to the controls' column (check boxes under the fields).
fn indented(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> bool) -> bool {
    let w = label_w(ui);
    ui.horizontal_top(|ui| {
        // (`add_space` brings no item spacing of its own.)
        ui.add_space(w + 16.0);
        ui.vertical(add).inner
    })
    .inner
}

fn check(ui: &mut Ui, value: &mut bool, label: &str) -> bool {
    found(ui, label, |ui| look::checkbox(ui, value, label).changed())
}

/// Check boxes in a column, 12 points apart.
fn checks(ui: &mut Ui, items: Vec<(&mut bool, &str)>) -> bool {
    checks_gap(ui, items, 12.0)
}

/// Check boxes in a column, `gap` points apart.
fn checks_gap(ui: &mut Ui, items: Vec<(&mut bool, &str)>, gap: f32) -> bool {
    let mut c = false;
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = gap;
        for (v, l) in items {
            c |= check(ui, v, l);
        }
    });
    c
}

/// Two columns of check boxes `gap` apart, each as wide as its longest label when they fit,
/// both narrowed in proportion to it when they do not (as flex boxes shrink).
fn check_columns(ui: &mut Ui, left: Vec<(&mut bool, &str)>, right: Vec<(&mut bool, &str)>, gap: f32, row_gap: f32) -> bool {
    let font = egui::FontId::proportional(13.0);
    let natural = |items: &Vec<(&mut bool, &str)>| {
        items.iter().map(|(_, l)| ui.painter().layout_no_wrap(l.to_string(), font.clone(), egui::Color32::PLACEHOLDER).size().x + 26.0).fold(0.0, f32::max)
    };
    let (lw, rw) = (natural(&left), natural(&right));
    let room = (ui.available_width() - gap).max(120.0);
    let scale = (room / (lw + rw)).min(1.0);
    let (lw, rw) = ((lw * scale).floor(), (rw * scale).floor());
    let mut c = false;
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        for (items, w) in [(left, lw), (right, rw)] {
            ui.allocate_ui_with_layout(egui::vec2(w, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                ui.set_width(w);
                c |= checks_gap(ui, items, row_gap);
            });
        }
    });
    c
}

fn combo<T: PartialEq + Copy>(ui: &mut Ui, id: &str, value: &mut T, options: &[(T, &str)], width: f32) -> bool {
    let labels: Vec<&str> = options.iter().map(|(_, l)| *l).collect();
    let mut current = options.iter().position(|(v, _)| v == value);
    let changed = combo_dyn(ui, id, &mut current, &labels, width);
    if changed && let Some(i) = current {
        *value = options[i].0;
    }
    changed
}

/// A select of `labels` (`current` is the chosen one), its drop-down in the design's menu look.
fn combo_dyn(ui: &mut Ui, id: &str, current: &mut Option<usize>, labels: &[&str], width: f32) -> bool {
    let mut changed = false;
    let text = current.and_then(|i| labels.get(i).copied()).unwrap_or("");
    egui::ComboBox::from_id_salt(id).icon(kit::combo_caret).width(width).selected_text(text).show_ui(ui, |ui| {
        kit::menu_look(ui, width);
        for (i, l) in labels.iter().enumerate() {
            let mut on = *current == Some(i);
            if kit::menu_check(ui, &mut on, l).clicked() && *current != Some(i) {
                *current = Some(i);
                changed = true;
            }
        }
    });
    changed
}

fn num<T: egui::emath::Numeric>(ui: &mut Ui, value: &mut T, range: std::ops::RangeInclusive<T>, unit: &str) -> bool {
    look::number(ui, value, range, unit).changed()
}

fn text(ui: &mut Ui, value: &mut String, width: f32, mono: bool) -> bool {
    look::text_field(ui, value, width, "", mono).changed()
}

/// Two groups side by side, 14 points apart, the left one taking `share` of the width (the
/// mockups' boxes share the row by their contents).
fn two(ui: &mut Ui, share: f32, left: impl FnOnce(&mut Ui) -> bool, right: impl FnOnce(&mut Ui) -> bool) -> bool {
    let (mut left, mut right) = (Some(left), Some(right));
    two_dyn(ui, share, &mut |ui| left.take().is_some_and(|f| f(ui)), &mut |ui| right.take().is_some_and(|f| f(ui)))
}

fn two_dyn(ui: &mut Ui, share: f32, left: &mut dyn FnMut(&mut Ui) -> bool, right: &mut dyn FnMut(&mut Ui) -> bool) -> bool {
    let mut c = false;
    let gap = 14.0;
    let lw = ((ui.available_width() - gap) * share).floor();
    let rw = (ui.available_width() - gap - lw).floor();
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        ui.allocate_ui_with_layout(egui::vec2(lw, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
            ui.set_width(lw);
            c |= left(ui);
        });
        ui.allocate_ui_with_layout(egui::vec2(rw, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
            ui.set_width(rw);
            c |= right(ui);
        });
    });
    c
}

/// A titled group of settings on a card ([`parts::group`]: 18 × 20 points of padding, 14
/// between its rows).
fn group(ui: &mut Ui, title: &str, add: impl FnOnce(&mut Ui) -> bool) -> bool {
    parts::group(ui, (!title.is_empty()).then_some(title), None, add)
}

/// [`group`] with a muted line under its title.
fn group_sub(ui: &mut Ui, title: &str, sub: &str, add: impl FnOnce(&mut Ui) -> bool) -> bool {
    parts::group(ui, Some(title), Some(sub), add)
}

/// [`group`] with a small text toggle at the right of its title (on the title's line, so the
/// group keeps the height of the design): "More options".
fn group_more(ui: &mut Ui, title: &str, more: &std::cell::Cell<bool>, add: impl FnOnce(&mut Ui) -> bool) -> bool {
    parts::group(ui, Some(title), None, |ui| {
        // So far the group holds its title only: the toggle goes at the right of that line.
        let r = ui.min_rect();
        more_toggle(ui, egui::Rect::from_x_y_ranges(r.left()..=ui.max_rect().right(), r.y_range()), more);
        add(ui)
    })
}

/// The "More options" toggle of [`group_more`] at the right end of `line`, not taking room.
fn more_toggle(ui: &mut Ui, line: egui::Rect, more: &std::cell::Cell<bool>) {
    let p = ternarygrid_ui::pal(ui);
    let label = if more.get() { tr("Fewer options") } else { tr("More options") };
    let caret = if more.get() { icon::CARET_UP } else { icon::CARET_DOWN };
    let g = ui.painter().layout_no_wrap(label.to_string(), ternarygrid_ui::style::medium(ui.ctx(), 12.5), p.text_muted);
    let w = g.size().x + 14.0 + 6.0 + 20.0;
    let rect = egui::Rect::from_center_size(egui::pos2(line.right() - w / 2.0 + 10.0, line.center().y), egui::vec2(w, 26.0));
    let resp = ui.interact(rect, ui.id().with("settings_more"), egui::Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
    if resp.hovered() {
        ui.painter().rect_filled(rect, ternarygrid_ui::style::RADIUS, p.hover);
    }
    ui.painter().text(egui::pos2(rect.left() + 17.0, rect.center().y), egui::Align2::CENTER_CENTER, caret, ternarygrid_ui::style::icon_font(14.0), p.text_muted);
    ui.painter().galley(egui::pos2(rect.left() + 30.0, rect.center().y - g.size().y / 2.0), g, p.text_muted);
    if resp.clicked() {
        more.set(!more.get());
    }
}

/// A chip of the mockups at 12 points: semibold text on a soft fill, 18 points high.
fn chip(ui: &mut Ui, text: &str, color: egui::Color32, fill: egui::Color32) -> egui::Response {
    let g = ui.painter().layout_no_wrap(text.to_string(), ternarygrid_ui::style::medium(ui.ctx(), 12.0), color);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(g.size().x + 14.0, 18.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, 9, fill);
    kit::paint_semibold(ui.painter(), rect.center() - g.size() / 2.0, g, color);
    resp
}

/// A small colour square (connection types).
fn swatch(ui: &mut Ui, color: egui::Color32, side: f32) -> egui::Response {
    let p = ternarygrid_ui::pal(ui);
    let (rect, resp) = ui.allocate_exact_size(egui::Vec2::splat(side), egui::Sense::hover());
    let r = if side > 16.0 { ternarygrid_ui::style::RADIUS } else { 3 };
    ui.painter().rect(rect, r, color, egui::Stroke::new(1.0, if side > 16.0 { p.border } else { egui::Color32::TRANSPARENT }), egui::StrokeKind::Inside);
    resp
}

/// A row of a list at the left of a section (the sections, connection types, network
/// profiles): 32 points, 10 of padding, a lead `lead_w` wide and the label 10 points after it,
/// the selected row on the soft accent (its label in medium with `bold`).
fn list_item(ui: &mut Ui, selected: bool, label: &str, lead: &mut dyn FnMut(&mut Ui, egui::Rect), lead_w: f32, bold: bool, right: Option<&str>) -> egui::Response {
    let p = ternarygrid_ui::pal(ui);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 32.0), egui::Sense::click());
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, label));
    if selected {
        ui.painter().rect_filled(rect, ternarygrid_ui::style::RADIUS, p.accent_soft);
    } else if resp.hovered() {
        ui.painter().rect_filled(rect, ternarygrid_ui::style::RADIUS, p.hover);
    }
    if resp.has_focus() {
        ui.painter().rect_stroke(rect, ternarygrid_ui::style::RADIUS, egui::Stroke::new(1.5, p.accent), egui::StrokeKind::Inside);
    }
    lead(ui, egui::Rect::from_min_size(egui::pos2(rect.left() + 10.0, rect.center().y - lead_w / 2.0), egui::vec2(lead_w, lead_w)));
    let font = if selected && bold { ternarygrid_ui::style::medium(ui.ctx(), 13.0) } else { egui::FontId::proportional(13.0) };
    ui.painter().text(egui::pos2(rect.left() + 20.0 + lead_w, rect.center().y), egui::Align2::LEFT_CENTER, label, font, p.text);
    if let Some(r) = right {
        ui.painter().text(egui::pos2(rect.right() - 10.0, rect.center().y), egui::Align2::RIGHT_CENTER, r, egui::FontId::proportional(11.5), p.text_faint);
    }
    resp
}

impl SettingsTab {
    fn general(&mut self, ui: &mut Ui, cx: &mut AppContext) -> bool {
        let s = &mut cx.settings.general;
        let mut c = group(ui, tr("Appearance"), |ui| {
            let mut c = false;
            c |= row_h(ui, tr("Theme"), 28.0, |ui| parts::segmented(ui, &mut s.theme, &[(Theme::System, tr("System")), (Theme::Light, tr("Light")), (Theme::Dark, tr("Dark"))]));
            c |= row(ui, tr("Font size"), |ui| num(ui, &mut s.ui_font_size, 8.0..=32.0, tr("pt")));
            c |= row(ui, tr("Zoom"), |ui| {
                let mut pct = (s.ui_scale * 100.0).round();
                let ch = num(ui, &mut pct, 50.0..=300.0, "%");
                s.ui_scale = pct / 100.0;
                ch
            });
            c
        });
        c |= group(ui, tr("Time"), |ui| {
            row_sub(ui, tr("Time zone"), tr("Dates and times in results are shown in this zone."), |ui| {
                let choices = crate::conn::time_zone_choices();
                let items: Vec<_> = choices.iter().map(|(v, l)| (*v, l.as_str())).collect();
                combo(ui, "set_time_zone", &mut s.time_zone, &items, 320.0)
            })
        });
        c |= group(ui, tr("Start and quit"), |ui| {
            let u = &mut cx.settings.updates;
            let mut auto = u.check_automatically == Some(true);
            let was = auto;
            let c = checks(
                ui,
                vec![
                    (&mut s.restore_session, tr("Reopen the editors of the last session on start")),
                    (&mut s.confirm_exit_with_unsaved, tr("Ask before quitting with unsaved changes")),
                    (&mut auto, tr("Check for updates automatically (at start and every few hours)")),
                ],
            );
            if auto != was {
                u.check_automatically = Some(auto);
            }
            c
        });
        c
    }

    fn editor(&mut self, ui: &mut Ui, cx: &mut AppContext) -> bool {
        let e = &mut cx.settings.editor;
        let case_opts = [(KeywordCase::Upper, tr("UPPER")), (KeywordCase::Lower, tr("lower")), (KeywordCase::Preserve, tr("As typed"))];
        let mut c = group(ui, tr("Editing"), |ui| {
            let mut c = false;
            // Two settings on a line, 24 points apart.
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                let w = label_w(ui);
                c |= form_row(ui, tr("Font size"), w, None, look::FIELD_H, &mut |ui| num(ui, &mut e.font_size, 8.0..=40.0, tr("pt")));
                ui.add_space(24.0);
                c |= form_row(ui, tr("Tab size"), w, None, look::FIELD_H, &mut |ui| num(ui, &mut e.tab_size, 1..=16, ""));
            });
            c |= row(ui, tr("Statement delimiter"), |ui| text(ui, &mut e.statement_delimiter, 80.0, true));
            c |= row_h(ui, tr("Blank line ends a statement"), 28.0, |ui| {
                parts::segmented(
                    ui,
                    &mut e.blank_line_delimiter,
                    &[(BlankLineDelimiter::Never, tr("Never")), (BlankLineDelimiter::Smart, tr("Smart")), (BlankLineDelimiter::Always, tr("Always"))],
                )
            });
            c |= row(ui, tr("Auto-save interval"), |ui| num(ui, &mut e.auto_save_interval_secs, 0..=3600, "s"));
            c |= indented(ui, |ui| {
                check_columns(
                    ui,
                    vec![
                        (&mut e.insert_spaces, tr("Insert spaces instead of tabs")),
                        (&mut e.line_numbers, tr("Show line numbers")),
                        (&mut e.folding, tr("Code folding (statements, block comments)")),
                        (&mut e.mark_occurrences, tr("Highlight occurrences of the word under the cursor")),
                        (&mut e.highlight_current_line, tr("Highlight the current line")),
                    ],
                    vec![
                        (&mut e.word_wrap, tr("Wrap long lines")),
                        (&mut e.auto_close_quotes, tr("Close quotes automatically")),
                        (&mut e.auto_close_brackets, tr("Close brackets automatically")),
                        (&mut e.spell_check, tr("Check spelling in comments and strings")),
                    ],
                    40.0,
                    10.0,
                )
            });
            c
        });
        let a = &mut e.autocomplete;
        let f = &mut e.format;
        c |= two(
            ui,
            0.5,
            |ui| {
                group(ui, tr("Autocomplete"), |ui| {
                    let mut c = checks(
                        ui,
                        vec![
                            (&mut a.enabled, tr("Enable autocomplete")),
                            (&mut a.auto_activate_on_letters, tr("Open automatically while typing")),
                            (&mut a.insert_single_proposal, tr("Insert a single proposal directly")),
                        ],
                    );
                    c |= row(ui, tr("Delay"), |ui| num(ui, &mut a.delay_ms, 0..=2000, "ms"));
                    c |= row(ui, tr("Minimum characters"), |ui| num(ui, &mut a.min_chars, 0..=5, ""));
                    c |= row(ui, tr("Maximum proposals"), |ui| num(ui, &mut a.max_items, 10..=1000, ""));
                    c |= row_h(ui, tr("Keyword case"), 28.0, |ui| parts::segmented(ui, &mut a.keyword_case, &case_opts));
                    c
                })
            },
            |ui| {
                group(ui, tr("Formatter"), |ui| {
                    let mut c = false;
                    {
                        use ternarygrid_config::settings::FormatterKind as K;
                        c |= row(ui, tr("Formatter"), |ui| {
                            combo(
                                ui,
                                "set_fmt_kind",
                                &mut f.formatter,
                                &[(K::Default, tr("Default")), (K::Compact, tr("Compact")), (K::External, tr("External command"))],
                                ui.available_width().min(200.0),
                            )
                        });
                        if f.formatter == K::External {
                            c |= row(ui, tr("Command"), |ui| {
                                look::text_field(ui, &mut f.external_command, ui.available_width().min(260.0), "pg_format -", true)
                                    .on_hover_text(tr("Gets the SQL on standard input and writes the formatted SQL to standard output"))
                                    .changed()
                            });
                        }
                    }
                    c |= row_h(ui, tr("Keyword case"), 28.0, |ui| parts::segmented(ui, &mut f.keyword_case, &case_opts));
                    c |= row(ui, tr("Indent width"), |ui| num(ui, &mut f.indent_width, 1..=8, ""));
                    c |= row(ui, tr("Lines between statements"), |ui| num(ui, &mut f.lines_between_statements, 0..=4, ""));
                    c |= row(ui, tr("Maximum line length"), |ui| num(ui, &mut f.max_line_length, 40..=400, ""));
                    c |= checks(
                        ui,
                        vec![
                            (&mut f.use_tabs, tr("Indent with tabs")),
                            (&mut f.comma_first, tr("Commas at the start of lines")),
                            (&mut f.newline_before_and_or, tr("New line before AND / OR")),
                            (&mut f.statement_only, tr("Without a selection, format only the statement under the cursor")),
                        ],
                    );
                    c
                })
            },
        );
        let p = &mut e.parameters;
        c |= group(ui, tr("Parameters and variables"), |ui| {
            let mut c = false;
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 60.0;
                c |= checks(ui, vec![(&mut p.enabled, tr("Enable parameters")), (&mut p.named_colon, tr(":name parameters")), (&mut p.dollar_brace, tr("${name} variables"))]);
                c |= checks(ui, vec![(&mut p.question_mark, tr("? positional parameters")), (&mut p.prompt_for_values, tr("Ask for values before executing"))]);
            });
            c
        });
        c
    }

    fn results(&mut self, ui: &mut Ui, cx: &mut AppContext) -> bool {
        let r = &mut cx.settings.results;
        let mut c = group(ui, tr("Fetching"), |ui| {
            let mut c = false;
            c |= row(ui, tr("Rows fetched per page"), |ui| num(ui, &mut r.fetch_size, 10..=100_000, ""));
            c |= row_h(ui, tr("Sorting and filtering"), 28.0, |ui| {
                parts::segmented(ui, &mut r.sort_filter_mode, &[(SortFilterMode::Server, tr("On the server (re-query)")), (SortFilterMode::Client, tr("On fetched rows"))])
            });
            c |= row(ui, tr("Confirm \"fetch all\" above"), |ui| num(ui, &mut r.confirm_fetch_all_above, 0..=100_000_000, tr(" rows")));
            c |= row(ui, tr("Auto-refresh interval"), |ui| num(ui, &mut r.auto_refresh_secs, 0..=3600, "s"));
            c
        });
        c |= group(ui, tr("Layout"), |ui| {
            let mut c = false;
            c |= row_h(ui, tr("Results position"), 28.0, |ui| {
                parts::segmented(ui, &mut r.layout, &[(ResultsLayout::Bottom, tr("Below the editor")), (ResultsLayout::Right, tr("Right of the editor"))])
            });
            c |= row_h(ui, tr("Row height"), 28.0, |ui| parts::segmented(ui, &mut r.row_density, &[(RowDensity::Compact, tr("Compact")), (RowDensity::Normal, tr("Normal"))]));
            c |= row(ui, tr("Maximum result tabs"), |ui| num(ui, &mut r.max_result_tabs, 1..=100, ""));
            c |= checks(ui, vec![(&mut r.open_each_query_in_new_tab, tr("Open each query in a new result tab")), (&mut r.show_row_numbers, tr("Show row numbers"))]);
            c
        });
        let (store_mb, global_mb, max_text) = (&mut r.store_memory_limit_mb, &mut r.global_memory_limit_mb, &mut r.max_text_length);
        let f = &mut r.format;
        c |= two(
            ui,
            0.427,
            |ui| {
                group(ui, tr("Memory"), |ui| {
                    // Number fields only: the labels get more room.
                    ui.data_mut(|d| d.insert_temp(narrow_controls_id(), 120.0f32));
                    let mut c = false;
                    c |= row(ui, tr("Memory per result set"), |ui| num(ui, store_mb, 16..=16_384, "MB"));
                    c |= row(ui, tr("Memory for all result sets"), |ui| num(ui, global_mb, 64..=65_536, "MB"));
                    c |= row(ui, tr("Maximum text length"), |ui| num(ui, max_text, 64..=10_000_000, ""));
                    ui.data_mut(|d| d.remove_temp::<f32>(narrow_controls_id()));
                    c
                })
            },
            |ui| {
                group(ui, tr("Value formats"), |ui| {
                    let mut c = false;
                    // The format fields fill the control column.
                    let field_w = |ui: &Ui| ui.available_width();
                    c |= row(ui, tr("NULL text"), |ui| text(ui, &mut f.null_text, 140.0, true));
                    c |= row(ui, tr("Timestamp format"), |ui| {
                        let w = field_w(ui);
                        text(ui, &mut f.datetime_format, w, true)
                    });
                    c |= row_h(ui, tr("Binary values"), 28.0, |ui| {
                        parts::segmented(ui, &mut f.binary_format, &[(BinaryFormat::Hex, tr("Hexadecimal")), (BinaryFormat::Base64, "Base64"), (BinaryFormat::Text, tr("Text"))])
                    });
                    c |= row_h(ui, tr("Booleans"), 28.0, |ui| {
                        parts::segmented(ui, &mut f.bool_style, &[(BoolStyle::TrueFalse, "true / false"), (BoolStyle::Checkbox, "☑ / ☐"), (BoolStyle::OneZero, "1 / 0")])
                    });
                    // Not in the design, kept after its rows.
                    c |= row(ui, tr("Date format"), |ui| {
                        let w = field_w(ui);
                        text(ui, &mut f.date_format, w, true)
                    });
                    c |= row(ui, tr("Time format"), |ui| {
                        let w = field_w(ui);
                        text(ui, &mut f.time_format, w, true)
                    });
                    c |= row(ui, tr("Float decimals"), |ui| {
                        let mut on = f.float_max_decimals.is_some();
                        let mut n = f.float_max_decimals.unwrap_or(6);
                        let mut ch = check(ui, &mut on, tr("Limit"));
                        if on {
                            ch |= num(ui, &mut n, 0..=20, "");
                        }
                        f.float_max_decimals = on.then_some(n);
                        ch
                    });
                    c |= checks(ui, vec![(&mut f.show_fractional_seconds, tr("Show fractional seconds")), (&mut f.thousands_separator, tr("Thousands separator"))]);
                    c
                })
            },
        );
        c
    }

    fn execution(&mut self, ui: &mut Ui, cx: &mut AppContext) -> bool {
        let e = &mut cx.settings.execution;
        let c = group(ui, "", |ui| {
            let mut c = row(ui, tr("Query timeout"), |ui| num(ui, &mut e.query_timeout_secs, 0..=86_400, tr(" s (0 = none)")));
            c |= checks(
                ui,
                vec![
                    (&mut e.stop_on_error, tr("Stop the script on the first error")),
                    (&mut e.confirm_dangerous_statements, tr("Confirm DELETE / UPDATE without WHERE, DROP and TRUNCATE")),
                    (&mut e.smart_commit, tr("Smart commit: switch to manual commit before data changes")),
                ],
            );
            c
        });
        look::callout(ui, Tone::Info, None, tr("Production connections ask before running statements that change data, whatever is set here: see Connection types."), |_| {});
        c
    }

    fn history(&mut self, ui: &mut Ui, cx: &mut AppContext) -> bool {
        let h = &mut cx.settings.history;
        let store = cx.services.history.clone();
        let mut clear = false;
        let c = group(ui, "", |ui| {
            let mut c = checks(ui, vec![(&mut h.enabled, tr("Record executed queries")), (&mut h.redact_secrets, tr("Mask passwords in recorded statements"))]);
            c |= row(ui, tr("Keep at most"), |ui| num(ui, &mut h.max_entries, 0..=10_000_000, tr(" entries")));
            c |= row(ui, tr("Keep for"), |ui| num(ui, &mut h.max_age_days, 0..=36_500, tr(" days")));
            if store.is_some() {
                clear = kit::button_ex(ui, Kind::Danger, Some(icon::TRASH), tr("Clear the whole history"), true).clicked();
            }
            c
        });
        if clear && let Some(store) = store {
            match store.lock().clear(None) {
                Ok(n) => cx.toasts.success(trf("Deleted {} history entries", &[&n])),
                Err(e) => cx.toasts.error(tr("Cannot clear the history"), Some(e.to_string())),
            }
        }
        c
    }

    fn navigator(&mut self, ui: &mut Ui, cx: &mut AppContext) -> bool {
        let n = &mut cx.settings.navigator;
        group(ui, "", |ui| {
            let mut c = false;
            c |= row_h(ui, tr("Order"), 28.0, |ui| {
                parts::segmented(ui, &mut n.sort, &[(NavigatorSort::Alphabetical, tr("Alphabetical")), (NavigatorSort::Natural, tr("As reported by the database"))])
            });
            c |= row_h(ui, tr("Default view of new connections"), 28.0, |ui| {
                parts::segmented(ui, &mut n.default_view, &[(NavigatorView::Simple, tr("Simple")), (NavigatorView::Advanced, tr("Advanced"))])
            });
            c |= checks(
                ui,
                vec![
                    (&mut n.show_system_objects, tr("Show system schemas and objects")),
                    (&mut n.show_connection_details, tr("Show user@host next to connection names")),
                    (&mut n.expand_on_connect, tr("Expand a connection after connecting")),
                ],
            );
            c
        })
    }

    fn keyboard(&mut self, ui: &mut Ui, cx: &mut AppContext) -> bool {
        let km = self.keymap.get_or_insert_with(|| cx.keymap.clone());
        let pal = ternarygrid_ui::pal(ui);
        let mut changed = false;
        ui.horizontal(|ui| {
            ui.label(RichText::new(tr("Preset")).color(pal.text_muted));
            let mut preset = km.preset();
            if parts::segmented(ui, &mut preset, &[(KeymapPreset::TernaryGrid, "TernaryGrid"), (KeymapPreset::Dbeaver, "DBeaver")]) {
                km.set_preset(preset);
                changed = true;
            }
            if kit::button_ex(ui, Kind::Secondary, None, tr("Reset all"), true).clicked() {
                km.reset_all();
                changed = true;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                kit::search_field(ui, egui::Id::new("set_keys_filter"), &mut self.key_filter, tr("Filter…"), 260.0);
            });
        });
        let conflicts = km.conflicts();
        if !conflicts.is_empty() {
            let lines: Vec<String> = conflicts
                .iter()
                .take(5)
                .map(|cf| {
                    let names: Vec<&str> = cf.actions.iter().map(|a| crate::keys::action_label(*a)).collect();
                    let key = cf.shortcut.format(IS_MAC);
                    if names.len() == 2 {
                        trf("{} is bound twice: {}. Only the first one runs.", &[&key, &names.join(", ")])
                    } else {
                        trf("{} is bound {} times: {}. Only the first one runs.", &[&key, &names.len(), &names.join(", ")])
                    }
                })
                .collect();
            look::callout(ui, Tone::Warning, None, &lines.join("\n"), |_| {});
        }
        // Recording a new shortcut: the next key press with a non-modifier key.
        if let Some(action) = self.recording {
            let ev = ui.ctx().input(|i| {
                i.events.iter().find_map(|e| match e {
                    egui::Event::Key { key, pressed: true, modifiers, .. } => Some((*key, *modifiers)),
                    _ => None,
                })
            });
            if let Some((key, m)) = ev {
                if key == egui::Key::Escape && m.is_none() {
                    self.recording = None;
                } else if let Some(sc) = crate::keys::shortcut_of(key, m) {
                    km.set(action, vec![sc]);
                    self.recording = None;
                    changed = true;
                }
                ui.ctx().input_mut(|i| i.consume_key(m, key));
            }
        }
        let filter = self.key_filter.to_lowercase();
        let actions: Vec<Action> = Action::ALL
            .iter()
            .copied()
            .filter(|a| {
                let label = crate::keys::action_label(*a);
                let keys: Vec<String> = km.shortcuts(*a).iter().map(|s| s.format(IS_MAC)).collect();
                filter.is_empty() || label.to_lowercase().contains(&filter) || keys.join(" ").to_lowercase().contains(&filter)
            })
            .collect();
        // Action and Shortcut share what the fixed columns leave (150 and 100 points).
        let fr = ((ui.available_width() - 252.0) / 2.0).floor().max(120.0);
        let cols = [
            look::Col::new(tr("Action"), look::Width::Initial(fr)),
            look::Col::new(tr("Category"), look::Width::Initial(150.0)),
            look::Col::new(tr("Shortcut"), look::Width::Initial(fr)),
            look::Col::new("", look::Width::Rest(100.0)),
        ];
        let recording = self.recording;
        let mut record = None;
        look::table(ui, "set_keys", &cols, actions.len(), |row, i| {
            let a = actions[i];
            row.set_selected(recording == Some(a));
            let keys: Vec<String> = km.shortcuts(a).iter().map(|s| s.format(IS_MAC)).collect();
            look::cell(row, false, |ui| {
                look::cell_text(ui, crate::keys::action_label(a));
            });
            look::cell(row, false, |ui| {
                look::cell_text(ui, RichText::new(tr(a.category().label())).color(pal.text_muted));
            });
            look::cell(row, false, |ui| {
                if recording == Some(a) {
                    look::cell_text(ui, RichText::new(tr("Press a key combination… (Esc cancels)")).italics().color(pal.accent_text));
                } else if keys.is_empty() {
                    look::cell_text(ui, look::mono(ui, "—", pal.text_faint));
                } else {
                    look::cell_text(ui, look::mono(ui, &keys.join("   "), pal.text));
                }
            });
            look::cell(row, false, |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                if kit::icon_button_ex(ui, icon::ROWS, tr("Record a shortcut"), recording == Some(a), 24.0).clicked() {
                    record = Some(a);
                }
                if kit::icon_button_ex(ui, icon::REPEAT, tr("Reset to default"), false, 24.0).clicked() {
                    km.reset(a);
                    changed = true;
                }
                if kit::icon_button_ex(ui, icon::X, tr("Remove the shortcut"), false, 24.0).clicked() {
                    km.unbind(a);
                    changed = true;
                }
            });
        });
        if record.is_some() {
            self.recording = record;
        }
        if changed {
            cx.settings.keymap = km.to_config();
        }
        let _ = Shortcut::parse;
        changed
    }

    fn connection_types(&mut self, ui: &mut Ui, cx: &mut AppContext) -> bool {
        let types = cx.conns.connection_types();
        let pal = ternarygrid_ui::pal(ui);
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 20.0;
            ui.allocate_ui_with_layout(egui::vec2(200.0, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                ui.set_width(200.0);
                ui.spacing_mut().item_spacing.y = 2.0;
                for (i, t) in types.iter().enumerate() {
                    let color = crate::theme::rgb(t.color);
                    let mut lead = |ui: &mut Ui, r: egui::Rect| {
                        ui.painter().rect_filled(r, 3, color);
                    };
                    if list_item(ui, self.selected_type == i, &t.name, &mut lead, 12.0, false, None).clicked() {
                        self.selected_type = i;
                    }
                }
                if kit::button_ex(ui, Kind::Ghost, Some(icon::PLUS), tr("New type"), true).clicked() {
                    let mut t = ConnectionType::development();
                    t.id = format!("custom-{}", uuid::Uuid::new_v4().simple());
                    t.name = tr("Custom").to_string();
                    cx.conns.store.set_connection_type(t);
                    cx.conns.dirty = true;
                    self.selected_type = types.len();
                }
            });
            let Some(mut t) = types.get(self.selected_type).cloned() else { return };
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 14.0;
                let builtin = ConnectionType::builtins().iter().any(|b| b.id == t.id);
                let used = cx.conns.configs().iter().filter(|c| c.connection_type == t.id).count();
                let mut reset = false;
                let title = t.name.clone();
                let mut c = group(ui, &title, |ui| {
                    let mut c = false;
                    c |= row(ui, tr("Name"), |ui| text(ui, &mut t.name, 260.0, false));
                    c |= row(ui, tr("Colour"), |ui| {
                        let mut color = crate::theme::rgb(t.color);
                        swatch(ui, color, 28.0);
                        let b = kit::button_ex(ui, Kind::Secondary, None, tr("Change…"), true);
                        let mut ch = false;
                        egui::Popup::from_toggle_button_response(&b).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
                            if egui::color_picker::color_picker_color32(ui, &mut color, egui::color_picker::Alpha::Opaque) {
                                t.color = [color.r(), color.g(), color.b()];
                                ch = true;
                            }
                        });
                        ch
                    });
                    c |= checks(
                        ui,
                        vec![
                            (&mut t.auto_commit, tr("Auto-commit by default")),
                            (&mut t.confirm_execute, tr("Confirm statements that change data or schema")),
                            (&mut t.confirm_data_change, tr("Confirm saving data changes")),
                            (&mut t.confirm_dangerous, tr("Confirm dangerous statements")),
                        ],
                    );
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        let label = if builtin { tr("Reset to default") } else { tr("Delete type") };
                        let kind = if builtin { Kind::Secondary } else { Kind::Danger };
                        reset = kit::button_ex(ui, kind, None, label, true).clicked();
                        let uses = if used == 1 { tr("1 connection uses it.").to_string() } else { trf("{} connections use it.", &[&used]) };
                        let note = if builtin { trf("Built-in type: {}", &[&uses]) } else { uses };
                        ui.label(RichText::new(note).size(12.5).color(pal.text_muted));
                    });
                    c
                });
                if reset {
                    let n = cx.conns.store.remove_connection_type(&t.id);
                    if n > 0 {
                        cx.toasts.info(trf("{} connection(s) now use the Development type", &[&n]));
                    }
                    cx.conns.dirty = true;
                    c = false;
                    self.selected_type = 0;
                }
                if c {
                    cx.conns.store.set_connection_type(t.clone());
                    cx.conns.dirty = true;
                }
                // How the type marks its connections: the navigator row, the tab, the top bar.
                let marker = crate::theme::connection_marker(t.color);
                let sample = cx.conns.configs().iter().find(|c| c.connection_type == t.id).map(|c| (c.name.clone(), c.kind));
                let sample_kind = sample.as_ref().map(|s| s.1);
                group(ui, tr("How it looks"), |ui| {
                    preview_row(ui, sample.as_ref().map_or(tr("connection"), |s| s.0.as_str()), sample_kind, marker);
                    preview_tab(ui, "report.sql", marker);
                    match marker {
                        Some(m) => {
                            kit::pill(ui, &t.name.to_uppercase(), m);
                        }
                        None => {
                            ui.label(RichText::new(tr("Connections of this type are not marked.")).color(pal.text_muted));
                        }
                    }
                    false
                });
            });
        });
        false
    }

    fn network_profiles(&mut self, ui: &mut Ui, cx: &mut AppContext) -> bool {
        use ternarygrid_core::config::{NetworkProfile, ProxyConfig, SecretSlot, SshConfig};
        let profiles = cx.conns.store.network_profiles.clone();
        let pal = ternarygrid_ui::pal(ui);
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 20.0;
            ui.allocate_ui_with_layout(egui::vec2(220.0, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                ui.set_width(220.0);
                ui.spacing_mut().item_spacing.y = 2.0;
                for p in &profiles {
                    let used = cx.conns.configs().iter().filter(|c| c.network_profile.as_deref() == Some(p.id.as_str())).count();
                    let sel = self.profile.as_ref().is_some_and(|x| x.id == p.id);
                    let mut lead = |ui: &mut Ui, r: egui::Rect| {
                        ui.painter().text(
                            r.center(),
                            egui::Align2::CENTER_CENTER,
                            icon::GLOBE,
                            ternarygrid_ui::style::icon_font(15.0),
                            if sel { pal.accent_text } else { pal.text_muted },
                        );
                    };
                    if list_item(ui, sel, &p.name, &mut lead, 15.0, false, Some(trf("{} used", &[&used]).as_str())).clicked() {
                        self.profile = Some(p.clone());
                        self.profile_ssh_password.clear();
                        self.profile_ssh_passphrase.clear();
                        self.profile_proxy_password.clear();
                    }
                }
                if kit::button_ex(ui, Kind::Ghost, Some(icon::PLUS), tr("New profile"), true).clicked() {
                    self.profile = Some(NetworkProfile {
                        id: uuid::Uuid::new_v4().to_string(),
                        name: tr("New profile").to_string(),
                        ssh: Some(SshConfig { enabled: true, ..Default::default() }),
                        proxy: None,
                    });
                }
            });
            let Some(mut profile) = self.profile.take() else {
                ui.label(RichText::new(tr("Select or create a profile.")).color(pal.text_muted));
                return;
            };
            let p = &mut profile;
            let mut deleted = false;
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 14.0;
                row(ui, tr("Name"), |ui| text(ui, &mut p.name, 300.0, false));
                let more = std::cell::Cell::new(self.ssh_more);
                let ssh = p.ssh.get_or_insert_with(SshConfig::default);
                if ssh.enabled {
                    group_more(ui, "SSH", &more, |ui| {
                        profile_ssh_form(ui, ssh, &mut self.profile_ssh_password, &mut self.profile_ssh_passphrase, more.get());
                        false
                    });
                } else {
                    group(ui, "SSH", |ui| {
                        profile_ssh_form(ui, ssh, &mut self.profile_ssh_password, &mut self.profile_ssh_passphrase, false);
                        false
                    });
                }
                self.ssh_more = more.get();
                group(ui, tr("Proxy"), |ui| {
                    let proxy = p.proxy.get_or_insert_with(ProxyConfig::default);
                    profile_proxy_form(ui, proxy, &mut self.profile_proxy_password);
                    false
                });
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    if kit::button_ex(ui, Kind::Primary, None, tr("Save profile"), false).clicked() {
                        let id = p.id.clone();
                        let ssh_save = p.ssh.as_ref().is_some_and(|s| s.enabled && s.save_credentials);
                        let proxy_save = p.proxy.as_ref().is_some_and(|s| s.enabled && s.save_credentials);
                        let pairs = [
                            (SecretSlot::SshPassword, &self.profile_ssh_password, ssh_save),
                            (SecretSlot::SshPassphrase, &self.profile_ssh_passphrase, ssh_save),
                            (SecretSlot::ProxyPassword, &self.profile_proxy_password, proxy_save),
                        ];
                        for (slot, value, save) in pairs {
                            let key = ternarygrid_config::profile_secret_key(&id, slot);
                            let r = if save && !value.is_empty() {
                                cx.conns.secrets.set(&key, &Secret::new(value.clone()))
                            } else if !save {
                                cx.conns.secrets.delete(&key)
                            } else {
                                Ok(())
                            };
                            if let Err(e) = r
                                && !crate::dialogs::vault::unlock_after_failed_write(cx, &e)
                            {
                                cx.toasts.error(tr("Cannot store the password securely"), Some(e.to_string()));
                            }
                        }
                        cx.conns.store.set_network_profile(p.clone());
                        cx.conns.dirty = true;
                        cx.toasts.success(tr("Profile saved"));
                    }
                    if kit::button_ex(ui, Kind::Danger, None, tr("Delete profile"), false).clicked() {
                        let n = cx.conns.store.remove_network_profile(&p.id);
                        for slot in [SecretSlot::SshPassword, SecretSlot::SshPassphrase, SecretSlot::ProxyPassword] {
                            let _ = cx.conns.secrets.delete(&ternarygrid_config::profile_secret_key(&p.id, slot));
                        }
                        cx.conns.dirty = true;
                        if n > 0 {
                            cx.toasts.info(trf("{} connection(s) now use their own network settings", &[&n]));
                        }
                        deleted = true;
                    }
                });
            });
            if !deleted {
                self.profile = Some(profile);
            }
        });
        false
    }

    fn ai(&mut self, ui: &mut Ui, cx: &mut AppContext) -> bool {
        let provider = cx.settings.ai.provider;
        if self.ai_key_loaded != Some(provider) {
            self.ai_key = String::new();
            self.ai_key_saved = cx.services.secrets.read().ai_api_key(provider.id()).ok().flatten().is_some();
            self.ai_key_loaded = Some(provider);
        }
        let a = &mut cx.settings.ai;
        let mut c = group(ui, "", |ui| {
            let mut c = parts::switch(ui, &mut a.enabled, tr("Enable the AI assistant"), None).changed();
            c |= row_h(ui, tr("Provider"), 28.0, |ui| {
                parts::segmented(ui, &mut a.provider, &[(AiProvider::Anthropic, "Anthropic Claude"), (AiProvider::OpenAiCompatible, tr("OpenAI-compatible server"))])
            });
            c |= row(ui, tr("Model"), |ui| text(ui, &mut a.model, 300.0, true));
            c |= row(ui, tr("Base URL"), |ui| {
                let mut v = a.base_url.clone().unwrap_or_default();
                let ch = look::text_field(ui, &mut v, 300.0, tr("(provider default)"), false).changed();
                a.base_url = if v.trim().is_empty() { None } else { Some(v.trim().to_string()) };
                ch
            });
            c |= row(ui, tr("Maximum answer length"), |ui| num(ui, &mut a.max_tokens, 256..=128_000, tr(" tokens")));
            c |= row_h(ui, tr("Effort"), 28.0, |ui| {
                parts::segmented(
                    ui,
                    &mut a.effort,
                    &[
                        (AiEffort::Low, tr("Low")),
                        (AiEffort::Medium, tr("Medium")),
                        (AiEffort::High, tr("High")),
                        (AiEffort::Xhigh, tr("Very high")),
                        (AiEffort::Max, tr("Maximum")),
                    ],
                )
            });
            c
        });
        c |= group(ui, tr("What it may send"), |ui| {
            checks(
                ui,
                vec![
                    (&mut a.include_schema_context, tr("Send table and column names as context")),
                    (&mut a.send_sample_data, tr("Allow sending sample rows")),
                    (&mut a.agent_queries, tr("Let the assistant run read-only queries (their results are sent to the AI service)")),
                ],
            )
        });
        group(ui, tr("API key"), |ui| {
            row(ui, tr("API key"), |ui| {
                let hint = if self.ai_key_saved { tr("(saved — enter a new key to replace it)") } else { tr("(not set)") };
                look::password_field(ui, &mut self.ai_key, 360.0, hint);
                let can_save = !self.ai_key.trim().is_empty();
                if ui.add_enabled_ui(can_save, |ui| kit::button_ex(ui, Kind::Primary, None, tr("Save key"), true)).inner.clicked() {
                    let key = Secret::new(self.ai_key.trim().to_string());
                    let r = cx.services.secrets.read().set_ai_api_key(provider.id(), Some(&key));
                    match r {
                        Ok(()) => {
                            self.ai_key.clear();
                            self.ai_key_saved = true;
                            cx.toasts.success(tr("API key saved securely"));
                        }
                        Err(e) if crate::dialogs::vault::unlock_after_failed_write(cx, &e) => {}
                        Err(e) => cx.toasts.error(tr("Cannot store the API key"), Some(e.to_string())),
                    }
                }
                if self.ai_key_saved && kit::button_ex(ui, Kind::Danger, None, tr("Remove key"), true).clicked() {
                    let r = cx.services.secrets.read().set_ai_api_key(provider.id(), None);
                    match r {
                        Ok(()) => self.ai_key_saved = false,
                        Err(e) => {
                            crate::dialogs::vault::unlock_after_failed_write(cx, &e);
                        }
                    }
                }
                false
            })
        });
        c
    }

    fn security(&mut self, ui: &mut Ui, cx: &mut AppContext) -> bool {
        let mut moved = false;
        if let Some(r) = self.secrets_job.as_mut().and_then(|j| j.poll()) {
            self.secrets_job = None;
            if let Ok((_, backend)) = &r {
                if let Some(b) = backend {
                    cx.settings.security.secret_backend = *b;
                    moved = true;
                }
                self.old_master.clear();
                self.new_master.clear();
                self.new_master2.clear();
            }
            self.message = Some(r.map(|(m, _)| m));
        }
        let busy = self.secrets_job.is_some();
        let current = cx.services.secrets.read().backend();
        let pal = ternarygrid_ui::pal(ui);
        let changed = group(ui, tr("Secret storage"), |ui| {
            row_h(ui, tr("Secret storage"), 18.0, |ui| {
                let label = match current {
                    SecretBackend::Keychain => tr("Operating system keychain"),
                    SecretBackend::EncryptedFile => tr("Encrypted file with master password"),
                };
                chip(ui, label, pal.accent_text, pal.accent_soft);
                false
            });
            row(ui, tr("Lock the vault after"), |ui| num(ui, &mut cx.settings.security.auto_lock_minutes, 0..=1440, tr(" min idle (0 = never)")))
        });
        let target = match current {
            SecretBackend::Keychain => SecretBackend::EncryptedFile,
            SecretBackend::EncryptedFile => SecretBackend::Keychain,
        };
        let need_master = target == SecretBackend::EncryptedFile;
        let (title, sub) = match target {
            SecretBackend::EncryptedFile => {
                (tr("Move secrets to an encrypted file"), tr("Saved passwords and API keys move from the keychain into one file encrypted with your master password."))
            }
            SecretBackend::Keychain => (tr("Move secrets to the keychain"), tr("Saved passwords and API keys move from the encrypted file into the operating system keychain.")),
        };
        let mut move_clicked = false;
        group_sub(ui, title, sub, |ui| {
            if need_master {
                row(ui, tr("New master password"), |ui| look::password_field(ui, &mut self.new_master, 300.0, "").changed());
                row(ui, tr("Repeat password"), |ui| look::password_field(ui, &mut self.new_master2, 300.0, "").changed());
            }
            move_clicked = ui.add_enabled_ui(!busy, |ui| kit::button_ex(ui, Kind::Secondary, None, tr("Move secrets"), false)).inner.clicked();
            false
        });
        if move_clicked {
            let keys = {
                let mut k = cx.conns.store.all_secret_keys();
                for p in [AiProvider::Anthropic, AiProvider::OpenAiCompatible] {
                    k.push(ternarygrid_config::ai_key(p.id()));
                }
                k
            };
            if need_master && (self.new_master.is_empty() || self.new_master != self.new_master2) {
                self.message = Some(Err(tr("The passwords do not match.").to_string()));
            } else {
                let secrets = cx.services.secrets.clone();
                let vault_file = cx.services.paths.vault_file();
                let master = Secret::new(self.new_master.clone());
                self.message = None;
                self.secrets_job = Some(blocking(cx, move || {
                    let r = if need_master {
                        let vault = std::sync::Arc::new(ternarygrid_config::VaultStore::new(vault_file));
                        let opened = if vault.exists() { vault.unlock(&master) } else { vault.create(&master) };
                        opened.map_err(|e| e.to_string())?;
                        secrets.write().switch_to_vault(vault, &keys, true)
                    } else {
                        let keychain = std::sync::Arc::new(ternarygrid_config::BundledStore::new(ternarygrid_config::KeychainStore::new()));
                        secrets.write().switch_backend(SecretBackend::Keychain, keychain, &keys, true)
                    };
                    let r = r.map_err(|e| e.to_string())?;
                    Ok((trf("{} secret(s) moved", &[&r.migrated.len()]), Some(target)))
                }));
            }
        }
        if current == SecretBackend::EncryptedFile {
            let (mut change, mut lock) = (false, false);
            group(ui, tr("Change master password"), |ui| {
                row(ui, tr("Current password"), |ui| look::password_field(ui, &mut self.old_master, 300.0, "").changed());
                row(ui, tr("New master password"), |ui| look::password_field(ui, &mut self.new_master, 300.0, "").changed());
                row(ui, tr("Repeat password"), |ui| look::password_field(ui, &mut self.new_master2, 300.0, "").changed());
                ui.horizontal(|ui| {
                    change = ui.add_enabled_ui(!busy, |ui| kit::button_ex(ui, Kind::Secondary, None, tr("Change password"), false)).inner.clicked();
                    lock = kit::button_ex(ui, Kind::Ghost, Some(icon::LOCK), tr("Lock now"), false).clicked();
                });
                false
            });
            if change {
                let mgr: parking_lot::RwLockReadGuard<'_, SecretsManager> = cx.services.secrets.read();
                if self.new_master.is_empty() || self.new_master != self.new_master2 {
                    self.message = Some(Err(tr("The passwords do not match.").to_string()));
                } else if let Some(v) = mgr.vault().cloned() {
                    let (old, new) = (Secret::new(self.old_master.clone()), Secret::new(self.new_master.clone()));
                    drop(mgr);
                    self.message = None;
                    self.secrets_job =
                        Some(blocking(cx, move || v.change_master_password(&old, &new).map(|_| (tr("Master password changed").to_string(), None)).map_err(|e| e.to_string())));
                } else {
                    self.message = Some(Err(tr("The vault is not active.").to_string()));
                }
            }
            if lock && let Some(v) = cx.services.secrets.read().vault() {
                v.lock();
            }
        }
        if busy {
            parts::spinner_label(ui, tr("Working…"));
        }
        match &self.message {
            Some(Ok(m)) => {
                look::callout(ui, Tone::Success, None, m, |_| {});
            }
            Some(Err(e)) => look::error(ui, e),
            None => {}
        }
        changed | moved
    }

    fn accessibility(&mut self, ui: &mut Ui, cx: &mut AppContext) -> bool {
        let a = &mut cx.settings.accessibility;
        group(ui, "", |ui| {
            ui.spacing_mut().item_spacing.y = 14.0;
            let mut c = false;
            c |= look::checkbox_sub(ui, &mut a.high_contrast, tr("High contrast"), Some(tr("Stronger borders and text, for bright rooms and low vision."))).changed();
            c |= look::checkbox_sub(ui, &mut a.reduce_motion, tr("Reduce motion"), Some(tr("No fading or sliding windows and pop-ups."))).changed();
            c |= look::checkbox_sub(ui, &mut a.focus_indicator, tr("Always show the keyboard focus"), Some(tr("A ring around the focused control, also after mouse clicks.")))
                .changed();
            c |= check(ui, &mut cx.settings.tasks.scheduler_enabled, tr("Run scheduled tasks while TernaryGrid is open"));
            c
        })
    }

    /// The side navigation: search, sections, the settings folder and reset.
    fn sections_ui(&mut self, ui: &mut Ui, cx: &mut AppContext) -> bool {
        let mut changed = false;
        ui.spacing_mut().item_spacing.y = 2.0;
        let before = self.search.clone();
        kit::search_field(ui, egui::Id::new("settings_search"), &mut self.search, tr("Search settings"), ui.available_width());
        ui.add_space(8.0);
        let query = self.search.trim().to_lowercase();
        if self.search != before {
            ui.data_mut(|d| d.insert_temp(scroll_id(), !query.is_empty()));
        }
        let shown: Vec<Section> = Section::ALL.iter().copied().filter(|s| s.matches(&query)).collect();
        if !shown.is_empty() && !shown.contains(&self.section) {
            self.section = shown[0];
        }
        if shown.is_empty() {
            let p = ternarygrid_ui::pal(ui);
            ui.add_space(4.0);
            ui.label(RichText::new(tr("No setting matches.")).color(p.text_muted));
        }
        let p = ternarygrid_ui::pal(ui);
        for s in shown {
            let on = self.section == s;
            let mut lead = |ui: &mut Ui, r: egui::Rect| {
                let color = if on { p.accent_text } else { p.text_muted };
                ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, s.icon(), ternarygrid_ui::style::icon_font(15.0), color);
            };
            let item = list_item(ui, on, s.label(), &mut lead, 15.0, true, None);
            // While searching: how many of the section's settings match.
            let hits = s.hits(&query);
            if hits > 0 {
                let p = ternarygrid_ui::pal(ui);
                let at = egui::pos2(item.rect.right() - 10.0, item.rect.center().y);
                ui.painter().text(at, egui::Align2::RIGHT_CENTER, hits.to_string(), egui::FontId::proportional(11.5), p.text_faint);
            }
            if item.clicked() {
                self.section = s;
                self.message = None;
                self.recording = None;
                ui.data_mut(|d| d.insert_temp(scroll_id(), !query.is_empty()));
            }
        }
        ui.add_space(12.0);
        let dir = cx.services.paths.config_dir().to_path_buf();
        if kit::button_ex(ui, Kind::Ghost, Some(icon::FOLDER_OPEN), tr("Open settings folder"), true).on_hover_text(dir.display().to_string()).clicked() {
            let _ = open::that_detached(&dir);
        }
        if self.confirm_reset {
            let p = ternarygrid_ui::pal(ui);
            ui.add_space(6.0);
            ui.add(egui::Label::new(RichText::new(tr("Reset every setting to its default? Connections, scripts and passwords are kept.")).color(p.text_muted)).wrap());
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if kit::button_ex(ui, Kind::Danger, None, tr("Reset"), true).clicked() {
                    reset_settings(cx.settings);
                    self.keymap = None;
                    self.confirm_reset = false;
                    self.message = Some(Ok(tr("The settings were reset to their defaults.").to_string()));
                    changed = true;
                }
                if kit::button_ex(ui, Kind::Ghost, None, tr("Cancel"), true).clicked() {
                    self.confirm_reset = false;
                }
            });
        } else if kit::button_ex(ui, Kind::Ghost, Some(icon::REPEAT), tr("Reset all settings…"), true).clicked() {
            self.confirm_reset = true;
        }
        changed
    }
}

/// A host field filling the row, "Port" and the port (80 points).
fn host_port(ui: &mut Ui, host: &mut String, port: &mut u16) {
    let p = ternarygrid_ui::pal(ui);
    let port_label = ui.painter().layout_no_wrap(tr("Port").to_string(), egui::FontId::proportional(13.0), p.text_muted).size().x;
    let w = (ui.available_width() - port_label - 80.0 - 2.0 * ui.spacing().item_spacing.x).max(120.0);
    look::text_field(ui, host, w, "", false);
    ui.label(RichText::new(tr("Port")).size(13.0).color(p.text_muted));
    look::port(ui, port);
}

/// The SSH group of a network profile as designed: host and port, user, authentication, its
/// key file or password and the host key check. The other options (passphrase, saved
/// credentials, local port, keep-alive, switching the tunnel off) are under "More options".
fn profile_ssh_form(ui: &mut Ui, ssh: &mut ternarygrid_core::config::SshConfig, password: &mut String, passphrase: &mut String, more: bool) {
    use ternarygrid_core::config::{HostKeyPolicy, SshAuth};
    if !ssh.enabled {
        check(ui, &mut ssh.enabled, tr("Use SSH tunnel"));
        return;
    }
    row(ui, tr("SSH host"), |ui| {
        host_port(ui, &mut ssh.host, &mut ssh.port);
        false
    });
    row(ui, tr("User"), |ui| text(ui, &mut ssh.user, 260.0, false));
    row_h(ui, tr("Authentication"), 16.0, |ui| {
        ui.spacing_mut().item_spacing.x = 16.0;
        let (pw, key, agent) = (matches!(ssh.auth, SshAuth::Password), matches!(ssh.auth, SshAuth::PublicKey { .. }), matches!(ssh.auth, SshAuth::Agent));
        if kit::radio(ui, pw, tr("Password")).clicked() {
            ssh.auth = SshAuth::Password;
        }
        if kit::radio(ui, key, tr("Private key")).clicked() && !key {
            let home = std::env::var("HOME").unwrap_or_default();
            ssh.auth = SshAuth::PublicKey { key_path: format!("{home}/.ssh/id_ed25519") };
        }
        if kit::radio(ui, agent, tr("SSH agent")).clicked() {
            ssh.auth = SshAuth::Agent;
        }
        false
    });
    match &mut ssh.auth {
        SshAuth::Password => {
            row(ui, tr("Password"), |ui| look::password_field(ui, password, 260.0, "").changed());
        }
        SshAuth::PublicKey { key_path } => {
            row(ui, tr("Private key file"), |ui| {
                let w = (ui.available_width() - 28.0 - ui.spacing().item_spacing.x).max(120.0);
                look::text_field(ui, key_path, w, "", true);
                if kit::icon_button_ex(ui, icon::FOLDER_OPEN, tr("Choose file"), false, 28.0).clicked()
                    && let Some(f) = rfd::FileDialog::new().pick_file()
                {
                    *key_path = f.display().to_string();
                }
                false
            });
        }
        SshAuth::Agent => {}
    }
    row(ui, tr("Host key check"), |ui| {
        let label = |p: HostKeyPolicy| match p {
            HostKeyPolicy::AcceptNew => tr("Trust on first use"),
            HostKeyPolicy::Strict => tr("Only known hosts"),
            HostKeyPolicy::AcceptAll => tr("Accept any key (insecure)"),
        };
        let opts = [HostKeyPolicy::AcceptNew, HostKeyPolicy::Strict, HostKeyPolicy::AcceptAll].map(|p| (p, label(p)));
        combo(ui, "profile_hostkey", &mut ssh.host_key_policy, &opts, 240.0)
    });
    if more {
        if matches!(ssh.auth, SshAuth::PublicKey { .. }) {
            row(ui, tr("Passphrase"), |ui| look::password_field(ui, passphrase, 260.0, "").changed());
        }
        row(ui, tr("Local port"), |ui| {
            let mut lp = ssh.local_port.map(|p| p.to_string()).unwrap_or_default();
            if look::text_field(ui, &mut lp, 140.0, tr("automatic"), false).changed() {
                ssh.local_port = lp.trim().parse().ok();
            }
            false
        });
        row(ui, tr("Keep-alive (s)"), |ui| num(ui, &mut ssh.keepalive_secs, 0..=3600, ""));
        checks(ui, vec![(&mut ssh.save_credentials, tr("Save credentials")), (&mut ssh.enabled, tr("Use SSH tunnel"))]);
    }
}

/// The proxy group of a network profile: "Use proxy", then its type, host, user, password.
fn profile_proxy_form(ui: &mut Ui, proxy: &mut ternarygrid_core::config::ProxyConfig, password: &mut String) {
    use ternarygrid_core::config::ProxyKind;
    check(ui, &mut proxy.enabled, tr("Use proxy"));
    if !proxy.enabled {
        return;
    }
    row_h(ui, tr("Type"), 16.0, |ui| {
        ui.spacing_mut().item_spacing.x = 16.0;
        for (k, l) in [(ProxyKind::Socks5, "SOCKS5"), (ProxyKind::Http, "HTTP CONNECT")] {
            if kit::radio(ui, proxy.kind == k, l).clicked() {
                proxy.kind = k;
            }
        }
        false
    });
    row(ui, tr("Proxy host"), |ui| {
        host_port(ui, &mut proxy.host, &mut proxy.port);
        false
    });
    row(ui, tr("User"), |ui| {
        let mut u = proxy.user.clone().unwrap_or_default();
        let ch = text(ui, &mut u, 260.0, false);
        if ch {
            proxy.user = Some(u.trim().to_string()).filter(|u| !u.is_empty());
        }
        ch
    });
    row(ui, tr("Password"), |ui| look::password_field(ui, password, 260.0, "").changed());
    check(ui, &mut proxy.save_credentials, tr("Save credentials"));
}

/// A navigator row as it looks for a connection of the type (its stripe).
fn preview_row(ui: &mut Ui, name: &str, kind: Option<ternarygrid_core::DbKind>, marker: Option<egui::Color32>) {
    let p = ternarygrid_ui::pal(ui);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(280.0, 26.0), egui::Sense::hover());
    let painter = ui.painter();
    if let Some(m) = marker {
        painter.rect_filled(egui::Rect::from_min_max(egui::pos2(rect.left(), rect.top() + 4.0), egui::pos2(rect.left() + 3.0, rect.bottom() - 4.0)), 2, m);
    }
    let cy = rect.center().y;
    painter.text(egui::pos2(rect.left() + 14.0, cy), egui::Align2::CENTER_CENTER, icon::CARET_RIGHT, ternarygrid_ui::style::icon_font(11.0), p.text_faint);
    let logo = egui::Rect::from_center_size(egui::pos2(rect.left() + 32.0, cy), egui::vec2(16.0, 16.0));
    let kind = kind.unwrap_or(ternarygrid_core::DbKind::Postgres);
    if !crate::dblogo::paint(ui, kind, logo, crate::theme::logo_color(kind, ui.visuals().dark_mode)) {
        ui.painter().text(logo.center(), egui::Align2::CENTER_CENTER, crate::nav::db_icon(kind), ternarygrid_ui::style::icon_font(15.0), p.text_muted);
    }
    let font = ternarygrid_ui::style::medium(ui.ctx(), ternarygrid_ui::style::body_size(ui.ctx()));
    let r = ui.painter().text(egui::pos2(rect.left() + 46.0, cy), egui::Align2::LEFT_CENTER, name, font, p.text);
    ui.painter().circle_filled(egui::pos2(r.right() + 9.0, cy), 3.5, p.ok);
}

/// A tab as it looks for a connection of the type (the line along its top).
fn preview_tab(ui: &mut Ui, label: &str, marker: Option<egui::Color32>) {
    let p = ternarygrid_ui::pal(ui);
    let font = egui::FontId::proportional(ternarygrid_ui::style::body_size(ui.ctx()));
    let g = ui.painter().layout_no_wrap(label.to_string(), font, p.text);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(g.size().x + 50.0, 34.0), egui::Sense::hover());
    let top = egui::CornerRadius { nw: 8, ne: 8, sw: 0, se: 0 };
    ui.painter().rect(rect, top, p.canvas, egui::Stroke::new(1.0, p.border), egui::StrokeKind::Inside);
    let line = marker.unwrap_or(p.accent);
    ui.painter().rect_filled(egui::Rect::from_min_max(egui::pos2(rect.left() + 8.0, rect.top()), egui::pos2(rect.right() - 8.0, rect.top() + 3.0)), 1, line);
    ui.painter().text(egui::pos2(rect.left() + 21.0, rect.center().y), egui::Align2::CENTER_CENTER, icon::FILE_CODE, ternarygrid_ui::style::icon_font(14.0), p.text_muted);
    ui.painter().galley(egui::pos2(rect.left() + 36.0, rect.center().y - g.size().y / 2.0), g, p.text);
}

impl TabView for SettingsTab {
    fn title(&self) -> String {
        tr("Settings").to_string()
    }

    fn icon(&self) -> &'static str {
        icon::GEAR
    }

    fn dedupe_key(&self) -> Option<String> {
        Some("settings".into())
    }

    fn ui(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        let mut changed = false;
        let p = ternarygrid_ui::pal(ui);
        let nav_frame = egui::Frame::new().fill(p.canvas).inner_margin(egui::Margin::symmetric(10, 14)).stroke(egui::Stroke::NONE);
        let nav = egui::Panel::left("settings_sections").resizable(false).exact_size(230.0).frame(nav_frame).show(ui, |ui| {
            changed |= self.sections_ui(ui, cx);
        });
        let r = nav.response.rect;
        ui.painter().vline(r.right(), r.y_range(), egui::Stroke::new(1.0, p.border));
        let query = self.search.trim().to_lowercase();
        ui.data_mut(|d| d.insert_temp(search_id(), query));
        let content_frame = egui::Frame::new().fill(p.canvas).inner_margin(egui::Margin { left: 28, right: 12, top: 20, bottom: 0 });
        egui::CentralPanel::default().frame(content_frame).show(ui, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                ternarygrid_ui::style::form_spacing(ui);
                // Selects and fields are 30 points high in the settings.
                ui.spacing_mut().interact_size.y = look::FIELD_H;
                ui.set_max_width((ui.available_width() - 16.0).min(860.0));
                ui.spacing_mut().item_spacing.y = 6.0;
                kit::semibold_label(ui, self.section.label(), 19.0, p.text);
                if let Some(intro) = self.section.intro() {
                    ui.add_space(1.0);
                    let w = ui.available_width().min(680.0);
                    ui.allocate_ui_with_layout(egui::vec2(w, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                        ui.set_max_width(w);
                        ui.add(egui::Label::new(RichText::new(intro).size(13.0).line_height(Some(19.5)).color(p.text_muted)).wrap());
                    });
                }
                ui.add_space(10.0);
                ui.spacing_mut().item_spacing.y = 14.0;
                changed |= match self.section {
                    Section::General => self.general(ui, cx),
                    Section::Editor => self.editor(ui, cx),
                    Section::Results => self.results(ui, cx),
                    Section::Execution => self.execution(ui, cx),
                    Section::History => self.history(ui, cx),
                    Section::Navigator => self.navigator(ui, cx),
                    Section::Keyboard => self.keyboard(ui, cx),
                    Section::ConnectionTypes => self.connection_types(ui, cx),
                    Section::NetworkProfiles => self.network_profiles(ui, cx),
                    Section::Ai => self.ai(ui, cx),
                    Section::Security => self.security(ui, cx),
                    Section::Accessibility => self.accessibility(ui, cx),
                };
                if self.section != Section::Security
                    && let Some(Ok(m)) = &self.message
                {
                    look::callout(ui, Tone::Success, None, m, |_| {});
                }
                ui.add_space(20.0);
            });
        });
        if changed {
            cx.push(AppCommand::SettingsChanged);
        }
    }

    crate::impl_tab_any!();
}

#[doc(hidden)]
impl SettingsTab {
    /// Cycle through the sections (headless smoke tests).
    pub fn next_section_for_tests(&mut self) {
        let i = Section::ALL.iter().position(|s| *s == self.section).unwrap_or(0);
        self.section = Section::ALL[(i + 1) % Section::ALL.len()];
    }
}
