//! Design tokens and shared components of TernaryGrid: the colours, type and
//! spacing in [`style`] (also used to build egui's `Visuals`), and the
//! components in [`kit`] (buttons, tab strips, pills, search fields …) that
//! screens use instead of styling egui widgets one by one.

pub mod kit;
pub mod parts;
pub mod style;

pub use style::{Palette, pal, palette, soft};
