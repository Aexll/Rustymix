//! Analyse musicale d'un morceau : tempo (BPM + position des temps) et tonalité.
//!
//! * Tempo : enveloppe d'attaques (variation d'énergie par bande), puis
//!   autocorrélation pour une première estimation et « peigne » de temps
//!   réguliers pour affiner le BPM et trouver la phase.
//! * Tonalité : chromagramme à Q constant (Goertzel par note), comparé aux
//!   profils de Krumhansl pour les 24 tonalités.
//! * Énergie : niveau sonore médian et densité d'attaques (suggestions de
//!   morceaux pour une transition).
//!
//! L'analyse porte sur le début du morceau ([`ANALYSIS_SECONDS`]) : c'est
//! suffisant et ça garde le scan de la bibliothèque rapide.

use std::f32::consts::PI;

use serde::{Deserialize, Serialize};

use crate::audio::dsp::{Biquad, BiquadCoeffs, ThreeBandEq};

/// Durée analysée au début de chaque morceau, en secondes.
pub const ANALYSIS_SECONDS: f32 = 150.0;

const MIN_BPM: f32 = 60.0;
const MAX_BPM: f32 = 200.0;
/// Cadence de l'enveloppe d'attaques, en Hz.
const ENVELOPE_RATE: f32 = 200.0;

/// Noms des notes en solfège français (dièses et bémols écrits `#` et `b`).
const NOTE_NAMES: [&str; 12] = [
    "Do", "Do#", "Ré", "Mib", "Mi", "Fa", "Fa#", "Sol", "Lab", "La", "Sib", "Si",
];
const MAJOR_PROFILE: [f32; 12] = [6.35, 2.23, 3.48, 2.33, 4.38, 4.09, 2.52, 5.19, 2.39, 3.66, 2.29, 2.88];
const MINOR_PROFILE: [f32; 12] = [6.33, 2.68, 3.52, 5.38, 2.60, 3.53, 2.54, 4.75, 3.98, 2.69, 3.34, 3.17];

/// Tonalité : note fondamentale (0 = do … 11 = si) et mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Key {
    pub pitch_class: u8,
    pub minor: bool,
}

impl Key {
    /// Tonalité transposée de `semitones` demi-tons.
    pub fn transposed(self, semitones: i32) -> Key {
        Key {
            pitch_class: (self.pitch_class as i32 + semitones).rem_euclid(12) as u8,
            minor: self.minor,
        }
    }

    /// Nom complet en français, par exemple `La mineur` ou `Fa# majeur`.
    pub fn name(self) -> String {
        format!(
            "{} {}",
            NOTE_NAMES[self.pitch_class as usize % 12],
            if self.minor { "mineur" } else { "majeur" }
        )
    }

    /// Nom abrégé en français, par exemple `La min` ou `Fa# maj`.
    pub fn short_name(self) -> String {
        format!(
            "{} {}",
            NOTE_NAMES[self.pitch_class as usize % 12],
            if self.minor { "min" } else { "maj" }
        )
    }

    /// Notation Camelot (roue harmonique des DJ), par exemple `8A`.
    pub fn camelot(self) -> String {
        let major_root = if self.minor {
            (self.pitch_class + 3) % 12
        } else {
            self.pitch_class % 12
        };
        let number = (major_root as u32 * 7 + 7) % 12 + 1;
        format!("{number}{}", if self.minor { 'A' } else { 'B' })
    }

    /// Numéro sur la roue Camelot (1 à 12).
    fn camelot_number(self) -> u32 {
        let major_root = if self.minor {
            (self.pitch_class + 3) % 12
        } else {
            self.pitch_class % 12
        };
        (major_root as u32 * 7 + 7) % 12 + 1
    }

    /// Compatibilité harmonique avec `other` (roue Camelot).
    pub fn harmony(self, other: Key) -> Harmony {
        let (a, b) = (self.camelot_number(), other.camelot_number());
        let distance = (a + 12 - b) % 12;
        let neighbour = distance == 1 || distance == 11;
        if self.pitch_class % 12 == other.pitch_class % 12 && self.minor == other.minor {
            Harmony::Same
        } else if (a == b) || (neighbour && self.minor == other.minor) {
            Harmony::Compatible
        } else {
            Harmony::Clash
        }
    }

