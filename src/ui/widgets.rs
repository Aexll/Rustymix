//! Contrôles dessinés à la main : potentiomètre rotatif, fader, vu-mètre,
//! boutons de transport. Chaque contrôle est placé dans un rectangle explicite.

use egui::{Align2, Color32, FontId, Id, Pos2, Rect, Response, Sense, Shape, Stroke, StrokeKind, Ui, pos2, vec2};

use super::theme;

// ---------------------------------------------------------------------------
// Découpage de rectangles
// ---------------------------------------------------------------------------

/// Retire une bande de hauteur `height` en haut de `rect` et la retourne.
pub fn take_top(rect: &mut Rect, height: f32) -> Rect {
    let (top, rest) = rect.split_top_bottom_at_y((rect.top() + height).min(rect.bottom()));
    *rect = rest;
    top
}

/// Retire une bande de hauteur `height` en bas de `rect` et la retourne.
pub fn take_bottom(rect: &mut Rect, height: f32) -> Rect {
    let (rest, bottom) = rect.split_top_bottom_at_y((rect.bottom() - height).max(rect.top()));
    *rect = rest;
    bottom
}

/// Retire une bande de largeur `width` à gauche de `rect` et la retourne.
pub fn take_left(rect: &mut Rect, width: f32) -> Rect {
    let (left, rest) = rect.split_left_right_at_x((rect.left() + width).min(rect.right()));
    *rect = rest;
    left
}

/// Retire une bande de largeur `width` à droite de `rect` et la retourne.
pub fn take_right(rect: &mut Rect, width: f32) -> Rect {
    let (rest, right) = rect.split_left_right_at_x((rect.right() - width).max(rect.left()));
    *rect = rest;
    right
}

// ---------------------------------------------------------------------------
// Potentiomètre rotatif
// ---------------------------------------------------------------------------

/// Angle (en radians, sens horaire depuis le haut) pour une valeur 0..1 :
/// course de 270° centrée en haut.
fn knob_angle(value: f32) -> f32 {
    (-135.0 + 270.0 * value.clamp(0.0, 1.0)).to_radians()
}

fn on_circle(center: Pos2, radius: f32, angle: f32) -> Pos2 {
    center + radius * vec2(angle.sin(), -angle.cos())
}

fn arc(center: Pos2, radius: f32, from: f32, to: f32) -> Vec<Pos2> {
    let steps = ((to - from).abs() / 0.12).ceil().max(1.0) as usize;
    (0..=steps)
        .map(|i| on_circle(center, radius, from + (to - from) * i as f32 / steps as f32))
        .collect()
}

/// Potentiomètre rotatif 0..1, zéro au centre.
///
/// Glisser verticalement pour tourner (Maj = réglage fin), molette au survol,
/// double-clic pour revenir à `default`.
pub fn knob(ui: &mut Ui, rect: Rect, id: Id, value: &mut f32, default: f32, color: Color32) -> Response {
    let mut response = ui.interact(rect, id, Sense::click_and_drag());
    let before = *value;

    if response.dragged() {
        let speed = if ui.input(|i| i.modifiers.shift) { 0.0015 } else { 0.006 };
        *value -= response.drag_delta().y * speed;
    }
    if response.hovered() {
        *value += ui.input(|i| i.smooth_scroll_delta.y) * 0.0015;
    }
    if response.double_clicked() {
        *value = default;
    }
    *value = value.clamp(0.0, 1.0);
    if *value != before {
        response.mark_changed();
    }

    let painter = ui.painter();
    let center = rect.center();
    let radius = rect.width().min(rect.height()) * 0.5 - 2.0;
    let lit = response.hovered() || response.dragged();

    // Piste et arc de valeur (depuis la position centrale).
    painter.add(Shape::line(
        arc(center, radius, knob_angle(0.0), knob_angle(1.0)),
        Stroke::new(3.0, theme::OUTLINE),
    ));
    if (*value - 0.5).abs() > 0.005 {
        painter.add(Shape::line(
            arc(center, radius, knob_angle(0.5), knob_angle(*value)),
            Stroke::new(3.0, color),
        ));
    }
    painter.line_segment(
        [
            on_circle(center, radius - 2.5, 0.0),
            on_circle(center, radius + 2.5, 0.0),
        ],
        Stroke::new(1.5, theme::TEXT_DIM),
    );

    // Corps et repère.
    let body = radius - 5.0;
    painter.circle_filled(
        center,
        body,
        if lit {
            theme::PANEL_LIGHT
        } else {
            Color32::from_rgb(0x1f, 0x23, 0x2b)
        },
    );
    painter.circle_stroke(center, body, Stroke::new(1.0, theme::OUTLINE));
    let angle = knob_angle(*value);
    painter.line_segment(
        [
            on_circle(center, body * 0.3, angle),
            on_circle(center, body - 2.0, angle),
        ],
        Stroke::new(2.5, if *value <= 0.0 { theme::RED } else { theme::TEXT }),
    );

    response
}

