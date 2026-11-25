//! Moteur de mixage temps réel.
//!
//! Chaîne de signal par platine :
//! buffer (mix, ou stems pondérés par leur volume ; boucle éventuelle) →
//! (tempo / key lock / transposition) → EQ → filtre → gain de canal →
//! crossfader → somme → master → limiteur doux → sortie.
//!
//! Communication avec l'interface :
//! * UI → audio : commandes ponctuelles par canal sans verrou ([`Command`]) et
//!   paramètres continus par atomiques ([`Shared`]) ;
//! * audio → UI : position, état de lecture et niveaux par atomiques.
//!
//! Le thread audio n'alloue pas, ne libère pas et ne prend aucun verrou : les
//! morceaux remplacés sont renvoyés à l'UI, qui se charge de les libérer.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::Relaxed};

use crossbeam_channel::{Receiver, Sender, bounded};

use super::dsp::{DjFilter, Smoothed, ThreeBandEq, crossfader_gains, hermite, soft_limit};
use super::stretch::Stretcher;

pub const NUM_DECKS: usize = 2;

/// Plage de tempo acceptée par le moteur (vitesse relative).
pub const TEMPO_RANGE: std::ops::RangeInclusive<f32> = 0.5..=2.0;
/// Transposition maximale, en demi-tons.
pub const MAX_PITCH_SHIFT: f32 = 12.0;

/// Durée des fondus de lecture/pause.
const FADE_MS: f32 = 6.0;
/// Durée de chaque moitié du « creux » qui masque un saut de position.
const DIP_MS: f32 = 3.0;
/// Constante de temps du lissage des gains (volume, EQ, crossfader, master).
const SMOOTH_MS: f32 = 6.0;
/// Constante de temps du lissage du tempo et de la transposition.
const RATE_SMOOTH_MS: f32 = 15.0;
/// Constante de temps du lissage du filtre (assez lente pour un balayage doux).
const FILTER_SMOOTH_MS: f32 = 20.0;
/// Fondu enchaîné entre la fin et le début d'une boucle (masque le raccord).
const LOOP_XFADE_MS: f32 = 4.0;

// ---------------------------------------------------------------------------
// Données partagées
// ---------------------------------------------------------------------------

/// Morceau entièrement décodé en mémoire : stéréo entrelacé, déjà à la
/// fréquence d'échantillonnage du moteur.
pub struct TrackData {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
}

impl TrackData {
    pub fn frames(&self) -> usize {
        self.samples.len() / 2
    }

    #[inline]
    fn frame(&self, index: usize) -> [f32; 2] {
        match self.samples.get(index * 2..index * 2 + 2) {
            Some(s) => [s[0], s[1]],
            None => [0.0; 2],
        }
    }

    /// Frame à un indice quelconque ; silence hors du morceau.
    #[inline]
    pub(super) fn frame_at(&self, index: i64) -> [f32; 2] {
        if index < 0 {
            [0.0; 2]
        } else {
            self.frame(index as usize)
        }
    }
}

/// Nombre de stems stockés : voix, batterie, basse. Le quatrième (« instruments »,
/// tout le reste) est déduit : mix − somme des trois autres.
pub const STORED_STEMS: usize = 3;
/// Nombre de volumes de stems réglables : voix, batterie, basse, instruments.
pub const NUM_STEMS: usize = STORED_STEMS + 1;

/// Pistes séparées d'un morceau, alignées sur son mix (même longueur, même
/// fréquence), en stéréo entrelacé sur 16 bits pour économiser la mémoire.
pub struct Stems {
    samples: [Vec<i16>; STORED_STEMS],
}

impl Stems {
    /// `samples` : un buffer stéréo entrelacé `f32` par stem (voix, batterie,
    /// basse), ajusté à `frames` frames.
    pub fn new(samples: [Vec<f32>; STORED_STEMS], frames: usize) -> Self {
        Self {
            samples: samples.map(|stem| {
                let mut out: Vec<i16> = stem
                    .iter()
                    .take(frames * 2)
                    .map(|&x| (x.clamp(-1.0, 1.0) * 32767.0).round() as i16)
                    .collect();
                out.resize(frames * 2, 0);
                out
            }),
        }
    }

    #[inline]
    fn frame(&self, stem: usize, index: usize) -> [f32; 2] {
        const SCALE: f32 = 1.0 / 32767.0;
        match self.samples[stem].get(index * 2..index * 2 + 2) {
            Some(s) => [s[0] as f32 * SCALE, s[1] as f32 * SCALE],
            None => [0.0; 2],
        }
    }
}

/// Ce que lit une platine : le mix du morceau, ou, si ses stems sont chargés,
/// la somme des stems pondérés par leur volume.
pub(super) struct Source<'a> {
    track: &'a TrackData,
    stems: Option<&'a Stems>,
    /// `[instruments, voix − instruments, batterie − instruments, basse − instruments]` :
    /// mix × instruments + Σ stem × (volume − instruments). Volumes tous à 1
    /// → mix exact, sans aucun artefact de séparation.
    coeffs: [f32; NUM_STEMS],
}

impl<'a> Source<'a> {
    /// `gains` : volumes `[voix, batterie, basse, instruments]`.
    pub(super) fn new(track: &'a TrackData, stems: Option<&'a Stems>, gains: [f32; NUM_STEMS]) -> Self {
        let rest = gains[STORED_STEMS];
        Self {
            track,
            stems,
            coeffs: [rest, gains[0] - rest, gains[1] - rest, gains[2] - rest],
        }
    }

    /// Frame à un indice quelconque ; silence hors du morceau.
    #[inline]
    pub(super) fn frame_at(&self, index: i64) -> [f32; 2] {
        let mix = self.track.frame_at(index);
        let Some(stems) = self.stems else { return mix };
        if index < 0 {
            return mix;
        }
        let mut out = [mix[0] * self.coeffs[0], mix[1] * self.coeffs[0]];
        for stem in 0..STORED_STEMS {
            let c = self.coeffs[stem + 1];
            if c != 0.0 {
                let f = stems.frame(stem, index as usize);
                out[0] += c * f[0];
                out[1] += c * f[1];
            }
        }
        out
    }

    /// Lecture à une position fractionnaire (interpolation cubique).
    #[inline]
    fn interpolate(&self, pos: f64) -> [f32; 2] {
        let base = pos.floor();
        let t = (pos - base) as f32;
        let i = base as i64;
        if t == 0.0 {
            return self.frame_at(i);
        }
        let (a, b, c, d) = (
            self.frame_at(i - 1),
            self.frame_at(i),
            self.frame_at(i + 1),
            self.frame_at(i + 2),
        );
        [
            hermite([a[0], b[0], c[0], d[0]], t),
            hermite([a[1], b[1], c[1], d[1]], t),
        ]
    }
}

