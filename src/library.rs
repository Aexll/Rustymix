//! Bibliothèque de sons : scan récursif des dossiers en arrière-plan, lecture
//! des tags, recherche instantanée et tri par colonne.
//!
//! Le tempo et la tonalité de chaque morceau sont détectés au scan par
//! quelques threads d'analyse ; les résultats sont mis en cache sur disque
//! (clé : chemin + taille + date de modification) pour les scans suivants.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::UNIX_EPOCH;

use crossbeam_channel::{Receiver, Sender, unbounded};
use lofty::prelude::*;
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::analysis::{ANALYSIS_SECONDS, Analysis, analyze};
use crate::audio::decoder::{SUPPORTED_EXTENSIONS, decode_excerpt_mono};

/// Nombre de résultats d'analyse accumulés avant d'écrire le cache sur disque.
const CACHE_SAVE_EVERY: usize = 25;

/// Nombre de morceaux envoyés d'un coup à l'UI pendant un scan.
const BATCH_SIZE: usize = 64;

/// Un morceau de la bibliothèque.
#[derive(Clone, Debug)]
pub struct TrackInfo {
    pub path: PathBuf,
    /// Titre des tags, ou nom de fichier sans extension à défaut.
    pub title: String,
    pub artist: String,
    /// Durée en secondes, si elle a pu être lue.
    pub duration: Option<f32>,
    pub file_name: String,
    /// Tempo et tonalité ; `None` tant que l'analyse n'est pas terminée.
    pub analysis: Option<Analysis>,
    /// Titre + artiste + fichier en minuscules, pour la recherche.
    search_key: String,
}

impl TrackInfo {
    /// Lit les tags et la durée du fichier ; ne lit pas l'audio.
    pub fn read(path: &Path) -> Self {
        let file_name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut title = String::new();
        let mut artist = String::new();
        let mut duration = None;

        if let Ok(tagged) = lofty::read_from_path(path) {
            let secs = tagged.properties().duration().as_secs_f32();
            duration = (secs > 0.0).then_some(secs);
            if let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) {
                title = tag.title().map(|t| t.trim().to_string()).unwrap_or_default();
                artist = tag.artist().map(|a| a.trim().to_string()).unwrap_or_default();
            }
        }
        if title.is_empty() {
            title = path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| file_name.clone());
        }

        Self::new(path.to_path_buf(), title, artist, duration, file_name)
    }

    fn new(path: PathBuf, title: String, artist: String, duration: Option<f32>, file_name: String) -> Self {
        let search_key = format!("{title}\n{artist}\n{file_name}").to_lowercase();
        Self {
            path,
            title,
            artist,
            duration,
            file_name,
            analysis: None,
            search_key,
        }
    }
}

pub fn is_supported(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| SUPPORTED_EXTENSIONS.iter().any(|s| ext.eq_ignore_ascii_case(s)))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortColumn {
    Title,
    Artist,
    Duration,
    Bpm,
    Key,
    FileName,
}

/// Empreinte d'un fichier : (taille, date de modification en secondes).
type FileStamp = (u64, u64);

fn file_stamp(path: &Path) -> FileStamp {
    let Ok(meta) = fs::metadata(path) else { return (0, 0) };
    let modified = meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok());
    (meta.len(), modified.map_or(0, |d| d.as_secs()))
}

/// Une ligne du cache d'analyse sur disque.
#[derive(Clone, Serialize, Deserialize)]
struct CacheRow {
    path: PathBuf,
    size: u64,
    modified: u64,
    analysis: Analysis,
}

#[derive(Default, Serialize, Deserialize)]
struct CacheFile {
    #[serde(default)]
    tracks: Vec<CacheRow>,
}

type CacheMap = HashMap<PathBuf, (FileStamp, Analysis)>;

fn load_cache(path: Option<&Path>) -> CacheMap {
    path.and_then(|p| fs::read_to_string(p).ok())
        .and_then(|text| toml::from_str::<CacheFile>(&text).ok())
        .map(|file| {
            file.tracks
                .into_iter()
                .map(|r| (r.path, ((r.size, r.modified), r.analysis)))
                .collect()
        })
        .unwrap_or_default()
}

/// Analyse le début d'un fichier ; un fichier illisible donne une analyse vide.
fn analyze_file(path: &Path) -> Analysis {
    match decode_excerpt_mono(path, ANALYSIS_SECONDS) {
        Ok((mono, rate)) => analyze(&mono, rate),
        Err(_) => Analysis::default(),
    }
}