/// Repère d'une seconde valeur sur un potentiomètre (dessiné par-dessus) :
/// un point sur l'anneau et un trait vers le centre.
pub fn knob_ghost(painter: &egui::Painter, rect: Rect, value: f32, color: Color32) {
    let center = rect.center();
    let radius = rect.width().min(rect.height()) * 0.5 - 2.0;
    let angle = knob_angle(value);
    painter.line_segment(
        [
            on_circle(center, radius - 6.0, angle),
            on_circle(center, radius + 1.0, angle),
        ],
        Stroke::new(1.5, color),
    );
    painter.circle_filled(on_circle(center, radius + 1.0, angle), 3.0, color);
}

// ---------------------------------------------------------------------------
// Fader
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Orientation {
    /// Valeur 1 en haut.
    Vertical,
    /// Valeur 1 à droite.
    Horizontal,
}

/// Repère d'une seconde valeur sur un fader (dessiné par-dessus) : un trait
/// en travers du rail et un petit triangle sur le côté.
pub fn fader_ghost(painter: &egui::Painter, rect: Rect, value: f32, orientation: Orientation, color: Color32) {
    const HANDLE: f32 = 16.0;
    let value = value.clamp(0.0, 1.0);
    let stroke = Stroke::new(2.0, color);
    if orientation == Orientation::Vertical {
        let travel = rect.shrink2(vec2(0.0, HANDLE / 2.0));
        let y = travel.bottom() - value * travel.height();
        let x = rect.center().x;
        painter.line_segment([pos2(x - 9.0, y), pos2(x + 9.0, y)], stroke);
        painter.add(Shape::convex_polygon(
            vec![pos2(x - 9.0, y), pos2(x - 15.0, y - 4.0), pos2(x - 15.0, y + 4.0)],
            color,
            Stroke::NONE,
        ));
    } else {
        let travel = rect.shrink2(vec2(HANDLE / 2.0, 0.0));
        let x = travel.left() + value * travel.width();
        let y = rect.center().y;
        painter.line_segment([pos2(x, y - 9.0), pos2(x, y + 9.0)], stroke);
        painter.add(Shape::convex_polygon(
            vec![pos2(x, y - 9.0), pos2(x - 4.0, y - 15.0), pos2(x + 4.0, y - 15.0)],
            color,
            Stroke::NONE,
        ));
    }
}

/// Fader linéaire 0..1. Clic ou glisser pour régler, molette au survol,
/// double-clic pour revenir à `default`.
pub fn fader(
    ui: &mut Ui,
    rect: Rect,
    id: Id,
    value: &mut f32,
    default: f32,
    orientation: Orientation,
    color: Color32,
) -> Response {
    const HANDLE: f32 = 16.0;
    let vertical = orientation == Orientation::Vertical;
    let mut response = ui.interact(rect, id, Sense::click_and_drag());
    let before = *value;

    // Zone parcourue par le centre de la poignée.
    let travel = if vertical {
        rect.shrink2(vec2(0.0, HANDLE / 2.0))
    } else {
        rect.shrink2(vec2(HANDLE / 2.0, 0.0))
    };

    if response.is_pointer_button_down_on()
        && let Some(pointer) = response.interact_pointer_pos()
    {
        *value = if vertical {
            1.0 - (pointer.y - travel.top()) / travel.height()
        } else {
            (pointer.x - travel.left()) / travel.width()
        };
    }
    if response.hovered() {
        *value += ui.input(|i| i.smooth_scroll_delta.y) * 0.0015;
    }
    if response.double_clicked() {
        *value = default;
    }
    *value = value.clamp(0.0, 1.0);
    if *value != before {
        response.mark_changed();
    }

    let painter = ui.painter();
    let lit = response.hovered() || response.is_pointer_button_down_on();
    let center = rect.center();

    // Rail, graduations et repère de la valeur par défaut.
    let (rail, handle) = if vertical {
        let y = travel.bottom() - *value * travel.height();
        (
            Rect::from_center_size(center, vec2(4.0, travel.height())),
            Rect::from_center_size(pos2(center.x, y), vec2(rect.width().min(30.0), HANDLE)),
        )
    } else {
        let x = travel.left() + *value * travel.width();
        (
            Rect::from_center_size(center, vec2(travel.width(), 4.0)),
            Rect::from_center_size(pos2(x, center.y), vec2(HANDLE, rect.height().min(30.0))),
        )
    };
    painter.rect_filled(rail, 2.0, theme::WAVE_BACKGROUND);
    painter.rect_stroke(rail, 2.0, Stroke::new(1.0, theme::OUTLINE), StrokeKind::Outside);
    for i in 0..=10 {
        let t = i as f32 / 10.0;
        let len = if i % 5 == 0 { 6.0 } else { 3.0 };
        let stroke = Stroke::new(1.0, theme::OUTLINE);
        if vertical {
            let y = travel.bottom() - t * travel.height();
            painter.line_segment([pos2(rail.left() - 5.0 - len, y), pos2(rail.left() - 5.0, y)], stroke);
            painter.line_segment([pos2(rail.right() + 5.0, y), pos2(rail.right() + 5.0 + len, y)], stroke);
        } else {
            let x = travel.left() + t * travel.width();
            painter.line_segment([pos2(x, rail.top() - 5.0 - len), pos2(x, rail.top() - 5.0)], stroke);
            painter.line_segment(
                [pos2(x, rail.bottom() + 5.0), pos2(x, rail.bottom() + 5.0 + len)],
                stroke,
            );
        }
    }

    // Poignée.
    painter.rect_filled(
        handle,
        3.0,
        if lit {
            Color32::from_rgb(0x4a, 0x50, 0x5c)
        } else {
            Color32::from_rgb(0x3a, 0x3f, 0x4a)
        },
    );
    painter.rect_stroke(
        handle,
        3.0,
        Stroke::new(1.0, Color32::from_rgb(0x60, 0x67, 0x75)),
        StrokeKind::Inside,
    );
    let mark = Stroke::new(2.0, color);
    if vertical {
        painter.line_segment(
            [
                handle.left_center() + vec2(3.0, 0.0),
                handle.right_center() - vec2(3.0, 0.0),
            ],
            mark,
        );
    } else {
        painter.line_segment(
            [
                handle.center_top() + vec2(0.0, 3.0),
                handle.center_bottom() - vec2(0.0, 3.0),
            ],
            mark,
        );
    }

    response
}

