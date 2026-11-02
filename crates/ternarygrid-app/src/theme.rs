//! Fonts, light/dark visuals, accessibility tweaks and connection-type colours.

use egui::{Color32, Context, FontData, FontDefinitions, FontFamily, Stroke, Theme, ThemePreference, Visuals};
use std::sync::{Arc, LazyLock};

/// User theme choice (stored in `settings.json`).
pub use ternarygrid_config::settings::Theme as ThemeChoice;

/// Inter 4.1 Regular and Medium (SIL Open Font License, `assets/fonts/Inter-LICENSE.txt`):
/// a typeface designed for screens, crisp at UI sizes; egui's default UI font
/// is a light weight that looks faint and blurry. Subset without the private
/// use area, where the Phosphor icons live (`pyftsubset --unicodes=
/// U+0000-DFFF,U+F900-FFFF,U+10000-10FFFF --layout-features='*'`), and
/// zlib-compressed (half the size in the binary; inflated once, in a few ms).
static INTER: LazyLock<Vec<u8>> = LazyLock::new(|| inflate(include_bytes!("../assets/fonts/Inter-Regular.ttf.zz")));
static INTER_MEDIUM: LazyLock<Vec<u8>> = LazyLock::new(|| inflate(include_bytes!("../assets/fonts/Inter-Medium.ttf.zz")));

fn inflate(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    std::io::Read::read_to_end(&mut flate2::read::ZlibDecoder::new(data), &mut out).expect("bundled font");
    out
}

/// Fonts: Inter for the UI, Hack for code, then egui's emoji fonts and the
/// Phosphor icon font as fallbacks, so icons can be mixed into any label.
/// Inter is the Medium weight in the light theme: dark text on a light
/// background reads thinner than light text on a dark one. (egui's other
/// bundled font, Ubuntu-Light, is not used and not linked.)
pub fn font_definitions(theme: Theme) -> FontDefinitions {
    use egui::FontTweak;
    let inter: &'static [u8] = if theme == Theme::Light { &INTER_MEDIUM } else { &INTER };
    let mut fonts = FontDefinitions::empty();
    let mut add = |name: &str, data: FontData| {
        fonts.font_data.insert(name.into(), Arc::new(data));
    };
    add("Inter", FontData::from_static(inter));
    // The medium weight for labels that need it (tabs, buttons, titles), in both themes.
    add("Inter-Medium", FontData::from_static(&INTER_MEDIUM));
    add("Hack", FontData::from_static(epaint_default_fonts::HACK_REGULAR));
    // egui's sizes for its emoji fonts.
    add("NotoEmoji-Regular", FontData::from_static(epaint_default_fonts::NOTO_EMOJI_REGULAR).tweak(FontTweak { scale: 0.81, ..Default::default() }));
    add("emoji-icon-font", FontData::from_static(epaint_default_fonts::EMOJI_ICON).tweak(FontTweak { scale: 0.90, ..Default::default() }));
    let emoji = ["NotoEmoji-Regular".to_string(), "emoji-icon-font".to_string()];
    fonts.families.insert(FontFamily::Proportional, ["Inter".to_string()].into_iter().chain(emoji.clone()).collect());
    // Labels in monospace still find letters and symbols Hack lacks.
    fonts.families.insert(FontFamily::Monospace, ["Hack".to_string(), "Inter".to_string()].into_iter().chain(emoji).collect());
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
    // The icons sit a little high next to Inter: level with the text.
    if let Some(phosphor) = fonts.font_data.get_mut("phosphor") {
        Arc::make_mut(phosphor).tweak.y_offset_factor = 0.08;
    }
    // Inter SemiBold is not bundled (binary size): titles use the medium weight.
    ternarygrid_ui::style::add_weight_families(&mut fonts, "Inter-Medium", "Inter-Medium");
    fonts
}

pub fn setup_fonts(ctx: &Context) {
    set_fonts(ctx, ctx.theme());
}

fn set_fonts(ctx: &Context, theme: Theme) {
    ctx.set_fonts(font_definitions(theme));
    ctx.data_mut(|d| d.insert_temp(egui::Id::new("ternarygrid-font-theme"), theme));
}

/// Every frame: the UI font follows the theme, also when it follows the system's.
pub fn sync_fonts(ctx: &Context) {
    let theme = ctx.theme();
    if ctx.data(|d| d.get_temp::<Theme>(egui::Id::new("ternarygrid-font-theme"))) != Some(theme) {
        set_fonts(ctx, theme);
    }
}

