//! Panneau des transitions — éditeur de nœuds et liste des transitions
//! disponibles entre les deux platines — et exécution de la transition
//! chargée (déclenchement des étapes sur les marqueurs du départ, fondus des
//! réglages).
//!
//! La transition va de la platine qui joue vers l'autre. On choisit dans la
//! colonne de droite une transition entre les morceaux chargés (A → B ou
//! B → A), ou on en crée une ; elle s'ouvre dans l'éditeur de nœuds, où on
//! l'édite puis l'active.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering::Relaxed;

use egui::{
    Align, Align2, Color32, Context, FontId, Layout, Popup, Rect, RichText, ScrollArea, Sense, Stroke, TextEdit, Ui,
    UiBuilder, pos2, vec2,
};

use super::deck::DeckUi;
use super::icons::{self, Icon};
use super::mixer::MixerUi;
use super::node_editor::{NodeEditor, RunView, StepStatus};
use super::theme;
use crate::audio::dsp::{fader_to_gain, gain_to_db, knob_to_gain};
use crate::audio::engine::{Command, EngineHandle, NUM_STEMS};
use crate::markers::{NUM_MARKERS, NUM_TRIGGERS};
use crate::transitions::{
    ALL_TRIGGERS, Action, Control, Crossing, MANUAL_FIRST, MIN_DB, NUM_MANUAL, Side, Step, TimeOp, Timing, Transition,
    TransitionStore, crossing, is_manual, templates, trigger_long_name,
};

const NUM_DECKS: usize = 2;

/// État d'une étape pendant l'exécution.
#[derive(Clone, Copy, PartialEq, Eq)]
enum StepState {
    Pending,
    Done,
    /// Marqueur sauté par un déplacement : l'étape est ignorée.
    Skipped,
    /// Déclenchée, en attente de son délai.
    Scheduled,
}

/// Déclenchement manuel demandé depuis l'éditeur ou le clavier.
#[derive(Clone, Copy)]
enum Fire {
    /// Toutes les étapes d'un déclencheur, enchaînées avec leurs délais.
    Group(usize),
}

/// Réglage de l'interface qu'un fondu fait évoluer (positions 0..1).
#[derive(Clone, Copy, PartialEq, Eq)]
enum RampTarget {
    Volume(usize),
    Eq(usize, usize),
    Stem(usize, usize),
    Tempo(usize),
    /// Bouton du filtre (0..1, 0,5 : neutre).
    Filter(usize),
    Crossfader,
}

impl RampTarget {
    fn get(self, decks: &[DeckUi; NUM_DECKS], mixer: &MixerUi) -> f32 {
        match self {
            RampTarget::Volume(deck) => decks[deck].volume,
            RampTarget::Eq(deck, band) => decks[deck].eq[band],
            RampTarget::Stem(deck, stem) => decks[deck].stem_levels[stem],
            RampTarget::Tempo(deck) => decks[deck].tempo,
            RampTarget::Filter(deck) => decks[deck].filter,
            RampTarget::Crossfader => mixer.crossfader,
        }
    }

    fn set(self, decks: &mut [DeckUi; NUM_DECKS], mixer: &mut MixerUi, value: f32) {
        match self {
            RampTarget::Volume(deck) => decks[deck].volume = value,
            RampTarget::Eq(deck, band) => decks[deck].eq[band] = value,
            RampTarget::Stem(deck, stem) => decks[deck].stem_levels[stem] = value,
            RampTarget::Tempo(deck) => decks[deck].tempo = value,
            RampTarget::Filter(deck) => decks[deck].filter = value,
            RampTarget::Crossfader => mixer.crossfader = value,
        }
    }
}

/// Fondu en cours.
struct Ramp {
    target: RampTarget,
    start: f32,
    end: f32,
    since: f64,
    secs: f32,
}

/// Transition en planification.
struct Plan {
    id: u64,
    active: bool,
    /// Platine de départ, figée pendant l'exécution.
    from_deck: usize,
    /// État de chaque étape (même indice que `Transition::steps`).
    states: Vec<StepState>,
    /// Position du départ à l'image précédente, en secondes.
    last_position: Option<f64>,
    ramps: Vec<Ramp>,
    /// Étapes déclenchées qui attendent leur délai : (indice, instant).
    queue: Vec<(usize, f64)>,
    /// Dernier déclenchement de chaque déclencheur (affichage des nœuds).
    fired_at: Vec<Option<f64>>,
}

impl Plan {
    fn new(id: u64, steps: usize) -> Self {
        Self {
            id,
            active: false,
            from_deck: 0,
            states: vec![StepState::Pending; steps],
            last_position: None,
            ramps: Vec::new(),
            queue: Vec::new(),
            fired_at: vec![None; ALL_TRIGGERS],
        }
    }

    fn reset(&mut self) {
        self.states.fill(StepState::Pending);
        self.last_position = None;
        self.ramps.clear();
        self.queue.clear();
        self.fired_at.fill(None);
    }

    /// Des étapes attendent leur délai, ou des fondus sont en cours.
    fn running(&self) -> bool {
        !self.queue.is_empty() || !self.ramps.is_empty()
    }

    /// Abandonne les étapes programmées et les fondus (désactivation).
    fn cancel(&mut self) {
        for (index, _) in self.queue.drain(..) {
            if let Some(state) = self.states.get_mut(index) {
                *state = StepState::Pending;
            }
        }
        self.ramps.clear();
    }

    /// Programme les étapes `indices` (dans cet ordre) : chacune attend son
    /// délai après la précédente, la première après `time` ; une étape à
    /// chemin musical tombe à l'instant que donne l'horloge du départ.
    fn schedule(&mut self, transition: &Transition, indices: &[usize], time: f64, clock: &BeatClock) {
        let mut chain = time;
        for &index in indices {
            let step = transition.steps[index];
            let at = if step.timing.is_empty() {
                chain += step.wait.max(0.0) as f64;
                chain
            } else {
                clock.resolve(&step.timing, time)
            };
            self.queue.retain(|(step, _)| *step != index);
            self.queue.push((index, at));
            self.states[index] = StepState::Scheduled;
        }
    }
}

/// Tempo et grille de la platine de départ au moment d'un déclenchement :
/// donne l'instant des étapes à délai en temps ou quantifiées.
struct BeatClock {
    /// Position dans le morceau, en secondes.
    position: f64,
    /// Vitesse de lecture (tempo relatif).
    rate: f64,
    playing: bool,
    /// Grille : instant d'un temps et durée d'un temps, en secondes de morceau.
    grid: Option<(f64, f64)>,
}

impl BeatClock {
    fn of(deck: &DeckUi, engine: &EngineHandle, index: usize) -> Self {
        Self {
            position: position_secs(engine, index),
            rate: (deck.tempo as f64).max(0.01),
            playing: engine.shared.decks[index].playing.load(Relaxed),
            grid: deck.beat_grid(),
        }
    }