/// Grille de temps d'un morceau, en frames à la fréquence du moteur.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BeatGrid {
    /// Position d'un temps de référence.
    pub anchor: f64,
    /// Durée d'un temps.
    pub period: f64,
}

impl BeatGrid {
    /// Position dans le temps courant, de 0 (sur le temps) à 1 (temps suivant).
    fn phase(&self, pos: f64) -> f64 {
        ((pos - self.anchor) / self.period).rem_euclid(1.0)
    }
}

/// `f32` atomique (stocké sous forme de bits dans un `AtomicU32`).
#[derive(Default)]
pub struct AtomicF32(AtomicU32);

impl AtomicF32 {
    pub fn new(value: f32) -> Self {
        Self(AtomicU32::new(value.to_bits()))
    }

    pub fn load(&self) -> f32 {
        f32::from_bits(self.0.load(Relaxed))
    }

    pub fn store(&self, value: f32) {
        self.0.store(value.to_bits(), Relaxed);
    }

    /// Conserve le maximum (valeurs positives uniquement : pour des flottants
    /// positifs, l'ordre des bits est celui des valeurs).
    fn raise(&self, value: f32) {
        self.0.fetch_max(value.max(0.0).to_bits(), Relaxed);
    }

    /// Lit la valeur et la remet à zéro (lecture d'un pic par l'UI).
    pub fn take(&self) -> f32 {
        f32::from_bits(self.0.swap(0, Relaxed))
    }
}

/// État d'une platine partagé entre l'UI et le thread audio.
pub struct DeckShared {
    // --- audio → UI ---
    /// Position de lecture, en frames.
    pub position: AtomicU64,
    /// Point de cue, en frames.
    pub cue: AtomicU64,
    pub playing: AtomicBool,
    /// Lecture demandée, en attente du prochain temps de l'autre platine (magnet).
    pub armed: AtomicBool,
    /// Identifiant du morceau actuellement chargé (0 = aucun).
    pub loaded_id: AtomicU64,
    /// Dernier identifiant de chargement refusé (platine en lecture).
    pub rejected_id: AtomicU64,
    /// Pics gauche/droite post-EQ, post-fader depuis la dernière lecture par l'UI.
    pub peak: [AtomicF32; 2],
    // --- UI → audio ---
    /// Gain de canal linéaire.
    pub volume: AtomicF32,
    /// Gains linéaires `[low, mid, high]`.
    pub eq: [AtomicF32; 3],
    pub kill: [AtomicBool; 3],
    /// Vitesse de lecture relative (1 = tempo d'origine).
    pub tempo: AtomicF32,
    /// Si vrai, la hauteur ne suit pas le tempo.
    pub key_lock: AtomicBool,
    /// Transposition en demi-tons (s'ajoute à l'effet éventuel du tempo).
    pub pitch_shift: AtomicF32,
    /// Si vrai, la lecture démarre calée sur les temps de l'autre platine.
    pub magnet: AtomicBool,
    /// Volumes linéaires des stems `[voix, batterie, basse, instruments]`
    /// (sans effet tant que les stems ne sont pas chargés).
    pub stems: [AtomicF32; NUM_STEMS],
    /// Filtre DJ, de −1 (passe-bas) à 1 (passe-haut) ; 0 = contourné.
    pub filter: AtomicF32,
    // --- audio → UI ---
    /// Boucle en cours : début et longueur en frames (longueur 0 = aucune).
    pub loop_start: AtomicU64,
    pub loop_len: AtomicU64,
}

impl DeckShared {
    fn new() -> Self {
        Self {
            position: AtomicU64::new(0),
            cue: AtomicU64::new(0),
            playing: AtomicBool::new(false),
            armed: AtomicBool::new(false),
            loaded_id: AtomicU64::new(0),
            rejected_id: AtomicU64::new(0),
            peak: Default::default(),
            volume: AtomicF32::new(1.0),
            eq: [AtomicF32::new(1.0), AtomicF32::new(1.0), AtomicF32::new(1.0)],
            kill: Default::default(),
            tempo: AtomicF32::new(1.0),
            key_lock: AtomicBool::new(false),
            pitch_shift: AtomicF32::new(0.0),
            magnet: AtomicBool::new(false),
            stems: std::array::from_fn(|_| AtomicF32::new(1.0)),
            filter: AtomicF32::new(0.0),
            loop_start: AtomicU64::new(0),
            loop_len: AtomicU64::new(0),
        }
    }
}

/// État global partagé entre l'UI et le thread audio.
pub struct Shared {
    pub sample_rate: u32,
    pub decks: [DeckShared; NUM_DECKS],
    /// 0 = tout A, 1 = tout B.
    pub crossfader: AtomicF32,
    /// Gain master linéaire.
    pub master: AtomicF32,
    /// Pics gauche/droite en sortie (après limiteur).
    pub master_peak: [AtomicF32; 2],
}

/// Commandes ponctuelles envoyées par l'UI au thread audio.
pub enum Command {
    /// Charge un morceau (refusé si la platine est en lecture).
    Load {
        deck: usize,
        id: u64,
        track: Arc<TrackData>,
        grid: Option<BeatGrid>,
    },
    /// Décharge la platine (ignoré si elle est en lecture).
    Eject(usize),
    TogglePlay(usize),
    /// Déplace la lecture à une position absolue (frames).
    Seek {
        deck: usize,
        frame: u64,
    },
    /// Déplace la lecture relativement à la position courante (frames).
    SeekRelative {
        deck: usize,
        frames: i64,
    },
    /// Bouton Cue : en lecture, retour au point de cue et arrêt ;
    /// à l'arrêt, définit le point de cue à la position courante.
    Cue(usize),
    /// Définit le point de cue à la position courante.
    CueSet(usize),
    /// Retourne au point de cue et s'arrête, quel que soit l'état.
    CueGotoAndStop(usize),
    /// Stems du morceau `id` (ignorés si ce n'est plus celui de la platine).
    SetStems {
        deck: usize,
        id: u64,
        stems: Arc<Stems>,
    },
    /// Boucle de `beats` temps, calée sur la grille : commence au temps
    /// courant (ou garde le début de la boucle en cours, pour la raccourcir).
    /// `None` : sortie de boucle.
    SetLoop {
        deck: usize,
        beats: Option<f32>,
    },
}

/// Données dont le moteur ne veut plus, renvoyées à l'UI pour être libérées
/// hors du thread audio.
#[allow(dead_code)] // seul le `Drop` des valeurs compte
pub enum Garbage {
    Track(Arc<TrackData>),
    Stems(Arc<Stems>),
}

/// Poignée côté UI pour piloter le moteur.
pub struct EngineHandle {
    pub shared: Arc<Shared>,
    commands: Sender<Command>,
    garbage: Receiver<Garbage>,
}

