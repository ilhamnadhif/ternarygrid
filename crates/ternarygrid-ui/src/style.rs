//! Design tokens (colours, type, spacing) of TernaryGrid, shared with its
//! sibling apps TernaryWire and TernaryShell: neutral surfaces, one accent —
//! the teal of the app icon — and soft tints instead of saturated fills.
//!
//! Every screen draws with these tokens: [`Palette`] for colours (light and
//! dark), [`medium`] / [`semibold`] for type, the components in `kit.rs`.

use egui::{Color32, Context, CornerRadius, FontFamily, FontId, Stroke, Ui, vec2};

/// Colours of one theme.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    /// Panels around the content: navigator, tab content, dialogs' backgrounds.
    pub canvas: Color32,
    /// Window chrome: top bar, toolbar, tab strip, status bar.
    pub chrome: Color32,
    /// Raised surfaces and content: inputs, cards, menus, the grid and the editor.
    pub surface: Color32,
    /// Hovered rows and quiet button fills.
    pub hover: Color32,
    /// Pressed state and subtle fills (segmented control track, count pills).
    pub subtle: Color32,
    /// Headers of tables and the grid.
    pub header: Color32,
    pub border: Color32,
    pub border_strong: Color32,
    pub text: Color32,
    pub text_muted: Color32,
    /// Hints and placeholders only (about 3.5:1).
    pub text_faint: Color32,
    /// Fill of primary buttons (white text on it: at least 4.5:1).
    pub accent: Color32,
    pub accent_hover: Color32,
    /// Text on an accent fill.
    pub on_accent: Color32,
    /// Background of selected rows and "on" toggles.
    pub accent_soft: Color32,
    /// Accent-coloured text (readable on the canvas and on `accent_soft`).
    pub accent_text: Color32,
    pub ok: Color32,
    pub warn: Color32,
    pub err: Color32,
    pub info: Color32,
    /// Lines between the rows and cells of tables and the grid.
    pub line: Color32,
    /// The selected cell of the grid.
    pub sel: Color32,
    /// A changed (not yet saved) cell, the pending changes bar.
    pub modified: Color32,
    /// Behind production markers (pills, error callouts).
    pub prod_soft: Color32,
    /// Every other row of tables and the grid.
    pub stripe: Color32,
    /// The selected segment of a segmented control, raised above its `subtle` track (lighter
    /// than the track in both themes: the surface in light, a lifted grey in dark).
    pub thumb: Color32,
}

const fn rgb(c: u32) -> Color32 {
    Color32::from_rgb((c >> 16) as u8, (c >> 8) as u8, c as u8)
}

pub const LIGHT: Palette = Palette {
    canvas: rgb(0xf5f5f7),
    chrome: rgb(0xececef),
    surface: rgb(0xffffff),
    hover: rgb(0xe8e8ec),
    subtle: rgb(0xe1e1e6),
    header: rgb(0xf5f5f7),
    border: rgb(0xdedee3),
    border_strong: rgb(0xcacace),
    text: rgb(0x17171a),
    text_muted: rgb(0x55555f),
    text_faint: rgb(0x767680),
    accent: rgb(0x0f7a68),
    accent_hover: rgb(0x0c6657),
    on_accent: Color32::WHITE,
    accent_soft: rgb(0xe4e4e9),
    accent_text: rgb(0x0b6a5a),
    ok: rgb(0x117040),
    warn: rgb(0x8f5300),
    err: rgb(0xbe2722),
    info: rgb(0x1f5fbf),
    line: rgb(0xe9e9ee),
    sel: rgb(0xd9eeea),
    modified: rgb(0xf6ecd6),
    prod_soft: rgb(0xfce7e6),
    stripe: rgb(0xf9f9fb),
    thumb: rgb(0xffffff),
};