/// Morceau à analyser : (numéro de scan, chemin, empreinte).
type Job = (u64, PathBuf, FileStamp);

enum ScanMessage {
    Batch(u64, Vec<TrackInfo>),
    Done(u64),
    Analyzed(u64, PathBuf, FileStamp, Analysis),
}

/// Bibliothèque : liste complète + vue filtrée et triée.
pub struct Library {
    tracks: Vec<TrackInfo>,
    /// Indices dans `tracks`, filtrés par la recherche et triés.
    view: Vec<usize>,
    view_dirty: bool,
    query: String,
    sort: (SortColumn, bool),
    scanning: bool,
    /// Numéro du scan courant ; un scan plus ancien s'arrête de lui-même.
    generation: Arc<AtomicU64>,
    tx: Sender<ScanMessage>,
    rx: Receiver<ScanMessage>,
    /// Position de chaque morceau dans `tracks`.
    index: HashMap<PathBuf, usize>,
    /// Analyses arrivées avant le lot contenant leur morceau.
    early: HashMap<PathBuf, Analysis>,
    /// Nombre de morceaux encore en attente d'analyse.
    pending: usize,
    cache: Arc<Mutex<CacheMap>>,
    cache_path: Option<PathBuf>,
    cache_unsaved: usize,
    jobs_tx: Sender<Job>,
    jobs_rx: Receiver<Job>,
    workers_started: bool,
}

impl Default for Library {
    fn default() -> Self {
        Self::new(None)
    }
}

impl Library {
    /// `cache_path` : fichier où conserver les analyses (aucun si `None`).
    pub fn new(cache_path: Option<PathBuf>) -> Self {
        let (tx, rx) = unbounded();
        let (jobs_tx, jobs_rx) = unbounded();
        Self {
            index: HashMap::new(),
            early: HashMap::new(),
            pending: 0,
            cache: Arc::new(Mutex::new(load_cache(cache_path.as_deref()))),
            cache_path,
            cache_unsaved: 0,
            jobs_tx,
            jobs_rx,
            workers_started: false,
            tracks: Vec::new(),
            view: Vec::new(),
            view_dirty: false,
            query: String::new(),
            sort: (SortColumn::Title, true),
            scanning: false,
            generation: Arc::new(AtomicU64::new(0)),
            tx,
            rx,
        }
    }

    /// Lance (une seule fois) les threads d'analyse.
    fn start_workers(&mut self, wake: Arc<dyn Fn() + Send + Sync>) {
        if std::mem::replace(&mut self.workers_started, true) {
            return;
        }
        let count = thread::available_parallelism().map_or(2, |n| n.get() / 2).clamp(1, 4);
        for _ in 0..count {
            let (jobs, tx) = (self.jobs_rx.clone(), self.tx.clone());
            let (current, wake) = (self.generation.clone(), wake.clone());
            thread::spawn(move || {
                while let Ok((generation, path, stamp)) = jobs.recv() {
                    if current.load(Relaxed) != generation {
                        continue; // scan périmé
                    }
                    let analysis = analyze_file(&path);
                    if tx
                        .send(ScanMessage::Analyzed(generation, path, stamp, analysis))
                        .is_err()
                    {
                        return;
                    }
                    wake();
                }
            });
        }
    }

    fn save_cache(&mut self) {
        self.cache_unsaved = 0;
        let Some(path) = &self.cache_path else { return };
        let Ok(cache) = self.cache.lock() else { return };
        let mut tracks: Vec<CacheRow> = cache
            .iter()
            .map(|(path, &((size, modified), analysis))| CacheRow {
                path: path.clone(),
                size,
                modified,
                analysis,
            })
            .collect();
        tracks.sort_by(|a, b| a.path.cmp(&b.path));
        if let Ok(text) = toml::to_string(&CacheFile { tracks }) {
            if let Some(dir) = path.parent() {
                let _ = fs::create_dir_all(dir);
            }
            let _ = fs::write(path, text);
        }
    }

