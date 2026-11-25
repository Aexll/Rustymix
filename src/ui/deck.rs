//! Interface d'une platine : en-tête, formes d'onde, transport, tempo et
//! tonalité, EQ, volume, vu-mètre.

use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;

use egui::{Align2, Color32, FontId, Id, Rect, Sense, Stroke, StrokeKind, Ui, pos2, vec2};

use super::theme;
use super::waveform_view::paint_waveform;
use super::widgets::{
    Meter, Orientation, TransportIcon, clipped_text, fader, knob, take_bottom, take_left, take_right, take_top,
    toggle_button, transport_button, vu_meter,
};
use crate::analysis::{Analysis, Harmony, Key};
use crate::audio::dsp::{fader_to_gain, gain_to_db, knob_to_gain};
use crate::audio::engine::{Command, EngineHandle, NUM_STEMS, TEMPO_RANGE};
use crate::library::TrackInfo;
use crate::markers::{MARKER_END, MARKER_START, Markers, NUM_MARKERS, NUM_TRIGGERS, Triggers};
use crate::waveform::Waveform;

/// Demi-fenêtre de la vue zoomée (secondes de part et d'autre de la tête de lecture).
const ZOOM_DEFAULT_SECS: f32 = 4.0;
const ZOOM_RANGE_SECS: std::ops::RangeInclusive<f32> = 0.5..=16.0;
/// La forme d'onde clignote quand il reste moins que ceci, en secondes.
const END_WARNING_SECS: f64 = 30.0;
/// Voir `DeckUi::seek_hold`.
const SEEK_HOLD_SECS: f32 = 0.06;
/// Course du fader de tempo : ±12 %.
const TEMPO_FADER_RANGE: f32 = 0.12;
/// Écart minimal entre deux barres de temps pour les dessiner, en pixels.
const MIN_BEAT_SPACING_PX: f64 = 5.0;
/// Attente avant de caler la position sur un temps après une recherche au
/// clavier (le moteur doit d'abord avoir traité les derniers déplacements).
const SNAP_DELAY_SECS: f32 = 0.1;
/// Durée d'appui sur SYNC / MATCH qui active / coupe le mode permanent.
const HOLD_SECS: f64 = 0.4;
/// Course du slider de tonalité : ±6 demi-tons (toutes les tonalités).
const KEY_RANGE: i32 = 6;

/// Libellés et bulles d'aide des stems, dans l'ordre du moteur.
const STEMS: [(&str, &str); NUM_STEMS] = [
    ("VOIX", "Voix"),
    ("BATT", "Batterie"),
    ("BASSE", "Basse"),
    ("INSTR", "Instruments (tout le reste : synthés, guitares, claviers…)"),
];

/// Libellés des potentiomètres d'EQ dans l'ordre d'affichage, avec l'indice de
/// bande côté moteur (`[low, mid, high]`).
const EQ_KNOBS: [(&str, usize); 3] = [("HIGH", 2), ("MID", 1), ("LOW", 0)];

/// Morceau chargé (ou en cours de prise en compte par le moteur).
pub struct LoadedTrack {
    pub id: u64,
    pub info: TrackInfo,
    pub waveform: Arc<Waveform>,
    /// Tempo, grille de temps et tonalité.
    pub analysis: Analysis,
    /// Marqueurs du morceau, en secondes.
    pub markers: Markers,
    /// Début et fin de la musique (marqueurs spéciaux automatiques), en secondes.
    pub bounds: (f64, f64),
}

impl LoadedTrack {
    /// Positions des 8 marqueurs, puis du début et de la fin de la musique.
    /// La fin est ramenée juste avant le bout du fichier pour pouvoir être
    /// franchie en lecture.
    pub fn triggers(&self) -> Triggers {
        let mut triggers = [None; NUM_TRIGGERS];
        triggers[..NUM_MARKERS].copy_from_slice(&self.markers);
        let duration = self.waveform.duration_secs();
        triggers[MARKER_START] = Some(self.bounds.0);
        triggers[MARKER_END] = Some(self.bounds.1.min(duration - 0.05).max(self.bounds.0));
        triggers
    }
}

/// Ce qu'une platine expose à l'autre pour SYNC et MATCH KEY.
#[derive(Clone, Copy, Default)]
pub struct DeckLink {
    /// Tempo réellement joué (BPM du morceau × vitesse).
    pub bpm: Option<f32>,
    /// Tonalité du morceau (d'origine).
    pub key: Option<Key>,
    /// Décalage de hauteur réellement entendu, en demi-tons.
    pub detune: f32,
}

/// Séparation en stems du morceau d'une platine.
pub enum StemState {
    /// Séparation (ou lecture du cache) en cours, avancement de 0 à 1.
    Working(f32),
    Ready,
    Failed(String),
}

/// Ce que l'utilisateur vient d'activer sur une platine.
#[derive(Clone, Copy, Default)]
pub struct DeckEvents {
    pub sync_enabled: bool,
    /// Calage unique du tempo demandé (clic court sur SYNC).
    pub sync_once: bool,
    pub match_key_enabled: bool,
    /// Accord unique de la tonalité demandé (clic court sur MATCH).
    pub match_once: bool,
    /// Un marqueur a été posé ou effacé (à enregistrer).
    pub markers_changed: bool,
    /// L'utilisateur demande la séparation du morceau en stems.
    pub separate: bool,
    /// La platine vient d'être réinitialisée.
    pub reset: bool,
}

/// État d'interface d'une platine.
pub struct DeckUi {
    index: usize,
    /// Morceau actuellement dans le moteur.
    current: Option<LoadedTrack>,
    /// Morceau envoyé au moteur, en attente de confirmation.
    pub incoming: Option<LoadedTrack>,
    /// Chargement en cours en arrière-plan : (identifiant, morceau).
    pub loading: Option<(u64, TrackInfo)>,
    /// Position du fader de volume, 0..1.
    pub volume: f32,
    /// Positions des potentiomètres `[low, mid, high]`, 0..1 (0,5 = 0 dB).
    pub eq: [f32; 3],
    pub kill: [bool; 3],
    /// Vitesse de lecture relative (1 = tempo d'origine).
    pub tempo: f32,
    /// La tonalité reste la même quel que soit le tempo.
    pub key_lock: bool,
    /// Le tempo suit en permanence celui de l'autre platine.
    pub sync: bool,
    /// Calage unique du tempo sur l'autre platine, à la prochaine image.
    sync_once: bool,
    /// Appui en cours sur SYNC : (début, SYNC permanent déjà basculé).
    sync_press: Option<(f64, bool)>,
    /// La tonalité suit en permanence celle de l'autre platine.
    pub match_key: bool,
    /// Accord unique de la tonalité sur l'autre platine, à la prochaine image.
    match_once: bool,
    /// Appui en cours sur MATCH, comme `sync_press`.
    match_press: Option<(f64, bool)>,
    /// Transposition choisie au slider de tonalité, en demi-tons.
    key_offset: i32,
    /// Tonalité entendue sur l'autre platine (pour mettre en avant les notes
    /// compatibles sur le slider).
    other_key: Option<Key>,
    /// La lecture démarre calée sur les temps de l'autre platine.
    pub magnet: bool,
    /// Transposition envoyée au moteur, en demi-tons : `key_offset`, ou la
    /// valeur exacte calculée par MATCH (fractionnaire si le tempo désaccorde).
    pitch_shift: f32,
    meter: Meter,
    zoom_secs: f32,
    /// Position de lecture lissée pour l'affichage, en frames.
    display_pos: f64,
    /// Dernière position envoyée pendant un glisser sur la vue d'ensemble.
    last_seek: Option<u64>,
    /// Après un seek demandé depuis l'interface, temps (s) pendant lequel on
    /// fait confiance à la position affichée plutôt qu'à celle du moteur, qui
    /// n'a pas encore traité la commande.
    seek_hold: f32,
    /// Calage sur un temps demandé (voir [`DeckUi::request_snap`]), dans ce délai.
    snap_delay: Option<f32>,
    /// Bouton du filtre, 0..1 (0,5 : neutre ; à gauche passe-bas, à droite
    /// passe-haut), et filtre allumé.
    pub filter: f32,
    pub filter_on: bool,
    /// Position des faders de stems, 0..1 (1 = volume d'origine).
    pub stem_levels: [f32; NUM_STEMS],
    pub stem_mute: [bool; NUM_STEMS],
    /// Séparation en stems : (identifiant du morceau, état).
    stems: Option<(u64, StemState)>,
    /// Premier clic sur RÉINIT. (instant) : un second clic rapide confirme.
    reset_armed: Option<f64>,
}