    /// Rang pour le tri (ordre de la roue Camelot).
    pub fn sort_rank(self) -> u32 {
        let major_root = if self.minor {
            (self.pitch_class + 3) % 12
        } else {
            self.pitch_class % 12
        };
        ((major_root as u32 * 7 + 7) % 12) * 2 + u32::from(!self.minor)
    }
}

/// Accord de deux tonalités, selon la roue Camelot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Harmony {
    /// Même tonalité.
    Same,
    /// Relative majeure / mineure (même numéro), ou voisine sur la roue
    /// (±1, même lettre) : le mélange sonne juste.
    Compatible,
    Clash,
}

/// Résultat de l'analyse d'un morceau.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Analysis {
    /// Tempo en battements par minute, s'il a pu être détecté.
    pub bpm: Option<f32>,
    /// Instant d'un temps de référence, en secondes depuis le début.
    #[serde(default)]
    pub beat_offset: f32,
    pub key: Option<Key>,
    /// Énergie perçue, de 0 (calme) à 1 (intense) ; absente des anciennes
    /// analyses et des silences.
    #[serde(default)]
    pub energy: Option<f32>,
}

/// Mixe en mono le début (au plus [`ANALYSIS_SECONDS`]) d'un buffer stéréo entrelacé.
pub fn mono_excerpt(stereo: &[f32], sample_rate: u32) -> Vec<f32> {
    let frames = (ANALYSIS_SECONDS * sample_rate as f32) as usize;
    let (pairs, _) = stereo.as_chunks::<2>();
    pairs.iter().take(frames).map(|[l, r]| 0.5 * (l + r)).collect()
}

/// Analyse un signal mono.
pub fn analyze(mono: &[f32], sample_rate: u32) -> Analysis {
    let limit = ((ANALYSIS_SECONDS * sample_rate as f32) as usize).min(mono.len());
    let mono = &mono[..limit];
    let (bpm, beat_offset) = match detect_tempo(mono, sample_rate) {
        Some((bpm, offset)) => (Some(bpm), offset),
        None => (None, 0.0),
    };
    Analysis {
        bpm,
        beat_offset,
        key: detect_key(mono, sample_rate),
        energy: detect_energy(mono, sample_rate),
    }
}

// ---------------------------------------------------------------------------
// Énergie
// ---------------------------------------------------------------------------

/// Énergie perçue, de 0 (calme) à 1 (intense) : moitié niveau sonore médian
/// (fenêtres de 50 ms, silences exclus), moitié densité d'attaques (nombre
/// de coups par seconde). `None` pour un silence.
fn detect_energy(mono: &[f32], sample_rate: u32) -> Option<f32> {
    let window = ((sample_rate as f32 * 0.05) as usize).max(1);
    let mut levels: Vec<f32> = mono
        .chunks_exact(window)
        .map(|chunk| {
            let power = chunk.iter().map(|x| x * x).sum::<f32>() / chunk.len() as f32;
            10.0 * power.max(1e-12).log10()
        })
        .filter(|db| *db > -50.0)
        .collect();
    if levels.len() < 20 {
        return None;
    }
    levels.sort_by(f32::total_cmp);
    let median_db = levels[levels.len() / 2];
    // −32 dB (très calme) → 0, −10 dB (master très fort) → 1.
    let loudness = ((median_db + 32.0) / 22.0).clamp(0.0, 1.0);

    // Attaques : pics de l'enveloppe au-dessus d'un écart type, espacés
    // d'au moins 70 ms.
    let (envelope, rate) = onset_envelope(mono, sample_rate);
    let deviation = (envelope.iter().map(|v| v * v).sum::<f32>() / envelope.len().max(1) as f32).sqrt();
    let spacing = (rate * 0.07) as usize;
    let mut onsets = 0;
    let mut last: Option<usize> = None;
    for i in 1..envelope.len().saturating_sub(1) {
        let peak = envelope[i] > deviation && envelope[i] >= envelope[i - 1] && envelope[i] > envelope[i + 1];
        if peak && last.is_none_or(|last| i - last >= spacing) {
            onsets += 1;
            last = Some(i);
        }
    }
    let seconds = levels.len() as f32 * 0.05;
    // 6 coups par seconde (doubles croches à 90 BPM, croches à 180) ou plus → 1.
    let density = (onsets as f32 / seconds / 6.0).clamp(0.0, 1.0);
    Some(((0.5 * loudness + 0.5 * density) * 100.0).round() / 100.0)
}