impl EngineHandle {
    pub fn send(&self, command: Command) {
        // Le canal est largement dimensionné ; s'il est plein (moteur à l'arrêt),
        // la commande est simplement abandonnée.
        let _ = self.commands.try_send(command);
    }

    /// Libère, hors du thread audio, les morceaux dont le moteur ne veut plus.
    pub fn collect_garbage(&self) {
        while self.garbage.try_recv().is_ok() {}
    }
}

// ---------------------------------------------------------------------------
// Platine (côté audio)
// ---------------------------------------------------------------------------

/// Cibles des paramètres d'une platine pour le bloc en cours.
#[derive(Clone, Copy)]
struct DeckTargets {
    eq: [f32; 3],
    volume: f32,
    crossfade: f32,
    tempo: f32,
    key_lock: bool,
    /// Facteur de hauteur demandé (2^(demi-tons / 12)).
    pitch_factor: f32,
    stems: [f32; NUM_STEMS],
    filter: f32,
}

/// Durées exprimées en pas par frame, calculées une fois.
#[derive(Clone, Copy)]
struct Steps {
    fade: f32,
    dip: f32,
    /// Durée du fondu de raccord des boucles, en frames.
    loop_xfade: f64,
}

/// Action à effectuer au fond d'un « creux » (fondu sortant très court).
#[derive(Clone, Copy)]
enum Dip {
    Seek(f64),
    /// Retour à la lecture directe (fin du key lock / de la transposition).
    ToDirect,
    /// Retour au début de la boucle (lecture étirée).
    LoopBack,
}

struct Deck {
    track: Option<Arc<TrackData>>,
    stems: Option<Arc<Stems>>,
    stem_gain: [Smoothed; NUM_STEMS],
    grid: Option<BeatGrid>,
    /// Position de lecture dans le morceau, en frames.
    pos: f64,
    cue: f64,
    /// État demandé ; le son suit avec un court fondu (`gain`).
    playing: bool,
    /// Frames restant à attendre avant un départ calé (magnet) ; 0 = pas d'attente.
    wait: u32,
    /// Enveloppe de lecture/pause, 0..1.
    gain: f32,
    /// Saut à effectuer une fois le fondu de sortie terminé.
    pending_jump: Option<f64>,
    dip: Option<Dip>,
    dip_gain: f32,
    /// Vrai quand la lecture passe par l'étirement temporel.
    stretching: bool,
    stretcher: Stretcher,
    rate: Smoothed,
    pitch_factor: Smoothed,
    eq: [ThreeBandEq; 2],
    eq_gain: [Smoothed; 3],
    filter: DjFilter,
    filter_amount: Smoothed,
    /// Boucle en cours : début et fin, en frames.
    loop_range: Option<(f64, f64)>,
    volume: Smoothed,
    crossfade: Smoothed,
    peak: [f32; 2],
}

impl Deck {
    fn new(sample_rate: f32) -> Self {
        let smooth = |initial| Smoothed::new(sample_rate, SMOOTH_MS, initial);
        Self {
            track: None,
            stems: None,
            stem_gain: [smooth(1.0); NUM_STEMS],
            grid: None,
            pos: 0.0,
            cue: 0.0,
            playing: false,
            wait: 0,
            gain: 0.0,
            pending_jump: None,
            dip: None,
            dip_gain: 1.0,
            stretching: false,
            stretcher: Stretcher::new(sample_rate),
            rate: Smoothed::new(sample_rate, RATE_SMOOTH_MS, 1.0),
            pitch_factor: Smoothed::new(sample_rate, RATE_SMOOTH_MS, 1.0),
            eq: [ThreeBandEq::new(sample_rate as f64); 2],
            eq_gain: [smooth(1.0); 3],
            filter: DjFilter::new(sample_rate as f64),
            filter_amount: Smoothed::new(sample_rate, FILTER_SMOOTH_MS, 0.0),
            loop_range: None,
            volume: smooth(1.0),
            crossfade: smooth(std::f32::consts::FRAC_1_SQRT_2),
            peak: [0.0; 2],
        }
    }

    fn frames(&self) -> f64 {
        self.track.as_ref().map_or(0.0, |t| t.frames() as f64)
    }

    /// Vrai tant que la platine produit du son (lecture ou fondu de sortie).
    fn is_active(&self) -> bool {
        self.track.is_some() && (self.playing || self.gain > 0.0)
    }

    /// Platine inaudible : les paramètres peuvent sauter à leur cible sans clic.
    fn snap(&mut self, t: &DeckTargets) {
        for (smoother, target) in self.eq_gain.iter_mut().zip(t.eq) {
            smoother.snap(target);
        }
        for (smoother, target) in self.stem_gain.iter_mut().zip(t.stems) {
            smoother.snap(target);
        }
        self.volume.snap(t.volume);
        self.crossfade.snap(t.crossfade);
        self.filter_amount.snap(t.filter);
        self.rate.snap(t.tempo);
        self.pitch_factor.snap(t.pitch_factor);
    }

    /// Place la lecture à `target` immédiatement (platine inaudible ou fond d'un creux).
    fn jump(&mut self, target: f64) {
        self.pos = target.clamp(0.0, self.frames());
        if self.stretching {
            self.stretcher.reset(self.pos);
        }
    }

    /// Déplacement demandé par l'utilisateur : quitte la boucle s'il en sort.
    fn seek(&mut self, target: f64) {
        if self
            .loop_range
            .is_some_and(|(start, end)| !(start..end).contains(&target))
        {
            self.loop_range = None;
        }
        self.move_to(target);
    }

    fn move_to(&mut self, target: f64) {
        let target = target.clamp(0.0, self.frames());
        self.pending_jump = None;
        if self.is_active() {
            // Le saut se fera au fond d'un creux de quelques millisecondes.
            self.dip = Some(Dip::Seek(target));
        } else {
            self.jump(target);
        }
    }

    /// `delay` : attente (en frames) avant un départ calé sur l'autre platine.
    fn toggle_play(&mut self, delay: Option<u32>) {
        if self.track.is_none() {
            return;
        }
        if self.playing {
            self.playing = false;
        } else if self.wait > 0 {
            self.wait = 0; // second appui pendant l'attente : annulation
        } else {
            if let Some(target) = self.pending_jump.take() {
                self.jump(target);
            }
            if self.pos < self.frames() {
                match delay {
                    Some(frames) if frames > 0 => self.wait = frames,
                    _ => self.playing = true,
                }
            }
        }
    }

