//! Editor colors for light and dark mode.

use egui::{Color32, Visuals};

use crate::language::TokenStyle;

/// All colors used by the editor and [`crate::code_view`].
///
/// Use [`EditorTheme::light`] / [`EditorTheme::dark`] or
/// [`EditorTheme::from_visuals`] (follows the current egui theme and picks
/// up its background and selection colors).
#[derive(Clone, Debug, PartialEq)]
pub struct EditorTheme {
    /// Text area background.
    pub background: Color32,
    /// Plain text (and [`TokenStyle::Plain`]).
    pub text: Color32,
    /// Keywords.
    pub keyword: Color32,
    /// Unquoted identifiers.
    pub identifier: Color32,
    /// Quoted identifiers.
    pub quoted_identifier: Color32,
    /// String literals.
    pub string: Color32,
    /// Numbers.
    pub number: Color32,
    /// Comments.
    pub comment: Color32,
    /// Operators.
    pub operator: Color32,
    /// Punctuation.
    pub punctuation: Color32,
    /// Bind parameters.
    pub parameter: Color32,
    /// Variables.
    pub variable: Color32,
    /// Function names.
    pub function: Color32,
    /// Data types.
    pub data_type: Color32,
    /// Client side commands.
    pub client_command: Color32,
    /// Render comments in italics.
    pub italic_comments: bool,
    /// Gutter background.
    pub gutter_background: Color32,
    /// Line numbers.
    pub gutter_text: Color32,
    /// Line number of the cursor line.
    pub gutter_current_text: Color32,
    /// Thin separator between gutter and text.
    pub gutter_separator: Color32,
    /// Background of the cursor line.
    pub current_line: Color32,
    /// Background of a line with an error.
    pub error_line: Color32,
    /// Background of the statement range set by
    /// [`crate::EditorState::set_statement_highlight`].
    pub statement_background: Color32,
    /// Gutter bar next to the highlighted statement.
    pub statement_marker: Color32,
    /// Selection background.
    pub selection: Color32,
    /// Selection background while the editor does not have keyboard focus.
    pub selection_inactive: Color32,
    /// Text cursor.
    pub cursor: Color32,
    /// Error squiggles and markers.
    pub error: Color32,
    /// Warning squiggles and markers.
    pub warning: Color32,
    /// Info squiggles and markers.
    pub info: Color32,
    /// Spell-check dotted underline.
    pub spelling: Color32,
    /// Background of every find match.
    pub find_match: Color32,
    /// Other occurrences of the word under the cursor.
    pub occurrence: Color32,
    /// Background of the current find match.
    pub find_current: Color32,
    /// Background of matching brackets.
    pub bracket_match: Color32,
    /// Outline of matching brackets.
    pub bracket_outline: Color32,
    /// Hyperlink underline / text (command + hover).
    pub link: Color32,
    /// Underline of IME pre-edit (composition) text.
    pub ime_underline: Color32,
}

