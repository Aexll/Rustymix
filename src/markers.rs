//! Marqueurs (hot cues) : jusqu'à [`NUM_MARKERS`] positions par morceau, où
//! l'on saute directement. Ils sont enregistrés par morceau dans
//! `markers.toml`, à côté de la configuration.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const NUM_MARKERS: usize = 8;

/// Position de chaque marqueur, en secondes depuis le début du morceau.
pub type Markers = [Option<f64>; NUM_MARKERS];

/// Marqueurs spéciaux, placés automatiquement : début et fin de la musique
/// (premier et dernier passage audible). Ils suivent les 8 marqueurs dans les
/// [`Triggers`].
pub const MARKER_START: usize = NUM_MARKERS;
pub const MARKER_END: usize = NUM_MARKERS + 1;
pub const NUM_TRIGGERS: usize = NUM_MARKERS + 2;

/// Positions des 8 marqueurs puis du début et de la fin de la musique.
pub type Triggers = [Option<f64>; NUM_TRIGGERS];

/// Nom court d'un marqueur (« M1 » à « M8 », « DÉBUT », « FIN »).
pub fn short_name(marker: usize) -> String {
    match marker {
        MARKER_START => "DÉBUT".into(),
        MARKER_END => "FIN".into(),
        slot => format!("M{}", slot + 1),
    }
}

/// Nom complet d'un marqueur.
pub fn long_name(marker: usize) -> String {
    match marker {
        MARKER_START => "début de la musique".into(),
        MARKER_END => "fin de la musique".into(),
        slot => format!("marqueur {}", slot + 1),
    }
}

#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MarkerStore {
    /// Chemin du morceau → numéro du marqueur (« 1 » à « 8 ») → secondes.
    #[serde(default)]
    tracks: BTreeMap<String, BTreeMap<String, f64>>,
    #[serde(skip)]
    path: Option<PathBuf>,
}

impl MarkerStore {
    /// Charge le fichier ; un fichier absent ou illisible donne un magasin vide.
    pub fn load(path: Option<PathBuf>) -> Self {
        let mut store: Self = path
            .as_ref()
            .and_then(|path| fs::read_to_string(path).ok())
            .and_then(|text| toml::from_str(&text).ok())
            .unwrap_or_default();
        store.path = path;
        store
    }

    pub fn get(&self, track: &Path) -> Markers {
        let mut markers = [None; NUM_MARKERS];
        if let Some(saved) = self.tracks.get(&key(track)) {
            for (slot, marker) in markers.iter_mut().enumerate() {
                *marker = saved.get(&(slot + 1).to_string()).copied();
            }
        }
        markers
    }

    pub fn set(&mut self, track: &Path, markers: &Markers) {
        let saved: BTreeMap<String, f64> = markers
            .iter()
            .enumerate()
            .filter_map(|(slot, marker)| Some(((slot + 1).to_string(), (*marker)?)))
            .collect();
        if saved.is_empty() {
            self.tracks.remove(&key(track));
        } else {
            self.tracks.insert(key(track), saved);
        }
    }

    pub fn save(&self) -> Result<(), String> {
        let path = self.path.as_ref().ok_or("dossier de configuration introuvable")?;
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let text = toml::to_string_pretty(self).map_err(|e| e.to_string())?;
        fs::write(path, text).map_err(|e| e.to_string())
    }
}

fn key(track: &Path) -> String {
    track.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_get_and_toml_round_trip() {
        let track = Path::new("C:\\Musique\\Daft Punk - One More Time.mp3");
        let mut markers = [None; NUM_MARKERS];
        markers[0] = Some(12.5);
        markers[3] = Some(64.0);

        let mut store = MarkerStore::default();
        store.set(track, &markers);
        assert_eq!(store.get(track), markers);
        assert_eq!(store.get(Path::new("autre.mp3")), [None; NUM_MARKERS]);

        let text = toml::to_string_pretty(&store).unwrap();
        let reloaded: MarkerStore = toml::from_str(&text).unwrap();
        assert_eq!(reloaded.get(track), markers);

        // Tous les marqueurs effacés : le morceau disparaît du fichier.
        store.set(track, &[None; NUM_MARKERS]);
        assert_eq!(store, MarkerStore::default());
    }
}