    /// Durée réelle d'un temps (120 BPM sans grille).
    fn beat_secs(&self) -> f64 {
        self.grid
            .map_or(crate::transitions::ESTIMATED_BEAT_SECS, |(_, period)| period)
            / self.rate
    }

    /// Instant de fin d'un chemin de temps parti de `start`.
    fn resolve(&self, timing: &Timing, start: f64) -> f64 {
        let mut elapsed = 0.0;
        for op in timing.ops() {
            match op {
                TimeOp::Secs { secs } => elapsed += secs.max(0.0) as f64,
                TimeOp::Beats { beats } => elapsed += beats.max(0.0) as f64 * self.beat_secs(),
                TimeOp::Quantize { beats } => {
                    // Départ à l'arrêt ou sans grille : pas d'attente.
                    let Some((anchor, period)) = self.grid.filter(|_| self.playing) else {
                        continue;
                    };
                    let quantum = period * beats.max(1) as f64;
                    let position = self.position + elapsed * self.rate;
                    // Déjà sur la limite (à quelques ms près) : on y est.
                    let next = anchor + ((position - anchor) / quantum - 0.002).ceil() * quantum;
                    elapsed += (next - position).max(0.0) / self.rate;
                }
            }
        }
        start + elapsed
    }
}

/// Réglages d'une platine que l'enregistrement observe.
#[derive(Clone, Copy, PartialEq)]
struct DeckState {
    volume: f32,
    eq: [f32; 3],
    kill: [bool; 3],
    stem_levels: [f32; NUM_STEMS],
    stem_mute: [bool; NUM_STEMS],
    playing: bool,
    /// Réglage effectif du filtre (−1..1, 0 si éteint).
    filter: f32,
}

/// État des platines et du crossfader à un instant.
#[derive(Clone, Copy, PartialEq)]
struct Snapshot {
    decks: [DeckState; NUM_DECKS],
    crossfader: f32,
}

impl Snapshot {
    fn take(decks: &[DeckUi; NUM_DECKS], mixer: &MixerUi, engine: &EngineHandle) -> Self {
        Self {
            decks: std::array::from_fn(|deck| {
                let ui = &decks[deck];
                let shared = &engine.shared.decks[deck];
                DeckState {
                    volume: ui.volume,
                    eq: ui.eq,
                    kill: ui.kill,
                    stem_levels: ui.stem_levels,
                    stem_mute: ui.stem_mute,
                    playing: shared.playing.load(Relaxed) || shared.armed.load(Relaxed),
                    filter: ui.filter_amount(),
                }
            }),
            crossfader: mixer.crossfader,
        }
    }
}

/// Réglage enregistré (une étape au plus par réglage et par enregistrement).
#[derive(Clone, Copy, PartialEq, Eq)]
enum RecKey {
    Volume(usize),
    Eq(usize, usize),
    Kill(usize, usize),
    Stem(usize, usize),
    StemMute(usize, usize),
    Play(usize),
    Filter(usize),
    Crossfader,
    Sync(usize),
    Match(usize),
}

/// Bandes d'EQ et stems côté moteur → réglages des transitions.
const EQ_CONTROLS: [Control; 3] = [Control::Low, Control::Mid, Control::High];
const STEM_CONTROLS: [Control; NUM_STEMS] = [Control::Vocals, Control::Drums, Control::Bass, Control::Instruments];

/// Enregistrement en cours : les réglages modifiés sur les platines
/// deviennent des étapes du marqueur `marker`.
struct Recording {
    transition: u64,
    marker: usize,
    /// Platine de départ de la transition.
    from_deck: usize,
    /// État au début de l'enregistrement (revenir à cette valeur retire l'étape).
    original: Snapshot,
    /// État à l'image précédente.
    previous: Snapshot,
    /// Étapes créées : réglage, indice dans la transition, début du geste.
    steps: Vec<(RecKey, usize, f64)>,
}

impl Recording {
    fn side(&self, deck: usize) -> Side {
        if deck == self.from_deck { Side::From } else { Side::To }
    }

    /// Met à jour les indices après la suppression de l'étape `index`.
    fn forget_step(&mut self, index: usize) {
        self.steps.retain(|(_, step, _)| *step != index);
        for (_, step, _) in &mut self.steps {
            if *step > index {
                *step -= 1;
            }
        }
    }
}

/// Niveau en dB d'un fader (volume, stem), borné à [`MIN_DB`].
fn fader_db(position: f32) -> f32 {
    round_db(gain_to_db(fader_to_gain(position)))
}

/// Niveau en dB d'un potentiomètre d'EQ, borné à [`MIN_DB`].
fn knob_db(position: f32) -> f32 {
    round_db(gain_to_db(knob_to_gain(position)))
}

fn round_db(db: f32) -> f32 {
    if db <= MIN_DB { MIN_DB } else { (db * 2.0).round() / 2.0 }
}

/// Durée d'un geste enregistré, arrondie au dixième (instantané si très court).
fn gesture_secs(elapsed: f64) -> f32 {
    if elapsed < 0.25 {
        0.0
    } else {
        ((elapsed * 10.0).round() / 10.0) as f32
    }
}

/// Couleur d'un marqueur (les spéciaux ont la leur).
pub(super) fn marker_color(marker: usize) -> Color32 {
    if marker < NUM_MARKERS {
        theme::MARKER_COLORS[marker]
    } else if is_manual(marker) {
        theme::MANUAL_TRIGGER
    } else {
        theme::SPECIAL_MARKER
    }
}

/// Ce que l'application doit faire après l'affichage de la fenêtre.
#[derive(Default)]
pub struct TransitionsOutcome {
    /// Charger ce morceau sur cette platine.
    pub load: Option<(usize, PathBuf)>,
    /// Message d'état (texte, erreur ?).
    pub status: Option<(String, bool)>,
}

pub struct TransitionsWindow {
    store: TransitionStore,
    /// Platine de départ (sens de la transition).
    from_deck: usize,
    plan: Option<Plan>,
    /// Transition dont la suppression attend une confirmation.
    confirm_delete: Option<u64>,
    /// Transition en cours de renommage (et focus à donner au champ).
    renaming: Option<u64>,
    rename_focus: bool,
    recording: Option<Recording>,
    /// Enregistrement à démarrer / arrêter : (marqueur, platine de départ).
    pending_record: Option<(usize, usize)>,
    /// Déclenchements manuels à exécuter à la prochaine image.
    pending_fire: Vec<Fire>,
    /// Éditeur de nœuds de la transition planifiée.
    nodes: NodeEditor,
    /// Modifications enregistrées à écrire sur le disque.
    unsaved: bool,
    last_save: f64,
}

impl TransitionsWindow {
    pub fn new(store: TransitionStore) -> Self {
        Self {
            store,
            from_deck: 0,
            plan: None,
            confirm_delete: None,
            renaming: None,
            rename_focus: false,
            recording: None,
            pending_record: None,
            pending_fire: Vec::new(),
            nodes: NodeEditor::default(),
            unsaved: false,
            last_save: 0.0,
        }
    }

