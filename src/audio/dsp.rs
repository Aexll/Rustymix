//! Briques DSP du moteur : filtres biquad, EQ 3 bandes, lissage de paramètres,
//! limiteur doux et courbes de gain (potentiomètres, faders, crossfader).
//!
//! Rien ici n'alloue : tout est utilisable depuis le thread audio temps réel.

use std::f64::consts::PI;

/// Fréquence de coupure basse de l'EQ (Low ↔ Mid), en Hz.
pub const EQ_LOW_HZ: f64 = 200.0;
/// Fréquence de coupure haute de l'EQ (Mid ↔ High), en Hz.
pub const EQ_HIGH_HZ: f64 = 3000.0;

const BUTTERWORTH_Q: f64 = std::f64::consts::FRAC_1_SQRT_2;

// ---------------------------------------------------------------------------
// Biquad
// ---------------------------------------------------------------------------

/// Coefficients normalisés (a0 = 1) d'un filtre du second ordre (formules RBJ).
#[derive(Clone, Copy, Debug)]
pub struct BiquadCoeffs {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
}

impl BiquadCoeffs {
    fn prepare(sample_rate: f64, freq: f64, q: f64) -> (f64, f64) {
        // On reste sous Nyquist même avec une fréquence d'échantillonnage très basse.
        let freq = freq.min(sample_rate * 0.45);
        let w0 = 2.0 * PI * freq / sample_rate;
        (w0.cos(), w0.sin() / (2.0 * q))
    }

    pub fn lowpass(sample_rate: f64, freq: f64, q: f64) -> Self {
        let (cos, alpha) = Self::prepare(sample_rate, freq, q);
        let a0 = 1.0 + alpha;
        Self {
            b0: (1.0 - cos) * 0.5 / a0,
            b1: (1.0 - cos) / a0,
            b2: (1.0 - cos) * 0.5 / a0,
            a1: -2.0 * cos / a0,
            a2: (1.0 - alpha) / a0,
        }
    }

    pub fn highpass(sample_rate: f64, freq: f64, q: f64) -> Self {
        let (cos, alpha) = Self::prepare(sample_rate, freq, q);
        let a0 = 1.0 + alpha;
        Self {
            b0: (1.0 + cos) * 0.5 / a0,
            b1: -(1.0 + cos) / a0,
            b2: (1.0 + cos) * 0.5 / a0,
            a1: -2.0 * cos / a0,
            a2: (1.0 - alpha) / a0,
        }
    }

    pub fn allpass(sample_rate: f64, freq: f64, q: f64) -> Self {
        let (cos, alpha) = Self::prepare(sample_rate, freq, q);
        let a0 = 1.0 + alpha;
        Self {
            b0: (1.0 - alpha) / a0,
            b1: -2.0 * cos / a0,
            b2: 1.0,
            a1: -2.0 * cos / a0,
            a2: (1.0 - alpha) / a0,
        }
    }
}

/// Filtre biquad, forme directe II transposée (stable numériquement en basses fréquences).
#[derive(Clone, Copy, Debug)]
pub struct Biquad {
    c: BiquadCoeffs,
    z1: f64,
    z2: f64,
}

impl Biquad {
    pub fn new(c: BiquadCoeffs) -> Self {
        Self { c, z1: 0.0, z2: 0.0 }
    }

    #[inline]
    pub fn process(&mut self, x: f64) -> f64 {
        let y = self.c.b0 * x + self.z1;
        self.z1 = self.c.b1 * x - self.c.a1 * y + self.z2;
        self.z2 = self.c.b2 * x - self.c.a2 * y;
        y
    }

    pub fn reset(&mut self) {
        self.z1 = 0.0;
        self.z2 = 0.0;
    }

    /// Change les coefficients en gardant l'état (balayage sans clic).
    pub fn set(&mut self, c: BiquadCoeffs) {
        self.c = c;
    }
}

// ---------------------------------------------------------------------------
// Filtre DJ
// ---------------------------------------------------------------------------

