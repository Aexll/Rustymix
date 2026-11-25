//! Séparation des morceaux en pistes (« stems » : voix, batterie, basse, et le
//! reste) par le modèle d'IA Demucs (Meta), lancé en arrière-plan par Python.
//!
//! * Le morceau est décodé par Rustymix lui-même (44,1 kHz, la fréquence du
//!   modèle) et transmis brut au script : les stems restent ainsi alignés à
//!   l'échantillon près sur le mix joué par le moteur.
//! * Les stems sont gardés en cache (FLAC, dossier `stems` à côté de la
//!   configuration) : un morceau déjà séparé se recharge en quelques secondes.
//!
//! Prérequis : Python avec `pip install demucs soundfile` (PyTorch avec CUDA
//! pour une séparation rapide sur carte graphique). La variable d'environnement
//! `RUSTYMIX_PYTHON` permet de choisir l'interpréteur.

use std::fs;
use std::hash::Hasher;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::thread;

use crossbeam_channel::{Receiver, Sender, unbounded};

use crate::audio::decoder::decode_file;
use crate::audio::engine::{STORED_STEMS, Stems};
use crate::config::Config;

/// Fréquence d'échantillonnage des modèles Demucs.
const DEMUCS_RATE: u32 = 44_100;
/// Modèle utilisé : Hybrid Transformer Demucs (4 stems).
const MODEL: &str = "htdemucs";
/// Fichiers produits par le script, dans l'ordre des stems du moteur.
const STEM_FILES: [&str; STORED_STEMS] = ["vocals.flac", "drums.flac", "bass.flac"];
const SCRIPT: &str = include_str!("separate_stems.py");

pub enum StemEvent {
    /// Avancement de la séparation, de 0 à 1.
    Progress { deck: usize, id: u64, fraction: f32 },
    Done {
        deck: usize,
        id: u64,
        result: Result<Arc<Stems>, String>,
    },
}

pub struct Separator {
    tx: Sender<StemEvent>,
    rx: Receiver<StemEvent>,
}

impl Default for Separator {
    fn default() -> Self {
        let (tx, rx) = unbounded();
        Self { tx, rx }
    }
}

impl Separator {
    /// Sépare (ou relit depuis le cache) le morceau `path`, chargé sur la
    /// platine `deck` sous l'identifiant `id` ; `frames` : longueur du mix à
    /// `sample_rate`, sur laquelle les stems sont ajustés.
    pub fn request(
        &self,
        deck: usize,
        id: u64,
        path: PathBuf,
        frames: usize,
        sample_rate: u32,
        wake: impl Fn() + Send + Sync + 'static,
    ) {
        let tx = self.tx.clone();
        thread::spawn(move || {
            let progress = |fraction: f32| {
                let _ = tx.send(StemEvent::Progress { deck, id, fraction });
                wake();
            };
            let result = separate(&path, frames, sample_rate, &progress).map(Arc::new);
            let _ = tx.send(StemEvent::Done { deck, id, result });
            wake();
        });
    }

    pub fn poll(&self) -> Option<StemEvent> {
        self.rx.try_recv().ok()
    }
}

/// Vrai si les stems de ce morceau sont déjà en cache.
pub fn is_cached(path: &Path) -> bool {
    cache_dir(path).is_some_and(|dir| has_all_stems(&dir))
}

fn has_all_stems(dir: &Path) -> bool {
    STEM_FILES.iter().all(|name| dir.join(name).is_file())
}

/// Dossier de cache d'un morceau : clé = chemin + taille + date de modification
/// (un fichier modifié est donc séparé à nouveau).
fn cache_dir(path: &Path) -> Option<PathBuf> {
    let metadata = fs::metadata(path).ok()?;
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_secs());
    let mut hasher = Fnv64::default();
    hasher.write(path.to_string_lossy().as_bytes());
    hasher.write_u64(metadata.len());
    hasher.write_u64(modified);
    Some(Config::stems_dir()?.join(format!("{:016x}", hasher.finish())))
}

fn separate(path: &Path, frames: usize, sample_rate: u32, progress: &dyn Fn(f32)) -> Result<Stems, String> {
    let dir = cache_dir(path).ok_or("dossier de cache des stems introuvable")?;
    if !has_all_stems(&dir) {
        run_demucs(path, &dir, progress)?;
    }
    let mut stems: [Vec<f32>; STORED_STEMS] = Default::default();
    for (stem, name) in stems.iter_mut().zip(STEM_FILES) {
        *stem = decode_file(&dir.join(name), sample_rate)
            .map_err(|error| format!("stem {name} illisible : {error}"))?
            .samples;
    }
    Ok(Stems::new(stems, frames))
}

fn python() -> String {
    std::env::var("RUSTYMIX_PYTHON").unwrap_or_else(|_| if cfg!(windows) { "python" } else { "python3" }.into())
}