impl DeckUi {
    pub fn new(index: usize) -> Self {
        Self {
            index,
            current: None,
            incoming: None,
            loading: None,
            volume: 1.0,
            eq: [0.5; 3],
            kill: [false; 3],
            tempo: 1.0,
            key_lock: false,
            sync: false,
            sync_once: false,
            sync_press: None,
            match_key: false,
            match_once: false,
            match_press: None,
            key_offset: 0,
            other_key: None,
            magnet: false,
            pitch_shift: 0.0,
            meter: Meter::default(),
            zoom_secs: ZOOM_DEFAULT_SECS,
            display_pos: 0.0,
            last_seek: None,
            seek_hold: 0.0,
            snap_delay: None,
            filter: 0.5,
            filter_on: false,
            stem_levels: [1.0; NUM_STEMS],
            stem_mute: [false; NUM_STEMS],
            stems: None,
            reset_armed: None,
        }
    }

    /// Réglage effectif du filtre, de −1 (passe-bas) à 1 (passe-haut) ; 0 si éteint.
    pub fn filter_amount(&self) -> f32 {
        if self.filter_on {
            (self.filter.clamp(0.0, 1.0) - 0.5) * 2.0
        } else {
            0.0
        }
    }

    /// Grille de temps du morceau : instant d'un temps et durée d'un temps,
    /// en secondes de morceau.
    pub fn beat_grid(&self) -> Option<(f64, f64)> {
        let analysis = self.current.as_ref()?.analysis;
        let bpm = analysis.bpm.filter(|bpm| *bpm > 0.0)?;
        Some((analysis.beat_offset as f64, 60.0 / bpm as f64))
    }

    /// Remet les réglages de la platine à leur état initial : volume, EQ et
    /// kills, filtre, tempo, tonalité, SYNC, MATCH, stems, zoom (la boucle est
    /// quittée par l'appelant). KEY LOCK et MAGNET
    /// (des préférences de jeu), le morceau, la position, le cue et les
    /// marqueurs sont conservés.
    pub fn reset_controls(&mut self) {
        let fresh = DeckUi::new(self.index);
        self.volume = fresh.volume;
        self.eq = fresh.eq;
        self.kill = fresh.kill;
        self.tempo = fresh.tempo;
        self.sync = fresh.sync;
        self.sync_once = false;
        self.sync_press = None;
        self.match_key = fresh.match_key;
        self.match_once = false;
        self.match_press = None;
        self.key_offset = 0;
        self.pitch_shift = 0.0;
        self.zoom_secs = fresh.zoom_secs;
        self.stem_levels = fresh.stem_levels;
        self.stem_mute = fresh.stem_mute;
        self.filter = fresh.filter;
        self.filter_on = fresh.filter_on;
        self.reset_armed = None;
    }

    /// Ce que joue la platine, pour les suggestions de morceaux : tempo
    /// réellement joué, tonalité entendue, énergie.
    pub fn suggestion_reference(&self) -> Option<crate::suggest::Reference> {
        let analysis = self.analysis()?;
        let link = self.link();
        Some(crate::suggest::Reference {
            bpm: link.bpm,
            key: link.key.map(|key| key.transposed(link.detune.round() as i32)),
            energy: analysis.energy,
        })
    }

    fn analysis(&self) -> Option<Analysis> {
        self.current.as_ref().map(|track| track.analysis)
    }

    /// Demi-tons dont la hauteur varie à cause du tempo (nul avec le key lock).
    fn tempo_detune(&self) -> f32 {
        if self.key_lock { 0.0 } else { 12.0 * self.tempo.log2() }
    }

    pub fn link(&self) -> DeckLink {
        let analysis = self.analysis();
        DeckLink {
            bpm: analysis.and_then(|a| a.bpm).map(|bpm| bpm * self.tempo),
            key: analysis.and_then(|a| a.key),
            detune: self.tempo_detune() + self.pitch_shift,
        }
    }

    /// Applique SYNC et MATCH KEY par rapport à l'autre platine ; à appeler à
    /// chaque image, avant [`DeckUi::sync`].
    pub fn follow(&mut self, other: &DeckLink) {
        let analysis = self.analysis();

        let sync_now = self.sync || std::mem::take(&mut self.sync_once);
        if sync_now && let (Some(own), Some(target)) = (analysis.and_then(|a| a.bpm), other.bpm) {
            // Si l'écart est énorme, on se cale sur la moitié ou le double.
            let mut ratio = target / own;
            while ratio > 1.42 {
                ratio *= 0.5;
            }
            while ratio < 0.705 {
                ratio *= 2.0;
            }
            self.tempo = ratio;
        }

        self.other_key = other.key.map(|key| key.transposed(other.detune.round() as i32));
        let match_now = self.match_key || std::mem::take(&mut self.match_once);
        if match_now && let (Some(own), Some(target)) = (analysis.and_then(|a| a.key), other.key) {
            // Hauteur entendue de l'autre platine, ramenée au mode de ce
            // morceau (une tonalité mineure s'accorde à sa relative majeure).
            let mut wanted = target.pitch_class as f32 + other.detune;
            if target.minor && !own.minor {
                wanted += 3.0;
            } else if !target.minor && own.minor {
                wanted -= 3.0;
            }
            // Plus petit décalage (−6 à +6 demi-tons), en plus de celui que
            // donne déjà le tempo quand le key lock est coupé.
            let shift = (wanted - own.pitch_class as f32 - self.tempo_detune() + 6.0).rem_euclid(12.0) - 6.0;
            self.pitch_shift = shift;
            self.key_offset = (shift.round() as i32).clamp(-KEY_RANGE, KEY_RANGE);
        }
    }

    /// Règle la transposition au slider de tonalité (coupe MATCH permanent).
    fn set_key_offset(&mut self, offset: i32) {
        self.key_offset = offset.clamp(-KEY_RANGE, KEY_RANGE);
        self.pitch_shift = self.key_offset as f32;
        self.match_key = false;
    }

    /// Vrai si quelque chose bouge à l'écran (lecture, vu-mètre, chargement).
    pub fn is_animating(&self, engine: &EngineHandle) -> bool {
        let shared = &engine.shared.decks[self.index];
        shared.playing.load(Relaxed)
            || shared.armed.load(Relaxed)
            || self.meter.is_active()
            || self.loading.is_some()
            || self.incoming.is_some()
    }

    /// Synchronise l'interface et le moteur ; à appeler une fois par image.
    /// Retourne un message d'erreur si un chargement a été refusé.
    pub fn sync(&mut self, engine: &EngineHandle, dt: f32) -> Option<String> {
        let shared = &engine.shared.decks[self.index];
        let mut error = None;

        // Paramètres UI → moteur.
        shared.volume.store(fader_to_gain(self.volume));
        for band in 0..3 {
            shared.eq[band].store(knob_to_gain(self.eq[band]));
            shared.kill[band].store(self.kill[band], Relaxed);
        }
        self.tempo = self.tempo.clamp(*TEMPO_RANGE.start(), *TEMPO_RANGE.end());
        shared.tempo.store(self.tempo);
        shared.key_lock.store(self.key_lock, Relaxed);
        shared.pitch_shift.store(self.pitch_shift);
        shared.magnet.store(self.magnet, Relaxed);
        shared.filter.store(self.filter_amount());
        let stems_ready = matches!(self.stem_state(), Some(StemState::Ready));
        for stem in 0..NUM_STEMS {
            let gain = match (stems_ready, self.stem_mute[stem]) {
                (false, _) => 1.0,
                (true, true) => 0.0,
                (true, false) => fader_to_gain(self.stem_levels[stem]),
            };
            shared.stems[stem].store(gain);
        }

        // Prise en compte (ou refus) du morceau envoyé au moteur.
        let loaded_id = shared.loaded_id.load(Relaxed);
        if let Some(incoming) = &self.incoming {
            if incoming.id == loaded_id {
                self.current = self.incoming.take();
                self.display_pos = 0.0;
                // Nouveau morceau : tonalité d'origine (sauf MATCH permanent).
                self.key_offset = 0;
                self.pitch_shift = 0.0;
            } else if shared.rejected_id.load(Relaxed) == incoming.id {
                error = Some(format!(
                    "Platine {} en lecture : impossible d'y charger « {} ».",
                    theme::DECK_NAMES[self.index],
                    incoming.info.title
                ));
                self.incoming = None;
            }
        }
        if self.current.as_ref().is_some_and(|track| track.id != loaded_id) {
            self.current = None;
        }

        // Position lissée : le moteur ne publie sa position qu'une fois par bloc
        // audio ; entre deux, on extrapole pour un défilement fluide.
        let engine_pos = shared.position.load(Relaxed) as f64;
        let sample_rate = engine.shared.sample_rate as f64;
        let playing = shared.playing.load(Relaxed);
        if playing {
            self.display_pos += dt as f64 * sample_rate * self.tempo as f64;
        }
        if self.seek_hold > 0.0 {
            self.seek_hold -= dt;
        } else if playing {
            let error = engine_pos - self.display_pos;
            if error.abs() > 0.08 * sample_rate {
                self.display_pos = engine_pos;
            } else {
                self.display_pos += error * 0.1;
            }
        } else {
            self.display_pos = engine_pos;
        }

        if let Some(delay) = &mut self.snap_delay {
            *delay -= dt;
            if *delay <= 0.0 {
                self.snap_delay = None;
                self.snap_position(engine);
            }
        }

        self.meter.update([shared.peak[0].take(), shared.peak[1].take()], dt);
        error
    }

