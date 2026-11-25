//! Transitions planifiées entre deux morceaux : une liste d'étapes déclenchées
//! quand le morceau de départ atteint ses marqueurs (« au marqueur 1, lance
//! la platine de destination », « au marqueur 2, coupe la voix du départ et
//! descends ses aigus à −15 dB en 1 s »…).
//!
//! Les transitions sont enregistrées localement dans `transitions.json`, à
//! côté de la configuration.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::audio::dsp::{db_to_gain, fader_to_gain, gain_to_db, knob_to_gain};
use crate::markers::{MARKER_END, MARKER_START, NUM_TRIGGERS, Triggers, long_name, short_name};

/// Déclencheurs manuels : après les 8 marqueurs, le début et la fin de la
/// musique. Ils ne dépendent pas de la lecture : on les lance d'un clic
/// (bouton ▶) ou au clavier ([`MANUAL_KEYS`]).
pub const MANUAL_FIRST: usize = NUM_TRIGGERS;
pub const NUM_MANUAL: usize = 4;
/// Nombre total de déclencheurs (marqueurs, début / fin, manuels).
pub const ALL_TRIGGERS: usize = NUM_TRIGGERS + NUM_MANUAL;
/// Touches des déclencheurs manuels 1 à 4.
pub const MANUAL_KEYS: [&str; NUM_MANUAL] = ["F7", "F8", "F9", "F10"];

/// Vrai pour un déclencheur manuel.
pub fn is_manual(trigger: usize) -> bool {
    (MANUAL_FIRST..ALL_TRIGGERS).contains(&trigger)
}

/// Nom court d'un déclencheur (« M1 », « DÉBUT », « MAN 1 »…).
pub fn trigger_short_name(trigger: usize) -> String {
    if is_manual(trigger) {
        format!("MAN {}", trigger - MANUAL_FIRST + 1)
    } else {
        short_name(trigger)
    }
}

/// Nom complet d'un déclencheur (« marqueur 2 », « déclencheur manuel 1 »…).
pub fn trigger_long_name(trigger: usize) -> String {
    if is_manual(trigger) {
        format!("déclencheur manuel {}", trigger - MANUAL_FIRST + 1)
    } else {
        long_name(trigger)
    }
}

/// Platine concernée par une action, relativement à la transition.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Side {
    /// Platine qui joue le morceau de départ.
    From,
    /// Platine où est chargé le morceau de destination.
    To,
}

impl Side {
    pub fn label(self) -> &'static str {
        match self {
            Side::From => "Départ",
            Side::To => "Destination",
        }
    }
}

/// Réglage d'une platine qu'une action peut modifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Control {
    Volume,
    High,
    Mid,
    Low,
    Vocals,
    Drums,
    Bass,
    Instruments,
}

/// Plus faible niveau réglable : en dessous, le réglage est coupé.
pub const MIN_DB: f32 = -60.0;

impl Control {
    #[cfg(test)]
    pub const ALL: [Control; 8] = [
        Control::Volume,
        Control::High,
        Control::Mid,
        Control::Low,
        Control::Vocals,
        Control::Drums,
        Control::Bass,
        Control::Instruments,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Control::Volume => "Volume",
            Control::High => "EQ High",
            Control::Mid => "EQ Mid",
            Control::Low => "EQ Low",
            Control::Vocals => "Voix",
            Control::Drums => "Batterie",
            Control::Bass => "Basse",
            Control::Instruments => "Instruments",
        }
    }

    /// Bande d'EQ côté moteur (`[low, mid, high]`), si c'en est une.
    pub fn eq_band(self) -> Option<usize> {
        match self {
            Control::Low => Some(0),
            Control::Mid => Some(1),
            Control::High => Some(2),
            _ => None,
        }
    }

    /// Stem côté moteur (voix, batterie, basse, instruments), si c'en est un.
    pub fn stem(self) -> Option<usize> {
        match self {
            Control::Vocals => Some(0),
            Control::Drums => Some(1),
            Control::Bass => Some(2),
            Control::Instruments => Some(3),
            _ => None,
        }
    }

    /// Plage réglable en dB : les EQ montent à +6 dB, les volumes à 0 dB.
    pub fn db_range(self) -> std::ops::RangeInclusive<f32> {
        if self.eq_band().is_some() {
            MIN_DB..=6.0
        } else {
            MIN_DB..=0.0
        }
    }

    /// Niveau en dB pour une position du contrôle (inverse de
    /// [`Control::position_for_db`]), arrondi au demi-dB ; tout en bas : coupé.
    pub fn db_for_position(self, position: f32) -> f32 {
        let gain = if self.eq_band().is_some() {
            knob_to_gain(position)
        } else {
            fader_to_gain(position)
        };
        let db = gain_to_db(gain);
        if db <= MIN_DB {
            MIN_DB
        } else {
            ((db * 2.0).round() / 2.0).min(*self.db_range().end())
        }
    }

    /// Libellé court, celui du bouton de la platine (VOIX, HIGH, VOL…).
    pub fn button_label(self) -> &'static str {
        match self {
            Control::Volume => "VOL",
            Control::High => "HIGH",
            Control::Mid => "MID",
            Control::Low => "LOW",
            Control::Vocals => "VOIX",
            Control::Drums => "BATT",
            Control::Bass => "BASSE",
            Control::Instruments => "INSTR",
        }
    }

    /// Position du contrôle de l'interface (0..1) pour `db` : potentiomètre
    /// pour l'EQ, fader pour le volume et les stems.
    pub fn position_for_db(self, db: f32) -> f32 {
        if db <= MIN_DB {
            return 0.0;
        }
        if self.eq_band().is_some() {
            // Voir `knob_to_gain` : 0,5 → 0 dB, 1 → +6 dB, en dessous gain = (2p)².
            if db >= 0.0 {
                (0.5 + db / 12.0).min(1.0)
            } else {
                db_to_gain(db).sqrt() / 2.0
            }
        } else {
            // Voir `fader_to_gain` : gain = p².
            db_to_gain(db.min(0.0)).sqrt()
        }
    }
}