impl EditorTheme {
    /// Light palette.
    pub fn light() -> Self {
        Self {
            background: Color32::from_rgb(0xff, 0xff, 0xff),
            text: Color32::from_rgb(0x1c, 0x1c, 0x20),
            keyword: Color32::from_rgb(0x1f, 0x58, 0xc4),
            identifier: Color32::from_rgb(0x1c, 0x1c, 0x20),
            quoted_identifier: Color32::from_rgb(0x1c, 0x1c, 0x20),
            string: Color32::from_rgb(0x16, 0x79, 0x42),
            number: Color32::from_rgb(0x9c, 0x5c, 0x00),
            comment: Color32::from_rgb(0x80, 0x80, 0x8a),
            operator: Color32::from_rgb(0x1c, 0x1c, 0x20),
            punctuation: Color32::from_rgb(0x55, 0x55, 0x5f),
            parameter: Color32::from_rgb(0x76, 0x3f, 0xd0),
            variable: Color32::from_rgb(0x76, 0x3f, 0xd0),
            function: Color32::from_rgb(0x1f, 0x58, 0xc4),
            data_type: Color32::from_rgb(0x0d, 0x75, 0x88),
            client_command: Color32::from_rgb(0x9c, 0x5c, 0x00),
            italic_comments: true,
            gutter_background: Color32::from_rgb(0xff, 0xff, 0xff),
            gutter_text: Color32::from_rgb(0x80, 0x80, 0x8a),
            gutter_current_text: Color32::from_rgb(0x55, 0x55, 0x5f),
            gutter_separator: Color32::TRANSPARENT,
            current_line: Color32::from_rgb(0xf5, 0xf5, 0xf7),
            error_line: Color32::from_rgb(0xfc, 0xe7, 0xe6),
            statement_background: Color32::from_rgba_unmultiplied(0x0f, 0x7a, 0x68, 18),
            statement_marker: Color32::from_rgb(0x0f, 0x7a, 0x68),
            selection: Color32::from_rgb(0xd9, 0xee, 0xea),
            selection_inactive: Color32::from_rgb(0xe4, 0xe4, 0xe9),
            cursor: Color32::from_rgb(0x17, 0x17, 0x1a),
            error: Color32::from_rgb(0xbe, 0x27, 0x22),
            warning: Color32::from_rgb(0x8f, 0x53, 0x00),
            info: Color32::from_rgb(0x1f, 0x5f, 0xbf),
            spelling: Color32::from_rgb(0x1f, 0x5f, 0xbf),
            find_match: Color32::from_rgba_unmultiplied(0xf5, 0xa6, 0x23, 90),
            occurrence: Color32::from_rgba_unmultiplied(0x0f, 0x7a, 0x68, 36),
            find_current: Color32::from_rgba_unmultiplied(0xf5, 0x7c, 0x00, 170),
            bracket_match: Color32::from_rgba_unmultiplied(0x0f, 0x7a, 0x68, 40),
            bracket_outline: Color32::from_rgb(0x0f, 0x7a, 0x68),
            link: Color32::from_rgb(0x0b, 0x6a, 0x5a),
            ime_underline: Color32::from_rgb(0x17, 0x17, 0x1a),
        }
    }

    /// Dark palette.
    pub fn dark() -> Self {
        Self {
            background: Color32::from_rgb(0x15, 0x15, 0x17),
            text: Color32::from_rgb(0xdc, 0xde, 0xe4),
            keyword: Color32::from_rgb(0x7a, 0xa7, 0xff),
            identifier: Color32::from_rgb(0xdc, 0xde, 0xe4),
            quoted_identifier: Color32::from_rgb(0xdc, 0xde, 0xe4),
            string: Color32::from_rgb(0x7f, 0xd1, 0x9a),
            number: Color32::from_rgb(0xeb, 0xc0, 0x6a),
            comment: Color32::from_rgb(0x72, 0x73, 0x7c),
            operator: Color32::from_rgb(0xdc, 0xde, 0xe4),
            punctuation: Color32::from_rgb(0xa1, 0xa1, 0xaa),
            parameter: Color32::from_rgb(0xc4, 0x9b, 0xff),
            variable: Color32::from_rgb(0xc4, 0x9b, 0xff),
            function: Color32::from_rgb(0x7a, 0xa7, 0xff),
            data_type: Color32::from_rgb(0x62, 0xcc, 0xd8),
            client_command: Color32::from_rgb(0xeb, 0xc0, 0x6a),
            italic_comments: true,
            gutter_background: Color32::from_rgb(0x15, 0x15, 0x17),
            gutter_text: Color32::from_rgb(0x72, 0x73, 0x7c),
            gutter_current_text: Color32::from_rgb(0xa1, 0xa1, 0xaa),
            gutter_separator: Color32::TRANSPARENT,
            current_line: Color32::from_rgb(0x1d, 0x1d, 0x20),
            error_line: Color32::from_rgb(0x38, 0x1b, 0x1b),
            statement_background: Color32::from_rgba_unmultiplied(0x52, 0xd1, 0xba, 18),
            statement_marker: Color32::from_rgb(0x52, 0xd1, 0xba),
            selection: Color32::from_rgb(0x17, 0x33, 0x2e),
            selection_inactive: Color32::from_rgb(0x24, 0x24, 0x28),
            cursor: Color32::from_rgb(0xed, 0xed, 0xf0),
            error: Color32::from_rgb(0xf0, 0x65, 0x60),
            warning: Color32::from_rgb(0xf0, 0xad, 0x42),
            info: Color32::from_rgb(0x6a, 0xa5, 0xff),
            spelling: Color32::from_rgb(0x6a, 0xa5, 0xff),
            find_match: Color32::from_rgba_unmultiplied(0xf0, 0xad, 0x42, 70),
            occurrence: Color32::from_rgba_unmultiplied(0x52, 0xd1, 0xba, 40),
            find_current: Color32::from_rgba_unmultiplied(0xf0, 0x89, 0x4a, 150),
            bracket_match: Color32::from_rgba_unmultiplied(0x4d, 0xc4, 0x8c, 40),
            bracket_outline: Color32::from_rgb(0x4d, 0xc4, 0x8c),
            link: Color32::from_rgb(0x52, 0xd1, 0xba),
            ime_underline: Color32::from_rgb(0xed, 0xed, 0xf0),
        }
    }