// ---------------------------------------------------------------------------
// Vu-mètre
// ---------------------------------------------------------------------------

/// Bas de l'échelle du vu-mètre, en dB.
const METER_MIN_DB: f32 = -42.0;
/// Haut de l'échelle du vu-mètre, en dB.
const METER_MAX_DB: f32 = 6.0;
/// Vitesse de retombée, en dB par seconde.
const METER_RELEASE_DB_PER_S: f32 = 36.0;
/// Durée de maintien du pic, en secondes.
const METER_HOLD_S: f32 = 1.0;

/// Balistique d'un vu-mètre stéréo : montée instantanée, retombée progressive
/// et maintien du pic.
#[derive(Clone, Copy)]
pub struct Meter {
    level_db: [f32; 2],
    hold_db: [f32; 2],
    hold_age: [f32; 2],
}

impl Default for Meter {
    fn default() -> Self {
        Self {
            level_db: [METER_MIN_DB; 2],
            hold_db: [METER_MIN_DB; 2],
            hold_age: [0.0; 2],
        }
    }
}

impl Meter {
    /// `peaks` : pics linéaires gauche/droite mesurés depuis la dernière image.
    pub fn update(&mut self, peaks: [f32; 2], dt: f32) {
        for (ch, peak) in peaks.into_iter().enumerate() {
            let db = if peak > 1e-5 { 20.0 * peak.log10() } else { METER_MIN_DB };
            let db = db.clamp(METER_MIN_DB, METER_MAX_DB);
            self.level_db[ch] = db
                .max(self.level_db[ch] - METER_RELEASE_DB_PER_S * dt)
                .max(METER_MIN_DB);

            self.hold_age[ch] += dt;
            if db >= self.hold_db[ch] {
                self.hold_db[ch] = db;
                self.hold_age[ch] = 0.0;
            } else if self.hold_age[ch] > METER_HOLD_S {
                self.hold_db[ch] = (self.hold_db[ch] - METER_RELEASE_DB_PER_S * dt).max(METER_MIN_DB);
            }
        }
    }

    /// Vrai tant que le vu-mètre n'est pas complètement retombé.
    pub fn is_active(&self) -> bool {
        self.level_db.iter().chain(&self.hold_db).any(|&db| db > METER_MIN_DB)
    }
}

fn meter_color(db: f32) -> Color32 {
    if db >= 0.0 {
        theme::RED
    } else if db >= -9.0 {
        theme::YELLOW
    } else {
        theme::GREEN
    }
}