    // -----------------------------------------------------------------------
    // MAGNET à l'arrêt et marqueurs
    // -----------------------------------------------------------------------

    /// Vrai si les déplacements se calent sur les temps : MAGNET actif,
    /// platine à l'arrêt et tempo connu.
    fn snaps(&self, engine: &EngineHandle) -> bool {
        self.magnet
            && !engine.shared.decks[self.index].playing.load(Relaxed)
            && self.analysis().is_some_and(|analysis| analysis.bpm.is_some())
    }

    /// Temps le plus proche de `frame` dans le morceau courant.
    fn nearest_beat(&self, frame: f64, sample_rate: f64) -> f64 {
        match &self.current {
            Some(track) => snap_to_beat(&track.analysis, sample_rate, frame).min(self.frames()),
            None => frame,
        }
    }

    fn seek_to(&mut self, engine: &EngineHandle, frame: f64) {
        let frame = frame.clamp(0.0, self.frames());
        engine.send(Command::Seek {
            deck: self.index,
            frame: frame as u64,
        });
        self.display_pos = frame;
        self.seek_hold = SEEK_HOLD_SECS;
    }

    /// Cale la position affichée (la dernière demandée) sur le temps le plus
    /// proche, si MAGNET est actif à l'arrêt.
    fn snap_display(&mut self, engine: &EngineHandle) {
        if !self.snaps(engine) {
            return;
        }
        let snapped = self.nearest_beat(self.display_pos, engine.shared.sample_rate as f64);
        if (snapped - self.display_pos).abs() >= 1.0 {
            self.seek_to(engine, snapped);
        }
    }

    /// Demande de caler bientôt la position sur un temps (fin d'une recherche
    /// au clavier) ; sans effet si MAGNET est coupé ou la platine en lecture.
    pub fn request_snap(&mut self) {
        self.snap_delay = Some(SNAP_DELAY_SECS);
    }

    fn snap_position(&mut self, engine: &EngineHandle) {
        if !self.snaps(engine) {
            return;
        }
        let sample_rate = engine.shared.sample_rate as f64;
        let position = engine.shared.decks[self.index].position.load(Relaxed) as f64;
        let snapped = self.nearest_beat(position, sample_rate);
        if (snapped - position).abs() >= 1.0 {
            self.seek_to(engine, snapped);
        }
    }

    // -----------------------------------------------------------------------
    // Stems
    // -----------------------------------------------------------------------

    /// État de la séparation du morceau courant.
    fn stem_state(&self) -> Option<&StemState> {
        let current = self.current.as_ref()?.id;
        self.stems
            .as_ref()
            .filter(|(id, _)| *id == current)
            .map(|(_, state)| state)
    }

    /// Morceau courant à séparer : (identifiant, fichier, longueur en frames).
    pub fn separation_target(&self) -> Option<(u64, std::path::PathBuf, usize)> {
        let track = self.current.as_ref()?;
        Some((track.id, track.info.path.clone(), track.waveform.frames))
    }

    /// Vrai si la séparation du morceau `id` est déjà en cours ou terminée.
    pub fn stems_requested(&self, id: u64) -> bool {
        self.stems
            .as_ref()
            .is_some_and(|(current, state)| *current == id && !matches!(state, StemState::Failed(_)))
    }

    pub fn stems_started(&mut self, id: u64) {
        self.stems = Some((id, StemState::Working(0.0)));
    }

    pub fn stems_progress(&mut self, id: u64, fraction: f32) {
        if let Some((current, StemState::Working(progress))) = &mut self.stems
            && *current == id
        {
            *progress = fraction;
        }
    }

    /// Fin de la séparation ; retourne `false` si elle concerne un morceau
    /// qui n'est plus sur la platine.
    pub fn stems_finished(&mut self, id: u64, result: Result<(), String>) -> bool {
        if self.stems.as_ref().is_none_or(|(current, _)| *current != id) {
            return false;
        }
        let state = match result {
            Ok(()) => StemState::Ready,
            Err(error) => StemState::Failed(error),
        };
        self.stems = Some((id, state));
        true
    }

    /// Cale une fois le tempo sur l'autre platine (comme un clic sur SYNC).
    pub fn request_sync_once(&mut self) {
        self.sync_once = true;
    }

    /// Accorde une fois la tonalité sur l'autre platine (clic sur MATCH).
    pub fn request_match_once(&mut self) {
        self.match_once = true;
    }

    /// Saute au marqueur `slot` (y compris début / fin de la musique) s'il
    /// est posé ; retourne `false` sinon.
    pub fn jump_to_marker(&mut self, slot: usize, engine: &EngineHandle) -> bool {
        let Some(secs) = self.current.as_ref().and_then(|track| track.triggers()[slot]) else {
            return false;
        };
        self.seek_to(engine, secs * engine.shared.sample_rate as f64);
        true
    }

    /// Vrai si le morceau courant est séparé en stems.
    pub fn stems_ready(&self) -> bool {
        matches!(self.stem_state(), Some(StemState::Ready))
    }

    /// Transpose la platine de `offset` demi-tons (étape de transition).
    pub fn transpose(&mut self, offset: i32) {
        self.set_key_offset(offset);
    }

    /// Tonalité d'origine du morceau courant.
    pub fn current_key(&self) -> Option<Key> {
        self.analysis().and_then(|analysis| analysis.key)
    }

    /// Titre du morceau courant.
    pub fn current_title(&self) -> Option<&str> {
        self.current.as_ref().map(|track| track.info.title.as_str())
    }

    /// Fichier du morceau courant.
    pub fn current_path(&self) -> Option<&std::path::Path> {
        self.current.as_ref().map(|track| track.info.path.as_path())
    }

    /// Chemin du morceau courant et positions de ses marqueurs (8 + début / fin).
    pub fn current_triggers(&self) -> Option<(&std::path::Path, Triggers)> {
        self.current
            .as_ref()
            .map(|track| (track.info.path.as_path(), track.triggers()))
    }

    /// Chemin et marqueurs du morceau courant.
    pub fn current_markers(&self) -> Option<(&std::path::Path, &Markers)> {
        self.current
            .as_ref()
            .map(|track| (track.info.path.as_path(), &track.markers))
    }

    /// Marqueur `slot` : y saute s'il est posé, sinon le pose à la position
    /// courante (sur le temps le plus proche si MAGNET est actif).
    /// Retourne `true` si les marqueurs ont changé.
    pub fn marker_pressed(&mut self, slot: usize, engine: &EngineHandle) -> bool {
        let sample_rate = engine.shared.sample_rate as f64;
        let Some(marker) = self.current.as_ref().map(|track| track.markers[slot]) else {
            return false;
        };
        if let Some(secs) = marker {
            self.seek_to(engine, secs * sample_rate);
            return false;
        }
        let mut frame = engine.shared.decks[self.index].position.load(Relaxed) as f64;
        if self.magnet {
            frame = self.nearest_beat(frame, sample_rate);
        }
        if let Some(track) = &mut self.current {
            track.markers[slot] = Some(frame / sample_rate);
        }
        true
    }

    /// Efface le marqueur `slot` ; retourne `true` s'il était posé.
    pub fn marker_clear(&mut self, slot: usize) -> bool {
        self.current
            .as_mut()
            .is_some_and(|track| track.markers[slot].take().is_some())
    }

    /// Dessine la platine dans `rect`. `time` : horloge de l'interface, en secondes.
    pub fn show(&mut self, ui: &mut Ui, rect: Rect, engine: &EngineHandle, time: f64) -> DeckEvents {
        ui.painter().rect_filled(rect, 8.0, theme::PANEL);
        let mut area = rect.shrink(10.0);

        let header = take_top(&mut area, 46.0);
        take_top(&mut area, 6.0);
        let zoom = take_top(&mut area, 104.0);
        take_top(&mut area, 5.0);
        let overview = take_top(&mut area, 40.0);
        take_top(&mut area, 6.0);
        let markers = take_top(&mut area, 22.0);
        take_top(&mut area, 8.0);

        self.show_header(ui, header, engine, time);
        self.show_zoom_waveform(ui, zoom, engine, time);
        self.show_overview(ui, overview, engine, time);
        let markers_changed = self.show_markers(ui, markers, engine);
        let mut events = self.show_controls(ui, area, engine, time);
        events.markers_changed |= markers_changed;
        events
    }

    fn id(&self, name: &str) -> Id {
        Id::unique(("deck", self.index, name))
    }

    fn color(&self) -> Color32 {
        theme::DECK_COLORS[self.index]
    }

    /// Longueur du morceau courant en frames (0 si aucun).
    fn frames(&self) -> f64 {
        self.current.as_ref().map_or(0.0, |track| track.waveform.frames as f64)
    }

