//! Colours of the grid and views, derived from the current [`egui::Visuals`]
//! so light and dark themes both work.

use egui::{Color32, Visuals};

/// Linear blend of two colours in gamma space (`t` = 0 → `a`, 1 → `b`).
pub fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgba_premultiplied(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()), l(a.a(), b.a()))
}

/// Grid colours.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GridTheme {
    /// Cell background.
    pub bg: Color32,
    /// Alternate row background.
    pub stripe: Color32,
    /// Grid lines.
    pub line: Color32,
    /// The line under the header and over the grid.
    pub border: Color32,
    /// Header / row number background.
    pub header_bg: Color32,
    /// Header text.
    pub header_text: Color32,
    /// Header background of selected rows / columns.
    pub header_selected_bg: Color32,
    /// Row numbers.
    pub rownum_text: Color32,
    /// Normal text.
    pub text: Color32,
    /// Weak text (defaults, descriptions, binary values).
    pub weak: Color32,
    /// NULL (in italics).
    pub null: Color32,
    /// Selected cells overlay.
    pub selection: Color32,
    /// Active cell border.
    pub active_border: Color32,
    /// Modified cell background.
    pub modified: Color32,
    /// Inserted row background.
    pub inserted: Color32,
    /// Deleted row background.
    pub deleted: Color32,
    /// Foreign key links.
    pub link: Color32,
    /// Badge background.
    pub badge_bg: Color32,
    /// Badge text.
    pub badge_text: Color32,
    /// Error text.
    pub error: Color32,
    /// Warning / read-only accent.
    pub warn: Color32,
    /// `true` values.
    pub bool_true: Color32,
}

impl GridTheme {
    /// Theme for `visuals`: the design's colours (the palette of `ternarygrid_ui`, and the
    /// grid's own tints: stripes, lines, the selection, changed, new and deleted rows).
    pub fn from_visuals(v: &Visuals) -> Self {
        let dark = v.dark_mode;
        let p = ternarygrid_ui::style::palette(dark);
        let rgb = |d: u32, l: u32| {
            let c = if dark { d } else { l };
            Color32::from_rgb((c >> 16) as u8, (c >> 8) as u8, c as u8)
        };
        let bg = p.surface;
        let sel = p.sel;
        GridTheme {
            bg,
            stripe: p.stripe,
            line: p.line,
            border: p.border,
            header_bg: p.header,
            header_text: p.text,
            // Selected headers and the record view's field: the header tinted with the selection.
            header_selected_bg: mix(p.header, sel, 0.7),
            rownum_text: p.text_faint,
            text: p.text,
            weak: p.text_faint,
            null: p.text_faint,
            selection: sel,
            active_border: p.accent,
            modified: p.modified,
            inserted: rgb(0x1f3a2b, 0xe6f4ea),
            deleted: rgb(0x43282b, 0xfbe9e7),
            link: p.accent_text,
            badge_bg: mix(bg, p.text, 0.13),
            badge_text: mix(p.text, bg, 0.25),
            error: p.err,
            warn: p.warn,
            bool_true: p.ok,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mix_endpoints() {
        let a = Color32::from_rgb(0, 0, 0);
        let b = Color32::from_rgb(200, 100, 50);
        assert_eq!(mix(a, b, 0.0), a);
        assert_eq!(mix(a, b, 1.0), b);
        assert_eq!(mix(a, b, 0.5), Color32::from_rgb(100, 50, 25));
        let light = GridTheme::from_visuals(&Visuals::light());
        let dark = GridTheme::from_visuals(&Visuals::dark());
        assert_ne!(light.bg, dark.bg);
        assert_ne!(light.modified, light.bg);
    }
}