    /// Déclencheur manuel `n` (0 à 3) de la transition planifiée (touches F7 à F10).
    pub fn fire_manual(&mut self, n: usize) {
        if self.plan.is_some() && self.recording.is_none() && n < NUM_MANUAL {
            self.pending_fire.push(Fire::Group(MANUAL_FIRST + n));
        }
    }

    /// Vrai si l'éditeur de nœuds prend le clavier (raccourcis des platines coupés).
    pub fn captures_keyboard(&self, ctx: &Context) -> bool {
        self.nodes.captures_keyboard(ctx)
    }

    /// Vrai si un enregistrement d'étapes est en cours.
    pub fn is_recording(&self) -> bool {
        self.recording.is_some()
    }

    /// SYNC ou MATCH demandé sur une platine : enregistré comme étape si un
    /// enregistrement est en cours.
    pub fn record_sync(&mut self, deck: usize, time: f64) {
        self.record_change(RecKey::Sync(deck), time);
    }

    pub fn record_match(&mut self, deck: usize, time: f64) {
        self.record_change(RecKey::Match(deck), time);
    }

    fn record_change(&mut self, key: RecKey, time: f64) {
        let Some(recording) = &self.recording else { return };
        let side = recording.side(match key {
            RecKey::Sync(deck) | RecKey::Match(deck) => deck,
            _ => return,
        });
        let action = match key {
            RecKey::Sync(_) => Action::Sync { side },
            _ => Action::Match { side },
        };
        self.apply_recorded(key, Some(action), time);
    }

    /// Ajoute, met à jour ou retire (`None` : retour à la valeur de départ)
    /// l'étape enregistrée pour `key`.
    fn apply_recorded(&mut self, key: RecKey, action: Option<Action>, time: f64) {
        let (Some(recording), Some(plan)) = (&mut self.recording, &mut self.plan) else {
            return;
        };
        let Some(transition) = self.store.get_mut(recording.transition) else {
            return;
        };
        let existing = recording.steps.iter().position(|(k, _, _)| *k == key);
        match (action, existing) {
            (None, Some(entry)) => {
                let index = recording.steps[entry].1;
                if index < transition.steps.len() {
                    transition.steps.remove(index);
                    plan.states.remove(index.min(plan.states.len().saturating_sub(1)));
                }
                recording.forget_step(index);
            }
            (None, None) => return,
            (Some(mut action), Some(entry)) => {
                let (_, index, since) = recording.steps[entry];
                match &mut action {
                    Action::Fade { secs, .. } | Action::Crossfader { secs, .. } | Action::Filter { secs, .. } => {
                        *secs = gesture_secs(time - since)
                    }
                    _ => {}
                }
                if let Some(step) = transition.steps.get_mut(index) {
                    step.action = action;
                }
            }
            (Some(action), None) => {
                transition.steps.push(Step::new(recording.marker, action));
                plan.states.resize(transition.steps.len(), StepState::Pending);
                recording.steps.push((key, transition.steps.len() - 1, time));
            }
        }
        self.unsaved = true;
    }

    /// Compare l'état des platines à l'image précédente et enregistre les
    /// réglages modifiés.
    fn record(&mut self, decks: &[DeckUi; NUM_DECKS], mixer: &MixerUi, engine: &EngineHandle, time: f64) {
        let Some(recording) = &self.recording else { return };
        let valid = self
            .plan
            .as_ref()
            .is_some_and(|plan| plan.id == recording.transition && !plan.active)
            && self.store.get(recording.transition).is_some();
        if !valid {
            self.recording = None;
            return;
        }
        let current = Snapshot::take(decks, mixer, engine);
        if current == recording.previous {
            return;
        }
        let (original, previous) = (recording.original, recording.previous);
        let mut changes: Vec<(RecKey, Option<Action>)> = Vec::new();
        for deck in 0..NUM_DECKS {
            let side = recording.side(deck);
            let (o, p, c) = (original.decks[deck], previous.decks[deck], current.decks[deck]);
            let fade = |control, db| Action::Fade {
                side,
                control,
                db,
                secs: 0.0,
                from: None,
            };
            let mute = |control, on: bool| {
                if on {
                    Action::Mute { side, control }
                } else {
                    Action::Unmute { side, control }
                }
            };
            if c.volume != p.volume {
                changes.push((
                    RecKey::Volume(deck),
                    (c.volume != o.volume).then(|| fade(Control::Volume, fader_db(c.volume))),
                ));
            }
            for (band, &control) in EQ_CONTROLS.iter().enumerate() {
                if c.eq[band] != p.eq[band] {
                    changes.push((
                        RecKey::Eq(deck, band),
                        (c.eq[band] != o.eq[band]).then(|| fade(control, knob_db(c.eq[band]))),
                    ));
                }
                if c.kill[band] != p.kill[band] {
                    changes.push((
                        RecKey::Kill(deck, band),
                        (c.kill[band] != o.kill[band]).then(|| mute(control, c.kill[band])),
                    ));
                }
            }
            for (stem, &control) in STEM_CONTROLS.iter().enumerate() {
                if c.stem_levels[stem] != p.stem_levels[stem] {
                    changes.push((
                        RecKey::Stem(deck, stem),
                        (c.stem_levels[stem] != o.stem_levels[stem])
                            .then(|| fade(control, fader_db(c.stem_levels[stem]))),
                    ));
                }
                if c.stem_mute[stem] != p.stem_mute[stem] {
                    changes.push((
                        RecKey::StemMute(deck, stem),
                        (c.stem_mute[stem] != o.stem_mute[stem]).then(|| mute(control, c.stem_mute[stem])),
                    ));
                }
            }
            if c.playing != p.playing {
                let action = if c.playing {
                    Action::Play { side }
                } else {
                    Action::Stop { side }
                };
                changes.push((RecKey::Play(deck), (c.playing != o.playing).then_some(action)));
            }
            if c.filter != p.filter {
                let action = Action::Filter {
                    side,
                    value: (c.filter * 100.0).round() / 100.0,
                    secs: 0.0,
                    from: None,
                };
                changes.push((RecKey::Filter(deck), (c.filter != o.filter).then_some(action)));
            }
        }
        if current.crossfader != previous.crossfader {
            let toward = if recording.from_deck == 0 {
                current.crossfader
            } else {
                1.0 - current.crossfader
            };
            let toward = (toward * 100.0).round() / 100.0;
            changes.push((
                RecKey::Crossfader,
                (current.crossfader != original.crossfader).then_some(Action::Crossfader {
                    toward,
                    secs: 0.0,
                    from: None,
                }),
            ));
        }
        for (key, action) in changes {
            self.apply_recorded(key, action, time);
        }
        if let Some(recording) = &mut self.recording {
            recording.previous = current;
        }
    }