    /// Boucle de `beats` temps (voir [`Command::SetLoop`]).
    fn set_loop(&mut self, beats: Option<f32>) {
        let Some(beats) = beats.filter(|beats| *beats > 0.0) else {
            self.loop_range = None;
            return;
        };
        let Some(grid) = self.grid else { return };
        let start = match self.loop_range {
            Some((start, _)) => start,
            None => {
                let pos = self.pending_jump.unwrap_or(self.pos);
                let mut start = grid.anchor + ((pos - grid.anchor) / grid.period + 1e-6).floor() * grid.period;
                while start < 0.0 {
                    start += grid.period;
                }
                start
            }
        };
        let end = (start + beats as f64 * grid.period).min(self.frames());
        if end - start < 64.0 {
            return;
        }
        self.loop_range = Some((start, end));
        // Boucle raccourcie : la lecture revient à la même place dans la nouvelle.
        if self.pos >= end {
            self.move_to(start + (self.pos - start) % (end - start));
        }
    }

    fn goto_cue_and_stop(&mut self) {
        if self
            .loop_range
            .is_some_and(|(start, end)| !(start..end).contains(&self.cue))
        {
            self.loop_range = None;
        }
        self.wait = 0;
        if self.is_active() {
            self.pending_jump = Some(self.cue);
            self.playing = false;
        } else {
            self.jump(self.cue);
        }
    }

    fn cue_button(&mut self) {
        if self.playing {
            self.goto_cue_and_stop();
        } else if !self.is_active() {
            self.cue = self.pos.min(self.frames());
        }
    }

    /// Appelé quand le fondu de sortie est terminé.
    fn finish_stop(&mut self) {
        self.dip = None;
        self.dip_gain = 1.0;
        if let Some(target) = self.pending_jump.take() {
            self.jump(target);
        } else if self.stretching {
            self.stretcher.reset(self.pos);
        }
        for eq in &mut self.eq {
            eq.reset();
        }
        self.filter.reset();
    }

    /// Produit une frame stéréo (déjà multipliée par le gain de crossfader).
    #[inline]
    fn next_frame(&mut self, t: &DeckTargets, steps: Steps) -> [f32; 2] {
        if self.track.is_none() {
            return [0.0; 2];
        }

        // --- Vitesse et hauteur ---
        // rate  : frames de morceau par frame de sortie (tempo) ;
        // pitch : facteur de hauteur ; sans key lock, la hauteur suit le tempo.
        let rate = self.rate.next(t.tempo) as f64;
        let factor = self.pitch_factor.next(t.pitch_factor) as f64;
        let pitch = if t.key_lock { factor } else { rate * factor };
        let want_stretch = t.key_lock || (factor - 1.0).abs() > 1e-4;
        if want_stretch && !self.stretching {
            self.stretcher.reset(self.pos);
            self.stretching = true;
        } else if !want_stretch && self.stretching && self.dip.is_none() {
            self.dip = Some(Dip::ToDirect);
        }

        // --- Creux masquant un saut ---
        if let Some(action) = self.dip {
            self.dip_gain -= steps.dip;
            if self.dip_gain <= 0.0 {
                self.dip_gain = 0.0;
                self.dip = None;
                match action {
                    Dip::Seek(target) => self.jump(target),
                    Dip::ToDirect => self.stretching = want_stretch,
                    Dip::LoopBack => {
                        if let Some((start, end)) = self.loop_range
                            && self.pos >= start + (end - start) * 0.5
                        {
                            self.jump(self.pos - (end - start));
                        }
                    }
                }
            }
        } else if self.dip_gain < 1.0 {
            self.dip_gain = (self.dip_gain + steps.dip).min(1.0);
        }

        // --- Lecture du morceau (mix ou stems pondérés) ---
        let gains: [f32; NUM_STEMS] = std::array::from_fn(|i| self.stem_gain[i].next(t.stems[i]));
        let track = self.track.as_deref().expect("vérifié plus haut");
        let source = Source::new(track, self.stems.as_deref(), gains);
        let frames = track.frames() as f64;
        let sample = if self.stretching {
            let stretch = rate / pitch;
            let sample = self.stretcher.next(&source, stretch, pitch);
            self.pos = self.stretcher.position(stretch);
            // Boucle : retour au début au fond d'un creux, qui commence juste
            // avant la fin pour que le saut tombe dessus.
            if let Some((_, end)) = self.loop_range
                && self.dip.is_none()
                && self.pos >= end - rate / steps.dip as f64
            {
                self.dip = Some(Dip::LoopBack);
            }
            sample
        } else {
            if rate == 1.0 && self.pos.fract() != 0.0 {
                // Tempo d'origine : on se recale sur les échantillons exacts.
                self.pos = self.pos.round();
            }
            let mut sample = source.interpolate(self.pos);
            if let Some((start, end)) = self.loop_range {
                // Juste avant la fin, fondu vers le début : le raccord ne s'entend pas.
                let len = end - start;
                let xfade = steps.loop_xfade.min(len * 0.25);
                if self.pos >= end - xfade && self.pos < end {
                    let w = ((self.pos - (end - xfade)) / xfade) as f32;
                    let head = source.interpolate(self.pos - len);
                    sample = [
                        sample[0] + (head[0] - sample[0]) * w,
                        sample[1] + (head[1] - sample[1]) * w,
                    ];
                }
            }
            self.pos += rate;
            if let Some((start, end)) = self.loop_range
                && self.pos >= end
            {
                self.pos -= end - start;
            }
            sample
        };

        // --- Enveloppe lecture / pause ---
        self.gain = if self.playing {
            (self.gain + steps.fade).min(1.0)
        } else {
            (self.gain - steps.fade).max(0.0)
        };
        let envelope = self.gain * self.dip_gain;

        // Fin du morceau : arrêt.
        if self.pos >= frames {
            self.pos = frames;
            self.playing = false;
            self.gain = 0.0;
        }
        let stopped = !self.playing && self.gain <= 0.0;

        // --- EQ → gain de canal → (mesure) → crossfader ---
        let eq_gains = [
            self.eq_gain[0].next(t.eq[0]),
            self.eq_gain[1].next(t.eq[1]),
            self.eq_gain[2].next(t.eq[2]),
        ];
        let volume = self.volume.next(t.volume);
        let crossfade = self.crossfade.next(t.crossfade);
        let filter = self.filter_amount.next(t.filter);

        let equalized: [f32; 2] = std::array::from_fn(|ch| self.eq[ch].process(sample[ch] * envelope, eq_gains));
        let filtered = self.filter.process(equalized, filter);
        let mut out = [0.0; 2];
        for ch in 0..2 {
            let y = filtered[ch] * volume;
            self.peak[ch] = self.peak[ch].max(y.abs());
            out[ch] = y * crossfade;
        }

        if stopped {
            self.finish_stop();
        }
        out
    }
}