    /// Vide la bibliothèque et relance un scan récursif des dossiers en
    /// arrière-plan. `wake` est appelé à chaque lot de résultats (pour
    /// redessiner l'interface).
    pub fn rescan(&mut self, folders: &[PathBuf], wake: impl Fn() + Send + Sync + 'static) {
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(wake);
        self.start_workers(wake.clone());
        let generation = self.generation.fetch_add(1, Relaxed) + 1;
        self.tracks.clear();
        self.index.clear();
        self.early.clear();
        self.pending = 0;
        self.view_dirty = true;
        self.scanning = !folders.is_empty();
        if folders.is_empty() {
            return;
        }

        let folders = folders.to_vec();
        let current = self.generation.clone();
        let tx = self.tx.clone();
        let (cache, jobs) = (self.cache.clone(), self.jobs_tx.clone());
        thread::spawn(move || {
            let mut seen = HashSet::new();
            let mut batch = Vec::with_capacity(BATCH_SIZE);
            let files = folders
                .iter()
                .flat_map(|folder| WalkDir::new(folder).follow_links(true))
                .filter_map(Result::ok)
                .filter(|entry| entry.file_type().is_file() && is_supported(entry.path()));
            for entry in files {
                if current.load(Relaxed) != generation {
                    return; // un scan plus récent a été lancé
                }
                // Des dossiers racine imbriqués ne doivent pas créer de doublons.
                if !seen.insert(entry.path().to_path_buf()) {
                    continue;
                }
                let mut info = TrackInfo::read(entry.path());
                // Analyse déjà connue pour ce fichier inchangé ? Sinon, à faire.
                let stamp = file_stamp(entry.path());
                let cached = cache.lock().ok().and_then(|c| c.get(entry.path()).copied());
                match cached {
                    // Les analyses d'avant la mesure de l'énergie sont refaites
                    // (une seule fois : un morceau sans tempo n'en a pas besoin).
                    Some((cached_stamp, analysis))
                        if cached_stamp == stamp && (analysis.energy.is_some() || analysis.bpm.is_none()) =>
                    {
                        info.analysis = Some(analysis)
                    }
                    _ => {
                        let _ = jobs.send((generation, entry.path().to_path_buf(), stamp));
                    }
                }
                batch.push(info);
                if batch.len() >= BATCH_SIZE {
                    let _ = tx.send(ScanMessage::Batch(generation, std::mem::take(&mut batch)));
                    wake();
                }
            }
            let _ = tx.send(ScanMessage::Batch(generation, batch));
            let _ = tx.send(ScanMessage::Done(generation));
            wake();
        });
    }

    /// Intègre les résultats de scan arrivés depuis le dernier appel.
    pub fn poll(&mut self) {
        let generation = self.generation.load(Relaxed);
        while let Ok(message) = self.rx.try_recv() {
            match message {
                ScanMessage::Batch(g, batch) if g == generation => {
                    for mut info in batch {
                        if info.analysis.is_none() {
                            info.analysis = self.early.remove(&info.path);
                        }
                        self.pending += usize::from(info.analysis.is_none());
                        self.index.insert(info.path.clone(), self.tracks.len());
                        self.tracks.push(info);
                    }
                    self.view_dirty = true;
                }
                ScanMessage::Done(g) if g == generation => self.scanning = false,
                ScanMessage::Analyzed(g, path, stamp, analysis) => {
                    // Même périmé, le résultat reste valable pour le cache.
                    if let Ok(mut cache) = self.cache.lock() {
                        cache.insert(path.clone(), (stamp, analysis));
                    }
                    self.cache_unsaved += 1;
                    if g != generation {
                        continue;
                    }
                    match self.index.get(&path) {
                        Some(&i) => {
                            self.pending -= usize::from(self.tracks[i].analysis.is_none());
                            self.tracks[i].analysis = Some(analysis);
                            self.view_dirty |= matches!(self.sort.0, SortColumn::Bpm | SortColumn::Key);
                        }
                        None => {
                            self.early.insert(path, analysis);
                        }
                    }
                }
                _ => {} // résultat d'un scan périmé
            }
        }
        if self.cache_unsaved >= CACHE_SAVE_EVERY || (self.cache_unsaved > 0 && self.pending == 0 && !self.scanning) {
            self.save_cache();
        }
    }

    /// Nombre de morceaux dont le tempo et la tonalité restent à détecter.
    pub fn pending_analysis(&self) -> usize {
        self.pending
    }

    pub fn is_scanning(&self) -> bool {
        self.scanning
    }

    pub fn len(&self) -> usize {
        self.tracks.len()
    }