/// Vu-mètre stéréo vertical à segments (vert / jaune à −9 dB / rouge à 0 dB).
pub fn vu_meter(ui: &Ui, rect: Rect, meter: &Meter) {
    const SEGMENT: f32 = 5.0;
    let painter = ui.painter();
    painter.rect_filled(rect, 2.0, theme::WAVE_BACKGROUND);

    let inner = rect.shrink(2.0);
    let segments = (inner.height() / SEGMENT).floor().max(1.0) as usize;
    let bar_width = (inner.width() - 2.0) / 2.0;
    let step_db = (METER_MAX_DB - METER_MIN_DB) / segments as f32;

    for ch in 0..2 {
        let left = inner.left() + ch as f32 * (bar_width + 2.0);
        let hold_segment = ((meter.hold_db[ch] - METER_MIN_DB) / step_db).ceil() as usize;
        for i in 0..segments {
            // Seuil bas du segment i (0 = en bas).
            let threshold = METER_MIN_DB + i as f32 * step_db;
            let color = meter_color(threshold + step_db);
            let lit = meter.level_db[ch] > threshold || (hold_segment == i + 1 && meter.hold_db[ch] > METER_MIN_DB);
            let bottom = inner.bottom() - i as f32 * SEGMENT;
            let segment = Rect::from_min_max(pos2(left, bottom - SEGMENT + 1.0), pos2(left + bar_width, bottom));
            painter.rect_filled(segment, 0.0, if lit { color } else { color.gamma_multiply(0.13) });
        }
    }
}

// ---------------------------------------------------------------------------
// Boutons
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TransportIcon {
    Play,
    Pause,
    Cue,
}

/// Gros bouton de transport, allumé en `color` quand `active`.
pub fn transport_button(
    ui: &mut Ui,
    rect: Rect,
    id: Id,
    icon: TransportIcon,
    active: bool,
    color: Color32,
) -> Response {
    let response = ui.interact(rect, id, Sense::click());
    let painter = ui.painter();
    let pressed = response.is_pointer_button_down_on();

    let fill = match (active, pressed, response.hovered()) {
        (true, _, _) => color.gamma_multiply(0.28),
        (false, true, _) => theme::PANEL_LIGHT,
        (false, false, true) => Color32::from_rgb(0x20, 0x24, 0x2c),
        _ => Color32::from_rgb(0x1b, 0x1e, 0x25),
    };
    let foreground = if active { color } else { theme::TEXT_DIM };
    painter.rect_filled(rect, 8.0, fill);
    painter.rect_stroke(
        rect,
        8.0,
        Stroke::new(
            if active { 2.0 } else { 1.0 },
            if active { color } else { theme::OUTLINE },
        ),
        StrokeKind::Inside,
    );

    let c = rect.center();
    let s = rect.width().min(rect.height()) * 0.2;
    match icon {
        TransportIcon::Play => {
            let points = vec![c + vec2(-s * 0.8, -s), c + vec2(s, 0.0), c + vec2(-s * 0.8, s)];
            painter.add(Shape::convex_polygon(points, foreground, Stroke::NONE));
        }
        TransportIcon::Pause => {
            for dx in [-s * 0.55, s * 0.55] {
                painter.rect_filled(
                    Rect::from_center_size(c + vec2(dx, 0.0), vec2(s * 0.55, s * 2.0)),
                    1.0,
                    foreground,
                );
            }
        }
        TransportIcon::Cue => {
            painter.text(
                c,
                Align2::CENTER_CENTER,
                "CUE",
                FontId::proportional(s * 1.5),
                foreground,
            );
        }
    }
    response
}

/// Petit bouton à bascule avec libellé (boutons « kill »).
pub fn toggle_button(ui: &mut Ui, rect: Rect, id: Id, label: &str, active: bool, color: Color32) -> Response {
    let response = ui.interact(rect, id, Sense::click());
    let painter = ui.painter();
    let fill = if active {
        color
    } else if response.hovered() {
        theme::PANEL_LIGHT
    } else {
        Color32::from_rgb(0x1b, 0x1e, 0x25)
    };
    painter.rect_filled(rect, 4.0, fill);
    painter.rect_stroke(
        rect,
        4.0,
        Stroke::new(1.0, if active { color } else { theme::OUTLINE }),
        StrokeKind::Inside,
    );
    let text = if active { Color32::BLACK } else { theme::TEXT_DIM };
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        FontId::proportional(10.0),
        text,
    );
    response
}

/// Texte simple, coupé net aux limites de `rect`.
pub fn clipped_text(ui: &Ui, rect: Rect, anchor: Align2, text: &str, font: FontId, color: Color32) {
    let pos = anchor.pos_in_rect(&rect);
    ui.painter()
        .with_clip_rect(rect.intersect(ui.clip_rect()))
        .text(pos, anchor, text, font, color);
}