// ---------------------------------------------------------------------------
// Tempo
// ---------------------------------------------------------------------------

/// Enveloppe d'attaques : hausse d'énergie (log) dans trois bandes, à 200 Hz.
fn onset_envelope(mono: &[f32], sample_rate: u32) -> (Vec<f32>, f32) {
    let hop = (sample_rate as f32 / ENVELOPE_RATE).round().max(1.0) as usize;
    let rate = sample_rate as f32 / hop as f32;
    let mut bands = ThreeBandEq::new(sample_rate as f64);
    let mut previous = [0.0f32; 3];
    let mut envelope: Vec<f32> = mono
        .chunks_exact(hop)
        .map(|chunk| {
            let mut energy = [0.0f32; 3];
            for &x in chunk {
                let split = bands.split(x);
                for b in 0..3 {
                    energy[b] += split[b] * split[b];
                }
            }
            let mut flux = 0.0;
            for b in 0..3 {
                let level = (1.0 + 100.0 * (energy[b] / hop as f32).sqrt()).ln();
                flux += (level - previous[b]).max(0.0);
                previous[b] = level;
            }
            flux
        })
        .collect();

    // Léger lissage (tolère l'arrondi des positions de temps) et retrait de la moyenne.
    let raw = envelope.clone();
    for i in 1..raw.len().saturating_sub(1) {
        envelope[i] = 0.25 * raw[i - 1] + 0.5 * raw[i] + 0.25 * raw[i + 1];
    }
    let mean = envelope.iter().sum::<f32>() / envelope.len().max(1) as f32;
    envelope.iter_mut().for_each(|v| *v -= mean);
    (envelope, rate)
}

/// Meilleur score d'un peigne de période `period` (en pas d'enveloppe) et sa phase.
fn comb_score(envelope: &[f32], period: f32) -> (f32, f32) {
    let phases = period.ceil() as usize;
    let mut best = (f32::MIN, 0.0);
    for phase in 0..phases {
        let mut sum = 0.0;
        let mut count = 0u32;
        let mut t = phase as f32;
        while (t as usize) < envelope.len() {
            sum += envelope[t.round().min(envelope.len() as f32 - 1.0) as usize];
            count += 1;
            t += period;
        }
        let score = sum / count.max(1) as f32;
        if score > best.0 {
            best = (score, phase as f32);
        }
    }
    best
}

