//! Raccourcis clavier, repris de la disposition par défaut de Mixxx.
//!
//! Les touches de lettres sont lues par leur **position physique** (comme les
//! fichiers de mapping localisés de Mixxx) : sur un clavier AZERTY, la touche
//! « A » de Mixxx est donc celle marquée « Q », et « ; » celle marquée « M ».

use egui::{Context, Event, Key};

/// Action ponctuelle déclenchée par un raccourci.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Action {
    TogglePlay(usize),
    /// Bouton Cue (retour au cue en lecture, pose du cue à l'arrêt).
    Cue(usize),
    CueSet(usize),
    CueGotoAndStop(usize),
    BassKill(usize),
    LoadSelected(usize),
    Eject(usize),
    SelectionUp,
    SelectionDown,
    FocusSearch,
    /// Change le tempo de la platine de la fraction donnée (0,01 = 1 %).
    TempoStep(usize, f32),
    ToggleSync(usize),
    /// Marqueur (platine, numéro 0..) : y aller, ou le poser s'il est vide.
    Marker(usize, usize),
    MarkerClear(usize, usize),
    /// Déclencheur manuel (0 à 3) de la transition planifiée.
    TransitionTrigger(usize),
}

/// Touches maintenues (actions continues).
#[derive(Clone, Copy, Debug, Default)]
pub struct Held {
    pub seek_back: [bool; 2],
    pub seek_forward: [bool; 2],
    pub crossfade_left: bool,
    pub crossfade_right: bool,
    pub shift: bool,
}

/// Tableau d'aide : (fonction, platine A, platine B). Touches QWERTY, avec
/// l'équivalent AZERTY entre parenthèses quand il diffère.
pub const HELP: &[(&str, &str, &str)] = &[
    ("Lecture / pause", "D", "L"),
    ("Cue", "F", "; (AZERTY : M)"),
    ("Définir le point de cue", "Maj + D", "Maj + L"),
    ("Retour au cue et arrêt", "Maj + F", "Maj + ; (AZERTY : Maj + M)"),
    ("Recherche arrière (maintenir)", "A (AZERTY : Q)", "J"),
    ("Recherche avant (maintenir)", "S", "K"),
    ("Kill des basses", "B", "N"),
    ("Tempo − / + (1 %)", "F1 / F2", "F5 / F6"),
    ("Tempo − / + (0,1 %)", "Maj + F1 / F2", "Maj + F5 / F6"),
    ("Sync du tempo sur l'autre platine", "1 (AZERTY : &)", "6 (AZERTY : -)"),
    ("Marqueurs 1 à 4 : poser / y aller", "2 3 4 5", "7 8 9 0"),
    ("Effacer les marqueurs 1 à 4", "Maj + 2 3 4 5", "Maj + 7 8 9 0"),
    ("Charger le morceau sélectionné", "Maj + ←", "Maj + →"),
    ("Décharger la platine", "Alt + Maj + ←", "Alt + Maj + →"),
    ("Crossfader vers la gauche / droite", "G", "H"),
    ("Crossfader, petits pas", "Maj + G", "Maj + H"),
    ("Transition : déclencheurs manuels 1 à 4", "F7 F8 F9 F10", ""),
    ("Sélection dans la bibliothèque", "↑ / ↓", ""),
    ("Champ de recherche", "Ctrl + F", "Échap pour en sortir"),
];

#[derive(Default)]
pub struct Shortcuts {
    held: Held,
}

impl Shortcuts {
    pub fn held(&self) -> Held {
        self.held
    }