    /// Démarre (ou arrête, s'il est déjà en cours sur ce marqueur)
    /// l'enregistrement d'étapes pour le marqueur `marker`.
    fn toggle_recording(&mut self, marker: usize, from_deck: usize, snapshot: Snapshot) {
        let same = self
            .recording
            .as_ref()
            .is_some_and(|recording| recording.marker == marker);
        self.recording = None;
        self.unsaved = true;
        if same {
            return;
        }
        if let Some(plan) = &self.plan {
            self.recording = Some(Recording {
                transition: plan.id,
                marker,
                from_deck,
                original: snapshot,
                previous: snapshot,
                steps: Vec::new(),
            });
        }
    }

    /// Vrai si une transition est en cours d'exécution.
    pub fn is_active(&self) -> bool {
        self.plan.as_ref().is_some_and(|plan| plan.active)
    }

    /// Vrai si quelque chose bouge : transition active, étapes programmées,
    /// fondus en cours.
    pub fn is_running(&self) -> bool {
        self.plan.as_ref().is_some_and(|plan| plan.active || plan.running())
    }

    /// Le sens ne peut pas être inversé à la main pendant l'exécution, ni
    /// quand une seule platine joue (c'est alors elle le départ).
    fn direction_locked(&self, engine: &EngineHandle) -> bool {
        self.is_running() || self.is_recording() || playing_decks(engine).len() == 1
    }

    // -----------------------------------------------------------------------
    // Exécution
    // -----------------------------------------------------------------------

    /// À appeler à chaque image : suit le sens de la transition et exécute
    /// la planification active. Retourne les messages d'état à afficher.
    pub fn tick(
        &mut self,
        decks: &mut [DeckUi; NUM_DECKS],
        mixer: &mut MixerUi,
        engine: &EngineHandle,
        time: f64,
    ) -> Vec<(String, bool)> {
        let mut messages = Vec::new();
        if let Some((marker, from)) = self.pending_record.take() {
            let snapshot = Snapshot::take(decks, mixer, engine);
            self.toggle_recording(marker, from, snapshot);
        }
        self.record(decks, mixer, engine, time);
        if self.unsaved && time - self.last_save > 0.5 {
            self.unsaved = false;
            self.last_save = time;
            if let Err(error) = self.store.save() {
                messages.push((format!("Transitions non enregistrées : {error}"), true));
            }
        }
        let playing = playing_decks(engine);
        match (&self.plan, &self.recording) {
            (Some(plan), _) if plan.active || plan.running() => self.from_deck = plan.from_deck,
            (_, Some(recording)) => self.from_deck = recording.from_deck,
            _ if playing.len() == 1 => self.from_deck = playing[0],
            _ => {}
        }

        let fires = std::mem::take(&mut self.pending_fire);
        let Some(plan) = self.plan.as_mut() else {
            return messages;
        };
        let Some(transition) = self.store.get(plan.id) else {
            self.plan = None;
            return messages;
        };
        plan.states.resize(transition.steps.len(), StepState::Pending);
        if !plan.active && !plan.running() && fires.is_empty() {
            return messages;
        }

        // Platine de départ : figée pendant l'exécution, sinon celle qui a le
        // morceau de départ (de préférence dans le sens affiché).
        let holds_from = |deck: usize| decks[deck].current_path() == Some(transition.from.as_path());
        if !plan.active && !plan.running() {
            plan.from_deck = if holds_from(self.from_deck) {
                self.from_deck
            } else {
                (0..NUM_DECKS).find(|&deck| holds_from(deck)).unwrap_or(self.from_deck)
            };
        }
        let from = plan.from_deck;
        let to = NUM_DECKS - 1 - from;
        let Some(markers) = decks[from]
            .current_triggers()
            .filter(|(path, _)| *path == transition.from)
            .map(|(_, triggers)| triggers)
        else {
            if plan.active || plan.running() {
                plan.active = false;
                plan.cancel();
                messages.push((
                    format!(
                        "Transition « {} » interrompue : le morceau de départ a changé.",
                        transition.name
                    ),
                    true,
                ));
            } else {
                messages.push((
                    "Transition : le morceau de départ n'est chargé sur aucune platine.".into(),
                    true,
                ));
            }
            return messages;
        };
        let order = transition.ordered_steps(&markers);
        let group = |trigger: usize| -> Vec<usize> {
            order
                .iter()
                .copied()
                .filter(|&index| transition.steps[index].marker == trigger)
                .collect()
        };
        let mut reached: Vec<usize> = Vec::new();
        let clock = BeatClock::of(&decks[from], engine, from);

        // Déclenchements manuels (bouton ▶ ou touches F7 à F10).
        for fire in fires {
            match fire {
                Fire::Group(trigger) => {
                    let steps = group(trigger);
                    if steps.is_empty() {
                        messages.push((format!("Transition : rien au {}.", trigger_long_name(trigger)), true));
                    } else {
                        plan.fired_at[trigger] = Some(time);
                        plan.schedule(transition, &steps, time, &clock);
                        reached.push(trigger);
                    }
                }
            }
        }

        // Marqueurs franchis par la lecture du départ (transition active).
        if plan.active {
            let position = position_secs(engine, from);
            let at = |marker: usize| markers.get(marker).copied().flatten();
            // À l'activation, les marqueurs déjà dépassés sont ignorés (et
            // réarmés si l'on revient avant eux).
            if plan.last_position.is_none() {
                for (step, state) in transition.steps.iter().zip(plan.states.iter_mut()) {
                    if *state == StepState::Pending && at(step.marker).is_some_and(|marker| marker < position) {
                        *state = StepState::Skipped;
                    }
                }
            }
            let last = plan.last_position.unwrap_or(position);
            let mut crossed = Vec::new();
            for &index in &order {
                let step = transition.steps[index];
                let Some(marker) = at(step.marker) else { continue };
                let state = &mut plan.states[index];
                match *state {
                    // Un marqueur sauté se réarme si l'on revient avant lui.
                    StepState::Skipped if position < marker => *state = StepState::Pending,
                    StepState::Pending => match crossing(last, position, marker) {
                        Crossing::Reached if !crossed.contains(&step.marker) => crossed.push(step.marker),
                        Crossing::Skipped => *state = StepState::Skipped,
                        _ => {}
                    },
                    _ => {}
                }
            }
            plan.last_position = Some(position);
            for trigger in crossed {
                let pending: Vec<usize> = group(trigger)
                    .into_iter()
                    .filter(|&index| plan.states[index] == StepState::Pending)
                    .collect();
                plan.fired_at[trigger] = Some(time);
                plan.schedule(transition, &pending, time, &clock);
                reached.push(trigger);
            }
        }

        // Étapes dont le délai est écoulé, dans l'ordre de leur échéance.
        let mut due: Vec<(usize, f64)> = plan.queue.iter().copied().filter(|(_, at)| *at <= time).collect();
        plan.queue.retain(|(_, at)| *at > time);
        due.sort_by(|a, b| a.1.total_cmp(&b.1));
        let mut done = Vec::new();
        for (index, _) in due {
            plan.states[index] = StepState::Done;
            let action = transition.steps[index].action;
            match execute(action, from, to, decks, mixer, engine, time, &mut plan.ramps) {
                Ok(()) => done.push(action.describe()),
                Err(error) => messages.push((format!("Transition : {error}"), true)),
            }
        }
        if !done.is_empty() || !reached.is_empty() {
            let names: Vec<String> = reached.into_iter().map(trigger_long_name).collect();
            let text = match (names.is_empty(), done.is_empty()) {
                (false, false) => format!("Transition, {} : {}.", names.join(", "), done.join(", ")),
                (false, true) => format!("Transition : {} déclenché.", names.join(", ")),
                _ => format!("Transition : {}.", done.join(", ")),
            };
            messages.push((text, false));
        }

        // Fondus.
        plan.ramps.retain(|ramp| {
            let t = if ramp.secs <= 0.0 {
                1.0
            } else {
                ((time - ramp.since) as f32 / ramp.secs).clamp(0.0, 1.0)
            };
            ramp.target.set(decks, mixer, ramp.start + (ramp.end - ramp.start) * t);
            t < 1.0
        });

        // Terminée quand plus rien ne peut se déclencher : les étapes dont le
        // marqueur n'est pas posé ne bloquent pas la fin, les déclencheurs
        // manuels pas encore utilisés si.
        let waiting = transition.steps.iter().zip(&plan.states).any(|(step, state)| {
            *state == StepState::Pending
                && (is_manual(step.marker) || markers.get(step.marker).copied().flatten().is_some())
        });
        if plan.active && !plan.running() && !waiting {
            plan.active = false;
            messages.push((format!("Transition « {} » terminée.", transition.name), false));
        }
        messages
    }