/// Retourne `(bpm, instant d'un temps en secondes)`.
fn detect_tempo(mono: &[f32], sample_rate: u32) -> Option<(f32, f32)> {
    let (envelope, rate) = onset_envelope(mono, sample_rate);
    let max_lag = (rate * 60.0 / MIN_BPM) as usize;
    let min_lag = (rate * 60.0 / MAX_BPM) as usize;
    if envelope.len() < max_lag * 6 {
        return None; // moins de quelques secondes : pas fiable
    }
    let energy: f32 = envelope.iter().map(|v| v * v).sum();
    if energy < 1e-4 {
        return None; // silence ou son sans attaque
    }

    // 1) Estimation grossière par autocorrélation, avec préférence pour ~120 BPM.
    let autocorr = |lag: usize| -> f32 {
        if lag >= envelope.len() {
            return 0.0;
        }
        envelope[lag..].iter().zip(&envelope).map(|(a, b)| a * b).sum::<f32>() / (envelope.len() - lag) as f32
    };
    let mut best = (f32::MIN, min_lag);
    for lag in min_lag..=max_lag {
        let bpm = rate * 60.0 / lag as f32;
        let prior = (-0.5 * ((bpm / 120.0).log2() / 0.7).powi(2)).exp();
        let score = (autocorr(lag) + 0.5 * autocorr(lag * 2)) * prior;
        if score > best.0 {
            best = (score, lag);
        }
    }
    if best.0 <= 0.0 {
        return None;
    }
    let coarse = rate * 60.0 / best.1 as f32;

    // 2) Affinage : peigne de temps réguliers, d'abord par pas de 0,02 BPM puis de 0,002.
    let search = |center: f32, span: f32, step: f32| -> (f32, f32) {
        let steps = (2.0 * span / step).round() as i32;
        let mut best = (f32::MIN, center, 0.0);
        for i in 0..=steps {
            let bpm = center - span + i as f32 * step;
            let (score, phase) = comb_score(&envelope, rate * 60.0 / bpm);
            if score > best.0 {
                best = (score, bpm, phase);
            }
        }
        (best.1, best.2)
    };
    let (bpm, _) = search(coarse, coarse * 0.03, 0.02);
    let (mut bpm, mut phase) = search(bpm, 0.02, 0.002);

    // La plupart des morceaux produits ont un tempo entier.
    if (bpm - bpm.round()).abs() < 0.04 {
        bpm = bpm.round();
        phase = comb_score(&envelope, rate * 60.0 / bpm).1;
    }
    Some((bpm, phase / rate))
}

// ---------------------------------------------------------------------------
// Tonalité
// ---------------------------------------------------------------------------

/// Énergie par classe de hauteur (do … si), sur 4 octaves à partir de do2.
fn chromagram(mono: &[f32], sample_rate: u32) -> [f32; 12] {
    const FRAME: usize = 4096;
    // Passe-bas puis décimation vers ~4,4 kHz : les fondamentales suffisent.
    let factor = ((sample_rate as f32 / 4400.0).round() as usize).max(1);
    let rate = sample_rate as f32 / factor as f32;
    let coeffs = BiquadCoeffs::lowpass(sample_rate as f64, (rate * 0.4) as f64, std::f64::consts::FRAC_1_SQRT_2);
    let mut filters = [Biquad::new(coeffs), Biquad::new(coeffs)];
    let low: Vec<f32> = mono
        .iter()
        .map(|&x| {
            let y = filters[0].process(x as f64);
            filters[1].process(y) as f32
        })
        .step_by(factor)
        .collect();

    // Par octave : fenêtre de Hann de plus en plus courte (Q constant) et
    // coefficients de Goertzel des 12 notes.
    let octaves: Vec<(Vec<f32>, [f32; 12])> = (0..4)
        .map(|octave| {
            let len = FRAME >> octave;
            let window = (0..len)
                .map(|n| 0.5 - 0.5 * (2.0 * PI * n as f32 / len as f32).cos())
                .collect();
            let coeffs = std::array::from_fn(|note| {
                let midi = 36 + octave * 12 + note;
                let freq = 440.0 * 2f32.powf((midi as f32 - 69.0) / 12.0);
                2.0 * (2.0 * PI * freq / rate).cos()
            });
            (window, coeffs)
        })
        .collect();

    let mut chroma = [0.0f32; 12];
    // Trames jointives, puis décalées d'une demi-trame.
    let (aligned, _) = low.as_chunks::<FRAME>();
    let (shifted, _) = low.get(FRAME / 2..).unwrap_or(&[]).as_chunks::<FRAME>();
    for frame in aligned.iter().chain(shifted) {
        for (window, coeffs) in &octaves {
            let start = (FRAME - window.len()) / 2;
            let segment = &frame[start..start + window.len()];
            for (note, &coeff) in coeffs.iter().enumerate() {
                let (mut s1, mut s2) = (0.0f32, 0.0f32);
                for (&x, &w) in segment.iter().zip(window) {
                    let s0 = x * w + coeff * s1 - s2;
                    s2 = s1;
                    s1 = s0;
                }
                let power = (s1 * s1 + s2 * s2 - coeff * s1 * s2).max(0.0);
                chroma[note] += power.sqrt() / window.len() as f32;
            }
        }
    }
    chroma
}