/// Coupure la plus basse du passe-bas (bouton tourné à fond à gauche), en Hz.
const FILTER_LOW_HZ: f64 = 70.0;
/// Coupure la plus haute du passe-haut (bouton tourné à fond à droite), en Hz.
const FILTER_HIGH_HZ: f64 = 9000.0;
/// Bornes « ouvertes » des deux filtres (transparentes).
const FILTER_OPEN_LP_HZ: f64 = 21_000.0;
const FILTER_OPEN_HP_HZ: f64 = 16.0;
/// Les coefficients sont recalculés toutes les `FILTER_UPDATE` frames.
const FILTER_UPDATE: u32 = 16;

/// Fréquences de coupure (passe-bas, passe-haut) pour `amount` de −1 à 1.
pub fn filter_cutoffs(amount: f32) -> (f64, f64) {
    let a = amount.clamp(-1.0, 1.0) as f64;
    let lp = if a < 0.0 {
        FILTER_OPEN_LP_HZ * (FILTER_LOW_HZ / FILTER_OPEN_LP_HZ).powf(-a)
    } else {
        FILTER_OPEN_LP_HZ
    };
    let hp = if a > 0.0 {
        FILTER_OPEN_HP_HZ * (FILTER_HIGH_HZ / FILTER_OPEN_HP_HZ).powf(a)
    } else {
        FILTER_OPEN_HP_HZ
    };
    (lp, hp)
}

/// Filtre DJ à un seul bouton, stéréo : `amount` de −1 (passe-bas presque
/// fermé) à +1 (passe-haut presque fermé), 0 = transparent. Un passe-bas et un
/// passe-haut en série : la coupure glisse d'un côté à l'autre sans clic,
/// avec une légère résonance quand on s'éloigne du centre.
#[derive(Clone, Copy, Debug)]
pub struct DjFilter {
    sample_rate: f64,
    lp: [Biquad; 2],
    hp: [Biquad; 2],
    /// Valeur pour laquelle les coefficients ont été calculés.
    applied: f32,
    countdown: u32,
}

impl DjFilter {
    pub fn new(sample_rate: f64) -> Self {
        let (lp, hp) = filter_cutoffs(0.0);
        Self {
            sample_rate,
            lp: [Biquad::new(BiquadCoeffs::lowpass(sample_rate, lp, BUTTERWORTH_Q)); 2],
            hp: [Biquad::new(BiquadCoeffs::highpass(sample_rate, hp, BUTTERWORTH_Q)); 2],
            applied: 0.0,
            countdown: 0,
        }
    }

    /// Les filtres tournent en permanence (leur état reste cohérent) ; près
    /// du centre, leur sortie est mélangée progressivement au signal direct,
    /// qui passe seul à 0.
    #[inline]
    pub fn process(&mut self, x: [f32; 2], amount: f32) -> [f32; 2] {
        if self.countdown == 0 {
            self.countdown = FILTER_UPDATE;
            if amount != self.applied {
                self.applied = amount;
                let (lp, hp) = filter_cutoffs(amount);
                let q = BUTTERWORTH_Q + 0.5 * amount.abs().min(1.0) as f64;
                let lp_q = if amount < 0.0 { q } else { BUTTERWORTH_Q };
                let hp_q = if amount > 0.0 { q } else { BUTTERWORTH_Q };
                for ch in 0..2 {
                    self.lp[ch].set(BiquadCoeffs::lowpass(self.sample_rate, lp, lp_q));
                    self.hp[ch].set(BiquadCoeffs::highpass(self.sample_rate, hp, hp_q));
                }
            }
        }
        self.countdown -= 1;
        let wet = (self.applied.abs() * 20.0).min(1.0);
        std::array::from_fn(|ch| {
            let y = self.hp[ch].process(self.lp[ch].process(x[ch] as f64)) as f32;
            x[ch] + (y - x[ch]) * wet
        })
    }

    pub fn reset(&mut self) {
        for filter in self.lp.iter_mut().chain(self.hp.iter_mut()) {
            filter.reset();
        }
    }
}

/// Deux biquads Butterworth en cascade = filtre Linkwitz-Riley d'ordre 4.
#[derive(Clone, Copy, Debug)]
struct Lr4([Biquad; 2]);