pub const DARK: Palette = Palette {
    // Obsidian: calm deep neutral surfaces shared across the Ternary family —
    // chrome #0d0d0f, navigator/panels #111114, grid & SQL editor #151517.
    canvas: rgb(0x111114),
    chrome: rgb(0x0d0d0f),
    surface: rgb(0x151517),
    hover: rgb(0x1f1f23),
    subtle: rgb(0x26262a),
    header: rgb(0x111114),
    border: rgb(0x27272b),
    border_strong: rgb(0x36363b),
    text: rgb(0xededf0),
    text_muted: rgb(0xa1a1aa),
    text_faint: rgb(0x75757e),
    accent: rgb(0x0f8572),
    accent_hover: rgb(0x0c7362),
    on_accent: Color32::WHITE,
    accent_soft: rgb(0x242428),
    accent_text: rgb(0x52d1ba),
    ok: rgb(0x4dc48c),
    warn: rgb(0xf0ad42),
    err: rgb(0xf06560),
    info: rgb(0x6aa5ff),
    line: rgb(0x1f1f23),
    sel: rgb(0x17332e),
    modified: rgb(0x3d3528),
    prod_soft: rgb(0x3c2224),
    stripe: rgb(0x18181b),
    thumb: rgb(0x323238),
};

impl Palette {
    /// Barely visible fill for hovered list rows.
    pub fn faint_row(&self) -> Color32 {
        soft(self.hover, 0.6)
    }
}

/// Semantic icon colours calibrated per theme so every toolbar and menu icon
/// has at least 4.5:1 contrast against `canvas`, `chrome`, `surface` and `hover`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct IconPalette {
    pub blue: Color32,
    pub cyan: Color32,
    pub teal: Color32,
    pub green: Color32,
    pub amber: Color32,
    pub orange: Color32,
    pub red: Color32,
    pub purple: Color32,
    pub slate: Color32,
}

pub const ICONS_LIGHT: IconPalette = IconPalette {
    blue: rgb(0x1d4ed8),
    cyan: rgb(0x0369a1),
    teal: rgb(0x0c6657),
    green: rgb(0x157337),
    amber: rgb(0x8f5300),
    orange: rgb(0xa84705),
    red: rgb(0xbe2722),
    purple: rgb(0x763fd0),
    slate: rgb(0x55555f),
};

pub const ICONS_DARK: IconPalette = IconPalette {
    blue: rgb(0x6aa5ff),
    cyan: rgb(0x62ccd8),
    teal: rgb(0x52d1ba),
    green: rgb(0x4dc48c),
    amber: rgb(0xf0ad42),
    orange: rgb(0xf0894a),
    red: rgb(0xf06560),
    purple: rgb(0xc49bff),
    slate: rgb(0xa1a1aa),
};

/// The semantic icon palette for the light or dark theme.
pub fn icon_palette(dark: bool) -> IconPalette {
    if dark { ICONS_DARK } else { ICONS_LIGHT }
}

/// The semantic icon palette for the theme `ui` is drawn with.
pub fn icons(ui: &Ui) -> IconPalette {
    icon_palette(ui.visuals().dark_mode)
}