    // -----------------------------------------------------------------------
    // En-tête : nom de la platine, titre, artiste, temps
    // -----------------------------------------------------------------------

    fn show_header(&self, ui: &Ui, mut rect: Rect, engine: &EngineHandle, time: f64) {
        let painter = ui.painter();
        let sample_rate = engine.shared.sample_rate as f64;

        // Pastille « A » / « B ».
        let badge = take_left(&mut rect, 46.0).shrink2(vec2(4.0, 4.0));
        let badge = Rect::from_center_size(badge.center(), vec2(36.0, 36.0));
        painter.rect_filled(badge, 8.0, self.color());
        painter.text(
            badge.center(),
            Align2::CENTER_CENTER,
            theme::DECK_NAMES[self.index],
            FontId::proportional(24.0),
            Color32::BLACK,
        );
        take_left(&mut rect, 8.0);

        // Temps : écoulé, restant et durée totale.
        let times = take_right(&mut rect, 190.0);
        if let Some(track) = &self.current {
            let elapsed = self.display_pos / sample_rate;
            let total = track.waveform.duration_secs();
            painter.text(
                times.right_top(),
                Align2::RIGHT_TOP,
                format_time(elapsed),
                FontId::monospace(24.0),
                theme::TEXT,
            );
            painter.text(
                times.right_bottom(),
                Align2::RIGHT_BOTTOM,
                format!(
                    "-{}  /  {}",
                    format_time((total - elapsed).max(0.0)),
                    format_time(total)
                ),
                FontId::monospace(12.0),
                theme::TEXT_DIM,
            );
        }

        // Tempo joué, tonalité entendue et variation de vitesse.
        let musical = take_right(&mut rect, 164.0);
        if let Some(analysis) = self.analysis() {
            let bpm = match analysis.bpm {
                Some(bpm) => format!("{:.1} BPM", bpm * self.tempo),
                None => "--- BPM".to_string(),
            };
            painter.text(
                musical.right_top() + vec2(-14.0, 3.0),
                Align2::RIGHT_TOP,
                bpm,
                FontId::monospace(16.0),
                self.color(),
            );
            let shift = (self.tempo_detune() + self.pitch_shift).round() as i32;
            let key = match analysis.key {
                Some(key) => {
                    let heard = key.transposed(shift);
                    format!("{} ({})", heard.short_name(), heard.camelot())
                }
                None => "?".to_string(),
            };
            painter.text(
                musical.right_bottom() + vec2(-14.0, 0.0),
                Align2::RIGHT_BOTTOM,
                format!("{key}  {:+.1}%", (self.tempo - 1.0) * 100.0),
                FontId::monospace(12.0),
                if shift != 0 { theme::YELLOW } else { theme::TEXT_DIM },
            );
        }

        // Titre et artiste (ou état de chargement).
        let (title, subtitle) = match (&self.loading, &self.current) {
            (Some((_, info)), _) => {
                let dots = ".".repeat(1 + (time * 3.0) as usize % 3);
                (info.title.clone(), format!("Chargement{dots}"))
            }
            (None, Some(track)) => (track.info.title.clone(), track.info.artist.clone()),
            (None, None) => (
                "Aucun morceau".to_string(),
                format!(
                    "Glissez un morceau ici, ou Maj + flèche {}",
                    if self.index == 0 { "gauche" } else { "droite" }
                ),
            ),
        };
        let title_color = if self.current.is_some() || self.loading.is_some() {
            theme::TEXT
        } else {
            theme::TEXT_DIM
        };
        let (top, bottom) = rect.split_top_bottom_at_fraction(0.58);
        clipped_text(
            ui,
            top,
            Align2::LEFT_CENTER,
            &title,
            FontId::proportional(18.0),
            title_color,
        );
        clipped_text(
            ui,
            bottom,
            Align2::LEFT_CENTER,
            &subtitle,
            FontId::proportional(13.0),
            theme::TEXT_DIM,
        );
    }

    // -----------------------------------------------------------------------
    // Formes d'onde
    // -----------------------------------------------------------------------

    /// Couleur d'alerte clignotante quand le morceau touche à sa fin.
    fn end_warning(&self, engine: &EngineHandle, time: f64) -> bool {
        let remaining = (self.frames() - self.display_pos) / engine.shared.sample_rate as f64;
        self.current.is_some()
            && engine.shared.decks[self.index].playing.load(Relaxed)
            && remaining < END_WARNING_SECS
            && (time * 2.0).fract() < 0.5
    }

