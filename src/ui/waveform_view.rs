//! Dessin des formes d'onde (vue d'ensemble et vue zoomée).
//!
//! Moitié haute = canal gauche, moitié basse = canal droit. Pour chaque colonne :
//! une barre sombre jusqu'au pic et une barre vive jusqu'au RMS, colorées selon
//! la répartition de l'énergie (basses → rouge, médiums → vert, aigus → bleu).

use egui::{Color32, Mesh, Painter, Rect, Shape, pos2};

use crate::waveform::{WavePoint, Waveform};

/// Couleurs associées aux bandes basses / médiums / aigus.
const BAND_COLORS: [[f32; 3]; 3] = [[1.0, 0.22, 0.12], [0.2, 0.95, 0.3], [0.25, 0.5, 1.0]];
/// Les aigus portent naturellement moins d'énergie : on les rehausse pour la teinte.
const BAND_WEIGHTS: [f32; 3] = [1.0, 1.5, 2.6];

/// Teinte d'un point selon l'énergie par bande, normalisée (composante max = 1).
fn band_tint(point: &WavePoint) -> [f32; 3] {
    let mut rgb = [0.0f32; 3];
    for band in 0..3 {
        let energy = point.bands[band] as f32 * BAND_WEIGHTS[band];
        for (acc, component) in rgb.iter_mut().zip(BAND_COLORS[band]) {
            *acc += energy * component;
        }
    }
    let max = rgb.iter().copied().fold(0.0, f32::max);
    if max <= 0.0 {
        [0.5, 0.5, 0.5]
    } else {
        rgb.map(|c| c / max)
    }
}

fn shade(tint: [f32; 3], brightness: f32) -> Color32 {
    let to_u8 = |c: f32| (c * brightness * 255.0).clamp(0.0, 255.0) as u8;
    Color32::from_rgb(to_u8(tint[0]), to_u8(tint[1]), to_u8(tint[2]))
}

/// Dessine la portion de forme d'onde commençant à `first_frame` (peut être
/// négatif ou dépasser la fin) à l'échelle `frames_per_px`.
///
/// `dim_before` : les frames situées avant cette position (déjà jouées) sont
/// assombries.
pub fn paint_waveform(
    painter: &Painter,
    rect: Rect,
    waveform: &Waveform,
    first_frame: f64,
    frames_per_px: f64,
    dim_before: Option<f64>,
) {
    if frames_per_px <= 0.0 || rect.width() <= 0.0 {
        return;
    }
    let level = waveform.level_for(frames_per_px);
    let step = level.frames_per_point as f64;
    let last_frame = first_frame + rect.width() as f64 * frames_per_px;

    // Les colonnes sont alignées sur la grille des points (et non sur les
    // pixels) : la forme reste stable pendant le défilement.
    let start = (first_frame / step).floor().max(0.0) as usize;
    let end = ((last_frame / step).ceil().max(0.0) as usize).min(level.points.len());
    if start >= end {
        return;
    }

    let mid = rect.center().y;
    let half = rect.height() * 0.5 - 1.0;
    let x_of = |frame: f64| rect.left() + ((frame - first_frame) / frames_per_px) as f32;

    let mut mesh = Mesh::default();
    for (index, point) in level.points[start..end].iter().enumerate() {
        let frame = (start + index) as f64 * step;
        let (x0, x1) = (x_of(frame), x_of(frame + step));
        let tint = band_tint(point);
        let dim = if dim_before.is_some_and(|limit| frame < limit) {
            0.4
        } else {
            1.0
        };

        let height = |value: u8| value as f32 / 255.0 * half;
        let peak = Rect::from_min_max(
            pos2(x0, mid - height(point.peak[0])),
            pos2(x1, mid + height(point.peak[1])),
        );
        let rms = Rect::from_min_max(
            pos2(x0, mid - height(point.rms[0])),
            pos2(x1, mid + height(point.rms[1])),
        );
        mesh.add_colored_rect(peak, shade(tint, 0.5 * dim));
        mesh.add_colored_rect(rms, shade(tint, 1.0 * dim));
    }
    painter.add(Shape::mesh(mesh));
}
