//! Icônes dessinées et boutons à icône. Les polices par défaut n'ont pas de
//! jeu d'icônes (et certains glyphes s'y affichent en emoji) : tout est
//! tracé à la main, au trait, pour une interface sobre.

use egui::{Align2, Color32, FontId, Painter, Rect, Response, Sense, Shape, Stroke, Ui, pos2, vec2};

use super::theme;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Icon {
    /// Activer / désactiver.
    Power,
    /// Enregistrer.
    Record,
    /// Tout cadrer.
    Fit,
    /// Réorganiser.
    Arrange,
    Undo,
    Redo,
    /// Détacher dans une fenêtre.
    Detach,
    /// Remettre sous les platines.
    Dock,
    Plus,
    /// Charger un morceau sur une platine.
    Load,
    Help,
    Check,
    Close,
    /// Position de départ : un trait, puis une flèche.
    Start,
    /// Suggestions : deux ondes qui s'emboîtent.
    Match,
}

/// Bouton à icône, sans fond au repos. `accent` : couleur de l'icône (et du
/// fond quand `on`).
pub fn icon_button(ui: &mut Ui, icon: Icon, accent: Color32, on: bool, enabled: bool, hint: &str) -> Response {
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, response) = ui.allocate_exact_size(vec2(26.0, 24.0), sense);
    let hovered = enabled && response.hovered();
    let painter = ui.painter();
    if on {
        painter.rect_filled(rect, 5.0, if hovered { accent.gamma_multiply(1.15) } else { accent });
    } else if hovered {
        painter.rect_filled(rect, 5.0, theme::PANEL_LIGHT);
    }
    let ink = if on {
        Color32::BLACK
    } else if !enabled {
        theme::TEXT_DIM.gamma_multiply(0.45)
    } else if hovered && accent == theme::TEXT_DIM {
        theme::TEXT
    } else {
        accent
    };
    paint(
        painter,
        Rect::from_center_size(rect.center(), vec2(12.0, 12.0)),
        icon,
        ink,
    );
    if hint.is_empty() {
        response
    } else {
        response.on_hover_text(hint)
    }
}