    /// Vue zoomée défilante, centrée sur la tête de lecture.
    fn show_zoom_waveform(&mut self, ui: &mut Ui, rect: Rect, engine: &EngineHandle, time: f64) {
        let response = ui.interact(rect, self.id("zoom"), Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 4.0, theme::WAVE_BACKGROUND);

        // Molette : zoom.
        if response.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                self.zoom_secs =
                    (self.zoom_secs * (-scroll * 0.004).exp()).clamp(*ZOOM_RANGE_SECS.start(), *ZOOM_RANGE_SECS.end());
            }
        }

        let sample_rate = engine.shared.sample_rate as f64;
        let frames_per_px = 2.0 * self.zoom_secs as f64 * sample_rate / rect.width().max(1.0) as f64;

        if let Some(track) = &self.current {
            // Glisser : déplace le morceau sous la tête de lecture.
            if response.dragged() {
                let frames = (-response.drag_delta().x as f64 * frames_per_px).round() as i64;
                if frames != 0 {
                    engine.send(Command::SeekRelative {
                        deck: self.index,
                        frames,
                    });
                    self.display_pos = (self.display_pos + frames as f64).clamp(0.0, self.frames());
                    self.seek_hold = SEEK_HOLD_SECS;
                }
            }

            let first_frame = self.display_pos - rect.width() as f64 * 0.5 * frames_per_px;
            paint_waveform(&painter, rect, &track.waveform, first_frame, frames_per_px, None);

            // Grille de temps : une barre par temps, plus marquée tous les 4 temps.
            if let Some(bpm) = track.analysis.bpm {
                let period = 60.0 / bpm as f64 * sample_rate;
                if period / frames_per_px >= MIN_BEAT_SPACING_PX {
                    let anchor = track.analysis.beat_offset as f64 * sample_rate;
                    let last_frame =
                        (first_frame + rect.width() as f64 * frames_per_px).min(track.waveform.frames as f64);
                    let mut beat = ((first_frame.max(0.0) - anchor) / period).ceil() as i64;
                    loop {
                        let frame = anchor + beat as f64 * period;
                        if frame > last_frame {
                            break;
                        }
                        let x = rect.left() + ((frame - first_frame) / frames_per_px) as f32;
                        let alpha = if beat.rem_euclid(4) == 0 { 130 } else { 50 };
                        painter.vline(x, rect.y_range(), Stroke::new(1.0, Color32::from_white_alpha(alpha)));
                        beat += 1;
                    }
                }
            }

            // Boucle : bande verte, et pastille pour en sortir.
            let shared = &engine.shared.decks[self.index];
            let loop_len = shared.loop_len.load(Relaxed) as f64;
            if loop_len > 0.0 {
                let start = shared.loop_start.load(Relaxed) as f64;
                let x0 = rect.left() + ((start - first_frame) / frames_per_px) as f32;
                let x1 = rect.left() + ((start + loop_len - first_frame) / frames_per_px) as f32;
                let band = Rect::from_x_y_ranges(x0.max(rect.left())..=x1.min(rect.right()), rect.y_range());
                if band.width() > 0.0 {
                    painter.rect_filled(band, 0.0, theme::GREEN.gamma_multiply(0.16));
                }
                for x in [x0, x1] {
                    if rect.x_range().contains(x) {
                        painter.vline(x, rect.y_range(), Stroke::new(1.5, theme::GREEN));
                    }
                }
                let beats = self
                    .beat_grid()
                    .map(|(_, period)| loop_len / sample_rate / period)
                    .map_or(String::new(), |beats| {
                        format!(
                            " {}",
                            crate::transitions::beat_label(((beats * 8.0).round() / 8.0) as f32)
                        )
                    });
                let text = format!("BOUCLE{beats}");
                let galley = painter.layout_no_wrap(text, FontId::proportional(10.0), Color32::BLACK);
                let chip = Rect::from_min_size(
                    pos2(rect.right() - galley.size().x - 12.0, rect.top() + 3.0),
                    galley.size() + vec2(8.0, 4.0),
                );
                let chip_response = ui
                    .interact(chip, self.id("loop_exit"), Sense::click())
                    .on_hover_text("Boucle en cours — clic : en sortir");
                let fill = if chip_response.hovered() {
                    theme::GREEN.gamma_multiply(1.2)
                } else {
                    theme::GREEN
                };
                painter.rect_filled(chip, 3.0, fill);
                painter.galley(chip.min + vec2(4.0, 2.0), galley, Color32::BLACK);
                if chip_response.clicked() {
                    engine.send(Command::SetLoop {
                        deck: self.index,
                        beats: None,
                    });
                }
            }

            // Point de cue.
            let cue = engine.shared.decks[self.index].cue.load(Relaxed) as f64;
            let cue_x = rect.left() + ((cue - first_frame) / frames_per_px) as f32;
            if rect.x_range().contains(cue_x) {
                painter.vline(cue_x, rect.y_range(), Stroke::new(2.0, theme::CUE));
                painter.text(
                    pos2(cue_x + 4.0, rect.top() + 2.0),
                    Align2::LEFT_TOP,
                    "CUE",
                    FontId::proportional(10.0),
                    theme::CUE,
                );
            }

            // Début / fin de la musique : trait clair pointillé, étiquette en haut.
            for (secs, label) in [(track.bounds.0, "DÉBUT"), (track.bounds.1, "FIN")] {
                let x = rect.left() + ((secs * sample_rate - first_frame) / frames_per_px) as f32;
                if !rect.x_range().contains(x) {
                    continue;
                }
                let stroke = Stroke::new(1.5, theme::SPECIAL_MARKER);
                let mut y = rect.top();
                while y < rect.bottom() {
                    painter.vline(x, y..=(y + 4.0).min(rect.bottom()), stroke);
                    y += 7.0;
                }
                let galley = painter.layout_no_wrap(label.into(), FontId::proportional(9.0), Color32::BLACK);
                let tag = Rect::from_min_size(pos2(x, rect.top()), galley.size() + vec2(6.0, 2.0));
                painter.rect_filled(tag, 2.0, theme::SPECIAL_MARKER);
                painter.galley(tag.min + vec2(3.0, 1.0), galley, Color32::BLACK);
            }

            // Marqueurs : trait de couleur et numéro en bas.
            for (slot, marker) in track.markers.iter().enumerate() {
                let Some(secs) = marker else { continue };
                let x = rect.left() + ((secs * sample_rate - first_frame) / frames_per_px) as f32;
                if !rect.x_range().contains(x) {
                    continue;
                }
                let color = theme::MARKER_COLORS[slot];
                painter.vline(x, rect.y_range(), Stroke::new(2.0, color));
                let label = Rect::from_min_size(pos2(x, rect.bottom() - 15.0), vec2(13.0, 15.0));
                painter.rect_filled(label, 2.0, color);
                painter.text(
                    label.center(),
                    Align2::CENTER_CENTER,
                    (slot + 1).to_string(),
                    FontId::proportional(11.0),
                    Color32::BLACK,
                );
            }

            if self.end_warning(engine, time) {
                painter.rect_filled(rect, 4.0, theme::RED.gamma_multiply(0.3));
            }
        }

        // MAGNET à l'arrêt : calage sur le temps le plus proche au relâchement.
        if response.drag_stopped() {
            self.snap_display(engine);
        }

        // Tête de lecture fixe au centre.
        painter.vline(rect.center().x, rect.y_range(), Stroke::new(2.0, Color32::WHITE));
        painter.rect_stroke(rect, 4.0, Stroke::new(1.0, theme::OUTLINE), StrokeKind::Inside);
        painter.text(
            rect.right_bottom() - vec2(6.0, 3.0),
            Align2::RIGHT_BOTTOM,
            format!("±{:.1} s", self.zoom_secs),
            FontId::proportional(10.0),
            theme::TEXT_DIM,
        );
    }

    /// Vue d'ensemble du morceau : clic ou glisser pour déplacer la lecture.
    fn show_overview(&mut self, ui: &mut Ui, rect: Rect, engine: &EngineHandle, time: f64) {
        let response = ui.interact(rect, self.id("overview"), Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 4.0, theme::WAVE_BACKGROUND);

        let sample_rate = engine.shared.sample_rate as f64;
        let mut released = false;
        if let Some(track) = &self.current {
            let frames = self.frames().max(1.0);
            let frames_per_px = frames / rect.width().max(1.0) as f64;

            // Seek : immédiat à l'appui, puis à chaque déplacement.
            if response.is_pointer_button_down_on() {
                if let Some(pointer) = response.interact_pointer_pos() {
                    let fraction = ((pointer.x - rect.left()) / rect.width()).clamp(0.0, 1.0) as f64;
                    let frame = (fraction * frames) as u64;
                    if self.last_seek != Some(frame) {
                        self.last_seek = Some(frame);
                        engine.send(Command::Seek {
                            deck: self.index,
                            frame,
                        });
                        self.display_pos = frame as f64;
                        self.seek_hold = SEEK_HOLD_SECS;
                    }
                }
            } else {
                released = self.last_seek.take().is_some();
            }

            paint_waveform(
                &painter,
                rect,
                &track.waveform,
                0.0,
                frames_per_px,
                Some(self.display_pos),
            );

            let x_of = |frame: f64| rect.left() + (frame / frames) as f32 * rect.width();
            // Avant le début et après la fin de la musique : zone assombrie.
            let (start, end) = (x_of(track.bounds.0 * sample_rate), x_of(track.bounds.1 * sample_rate));
            let shade = Color32::from_black_alpha(140);
            painter.rect_filled(Rect::from_x_y_ranges(rect.left()..=start, rect.y_range()), 0.0, shade);
            painter.rect_filled(Rect::from_x_y_ranges(end..=rect.right(), rect.y_range()), 0.0, shade);
            for x in [start, end] {
                painter.vline(x, rect.y_range(), Stroke::new(1.0, theme::SPECIAL_MARKER));
            }
            let shared = &engine.shared.decks[self.index];
            let loop_len = shared.loop_len.load(Relaxed) as f64;
            if loop_len > 0.0 {
                let start = shared.loop_start.load(Relaxed) as f64;
                let band = Rect::from_x_y_ranges(
                    x_of(start)..=x_of(start + loop_len).max(x_of(start) + 2.0),
                    rect.y_range(),
                );
                painter.rect_filled(band, 0.0, theme::GREEN.gamma_multiply(0.45));
            }
            let cue = shared.cue.load(Relaxed) as f64;
            painter.vline(x_of(cue), rect.y_range(), Stroke::new(1.5, theme::CUE));
            for (slot, marker) in track.markers.iter().enumerate() {
                if let Some(secs) = marker {
                    let stroke = Stroke::new(1.5, theme::MARKER_COLORS[slot]);
                    painter.vline(x_of(secs * sample_rate), rect.y_range(), stroke);
                }
            }

            if self.end_warning(engine, time) {
                painter.rect_filled(rect, 4.0, theme::RED.gamma_multiply(0.3));
            }
            painter.vline(x_of(self.display_pos), rect.y_range(), Stroke::new(2.0, Color32::WHITE));
        }
        painter.rect_stroke(rect, 4.0, Stroke::new(1.0, theme::OUTLINE), StrokeKind::Inside);

        // MAGNET à l'arrêt : calage sur le temps le plus proche au relâchement.
        if released {
            self.snap_display(engine);
        }
    }

    /// Rangée des marqueurs : clic sur un marqueur vide pour le poser à la
    /// position courante, sur un marqueur posé pour y sauter ; clic droit pour
    /// l'effacer. Retourne `true` si les marqueurs ont changé.
    fn show_markers(&mut self, ui: &mut Ui, mut rect: Rect, engine: &EngineHandle) -> bool {
        const GAP: f32 = 4.0;
        // Début et fin de la musique, aux deux bouts : clic pour y aller.
        let start_rect = take_left(&mut rect, 46.0);
        take_left(&mut rect, GAP);
        let end_rect = take_right(&mut rect, 46.0);
        take_right(&mut rect, GAP);
        for (slot, button, label) in [(MARKER_START, start_rect, "DÉBUT"), (MARKER_END, end_rect, "FIN")] {
            let position = self.current.as_ref().and_then(|track| track.triggers()[slot]);
            let mut response = toggle_button(
                ui,
                button,
                self.id("special_marker").with(slot),
                label,
                false,
                theme::SPECIAL_MARKER,
            );
            if let Some(secs) = position {
                response = response.on_hover_text(format!(
                    "{} de la musique — {} (placé automatiquement)
Clic : y aller",
                    if slot == MARKER_START { "Début" } else { "Fin" },
                    format_time(secs)
                ));
            }
            if response.clicked() {
                self.jump_to_marker(slot, engine);
            }
        }
        let width = (rect.width() - GAP * (NUM_MARKERS - 1) as f32) / NUM_MARKERS as f32;
        let mut changed = false;
        for slot in 0..NUM_MARKERS {
            let button = Rect::from_min_size(
                rect.left_top() + vec2(slot as f32 * (width + GAP), 0.0),
                vec2(width, rect.height()),
            );
            let marker = self.current.as_ref().and_then(|track| track.markers[slot]);
            let response = toggle_button(
                ui,
                button,
                self.id("marker").with(slot),
                &(slot + 1).to_string(),
                marker.is_some(),
                theme::MARKER_COLORS[slot],
            );
            let response = match marker {
                Some(secs) => response.on_hover_text(format!(
                    "Marqueur {} — {}\nClic : y aller\nClic droit : effacer",
                    slot + 1,
                    format_time(secs)
                )),
                None => response.on_hover_text(format!(
                    "Marqueur {} (vide)\nClic : le poser à la position courante\n\
                     (sur le temps le plus proche si MAGNET est actif)",
                    slot + 1
                )),
            };
            if response.clicked() {
                changed |= self.marker_pressed(slot, engine);
            }
            if response.secondary_clicked() {
                changed |= self.marker_clear(slot);
            }
        }
        changed
    }

    // -----------------------------------------------------------------------
    // Contrôles : SYNC + tempo, transport, options, EQ, stems, volume
    // -----------------------------------------------------------------------

    fn show_controls(&mut self, ui: &mut Ui, mut rect: Rect, engine: &EngineHandle, time: f64) -> DeckEvents {
        let mut events = DeckEvents::default();
        let shared = &engine.shared.decks[self.index];
        let playing = shared.playing.load(Relaxed);
        let loaded = self.current.is_some();
        let color = self.color();
        // Platine A : de l'extérieur (gauche) vers le mixer (droite) ; platine B
        // en miroir. `outer` : prendre la bande côté extérieur, sinon côté mixer.
        let mirrored = self.index == 1;
        let take = |rect: &mut Rect, width: f32, outer: bool| {
            if outer != mirrored {
                take_left(rect, width)
            } else {
                take_right(rect, width)
            }
        };

        // --- MATCH au-dessus du slider de tonalité (côté extérieur) ---
        let mut key_rect = take(&mut rect, 64.0, true);
        let match_rect = take_top(&mut key_rect, 20.0);
        take_top(&mut key_rect, 4.0);
        self.match_button(ui, match_rect, time, &mut events);
        self.show_key_slider(ui, key_rect, color);
        take(&mut rect, 6.0, true);

        // --- SYNC au-dessus du fader de tempo ---
        let mut tempo_rect = take(&mut rect, 46.0, true);
        let sync_rect = take_top(&mut tempo_rect, 20.0);
        take_top(&mut tempo_rect, 4.0);
        self.sync_button(ui, sync_rect, time, &mut events);

        let tempo_label = take_bottom(&mut tempo_rect, 14.0);
        let mut position = (0.5 + (self.tempo - 1.0) / (2.0 * TEMPO_FADER_RANGE)).clamp(0.0, 1.0);
        let tempo_fader = fader(
            ui,
            tempo_rect,
            self.id("tempo"),
            &mut position,
            0.5,
            Orientation::Vertical,
            color,
        )
        .on_hover_text("Tempo, ±12 % (haut = plus rapide)\nDouble-clic : tempo d'origine\nLe déplacer coupe SYNC.");
        if tempo_fader.changed() {
            self.tempo = 1.0 + (position - 0.5) * 2.0 * TEMPO_FADER_RANGE;
            self.sync = false;
        }
        ui.painter().text(
            tempo_label.center(),
            Align2::CENTER_CENTER,
            "TEMPO",
            FontId::proportional(10.0),
            theme::TEXT_DIM,
        );
        take(&mut rect, 4.0, true);

        // --- Transport ---
        let transport = take(&mut rect, 124.0, true);
        let button = vec2(58.0, 58.0);
        let cue_rect = Rect::from_center_size(transport.center() - vec2(31.0, 0.0), button);
        let play_rect = Rect::from_center_size(transport.center() + vec2(31.0, 0.0), button);

        let at_cue = loaded && !playing && shared.cue.load(Relaxed) == shared.position.load(Relaxed);
        let cue = transport_button(ui, cue_rect, self.id("cue"), TransportIcon::Cue, at_cue, theme::CUE)
            .on_hover_text("En lecture : retour au point de cue et arrêt.\nÀ l'arrêt : définit le point de cue ici.\nClic droit : retour au point de cue.");
        if cue.clicked() {
            engine.send(Command::Cue(self.index));
        }
        if cue.secondary_clicked() {
            engine.send(Command::CueGotoAndStop(self.index));
        }

        let icon = if playing {
            TransportIcon::Pause
        } else {
            TransportIcon::Play
        };
        // En attente du prochain temps de l'autre platine (magnet) : le bouton clignote.
        let armed = shared.armed.load(Relaxed) && (time * 5.0).fract() < 0.5;
        if transport_button(ui, play_rect, self.id("play"), icon, playing || armed, theme::GREEN).clicked() {
            engine.send(Command::TogglePlay(self.index));
        }
        take(&mut rect, 4.0, true);

        // --- KEY LOCK, MAGNET ---
        let options = take(&mut rect, 66.0, true);
        let labels = [
            ("KEY LOCK", "La tonalité reste la même quel que soit le tempo."),
            (
                "MAGNET",
                "Au lancement, attend le prochain temps de l'autre platine pour démarrer calé.\n\
                 À l'arrêt, la lecture se cale sur le temps le plus proche quand on relâche\n\
                 la forme d'onde après l'avoir déplacée.",
            ),
        ];
        let spacing = 30.0;
        let top = options.center().y - spacing;
        for (i, (label, help)) in labels.into_iter().enumerate() {
            let button = Rect::from_center_size(pos2(options.center().x, top + i as f32 * spacing), vec2(64.0, 22.0));
            let state = if i == 0 { &mut self.key_lock } else { &mut self.magnet };
            if toggle_button(
                ui,
                button,
                Id::unique(("deck", self.index, label)),
                label,
                *state,
                color,
            )
            .on_hover_text(help)
            .clicked()
            {
                *state = !*state;
            }
        }
        // RÉINIT. : deux clics (le premier arme le bouton pendant 2 s).
        let armed = self.reset_armed.is_some_and(|since| time - since < 2.0);
        if !armed {
            self.reset_armed = None;
        } else {
            ui.ctx().request_repaint();
        }
        let button = Rect::from_center_size(pos2(options.center().x, top + 2.0 * spacing), vec2(64.0, 22.0));
        let label = if armed { "SÛR ?" } else { "RÉINIT." };
        if toggle_button(ui, button, self.id("reset"), label, armed, theme::RED)
            .on_hover_text(
                "Réinitialise la platine : volume, EQ et kills, filtre, boucle, tempo,\n\
                 tonalité, SYNC, MATCH, faders et coupures des stems, zoom.\n\
                 KEY LOCK, MAGNET, le morceau, sa position, le cue et les marqueurs sont conservés.\n\
                 Cliquer deux fois (confirmation).",
            )
            .clicked()
        {
            if armed {
                self.reset_controls();
                engine.send(Command::SetLoop {
                    deck: self.index,
                    beats: None,
                });
                events.reset = true;
            } else {
                self.reset_armed = Some(time);
            }
        }
        take(&mut rect, 8.0, true);

        // --- Vu-mètre et fader de volume (côté mixer) ---
        let meter_rect = take(&mut rect, 18.0, false);
        vu_meter(ui, meter_rect.shrink2(vec2(0.0, 2.0)), &self.meter);
        take(&mut rect, 6.0, false);

        let mut volume_rect = take(&mut rect, 40.0, false);
        let label = take_bottom(&mut volume_rect, 14.0);
        fader(
            ui,
            volume_rect,
            self.id("volume"),
            &mut self.volume,
            1.0,
            Orientation::Vertical,
            color,
        )
        .on_hover_text("Volume de la platine (double-clic : 0 dB)");
        ui.painter().text(
            label.center(),
            Align2::CENTER_CENTER,
            "VOL",
            FontId::proportional(10.0),
            theme::TEXT_DIM,
        );
        take(&mut rect, 8.0, false);

        // --- EQ (côté options), puis stems (côté volume) ---
        let eq = take(&mut rect, 76.0, true);
        take(&mut rect, 8.0, true);
        self.show_eq(ui, eq, color);
        events.separate |= self.show_stems(ui, rect, time);
        events
    }

    /// SYNC : un clic cale le tempo une fois sur l'autre platine ; un appui
    /// maintenu active ou coupe le SYNC permanent.
    fn sync_button(&mut self, ui: &mut Ui, rect: Rect, time: f64, events: &mut DeckEvents) {
        let help = "Clic : cale le tempo une fois sur celui de l'autre platine.\n\
                    Maintenir : active / coupe le SYNC permanent (le tempo suit l'autre platine).\n\
                    Bouger le fader de tempo coupe le SYNC permanent.";
        let id = self.id("sync");
        match hold_button(
            ui,
            rect,
            id,
            "SYNC",
            self.sync,
            self.color(),
            help,
            &mut self.sync_press,
            time,
        ) {
            Press::Click => {
                self.sync_once = true;
                events.sync_once = true;
            }
            Press::Hold => {
                self.sync = !self.sync;
                events.sync_enabled |= self.sync;
            }
            Press::None => {}
        }
    }

    /// MATCH : comme SYNC, pour la tonalité (clic : accord unique ; appui
    /// maintenu : MATCH permanent).
    fn match_button(&mut self, ui: &mut Ui, rect: Rect, time: f64, events: &mut DeckEvents) {
        let help = "Clic : transpose le morceau une fois pour l'accorder à la tonalité de l'autre platine.\n\
                    Maintenir : active / coupe le MATCH permanent (la tonalité suit l'autre platine).\n\
                    Bouger le slider de tonalité coupe le MATCH permanent.";
        let id = self.id("match");
        match hold_button(
            ui,
            rect,
            id,
            "MATCH",
            self.match_key,
            self.color(),
            help,
            &mut self.match_press,
            time,
        ) {
            Press::Click => {
                self.match_once = true;
                events.match_once = true;
            }
            Press::Hold => {
                self.match_key = !self.match_key;
                events.match_key_enabled |= self.match_key;
            }
            Press::None => {}
        }
    }

    /// Slider de tonalité : un cran par demi-ton (±6), chaque cran étiqueté
    /// avec la note entendue à cette position ; les notes qui s'accordent à
    /// l'autre platine sont mises en avant.
    fn show_key_slider(&mut self, ui: &mut Ui, mut rect: Rect, color: Color32) {
        let label = take_bottom(&mut rect, 14.0);
        let response = ui.interact(rect, self.id("key"), Sense::click_and_drag());
        // 13 crans plus une demi-marge de cran en haut et en bas, pour que
        // les étiquettes des extrémités (hautes de deux crans) ne débordent pas.
        let step_height = rect.height() / (2 * KEY_RANGE + 2) as f32;
        // Cran `offset` : +6 en haut, −6 en bas.
        let center_y = |offset: i32| rect.top() + (KEY_RANGE + 1 - offset) as f32 * step_height;

        if response.double_clicked() {
            self.set_key_offset(0);
        } else if (response.clicked() || response.dragged())
            && let Some(pointer) = response.interact_pointer_pos()
        {
            let offset = KEY_RANGE + 1 - ((pointer.y - rect.top()) / step_height).round() as i32;
            if offset.clamp(-KEY_RANGE, KEY_RANGE) != self.key_offset || self.match_key {
                self.set_key_offset(offset);
            }
        }
        if response.hovered() {
            // Un cran de molette = un demi-ton.
            let steps: i32 = ui.input(|i| {
                i.events
                    .iter()
                    .filter_map(|event| match event {
                        egui::Event::MouseWheel { delta, .. } if delta.y != 0.0 => Some(delta.y.signum() as i32),
                        _ => None,
                    })
                    .sum()
            });
            if steps != 0 {
                self.set_key_offset(self.key_offset + steps);
            }
        }

        let own = self.analysis().and_then(|analysis| analysis.key);
        let target = self.other_key;
        // Hauteur déjà apportée par le tempo (key lock coupé).
        let tempo_shift = self.tempo_detune().round() as i32;
        let painter = ui.painter();
        painter.rect_filled(rect, 4.0, theme::WAVE_BACKGROUND);
        painter.rect_stroke(rect, 4.0, Stroke::new(1.0, theme::OUTLINE), StrokeKind::Inside);
        // Les crans sont serrés : étiquettes en quinconce (pairs à gauche,
        // impairs à droite), chacune sur la hauteur de deux crans ; un trait
        // relie chaque étiquette au rail central.
        let half = (rect.width() - 6.0) / 2.0;
        let label_height = (2.0 * step_height - 2.0).min(18.0);
        let rail = rect.center().x;
        painter.line_segment(
            [pos2(rail, center_y(KEY_RANGE)), pos2(rail, center_y(-KEY_RANGE))],
            Stroke::new(1.0, theme::OUTLINE),
        );
        // Le cran sélectionné est dessiné en dernier, par-dessus ses voisins.
        let order = (-KEY_RANGE..=KEY_RANGE)
            .filter(|offset| *offset != self.key_offset)
            .chain(std::iter::once(self.key_offset));
        for offset in order {
            let left = offset.rem_euclid(2) == 0;
            let x = if left {
                rect.left() + 3.0 + half / 2.0
            } else {
                rect.right() - 3.0 - half / 2.0
            };
            let y = center_y(offset);
            let row = Rect::from_center_size(pos2(x, y), vec2(half - 2.0, label_height));
            let tick_end = if left { row.right() } else { row.left() };
            painter.line_segment([pos2(tick_end, y), pos2(rail, y)], Stroke::new(1.0, theme::OUTLINE));
            let heard = own.map(|key| key.transposed(offset + tempo_shift));
            let harmony = heard.zip(target).map(|(heard, target)| heard.harmony(target));
            let text = match heard {
                Some(key) => key_label(key),
                None if offset == 0 => "0".to_string(),
                None => format!("{offset:+}"),
            };
            let selected = offset == self.key_offset;
            let (fill, text_color) = match (selected, harmony) {
                (true, _) => (Some(color), Color32::BLACK),
                (false, Some(Harmony::Same)) => (Some(theme::GREEN.gamma_multiply(0.35)), theme::GREEN),
                (false, Some(Harmony::Compatible)) => (Some(theme::GREEN.gamma_multiply(0.12)), theme::GREEN),
                (false, _) if offset == 0 => (Some(theme::PANEL_LIGHT), theme::TEXT),
                (false, _) => (None, theme::TEXT_DIM),
            };
            if let Some(fill) = fill {
                painter.rect_filled(row, 3.0, fill);
            }
            if selected && matches!(harmony, Some(Harmony::Same | Harmony::Compatible)) {
                painter.rect_stroke(row, 3.0, Stroke::new(1.5, theme::GREEN), StrokeKind::Inside);
            }
            let size = (label_height - 3.0).clamp(8.0, 11.0);
            painter.text(
                row.center(),
                Align2::CENTER_CENTER,
                text,
                FontId::proportional(size),
                text_color,
            );
        }

        let mode = own.map_or("", |key| if key.minor { " min" } else { " maj" });
        painter.text(
            label.center(),
            Align2::CENTER_CENTER,
            format!("KEY{mode}"),
            FontId::proportional(10.0),
            theme::TEXT_DIM,
        );
        let shift = self.pitch_shift;
        response.on_hover_text(format!(
            "Tonalité : transposition de {shift:+.1} demi-ton(s)\n\
             Cliquer / glisser sur une note, ou molette ; double-clic : tonalité d'origine.\n\
             En vert : notes qui s'accordent à l'autre platine (plus marqué : même tonalité).\n\
             Avec KEY LOCK coupé, le tempo change aussi la hauteur."
        ));
    }

    /// EQ 3 bandes, une ligne par bande (aigus en haut) : bouton kill à gauche,
    /// potentiomètre à droite.
    /// Lignes d'EQ (kill et potentiomètre) puis celle du filtre (bouton
    /// marche / arrêt et potentiomètre : à gauche passe-bas, à droite passe-haut).
    fn show_eq(&mut self, ui: &mut Ui, rect: Rect, color: Color32) {
        const KILL_WIDTH: f32 = 36.0;
        // Quatre lignes : un peu de marge en bas pour le dernier potentiomètre.
        let rect = rect.with_max_y(rect.bottom() - 4.0);
        let row_height = rect.height() / (EQ_KNOBS.len() + 1) as f32;
        for (i, (label, band)) in EQ_KNOBS.into_iter().enumerate() {
            let row = Rect::from_min_size(
                rect.left_top() + vec2(0.0, i as f32 * row_height),
                vec2(rect.width(), row_height),
            );
            let kill_rect = Rect::from_center_size(
                pos2(row.left() + KILL_WIDTH / 2.0, row.center().y),
                vec2(KILL_WIDTH, 18.0),
            );
            let knob_area = Rect::from_min_max(pos2(row.left() + KILL_WIDTH + 2.0, row.top()), row.max);
            let size = (row_height - 3.0).min(knob_area.width()).min(38.0);
            let knob_rect = Rect::from_center_size(knob_area.center(), vec2(size, size));

            if toggle_button(
                ui,
                kill_rect,
                self.id(label).with("kill"),
                label,
                self.kill[band],
                theme::RED,
            )
            .on_hover_text(format!("Kill : coupe complètement la bande {label}"))
            .clicked()
            {
                self.kill[band] = !self.kill[band];
            }

            let knob_color = if self.kill[band] { theme::TEXT_DIM } else { color };
            let value = if self.kill[band] {
                "KILL".to_string()
            } else {
                format_db(knob_to_gain(self.eq[band]))
            };
            knob(ui, knob_rect, self.id(label), &mut self.eq[band], 0.5, knob_color).on_hover_text(format!(
                "{label} : {value}\nGlisser verticalement (Maj : réglage fin)\nDouble-clic : 0 dB"
            ));
        }

        let row = Rect::from_min_size(
            rect.left_top() + vec2(0.0, EQ_KNOBS.len() as f32 * row_height),
            vec2(rect.width(), row_height),
        );
        let button = Rect::from_center_size(
            pos2(row.left() + KILL_WIDTH / 2.0, row.center().y),
            vec2(KILL_WIDTH, 18.0),
        );
        let knob_area = Rect::from_min_max(pos2(row.left() + KILL_WIDTH + 2.0, row.top()), row.max);
        let size = (row_height - 3.0).min(knob_area.width()).min(38.0);
        let knob_rect = Rect::from_center_size(knob_area.center(), vec2(size, size));
        if toggle_button(ui, button, self.id("filter_on"), "FLT", self.filter_on, color)
            .on_hover_text("Filtre : marche / arrêt (le bouton garde sa position)")
            .clicked()
        {
            self.filter_on = !self.filter_on;
        }
        let knob_color = if self.filter_on { color } else { theme::TEXT_DIM };
        let state = if self.filter_on {
            crate::transitions::filter_label((self.filter - 0.5) * 2.0)
        } else {
            "éteint".to_string()
        };
        let response = knob(ui, knob_rect, self.id("filter"), &mut self.filter, 0.5, knob_color).on_hover_text(format!(
            "Filtre : {state}\nÀ gauche : passe-bas · à droite : passe-haut\nGlisser verticalement · double-clic : neutre"
        ));
        // Tourner le bouton allume le filtre.
        if response.changed() && (self.filter - 0.5).abs() > 0.01 {
            self.filter_on = true;
        }
    }

    /// Stems : bouton de séparation (ou avancement) en haut, puis pour chaque
    /// stem un fader vertical et, dessous, son bouton de coupure.
    /// Retourne `true` si l'utilisateur demande la séparation.
    fn show_stems(&mut self, ui: &mut Ui, mut rect: Rect, time: f64) -> bool {
        let color = self.color();
        let loaded = self.current.is_some();
        let mut separate = false;

        let status = take_top(&mut rect, 18.0);
        take_top(&mut rect, 4.0);
        let ready = match self.stem_state() {
            None => {
                let response = toggle_button(ui, status, self.id("separate"), "SÉPARER", false, color).on_hover_text(
                    "Sépare le morceau en pistes (voix, batterie, basse, instruments)\n\
                         avec le modèle d'IA Demucs, pour régler le volume de chacune.\n\
                         Le résultat est gardé en cache.",
                );
                separate = response.clicked() && loaded;
                false
            }
            Some(StemState::Working(fraction)) => {
                let painter = ui.painter();
                painter.rect_filled(status, 4.0, theme::WAVE_BACKGROUND);
                // Avancement inconnu au début (chargement du modèle) : pulsation.
                let shown = if *fraction > 0.0 {
                    *fraction
                } else {
                    (time * 1.5).fract() as f32 * 0.15
                };
                let mut filled = status;
                filled.set_width(status.width() * shown.clamp(0.0, 1.0));
                painter.rect_filled(filled, 4.0, color.gamma_multiply(0.5));
                painter.rect_stroke(status, 4.0, Stroke::new(1.0, theme::OUTLINE), StrokeKind::Inside);
                painter.text(
                    status.center(),
                    Align2::CENTER_CENTER,
                    format!("{:.0} %", fraction * 100.0),
                    FontId::monospace(10.0),
                    theme::TEXT,
                );
                ui.interact(status, self.id("separate"), Sense::hover())
                    .on_hover_text("Séparation en cours (Demucs)…");
                false
            }
            Some(StemState::Ready) => {
                ui.painter().text(
                    status.center(),
                    Align2::CENTER_CENTER,
                    "STEMS",
                    FontId::proportional(11.0),
                    color,
                );
                true
            }
            Some(StemState::Failed(error)) => {
                let error = error.clone();
                let response = toggle_button(ui, status, self.id("separate"), "ÉCHEC", true, theme::RED)
                    .on_hover_text(format!("{error}\nClic : réessayer"));
                separate = response.clicked();
                false
            }
        };

        const GAP: f32 = 4.0;
        let width = ((rect.width() - GAP * (NUM_STEMS - 1) as f32) / NUM_STEMS as f32).max(0.0);
        for (stem, (label, help)) in STEMS.into_iter().enumerate() {
            let mut column = Rect::from_min_size(
                rect.left_top() + vec2(stem as f32 * (width + GAP), 0.0),
                vec2(width, rect.height()),
            );
            let button = take_bottom(&mut column, 18.0);
            take_bottom(&mut column, 4.0);
            let audible = !self.stem_mute[stem];
            let tint = if ready { color } else { theme::TEXT_DIM };
            fader(
                ui,
                column,
                self.id("stem_level").with(stem),
                &mut self.stem_levels[stem],
                1.0,
                Orientation::Vertical,
                tint,
            )
            .on_hover_text(format!("Volume : {help}\nDouble-clic : volume d'origine"));
            if toggle_button(ui, button, self.id("stem").with(stem), label, ready && audible, tint)
                .on_hover_text(format!("{help}\nClic : couper / rétablir"))
                .clicked()
            {
                self.stem_mute[stem] = !self.stem_mute[stem];
            }
        }
        separate
    }
}