/// Ce que fait une étape.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Action {
    /// Lance la lecture (rien si la platine joue déjà).
    Play { side: Side },
    /// Arrête la lecture.
    Stop { side: Side },
    /// Amène un réglage à `db` en `secs` secondes (0 : immédiat), en partant
    /// de `from` dB s'il est donné (posé d'abord, sans fondu).
    Fade {
        side: Side,
        control: Control,
        db: f32,
        secs: f32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<f32>,
    },
    /// Coupe un réglage (kill pour l'EQ, coupure pour un stem, volume à zéro).
    Mute { side: Side, control: Control },
    /// Rétablit un réglage coupé (volume : 0 dB).
    Unmute { side: Side, control: Control },
    /// Amène le crossfader à `toward` (0 : côté départ, 1 : côté destination),
    /// en partant de `from` s'il est donné.
    Crossfader {
        toward: f32,
        secs: f32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<f32>,
    },
    /// Cale une fois le tempo de la platine sur l'autre (comme un clic sur SYNC).
    Sync { side: Side },
    /// Accorde une fois la tonalité sur l'autre platine (clic sur MATCH).
    Match { side: Side },
    /// Place la lecture sur un marqueur (0 à 7) de la platine.
    Jump { side: Side, marker: usize },
    /// Amène le tempo à `percent` % du tempo d'origine (0 : tempo d'origine)
    /// en `secs` secondes, en partant de `from` % s'il est donné ; coupe le
    /// SYNC permanent.
    Tempo {
        side: Side,
        percent: f32,
        secs: f32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<f32>,
    },
    /// Transpose la platine de `semitones` demi-tons (slider de tonalité) ;
    /// coupe le MATCH permanent.
    Key { side: Side, semitones: i32 },
    /// Amène le filtre à `value` (−1 : passe-bas fermé, 0 : neutre, 1 :
    /// passe-haut fermé) en `secs` secondes, en partant de `from` s'il est
    /// donné ; allume le filtre de la platine.
    Filter {
        side: Side,
        value: f32,
        secs: f32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<f32>,
    },
    /// Boucle de `beats` temps sur la grille de la platine (une boucle en
    /// cours garde son début : on peut la raccourcir) ; `None` : en sortir.
    Loop {
        side: Side,
        #[serde(default)]
        beats: Option<f32>,
    },
}

/// Types d'action proposés dans l'éditeur, avec une valeur par défaut.
pub const ACTION_KINDS: [(&str, Action); 13] = [
    ("Lancer", Action::Play { side: Side::To }),
    ("Arrêter", Action::Stop { side: Side::From }),
    (
        "Régler",
        Action::Fade {
            side: Side::From,
            control: Control::High,
            db: -15.0,
            secs: 1.0,
            from: None,
        },
    ),
    (
        "Couper",
        Action::Mute {
            side: Side::From,
            control: Control::Vocals,
        },
    ),
    (
        "Rétablir",
        Action::Unmute {
            side: Side::To,
            control: Control::Vocals,
        },
    ),
    (
        "Crossfader",
        Action::Crossfader {
            toward: 1.0,
            secs: 8.0,
            from: None,
        },
    ),
    ("SYNC", Action::Sync { side: Side::To }),
    ("MATCH", Action::Match { side: Side::To }),
    (
        "Aller au marqueur",
        Action::Jump {
            side: Side::To,
            marker: 0,
        },
    ),
    (
        "Tempo",
        Action::Tempo {
            side: Side::To,
            percent: 0.0,
            secs: 8.0,
            from: None,
        },
    ),
    (
        "Tonalité",
        Action::Key {
            side: Side::To,
            semitones: 0,
        },
    ),
    (
        "Filtre",
        Action::Filter {
            side: Side::From,
            value: 0.6,
            secs: 8.0,
            from: None,
        },
    ),
    (
        "Boucle",
        Action::Loop {
            side: Side::From,
            beats: Some(4.0),
        },
    ),
];

