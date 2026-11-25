//! Suggestions pour une transition : note la compatibilité d'un morceau de
//! la bibliothèque avec celui d'une platine, sans aucune base extérieure,
//! à partir de l'analyse locale (tempo, tonalité, énergie).
//!
//! * Tempo : écart de BPM à rattraper au fader pour caler le morceau (en
//!   comptant le double et la moitié) ; au-delà de ±12 % il ne se cale plus.
//! * Tonalité : roue Camelot (même tonalité, relative, voisine), ou accord
//!   possible en transposant d'un ou deux demi-tons (MATCH).
//! * Énergie : garder ou monter légèrement l'énergie plutôt que la casser.

use crate::analysis::{Analysis, Harmony, Key};

/// Écart de tempo au-delà duquel le morceau ne se cale plus au fader, en %.
pub const MAX_TEMPO_PERCENT: f32 = 12.0;
/// Écart de tempo qui annule la note de tempo, en %.
const TEMPO_FALLOFF_PERCENT: f32 = 10.0;

const TEMPO_WEIGHT: f32 = 0.45;
const KEY_WEIGHT: f32 = 0.40;
const ENERGY_WEIGHT: f32 = 0.15;
/// Note d'un critère inconnu (analyse incomplète).
const UNKNOWN: f32 = 0.4;

/// Ce que joue la platine de référence.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Reference {
    /// Tempo réellement joué (BPM × vitesse).
    pub bpm: Option<f32>,
    /// Tonalité entendue (transposition comprise).
    pub key: Option<Key>,
    pub energy: Option<f32>,
}

/// Accord de tonalité d'un morceau candidat.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyFit {
    Same,
    Compatible,
    /// Compatible une fois transposé de ce nombre de demi-tons (±1 ou ±2).
    Shift(i32),
    Clash,
}

impl KeyFit {
    fn score(self) -> f32 {
        match self {
            KeyFit::Same => 1.0,
            KeyFit::Compatible => 0.85,
            KeyFit::Shift(n) if n.abs() == 1 => 0.5,
            KeyFit::Shift(_) => 0.35,
            KeyFit::Clash => 0.0,
        }
    }
}

/// Compatibilité d'un morceau avec la référence.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Match {
    /// Note globale, de 0 à 1.
    pub score: f32,
    /// Variation de tempo (en %) à appliquer au morceau pour le caler.
    pub tempo_percent: Option<f32>,
    /// Le morceau se cale en jouant au double ou à la moitié de son tempo.
    pub tempo_multiple: Option<f32>,
    pub key: Option<KeyFit>,
    /// Écart d'énergie (morceau − référence).
    pub energy_delta: Option<f32>,
}

impl Match {
    /// Le tempo se rattrape au fader (±12 %).
    pub fn tempo_reachable(&self) -> bool {
        self.tempo_percent
            .is_some_and(|percent| percent.abs() <= MAX_TEMPO_PERCENT)
    }
}

/// Accord de `key` avec `reference`, transposition éventuelle comprise
/// (le plus petit décalage qui rend les tonalités compatibles).
pub fn key_fit(key: Key, reference: Key) -> KeyFit {
    match key.harmony(reference) {
        Harmony::Same => KeyFit::Same,
        Harmony::Compatible => KeyFit::Compatible,
        Harmony::Clash => [1, -1, 2, -2]
            .into_iter()
            .find(|&shift| key.transposed(shift).harmony(reference) != Harmony::Clash)
            .map_or(KeyFit::Clash, KeyFit::Shift),
    }
}