impl Lr4 {
    fn lowpass(sample_rate: f64, freq: f64) -> Self {
        Self([Biquad::new(BiquadCoeffs::lowpass(sample_rate, freq, BUTTERWORTH_Q)); 2])
    }

    fn highpass(sample_rate: f64, freq: f64) -> Self {
        Self([Biquad::new(BiquadCoeffs::highpass(sample_rate, freq, BUTTERWORTH_Q)); 2])
    }

    #[inline]
    fn process(&mut self, x: f64) -> f64 {
        let y = self.0[0].process(x);
        self.0[1].process(y)
    }

    fn reset(&mut self) {
        self.0.iter_mut().for_each(Biquad::reset);
    }
}

// ---------------------------------------------------------------------------
// EQ 3 bandes
// ---------------------------------------------------------------------------

/// EQ 3 bandes de type « isolateur DJ » pour un canal.
///
/// Le signal est découpé en trois bandes par des filtres Linkwitz-Riley d'ordre 4
/// (200 Hz et 3 kHz), puis les bandes sont re-sommées avec leur gain :
///
/// * le gain Low se comporte comme un low-shelf à ~200 Hz ;
/// * le gain Mid comme une cloche large centrée vers ~800 Hz – 1 kHz ;
/// * le gain High comme un high-shelf à ~3 kHz.
///
/// Contrairement à des shelves classiques, un gain de 0 coupe réellement la bande
/// (« kill », −∞ dB), et avec les trois gains à 1 la somme est un passe-tout :
/// la réponse en amplitude est parfaitement plate.
#[derive(Clone, Copy, Debug)]
pub struct ThreeBandEq {
    low_lp: Lr4,
    low_hp: Lr4,
    high_lp: Lr4,
    high_hp: Lr4,
    /// Compense sur la bande basse le déphasage du second filtre de coupure.
    low_phase: Biquad,
}

impl ThreeBandEq {
    pub fn new(sample_rate: f64) -> Self {
        Self {
            low_lp: Lr4::lowpass(sample_rate, EQ_LOW_HZ),
            low_hp: Lr4::highpass(sample_rate, EQ_LOW_HZ),
            high_lp: Lr4::lowpass(sample_rate, EQ_HIGH_HZ),
            high_hp: Lr4::highpass(sample_rate, EQ_HIGH_HZ),
            low_phase: Biquad::new(BiquadCoeffs::allpass(sample_rate, EQ_HIGH_HZ, BUTTERWORTH_Q)),
        }
    }

    /// Sépare un échantillon en `[low, mid, high]`.
    #[inline]
    pub fn split(&mut self, x: f32) -> [f32; 3] {
        // Minuscule composante continue : évite que les états des filtres ne
        // tombent dans les nombres dénormalisés (très lents) sur du silence.
        let x = x as f64 + 1e-20;
        let low = self.low_phase.process(self.low_lp.process(x));
        let rest = self.low_hp.process(x);
        let mid = self.high_lp.process(rest);
        let high = self.high_hp.process(rest);
        [low as f32, mid as f32, high as f32]
    }

    /// Traite un échantillon avec les gains linéaires `[low, mid, high]`.
    #[inline]
    pub fn process(&mut self, x: f32, gains: [f32; 3]) -> f32 {
        let [low, mid, high] = self.split(x);
        low * gains[0] + mid * gains[1] + high * gains[2]
    }

    pub fn reset(&mut self) {
        self.low_lp.reset();
        self.low_hp.reset();
        self.high_lp.reset();
        self.high_hp.reset();
        self.low_phase.reset();
    }
}

// ---------------------------------------------------------------------------
// Lissage de paramètres
// ---------------------------------------------------------------------------

/// Lissage exponentiel d'un paramètre (rampe de quelques millisecondes) pour
/// éviter les clics quand un gain change brutalement.
#[derive(Clone, Copy, Debug)]
pub struct Smoothed {
    current: f32,
    coeff: f32,
}

