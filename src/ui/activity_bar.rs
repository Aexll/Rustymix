//! Barre d'activité verticale, comme celle de VS Code : un bouton carré à
//! icône par panneau (bibliothèque, transitions en nœuds, installation). Le
//! panneau choisi s'affiche sous les platines ; les autres peuvent être
//! détachés dans des fenêtres flottantes.

use egui::{Align2, Color32, FontId, Id, Painter, Pos2, Rect, Sense, Shape, Stroke, Ui, pos2, vec2};

use super::theme;

/// Largeur de la barre.
pub const BAR_WIDTH: f32 = 48.0;
const BUTTON: f32 = 44.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tab {
    Library,
    Nodes,
    Install,
}

impl Tab {
    pub const ALL: [Tab; 3] = [Tab::Library, Tab::Nodes, Tab::Install];

    pub fn index(self) -> usize {
        match self {
            Tab::Library => 0,
            Tab::Nodes => 1,
            Tab::Install => 2,
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Tab::Library => "Bibliothèque",
            Tab::Nodes => "Transitions",
            Tab::Install => "Installer un son",
        }
    }

    /// La bibliothèque reste toujours sous les platines.
    pub fn detachable(self) -> bool {
        self != Tab::Library
    }
}

/// État d'un bouton de la barre.
pub struct BarItem {
    pub tab: Tab,
    /// Panneau affiché sous les platines.
    pub active: bool,
    /// Panneau détaché dans une fenêtre.
    pub floating: bool,
    /// Pastille (activité en cours) et son nombre éventuel.
    pub badge: Option<(Color32, Option<usize>)>,
}

/// Dessine la barre ; retourne le panneau cliqué.
pub fn activity_bar(ui: &mut Ui, items: &[BarItem]) -> Option<Tab> {
    let mut clicked = None;
    let top = ui.max_rect().top() + 6.0;
    let x = ui.max_rect().left();
    for (index, item) in items.iter().enumerate() {
        let rect = Rect::from_min_size(
            pos2(x + (BAR_WIDTH - BUTTON) / 2.0, top + index as f32 * (BUTTON + 4.0)),
            vec2(BUTTON, BUTTON),
        );
        let response = ui.interact(rect, Id::unique(("activity_bar", index)), Sense::click());
        let painter = ui.painter();
        let hovered = response.hovered();
        if item.active || hovered {
            painter.rect_filled(
                rect,
                6.0,
                if item.active {
                    theme::PANEL
                } else {
                    theme::PANEL_LIGHT.gamma_multiply(0.6)
                },
            );
        }
        // Repère d'activité à gauche, comme VS Code.
        if item.active {
            painter.rect_filled(
                Rect::from_min_size(pos2(x, rect.top() + 6.0), vec2(3.0, BUTTON - 12.0)),
                1.5,
                theme::DECK_COLORS[0],
            );
        }
        let color = if item.active || hovered {
            theme::TEXT
        } else {
            theme::TEXT_DIM
        };
        paint_icon(painter, rect.shrink(11.0), item.tab, color);
        // Panneau détaché : petite fenêtre dans le coin.
        if item.floating {
            let corner = Rect::from_min_size(rect.left_bottom() + vec2(5.0, -13.0), vec2(9.0, 8.0));
            painter.rect_stroke(corner, 1.0, Stroke::new(1.2, theme::YELLOW), egui::StrokeKind::Inside);
            painter.hline(corner.x_range(), corner.top() + 2.0, Stroke::new(1.2, theme::YELLOW));
        }
        if let Some((badge, count)) = item.badge {
            let center = rect.right_top() + vec2(-9.0, 9.0);
            match count {
                Some(count) if count > 0 => {
                    painter.circle_filled(center, 7.0, badge);
                    painter.text(
                        center,
                        Align2::CENTER_CENTER,
                        count.min(9).to_string(),
                        FontId::proportional(10.0),
                        Color32::BLACK,
                    );
                }
                _ => {
                    painter.circle_filled(center, 4.5, badge);
                }
            }
        }
        let hint = match (item.tab.detachable(), item.floating) {
            (_, true) => format!(
                "{} (Ctrl + {}) — détaché ; clic : le remettre ici",
                item.tab.title(),
                index + 1
            ),
            _ => format!("{} (Ctrl + {})", item.tab.title(), index + 1),
        };
        if response.on_hover_text(hint).clicked() {
            clicked = Some(item.tab);
        }
    }
    clicked
}

/// Icônes dessinées (les polices par défaut n'ont pas de jeu d'icônes).
fn paint_icon(painter: &Painter, rect: Rect, tab: Tab, color: Color32) {
    let stroke = Stroke::new(1.8, color);
    let (l, r, t, b) = (rect.left(), rect.right(), rect.top(), rect.bottom());
    match tab {
        Tab::Library => {
            // Liste de morceaux : une note et trois lignes.
            for i in 0..3 {
                let y = t + 3.0 + i as f32 * 8.0;
                // Les lignes du bas laissent la place à la note.
                let end = if i == 0 { r } else { r - 10.0 };
                painter.circle_filled(pos2(l + 2.5, y), 2.0, color);
                painter.hline((l + 8.0)..=end, y, stroke);
            }
            let note = pos2(r - 6.0, b - 2.5);
            painter.circle_filled(note, 3.0, color);
            painter.vline(note.x + 2.5, (b - 13.0)..=(b - 2.5), stroke);
            painter.line_segment([pos2(note.x + 2.5, b - 13.0), pos2(note.x + 6.5, b - 10.0)], stroke);
        }
        Tab::Nodes => {
            // Graphe : trois nœuds reliés par des fils.
            let node = |min: Pos2| Rect::from_min_size(min, vec2(8.0, 6.0));
            let a = node(pos2(l, t + 2.0));
            let c = node(pos2(l, b - 8.0));
            let d = node(pos2(r - 8.0, rect.center().y - 3.0));
            for n in [a, c, d] {
                painter.rect_stroke(n, 1.5, stroke, egui::StrokeKind::Inside);
            }
            for from in [a, c] {
                let start = from.right_center();
                let end = d.left_center();
                let bend = (end.x - start.x) * 0.5;
                painter.add(egui::epaint::CubicBezierShape::from_points_stroke(
                    [start, start + vec2(bend, 0.0), end - vec2(bend, 0.0), end],
                    false,
                    Color32::TRANSPARENT,
                    Stroke::new(1.4, color),
                ));
            }
        }
        Tab::Install => {
            // Téléchargement : flèche vers un bac.
            let center = rect.center().x;
            painter.vline(center, t..=(b - 7.0), stroke);
            painter.add(Shape::convex_polygon(
                vec![
                    pos2(center - 6.0, b - 10.0),
                    pos2(center + 6.0, b - 10.0),
                    pos2(center, b - 3.5),
                ],
                color,
                Stroke::NONE,
            ));
            painter.line_segment([pos2(l, b - 6.0), pos2(l, b)], stroke);
            painter.hline(l..=r, b, stroke);
            painter.line_segment([pos2(r, b - 6.0), pos2(r, b)], stroke);
        }
    }
}