    // -----------------------------------------------------------------------
    // Panneau
    // -----------------------------------------------------------------------

    /// Panneau des transitions (ancré ou dans une fenêtre) : l'éditeur de
    /// nœuds de la transition chargée et, à sa droite, la liste des
    /// transitions entre les morceaux des deux platines.
    pub fn ui(&mut self, ui: &mut Ui, decks: &[DeckUi; NUM_DECKS], engine: &EngineHandle) -> TransitionsOutcome {
        let mut outcome = TransitionsOutcome::default();
        let full = ui.available_rect_before_wrap();
        let side = Rect::from_min_max(pos2(full.right() - SIDEBAR_WIDTH, full.top()), full.max);
        let main = Rect::from_min_max(full.min, pos2(side.left() - 10.0, full.bottom()));
        ui.scope_builder(UiBuilder::new().max_rect(main), |ui| self.editor(ui, decks));
        ui.painter()
            .vline(side.left() - 5.0, full.y_range(), Stroke::new(1.0, theme::OUTLINE));
        ui.scope_builder(UiBuilder::new().max_rect(side), |ui| {
            self.sidebar(ui, decks, engine, &mut outcome)
        });
        ui.advance_cursor_after_rect(full);
        outcome
    }

    /// Éditeur de nœuds de la transition chargée.
    fn editor(&mut self, ui: &mut Ui, decks: &[DeckUi; NUM_DECKS]) {
        let ctx = ui.ctx().clone();
        let Some(plan) = &mut self.plan else {
            let rect = ui.available_rect_before_wrap();
            ui.painter().text(
                rect.center(),
                Align2::CENTER_CENTER,
                "Aucune transition chargée",
                FontId::proportional(14.0),
                theme::TEXT_DIM,
            );
            ui.advance_cursor_after_rect(rect);
            return;
        };
        let ctx = &ctx;
        let Some(transition) = self.store.get_mut(plan.id) else {
            self.plan = None;
            return;
        };
        plan.states.resize(transition.steps.len(), StepState::Pending);
        let holds = |deck: usize, path: &Path| decks[deck].current_path() == Some(path);
        let from = if plan.active || plan.running() {
            plan.from_deck
        } else if holds(self.from_deck, &transition.from) {
            self.from_deck
        } else {
            (0..NUM_DECKS)
                .find(|&deck| holds(deck, &transition.from))
                .unwrap_or(self.from_deck)
        };
        let to = NUM_DECKS - 1 - from;
        let from_ready = holds(from, &transition.from);
        let to_ready = holds(to, &transition.to);
        let from_markers = decks[from]
            .current_triggers()
            .filter(|_| from_ready)
            .map_or([None; NUM_TRIGGERS], |(_, triggers)| triggers);
        let now = ctx.input(|input| input.time);
        let statuses: Vec<StepStatus> = plan
            .states
            .iter()
            .enumerate()
            .map(|(index, state)| match state {
                StepState::Pending => StepStatus::Pending,
                StepState::Done => StepStatus::Done,
                StepState::Skipped => StepStatus::Skipped,
                StepState::Scheduled => StepStatus::Scheduled(
                    plan.queue
                        .iter()
                        .find(|(step, _)| *step == index)
                        .map_or(0.0, |(_, at)| (at - now).max(0.0)),
                ),
            })
            .collect();
        let recording = self
            .recording
            .as_ref()
            .filter(|recording| recording.transition == transition.id)
            .map(|recording| recording.marker);
        let view = RunView {
            from,
            to,
            from_markers,
            statuses: &statuses,
            fired_at: &plan.fired_at,
            now,
            can_fire: from_ready && to_ready && recording.is_none(),
            can_activate: plan.active || (from_ready && to_ready),
            active: plan.active,
            keys: [decks[0].current_key(), decks[1].current_key()],
            recording,
            // Pas d'enregistrement pendant une exécution : il capterait les
            // réglages modifiés par les étapes.
            can_record: from_ready && !plan.active && !plan.running(),
        };
        let result = self.nodes.ui(ui, transition, &view);
        drop(statuses);

        for trigger in result.fire {
            self.pending_fire.push(Fire::Group(trigger));
        }
        if let Some(marker) = result.record {
            // Démarré à la prochaine image (il faut l'état du crossfader).
            self.pending_record = Some((marker, from));
        }
        if result.steps_changed {
            // Les indices des étapes ont changé : états remis à zéro (sauf en
            // pleine exécution, où l'on garde ce qui peut l'être).
            if plan.active || plan.running() {
                plan.states.resize(transition.steps.len(), StepState::Pending);
                plan.queue.retain(|(index, _)| *index < transition.steps.len());
            } else {
                plan.states = vec![StepState::Pending; transition.steps.len()];
            }
            if recording.is_none() {
                self.recording = None;
            }
        }
        if result.changed {
            self.unsaved = true;
        }
        if result.toggle_active {
            if plan.active {
                plan.active = false;
                plan.cancel();
            } else if from_ready && to_ready {
                if plan.states.iter().all(|state| *state != StepState::Pending) {
                    plan.reset();
                }
                self.recording = None;
                plan.active = true;
                plan.from_deck = from;
                plan.last_position = None;
                self.from_deck = from;
            }
        }
    }