impl Action {
    /// Indice du type d'action dans [`ACTION_KINDS`].
    pub fn kind(&self) -> usize {
        match self {
            Action::Play { .. } => 0,
            Action::Stop { .. } => 1,
            Action::Fade { .. } => 2,
            Action::Mute { .. } => 3,
            Action::Unmute { .. } => 4,
            Action::Crossfader { .. } => 5,
            Action::Sync { .. } => 6,
            Action::Match { .. } => 7,
            Action::Jump { .. } => 8,
            Action::Tempo { .. } => 9,
            Action::Key { .. } => 10,
            Action::Filter { .. } => 11,
            Action::Loop { .. } => 12,
        }
    }

    pub fn side(&self) -> Option<Side> {
        match *self {
            Action::Play { side }
            | Action::Stop { side }
            | Action::Fade { side, .. }
            | Action::Mute { side, .. }
            | Action::Unmute { side, .. }
            | Action::Sync { side }
            | Action::Match { side }
            | Action::Jump { side, .. }
            | Action::Tempo { side, .. }
            | Action::Key { side, .. }
            | Action::Filter { side, .. }
            | Action::Loop { side, .. } => Some(side),
            Action::Crossfader { .. } => None,
        }
    }

    pub fn set_side(&mut self, new: Side) {
        match self {
            Action::Play { side }
            | Action::Stop { side }
            | Action::Fade { side, .. }
            | Action::Mute { side, .. }
            | Action::Unmute { side, .. }
            | Action::Sync { side }
            | Action::Match { side }
            | Action::Jump { side, .. }
            | Action::Tempo { side, .. }
            | Action::Key { side, .. }
            | Action::Filter { side, .. }
            | Action::Loop { side, .. } => *side = new,
            Action::Crossfader { .. } => {}
        }
    }

    /// Description lisible, pour les messages d'état.
    pub fn describe(&self) -> String {
        match *self {
            Action::Play { side } => format!("lance {}", side.label()),
            Action::Stop { side } => format!("arrête {}", side.label()),
            Action::Fade {
                side,
                control,
                db,
                secs,
                ..
            } => {
                format!("{} {} → {}", side.label(), control.label(), format_fade(db, secs))
            }
            Action::Mute { side, control } => format!("coupe {} de {}", control.label(), side.label()),
            Action::Unmute { side, control } => format!("rétablit {} de {}", control.label(), side.label()),
            Action::Crossfader { toward, secs, .. } => {
                format!("crossfader → {:.0} % destination en {secs:.1} s", toward * 100.0)
            }
            Action::Sync { side } => format!("SYNC {}", side.label()),
            Action::Match { side } => format!("MATCH {}", side.label()),
            Action::Jump { side, marker } => format!("{} au {}", side.label(), long_name(marker)),
            Action::Tempo {
                side, percent, secs, ..
            } => {
                let target = if percent == 0.0 {
                    "tempo d'origine".to_string()
                } else {
                    format!("{percent:+.1} %")
                };
                if secs > 0.0 {
                    format!("tempo de {} → {target} en {secs:.1} s", side.label())
                } else {
                    format!("tempo de {} → {target}", side.label())
                }
            }
            Action::Key { side, semitones } => match semitones {
                0 => format!("tonalité d'origine pour {}", side.label()),
                n => format!("tonalité de {} → {n:+} demi-ton(s)", side.label()),
            },
            Action::Filter { side, value, secs, .. } => {
                let target = filter_label(value);
                if secs > 0.0 {
                    format!("filtre de {} → {target} en {secs:.1} s", side.label())
                } else {
                    format!("filtre de {} → {target}", side.label())
                }
            }
            Action::Loop {
                side,
                beats: Some(beats),
            } => {
                format!("boucle de {} temps sur {}", beat_label(beats), side.label())
            }
            Action::Loop { side, beats: None } => format!("{} sort de sa boucle", side.label()),
        }
    }
}

/// Réglage du filtre, lisible : « neutre », « passe-bas 40 % »…
pub fn filter_label(value: f32) -> String {
    let percent = (value.abs() * 100.0).round();
    if percent == 0.0 {
        "neutre".to_string()
    } else if value < 0.0 {
        format!("passe-bas {percent:.0} %")
    } else {
        format!("passe-haut {percent:.0} %")
    }
}