/// Attente (en frames de sortie) pour que `deck` démarre en phase avec les
/// temps de `other` : on attend que `other` atteigne, dans son temps courant,
/// la même position que `deck` dans le sien.
fn magnet_delay(deck: &Deck, other: &Deck) -> Option<u32> {
    if !other.playing {
        return None;
    }
    let (own_grid, other_grid) = (deck.grid?, other.grid?);
    let start = deck.pending_jump.unwrap_or(deck.pos);
    let phase_gap = (own_grid.phase(start) - other_grid.phase(other.pos)).rem_euclid(1.0);
    let other_rate = (other.rate.current() as f64).max(0.01);
    Some((phase_gap * other_grid.period / other_rate).round() as u32)
}

// ---------------------------------------------------------------------------
// Moteur
// ---------------------------------------------------------------------------

/// Moteur de mixage ; vit dans le callback audio.
pub struct Engine {
    shared: Arc<Shared>,
    commands: Receiver<Command>,
    garbage: Sender<Garbage>,
    decks: [Deck; NUM_DECKS],
    master: Smoothed,
    steps: Steps,
}

impl Engine {
    /// Crée le moteur (à déplacer dans le callback audio) et sa poignée côté UI.
    pub fn new(sample_rate: u32) -> (Engine, EngineHandle) {
        let shared = Arc::new(Shared {
            sample_rate,
            decks: [DeckShared::new(), DeckShared::new()],
            crossfader: AtomicF32::new(0.5),
            master: AtomicF32::new(1.0),
            master_peak: Default::default(),
        });
        let (command_tx, command_rx) = bounded(1024);
        let (garbage_tx, garbage_rx) = bounded(16);
        let sr = sample_rate as f32;
        let step = |ms: f32| 1.0 / (ms * 0.001 * sr).max(1.0);
        let engine = Engine {
            shared: shared.clone(),
            commands: command_rx,
            garbage: garbage_tx,
            decks: [Deck::new(sr), Deck::new(sr)],
            master: Smoothed::new(sr, SMOOTH_MS, 1.0),
            steps: Steps {
                fade: step(FADE_MS),
                dip: step(DIP_MS),
                loop_xfade: (LOOP_XFADE_MS * 0.001 * sr).max(1.0) as f64,
            },
        };
        let handle = EngineHandle {
            shared,
            commands: command_tx,
            garbage: garbage_rx,
        };
        (engine, handle)
    }

    /// Remplit `out` (entrelacé, `channels` canaux) avec le mix.
    pub fn process(&mut self, out: &mut [f32], channels: usize) {
        self.handle_commands();

        let targets = self.read_targets();
        let master_target = self.shared.master.load();
        for (deck, t) in self.decks.iter_mut().zip(&targets) {
            if !deck.is_active() {
                deck.snap(t);
            }
        }

        let steps = self.steps;
        let mut master_peak = [0.0f32; 2];

        for frame in out.chunks_exact_mut(channels.max(1)) {
            // Somme des platines.
            let mut mix = [0.0f32; 2];
            for (deck, t) in self.decks.iter_mut().zip(&targets) {
                if deck.wait > 0 {
                    // Départ calé (magnet) : on compte les frames jusqu'au temps.
                    deck.wait -= 1;
                    deck.playing = deck.wait == 0;
                }
                if deck.is_active() {
                    let s = deck.next_frame(t, steps);
                    mix[0] += s[0];
                    mix[1] += s[1];
                }
            }

            // Master → limiteur doux.
            let master = self.master.next(master_target);
            let left = soft_limit(mix[0] * master);
            let right = soft_limit(mix[1] * master);
            master_peak[0] = master_peak[0].max(left.abs());
            master_peak[1] = master_peak[1].max(right.abs());

            match frame {
                [mono] => *mono = 0.5 * (left + right),
                [l, r, rest @ ..] => {
                    *l = left;
                    *r = right;
                    rest.fill(0.0);
                }
                [] => {}
            }
        }

        self.publish(master_peak);
    }

    fn handle_commands(&mut self) {
        while let Ok(command) = self.commands.try_recv() {
            match command {
                Command::Load { deck, id, track, grid } => self.load(deck, id, track, grid),
                Command::Eject(deck) => {
                    let (d, shared) = (&mut self.decks[deck], &self.shared.decks[deck]);
                    if !d.is_active() {
                        if let Some(old) = d.track.take() {
                            let _ = self.garbage.try_send(Garbage::Track(old));
                        }
                        if let Some(old) = d.stems.take() {
                            let _ = self.garbage.try_send(Garbage::Stems(old));
                        }
                        d.grid = None;
                        d.loop_range = None;
                        d.pos = 0.0;
                        d.cue = 0.0;
                        d.wait = 0;
                        shared.loaded_id.store(0, Relaxed);
                    }
                }
                Command::TogglePlay(deck) => {
                    let delay = if self.shared.decks[deck].magnet.load(Relaxed) {
                        magnet_delay(&self.decks[deck], &self.decks[NUM_DECKS - 1 - deck])
                    } else {
                        None
                    };
                    self.decks[deck].toggle_play(delay);
                }
                Command::Seek { deck, frame } => self.decks[deck].seek(frame as f64),
                Command::SeekRelative { deck, frames } => {
                    let d = &mut self.decks[deck];
                    d.seek(d.pos + frames as f64);
                }
                Command::Cue(deck) => self.decks[deck].cue_button(),
                Command::CueSet(deck) => {
                    let d = &mut self.decks[deck];
                    d.cue = d.pos.min(d.frames());
                }
                Command::CueGotoAndStop(deck) => self.decks[deck].goto_cue_and_stop(),
                Command::SetLoop { deck, beats } => self.decks[deck].set_loop(beats),
                Command::SetStems { deck, id, stems } => {
                    let (d, shared) = (&mut self.decks[deck], &self.shared.decks[deck]);
                    let matches = d.track.is_some() && shared.loaded_id.load(Relaxed) == id;
                    let old = if matches { d.stems.replace(stems) } else { Some(stems) };
                    if let Some(old) = old {
                        let _ = self.garbage.try_send(Garbage::Stems(old));
                    }
                }
            }
        }
    }

    fn load(&mut self, deck: usize, id: u64, track: Arc<TrackData>, grid: Option<BeatGrid>) {
        let (d, shared) = (&mut self.decks[deck], &self.shared.decks[deck]);
        if d.is_active() {
            // Interdit de charger sur une platine en cours de lecture.
            shared.rejected_id.store(id, Relaxed);
            let _ = self.garbage.try_send(Garbage::Track(track));
            return;
        }
        if let Some(old) = d.track.replace(track) {
            let _ = self.garbage.try_send(Garbage::Track(old));
        }
        if let Some(old) = d.stems.take() {
            let _ = self.garbage.try_send(Garbage::Stems(old));
        }
        d.grid = grid.filter(|g| g.period > 0.0);
        d.loop_range = None;
        d.cue = 0.0;
        d.wait = 0;
        d.pending_jump = None;
        d.jump(0.0);
        shared.loaded_id.store(id, Relaxed);
    }