/// Apply the appearance preferences: theme, contrast, zoom and UI font size.
pub fn apply_settings(ctx: &Context, settings: &ternarygrid_config::AppSettings) {
    // Monochrome icons: colour only where it means something (ternarygrid_icons).
    ternarygrid_icons::set_colored(false);
    apply_theme(ctx, settings.general.theme, settings.accessibility.high_contrast);
    let scale = settings.general.ui_scale.clamp(0.5, 3.0);
    if (ctx.zoom_factor() - scale).abs() > f32::EPSILON {
        ctx.set_zoom_factor(scale);
    }
    let size = settings.general.ui_font_size.clamp(8.0, 32.0);
    for theme in [Theme::Light, Theme::Dark] {
        ctx.style_mut_of(theme, |style| {
            use egui::{FontFamily, FontId, TextStyle};
            style.text_styles.insert(TextStyle::Body, FontId::new(size, FontFamily::Proportional));
            style.text_styles.insert(TextStyle::Button, FontId::new(size, FontFamily::Proportional));
            style.text_styles.insert(TextStyle::Small, FontId::new((size - 3.0).max(7.0), FontFamily::Proportional));
            style.text_styles.insert(TextStyle::Heading, FontId::new(size + 5.0, FontFamily::Proportional));
            style.text_styles.insert(TextStyle::Monospace, FontId::new(size, FontFamily::Monospace));
            // Back to egui's default when switched off again.
            style.animation_time = if settings.accessibility.reduce_motion { 0.0 } else { egui::Style::default().animation_time };
        });
    }
}

/// Apply theme preference and TernaryGrid's style adjustments to both themes.
pub fn apply_theme(ctx: &Context, choice: ThemeChoice, high_contrast: bool) {
    ctx.set_theme(match choice {
        ThemeChoice::System => ThemePreference::System,
        ThemeChoice::Light => ThemePreference::Light,
        ThemeChoice::Dark => ThemePreference::Dark,
    });
    for theme in [Theme::Light, Theme::Dark] {
        ctx.style_mut_of(theme, |style| {
            style.spacing.item_spacing = egui::vec2(6.0, 4.0);
            style.spacing.button_padding = egui::vec2(6.0, 2.0);
            style.spacing.interact_size.y = 20.0;
            style.interaction.tooltip_delay = 0.4;
            // Scroll bars stay visible (egui shows them only on hover), wider when used.
            style.spacing.scroll = egui::style::ScrollStyle {
                floating_width: 6.0,
                bar_width: 10.0,
                dormant_handle_opacity: 0.55,
                active_handle_opacity: 0.8,
                interact_handle_opacity: 1.0,
                foreground_color: true,
                ..egui::style::ScrollStyle::floating()
            };
            style.visuals = visuals(theme);
            let v = &mut style.visuals;
            v.window_highlight_topmost = true;
            if high_contrast {
                let (fg, bg) = if v.dark_mode { (Color32::WHITE, Color32::BLACK) } else { (Color32::BLACK, Color32::WHITE) };
                v.override_text_color = Some(fg);
                v.panel_fill = bg;
                v.window_fill = bg;
                v.extreme_bg_color = bg;
                v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, fg);
                v.widgets.inactive.bg_stroke = Stroke::new(1.0, fg);
                v.selection.stroke = Stroke::new(2.0, fg);
            } else {
                v.override_text_color = None;
            }
        });
    }
}

