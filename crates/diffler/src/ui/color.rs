//! Golden-angle colours that stay readable on the theme's background.

use diffler_core::language;
use ratatui::style::Color;

use super::rgb_of;
use crate::theme::Theme;

fn hsl_to_rgb(hue: f32, saturation: f32, lightness: f32) -> (u8, u8, u8) {
    let c = (1.0 - (2.0 * lightness - 1.0).abs()) * saturation;
    let h = hue.rem_euclid(360.0) / 60.0;
    let x = c * (1.0 - (h.rem_euclid(2.0) - 1.0).abs());
    // `h` is `hue.rem_euclid(360.0) / 60.0`, always in 0.0..6.0
    #[allow(clippy::cast_sign_loss)]
    let sector = h as u32;
    let (r1, g1, b1) = match sector {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = lightness - c / 2.0;
    let channel = |v: f32| {
        // the clamp keeps the value in 0.0..=255.0 before the cast
        #[allow(clippy::cast_sign_loss)]
        let byte = ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8;
        byte
    };
    (channel(r1), channel(g1), channel(b1))
}

/// The golden angle, so each new hue lands far from every earlier one.
const HUE_STEP: f32 = 137.507_76;

pub(in crate::ui) fn golden_color(
    index: usize,
    offset: f32,
    saturation: f32,
    lightness: f32,
    bg: Color,
) -> Color {
    #[allow(clippy::cast_precision_loss)] // a hue only needs to look distinct, not be exact
    let hue = (index as f32 * HUE_STEP + offset).rem_euclid(360.0);
    let (r, g, b) = hsl_to_rgb(hue, saturation, lightness);
    let (r, g, b) = language::readable_on((r, g, b), rgb_of(bg));
    Color::Rgb(r, g, b)
}

/// A distinct, readable colour for the `slot`th thing a screen tells apart.
pub(in crate::ui) fn slot_color(theme: &Theme, slot: usize) -> Color {
    golden_color(slot, 25.0, 0.7, 0.55, theme.bg)
}
