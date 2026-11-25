//! Formes d'onde multi-résolution, calculées une fois au chargement d'un morceau.
//!
//! Le morceau est découpé en segments ; pour chacun on garde, par canal, le pic
//! et la valeur RMS, plus l'énergie dans trois bandes de fréquences (basses /
//! médiums / aigus) pour la coloration. Une pyramide de niveaux (chaque niveau
//! regroupe deux segments du précédent) sert aussi bien la vue d'ensemble que
//! la vue zoomée sans jamais reparcourir les échantillons.

use crate::audio::dsp::ThreeBandEq;

/// Nombre de frames par segment au niveau le plus fin.
const BASE_FRAMES_PER_POINT: usize = 64;
/// On arrête la pyramide quand un niveau a moins de points que ceci.
const MIN_POINTS: usize = 128;

/// Résumé d'un segment du morceau (valeurs 0..255 pour 0..1).
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct WavePoint {
    /// Pic absolu `[gauche, droite]`.
    pub peak: [u8; 2],
    /// Valeur RMS `[gauche, droite]`.
    pub rms: [u8; 2],
    /// Pic par bande `[basses, médiums, aigus]` (sur le mix mono).
    pub bands: [u8; 3],
}

impl WavePoint {
    fn merge(a: WavePoint, b: WavePoint) -> WavePoint {
        let max2 = |x: [u8; 2], y: [u8; 2]| [x[0].max(y[0]), x[1].max(y[1])];
        let rms = |x: u8, y: u8| {
            let (x, y) = (x as f32, y as f32);
            ((x * x + y * y) * 0.5).sqrt().round() as u8
        };
        WavePoint {
            peak: max2(a.peak, b.peak),
            rms: [rms(a.rms[0], b.rms[0]), rms(a.rms[1], b.rms[1])],
            bands: std::array::from_fn(|i| a.bands[i].max(b.bands[i])),
        }
    }
}

/// Un niveau de résolution de la forme d'onde.
pub struct WaveLevel {
    pub frames_per_point: usize,
    pub points: Vec<WavePoint>,
}

/// Forme d'onde complète d'un morceau, à plusieurs résolutions.
pub struct Waveform {
    /// Longueur du morceau, en frames.
    pub frames: usize,
    pub sample_rate: u32,
    /// Du plus fin au plus grossier.
    levels: Vec<WaveLevel>,
}

impl Waveform {
    /// Calcule la forme d'onde d'un buffer stéréo entrelacé.
    pub fn compute(samples: &[f32], sample_rate: u32) -> Self {
        let frames = samples.len() / 2;
        let mut bands = ThreeBandEq::new(sample_rate as f64);
        let to_u8 = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;

        let base: Vec<WavePoint> = samples
            .chunks(BASE_FRAMES_PER_POINT * 2)
            .map(|segment| {
                let mut peak = [0.0f32; 2];
                let mut sum_sq = [0.0f32; 2];
                let mut band_peak = [0.0f32; 3];
                for frame in segment.as_chunks::<2>().0 {
                    for ch in 0..2 {
                        peak[ch] = peak[ch].max(frame[ch].abs());
                        sum_sq[ch] += frame[ch] * frame[ch];
                    }
                    let split = bands.split(0.5 * (frame[0] + frame[1]));
                    for (acc, value) in band_peak.iter_mut().zip(split) {
                        *acc = acc.max(value.abs());
                    }
                }
                let count = (segment.len() / 2).max(1) as f32;
                WavePoint {
                    peak: peak.map(to_u8),
                    rms: sum_sq.map(|s| to_u8((s / count).sqrt())),
                    bands: band_peak.map(to_u8),
                }
            })
            .collect();

        let mut levels = vec![WaveLevel {
            frames_per_point: BASE_FRAMES_PER_POINT,
            points: base,
        }];
        loop {
            let last = levels.last().expect("au moins un niveau");
            if last.points.len() < MIN_POINTS * 2 {
                break;
            }
            let points = last
                .points
                .chunks(2)
                .map(|pair| {
                    if pair.len() == 2 {
                        WavePoint::merge(pair[0], pair[1])
                    } else {
                        pair[0]
                    }
                })
                .collect();
            let frames_per_point = last.frames_per_point * 2;
            levels.push(WaveLevel {
                frames_per_point,
                points,
            });
        }

        Self {
            frames,
            sample_rate,
            levels,
        }
    }