fn hex(rgb: u32) -> Color32 {
    Color32::from_rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

/// TernaryGrid's visuals, built from the design tokens it shares with
/// TernaryWire and TernaryShell (`ternarygrid_ui::style`): neutral surfaces,
/// the teal accent of the app icon, soft selection tints instead of saturated
/// fills. The grid and the editor are drawn on `extreme_bg_color` (the
/// surface), their headers in `noninteractive.weak_bg_fill`.
pub fn visuals(theme: Theme) -> Visuals {
    use egui::{CornerRadius, Shadow};
    let dark = theme == Theme::Dark;
    let p = ternarygrid_ui::palette(dark);
    let soft = ternarygrid_ui::soft;
    let mut v = if dark { Visuals::dark() } else { Visuals::light() };
    // Sharper glyphs: no fractional glyph positions (they blur stems).
    v.text_options.subpixel_binning = false;
    v.override_text_color = None;
    v.weak_text_color = Some(p.text_muted);
    v.panel_fill = p.canvas;
    v.window_fill = p.surface;
    v.window_stroke = Stroke::new(1.0, p.border);
    v.extreme_bg_color = p.surface;
    v.text_edit_bg_color = Some(p.surface);
    v.faint_bg_color = if dark { hex(0x272930) } else { hex(0xf7f7f9) };
    v.code_bg_color = p.subtle;
    v.hyperlink_color = p.accent_text;
    v.warn_fg_color = p.warn;
    v.error_fg_color = p.err;
    // Opaque (the accent over the surface): the grid mixes it with its header.
    v.selection.bg_fill = over(soft(p.accent, if dark { 0.30 } else { 0.18 }), p.surface);
    v.selection.stroke = Stroke::new(1.0, p.accent_text);
    v.text_cursor.stroke = Stroke::new(1.5, p.accent);
    // A blinking caret would repaint twice a second (idle CPU stays at 0 %).
    v.text_cursor.blink = false;
    v.window_corner_radius = CornerRadius::same(ternarygrid_ui::style::RADIUS_LG);
    v.menu_corner_radius = CornerRadius::same(8);
    v.window_shadow = Shadow { offset: [0, 10], blur: 30, spread: 0, color: Color32::from_black_alpha(if dark { 120 } else { 38 }) };
    v.popup_shadow = Shadow { offset: [0, 4], blur: 14, spread: 0, color: Color32::from_black_alpha(if dark { 100 } else { 28 }) };
    v.collapsing_header_frame = false;
    v.indent_has_left_vline = true;
    v.striped = false;
    v.slider_trailing_fill = true;
    v.handle_shape = egui::style::HandleShape::Circle;
    v.disabled_alpha = 0.45;
    let radius = CornerRadius::same(ternarygrid_ui::style::RADIUS);
    let w = &mut v.widgets;
    // Labels, separators, frames; `weak_bg_fill` is the grid's header.
    w.noninteractive.bg_fill = p.surface;
    w.noninteractive.weak_bg_fill = p.header;
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
    w.noninteractive.corner_radius = radius;
    // Buttons, inputs, check boxes at rest: an outlined surface (text fields
    // must stand out in dialogs too).
    w.inactive.bg_fill = p.subtle;
    w.inactive.weak_bg_fill = p.surface;
    w.inactive.bg_stroke = Stroke::new(1.0, p.border_strong);
    w.inactive.fg_stroke = Stroke::new(1.0, p.text);
    w.inactive.corner_radius = radius;
    w.inactive.expansion = 0.0;
    w.hovered.bg_fill = p.subtle;
    w.hovered.weak_bg_fill = p.hover;
    w.hovered.bg_stroke = Stroke::new(1.0, if dark { p.border_strong } else { hex(0xbebec8) });
    w.hovered.fg_stroke = Stroke::new(1.0, p.text);
    w.hovered.corner_radius = radius;
    w.hovered.expansion = 0.0;
    w.active.bg_fill = p.subtle;
    w.active.weak_bg_fill = p.subtle;
    w.active.bg_stroke = Stroke::new(1.0, p.accent);
    w.active.fg_stroke = Stroke::new(1.0, p.text);
    w.active.corner_radius = radius;
    w.active.expansion = 0.0;
    w.open.bg_fill = p.subtle;
    w.open.weak_bg_fill = p.hover;
    w.open.bg_stroke = Stroke::new(1.0, p.border_strong);
    w.open.fg_stroke = Stroke::new(1.0, p.text);
    w.open.corner_radius = radius;
    v
}

/// Toolbars, the menu, the status bar and the tab bar.
pub fn chrome(v: &Visuals) -> Color32 {
    ternarygrid_ui::palette(v.dark_mode).chrome
}

/// Frame of the panels drawn in [`chrome`].
pub fn chrome_frame(style: &egui::Style) -> egui::Frame {
    egui::Frame::side_top_panel(style).fill(chrome(&style.visuals))
}

/// The tab area's style, as in TernaryWire and TernaryShell: the tab bar in
/// [`chrome`], the active tab in the content's colour (it opens onto it, with
/// rounded top corners), inactive tabs muted. The focused tab's accent line is
/// painted by the tab viewer (`tabs::DockViewer::on_tab_button`).
pub fn dock_style(style: &egui::Style) -> egui_dock::Style {
    use egui::CornerRadius;
    let mut s = egui_dock::Style::from_egui(style);
    let v = &style.visuals;
    let p = ternarygrid_ui::palette(v.dark_mode);
    let top = CornerRadius { nw: 6, ne: 6, sw: 0, se: 0 };
    s.tab_bar.bg_fill = p.chrome;
    s.tab_bar.hline_color = p.border;
    s.tab_bar.height = s.tab_bar.height.max(30.0);
    for t in [&mut s.tab.active, &mut s.tab.focused, &mut s.tab.active_with_kb_focus, &mut s.tab.focused_with_kb_focus] {
        t.bg_fill = p.canvas;
        t.outline_color = p.border;
        t.text_color = p.text;
        t.corner_radius = top;
    }
    for t in [&mut s.tab.inactive, &mut s.tab.inactive_with_kb_focus] {
        t.bg_fill = p.chrome;
        t.outline_color = p.chrome;
        t.text_color = p.text_muted;
        t.corner_radius = top;
    }
    s.tab.hovered.bg_fill = p.hover;
    s.tab.hovered.outline_color = p.hover;
    s.tab.hovered.text_color = p.text;
    s.tab.hovered.corner_radius = top;
    s.tab.tab_body.bg_fill = p.canvas;
    s.tab.tab_body.stroke = Stroke::new(1.0, p.border);
    s.separator.width = 1.0;
    s.separator.color_idle = p.border;
    s.separator.color_hovered = p.border_strong;
    s.separator.color_dragged = p.accent;
    s.buttons.close_tab_color = p.text_muted;
    s.buttons.close_tab_active_color = p.text;
    s.buttons.close_tab_bg_fill = p.hover;
    s.buttons.add_tab_color = p.text_muted;
    s.buttons.add_tab_active_color = p.text;
    s.buttons.add_tab_bg_fill = p.hover;
    s.overlay.selection_color = ternarygrid_ui::soft(p.accent, 0.25);
    s
}

/// Colour of a connection type marker.
pub fn rgb(c: [u8; 3]) -> Color32 {
    Color32::from_rgb(c[0], c[1], c[2])
}

/// Marker colour suitable for the current theme: white "no colour" markers
/// become transparent so development connections are not highlighted.
pub fn connection_marker(c: [u8; 3]) -> Option<Color32> {
    if c == [255, 255, 255] { None } else { Some(rgb(c)) }
}

/// Softened version of a marker colour for backgrounds (tab strips, status bar).
pub fn tint(c: Color32, dark_mode: bool) -> Color32 {
    let alpha = if dark_mode { 70 } else { 90 };
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), alpha)
}