impl Smoothed {
    /// `time_ms` est la constante de temps (≈ 63 % du trajet).
    pub fn new(sample_rate: f32, time_ms: f32, initial: f32) -> Self {
        let coeff = 1.0 - (-1.0 / (time_ms * 0.001 * sample_rate)).exp();
        Self {
            current: initial,
            coeff,
        }
    }

    #[inline]
    pub fn next(&mut self, target: f32) -> f32 {
        let delta = target - self.current;
        // Arrivé (à un cheveu près) : on se pose exactement sur la cible.
        self.current = if delta.abs() < 1e-6 {
            target
        } else {
            self.current + delta * self.coeff
        };
        self.current
    }

    pub fn current(&self) -> f32 {
        self.current
    }

    /// Saute directement à la cible (à n'utiliser que lorsque rien n'est audible).
    pub fn snap(&mut self, target: f32) {
        self.current = target;
    }
}

/// Interpolation cubique (Hermite / Catmull-Rom) entre `p[1]` et `p[2]`,
/// `t` dans 0..1. Sert à lire un morceau à vitesse variable.
#[inline]
pub fn hermite(p: [f32; 4], t: f32) -> f32 {
    let c1 = 0.5 * (p[2] - p[0]);
    let c2 = p[0] - 2.5 * p[1] + 2.0 * p[2] - 0.5 * p[3];
    let c3 = 0.5 * (p[3] - p[0]) + 1.5 * (p[1] - p[2]);
    ((c3 * t + c2) * t + c1) * t + p[1]
}

// ---------------------------------------------------------------------------
// Limiteur et courbes de gain
// ---------------------------------------------------------------------------

/// Limiteur doux sans latence : linéaire jusqu'à −2,5 dBFS, puis saturation
/// progressive (tanh) qui ne dépasse jamais ±1.
#[inline]
pub fn soft_limit(x: f32) -> f32 {
    const THRESHOLD: f32 = 0.75;
    const HEADROOM: f32 = 1.0 - THRESHOLD;
    let a = x.abs();
    if a <= THRESHOLD {
        x
    } else {
        (THRESHOLD + HEADROOM * ((a - THRESHOLD) / HEADROOM).tanh()).copysign(x)
    }
}