fn run_demucs(path: &Path, dir: &Path, progress: &dyn Fn(f32)) -> Result<(), String> {
    fs::create_dir_all(dir).map_err(|error| format!("cache des stems : {error}"))?;

    // Entrée : le morceau décodé par Rustymix, en float32 brut à 44,1 kHz.
    let track = decode_file(path, DEMUCS_RATE)?;
    let input = dir.join("input.f32");
    let bytes: Vec<u8> = track.samples.iter().flat_map(|sample| sample.to_le_bytes()).collect();
    drop(track);
    fs::write(&input, bytes).map_err(|error| format!("cache des stems : {error}"))?;

    let script = dir.with_file_name("separate_stems.py");
    fs::write(&script, SCRIPT).map_err(|error| format!("cache des stems : {error}"))?;

    let result = run_script(&script, &input, dir, progress);
    let _ = fs::remove_file(&input);
    result
}

fn run_script(script: &Path, input: &Path, dir: &Path, progress: &dyn Fn(f32)) -> Result<(), String> {
    let python = python();
    let mut command = Command::new(&python);
    command
        .arg(script)
        .arg(input)
        .arg(dir)
        .arg(MODEL)
        .env("PYTHONIOENCODING", "utf-8")
        .env("PYTHONUTF8", "1")
        .env("HF_HUB_DISABLE_SYMLINKS_WARNING", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = command.spawn().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            format!("{python} introuvable : installez Python, puis « pip install demucs soundfile ».")
        } else {
            format!("impossible de lancer {python} : {error}")
        }
    })?;

    let mut stderr = child.stderr.take().expect("stderr redirigé");
    let errors = thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stderr.read_to_end(&mut bytes);
        String::from_utf8_lossy(&bytes).into_owned()
    });

    let stdout = child.stdout.take().expect("stdout redirigé");
    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
        if let Some(fraction) = line
            .strip_prefix("PROGRESS ")
            .and_then(|value| value.trim().parse().ok())
        {
            progress(fraction);
        }
    }

    let status = child.wait().map_err(|error| error.to_string())?;
    let errors = errors.join().unwrap_or_default();
    if status.success() && has_all_stems(dir) {
        Ok(())
    } else {
        Err(script_error(&errors))
    }
}

/// Message d'erreur lisible à partir de la sortie d'erreur du script.
fn script_error(stderr: &str) -> String {
    if stderr.contains("No module named 'demucs'") || stderr.contains("No module named 'soundfile'") {
        return "Demucs n'est pas installé : pip install demucs soundfile".into();
    }
    if stderr.contains("No module named 'torch'") {
        return "PyTorch n'est pas installé (voir pytorch.org), puis : pip install demucs soundfile".into();
    }
    if stderr.contains("out of memory") {
        return "séparation : mémoire de la carte graphique insuffisante".into();
    }
    let last = stderr
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty() && !line.starts_with("warnings.warn"))
        .unwrap_or("erreur inconnue");
    format!("séparation impossible : {last}")
}

/// Hachage FNV-1a 64 bits : stable d'une version de Rust à l'autre, contrairement
/// au hachage par défaut de la bibliothèque standard (la clé du cache doit durer).
struct Fnv64(u64);

impl Default for Fnv64 {
    fn default() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }
}

impl Hasher for Fnv64 {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.0 = (self.0 ^ byte as u64).wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv_is_stable() {
        let mut hasher = Fnv64::default();
        hasher.write(b"hello");
        assert_eq!(hasher.finish(), 0xa430_d846_80aa_bd0b);
    }

    #[test]
    fn readable_errors() {
        let missing = "Traceback (most recent call last):\n  ...\nModuleNotFoundError: No module named 'demucs'\n";
        assert!(script_error(missing).contains("pip install demucs"));
        assert_eq!(
            script_error("Traceback\nRuntimeError: boom\n"),
            "séparation impossible : RuntimeError: boom"
        );
    }

    /// Chaîne complète sur un vrai fichier (Python + Demucs requis) :
    /// `RUSTYMIX_STEM_TEST_FILE=morceau.mp3 RUSTYMIX_CONFIG=<dossier jetable>/config.toml
    /// cargo test -- --ignored separates_real_file`
    #[test]
    #[ignore]
    fn separates_real_file() {
        let path = PathBuf::from(std::env::var("RUSTYMIX_STEM_TEST_FILE").expect("RUSTYMIX_STEM_TEST_FILE"));
        let mix = decode_file(&path, 48_000).unwrap();
        let frames = mix.frames();
        let steps = std::sync::Mutex::new(Vec::new());
        let stems = separate(&path, frames, 48_000, &|f| steps.lock().unwrap().push(f)).unwrap();
        assert!(is_cached(&path));
        assert!(steps.lock().unwrap().last().is_some_and(|f| *f >= 1.0) || steps.lock().unwrap().is_empty());
        // Les stems sont alignés sur le mix : la voix + batterie + basse
        // expliquent une bonne part de son énergie, pas du bruit décalé.
        let gains = [1.0, 1.0, 1.0, 0.0];
        let source = crate::audio::engine::tests_support::source_frames(&mix, &stems, gains, frames);
        let energy = |x: &[f32]| x.iter().map(|v| v * v).sum::<f32>();
        let residual: Vec<f32> = mix.samples.iter().zip(&source).map(|(a, b)| a - b).collect();
        eprintln!("énergie mix {} / résidu {}", energy(&mix.samples), energy(&residual));
        assert!(energy(&residual) < 0.5 * energy(&mix.samples));
        // Rechargement depuis le cache.
        let again = separate(&path, frames, 48_000, &|_| {}).unwrap();
        drop(again);
    }
}