/// Fill of primary buttons (white text on it): the teal of the app icon.
pub fn accent(dark_mode: bool) -> Color32 {
    ternarygrid_ui::palette(dark_mode).accent
}

/// A database type's brand colour (one-colour logos and icons).
pub fn brand_color(kind: ternarygrid_core::DbKind) -> Color32 {
    use ternarygrid_core::DbKind as K;
    match kind {
        K::Postgres => hex(0x336791),
        K::CockroachDb => hex(0x6933FF),
        K::MySql => hex(0x00758F),
        K::MariaDb => hex(0xC0765A),
        K::Sqlite => hex(0x0F80CC),
        K::SqlServer => hex(0xCC2927),
        K::ClickHouse => ternarygrid_icons::GOLD,
        K::Oracle => hex(0xF80000),
        K::Cassandra => hex(0x1287B1),
        K::Redis => hex(0xDC382D),
        K::MongoDb => hex(0x47A248),
        K::Elasticsearch => hex(0x005571),
        K::Neo4j => hex(0x4581C3),
        _ => ternarygrid_icons::SLATE,
    }
}

/// A one-colour database logo's colour: the brand colour, a little lighter
/// on dark backgrounds (coloured logos keep their own colours).
pub fn logo_color(kind: ternarygrid_core::DbKind, dark_mode: bool) -> Color32 {
    let c = brand_color(kind);
    if dark_mode { crate::theme::lighten(c, 0.2) } else { c }
}

/// `c` mixed with white by `t` (0 … 1).
pub fn lighten(c: Color32, t: f32) -> Color32 {
    let mix = |a: u8| (a as f32 + (255.0 - a as f32) * t).round() as u8;
    Color32::from_rgb(mix(c.r()), mix(c.g()), mix(c.b()))
}

/// `c` mixed with black by `t` (0 … 1).
pub fn darken(c: Color32, t: f32) -> Color32 {
    let mix = |a: u8| (a as f32 * (1.0 - t)).round() as u8;
    Color32::from_rgb(mix(c.r()), mix(c.g()), mix(c.b()))
}

/// `top` (premultiplied, e.g. from `gamma_multiply`) painted over the opaque `bottom`.
pub fn over(top: Color32, bottom: Color32) -> Color32 {
    let rest = 1.0 - top.a() as f32 / 255.0;
    let mix = |t: u8, b: u8| (t as f32 + b as f32 * rest).round().min(255.0) as u8;
    Color32::from_rgb(mix(top.r(), bottom.r()), mix(top.g(), bottom.g()), mix(top.b(), bottom.b()))
}