/// Résultat d'un bouton SYNC / MATCH, au relâchement ou à l'appui long.
enum Press {
    None,
    /// Clic court relâché sur le bouton : action unique.
    Click,
    /// Appui maintenu [`HOLD_SECS`] : bascule du mode permanent.
    Hold,
}

/// Bouton à double usage (SYNC, MATCH) : `press` garde l'appui en cours,
/// (début, mode déjà basculé). Allumé tant que le mode est actif ou pendant
/// un appui qui n'a pas encore basculé.
#[allow(clippy::too_many_arguments)]
fn hold_button(
    ui: &mut Ui,
    rect: Rect,
    id: Id,
    label: &str,
    active: bool,
    color: Color32,
    help: &str,
    press: &mut Option<(f64, bool)>,
    time: f64,
) -> Press {
    let holding = press.is_some_and(|(_, toggled)| !toggled);
    let response = toggle_button(ui, rect, id, label, active || holding, color).on_hover_text(help);
    let down = response.is_pointer_button_down_on();
    match press {
        None if down => {
            *press = Some((time, false));
            Press::None
        }
        Some((start, toggled)) if down => {
            // Le seuil doit être détecté même si la souris ne bouge pas.
            ui.ctx().request_repaint();
            if !*toggled && time - *start >= HOLD_SECS {
                *toggled = true;
                Press::Hold
            } else {
                Press::None
            }
        }
        Some((_, toggled)) => {
            let click = !*toggled && response.hovered();
            *press = None;
            if click {
                ui.ctx().request_repaint();
                Press::Click
            } else {
                Press::None
            }
        }
        None => Press::None,
    }
}