fn correlation(a: &[f32; 12], b: &[f32; 12]) -> f32 {
    let mean = |v: &[f32; 12]| v.iter().sum::<f32>() / 12.0;
    let (ma, mb) = (mean(a), mean(b));
    let (mut num, mut da, mut db) = (0.0, 0.0, 0.0);
    for i in 0..12 {
        num += (a[i] - ma) * (b[i] - mb);
        da += (a[i] - ma).powi(2);
        db += (b[i] - mb).powi(2);
    }
    num / (da * db).sqrt().max(1e-12)
}

fn detect_key(mono: &[f32], sample_rate: u32) -> Option<Key> {
    let chroma = chromagram(mono, sample_rate);
    if chroma.iter().sum::<f32>() < 1e-4 {
        return None;
    }
    let mut best = (
        f32::MIN,
        Key {
            pitch_class: 0,
            minor: false,
        },
    );
    for root in 0..12u8 {
        for (profile, minor) in [(&MAJOR_PROFILE, false), (&MINOR_PROFILE, true)] {
            let rotated: [f32; 12] = std::array::from_fn(|i| profile[(i + 12 - root as usize) % 12]);
            let score = correlation(&chroma, &rotated);
            if score > best.0 {
                best = (
                    score,
                    Key {
                        pitch_class: root,
                        minor,
                    },
                );
            }
        }
    }
    Some(best.1)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 44_100;

    /// Piste de « clics » graves à `bpm`, le premier à `offset` secondes.
    fn click_track(bpm: f32, offset: f32, seconds: f32) -> Vec<f32> {
        let period = 60.0 / bpm;
        (0..(seconds * SR as f32) as usize)
            .map(|n| {
                let t = n as f32 / SR as f32 - offset;
                if t < 0.0 {
                    return 0.0;
                }
                let since_beat = t % period;
                0.8 * (2.0 * PI * 60.0 * since_beat).sin() * (-since_beat * 25.0).exp()
            })
            .collect()
    }

    #[test]
    fn detects_tempo_and_beat_phase() {
        for (bpm, offset) in [(128.0f32, 0.10f32), (90.0, 0.25), (140.0, 0.0), (123.5, 0.05)] {
            let analysis = analyze(&click_track(bpm, offset, 40.0), SR);
            let found = analysis.bpm.expect("tempo non détecté");
            assert!((found - bpm).abs() < 0.08, "attendu {bpm}, trouvé {found}");
            // La phase doit tomber sur un temps, à 15 ms près.
            let period = 60.0 / bpm;
            let error = (analysis.beat_offset - offset).rem_euclid(period);
            assert!(
                error.min(period - error) < 0.015,
                "{bpm} BPM : phase décalée de {error} s"
            );
        }
    }

    #[test]
    fn energy_grows_with_level_and_density() {
        let seconds = 20.0;
        let n = (seconds * SR as f32) as usize;
        // Nappe calme : un sinus faible et continu.
        let calm: Vec<f32> = (0..n)
            .map(|i| 0.05 * (2.0 * PI * 220.0 * i as f32 / SR as f32).sin())
            .collect();
        // Rythme dense et fort : un bruit coupé en croches à 128 BPM (doubles croches).
        let mut seed = 1u32;
        let sixteenth = 60.0 / 128.0 / 4.0;
        let dense: Vec<f32> = (0..n)
            .map(|i| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let noise = (seed >> 9) as f32 / (1u32 << 23) as f32 * 2.0 - 1.0;
                let since = (i as f32 / SR as f32) % sixteenth;
                0.9 * noise * (-since * 30.0).exp()
            })
            .collect();
        let calm = analyze(&calm, SR).energy.expect("énergie");
        let clicks = analyze(&click_track(128.0, 0.0, seconds), SR).energy.expect("énergie");
        let dense = analyze(&dense, SR).energy.expect("énergie");
        assert!(calm < clicks && clicks < dense, "{calm} < {clicks} < {dense}");
        assert!(calm < 0.2 && dense > 0.7, "{calm}, {dense}");
    }

    #[test]
    fn harmonic_compatibility() {
        let key = |pitch_class, minor| Key { pitch_class, minor };
        let a_minor = key(9, true); // 8A
        assert_eq!(a_minor.harmony(a_minor), Harmony::Same);
        assert_eq!(a_minor.harmony(key(0, false)), Harmony::Compatible); // Do majeur, 8B
        assert_eq!(a_minor.harmony(key(4, true)), Harmony::Compatible); // Mi mineur, 9A
        assert_eq!(a_minor.harmony(key(2, true)), Harmony::Compatible); // Ré mineur, 7A
        assert_eq!(a_minor.harmony(key(7, false)), Harmony::Clash); // Sol majeur, 9B
        assert_eq!(a_minor.harmony(key(10, true)), Harmony::Clash); // Sib mineur, 3A
    }

    #[test]
    fn silence_has_no_tempo_nor_key() {
        let analysis = analyze(&vec![0.0; SR as usize * 20], SR);
        assert_eq!(analysis, Analysis::default());
    }

    /// Accords joués en boucle (notes MIDI), une seconde chacun.
    fn chords(progression: &[&[i32]], seconds: usize) -> Vec<f32> {
        (0..seconds * SR as usize)
            .map(|n| {
                let chord = progression[(n / SR as usize) % progression.len()];
                let t = n as f32 / SR as f32;
                chord
                    .iter()
                    .map(|&midi| {
                        let f = 440.0 * 2f32.powf((midi as f32 - 69.0) / 12.0);
                        0.15 * ((2.0 * PI * f * t).sin() + 0.4 * (4.0 * PI * f * t).sin())
                    })
                    .sum()
            })
            .collect()
    }

    #[test]
    fn detects_major_and_minor_keys() {
        // Do majeur : I – IV – V – I.
        let c_major = chords(
            &[
                &[48, 60, 64, 67],
                &[53, 60, 65, 69],
                &[55, 59, 62, 67],
                &[48, 60, 64, 67],
            ],
            24,
        );
        assert_eq!(
            detect_key(&c_major, SR),
            Some(Key {
                pitch_class: 0,
                minor: false
            })
        );
        // La mineur : i – iv – V – i.
        let a_minor = chords(
            &[
                &[45, 57, 60, 64],
                &[50, 57, 62, 65],
                &[52, 56, 59, 64],
                &[45, 57, 60, 64],
            ],
            24,
        );
        assert_eq!(
            detect_key(&a_minor, SR),
            Some(Key {
                pitch_class: 9,
                minor: true
            })
        );
        // Même chose trois demi-tons plus haut : do mineur.
        let c_minor = chords(
            &[
                &[48, 60, 63, 67],
                &[53, 60, 65, 68],
                &[55, 59, 62, 67],
                &[48, 60, 63, 67],
            ],
            24,
        );
        assert_eq!(
            detect_key(&c_minor, SR),
            Some(Key {
                pitch_class: 0,
                minor: true
            })
        );
    }

    #[test]
    fn key_names_and_camelot() {
        let a_minor = Key {
            pitch_class: 9,
            minor: true,
        };
        assert_eq!(
            (a_minor.name().as_str(), a_minor.camelot().as_str()),
            ("La mineur", "8A")
        );
        assert_eq!(a_minor.short_name(), "La min");
        let c_major = Key {
            pitch_class: 0,
            minor: false,
        };
        assert_eq!(
            (c_major.name().as_str(), c_major.camelot().as_str()),
            ("Do majeur", "8B")
        );
        assert_eq!(
            Key {
                pitch_class: 6,
                minor: false
            }
            .camelot(),
            "2B"
        );
        assert_eq!(
            Key {
                pitch_class: 4,
                minor: true
            }
            .camelot(),
            "9A"
        );
        assert_eq!(
            a_minor.transposed(3),
            Key {
                pitch_class: 0,
                minor: true
            }
        );
        assert_eq!(
            a_minor.transposed(-10),
            Key {
                pitch_class: 11,
                minor: true
            }
        );
    }
}
