//! Value panel for the active cell (FR-D5, FR-D7 special value viewers).

use crate::grid::{DataGridState, GridAction, GridOptions};
use crate::model::{ResultModel, RowKey};
use crate::valueview::{array_lines, byte_size_text, decode_image, detect_image, hex_dump, pretty_json, pretty_xml, split_ewkt};
use egui::{RichText, ScrollArea, TextEdit, TextureHandle, TextureOptions, Ui};
use ternarygrid_core::{DataKind, Value, tr, trf};

/// How a value is presented.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ValueMode {
    /// Pick from the column kind / value.
    #[default]
    Auto,
    /// Plain text editor.
    Text,
    /// JSON pretty printed.
    Json,
    /// JSON tree.
    JsonTree,
    /// XML pretty printed.
    Xml,
    /// Hex dump.
    Hex,
    /// Image preview.
    Image,
}

/// Identity of a cell value: row, model column, model generation.
type CellKey = (RowKey, usize, u64);

/// A decoded image preview: texture, original size and format label.
type Preview = (TextureHandle, [u32; 2], &'static str);

/// State of the value panel.
pub struct ValuePanelState {
    /// Presentation mode.
    pub mode: ValueMode,
    /// Wrap long lines.
    pub wrap: bool,
    edit_key: Option<CellKey>,
    /// The row store the text was read from: after a refresh a row key (a
    /// store index) names another record, so typed text is not kept.
    edit_store: usize,
    edit_text: String,
    edit_dirty: bool,
    error: Option<String>,
    image_key: Option<CellKey>,
    image: Option<Result<Preview, String>>,
    last_value: Option<(Option<CellKey>, Value)>,
    /// Nodes of the JSON tree opened or closed by hand (path → open).
    tree_open: std::collections::HashMap<String, bool>,
}

impl Default for ValuePanelState {
    fn default() -> Self {
        ValuePanelState {
            mode: ValueMode::Auto,
            // Long values wrap at the panel's edge.
            wrap: true,
            edit_key: None,
            edit_store: 0,
            edit_text: String::new(),
            edit_dirty: false,
            error: None,
            image_key: None,
            image: None,
            last_value: None,
            tree_open: Default::default(),
        }
    }
}

impl std::fmt::Debug for ValuePanelState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ValuePanelState").field("mode", &self.mode).field("wrap", &self.wrap).finish_non_exhaustive()
    }
}

/// Value viewer / editor of the grid's active cell.
pub struct ValuePanel<'a> {
    model: &'a mut ResultModel,
    grid: &'a DataGridState,
    options: &'a GridOptions,
    state: &'a mut ValuePanelState,
}

/// Colours of the JSON tree, the JSON view's tokens: keys, strings, numbers, and the
/// constants (`true`, `false`, `null`) and counts faint.
struct TreeColors {
    text: egui::Color32,
    faint: egui::Color32,
    key: egui::Color32,
    string: egui::Color32,
    number: egui::Color32,
}

impl TreeColors {
    fn new(ui: &Ui) -> Self {
        let p = ternarygrid_ui::pal(ui);
        let (key, string, number) = crate::views::json_token_colors(ui.visuals().dark_mode);
        TreeColors { text: p.text, faint: p.text_faint, key, string, number }
    }
}