    /// Colonne de droite : sens, création, transitions disponibles.
    fn sidebar(
        &mut self,
        ui: &mut Ui,
        decks: &[DeckUi; NUM_DECKS],
        engine: &EngineHandle,
        outcome: &mut TransitionsOutcome,
    ) {
        let from = self.from_deck;
        let to = NUM_DECKS - 1 - from;
        let locked = self.direction_locked(engine);

        // Sens (clic : inverser) et création d'une transition dans ce sens.
        ui.horizontal(|ui| {
            let sense = if locked { Sense::hover() } else { Sense::click() };
            let (rect, response) = ui.allocate_exact_size(vec2(64.0, 24.0), sense);
            if response.hovered() && !locked {
                ui.painter().rect_filled(rect, 5.0, theme::PANEL_LIGHT);
            }
            icons::paint_direction(ui.painter(), rect.shrink2(vec2(6.0, 0.0)), from, 16.0, 1.0);
            let title = |deck: usize| {
                decks[deck]
                    .current_title()
                    .map_or("—".to_string(), |t| ellipsize(t, 40))
            };
            let hint = format!(
                "{} : {}\n{} : {}{}",
                theme::DECK_NAMES[from],
                title(from),
                theme::DECK_NAMES[to],
                title(to),
                if locked { "" } else { "\nClic : inverser le sens" }
            );
            if response.on_hover_text(hint).clicked() && !locked {
                self.from_deck = to;
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let from_path = decks[from].current_path().map(Path::to_path_buf);
                let to_path = decks[to].current_path().map(Path::to_path_buf);
                let can_create = from_path.is_some() && to_path.is_some() && !self.is_active();
                let hint = format!(
                    "Nouvelle transition {} → {}",
                    theme::DECK_NAMES[from],
                    theme::DECK_NAMES[to]
                );
                let response = icons::icon_button(ui, Icon::Plus, theme::TEXT_DIM, false, can_create, &hint);
                let (Some(from_path), Some(to_path)) = (from_path, to_path) else {
                    return;
                };
                Popup::menu(&response).show(|ui| {
                    let templates = templates();
                    let mut chosen = None;
                    if ui.button("Vide").clicked() {
                        chosen = Some((String::from("Transition"), Vec::new()));
                    }
                    ui.separator();
                    for template in templates {
                        if ui.button(template.name).on_hover_text(template.description).clicked() {
                            chosen = Some((template.name.to_string(), template.steps));
                        }
                    }
                    if let Some((name, steps)) = chosen {
                        let count = steps.len();
                        let id = self.store.create(Transition {
                            id: 0,
                            name,
                            from: from_path.clone(),
                            to: to_path.clone(),
                            from_title: decks[from].current_title().unwrap_or_default().to_string(),
                            to_title: decks[to].current_title().unwrap_or_default().to_string(),
                            steps,
                            graph: None,
                        });
                        self.recording = None;
                        self.plan = Some(Plan::new(id, count));
                        self.unsaved = true;
                    }
                });
            });
        });
        ui.add_space(4.0);

        // Transitions qui partent du morceau de chaque platine (sens affiché
        // d'abord) ; celles dont la destination est sur l'autre platine en tête.
        let mut rows: Vec<Row> = Vec::new();
        for deck in [from, to] {
            let Some(path) = decks[deck].current_path() else {
                continue;
            };
            let other = decks[NUM_DECKS - 1 - deck].current_path();
            for transition in self.store.starting_from(path) {
                if rows.iter().any(|row| row.id == transition.id) {
                    continue;
                }
                rows.push(Row {
                    id: transition.id,
                    from_deck: deck,
                    name: transition.name.clone(),
                    from_title: display_title(&transition.from_title, &transition.from),
                    to_title: display_title(&transition.to_title, &transition.to),
                    to_path: transition.to.clone(),
                    steps: transition.steps.len(),
                    ready: other == Some(transition.to.as_path()),
                });
            }
        }
        rows.sort_by_key(|row| !row.ready);

        if rows.is_empty() {
            let text = if decks.iter().all(|deck| deck.current_path().is_none()) {
                "Aucun morceau chargé"
            } else {
                "Aucune transition"
            };
            ui.add_space(8.0);
            ui.vertical_centered(|ui| ui.label(RichText::new(text).color(theme::TEXT_DIM).size(12.0)));
            return;
        }
        ScrollArea::vertical()
            .id_salt("transition_rows")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                let mut separated = false;
                for row in &rows {
                    if !row.ready && !separated {
                        separated = true;
                        if rows.iter().any(|row| row.ready) {
                            ui.add_space(4.0);
                            ui.separator();
                            ui.add_space(2.0);
                        }
                    }
                    self.row(ui, row, engine, outcome);
                }
            });
    }

    /// Ligne d'une transition : sens, nom ; clic : la charger dans l'éditeur.
    fn row(&mut self, ui: &mut Ui, row: &Row, engine: &EngineHandle, outcome: &mut TransitionsOutcome) {
        let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::click());
        let selected = self.plan.as_ref().is_some_and(|plan| plan.id == row.id);
        let active = selected && self.is_running();
        let recording = self
            .recording
            .as_ref()
            .is_some_and(|recording| recording.transition == row.id);
        let painter = ui.painter().clone();
        if selected {
            painter.rect_filled(rect, 4.0, theme::PANEL_LIGHT);
            painter.rect_filled(
                Rect::from_min_size(rect.left_top() + vec2(0.0, 5.0), vec2(2.5, rect.height() - 10.0)),
                1.0,
                theme::DECK_COLORS[row.from_deck],
            );
        } else if response.hovered() {
            painter.rect_filled(rect, 4.0, theme::PANEL_LIGHT.gamma_multiply(0.55));
        }
        let strength = if row.ready { 1.0 } else { 0.5 };
        let badge = Rect::from_min_size(rect.left_top() + vec2(6.0, 0.0), vec2(36.0, rect.height()));
        icons::paint_direction(&painter, badge, row.from_deck, 11.0, strength);

        // Suppression en attente de confirmation.
        if self.confirm_delete == Some(row.id) {
            painter.text(
                pos2(badge.right() + 8.0, rect.center().y),
                Align2::LEFT_CENTER,
                "Supprimer ?",
                FontId::proportional(13.0),
                theme::RED,
            );
            let mut buttons = ui.new_child(
                UiBuilder::new()
                    .max_rect(rect.shrink2(vec2(2.0, 1.0)))
                    .layout(Layout::right_to_left(Align::Center)),
            );
            if icons::icon_button(&mut buttons, Icon::Close, theme::TEXT_DIM, false, true, "Annuler").clicked()
                || ui.input(|input| input.key_pressed(egui::Key::Escape))
            {
                self.confirm_delete = None;
            }
            if icons::icon_button(&mut buttons, Icon::Check, theme::RED, false, true, "Supprimer").clicked() {
                self.store.remove(row.id);
                if selected {
                    self.plan = None;
                    self.recording = None;
                }
                self.confirm_delete = None;
                self.unsaved = true;
            }
            return;
        }

        // Boutons à droite : charger la destination, état.
        let mut right = rect.right() - 4.0;
        if !row.ready {
            let deck = NUM_DECKS - 1 - row.from_deck;
            let busy = engine.shared.decks[deck].playing.load(Relaxed);
            let mut slot = ui.new_child(
                UiBuilder::new()
                    .max_rect(Rect::from_min_max(
                        pos2(right - 26.0, rect.top()),
                        pos2(right, rect.bottom()),
                    ))
                    .layout(Layout::right_to_left(Align::Center)),
            );
            let hint = if busy {
                format!(
                    "{} joue : arrêtez-la pour y charger la destination",
                    theme::DECK_NAMES[deck]
                )
            } else {
                format!(
                    "Charger « {} » sur {}",
                    ellipsize(&row.to_title, 40),
                    theme::DECK_NAMES[deck]
                )
            };
            if icons::icon_button(&mut slot, Icon::Load, theme::TEXT_DIM, false, !busy, &hint).clicked() {
                outcome.load = Some((deck, row.to_path.clone()));
            }
            right -= 28.0;
        }
        if active || recording {
            painter.circle_filled(
                pos2(right - 5.0, rect.center().y),
                4.0,
                if recording { theme::RED } else { theme::GREEN },
            );
            right -= 14.0;
        }

        // Nom (ou champ de saisie pendant le renommage).
        let name_rect = Rect::from_min_max(pos2(badge.right() + 8.0, rect.top()), pos2(right - 4.0, rect.bottom()));
        if self.renaming == Some(row.id) {
            let Some(transition) = self.store.get_mut(row.id) else {
                return;
            };
            let edit = ui.put(
                name_rect.shrink2(vec2(0.0, 2.0)),
                TextEdit::singleline(&mut transition.name).font(FontId::proportional(13.0)),
            );
            if !edit.has_focus() && self.rename_focus {
                edit.request_focus();
                self.rename_focus = false;
            } else if edit.lost_focus() {
                self.renaming = None;
            }
            if edit.changed() {
                self.unsaved = true;
            }
            return;
        }
        let color = if row.ready { theme::TEXT } else { theme::TEXT_DIM };
        let mut job = egui::text::LayoutJob::single_section(
            row.name.clone(),
            egui::TextFormat::simple(FontId::proportional(13.0), color),
        );
        job.wrap = egui::text::TextWrapping::truncate_at_width(name_rect.width().max(10.0));
        let galley = painter.layout_job(job);
        painter.galley(
            pos2(name_rect.left(), rect.center().y - galley.size().y / 2.0),
            galley,
            color,
        );

        let response = response.on_hover_text(format!(
            "{}\n{} → {}\n{} étape(s)\nDouble-clic : renommer · clic droit : options",
            row.name, row.from_title, row.to_title, row.steps
        ));
        if response.double_clicked() {
            self.renaming = Some(row.id);
            self.rename_focus = true;
        } else if response.clicked() && !selected && !self.is_running() {
            self.recording = None;
            self.plan = Some(Plan::new(row.id, row.steps));
        }
        response.context_menu(|ui| {
            if ui.button("Renommer").clicked() {
                self.renaming = Some(row.id);
                self.rename_focus = true;
            }
            if ui.button(RichText::new("Supprimer").color(theme::RED)).clicked() {
                self.confirm_delete = Some(row.id);
            }
        });
    }
}