    /// Lit les évènements clavier de l'image courante.
    /// `blocked` : une autre partie de l'interface (éditeur de nœuds) prend
    /// le clavier ; aucun raccourci des platines ne s'applique.
    pub fn update(&mut self, ctx: &Context, blocked: bool) -> Vec<Action> {
        // Pendant la saisie dans un champ de texte, les lettres sont du texte.
        let typing = ctx.text_edit_focused();
        let mut actions = Vec::new();
        if blocked {
            self.held = Held::default();
            return actions;
        }

        ctx.input(|input| {
            if typing || !input.focused {
                self.held = Held::default();
            }
            self.held.shift = input.modifiers.shift;

            for event in &input.events {
                let Event::Key {
                    key,
                    physical_key,
                    pressed,
                    repeat,
                    modifiers,
                } = event
                else {
                    continue;
                };
                let (pressed, repeat) = (*pressed, *repeat);

                // Navigation et chargement : touches logiques.
                if pressed {
                    match key {
                        Key::ArrowUp => actions.push(Action::SelectionUp),
                        Key::ArrowDown => actions.push(Action::SelectionDown),
                        Key::ArrowLeft | Key::ArrowRight if modifiers.shift && !typing && !repeat => {
                            let deck = usize::from(*key == Key::ArrowRight);
                            actions.push(if modifiers.alt {
                                Action::Eject(deck)
                            } else {
                                Action::LoadSelected(deck)
                            });
                        }
                        Key::F if modifiers.command && !repeat => actions.push(Action::FocusSearch),
                        _ => {}
                    }
                }

                if typing || modifiers.command || modifiers.alt {
                    continue;
                }

                // Platines et crossfader : position physique des touches.
                let key = physical_key.unwrap_or(*key);
                match key {
                    Key::A => self.held.seek_back[0] = pressed,
                    Key::S => self.held.seek_forward[0] = pressed,
                    Key::J => self.held.seek_back[1] = pressed,
                    Key::K => self.held.seek_forward[1] = pressed,
                    Key::G => self.held.crossfade_left = pressed,
                    Key::H => self.held.crossfade_right = pressed,
                    _ => {}
                }
                if pressed {
                    // Tempo : la répétition de la touche est acceptée.
                    let step = if modifiers.shift { 0.001 } else { 0.01 };
                    match key {
                        Key::F1 => actions.push(Action::TempoStep(0, -step)),
                        Key::F2 => actions.push(Action::TempoStep(0, step)),
                        Key::F5 => actions.push(Action::TempoStep(1, -step)),
                        Key::F6 => actions.push(Action::TempoStep(1, step)),
                        _ => {}
                    }
                }
                if pressed && !repeat {
                    let action = match (key, modifiers.shift) {
                        (Key::Num1, _) => Some(Action::ToggleSync(0)),
                        (Key::Num6, _) => Some(Action::ToggleSync(1)),
                        (Key::D, false) => Some(Action::TogglePlay(0)),
                        (Key::D, true) => Some(Action::CueSet(0)),
                        (Key::F, false) => Some(Action::Cue(0)),
                        (Key::F, true) => Some(Action::CueGotoAndStop(0)),
                        (Key::B, _) => Some(Action::BassKill(0)),
                        (Key::L, false) => Some(Action::TogglePlay(1)),
                        (Key::L, true) => Some(Action::CueSet(1)),
                        (Key::Semicolon, false) => Some(Action::Cue(1)),
                        (Key::Semicolon, true) => Some(Action::CueGotoAndStop(1)),
                        (Key::N, _) => Some(Action::BassKill(1)),
                        (Key::F7, _) => Some(Action::TransitionTrigger(0)),
                        (Key::F8, _) => Some(Action::TransitionTrigger(1)),
                        (Key::F9, _) => Some(Action::TransitionTrigger(2)),
                        (Key::F10, _) => Some(Action::TransitionTrigger(3)),
                        _ => marker_key(key).map(|(deck, slot)| {
                            if modifiers.shift {
                                Action::MarkerClear(deck, slot)
                            } else {
                                Action::Marker(deck, slot)
                            }
                        }),
                    };
                    actions.extend(action);
                }
            }
        });

        actions
    }
}

/// Touches des marqueurs 1 à 4 (rangée des chiffres, comme les hot cues de
/// Mixxx) : 2–5 pour la platine A, 7–0 pour la platine B.
fn marker_key(key: Key) -> Option<(usize, usize)> {
    match key {
        Key::Num2 => Some((0, 0)),
        Key::Num3 => Some((0, 1)),
        Key::Num4 => Some((0, 2)),
        Key::Num5 => Some((0, 3)),
        Key::Num7 => Some((1, 0)),
        Key::Num8 => Some((1, 1)),
        Key::Num9 => Some((1, 2)),
        Key::Num0 => Some((1, 3)),
        _ => None,
    }
}
