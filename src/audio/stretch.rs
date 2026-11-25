//! Étirement temporel et transposition en temps réel (« key lock », « match key »).
//!
//! Deux étages :
//! 1. un étirement temporel WSOLA : le morceau est relu par segments qui se
//!    chevauchent ; quand la position de lecture s'écarte trop de la position
//!    voulue, on saute à l'endroit voisin dont la forme d'onde ressemble le plus
//!    (corrélation) et on raccorde par un fondu — la durée change, pas la hauteur ;
//! 2. un rééchantillonnage (interpolation cubique) qui change la hauteur.
//!
//! Avec un étirement `s` et un facteur de hauteur `p`, le morceau avance à la
//! vitesse `s × p` et sonne `p` fois plus aigu. Aucune allocation après `new`.

use super::dsp::hermite;
use super::engine::Source;

/// La recherche de ressemblance se fait sur un signal mono décimé (suffisant
/// et bien moins coûteux).
const DECIMATION: usize = 4;
/// Longueur d'un pas de sortie (demi-segment), en secondes.
const HOP_SECS: f32 = 0.021;
/// Écart maximal toléré autour de la position voulue, en secondes.
const SEARCH_SECS: f32 = 0.010;

pub struct Stretcher {
    hop: usize,
    search: usize,
    fade_in: Vec<f32>,
    template: Vec<f32>,
    region: Vec<f32>,
    /// Dernier pas produit par l'étirement, lu par le rééchantillonneur.
    out: Vec<[f32; 2]>,
    read: usize,
    /// Début (dans le morceau) du segment précédent.
    prev: i64,
    /// Position voulue du prochain segment.
    nominal: f64,
    /// Quatre dernières frames étirées, pour l'interpolation.
    history: [[f32; 2]; 4],
    phase: f64,
    primed: bool,
}

impl Stretcher {
    pub fn new(sample_rate: f32) -> Self {
        let round = |secs: f32| (((sample_rate * secs) as usize / DECIMATION).max(4)) * DECIMATION;
        let (hop, search) = (round(HOP_SECS), round(SEARCH_SECS));
        Self {
            hop,
            search,
            fade_in: (0..hop)
                .map(|i| 0.5 - 0.5 * (std::f32::consts::PI * (i as f32 + 0.5) / hop as f32).cos())
                .collect(),
            template: vec![0.0; hop / DECIMATION],
            region: vec![0.0; (hop + 2 * search) / DECIMATION],
            out: vec![[0.0; 2]; hop],
            read: hop,
            prev: 0,
            nominal: 0.0,
            history: [[0.0; 2]; 4],
            phase: 0.0,
            primed: false,
        }
    }

    /// Repart de la position `pos` (frames) du morceau.
    pub fn reset(&mut self, pos: f64) {
        let start = pos.round();
        self.prev = start as i64 - self.hop as i64;
        self.nominal = start;
        self.read = self.hop;
        self.phase = 0.0;
        self.primed = false;
    }

    /// Produit une frame. `stretch` : frames de morceau consommées par frame
    /// étirée ; `pitch` : facteur de hauteur.
    #[inline]
    pub fn next(&mut self, track: &Source, stretch: f64, pitch: f64) -> [f32; 2] {
        if !self.primed {
            let first = self.pull(track, stretch);
            self.history = [first, first, self.pull(track, stretch), self.pull(track, stretch)];
            self.primed = true;
        }
        while self.phase >= 1.0 {
            self.phase -= 1.0;
            self.history.rotate_left(1);
            self.history[3] = self.pull(track, stretch);
        }
        let h = &self.history;
        let t = self.phase as f32;
        let frame = if t == 0.0 {
            h[1]
        } else {
            [
                hermite([h[0][0], h[1][0], h[2][0], h[3][0]], t),
                hermite([h[0][1], h[1][1], h[2][1], h[3][1]], t),
            ]
        };
        self.phase += pitch;
        frame
    }

    /// Position (frames du morceau) correspondant à la prochaine frame produite.
    pub fn position(&self, stretch: f64) -> f64 {
        let ahead = (self.hop - self.read) as f64 + 3.0 - self.phase;
        self.nominal - stretch * ahead
    }

    #[inline]
    fn pull(&mut self, track: &Source, stretch: f64) -> [f32; 2] {
        if self.read >= self.hop {
            self.fill(track, stretch);
        }
        let frame = self.out[self.read];
        self.read += 1;
        frame
    }

    /// Produit le pas suivant de signal étiré.
    fn fill(&mut self, track: &Source, stretch: f64) {
        let hop = self.hop as i64;
        // Suite naturelle du segment précédent, et position où l'on voudrait être.
        let natural = self.prev + hop;
        let target = self.nominal.round() as i64;

        // Tant que la suite naturelle reste proche de la cible, on la garde :
        // c'est le signal d'origine, sans aucun raccord.
        let current = if (target - natural).abs() <= self.search as i64 {
            natural
        } else {
            self.best_match(track, natural, target)
        };

        if current == natural {
            for (i, out) in self.out.iter_mut().enumerate() {
                *out = track.frame_at(natural + i as i64);
            }
        } else {
            for (i, out) in self.out.iter_mut().enumerate() {
                let a = track.frame_at(natural + i as i64);
                let b = track.frame_at(current + i as i64);
                let w = self.fade_in[i];
                *out = [a[0] + (b[0] - a[0]) * w, a[1] + (b[1] - a[1]) * w];
            }
        }

        self.prev = current;
        self.nominal += stretch * self.hop as f64;
        self.read = 0;
    }

    /// Cherche, autour de `target`, le début de segment qui ressemble le plus
    /// à la suite naturelle (corrélation normalisée sur le signal mono décimé).
    fn best_match(&mut self, track: &Source, natural: i64, target: i64) -> i64 {
        let d = DECIMATION as i64;
        let start = target - self.search as i64;
        decimate(track, natural, &mut self.template);
        decimate(track, start, &mut self.region);

        let len = self.template.len();
        let template_energy: f32 = self.template.iter().map(|v| v * v).sum();
        if template_energy < 1e-9 {
            return target; // silence : rien à aligner
        }

        let mut energy: f32 = self.region[..len].iter().map(|v| v * v).sum();
        let mut best = (f32::MIN, 0usize);
        for offset in 0..=self.region.len() - len {
            let window = &self.region[offset..offset + len];
            let correlation: f32 = window.iter().zip(&self.template).map(|(a, b)| a * b).sum();
            let score = correlation / (energy + 1e-9).sqrt();
            if score > best.0 {
                best = (score, offset);
            }
            if let Some(next) = self.region.get(offset + len) {
                energy = (energy + next * next - window[0] * window[0]).max(0.0);
            }
        }
        start + best.1 as i64 * d
    }
}

/// Remplit `out` avec le signal mono décimé à partir de la frame `start`.
fn decimate(track: &Source, start: i64, out: &mut [f32]) {
    for (j, value) in out.iter_mut().enumerate() {
        let base = start + (j * DECIMATION) as i64;
        let mut sum = 0.0;
        for k in 0..DECIMATION as i64 {
            let frame = track.frame_at(base + k);
            sum += frame[0] + frame[1];
        }
        *value = sum;
    }
}