    /// Palette matching `visuals` (dark or light), using its
    /// `extreme_bg_color` as background (not `text_edit_bg_color`, which apps
    /// may set to a distinct input-field color) and its selection color so
    /// the editor blends into the app.
    pub fn from_visuals(visuals: &Visuals) -> Self {
        let mut theme = if visuals.dark_mode {
            Self::dark()
        } else {
            Self::light()
        };
        theme.background = visuals.extreme_bg_color;
        theme.gutter_background = visuals.extreme_bg_color;
        theme.selection = visuals.selection.bg_fill;
        theme
    }

    /// Text color for a token style.
    pub fn token_color(&self, style: TokenStyle) -> Color32 {
        match style {
            TokenStyle::Keyword => self.keyword,
            TokenStyle::Identifier => self.identifier,
            TokenStyle::QuotedIdentifier => self.quoted_identifier,
            TokenStyle::String => self.string,
            TokenStyle::Number => self.number,
            TokenStyle::Comment => self.comment,
            TokenStyle::Operator => self.operator,
            TokenStyle::Punctuation => self.punctuation,
            TokenStyle::Parameter => self.parameter,
            TokenStyle::Variable => self.variable,
            TokenStyle::Function => self.function,
            TokenStyle::DataType => self.data_type,
            TokenStyle::ClientCommand => self.client_command,
            TokenStyle::Plain => self.text,
        }
    }

    /// Hash of everything that affects text layout (token colors and
    /// italics); used to invalidate cached line galleys.
    pub(crate) fn layout_key(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for style in TokenStyle::ALL {
            self.token_color(style).hash(&mut h);
        }
        self.italic_comments.hash(&mut h);
        h.finish()
    }
}

impl Default for EditorTheme {
    fn default() -> Self {
        Self::dark()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palettes_differ_and_follow_visuals() {
        let light = EditorTheme::light();
        let dark = EditorTheme::dark();
        assert_ne!(light.layout_key(), dark.layout_key());
        let mut v = Visuals::dark();
        // A distinct text-edit fill (as the app sets) must not leak into the editor.
        v.text_edit_bg_color = Some(Color32::from_rgb(1, 2, 3));
        let t = EditorTheme::from_visuals(&v);
        assert_eq!(t.background, v.extreme_bg_color);
        assert_eq!(t.gutter_background, v.extreme_bg_color);
        assert_eq!(t.selection, v.selection.bg_fill);
        assert_eq!(t.keyword, dark.keyword);
        let t = EditorTheme::from_visuals(&Visuals::light());
        assert_eq!(t.keyword, light.keyword);
        for style in TokenStyle::ALL {
            assert_ne!(light.token_color(style), Color32::TRANSPARENT);
        }
    }

    #[test]
    fn text_contrasts_with_background() {
        fn luminance(c: Color32) -> f32 {
            let f = |v: u8| {
                let v = v as f32 / 255.0;
                if v <= 0.03928 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * f(c.r()) + 0.7152 * f(c.g()) + 0.0722 * f(c.b())
        }
        fn contrast(a: Color32, b: Color32) -> f32 {
            let (la, lb) = (luminance(a), luminance(b));
            (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
        }
        for theme in [EditorTheme::light(), EditorTheme::dark()] {
            for style in TokenStyle::ALL {
                let c = contrast(theme.token_color(style), theme.background);
                assert!(c >= 3.0, "{style:?} contrast {c}");
            }
            assert!(contrast(theme.gutter_text, theme.gutter_background) >= 3.0);
        }
    }
}