/// Dessine `icon` dans `rect` (environ 12 × 12).
pub fn paint(painter: &Painter, rect: Rect, icon: Icon, color: Color32) {
    let stroke = Stroke::new(1.6, color);
    let c = rect.center();
    let (l, r, t, b) = (rect.left(), rect.right(), rect.top(), rect.bottom());
    let arrow_head = |tip: egui::Pos2, dir: egui::Vec2| {
        let side = vec2(-dir.y, dir.x);
        Shape::convex_polygon(
            vec![tip, tip - dir * 4.5 + side * 3.5, tip - dir * 4.5 - side * 3.5],
            color,
            Stroke::NONE,
        )
    };
    match icon {
        Icon::Power => {
            let radius = 5.5;
            let points = (0..=24)
                .map(|i| {
                    let angle = (-60.0 + 300.0 * i as f32 / 24.0).to_radians();
                    c + vec2(angle.cos(), angle.sin()) * radius
                })
                .collect();
            painter.add(Shape::line(points, stroke));
            painter.vline(c.x, (c.y - radius - 1.0)..=(c.y - 1.0), stroke);
        }
        Icon::Record => {
            painter.circle_filled(c, 5.0, color);
        }
        Icon::Fit => {
            let k = 4.0;
            for (corner, dx, dy) in [
                (pos2(l, t), 1.0, 1.0),
                (pos2(r, t), -1.0, 1.0),
                (pos2(l, b), 1.0, -1.0),
                (pos2(r, b), -1.0, -1.0),
            ] {
                painter.line_segment([corner, corner + vec2(dx * k, 0.0)], stroke);
                painter.line_segment([corner, corner + vec2(0.0, dy * k)], stroke);
            }
            painter.rect_filled(Rect::from_center_size(c, vec2(3.0, 3.0)), 0.5, color);
        }
        Icon::Arrange => {
            for row in 0..3 {
                let y = t + 1.5 + row as f32 * 4.5;
                painter.rect_filled(Rect::from_min_size(pos2(l, y - 1.5), vec2(3.0, 3.0)), 0.5, color);
                painter.hline((l + 4.0)..=(r - 4.0), y, Stroke::new(1.2, color));
                painter.rect_filled(Rect::from_min_size(pos2(r - 3.0, y - 1.5), vec2(3.0, 3.0)), 0.5, color);
            }
        }
        Icon::Undo | Icon::Redo => {
            let flip = if icon == Icon::Undo { 1.0 } else { -1.0 };
            let center = pos2(c.x, c.y + 1.0);
            let radius = 4.5;
            // Arc par le haut, d'un côté à l'autre ; flèche à l'arrivée.
            let points: Vec<_> = (0..=16)
                .map(|i| {
                    let angle = (180.0 * i as f32 / 16.0).to_radians();
                    center + vec2(flip * angle.cos() * radius, -angle.sin() * radius)
                })
                .collect();
            let end = *points.last().expect("arc");
            painter.add(Shape::line(points, stroke));
            painter.line_segment(
                [center + vec2(flip * radius, 0.0), center + vec2(flip * radius, 4.0)],
                stroke,
            );
            painter.add(arrow_head(end + vec2(0.0, 4.0), vec2(0.0, 1.0)));
        }
        Icon::Detach | Icon::Dock => {
            let frame = Rect::from_min_max(pos2(l, t + 3.0), pos2(r - 3.0, b));
            painter.rect_stroke(frame, 1.5, Stroke::new(1.3, color), egui::StrokeKind::Inside);
            let (start, end) = if icon == Icon::Detach {
                (pos2(c.x - 1.0, c.y + 1.0), pos2(r + 0.5, t - 0.5))
            } else {
                (pos2(r + 0.5, t - 0.5), pos2(c.x - 1.0, c.y + 1.0))
            };
            let dir = (end - start).normalized();
            painter.line_segment([start, end - dir * 3.0], stroke);
            painter.add(arrow_head(end, dir));
        }
        Icon::Plus => {
            painter.hline(l..=r, c.y, Stroke::new(1.8, color));
            painter.vline(c.x, t..=b, Stroke::new(1.8, color));
        }
        Icon::Load => {
            painter.vline(c.x, t..=(b - 5.0), stroke);
            painter.add(arrow_head(pos2(c.x, b - 2.5), vec2(0.0, 1.0)));
            painter.hline(l..=r, b, stroke);
        }
        Icon::Match => {
            // Deux cercles qui se chevauchent : ce qui va ensemble.
            painter.circle_stroke(pos2(c.x - 2.5, c.y), 4.5, Stroke::new(1.5, color));
            painter.circle_stroke(pos2(c.x + 2.5, c.y), 4.5, Stroke::new(1.5, color));
        }
        Icon::Help => {
            painter.text(c, Align2::CENTER_CENTER, "?", FontId::proportional(14.0), color);
        }
        Icon::Check => {
            painter.add(Shape::line(
                vec![pos2(l + 1.0, c.y), pos2(c.x - 1.0, b - 2.0), pos2(r - 0.5, t + 2.0)],
                Stroke::new(1.8, color),
            ));
        }
        Icon::Start => {
            painter.vline(l + 1.0, (t + 1.0)..=(b - 1.0), Stroke::new(1.8, color));
            painter.line_segment([pos2(l + 4.0, c.y), pos2(r - 3.0, c.y)], stroke);
            painter.add(arrow_head(pos2(r, c.y), vec2(1.0, 0.0)));
        }
        Icon::Close => {
            let k = 4.5;
            painter.line_segment([c + vec2(-k, -k), c + vec2(k, k)], stroke);
            painter.line_segment([c + vec2(-k, k), c + vec2(k, -k)], stroke);
        }
    }
}

/// Sens d'une transition : lettre du départ → lettre de la destination,
/// chacune dans la couleur de sa platine. `strength` atténue (0..1).
pub fn paint_direction(painter: &Painter, rect: Rect, from: usize, size: f32, strength: f32) {
    let to = 1 - from;
    let font = FontId::monospace(size);
    let y = rect.center().y;
    let left = pos2(rect.left() + size * 0.35, y);
    let right = pos2(rect.right() - size * 0.35, y);
    painter.text(
        left,
        Align2::CENTER_CENTER,
        theme::DECK_NAMES[from],
        font.clone(),
        theme::DECK_COLORS[from].gamma_multiply(strength),
    );
    painter.text(
        right,
        Align2::CENTER_CENTER,
        theme::DECK_NAMES[to],
        font,
        theme::DECK_COLORS[to].gamma_multiply(strength),
    );
    let start = left.x + size * 0.55;
    let end = right.x - size * 0.55;
    let color = theme::TEXT_DIM.gamma_multiply(strength);
    let width = (size / 9.0).max(1.2);
    painter.hline(start..=(end - 3.0), y, Stroke::new(width, color));
    let head = size * 0.3;
    painter.add(Shape::convex_polygon(
        vec![
            pos2(end, y),
            pos2(end - head, y - head * 0.7),
            pos2(end - head, y + head * 0.7),
        ],
        color,
        Stroke::NONE,
    ));
}