/// Nombre de temps lisible : 4, 1, ½, ¼…
pub fn beat_label(beats: f32) -> String {
    match beats {
        0.5 => "½".to_string(),
        0.25 => "¼".to_string(),
        0.125 => "⅛".to_string(),
        beats if beats.fract() == 0.0 => format!("{beats:.0}"),
        beats => format!("{beats}"),
    }
}

fn format_fade(db: f32, secs: f32) -> String {
    let level = if db <= MIN_DB {
        "coupé".to_string()
    } else {
        format!("{db:+.0} dB")
    };
    if secs > 0.0 {
        format!("{level} en {secs:.1} s")
    } else {
        level
    }
}

/// Attente entre un déclencheur et une étape.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum TimeOp {
    /// Attendre des secondes.
    Secs { secs: f32 },
    /// Attendre des temps, au tempo de la platine de départ.
    Beats { beats: f32 },
    /// Attendre le prochain multiple de `beats` temps sur la grille du départ
    /// (1 : temps, 4 : mesure…).
    Quantize { beats: u32 },
}

/// Opérations au plus dans un chemin de temps.
pub const MAX_TIME_OPS: usize = 16;
/// Durée d'un temps pour les estimations (120 BPM), avant l'exécution.
pub const ESTIMATED_BEAT_SECS: f64 = 0.5;

/// Chemin de temps d'une étape depuis son déclencheur, quand il passe par
/// des temps musicaux (délai en temps, quantification) : l'instant ne se
/// calcule qu'à l'exécution, sur le tempo et la grille du départ.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(from = "Vec<TimeOp>", into = "Vec<TimeOp>")]
pub struct Timing {
    ops: [Option<TimeOp>; MAX_TIME_OPS],
}

impl Timing {
    pub fn is_empty(&self) -> bool {
        self.ops[0].is_none()
    }

    pub fn ops(&self) -> impl Iterator<Item = TimeOp> + '_ {
        self.ops.iter().map_while(|op| *op)
    }

    /// Ajoute une attente (fusionnée avec la précédente si même unité).
    pub fn push(&mut self, op: TimeOp) {
        let len = self.ops().count();
        if let Some(last) = len.checked_sub(1).and_then(|index| self.ops[index].as_mut()) {
            match (last, op) {
                (TimeOp::Secs { secs }, TimeOp::Secs { secs: more }) => {
                    *secs += more;
                    return;
                }
                (TimeOp::Beats { beats }, TimeOp::Beats { beats: more }) => {
                    *beats += more;
                    return;
                }
                _ => {}
            }
        }
        if len < MAX_TIME_OPS {
            self.ops[len] = Some(op);
        }
    }

    /// Vrai si le chemin dépend du tempo ou de la grille du départ.
    pub fn is_musical(&self) -> bool {
        self.ops().any(|op| !matches!(op, TimeOp::Secs { .. }))
    }

    /// Instant estimé, en secondes (temps à 120 BPM, quantification
    /// ignorée) : sert à ordonner les étapes avant l'exécution.
    pub fn estimate(&self) -> f64 {
        self.ops()
            .map(|op| match op {
                TimeOp::Secs { secs } => secs.max(0.0) as f64,
                TimeOp::Beats { beats } => beats.max(0.0) as f64 * ESTIMATED_BEAT_SECS,
                TimeOp::Quantize { .. } => 0.0,
            })
            .sum()
    }
}

impl From<Vec<TimeOp>> for Timing {
    fn from(ops: Vec<TimeOp>) -> Self {
        let mut timing = Timing::default();
        for op in ops {
            timing.push(op);
        }
        timing
    }
}

impl From<Timing> for Vec<TimeOp> {
    fn from(timing: Timing) -> Self {
        timing.ops().collect()
    }
}

/// Une étape : `action`, quand le déclencheur `marker` se produit (marqueur
/// 1 à 8, début / fin de la musique, ou déclencheur manuel).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Step {
    pub marker: usize,
    /// Attente avant l'action, en secondes, après l'étape précédente du même
    /// déclencheur (ou après le déclenchement pour la première) : les étapes
    /// d'un déclencheur s'enchaînent ainsi automatiquement.
    #[serde(default)]
    pub wait: f32,
    pub action: Action,
    /// Chemin de temps musical depuis le déclenchement : s'il n'est pas vide,
    /// il donne l'instant de l'étape et `wait` est ignoré (l'étape ne compte
    /// pas dans l'enchaînement des autres).
    #[serde(default, skip_serializing_if = "Timing::is_empty")]
    pub timing: Timing,
}

impl Step {
    fn with_wait(self, wait: f32) -> Self {
        Self { wait, ..self }
    }