/// One node of the JSON tree and its children: lines of 22 in monospace 12, 16 more
/// in per level; objects and arrays open and close with the kit's caret (the first two
/// levels open), their size faint after them ("6 keys", "2 items").
#[allow(clippy::too_many_arguments)]
fn json_tree(ui: &mut Ui, open: &mut std::collections::HashMap<String, bool>, c: &TreeColors, path: &str, key: &str, v: &serde_json::Value, budget: &mut usize, depth: usize) {
    if *budget == 0 {
        return;
    }
    *budget -= 1;
    let font = egui::FontId::monospace(12.0);
    // The caret's column, then the text.
    let indent = depth as f32 * 16.0;
    let caret_w = 14.0;
    let mut job = egui::text::LayoutJob::default();
    let mut add = |s: &str, color: egui::Color32| job.append(s, 0.0, egui::TextFormat::simple(font.clone(), color));
    let (children, summary) = match v {
        serde_json::Value::Object(map) => (Some(map.len()), if map.len() == 1 { tr("1 key").to_string() } else { trf("{} keys", &[&map.len()]) }),
        serde_json::Value::Array(items) => (Some(items.len()), if items.len() == 1 { tr("1 item").to_string() } else { trf("{} items", &[&items.len()]) }),
        _ => (None, String::new()),
    };
    let is_open = children.is_some() && *open.get(path).unwrap_or(&(depth < 2));
    // Object keys in the key colour, array indexes faint.
    let key_color = if key.parse::<usize>().is_ok() { c.faint } else { c.key };
    match children {
        Some(_) => {
            if !key.is_empty() {
                add(key, key_color);
                add("  ", c.text);
            }
            add(&summary, c.faint);
        }
        None => {
            add(key, key_color);
            add(": ", c.text);
            match v {
                serde_json::Value::String(s) => add(&format!("\"{s}\""), c.string),
                serde_json::Value::Number(n) => add(&n.to_string(), c.number),
                o => add(&o.to_string(), c.faint),
            }
        }
    }
    let galley = ui.fonts_mut(|f| f.layout_job(job));
    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(indent + caret_w + galley.size().x + 8.0, 22.0), if children.is_some() { egui::Sense::click() } else { egui::Sense::hover() });
    if children.is_some() {
        let caret = if is_open { egui_phosphor::regular::CARET_DOWN } else { egui_phosphor::regular::CARET_RIGHT };
        let color = if resp.hovered() { c.text } else { c.faint };
        ui.painter().text(egui::pos2(rect.left() + indent + 5.0, rect.center().y), egui::Align2::CENTER_CENTER, caret, ternarygrid_ui::style::icon_font(11.0), color);
    }
    ui.painter().galley(egui::pos2(rect.left() + indent + caret_w, rect.center().y - galley.size().y / 2.0), galley, c.text);
    if resp.clicked() {
        open.insert(path.to_string(), !is_open);
    }
    if !is_open {
        return;
    }
    match v {
        serde_json::Value::Object(map) => {
            for (k, child) in map {
                json_tree(ui, open, c, &format!("{path}.{k}"), k, child, budget, depth + 1);
            }
        }
        serde_json::Value::Array(items) => {
            for (i, child) in items.iter().enumerate().take(5000) {
                json_tree(ui, open, c, &format!("{path}[{i}]"), &i.to_string(), child, budget, depth + 1);
            }
        }
        _ => {}
    }
}

impl<'a> ValuePanel<'a> {
    /// New value panel.
    pub fn new(model: &'a mut ResultModel, grid: &'a DataGridState, options: &'a GridOptions, state: &'a mut ValuePanelState) -> Self {
        ValuePanel { model, grid, options, state }
    }