/// Largeur de la colonne des transitions.
const SIDEBAR_WIDTH: f32 = 224.0;

/// Transition affichée dans la colonne de droite.
struct Row {
    id: u64,
    /// Platine qui a le morceau de départ.
    from_deck: usize,
    name: String,
    from_title: String,
    to_title: String,
    to_path: PathBuf,
    steps: usize,
    /// La destination est chargée sur l'autre platine.
    ready: bool,
}

/// Bouton « lecture » : triangle dessiné (le glyphe ▶ des polices par défaut
/// s'affiche en emoji), suivi d'un texte éventuel ; plein si `filled`.
pub(super) fn play_button(
    ui: &mut Ui,
    label: Option<&str>,
    color: Color32,
    filled: bool,
    enabled: bool,
) -> egui::Response {
    let font = FontId::proportional(12.0);
    let text_width = label.map_or(0.0, |label| {
        ui.painter()
            .layout_no_wrap(label.into(), font.clone(), Color32::WHITE)
            .size()
            .x
            + 6.0
    });
    let size = vec2(22.0 + text_width, 18.0);
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, response) = ui.allocate_exact_size(size, sense);
    let hovered = enabled && response.hovered();
    let (fill, ink) = match (filled, enabled) {
        (true, true) => (color, Color32::BLACK),
        (_, false) => (theme::PANEL_LIGHT, theme::TEXT_DIM.gamma_multiply(0.6)),
        (false, true) => (
            if hovered {
                theme::PANEL_LIGHT
            } else {
                Color32::from_rgb(0x1b, 0x1e, 0x25)
            },
            color,
        ),
    };
    let painter = ui.painter();
    painter.rect_filled(
        rect,
        4.0,
        if filled && hovered {
            color.gamma_multiply(1.2)
        } else {
            fill
        },
    );
    painter.rect_stroke(rect, 4.0, Stroke::new(1.0, theme::OUTLINE), egui::StrokeKind::Inside);
    let c = pos2(rect.left() + 11.0, rect.center().y);
    painter.add(egui::Shape::convex_polygon(
        vec![
            pos2(c.x - 4.0, c.y - 5.0),
            pos2(c.x + 5.0, c.y),
            pos2(c.x - 4.0, c.y + 5.0),
        ],
        ink,
        Stroke::NONE,
    ));
    if let Some(label) = label {
        painter.text(pos2(rect.left() + 20.0, c.y), Align2::LEFT_CENTER, label, font, ink);
    }
    response
}