    pub fn new(marker: usize, action: Action) -> Self {
        Self {
            marker,
            wait: 0.0,
            action,
            timing: Timing::default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Transition {
    pub id: u64,
    pub name: String,
    /// Morceau de départ (celui dont les marqueurs déclenchent les étapes).
    pub from: PathBuf,
    /// Morceau de destination.
    pub to: PathBuf,
    /// Titres des deux morceaux, pour l'affichage.
    #[serde(default)]
    pub from_title: String,
    #[serde(default)]
    pub to_title: String,
    pub steps: Vec<Step>,
    /// Graphe de nœuds (éditeur visuel), s'il a été utilisé ; les étapes en
    /// sont la traduction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph: Option<crate::transition_graph::Graph>,
}

impl Transition {
    /// Indices des étapes dans l'ordre du morceau : par position de leur
    /// marqueur (`positions`, si connue), le début de la musique en premier
    /// et la fin en dernier ; dans l'ordre d'ajout pour un même marqueur.
    pub fn ordered_steps(&self, positions: &Triggers) -> Vec<usize> {
        let key = |marker: usize| match (positions.get(marker).copied().flatten(), marker) {
            (Some(secs), _) => secs,
            (None, MARKER_START) => -1.0,
            (None, MARKER_END) => 2e9,
            (None, slot) if is_manual(slot) => 3e9 + slot as f64,
            (None, slot) => 1e9 + slot as f64,
        };
        let mut order: Vec<usize> = (0..self.steps.len()).collect();
        order.sort_by(|&a, &b| key(self.steps[a].marker).total_cmp(&key(self.steps[b].marker)));
        order
    }
}

/// Un modèle de transition : nom, description, étapes.
pub struct Template {
    pub name: &'static str,
    pub description: &'static str,
    pub steps: Vec<Step>,
}

/// Modèles d'étapes proposés à la création ou à l'ajout (« types » de transition).
pub fn templates() -> Vec<Template> {
    use Control::{Bass, Drums, High, Instruments, Low, Mid, Vocals, Volume};
    use Side::{From, To};
    const MAN1: usize = MANUAL_FIRST;
    // Étape au déclencheur `marker`, après `wait` secondes.
    let at = |marker: usize, wait: f32, action: Action| Step::new(marker, action).with_wait(wait);
    // Étape après un chemin de temps musical (temps, quantification).
    let after = |marker: usize, ops: &[TimeOp], action: Action| Step {
        timing: Timing::from(ops.to_vec()),
        ..Step::new(marker, action)
    };
    let beats = |beats: f32| TimeOp::Beats { beats };
    let bar = TimeOp::Quantize { beats: 4 };
    let looping = |side, beats| Action::Loop { side, beats };
    let filter = |side, value, secs, from| Action::Filter {
        side,
        value,
        secs,
        from,
    };
    let play = |side| Action::Play { side };
    let stop = |side| Action::Stop { side };
    let sync = |side| Action::Sync { side };
    let fade = |side, control, db, secs| Action::Fade {
        side,
        control,
        db,
        secs,
        from: None,
    };
    let mute = |side, control| Action::Mute { side, control };
    let unmute = |side, control| Action::Unmute { side, control };
    let crossfader = |toward, secs| Action::Crossfader {
        toward,
        secs,
        from: None,
    };
    vec![
        Template {
            name: "Lancement simple",
            description: "Marqueur 1 : SYNC et lancement de la destination.",
            steps: vec![at(0, 0.0, sync(To)), at(0, 0.0, play(To))],
        },
        Template {
            name: "Échange des basses",
            description: "Marqueur 1 : la destination démarre sans basses. Marqueur 2 : les basses \
                          passent du départ à la destination en 2 s. Marqueur 3 : fondu en 8 s.",
            steps: vec![
                at(0, 0.0, fade(To, Low, MIN_DB, 0.0)),
                at(0, 0.0, sync(To)),
                at(0, 0.0, play(To)),
                at(1, 0.0, fade(From, Low, MIN_DB, 2.0)),
                at(1, 0.0, fade(To, Low, 0.0, 2.0)),
                at(2, 0.0, crossfader(1.0, 8.0)),
            ],
        },
        Template {
            name: "Voix coupée puis fondu",
            description: "Marqueur 1 : lancement. Marqueur 2 : la voix du départ est coupée et ses \
                          aigus baissent. Marqueur 3 : fondu en 8 s. Marqueur 4 : arrêt du départ.",
            steps: vec![
                at(0, 0.0, sync(To)),
                at(0, 0.0, play(To)),
                at(1, 0.0, mute(From, Vocals)),
                at(1, 0.0, fade(From, High, -15.0, 1.0)),
                at(2, 0.0, crossfader(1.0, 8.0)),
                at(3, 0.0, stop(From)),
            ],
        },
        Template {
            name: "Fondu long",
            description: "Marqueur 1 : lancement calé. Marqueur 2 : crossfader en 16 s, puis arrêt \
                          du départ ; la destination revient doucement à son tempo d'origine.",
            steps: vec![
                at(0, 0.0, sync(To)),
                at(0, 0.0, play(To)),
                at(1, 0.0, crossfader(1.0, 16.0)),
                at(1, 16.0, stop(From)),
                at(
                    1,
                    0.0,
                    Action::Tempo {
                        side: To,
                        percent: 0.0,
                        secs: 16.0,
                        from: None,
                    },
                ),
            ],
        },
        Template {
            name: "Coupure nette (manuelle)",
            description: "Marqueur 1 : la destination tourne calée mais muette. Déclencheur manuel 1 \
                          (F7) : bascule instantanée sur la destination, le départ s'arrête.",
            steps: vec![
                at(0, 0.0, fade(To, Volume, MIN_DB, 0.0)),
                at(0, 0.0, sync(To)),
                at(0, 0.0, play(To)),
                at(MAN1, 0.0, fade(To, Volume, 0.0, 0.0)),
                at(MAN1, 0.0, crossfader(1.0, 0.0)),
                at(MAN1, 0.5, stop(From)),
            ],
        },
        Template {
            name: "Balayage EQ",
            description: "Marqueur 1 : la destination démarre sans basses. Marqueur 2 : en 8 s, le \
                          départ perd aigus, médiums et basses pendant que la destination récupère les \
                          siennes ; puis fondu en 4 s et arrêt.",
            steps: vec![
                at(0, 0.0, fade(To, Low, MIN_DB, 0.0)),
                at(0, 0.0, sync(To)),
                at(0, 0.0, play(To)),
                at(1, 0.0, fade(From, High, -40.0, 8.0)),
                at(1, 0.0, fade(From, Mid, -24.0, 8.0)),
                at(1, 0.0, fade(From, Low, MIN_DB, 8.0)),
                at(1, 0.0, fade(To, Low, 0.0, 8.0)),
                at(1, 8.0, crossfader(1.0, 4.0)),
                at(1, 4.0, stop(From)),
            ],
        },
        Template {
            name: "Montée progressive (enchaînée)",
            description: "Tout part du marqueur 1, enchaîné par des délais : la destination monte par \
                          paliers pendant 20 s, prend les basses, puis le fondu termine.",
            steps: vec![
                at(0, 0.0, fade(To, Volume, MIN_DB, 0.0)),
                at(0, 0.0, sync(To)),
                at(0, 0.0, play(To)),
                at(0, 0.0, fade(To, Volume, -12.0, 8.0)),
                at(0, 8.0, fade(To, Volume, -6.0, 8.0)),
                at(0, 8.0, fade(To, Volume, 0.0, 4.0)),
                at(0, 0.0, fade(From, Low, MIN_DB, 4.0)),
                at(0, 4.0, crossfader(1.0, 8.0)),
                at(0, 8.0, stop(From)),
            ],
        },
        Template {
            name: "Mashup a cappella (stems)",
            description: "Nécessite les stems des deux morceaux. Marqueur 1 : la voix seule de la \
                          destination remplace celle du départ. Marqueur 2 : la destination complète \
                          revient. Marqueur 3 : fondu en 8 s puis arrêt.",
            steps: vec![
                at(0, 0.0, mute(To, Drums)),
                at(0, 0.0, mute(To, Bass)),
                at(0, 0.0, mute(To, Instruments)),
                at(0, 0.0, sync(To)),
                at(0, 0.0, Action::Match { side: To }),
                at(0, 0.0, play(To)),
                at(0, 0.0, mute(From, Vocals)),
                at(1, 0.0, unmute(To, Drums)),
                at(1, 0.0, unmute(To, Bass)),
                at(1, 0.0, unmute(To, Instruments)),
                at(1, 0.0, fade(From, Low, MIN_DB, 2.0)),
                at(2, 0.0, crossfader(1.0, 8.0)),
                at(2, 8.0, stop(From)),
            ],
        },
        Template {
            name: "Enchaînement en fin de musique",
            description: "À la fin de la musique du départ : la destination part de son début de \
                          musique, sans chevauchement (style radio).",
            steps: vec![
                at(
                    MARKER_END,
                    0.0,
                    Action::Jump {
                        side: To,
                        marker: MARKER_START,
                    },
                ),
                at(MARKER_END, 0.0, play(To)),
                at(MARKER_END, 0.0, crossfader(1.0, 0.0)),
                at(MARKER_END, 1.0, stop(From)),
            ],
        },
        Template {
            name: "Drop manuel",
            description: "Déclencheur manuel 1 (F7) : la destination repart de son marqueur 1, calée, \
                          pendant que le départ s'arrête net.",
            steps: vec![
                at(MAN1, 0.0, Action::Jump { side: To, marker: 0 }),
                at(MAN1, 0.0, sync(To)),
                at(MAN1, 0.0, play(To)),
                at(MAN1, 0.0, crossfader(1.0, 0.0)),
                at(MAN1, 0.0, stop(From)),
            ],
        },
        Template {
            name: "Loop roll",
            description: "Marqueur 1 : SYNC de la destination, placée au début de sa musique. \
                          Marqueur 2 : le départ boucle 4 temps, puis 2, 1, ½ et ¼ (calés sur ses \
                          temps) pendant que son filtre monte ; au bout de 8 temps, la destination \
                          part et le départ s'arrête.",
            steps: vec![
                at(0, 0.0, sync(To)),
                at(
                    0,
                    0.0,
                    Action::Jump {
                        side: To,
                        marker: MARKER_START,
                    },
                ),
                at(1, 0.0, looping(From, Some(4.0))),
                at(1, 0.0, filter(From, 0.6, 4.0, None)),
                after(1, &[beats(4.0)], looping(From, Some(2.0))),
                after(1, &[beats(6.0)], looping(From, Some(1.0))),
                after(1, &[beats(7.0)], looping(From, Some(0.5))),
                after(1, &[beats(7.5)], looping(From, Some(0.25))),
                after(1, &[beats(8.0)], play(To)),
                after(1, &[beats(8.0)], crossfader(1.0, 0.0)),
                after(1, &[beats(8.0)], stop(From)),
                after(1, &[beats(8.0)], looping(From, None)),
                after(1, &[beats(8.0)], filter(From, 0.0, 0.0, None)),
            ],
        },
        Template {
            name: "Sortie filtrée",
            description: "Marqueur 1 : SYNC ; à la mesure suivante, la destination part filtrée \
                          (passe-bas) et s'ouvre en 8 s pendant que le passe-haut monte sur le \
                          départ et que le crossfader passe ; 8 s plus tard, le départ s'arrête.",
            steps: vec![
                at(0, 0.0, sync(To)),
                after(0, &[bar], play(To)),
                after(0, &[bar], filter(To, 0.0, 8.0, Some(-0.8))),
                after(0, &[bar], filter(From, 0.7, 8.0, None)),
                after(0, &[bar], crossfader(1.0, 8.0)),
                after(0, &[bar, TimeOp::Secs { secs: 8.0 }], stop(From)),
                after(0, &[bar, TimeOp::Secs { secs: 8.0 }], filter(From, 0.0, 0.0, None)),
            ],
        },
    ]
}

/// Passage d'un marqueur entre deux positions successives du départ.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Crossing {
    /// Marqueur pas encore atteint (ou déjà dépassé avant).
    None,
    /// Atteint en lecture normale : l'étape se déclenche.
    Reached,
    /// Sauté par un déplacement (recherche, saut à un marqueur) : ignoré.
    Skipped,
}

/// Plus grand pas entre deux images considéré comme de la lecture normale.
const MAX_PLAY_STEP_SECS: f64 = 0.5;

/// Le marqueur `marker` a-t-il été franchi en passant de `last` à `position`
/// (en secondes) ? Un marqueur posé exactement sur la position de départ se
/// déclenche dès que la lecture avance.
pub fn crossing(last: f64, position: f64, marker: f64) -> Crossing {
    if !(last <= marker && marker < position) {
        Crossing::None
    } else if position - last <= MAX_PLAY_STEP_SECS {
        Crossing::Reached
    } else {
        Crossing::Skipped
    }
}

/// Toutes les transitions enregistrées.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct TransitionStore {
    #[serde(default)]
    transitions: Vec<Transition>,
    #[serde(skip)]
    path: Option<PathBuf>,
}

impl TransitionStore {
    /// Charge le fichier ; absent ou illisible, le magasin est vide.
    pub fn load(path: Option<PathBuf>) -> Self {
        let mut store: Self = path
            .as_ref()
            .and_then(|path| fs::read_to_string(path).ok())
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        store.path = path;
        store
    }