    /// Show the panel. Returns actions (e.g. save to file).
    pub fn show(self, ui: &mut Ui) -> Vec<GridAction> {
        let ValuePanel { model, grid, options, state } = self;
        let mut actions = Vec::new();
        let Some((row, col)) = grid.active_cell().filter(|(r, _)| *r < model.row_count()) else {
            ui.label(RichText::new(tr("Select a cell to see its value.")).color(ternarygrid_ui::pal(ui).text_muted));
            return actions;
        };
        let kind = model.column_kind(col);
        let key = model.row_key(row).map(|k| (k, col, model.generation()));
        // Never wait for the fetch task: reuse the last value of this cell when the store is busy.
        let value = match model.try_cell(row, col, std::time::Duration::from_millis(4)) {
            Some(v) => {
                let v = v.unwrap_or_default();
                state.last_value = Some((key, v.clone()));
                v
            }
            None => {
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(10));
                match &state.last_value {
                    Some((k, v)) if *k == key => v.clone(),
                    _ => Value::Null,
                }
            }
        };
        let cell_editable = options.allow_editing && model.can_edit_cell(row, col).is_ok();
        let pal = ternarygrid_ui::pal(ui);
        // 12 between the header, the modes, the value and its actions.
        ui.spacing_mut().item_spacing.y = 12.0;
        ui.horizontal(|ui| {
            ternarygrid_ui::kit::semibold_label(ui, model.column_name(col), 13.0, pal.text);
            if let Some(m) = model.column_meta(col) {
                ui.label(RichText::new(m.type_name.to_lowercase()).color(pal.text_faint));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(RichText::new(trf("row {}", &[&(row + 1)])).color(pal.text_faint));
            });
        });
        // Mode selector with the modes that make sense for the value.
        // Bytes are not text: Auto, Hex (and Image) for them.
        let mut modes = vec![(ValueMode::Auto, tr("Auto"))];
        if !matches!(value, Value::Bytes(_)) {
            modes.push((ValueMode::Text, tr("Text")));
        }
        let is_json = matches!(value, Value::Json(_)) || kind == DataKind::Json;
        if is_json || value.as_str().is_some_and(|s| s.trim_start().starts_with(['{', '['])) {
            modes.push((ValueMode::Json, "JSON"));
            modes.push((ValueMode::JsonTree, tr("JSON tree")));
        }
        if kind == DataKind::Xml || value.as_str().is_some_and(|s| s.trim_start().starts_with('<')) {
            modes.push((ValueMode::Xml, "XML"));
        }
        if let Value::Bytes(b) = &value {
            modes.push((ValueMode::Hex, tr("Hex")));
            if detect_image(b).is_some() {
                modes.push((ValueMode::Image, tr("Image")));
            }
        }
        if !modes.iter().any(|(m, _)| *m == state.mode) {
            state.mode = ValueMode::Auto;
        }
        let shown_mode = |m: ValueMode| match m {
            // NULL has nothing to parse or decode: show the (empty) text editor.
            _ if value.is_null() => ValueMode::Text,
            ValueMode::Auto => match &value {
                Value::Bytes(b) if detect_image(b).is_some() => ValueMode::Image,
                Value::Bytes(_) => ValueMode::Hex,
                Value::Json(_) => ValueMode::Json,
                _ if kind == DataKind::Json => ValueMode::Json,
                _ if kind == DataKind::Xml => ValueMode::Xml,
                _ => ValueMode::Text,
            },
            m => m,
        };
        ui.horizontal(|ui| {
            ternarygrid_ui::parts::segmented_small(ui, &mut state.mode, &modes);
            // Wrapping is for text: not for an image, a hex dump or a tree.
            if matches!(shown_mode(state.mode), ValueMode::Text | ValueMode::Json | ValueMode::Xml) {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ternarygrid_ui::kit::checkbox(ui, &mut state.wrap, tr("Wrap"));
                });
            }
        });
        let mode = shown_mode(state.mode);
        if value.is_null() {
            ui.weak(&options.format.null_text);
        }
        match mode {
            ValueMode::Text | ValueMode::Auto => {
                // New rows or a dictionary change the generation: typed text of the same cell stays.
                let store = std::sync::Arc::as_ptr(model.store()) as usize;
                let same_cell = state.edit_store == store && state.edit_key.as_ref().map(|k| (&k.0, k.1)) == key.as_ref().map(|k| (&k.0, k.1));
                if state.edit_key != key && !(same_cell && state.edit_dirty) || state.edit_store != store {
                    state.edit_key = key;
                    state.edit_store = store;
                    state.edit_text = text_for(&value, kind);
                    state.edit_dirty = false;
                    state.error = None;
                }
                // As tall as the text (up to the panel): Apply / Revert / Set NULL right below it.
                ScrollArea::both().auto_shrink([false, true]).max_height((ui.available_height() - 40.0).max(180.0)).show(ui, |ui| {
                    let te = TextEdit::multiline(&mut state.edit_text)
                        .desired_width(if state.wrap { ui.available_width() } else { f32::INFINITY })
                        .desired_rows(8)
                        .min_size(egui::vec2(0.0, 180.0))
                        .margin(egui::Margin::symmetric(12, 10))
                        .font(egui::FontId::monospace(12.5));
                    let resp = if cell_editable {
                        ui.add(te)
                    } else {
                        let mut ro = state.edit_text.as_str();
                        ui.add(
                            TextEdit::multiline(&mut ro)
                                .desired_width(f32::INFINITY)
                                .min_size(egui::vec2(0.0, 180.0))
                                .margin(egui::Margin::symmetric(12, 10))
                                .font(egui::FontId::monospace(12.5)),
                        )
                    };
                    if resp.changed() {
                        state.edit_dirty = true;
                        state.error = None;
                    }
                });
                ui.horizontal(|ui| {
                    use ternarygrid_ui::kit;
                    ui.spacing_mut().item_spacing.x = 8.0;
                    if cell_editable {
                        let dirty = state.edit_dirty;
                        let apply = ui.add_enabled_ui(dirty, |ui| kit::button_full(ui, kit::Kind::Primary, None, tr("Apply"), true, false).clicked()).inner;
                        if apply {
                            match model.set_cell(row, col, &state.edit_text) {
                                Ok(()) => {
                                    state.edit_dirty = false;
                                    state.edit_key = model.row_key(row).map(|k| (k, col, model.generation()));
                                }
                                Err(e) => state.error = Some(e),
                            }
                        }
                        let revert = ui.add_enabled_ui(dirty, |ui| kit::button_full(ui, kit::Kind::Ghost, None, tr("Revert"), true, false).clicked()).inner;
                        if revert {
                            state.edit_text = text_for(&value, kind);
                            state.edit_dirty = false;
                        }
                        if kit::button_full(ui, kit::Kind::Ghost, None, tr("Set NULL"), true, false).clicked() {
                            match model.set_null(row, col) {
                                // The text shows the cell again (typed text is gone).
                                Ok(()) => {
                                    state.edit_dirty = false;
                                    state.edit_key = None;
                                }
                                Err(e) => state.error = Some(e),
                            }
                        }
                    } else if let Some(r) = model.read_only_reason() {
                        ui.label(RichText::new(format!("{} {}", egui_phosphor::regular::LOCK_SIMPLE, r.short_label())).color(pal.warn));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let n = state.edit_text.chars().count();
                        let count = if n == 1 { tr("1 character").to_string() } else { trf("{} characters", &[&n]) };
                        ui.label(RichText::new(count).color(pal.text_faint));
                    });
                });
                if let Some(e) = &state.error {
                    ternarygrid_ui::parts::status(ui, ternarygrid_ui::parts::Tone::Error, e);
                }
            }
            ValueMode::Json => {
                let text = value.to_text();
                match pretty_json(&text) {
                    Ok(p) => readonly_text(ui, &p, state.wrap),
                    Err(e) => {
                        ui.colored_label(ui.visuals().error_fg_color, e);
                        readonly_text(ui, &text, state.wrap);
                    }
                }
            }
            ValueMode::JsonTree => match serde_json::from_str::<serde_json::Value>(&value.to_text()) {
                Ok(j) => {
                    ternarygrid_ui::parts::bordered(ui).inner_margin(egui::Margin::symmetric(10, 8)).show(ui, |ui| {
                        ScrollArea::both().auto_shrink([false, true]).show(ui, |ui| {
                            ui.spacing_mut().item_spacing.y = 0.0;
                            let mut budget = 20_000usize;
                            let colors = TreeColors::new(ui);
                            json_tree(ui, &mut state.tree_open, &colors, "", "", &j, &mut budget, 0);
                            if budget == 0 {
                                ui.label(RichText::new(tr("The rest of the tree is not shown.")).color(ternarygrid_ui::pal(ui).text_muted));
                            }
                        });
                    });
                }
                Err(e) => {
                    ui.colored_label(ui.visuals().error_fg_color, trf("Invalid JSON: {}", &[&e]));
                }
            },
            ValueMode::Xml => {
                let text = value.to_text();
                match pretty_xml(&text) {
                    Ok(p) => readonly_text(ui, &p, state.wrap),
                    Err(e) => {
                        ui.colored_label(ui.visuals().error_fg_color, e);
                        readonly_text(ui, &text, state.wrap);
                    }
                }
            }
            ValueMode::Hex => {
                if let Value::Bytes(b) = &value {
                    size_row(ui, &mut actions, row, col, b, None);
                    readonly_text(ui, &hex_dump(b, 64 * 1024), false);
                }
            }
            ValueMode::Image => {
                if let Value::Bytes(b) = &value {
                    if state.image_key != key {
                        state.image_key = key;
                        state.image = Some(decode_image(b, 2048).map(|d| {
                            // Small images are shown enlarged: their pixels sharp, not blurred.
                            let small = d.image.size[0].max(d.image.size[1]) <= 256;
                            let filter = if small { TextureOptions::NEAREST } else { TextureOptions::LINEAR };
                            let tex = ui.ctx().load_texture(format!("ternarygrid-value-{row}-{col}"), d.image, filter);
                            (tex, d.original_size, d.kind.label())
                        }));
                    }
                    // "74 B · PNG 8 × 6" on one line.
                    let detail = match &state.image {
                        Some(Ok((_, size, label))) => Some(format!("{label} {} × {}", size[0], size[1])),
                        _ => None,
                    };
                    size_row(ui, &mut actions, row, col, b, detail.as_deref());
                    match &state.image {
                        Some(Ok((tex, _, _))) => {
                            // In a 220 high box on a checkerboard (transparency shows), scaled to
                            // fit: larger ones down, small ones up (whole steps, at most 8 times).
                            let w = ui.available_width();
                            let (rect, _) = ui.allocate_exact_size(egui::vec2(w, 220.0), egui::Sense::hover());
                            let s = tex.size_vec2();
                            let inner = rect.shrink(12.0);
                            let fit = (inner.width() / s.x).min(inner.height() / s.y);
                            let scale = if fit >= 2.0 { fit.floor().min(8.0) } else { fit.clamp(0.05, 1.0) };
                            let img = egui::emath::GuiRounding::round_to_pixels(egui::Rect::from_center_size(rect.center(), s * scale), ui.pixels_per_point());
                            let painter = ui.painter().with_clip_rect(rect.shrink(1.0));
                            painter.rect_filled(rect, 8, pal.surface);
                            checkerboard(&painter, img, pal.subtle, pal.surface);
                            painter.image(tex.id(), img, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
                            ui.painter().rect_stroke(rect, 8, egui::Stroke::new(1.0, pal.border), egui::StrokeKind::Inside);
                        }
                        Some(Err(e)) => {
                            ui.colored_label(ui.visuals().error_fg_color, e);
                        }
                        None => {}
                    }
                }
            }
        }
        // Extra details for special kinds.
        match (&value, kind) {
            (Value::Uuid(u), _) => {
                ui.separator();
                ui.monospace(u.hyphenated().to_string().to_uppercase());
                ui.monospace(u.simple().to_string());
            }
            (Value::Array(items), _) => {
                ui.separator();
                ui.label(trf("{} elements", &[&items.len()]));
                readonly_text(ui, &array_lines(items), false);
            }
            (Value::Text(s), DataKind::Geometry) => {
                let (srid, wkt) = split_ewkt(s);
                ui.separator();
                if let Some(srid) = srid {
                    ui.label(trf("SRID {}", &[&srid]));
                }
                let geom = wkt.split(['(', ' ']).next().unwrap_or("");
                ui.weak(trf("Geometry type: {}", &[&geom]));
            }
            (Value::Text(_), DataKind::Interval) => {
                ui.separator();
                ui.weak(tr("Interval"));
            }
            _ => {}
        }
        actions
    }
}