/// Exécute une action ; `from` / `to` : platines de départ et de destination.
#[allow(clippy::too_many_arguments)]
fn execute(
    action: Action,
    from: usize,
    to: usize,
    decks: &mut [DeckUi; NUM_DECKS],
    mixer: &mut MixerUi,
    engine: &EngineHandle,
    time: f64,
    ramps: &mut Vec<Ramp>,
) -> Result<(), String> {
    let deck_of = |side: Side| if side == Side::From { from } else { to };
    // Fondu de `target` vers `end` en `secs`, depuis `from` s'il est donné
    // (posé d'abord), sinon depuis la valeur actuelle.
    let mut start_ramp = |target: RampTarget,
                          from: Option<f32>,
                          end: f32,
                          secs: f32,
                          decks: &mut [DeckUi; NUM_DECKS],
                          mixer: &mut MixerUi| {
        ramps.retain(|ramp| ramp.target != target);
        if let Some(from) = from {
            target.set(decks, mixer, from);
        }
        ramps.push(Ramp {
            target,
            start: target.get(decks, mixer),
            end,
            since: time,
            secs,
        });
    };
    match action {
        Action::Play { side } => {
            let deck = deck_of(side);
            let shared = &engine.shared.decks[deck];
            if decks[deck].current_path().is_none() {
                return Err(format!("platine {} vide", theme::DECK_NAMES[deck]));
            }
            if !shared.playing.load(Relaxed) && !shared.armed.load(Relaxed) {
                engine.send(Command::TogglePlay(deck));
            }
        }
        Action::Stop { side } => {
            let deck = deck_of(side);
            let shared = &engine.shared.decks[deck];
            if shared.playing.load(Relaxed) || shared.armed.load(Relaxed) {
                engine.send(Command::TogglePlay(deck));
            }
        }
        Action::Fade {
            side,
            control,
            db,
            secs,
            from,
        } => {
            let deck = deck_of(side);
            let target = match (control.eq_band(), control.stem()) {
                (Some(band), _) => {
                    decks[deck].kill[band] = false;
                    RampTarget::Eq(deck, band)
                }
                (_, Some(stem)) => {
                    decks[deck].stem_mute[stem] = false;
                    RampTarget::Stem(deck, stem)
                }
                _ => RampTarget::Volume(deck),
            };
            let start = from.map(|db| control.position_for_db(db));
            start_ramp(target, start, control.position_for_db(db), secs, decks, mixer);
            check_stems(&decks[deck], deck, control)?;
        }
        Action::Mute { side, control } | Action::Unmute { side, control } => {
            let deck = deck_of(side);
            let mute = matches!(action, Action::Mute { .. });
            match (control.eq_band(), control.stem()) {
                (Some(band), _) => decks[deck].kill[band] = mute,
                (_, Some(stem)) => decks[deck].stem_mute[stem] = mute,
                _ => {
                    ramps.retain(|ramp| ramp.target != RampTarget::Volume(deck));
                    decks[deck].volume = if mute { 0.0 } else { 1.0 };
                }
            }
            check_stems(&decks[deck], deck, control)?;
        }
        Action::Crossfader {
            toward,
            secs,
            from: start,
        } => {
            let position = |toward: f32| (if from == 0 { toward } else { 1.0 - toward }).clamp(0.0, 1.0);
            start_ramp(
                RampTarget::Crossfader,
                start.map(position),
                position(toward),
                secs,
                decks,
                mixer,
            );
        }
        Action::Tempo {
            side,
            percent,
            secs,
            from,
        } => {
            let deck = deck_of(side);
            decks[deck].sync = false;
            let tempo = |percent: f32| 1.0 + percent / 100.0;
            start_ramp(
                RampTarget::Tempo(deck),
                from.map(tempo),
                tempo(percent),
                secs,
                decks,
                mixer,
            );
        }
        Action::Filter {
            side,
            value,
            secs,
            from,
        } => {
            let deck = deck_of(side);
            if !decks[deck].filter_on {
                // Filtre éteint : il part du neutre.
                decks[deck].filter = 0.5;
                decks[deck].filter_on = true;
            }
            let knob = |value: f32| (value.clamp(-1.0, 1.0) + 1.0) / 2.0;
            start_ramp(
                RampTarget::Filter(deck),
                from.map(knob),
                knob(value),
                secs,
                decks,
                mixer,
            );
        }
        Action::Loop { side, beats } => {
            let deck = deck_of(side);
            if beats.is_some() && decks[deck].beat_grid().is_none() {
                return Err(format!(
                    "pas de tempo détecté sur la platine {} : boucle impossible",
                    theme::DECK_NAMES[deck]
                ));
            }
            engine.send(Command::SetLoop { deck, beats });
        }
        Action::Key { side, semitones } => decks[deck_of(side)].transpose(semitones),
        Action::Sync { side } => decks[deck_of(side)].request_sync_once(),
        Action::Match { side } => decks[deck_of(side)].request_match_once(),
        Action::Jump { side, marker } => {
            let deck = deck_of(side);
            if !decks[deck].jump_to_marker(marker, engine) {
                return Err(format!(
                    "{} non posé sur la platine {}",
                    trigger_long_name(marker),
                    theme::DECK_NAMES[deck]
                ));
            }
        }
    }
    Ok(())
}

/// Une action sur un stem n'a d'effet qu'une fois le morceau séparé : le
/// réglage est appliqué quand même (il comptera dès la fin de la séparation),
/// mais l'utilisateur est prévenu.
fn check_stems(deck_ui: &DeckUi, deck: usize, control: Control) -> Result<(), String> {
    if control.stem().is_some() && !deck_ui.stems_ready() {
        return Err(format!(
            "{} : morceau de la platine {} pas encore séparé (bouton SÉPARER)",
            control.label(),
            theme::DECK_NAMES[deck]
        ));
    }
    Ok(())
}

fn playing_decks(engine: &EngineHandle) -> Vec<usize> {
    (0..NUM_DECKS)
        .filter(|&deck| engine.shared.decks[deck].playing.load(Relaxed))
        .collect()
}

fn position_secs(engine: &EngineHandle, deck: usize) -> f64 {
    engine.shared.decks[deck].position.load(Relaxed) as f64 / engine.shared.sample_rate as f64
}

/// Titre enregistré, ou nom du fichier à défaut.
fn display_title(title: &str, path: &Path) -> String {
    if title.is_empty() {
        path.file_stem()
            .map_or_else(String::new, |stem| stem.to_string_lossy().into_owned())
    } else {
        title.to_string()
    }
}

/// Première lettre en majuscule.
pub(super) fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map_or_else(String::new, |first| first.to_uppercase().chain(chars).collect())
}

/// Coupe un texte trop long avec « … ».
fn ellipsize(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let cut: String = text.chars().take(max.saturating_sub(1)).collect();
        format!("{cut}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn beat_clock_resolves_beats_and_quantize() {
        // 120 BPM (un temps = 0,5 s), lecture à 1,1 s, tempo d'origine.
        let clock = BeatClock {
            position: 1.1,
            rate: 1.0,
            playing: true,
            grid: Some((0.0, 0.5)),
        };
        let timing = |ops: Vec<TimeOp>| Timing::from(ops);
        let close = |a: f64, b: f64| (a - b).abs() < 1e-9;
        assert!(close(
            clock.resolve(&timing(vec![TimeOp::Beats { beats: 2.0 }]), 10.0),
            11.0
        ));
        // Prochaine mesure (4 temps = 2 s) : dans 0,9 s.
        assert!(close(
            clock.resolve(&timing(vec![TimeOp::Quantize { beats: 4 }]), 10.0),
            10.9
        ));
        // Quantifier après une attente : depuis la position atteinte.
        let ops = vec![TimeOp::Secs { secs: 1.0 }, TimeOp::Quantize { beats: 1 }];
        assert!(close(clock.resolve(&timing(ops), 0.0), 1.4));
        // Tempo +25 % : les temps raccourcissent.
        let fast = BeatClock { rate: 1.25, ..clock };
        assert!(close(
            fast.resolve(&timing(vec![TimeOp::Beats { beats: 5.0 }]), 0.0),
            2.0
        ));
        // Départ à l'arrêt : pas d'attente de quantification.
        let stopped = BeatClock {
            playing: false,
            ..clock
        };
        assert!(close(
            stopped.resolve(&timing(vec![TimeOp::Quantize { beats: 4 }]), 3.0),
            3.0
        ));
    }
}