/// Note `candidate` par rapport à `reference`.
pub fn rate(reference: &Reference, candidate: &Analysis) -> Match {
    // Tempo : multiple (×½, ×1, ×2) qui demande le moins de correction.
    let tempo = match (reference.bpm, candidate.bpm) {
        (Some(target), Some(bpm)) if target > 0.0 && bpm > 0.0 => [1.0f32, 2.0, 0.5]
            .into_iter()
            .map(|multiple| (multiple, (target / (bpm * multiple) - 1.0) * 100.0))
            .min_by(|a, b| a.1.abs().total_cmp(&b.1.abs())),
        _ => None,
    };
    let tempo_score = tempo.map_or(UNKNOWN, |(_, percent)| {
        if percent.abs() > MAX_TEMPO_PERCENT {
            0.0
        } else {
            (1.0 - percent.abs() / TEMPO_FALLOFF_PERCENT).max(0.0)
        }
    });

    let key = match (candidate.key, reference.key) {
        (Some(key), Some(target)) => Some(key_fit(key, target)),
        _ => None,
    };
    let key_score = key.map_or(UNKNOWN, KeyFit::score);

    // Énergie : idéale si égale ou un peu plus haute (on fait monter la soirée).
    let energy_delta = match (candidate.energy, reference.energy) {
        (Some(energy), Some(target)) => Some(energy - target),
        _ => None,
    };
    let energy_score = energy_delta.map_or(UNKNOWN, |delta| {
        if (0.0..=0.15).contains(&delta) {
            1.0
        } else {
            let distance = if delta > 0.0 { delta - 0.15 } else { -delta };
            (1.0 - distance * 2.5).max(0.0)
        }
    });

    Match {
        score: TEMPO_WEIGHT * tempo_score + KEY_WEIGHT * key_score + ENERGY_WEIGHT * energy_score,
        tempo_percent: tempo.map(|(_, percent)| percent),
        tempo_multiple: tempo.map(|(multiple, _)| multiple).filter(|multiple| *multiple != 1.0),
        key,
        energy_delta,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(pitch_class: u8, minor: bool) -> Key {
        Key { pitch_class, minor }
    }

    fn analysis(bpm: f32, key: Key, energy: f32) -> Analysis {
        Analysis {
            bpm: Some(bpm),
            beat_offset: 0.0,
            key: Some(key),
            energy: Some(energy),
        }
    }

    #[test]
    fn tempo_counts_double_and_half() {
        let reference = Reference {
            bpm: Some(128.0),
            key: None,
            energy: None,
        };
        let same = rate(&reference, &analysis(128.0, key(0, false), 0.5));
        assert_eq!(same.tempo_percent, Some(0.0));
        let half = rate(&reference, &analysis(64.5, key(0, false), 0.5));
        assert_eq!(half.tempo_multiple, Some(2.0));
        assert!(half.tempo_reachable());
        // 100 BPM → +28 % : hors de portée du fader.
        let far = rate(&reference, &analysis(100.0, key(0, false), 0.5));
        assert!(!far.tempo_reachable());
        assert!(same.score > half.score && half.score > far.score);
    }

    #[test]
    fn key_fit_uses_the_camelot_wheel_and_small_shifts() {
        let a_minor = key(9, true); // 8A
        assert_eq!(key_fit(a_minor, a_minor), KeyFit::Same);
        assert_eq!(key_fit(key(0, false), a_minor), KeyFit::Compatible); // Do majeur, 8B
        // Sib mineur (3A) : un demi-ton plus bas, c'est La mineur.
        assert_eq!(key_fit(key(10, true), a_minor), KeyFit::Shift(-1));
        // Fa# mineur (11A) → ±2 demi-tons donnent 1A / 9A : 9A est voisine de 8A.
        assert_eq!(key_fit(key(6, true), a_minor), KeyFit::Shift(-2));
    }

    #[test]
    fn best_match_wins() {
        let reference = Reference {
            bpm: Some(126.0),
            key: Some(key(9, true)),
            energy: Some(0.6),
        };
        let ideal = rate(&reference, &analysis(125.0, key(9, true), 0.65));
        let clash = rate(&reference, &analysis(125.0, key(3, false), 0.65));
        let calm = rate(&reference, &analysis(125.0, key(9, true), 0.1));
        let unknown = rate(&reference, &Analysis::default());
        assert!(ideal.score > 0.95, "{}", ideal.score);
        assert!(ideal.score > calm.score && calm.score > clash.score);
        assert!(unknown.score < calm.score);
    }
}