/// Semantic colour for a Phosphor glyph in toolbars and menus (`None` keeps the
/// neutral muted colour for chrome controls such as `⋯`, `×`, and carets).
pub fn icon_color(glyph: &str, dark: bool) -> Option<Color32> {
    use egui_phosphor::regular as i;
    let c = icon_palette(dark);
    Some(match glyph {
        i::PLUGS | i::TRASH | i::STOP | i::STOP_CIRCLE | i::X_CIRCLE | i::MINUS_CIRCLE | i::WARNING_OCTAGON => c.red,
        i::PLUG
        | i::PLUGS_CONNECTED
        | i::PLUS
        | i::PLUS_CIRCLE
        | i::FILE_PLUS
        | i::PLAY
        | i::PLAY_CIRCLE
        | i::CHECK
        | i::CHECK_CIRCLE
        | i::LIST_CHECKS
        | i::CHART_BAR
        | i::TREE_STRUCTURE
        | i::ARROW_CIRCLE_UP => c.green,
        i::ARROW_CLOCKWISE | i::ARROWS_CLOCKWISE | i::TABLE | i::ROWS | i::EYE | i::FLOPPY_DISK | i::LIST_BULLETS => c.blue,
        i::COPY | i::DATABASE | i::INFO | i::FILE_CODE | i::FILE_SQL => c.cyan,
        i::EXPORT
        | i::UPLOAD_SIMPLE
        | i::DOWNLOAD
        | i::DOWNLOAD_SIMPLE
        | i::ROWS_PLUS_TOP
        | i::LIST_DASHES
        | i::ACTIVITY
        | i::TERMINAL
        | i::TERMINAL_WINDOW
        | i::ARROW_LINE_DOWN => c.teal,
        i::FOLDER
        | i::FOLDER_OPEN
        | i::FOLDER_SIMPLE
        | i::FOLDERS
        | i::FOLDER_PLUS
        | i::TIMER
        | i::CLOCK
        | i::CLOCK_COUNTER_CLOCKWISE
        | i::HOURGLASS
        | i::ARROW_FAT_LINE_DOWN
        | i::ARROW_FAT_LINE_UP
        | i::ARROW_U_UP_LEFT
        | i::BRACKETS_CURLY
        | i::CALCULATOR
        | i::KEY
        | i::PENCIL
        | i::PENCIL_SIMPLE
        | i::NOTE_PENCIL
        | i::SUN
        | i::PUSH_PIN
        | i::FILE_TEXT
        | i::WARNING => c.amber,
        i::GIT_DIFF | i::ARROWS_LEFT_RIGHT => c.orange,
        i::HASH | i::MAP_TRIFOLD | i::STACK | i::SPARKLE | i::ROBOT | i::MAGIC_WAND | i::TEXT_INDENT | i::FUNNEL | i::FUNNEL_SIMPLE | i::MOON | i::FLASK => c.purple,
        _ => return None,
    })
}

/// The palette of the light or the dark theme.
pub fn palette(dark: bool) -> Palette {
    if dark { DARK } else { LIGHT }
}

/// The palette of the theme `ui` is drawn with.
pub fn pal(ui: &Ui) -> Palette {
    palette(ui.visuals().dark_mode)
}

/// A colour at `alpha` (0–1) opacity, for soft fills behind coloured text and icons.
pub fn soft(c: Color32, alpha: f32) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), (alpha.clamp(0.0, 1.0) * 255.0) as u8)
}

// ---------------------------------------------------------------------------------------------
// Type
// ---------------------------------------------------------------------------------------------

/// Font family of labels that need a little weight (tabs, buttons, row titles).
pub const MEDIUM: &str = "ui-medium";
/// Font family of titles and headings.
pub const SEMIBOLD: &str = "ui-semibold";

/// Small text: details, hints, the status bar.
pub const SMALL: f32 = 11.0;

/// The body text size of `ctx` (it follows the UI font size setting).
pub fn body_size(ctx: &Context) -> f32 {
    ctx.global_style().text_styles.get(&egui::TextStyle::Body).map(|f| f.size).unwrap_or(13.0)
}

fn family_or_proportional(ctx: &Context, name: &str, size: f32) -> FontId {
    let family = FontFamily::Name(name.into());
    // A family that is not registered would panic (tests and examples run
    // with egui's default fonts): fall back to the proportional font.
    if ctx.fonts(|f| f.definitions().families.contains_key(&family)) { FontId::new(size, family) } else { FontId::proportional(size) }
}

/// The medium weight at `size` (the regular one without it).
pub fn medium(ctx: &Context, size: f32) -> FontId {
    family_or_proportional(ctx, MEDIUM, size)
}

/// The semibold weight at `size` (the regular one without it).
pub fn semibold(ctx: &Context, size: f32) -> FontId {
    family_or_proportional(ctx, SEMIBOLD, size)
}

/// How much smaller a Phosphor glyph is drawn than its nominal size so it matches the design's
/// icons of that size (Phosphor's glyphs fill more of their box than the design's SVG icons).
pub const ICON_SCALE: f32 = 0.87;

/// Icons as large as the design's icons of `size` points (14 beside 13-point text, 15 in a
/// 28-point icon button, 18 on the rail): the Phosphor font, a fallback of proportional text,
/// at [`ICON_SCALE`] of `size`.
pub fn icon_font(size: f32) -> FontId {
    FontId::proportional(size * ICON_SCALE)
}