    /// Niveau le plus fin dont un point couvre au moins un pixel
    /// (`frames_per_pixel` frames), pour dessiner des colonnes d'au moins 1 px.
    pub fn level_for(&self, frames_per_pixel: f64) -> &WaveLevel {
        self.levels
            .iter()
            .find(|level| level.frames_per_point as f64 >= frames_per_pixel)
            .unwrap_or_else(|| self.levels.last().expect("au moins un niveau"))
    }

    pub fn duration_secs(&self) -> f64 {
        self.frames as f64 / self.sample_rate.max(1) as f64
    }

    /// Début et fin de la musique, en secondes : premier et dernier passage
    /// au-dessus d'environ −36 dBFS (le silence des bords est ignoré).
    /// Morceau entièrement silencieux : (0, durée).
    pub fn music_bounds(&self) -> (f64, f64) {
        const THRESHOLD: u8 = 4; // ≈ 0,016 ≈ −36 dBFS.
        let level = &self.levels[0];
        let audible = |point: &WavePoint| point.peak[0].max(point.peak[1]) >= THRESHOLD;
        let rate = self.sample_rate.max(1) as f64;
        match (
            level.points.iter().position(audible),
            level.points.iter().rposition(audible),
        ) {
            (Some(first), Some(last)) => {
                let start = (first * level.frames_per_point) as f64 / rate;
                let end = (((last + 1) * level.frames_per_point).min(self.frames)) as f64 / rate;
                (start, end)
            }
            _ => (0.0, self.duration_secs()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pyramid_and_values() {
        let sr = 48_000u32;
        let frames = sr as usize * 10;
        // Gauche : sinus 100 Hz d'amplitude 0,8 ; droite : silence.
        let samples: Vec<f32> = (0..frames)
            .flat_map(|n| {
                let v = 0.8 * (2.0 * std::f32::consts::PI * 100.0 * n as f32 / sr as f32).sin();
                [v, 0.0]
            })
            .collect();
        let wf = Waveform::compute(&samples, sr);
        assert_eq!(wf.frames, frames);
        assert!((wf.duration_secs() - 10.0).abs() < 1e-9);
        assert!(wf.levels.len() > 3);
        assert_eq!(wf.levels[0].points.len(), frames / 64);
        for pair in wf.levels.windows(2) {
            assert_eq!(pair[1].frames_per_point, pair[0].frames_per_point * 2);
        }

        // Vue d'ensemble : pic ≈ 0,8 à gauche, RMS ≈ 0,8/√2, rien à droite.
        let overview = wf.level_for(frames as f64 / 1000.0);
        assert!(overview.frames_per_point as f64 >= frames as f64 / 1000.0);
        let p = overview.points[overview.points.len() / 2];
        assert!((p.peak[0] as f32 / 255.0 - 0.8).abs() < 0.02);
        assert!((p.rms[0] as f32 / 255.0 - 0.566).abs() < 0.03);
        assert_eq!(p.peak[1], 0);
        // 100 Hz : l'énergie est dans la bande basse.
        assert!(p.bands[0] > 80 && p.bands[1] < 20 && p.bands[2] < 5, "{:?}", p.bands);

        // Zoom maximal : niveau le plus fin.
        assert_eq!(wf.level_for(10.0).frames_per_point, 64);
    }

    #[test]
    fn music_bounds_skip_silence() {
        let sr = 48_000u32;
        // 1 s de silence, 2 s de son, 0,5 s de silence.
        let samples: Vec<f32> = (0..(sr as usize * 7 / 2))
            .flat_map(|n| {
                let t = n as f32 / sr as f32;
                let v = if (1.0..3.0).contains(&t) {
                    0.5 * (t * 2000.0).sin()
                } else {
                    0.0
                };
                [v, v]
            })
            .collect();
        let (start, end) = Waveform::compute(&samples, sr).music_bounds();
        assert!((start - 1.0).abs() < 0.01, "début {start}");
        assert!((end - 3.0).abs() < 0.01, "fin {end}");
        let (start, end) = Waveform::compute(&vec![0.0; 9600], sr).music_bounds();
        assert_eq!((start, end), (0.0, 0.1));
    }
}