    fn read_targets(&self) -> [DeckTargets; NUM_DECKS] {
        let (xf_a, xf_b) = crossfader_gains(self.shared.crossfader.load());
        let crossfade = [xf_a, xf_b];
        std::array::from_fn(|i| {
            let s = &self.shared.decks[i];
            let shift = s.pitch_shift.load().clamp(-MAX_PITCH_SHIFT, MAX_PITCH_SHIFT);
            DeckTargets {
                eq: std::array::from_fn(|band| {
                    if s.kill[band].load(Relaxed) {
                        0.0
                    } else {
                        s.eq[band].load()
                    }
                }),
                volume: s.volume.load(),
                crossfade: crossfade[i],
                tempo: s.tempo.load().clamp(*TEMPO_RANGE.start(), *TEMPO_RANGE.end()),
                key_lock: s.key_lock.load(Relaxed),
                pitch_factor: 2f32.powf(shift / 12.0),
                stems: std::array::from_fn(|stem| s.stems[stem].load().clamp(0.0, 2.0)),
                filter: s.filter.load().clamp(-1.0, 1.0),
            }
        })
    }

    fn publish(&mut self, master_peak: [f32; 2]) {
        for (deck, shared) in self.decks.iter_mut().zip(&self.shared.decks) {
            shared.position.store(deck.pos.max(0.0).round() as u64, Relaxed);
            shared.cue.store(deck.cue.round() as u64, Relaxed);
            shared.playing.store(deck.playing, Relaxed);
            shared.armed.store(deck.wait > 0, Relaxed);
            let (loop_start, loop_len) = deck.loop_range.map_or((0, 0), |(start, end)| {
                (start.round() as u64, (end - start).round() as u64)
            });
            shared.loop_start.store(loop_start, Relaxed);
            shared.loop_len.store(loop_len, Relaxed);
            for (shared_peak, peak) in shared.peak.iter().zip(&mut deck.peak) {
                shared_peak.raise(*peak);
                *peak = 0.0;
            }
        }
        for (shared_peak, peak) in self.shared.master_peak.iter().zip(master_peak) {
            shared_peak.raise(peak);
        }
    }
}

/// Accès au rendu des stems pour les tests d'autres modules.
#[cfg(test)]
pub mod tests_support {
    use super::*;