pub fn db_to_gain(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

pub fn gain_to_db(gain: f32) -> f32 {
    if gain <= 1e-6 {
        f32::NEG_INFINITY
    } else {
        20.0 * gain.log10()
    }
}

/// Potentiomètre (EQ, master) : position 0 → −∞ (kill), 0,5 → 0 dB, 1 → +6 dB.
pub fn knob_to_gain(position: f32) -> f32 {
    let p = position.clamp(0.0, 1.0);
    if p >= 0.5 {
        db_to_gain((p - 0.5) * 2.0 * 6.0)
    } else {
        let t = p * 2.0;
        t * t
    }
}

/// Fader de volume : position 0 → silence, 1 → 0 dB (courbe quadratique).
pub fn fader_to_gain(position: f32) -> f32 {
    let p = position.clamp(0.0, 1.0);
    p * p
}

/// Crossfader à puissance constante : `x` = 0 → tout A, 1 → tout B.
/// Retourne `(gain_a, gain_b)` avec `gain_a² + gain_b² = 1`.
pub fn crossfader_gains(x: f32) -> (f32, f32) {
    let angle = x.clamp(0.0, 1.0) * std::f32::consts::FRAC_PI_2;
    (angle.cos(), angle.sin())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine_rms_through_eq(freq: f64, gains: [f32; 3]) -> f32 {
        let sr = 48_000.0;
        let mut eq = ThreeBandEq::new(sr);
        let total = 48_000;
        let skip = 24_000; // laisse passer le transitoire
        let mut sum = 0.0f64;
        for n in 0..total {
            let x = (2.0 * PI * freq * n as f64 / sr).sin() as f32;
            let y = eq.process(x, gains);
            if n >= skip {
                sum += (y as f64).powi(2);
            }
        }
        (sum / (total - skip) as f64).sqrt() as f32
    }

    fn sine_rms_through_filter(freq: f64, amount: f32) -> f32 {
        let sr = 48_000.0;
        let mut filter = DjFilter::new(sr);
        let n = 48_000;
        let mut sum = 0.0;
        for i in 0..n {
            let x = (2.0 * PI * freq * i as f64 / sr).sin() as f32;
            let y = filter.process([x, x], amount)[0];
            if i > n / 2 {
                sum += y * y;
            }
        }
        (sum / (n / 2) as f32).sqrt() / std::f32::consts::FRAC_1_SQRT_2
    }

    #[test]
    fn dj_filter_is_bypassed_at_center_and_cuts_when_turned() {
        assert!((sine_rms_through_filter(100.0, 0.0) - 1.0).abs() < 0.01);
        assert!((sine_rms_through_filter(8000.0, 0.0) - 1.0).abs() < 0.01);
        // Passe-bas fermé : les aigus disparaissent, les basses restent.
        assert!(sine_rms_through_filter(5000.0, -0.9) < 0.05);
        assert!(sine_rms_through_filter(50.0, -0.9) > 0.7);
        // Passe-haut fermé : l'inverse.
        assert!(sine_rms_through_filter(60.0, 0.9) < 0.05);
        assert!(sine_rms_through_filter(15000.0, 0.9) > 0.7);
    }

    #[test]
    fn eq_is_flat_at_unity() {
        let expected = std::f32::consts::FRAC_1_SQRT_2;
        for freq in [40.0, 120.0, 200.0, 450.0, 1000.0, 3000.0, 6000.0, 15_000.0] {
            let rms = sine_rms_through_eq(freq, [1.0; 3]);
            assert!((rms - expected).abs() < 0.01, "{freq} Hz : rms = {rms}");
        }
    }

    #[test]
    fn eq_kill_removes_band() {
        // Kill des basses : 50 Hz disparaît, 5 kHz reste intact.
        assert!(sine_rms_through_eq(50.0, [0.0, 1.0, 1.0]) < 0.01);
        assert!(sine_rms_through_eq(5000.0, [0.0, 1.0, 1.0]) > 0.69);
        // Kill des médiums et des aigus.
        assert!(sine_rms_through_eq(800.0, [1.0, 0.0, 1.0]) < 0.06);
        assert!(sine_rms_through_eq(12_000.0, [1.0, 1.0, 0.0]) < 0.01);
        // Tout coupé : silence.
        assert!(sine_rms_through_eq(1000.0, [0.0; 3]) < 1e-6);
    }

    #[test]
    fn crossfader_is_constant_power() {
        for i in 0..=20 {
            let (a, b) = crossfader_gains(i as f32 / 20.0);
            assert!((a * a + b * b - 1.0).abs() < 1e-5);
        }
        let (a, b) = crossfader_gains(0.0);
        assert!((a - 1.0).abs() < 1e-6 && b.abs() < 1e-6);
        let (a, b) = crossfader_gains(1.0);
        assert!(a.abs() < 1e-6 && (b - 1.0).abs() < 1e-6);
    }

    #[test]
    fn knob_curve_matches_spec() {
        assert_eq!(knob_to_gain(0.0), 0.0);
        assert!((knob_to_gain(0.5) - 1.0).abs() < 1e-6);
        assert!((gain_to_db(knob_to_gain(1.0)) - 6.0).abs() < 1e-3);
        assert_eq!(gain_to_db(0.0), f32::NEG_INFINITY);
    }

    #[test]
    fn limiter_is_transparent_then_bounded() {
        assert_eq!(soft_limit(0.5), 0.5);
        assert_eq!(soft_limit(-0.5), -0.5);
        for x in [0.8f32, 1.0, 2.0, 10.0, 1000.0] {
            assert!(soft_limit(x) <= 1.0 && soft_limit(x) > 0.75);
            assert!(soft_limit(-x) >= -1.0 && soft_limit(-x) < -0.75);
        }
    }

    #[test]
    fn smoother_converges() {
        let mut s = Smoothed::new(48_000.0, 5.0, 0.0);
        let mut v = 0.0;
        for _ in 0..4800 {
            v = s.next(1.0);
        }
        assert!((v - 1.0).abs() < 1e-4);
    }
}