/// Relative luminance (WCAG 2).
fn luminance(c: Color32) -> f32 {
    let lin = |v: u8| {
        let v = v as f32 / 255.0;
        if v <= 0.040_45 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * lin(c.r()) + 0.7152 * lin(c.g()) + 0.0722 * lin(c.b())
}

/// Contrast ratio of two colours (1 … 21, WCAG 2).
pub fn contrast(a: Color32, b: Color32) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// `c` made lighter (on a darker `bg`) or darker until it has a contrast of
/// 4.5 against `bg`, so that a logo's thin lines stay legible on a tinted or
/// selected row; `c` itself when it has that already.
pub fn legible_on(c: Color32, bg: Color32) -> Color32 {
    let lighter = luminance(c) >= luminance(bg);
    (0..=10).map(|i| i as f32 / 10.0).map(|t| if lighter { lighten(c, t) } else { darken(c, t) }).find(|&x| contrast(x, bg) >= 4.5).unwrap_or(if lighter {
        Color32::WHITE
    } else {
        Color32::BLACK
    })
}

pub fn success_color(dark_mode: bool) -> Color32 {
    ternarygrid_ui::palette(dark_mode).ok
}

pub fn warning_color(dark_mode: bool) -> Color32 {
    ternarygrid_ui::palette(dark_mode).warn
}

pub fn error_color(dark_mode: bool) -> Color32 {
    ternarygrid_ui::palette(dark_mode).err
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inter(ctx: &Context) -> usize {
        ctx.data(|d| d.get_temp::<Theme>(egui::Id::new("ternarygrid-font-theme"))).map_or(0, |t| font_definitions(t).font_data["Inter"].font.len())
    }

    /// One-colour logos (MySQL, MariaDB, CockroachDB) on every row of the
    /// navigator: plain, tinted by a connection type, selected, hovered.
    #[test]
    fn database_logos_are_legible_on_every_row() {
        use ternarygrid_core::DbKind;
        for theme in [Theme::Light, Theme::Dark] {
            let v = visuals(theme);
            let tints =
                ternarygrid_core::config::ConnectionType::builtins().into_iter().filter_map(|t| connection_marker(t.color)).map(|t| over(t.gamma_multiply(0.22), v.panel_fill));
            let rows: Vec<Color32> = [v.panel_fill, over(v.selection.bg_fill, v.panel_fill), over(v.widgets.hovered.weak_bg_fill, v.panel_fill)].into_iter().chain(tints).collect();
            for kind in [DbKind::MySql, DbKind::MariaDb, DbKind::CockroachDb] {
                for &bg in &rows {
                    let c = legible_on(logo_color(kind, theme == Theme::Dark), bg);
                    assert!(contrast(c, bg) >= 4.5, "{theme:?} {kind:?} {c:?} on {bg:?}: {}", contrast(c, bg));
                }
            }
        }
        // The dark theme's MySQL logo on a test connection's green row was too faint.
        let v = visuals(Theme::Dark);
        let green = over(connection_marker(ternarygrid_core::config::ConnectionType::test().color).unwrap().gamma_multiply(0.22), v.panel_fill);
        assert!(contrast(logo_color(DbKind::MySql, true), green) < 4.5);
        assert_eq!(legible_on(Color32::WHITE, Color32::BLACK), Color32::WHITE);
    }

    /// The weights of the design kit are registered (with the icons as their
    /// fallback), and the icons sit level with the text.
    #[test]
    fn weight_families_and_icon_offset() {
        for theme in [Theme::Light, Theme::Dark] {
            let fonts = font_definitions(theme);
            for family in [ternarygrid_ui::style::MEDIUM, ternarygrid_ui::style::SEMIBOLD] {
                let keys = &fonts.families[&FontFamily::Name(family.into())];
                assert_eq!(keys.first().map(String::as_str), Some("Inter-Medium"), "{family}");
                assert!(keys.iter().any(|k| k == "phosphor"), "{family} falls back to the icons");
            }
            assert_eq!(fonts.font_data["phosphor"].tweak.y_offset_factor, 0.08);
        }
    }

    #[test]
    fn light_theme_uses_a_heavier_inter() {
        assert_eq!(font_definitions(Theme::Light).font_data["Inter"].font.len(), INTER_MEDIUM.len());
        assert_eq!(font_definitions(Theme::Dark).font_data["Inter"].font.len(), INTER.len());
        let ctx = Context::default();
        ctx.set_theme(Theme::Dark);
        sync_fonts(&ctx);
        assert_eq!(inter(&ctx), INTER.len());
        ctx.set_theme(Theme::Light);
        sync_fonts(&ctx);
        assert_eq!(inter(&ctx), INTER_MEDIUM.len());
    }
}