/// Register the [`MEDIUM`] and [`SEMIBOLD`] families with the fonts named
/// `medium` and `semibold` in `fonts` (the same font for both when there is no
/// semibold one), each followed by the proportional fallbacks (icons, emoji).
pub fn add_weight_families(fonts: &mut egui::FontDefinitions, medium: &str, semibold: &str) {
    let fallbacks: Vec<String> = fonts.families.get(&FontFamily::Proportional).map(|f| f.iter().skip(1).cloned().collect()).unwrap_or_default();
    for (family, font) in [(MEDIUM, medium), (SEMIBOLD, semibold)] {
        let keys = std::iter::once(font.to_string()).chain(fallbacks.iter().cloned()).collect();
        fonts.families.insert(FontFamily::Name(family.into()), keys);
    }
}

// ---------------------------------------------------------------------------------------------
// Shape and spacing
// ---------------------------------------------------------------------------------------------

/// Corner radius of controls, menus' items and rows.
pub const RADIUS: u8 = 6;
/// Corner radius of windows and dialogs.
pub const RADIUS_LG: u8 = 10;
/// Width of the label column in forms.
pub const LABEL_W: f32 = 128.0;
/// Height of a form control (text field, select box, button) in dialogs.
pub const CONTROL_H: f32 = 30.0;
/// Height of a small button (toolbars, footers).
pub const SMALL_H: f32 = 26.0;

/// Roomier spacing for the content of a dialog or form.
pub fn form_spacing(ui: &mut Ui) {
    let s = ui.spacing_mut();
    s.interact_size.y = CONTROL_H;
    s.item_spacing = vec2(8.0, 8.0);
    // Select boxes: their text 11 points in (the design's 10 and the hairline).
    s.button_padding = vec2(11.0, 5.0);
}

/// Focus of a control drawn by hand: a 1 px accent border and a soft accent
/// halo just outside it (calmer than a thick ring, which reads like an error).
pub fn focus_halo(ui: &Ui, rect: egui::Rect, radius: u8) {
    let p = pal(ui);
    let halo = soft(p.accent, if ui.visuals().dark_mode { 0.30 } else { 0.22 });
    let painter = ui.painter();
    painter.rect_stroke(rect, CornerRadius::same(radius), Stroke::new(3.0, halo), egui::StrokeKind::Outside);
    painter.rect_stroke(rect, CornerRadius::same(radius), Stroke::new(1.0, p.accent), egui::StrokeKind::Inside);
}

/// A section heading inside a form or dialog.
pub fn section(ui: &mut Ui, title: &str) {
    ui.add_space(4.0);
    ui.label(egui::RichText::new(title).font(semibold(ui.ctx(), body_size(ui.ctx()))));
    ui.add_space(2.0);
}

/// A form row: `label` in a column of [`LABEL_W`], then the control.
pub fn row<R>(ui: &mut Ui, label: &str, add: impl FnOnce(&mut Ui) -> R) -> R {
    ui.horizontal(|ui| {
        let p = pal(ui);
        ui.add_sized([LABEL_W, ui.spacing().interact_size.y], egui::Label::new(egui::RichText::new(label).color(p.text_muted)).truncate());
        add(ui)
    })
    .inner
}

// ---------------------------------------------------------------------------------------------
// Contrast (WCAG 2)
// ---------------------------------------------------------------------------------------------

/// Relative luminance of an opaque colour.
pub fn luminance(c: Color32) -> f32 {
    let ch = |v: u8| {
        let v = v as f32 / 255.0;
        if v <= 0.040_45 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * ch(c.r()) + 0.7152 * ch(c.g()) + 0.0722 * ch(c.b())
}

/// Contrast ratio of two opaque colours (1–21).
pub fn contrast(a: Color32, b: Color32) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// `a` over `b` at `t` (0: `b`, 1: `a`), opaque.
pub fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let m = |x: u8, y: u8| (y as f32 + (x as f32 - y as f32) * t).round() as u8;
    Color32::from_rgb(m(a.r(), b.r()), m(a.g(), b.g()), m(a.b(), b.b()))
}

