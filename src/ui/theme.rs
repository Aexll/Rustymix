//! Couleurs et style de l'interface.

use egui::{Color32, Context, Visuals};

pub const BACKGROUND: Color32 = Color32::from_rgb(0x10, 0x12, 0x16);
pub const PANEL: Color32 = Color32::from_rgb(0x18, 0x1b, 0x21);
pub const PANEL_LIGHT: Color32 = Color32::from_rgb(0x26, 0x2a, 0x33);
pub const WAVE_BACKGROUND: Color32 = Color32::from_rgb(0x0a, 0x0b, 0x0e);
pub const OUTLINE: Color32 = Color32::from_rgb(0x34, 0x39, 0x44);

pub const TEXT: Color32 = Color32::from_rgb(0xe6, 0xe8, 0xec);
pub const TEXT_DIM: Color32 = Color32::from_rgb(0x8b, 0x93, 0xa1);
/// Texte des morceaux déjà joués dans la bibliothèque.
pub const PLAYED: Color32 = Color32::from_rgb(0x55, 0x5b, 0x66);

pub const GREEN: Color32 = Color32::from_rgb(0x3d, 0xdc, 0x84);
pub const YELLOW: Color32 = Color32::from_rgb(0xff, 0xd2, 0x3f);
pub const RED: Color32 = Color32::from_rgb(0xff, 0x4d, 0x4d);
pub const CUE: Color32 = Color32::from_rgb(0xff, 0xb0, 0x00);
/// Marqueurs spéciaux (début / fin de la musique).
pub const SPECIAL_MARKER: Color32 = Color32::from_rgb(0xc8, 0xcd, 0xd6);
/// Déclencheurs manuels des transitions.
pub const MANUAL_TRIGGER: Color32 = Color32::from_rgb(0xb3, 0x8c, 0xff);

/// Couleur d'accent de chaque platine (A, B).
pub const DECK_COLORS: [Color32; 2] = [Color32::from_rgb(0x38, 0xb6, 0xff), Color32::from_rgb(0xff, 0x9a, 0x3c)];
/// Couleur de chaque marqueur (1 à 8).
pub const MARKER_COLORS: [Color32; 8] = [
    Color32::from_rgb(0xff, 0x4d, 0x6d),
    Color32::from_rgb(0x3d, 0xdc, 0x84),
    Color32::from_rgb(0x4d, 0x9d, 0xff),
    Color32::from_rgb(0xff, 0xd2, 0x3f),
    Color32::from_rgb(0xc0, 0x6c, 0xff),
    Color32::from_rgb(0x2e, 0xe6, 0xd6),
    Color32::from_rgb(0xff, 0x8c, 0x42),
    Color32::from_rgb(0xff, 0x7e, 0xd1),
];
pub const DECK_NAMES: [&str; 2] = ["A", "B"];

pub fn apply(ctx: &Context) {
    let mut visuals = Visuals::dark();
    visuals.panel_fill = BACKGROUND;
    visuals.window_fill = PANEL;
    visuals.extreme_bg_color = WAVE_BACKGROUND;
    visuals.faint_bg_color = Color32::from_rgb(0x15, 0x18, 0x1d);
    visuals.override_text_color = Some(TEXT);
    visuals.selection.bg_fill = Color32::from_rgb(0x1f, 0x4e, 0x70);
    ctx.set_visuals(visuals);
}

/// Mélange linéaire de deux couleurs (`t` = 0 → `a`, 1 → `b`).
pub fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let lerp = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t.clamp(0.0, 1.0)).round() as u8;
    Color32::from_rgb(lerp(a.r(), b.r()), lerp(a.g(), b.g()), lerp(a.b(), b.b()))
}