    /// Les `frames` premières frames (entrelacées) de `track` avec ces volumes de stems.
    pub fn source_frames(track: &TrackData, stems: &Stems, gains: [f32; NUM_STEMS], frames: usize) -> Vec<f32> {
        let source = Source::new(track, Some(stems), gains);
        (0..frames as i64).flat_map(|i| source.frame_at(i)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 48_000;

    fn sine_track(seconds: f32, amplitude: f32) -> Arc<TrackData> {
        let frames = (seconds * SR as f32) as usize;
        let mut samples = Vec::with_capacity(frames * 2);
        for n in 0..frames {
            let v = amplitude * (2.0 * std::f32::consts::PI * 440.0 * n as f32 / SR as f32).sin();
            samples.extend([v, v]);
        }
        Arc::new(TrackData {
            samples,
            sample_rate: SR,
        })
    }

    fn load(handle: &EngineHandle, deck: usize, id: u64, track: Arc<TrackData>) {
        handle.send(Command::Load {
            deck,
            id,
            track,
            grid: None,
        });
    }

    fn render(engine: &mut Engine, frames: usize) -> Vec<f32> {
        let mut out = vec![0.0; frames * 2];
        for chunk in out.chunks_mut(512 * 2) {
            engine.process(chunk, 2);
        }
        out
    }

    fn left(buffer: &[f32]) -> Vec<f32> {
        buffer.as_chunks::<2>().0.iter().map(|[l, _]| *l).collect()
    }

    /// Plus grand saut entre deux échantillons consécutifs du canal gauche.
    fn max_step(buffer: &[f32]) -> f32 {
        left(buffer).windows(2).fold(0.0, |m, w| m.max((w[1] - w[0]).abs()))
    }

    /// Fréquence estimée par comptage des passages par zéro montants.
    fn frequency(buffer: &[f32]) -> f32 {
        let signal = left(buffer);
        let crossings = signal.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count();
        crossings as f32 * SR as f32 / signal.len() as f32
    }

    fn position(handle: &EngineHandle, deck: usize) -> i64 {
        handle.shared.decks[deck].position.load(Relaxed) as i64
    }

    #[test]
    fn silent_until_play_then_audible() {
        let (mut engine, handle) = Engine::new(SR);
        load(&handle, 0, 1, sine_track(2.0, 0.5));
        handle.shared.crossfader.store(0.0);
        assert!(render(&mut engine, 4800).iter().all(|&s| s == 0.0));
        assert_eq!(handle.shared.decks[0].loaded_id.load(Relaxed), 1);

        handle.send(Command::TogglePlay(0));
        let out = render(&mut engine, 9600);
        assert!(handle.shared.decks[0].playing.load(Relaxed));
        assert!(out.iter().all(|s| s.is_finite() && s.abs() <= 1.0));
        let peak = out[9600..].iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!((peak - 0.5).abs() < 0.02, "pic = {peak}");
        assert!(position(&handle, 0) >= 9600);
    }

    #[test]
    fn load_is_rejected_while_playing() {
        let (mut engine, handle) = Engine::new(SR);
        load(&handle, 1, 1, sine_track(2.0, 0.5));
        handle.send(Command::TogglePlay(1));
        render(&mut engine, 4800);
        load(&handle, 1, 2, sine_track(1.0, 0.5));
        handle.send(Command::Eject(1));
        render(&mut engine, 512);
        let shared = &handle.shared.decks[1];
        assert_eq!(shared.loaded_id.load(Relaxed), 1);
        assert_eq!(shared.rejected_id.load(Relaxed), 2);
        assert!(shared.playing.load(Relaxed));
    }

    #[test]
    fn transport_changes_do_not_click() {
        let (mut engine, handle) = Engine::new(SR);
        load(&handle, 0, 1, sine_track(5.0, 0.6));
        handle.shared.crossfader.store(0.0);
        // Un sinus 440 Hz d'amplitude 0,6 varie au plus de ~0,035 par échantillon.
        let limit = 0.06;

        handle.send(Command::TogglePlay(0));
        assert!(max_step(&render(&mut engine, 4800)) < limit, "clic au démarrage");
        handle.send(Command::Seek {
            deck: 0,
            frame: 100_123,
        });
        assert!(max_step(&render(&mut engine, 4800)) < limit, "clic au seek");
        assert!((position(&handle, 0) - 100_123 - 4800).abs() < 600, "seek non effectué");
        handle.shared.decks[0].volume.store(0.1);
        handle.shared.decks[0].kill[0].store(true, Relaxed);
        handle.shared.crossfader.store(0.5);
        assert!(max_step(&render(&mut engine, 4800)) < limit, "clic sur les gains");
        handle.shared.decks[0].volume.store(1.0);
        handle.shared.crossfader.store(0.0);
        render(&mut engine, 4800);

        // Tempo, key lock et transposition, activés puis coupés en pleine lecture.
        handle.shared.decks[0].tempo.store(1.08);
        assert!(
            max_step(&render(&mut engine, 9600)) < limit,
            "clic au changement de tempo"
        );
        handle.shared.decks[0].key_lock.store(true, Relaxed);
        assert!(max_step(&render(&mut engine, 48_000)) < limit, "clic avec le key lock");
        handle.shared.decks[0].pitch_shift.store(2.0);
        assert!(
            max_step(&render(&mut engine, 24_000)) < limit * 1.3,
            "clic à la transposition"
        );
        handle.shared.decks[0].pitch_shift.store(0.0);
        handle.shared.decks[0].key_lock.store(false, Relaxed);
        assert!(
            max_step(&render(&mut engine, 24_000)) < limit * 1.3,
            "clic à la sortie du key lock"
        );

        handle.send(Command::TogglePlay(0));
        let out = render(&mut engine, 4800);
        assert!(max_step(&out) < limit * 1.3, "clic à la pause");
        assert!(
            out[4800..].iter().all(|&s| s.abs() < 1e-6),
            "pas silencieux après la pause"
        );
    }

    #[test]
    fn cue_behaviour() {
        let (mut engine, handle) = Engine::new(SR);
        let shared = &handle.shared.decks[0];
        load(&handle, 0, 1, sine_track(5.0, 0.5));
        handle.send(Command::Seek { deck: 0, frame: 48_000 });
        // À l'arrêt : Cue définit le point de cue à la position courante.
        handle.send(Command::Cue(0));
        render(&mut engine, 512);
        assert_eq!(shared.cue.load(Relaxed), 48_000);

        // En lecture : Cue revient au point de cue et s'arrête.
        handle.send(Command::TogglePlay(0));
        render(&mut engine, 24_000);
        assert!(shared.position.load(Relaxed) > 48_000);
        handle.send(Command::Cue(0));
        render(&mut engine, 4800);
        assert!(!shared.playing.load(Relaxed));
        assert_eq!(shared.position.load(Relaxed), 48_000);
    }

    #[test]
    fn stops_at_end_of_track() {
        let (mut engine, handle) = Engine::new(SR);
        load(&handle, 0, 1, sine_track(0.5, 0.5));
        handle.send(Command::TogglePlay(0));
        render(&mut engine, 48_000);
        let shared = &handle.shared.decks[0];
        assert!(!shared.playing.load(Relaxed));
        assert_eq!(shared.position.load(Relaxed), 24_000);
    }

    #[test]
    fn master_output_is_limited() {
        let (mut engine, handle) = Engine::new(SR);
        for deck in 0..2 {
            load(&handle, deck, 1, sine_track(2.0, 1.0));
            handle.send(Command::TogglePlay(deck));
        }
        handle.shared.master.store(2.0);
        let out = render(&mut engine, 24_000);
        assert!(out.iter().all(|s| s.abs() <= 1.0));
        assert!(handle.shared.master_peak[0].take() > 0.9);
    }

    /// Joue un sinus 440 Hz avec les réglages donnés ; retourne
    /// (vitesse d'avancement mesurée, fréquence entendue).
    fn play_with(tempo: f32, key_lock: bool, pitch_shift: f32) -> (f32, f32) {
        let (mut engine, handle) = Engine::new(SR);
        let shared = &handle.shared.decks[0];
        load(&handle, 0, 1, sine_track(10.0, 0.5));
        handle.shared.crossfader.store(0.0);
        shared.tempo.store(tempo);
        shared.key_lock.store(key_lock, Relaxed);
        shared.pitch_shift.store(pitch_shift);
        handle.send(Command::TogglePlay(0));
        render(&mut engine, 9600);
        let start = position(&handle, 0);
        let out = render(&mut engine, 96_000);
        let peak = out.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!((0.4..0.62).contains(&peak), "niveau anormal : {peak}");
        ((position(&handle, 0) - start) as f32 / 96_000.0, frequency(&out))
    }

    #[test]
    fn tempo_changes_speed_and_pitch_together_without_key_lock() {
        let (speed, freq) = play_with(1.25, false, 0.0);
        assert!((speed - 1.25).abs() < 0.005, "vitesse = {speed}");
        assert!((freq - 550.0).abs() < 3.0, "fréquence = {freq}");
        let (speed, freq) = play_with(0.8, false, 0.0);
        assert!((speed - 0.8).abs() < 0.005, "vitesse = {speed}");
        assert!((freq - 352.0).abs() < 3.0, "fréquence = {freq}");
    }

    #[test]
    fn key_lock_keeps_pitch_at_any_tempo() {
        for tempo in [0.85f32, 1.0, 1.06, 1.3] {
            let (speed, freq) = play_with(tempo, true, 0.0);
            assert!((speed - tempo).abs() < 0.01, "tempo {tempo} : vitesse = {speed}");
            assert!((freq - 440.0).abs() < 4.0, "tempo {tempo} : fréquence = {freq}");
        }
    }

    #[test]
    fn pitch_shift_transposes_without_changing_speed() {
        // +12 demi-tons : une octave au-dessus, même vitesse.
        let (speed, freq) = play_with(1.0, true, 12.0);
        assert!((speed - 1.0).abs() < 0.01, "vitesse = {speed}");
        assert!((freq - 880.0).abs() < 8.0, "fréquence = {freq}");
        // −3 demi-tons, sans key lock et à tempo modifié : les deux effets se cumulent.
        let (speed, freq) = play_with(1.1, false, -3.0);
        assert!((speed - 1.1).abs() < 0.01, "vitesse = {speed}");
        let expected = 440.0 * 1.1 * 2f32.powf(-3.0 / 12.0);
        assert!((freq - expected).abs() < 5.0, "fréquence = {freq}, attendu {expected}");
    }

    #[test]
    fn magnet_starts_on_the_other_decks_beat() {
        let (mut engine, handle) = Engine::new(SR);
        // Un temps toutes les 24 000 frames (120 BPM), calé à 1 000 frames sur A.
        let grid_a = BeatGrid {
            anchor: 1000.0,
            period: 24_000.0,
        };
        let grid_b = BeatGrid {
            anchor: 0.0,
            period: 24_000.0,
        };
        handle.send(Command::Load {
            deck: 0,
            id: 1,
            track: sine_track(10.0, 0.3),
            grid: Some(grid_a),
        });
        handle.send(Command::Load {
            deck: 1,
            id: 2,
            track: sine_track(10.0, 0.3),
            grid: Some(grid_b),
        });
        handle.shared.decks[1].magnet.store(true, Relaxed);

        handle.send(Command::TogglePlay(0));
        render(&mut engine, 6144);
        // B est lancée entre deux temps de A : elle doit attendre le suivant.
        handle.send(Command::TogglePlay(1));
        render(&mut engine, 512);
        assert!(handle.shared.decks[1].armed.load(Relaxed));
        assert!(!handle.shared.decks[1].playing.load(Relaxed));
        assert_eq!(position(&handle, 1), 0);

        render(&mut engine, 48_000);
        assert!(handle.shared.decks[1].playing.load(Relaxed));
        assert!(!handle.shared.decks[1].armed.load(Relaxed));
        // Les temps coïncident : même phase dans les deux grilles.
        let phase_a = grid_a.phase(position(&handle, 0) as f64);
        let phase_b = grid_b.phase(position(&handle, 1) as f64);
        let gap = (phase_a - phase_b).rem_euclid(1.0);
        assert!(
            gap.min(1.0 - gap) * 24_000.0 < 3.0,
            "décalage de {} frames",
            gap.min(1.0 - gap) * 24_000.0
        );

        // Sans magnet (ou si l'autre platine est arrêtée) : départ immédiat.
        handle.send(Command::TogglePlay(1));
        handle.send(Command::TogglePlay(0));
        render(&mut engine, 4800);
        handle.send(Command::TogglePlay(1));
        render(&mut engine, 512);
        assert!(handle.shared.decks[1].playing.load(Relaxed));
    }

    /// Mix = voix (440 Hz) + reste (880 Hz) ; seule la voix est fournie en stem.
    fn two_tone(seconds: f32) -> (Arc<TrackData>, Arc<Stems>) {
        let frames = (seconds * SR as f32) as usize;
        let tone =
            |freq: f32, amp: f32, n: usize| amp * (2.0 * std::f32::consts::PI * freq * n as f32 / SR as f32).sin();
        let mut mix = Vec::with_capacity(frames * 2);
        let mut vocals = Vec::with_capacity(frames * 2);
        for n in 0..frames {
            let v = tone(440.0, 0.3, n);
            let rest = tone(880.0, 0.2, n);
            mix.extend([v + rest, v + rest]);
            vocals.extend([v, v]);
        }
        let stems = Stems::new([vocals, vec![0.0; frames * 2], vec![0.0; frames * 2]], frames);
        (
            Arc::new(TrackData {
                samples: mix,
                sample_rate: SR,
            }),
            Arc::new(stems),
        )
    }

    #[test]
    fn stem_volumes() {
        let (mut engine, handle) = Engine::new(SR);
        let (track, stems) = two_tone(2.0);
        load(&handle, 0, 1, track);
        // Stems d'un autre morceau : ignorés.
        handle.send(Command::SetStems {
            deck: 0,
            id: 99,
            stems: stems.clone(),
        });
        handle.shared.decks[0].stems[0].store(0.0);
        handle.send(Command::TogglePlay(0));
        render(&mut engine, 4800);
        let out = render(&mut engine, 9600);
        assert!((frequency(&out) - 440.0).abs() < 10.0, "sans stems : le mix complet");

        handle.send(Command::SetStems { deck: 0, id: 1, stems });
        render(&mut engine, 4800);
        let out = render(&mut engine, 9600);
        assert!((frequency(&out) - 880.0).abs() < 10.0, "voix coupée : il reste 880 Hz");

        // Instruments coupés, voix rétablie : 440 Hz seul.
        handle.shared.decks[0].stems[0].store(1.0);
        handle.shared.decks[0].stems[3].store(0.0);
        render(&mut engine, 4800);
        let out = render(&mut engine, 9600);
        assert!(
            (frequency(&out) - 440.0).abs() < 10.0,
            "instruments coupés : il reste 440 Hz"
        );
        let peak = left(&out).iter().fold(0.0f32, |m, x| m.max(x.abs()));
        let expected = 0.3 * std::f32::consts::FRAC_1_SQRT_2;
        assert!((peak - expected).abs() < 0.01, "pic {peak}, attendu {expected}");
    }

    #[test]
    fn beat_loop_repeats_without_clicks_then_exits() {
        let (mut engine, handle) = Engine::new(SR);
        handle.send(Command::Load {
            deck: 0,
            id: 1,
            track: sine_track(10.0, 0.3),
            grid: Some(BeatGrid {
                anchor: 0.0,
                period: 24_000.0,
            }),
        });
        handle.shared.crossfader.store(0.0);
        handle.send(Command::TogglePlay(0));
        render(&mut engine, 30_000);
        // Boucle d'un temps : du temps courant (24 000) au suivant (48 000).
        handle.send(Command::SetLoop {
            deck: 0,
            beats: Some(1.0),
        });
        let out = render(&mut engine, 120_000);
        let pos = position(&handle, 0);
        assert!((24_000..48_000).contains(&pos), "position {pos}");
        assert_eq!(handle.shared.decks[0].loop_start.load(Relaxed), 24_000);
        assert_eq!(handle.shared.decks[0].loop_len.load(Relaxed), 24_000);
        assert!(max_step(&out) < 0.05, "saut de {}", max_step(&out));
        // Raccourcie à un demi-temps : même début.
        handle.send(Command::SetLoop {
            deck: 0,
            beats: Some(0.5),
        });
        render(&mut engine, 50_000);
        let pos = position(&handle, 0);
        assert!((24_000..36_000).contains(&pos), "position {pos}");
        // Sortie : la lecture continue.
        handle.send(Command::SetLoop { deck: 0, beats: None });
        render(&mut engine, 48_000);
        assert!(position(&handle, 0) > 48_000);
        assert_eq!(handle.shared.decks[0].loop_len.load(Relaxed), 0);
    }

    #[test]
    fn filter_follows_the_shared_amount() {
        let (mut engine, handle) = Engine::new(SR);
        load(&handle, 0, 1, sine_track(4.0, 0.5));
        handle.shared.crossfader.store(0.0);
        handle.send(Command::TogglePlay(0));
        render(&mut engine, 9600);
        let rms = |buffer: &[f32]| (left(buffer).iter().map(|s| s * s).sum::<f32>() / (buffer.len() / 2) as f32).sqrt();
        let open = rms(&render(&mut engine, 9600));
        // Passe-haut presque fermé : le 440 Hz disparaît, sans clic.
        handle.shared.decks[0].filter.store(0.95);
        let sweep = render(&mut engine, 9600);
        let closed = rms(&render(&mut engine, 9600));
        assert!(max_step(&sweep) < 0.1);
        assert!(closed < open * 0.1, "{closed} / {open}");
    }
}