/// `fg`, darkened (light theme) or lightened (dark theme) just enough to be read on `bg`
/// (4.5:1): a user's light marker colour as text on its own tint.
pub fn readable_on(fg: Color32, bg: Color32, dark: bool) -> Color32 {
    let toward = if dark { Color32::WHITE } else { Color32::BLACK };
    let fg = Color32::from_rgb(fg.r(), fg.g(), fg.b());
    (0..=10).map(|i| mix(toward, fg, i as f32 / 10.0)).find(|c| contrast(*c, bg) >= 4.5).unwrap_or(toward)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Text and controls meet WCAG AA in both themes: body text 4.5:1, hints
    /// and non-text UI (borders of the accent fill, the faint text) 3:1.
    #[test]
    fn palettes_are_legible() {
        for (name, p) in [("light", LIGHT), ("dark", DARK)] {
            let text = [("text", p.text), ("text_muted", p.text_muted), ("accent_text", p.accent_text), ("err", p.err)];
            for (bg_name, bg) in [("canvas", p.canvas), ("chrome", p.chrome), ("surface", p.surface), ("hover", p.hover)] {
                for (fg_name, fg) in text {
                    let c = contrast(fg, bg);
                    assert!(c >= 4.5, "{name}: {fg_name} on {bg_name} is {c:.2}:1");
                }
                let c = contrast(p.text_faint, bg);
                assert!(c >= 3.0, "{name}: text_faint on {bg_name} is {c:.2}:1");
            }
            for (fg_name, fg) in [("text", p.text), ("accent_text", p.accent_text)] {
                let c = contrast(fg, p.accent_soft);
                assert!(c >= 4.5, "{name}: {fg_name} on accent_soft is {c:.2}:1");
            }
            for (fill_name, fill) in [("accent", p.accent), ("accent_hover", p.accent_hover)] {
                let c = contrast(p.on_accent, fill);
                assert!(c >= 4.5, "{name}: on_accent on {fill_name} is {c:.2}:1");
            }
            let c = contrast(p.accent, p.canvas);
            assert!(c >= 3.0, "{name}: the accent fill against the canvas is {c:.2}:1");
            let ic = icon_palette(name == "dark");
            for (ic_name, fg) in [
                ("blue", ic.blue),
                ("cyan", ic.cyan),
                ("teal", ic.teal),
                ("green", ic.green),
                ("amber", ic.amber),
                ("orange", ic.orange),
                ("red", ic.red),
                ("purple", ic.purple),
                ("slate", ic.slate),
            ] {
                for (bg_name, bg) in [("canvas", p.canvas), ("chrome", p.chrome), ("surface", p.surface), ("hover", p.hover)] {
                    let c = contrast(fg, bg);
                    assert!(c >= 4.5, "{name}: icon {ic_name} on {bg_name} is {c:.2}:1");
                }
            }
        }
    }

    /// Without the families registered (egui's default fonts), the helpers
    /// fall back to the proportional font instead of panicking.
    #[test]
    fn weights_fall_back_without_their_families() {
        let ctx = Context::default();
        let mut out = ctx.run_ui(Default::default(), |ui| {
            assert_eq!(medium(ui.ctx(), 13.0), FontId::proportional(13.0));
            assert_eq!(semibold(ui.ctx(), 13.0), FontId::proportional(13.0));
            ui.label(egui::RichText::new("x").font(semibold(ui.ctx(), 13.0)));
        });
        out.textures_delta.clear();
    }

    #[test]
    fn weight_families_are_registered() {
        let mut fonts = egui::FontDefinitions::default();
        let first = fonts.families[&FontFamily::Proportional][0].clone();
        add_weight_families(&mut fonts, &first, &first);
        let ctx = Context::default();
        ctx.set_fonts(fonts);
        let mut out = ctx.run_ui(Default::default(), |ui| {
            assert_eq!(medium(ui.ctx(), 13.0).family, FontFamily::Name(MEDIUM.into()));
            ui.label(egui::RichText::new("x").font(semibold(ui.ctx(), 13.0)));
        });
        out.textures_delta.clear();
    }
}