/// Étiquette courte d'une tonalité sur le slider : note, et « m » si mineure.
pub(super) fn key_label(key: Key) -> String {
    let name = key.short_name();
    let note = name.split(' ').next().unwrap_or_default();
    if key.minor {
        format!("{note}m")
    } else {
        note.to_string()
    }
}

/// Temps de la grille le plus proche de `frame` (inchangé sans tempo détecté).
pub fn snap_to_beat(analysis: &Analysis, sample_rate: f64, frame: f64) -> f64 {
    let Some(bpm) = analysis.bpm.filter(|bpm| *bpm > 0.0) else {
        return frame;
    };
    let period = 60.0 / bpm as f64 * sample_rate;
    let anchor = analysis.beat_offset as f64 * sample_rate;
    let snapped = anchor + ((frame - anchor) / period).round() * period;
    // Avant le premier temps du morceau : on prend le premier.
    if snapped < 0.0 { snapped + period } else { snapped }
}

/// `mm:ss.d`
pub fn format_time(seconds: f64) -> String {
    let tenths = (seconds.max(0.0) * 10.0).floor() as u64;
    format!("{:02}:{:02}.{}", tenths / 600, tenths / 10 % 60, tenths % 10)
}

/// Gain linéaire → texte en dB (`-∞` pour un gain nul).
pub fn format_db(gain: f32) -> String {
    let db = gain_to_db(gain);
    if db == f32::NEG_INFINITY {
        "-∞ dB".to_string()
    } else {
        format!("{db:+.1} dB")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snap_to_nearest_beat() {
        // 120 BPM à 1000 Hz : un temps toutes les 500 frames, le premier à 125.
        let analysis = Analysis {
            bpm: Some(120.0),
            beat_offset: 0.125,
            key: None,
            energy: None,
        };
        assert_eq!(snap_to_beat(&analysis, 1000.0, 360.0), 125.0);
        assert_eq!(snap_to_beat(&analysis, 1000.0, 380.0), 625.0);
        assert_eq!(snap_to_beat(&analysis, 1000.0, 2125.0), 2125.0);
        // Plus près d'un temps « négatif » (avant le début) : premier temps.
        let late = Analysis {
            beat_offset: 0.375,
            ..analysis
        };
        assert_eq!(snap_to_beat(&late, 1000.0, 10.0), 375.0);
        // Sans tempo : position inchangée.
        assert_eq!(snap_to_beat(&Analysis::default(), 1000.0, 123.0), 123.0);
    }
}