    pub fn save(&self) -> Result<(), String> {
        let path = self.path.as_ref().ok_or("dossier de configuration introuvable")?;
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        fs::write(path, text).map_err(|e| e.to_string())
    }

    /// Transitions qui partent du morceau `from`.
    pub fn starting_from<'a>(&'a self, from: &'a Path) -> impl Iterator<Item = &'a Transition> + 'a {
        self.transitions
            .iter()
            .filter(move |transition| transition.from == from)
    }

    pub fn get(&self, id: u64) -> Option<&Transition> {
        self.transitions.iter().find(|transition| transition.id == id)
    }

    pub fn get_mut(&mut self, id: u64) -> Option<&mut Transition> {
        self.transitions.iter_mut().find(|transition| transition.id == id)
    }

    /// Crée une transition et retourne son identifiant.
    pub fn create(&mut self, mut transition: Transition) -> u64 {
        transition.id = self
            .transitions
            .iter()
            .map(|transition| transition.id)
            .max()
            .unwrap_or(0)
            + 1;
        let id = transition.id;
        self.transitions.push(transition);
        id
    }

    pub fn remove(&mut self, id: u64) {
        self.transitions.retain(|transition| transition.id != id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::dsp::{fader_to_gain, gain_to_db, knob_to_gain};

    #[test]
    fn crossing_detection() {
        assert_eq!(crossing(9.98, 10.0, 10.0), Crossing::None);
        assert_eq!(crossing(9.99, 10.01, 10.0), Crossing::Reached);
        assert_eq!(crossing(10.0, 10.01, 10.0), Crossing::Reached);
        assert_eq!(crossing(10.0, 10.0, 10.0), Crossing::None); // À l'arrêt.
        assert_eq!(crossing(2.0, 30.0, 10.0), Crossing::Skipped);
        assert_eq!(crossing(30.0, 2.0, 10.0), Crossing::None); // Retour en arrière.
    }

    #[test]
    fn db_positions_match_the_controls() {
        for db in [-40.0f32, -15.0, -6.0, 0.0, 3.0, 6.0] {
            let eq = gain_to_db(knob_to_gain(Control::High.position_for_db(db)));
            assert!((eq - db).abs() < 0.01, "EQ {db} → {eq}");
        }
        for db in [-40.0f32, -15.0, -6.0, 0.0] {
            let volume = gain_to_db(fader_to_gain(Control::Volume.position_for_db(db)));
            assert!((volume - db).abs() < 0.01, "volume {db} → {volume}");
        }
        assert_eq!(Control::Vocals.position_for_db(MIN_DB), 0.0);
        for control in Control::ALL {
            for db in [MIN_DB, -15.0, -6.0, 0.0] {
                assert_eq!(
                    control.db_for_position(control.position_for_db(db)),
                    db,
                    "{}",
                    control.label()
                );
            }
        }
        assert_eq!(Control::Volume.position_for_db(6.0), 1.0);
    }

    #[test]
    fn store_round_trip() {
        let mut store = TransitionStore::default();
        let steps = templates().remove(2).steps;
        let id = store.create(Transition {
            id: 0,
            name: "Test".into(),
            from: "a.mp3".into(),
            to: "b.mp3".into(),
            from_title: "A".into(),
            to_title: "B".into(),
            steps,
            graph: None,
        });
        let text = serde_json::to_string(&store).unwrap();
        let back: TransitionStore = serde_json::from_str(&text).unwrap();
        assert_eq!(back.get(id), store.get(id));
        assert_eq!(back.starting_from(Path::new("a.mp3")).count(), 1);
        assert_eq!(back.starting_from(Path::new("b.mp3")).count(), 0);
    }

    #[test]
    fn steps_follow_the_track_order() {
        let step = |marker| Step::new(marker, Action::Play { side: Side::To });
        let transition = Transition {
            id: 1,
            name: String::new(),
            from: "a".into(),
            to: "b".into(),
            from_title: String::new(),
            to_title: String::new(),
            graph: None,
            steps: vec![
                step(MARKER_END),
                step(0),
                step(1),
                step(MARKER_START),
                step(0),
                step(MANUAL_FIRST),
            ],
        };
        let mut positions = [None; crate::markers::NUM_TRIGGERS];
        positions[0] = Some(30.0);
        positions[1] = Some(10.0); // Le marqueur 2 est avant le 1 dans le morceau.
        positions[MARKER_START] = Some(0.5);
        positions[MARKER_END] = Some(200.0);
        assert_eq!(transition.ordered_steps(&positions), vec![3, 2, 1, 4, 0, 5]);
        // Positions inconnues : début, marqueurs par numéro, fin.
        let unknown = [None; crate::markers::NUM_TRIGGERS];
        assert_eq!(transition.ordered_steps(&unknown), vec![3, 1, 4, 2, 0, 5]);
    }

    #[test]
    fn templates_are_valid_and_old_files_load() {
        for template in templates() {
            assert!(!template.steps.is_empty(), "{}", template.name);
            for step in &template.steps {
                assert!(step.marker < ALL_TRIGGERS, "{}", template.name);
                assert!(step.wait >= 0.0);
            }
        }
        // Fichier d'une version précédente : pas de champ `wait`.
        let old = r#"{"marker":1,"action":{"type":"Play","side":"To"}}"#;
        let step: Step = serde_json::from_str(old).unwrap();
        assert_eq!(step, Step::new(1, Action::Play { side: Side::To }));
        assert_eq!(trigger_short_name(MANUAL_FIRST + 1), "MAN 2");
        assert!(is_manual(MANUAL_FIRST) && !is_manual(MARKER_END));
    }
}