    pub fn track(&self, index: usize) -> Option<&TrackInfo> {
        self.tracks.get(index)
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn set_query(&mut self, query: &str) {
        if query != self.query {
            self.query = query.to_string();
            self.view_dirty = true;
        }
    }

    pub fn sort(&self) -> (SortColumn, bool) {
        self.sort
    }

    /// Trie par `column` ; un second clic sur la même colonne inverse l'ordre.
    pub fn toggle_sort(&mut self, column: SortColumn) {
        self.sort = if self.sort.0 == column {
            (column, !self.sort.1)
        } else {
            (column, true)
        };
        self.view_dirty = true;
    }

    /// Recalcule la vue filtrée/triée si quelque chose a changé.
    pub fn refresh(&mut self) {
        if self.view_dirty {
            self.rebuild_view();
            self.view_dirty = false;
        }
    }

    /// Indices (dans la bibliothèque) des morceaux à afficher, dans l'ordre.
    /// Appeler [`Library::refresh`] avant pour une vue à jour.
    pub fn view(&self) -> &[usize] {
        &self.view
    }

    fn rebuild_view(&mut self) {
        let words: Vec<String> = self
            .query
            .to_lowercase()
            .split_whitespace()
            .map(str::to_string)
            .collect();
        let tracks = &self.tracks;
        self.view.clear();
        self.view.extend(
            (0..tracks.len()).filter(|&i| words.iter().all(|word| tracks[i].search_key.contains(word.as_str()))),
        );

        let (column, ascending) = self.sort;
        self.view.sort_by(|&a, &b| {
            let (a, b) = (&tracks[a], &tracks[b]);
            let order = match column {
                SortColumn::Title => compare_text(&a.title, &b.title),
                SortColumn::Artist => compare_text(&a.artist, &b.artist),
                SortColumn::Duration => a
                    .duration
                    .unwrap_or(0.0)
                    .partial_cmp(&b.duration.unwrap_or(0.0))
                    .unwrap_or(Ordering::Equal),
                SortColumn::Bpm => {
                    let bpm = |t: &TrackInfo| t.analysis.and_then(|a| a.bpm).unwrap_or(0.0);
                    bpm(a).partial_cmp(&bpm(b)).unwrap_or(Ordering::Equal)
                }
                SortColumn::Key => {
                    let rank = |t: &TrackInfo| t.analysis.and_then(|a| a.key).map_or(u32::MAX, |k| k.sort_rank());
                    rank(a).cmp(&rank(b))
                }
                SortColumn::FileName => compare_text(&a.file_name, &b.file_name),
            };
            let order = if ascending { order } else { order.reverse() };
            order.then_with(|| compare_text(&a.file_name, &b.file_name))
        });
    }
}

/// Comparaison insensible à la casse.
fn compare_text(a: &str, b: &str) -> Ordering {
    a.chars()
        .flat_map(char::to_lowercase)
        .cmp(b.chars().flat_map(char::to_lowercase))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(title: &str, artist: &str, duration: f32, file: &str) -> TrackInfo {
        TrackInfo::new(
            PathBuf::from(file),
            title.into(),
            artist.into(),
            Some(duration),
            file.into(),
        )
    }

    fn library() -> Library {
        Library {
            tracks: vec![
                track("Zenith", "alpha", 200.0, "01.mp3"),
                track("around the World", "Daft Punk", 420.0, "02.flac"),
                track("Blue Monday", "New Order", 450.0, "03.wav"),
            ],
            view_dirty: true,
            ..Library::default()
        }
    }

    fn titles(lib: &mut Library) -> Vec<String> {
        lib.refresh();
        lib.view().iter().map(|&i| lib.tracks[i].title.clone()).collect()
    }

    #[test]
    fn sorts_by_column_case_insensitively() {
        let mut lib = library();
        assert_eq!(titles(&mut lib), ["around the World", "Blue Monday", "Zenith"]);
        lib.toggle_sort(SortColumn::Title);
        assert_eq!(titles(&mut lib), ["Zenith", "Blue Monday", "around the World"]);
        lib.toggle_sort(SortColumn::Duration);
        assert_eq!(titles(&mut lib), ["Zenith", "around the World", "Blue Monday"]);
    }

    #[test]
    fn filters_on_title_artist_and_file() {
        let mut lib = library();
        lib.set_query("DAFT");
        assert_eq!(titles(&mut lib), ["around the World"]);
        lib.set_query("03.wav");
        assert_eq!(titles(&mut lib), ["Blue Monday"]);
        lib.set_query("o order");
        assert_eq!(titles(&mut lib), ["Blue Monday"]);
        lib.set_query("");
        assert_eq!(titles(&mut lib).len(), 3);
    }

    #[test]
    fn recognises_supported_files() {
        assert!(is_supported(Path::new("a/b/track.MP3")));
        assert!(is_supported(Path::new("track.flac")));
        assert!(!is_supported(Path::new("cover.jpg")));
        assert!(!is_supported(Path::new("noext")));
    }
}