/// Squares of 8 points in two colours over `rect` (behind an image: its transparency shows).
fn checkerboard(painter: &egui::Painter, rect: egui::Rect, a: egui::Color32, b: egui::Color32) {
    const SQ: f32 = 8.0;
    painter.rect_filled(rect, 0.0, b);
    let (cols, rows) = ((rect.width() / SQ).ceil() as usize, (rect.height() / SQ).ceil() as usize);
    for r in 0..rows.min(64) {
        for c in 0..cols.min(128) {
            if (r + c) % 2 == 0 {
                let min = rect.min + egui::vec2(c as f32 * SQ, r as f32 * SQ);
                let sq = egui::Rect::from_min_size(min, egui::vec2(SQ, SQ)).intersect(rect);
                painter.rect_filled(sq, 0.0, a);
            }
        }
    }
}

/// The value's size (and `detail`: the image's format and size) muted and, at the right,
/// Save to file….
fn size_row(ui: &mut Ui, actions: &mut Vec<GridAction>, row: usize, col: usize, bytes: &[u8], detail: Option<&str>) {
    let ext = detect_image(bytes).map_or("bin", |k| k.extension());
    let muted = ternarygrid_ui::pal(ui).text_muted;
    let text = match detail {
        Some(d) => format!("{} · {d}", byte_size_text(bytes.len())),
        None => byte_size_text(bytes.len()),
    };
    ui.horizontal(|ui| {
        ui.label(RichText::new(text).size(12.0).color(muted));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ternarygrid_ui::kit::button_full(ui, ternarygrid_ui::kit::Kind::Ghost, Some(egui_phosphor::regular::DOWNLOAD_SIMPLE), tr("Save to file…"), true, false).clicked() {
                actions.push(GridAction::SaveValueToFile { row, col, extension: ext.to_string() });
            }
        });
    });
}

fn text_for(v: &Value, kind: DataKind) -> String {
    match v {
        Value::Null => String::new(),
        Value::Json(s) if kind == DataKind::Json => s.to_string(),
        other => other.to_text().into_owned(),
    }
}

fn readonly_text(ui: &mut Ui, text: &str, wrap: bool) {
    ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
        let mut s = text;
        let mut te = TextEdit::multiline(&mut s).font(egui::TextStyle::Monospace).desired_width(f32::INFINITY);
        if !wrap {
            te = te.clip_text(false);
        }
        ui.add(te);
    });
}
