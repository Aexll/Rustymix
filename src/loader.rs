//! Chargement des morceaux en arrière-plan : décodage, rééchantillonnage et
//! calcul de la forme d'onde se font hors du thread d'interface.

use std::sync::Arc;
use std::thread;

use crossbeam_channel::{Receiver, Sender, unbounded};

use crate::analysis::{Analysis, analyze, mono_excerpt};
use crate::audio::decoder::decode_file;
use crate::audio::engine::TrackData;
use crate::library::TrackInfo;
use crate::waveform::Waveform;

/// Résultat d'une demande de chargement.
pub struct LoadResult {
    pub deck: usize,
    /// Identifiant retourné par [`Loader::request`].
    pub id: u64,
    pub info: TrackInfo,
    pub result: Result<(Arc<TrackData>, Arc<Waveform>, Analysis), String>,
}

pub struct Loader {
    tx: Sender<LoadResult>,
    rx: Receiver<LoadResult>,
    next_id: u64,
}

impl Default for Loader {
    fn default() -> Self {
        let (tx, rx) = unbounded();
        // L'identifiant 0 signifie « aucun morceau » côté moteur.
        Self { tx, rx, next_id: 1 }
    }
}

impl Loader {
    /// Lance le chargement de `info` pour la platine `deck` et retourne son
    /// identifiant. `wake` est appelé quand le résultat est disponible.
    pub fn request(&mut self, deck: usize, info: TrackInfo, sample_rate: u32, wake: impl Fn() + Send + 'static) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        let tx = self.tx.clone();
        thread::spawn(move || {
            let result = decode_file(&info.path, sample_rate).map(|track| {
                let waveform = Waveform::compute(&track.samples, track.sample_rate);
                // Tempo et tonalité : repris de la bibliothèque s'ils y sont déjà.
                let analysis = info
                    .analysis
                    .unwrap_or_else(|| analyze(&mono_excerpt(&track.samples, track.sample_rate), track.sample_rate));
                (Arc::new(track), Arc::new(waveform), analysis)
            });
            let _ = tx.send(LoadResult { deck, id, info, result });
            wake();
        });
        id
    }

    /// Prochain résultat disponible, s'il y en a un.
    pub fn poll(&self) -> Option<LoadResult> {
        self.rx.try_recv().ok()
    }
}
