//! Installation de morceaux depuis Internet, par des outils externes lancés
//! en arrière-plan :
//!
//! * **YouTube** : recherche et téléchargement par `yt-dlp` (conversion en MP3
//!   par `ffmpeg`) ; aucun compte nécessaire.
//! * **Spotify** : recherche par l'API Web de Spotify, téléchargement par
//!   `spotdl` (qui retrouve le son sur YouTube Music et y écrit les tags
//!   Spotify). Il faut ses propres identifiants d'application Spotify : ceux
//!   fournis par défaut avec spotdl sont partagés et presque toujours bloqués.

use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender, unbounded};
use serde::Deserialize;
use serde_json::Value;

/// Nombre de résultats demandés par recherche.
const SEARCH_LIMIT: usize = 10;
const YT_DLP: &str = "yt-dlp";
const SPOTDL: &str = "spotdl";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    YouTube,
    Spotify,
}

impl Source {
    pub fn name(self) -> &'static str {
        match self {
            Source::YouTube => "YouTube",
            Source::Spotify => "Spotify",
        }
    }
}

/// Un résultat de recherche.
#[derive(Clone, Debug)]
pub struct SearchResult {
    pub source: Source,
    pub title: String,
    /// Artistes (Spotify) ou chaîne (YouTube).
    pub artist: String,
    pub album: String,
    pub duration: Option<f32>,
    /// Adresse passée à l'outil de téléchargement ; identifie aussi le résultat.
    pub url: String,
    /// Pourquoi ce morceau est proposé (recommandations), sinon vide.
    pub note: String,
}

#[derive(Clone, Debug, Default)]
pub struct SpotifyCredentials {
    pub client_id: String,
    pub client_secret: String,
}

impl SpotifyCredentials {
    pub fn is_set(&self) -> bool {
        !self.client_id.trim().is_empty() && !self.client_secret.trim().is_empty()
    }
}

pub enum Event {
    Search {
        id: u64,
        result: Result<Vec<SearchResult>, String>,
    },
    /// Avancement d'un téléchargement, de 0 à 1.
    Progress {
        id: u64,
        fraction: f32,
    },
    Finished {
        id: u64,
        result: Result<(), String>,
    },
    /// Morceaux similaires (morceau reconnu, propositions).
    Similar {
        id: u64,
        result: Result<crate::recommend::Recommendations, String>,
    },
}

pub struct Downloader {
    tx: Sender<Event>,
    rx: Receiver<Event>,
    next_id: u64,
}

impl Default for Downloader {
    fn default() -> Self {
        let (tx, rx) = unbounded();
        Self { tx, rx, next_id: 1 }
    }
}

impl Downloader {
    fn next_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id - 1
    }

    /// Lance une recherche et retourne son identifiant.
    pub fn search(
        &mut self,
        source: Source,
        query: String,
        credentials: SpotifyCredentials,
        wake: impl Fn() + Send + 'static,
    ) -> u64 {
        let id = self.next_id();
        let tx = self.tx.clone();
        thread::spawn(move || {
            let result = match source {
                Source::YouTube => search_youtube(&query),
                Source::Spotify => search_spotify(&query, &credentials),
            };
            let _ = tx.send(Event::Search { id, result });
            wake();
        });
        id
    }

    /// Cherche des morceaux similaires à `seed` (Deezer) et retourne
    /// l'identifiant de la demande.
    pub fn similar(&mut self, seed: crate::recommend::Seed, wake: impl Fn() + Send + 'static) -> u64 {
        let id = self.next_id();
        let tx = self.tx.clone();
        thread::spawn(move || {
            let result = crate::recommend::recommend(&seed);
            let _ = tx.send(Event::Similar { id, result });
            wake();
        });
        id
    }

    /// Lance le téléchargement de `track` dans `folder` et retourne son identifiant.
    pub fn download(
        &mut self,
        track: SearchResult,
        folder: PathBuf,
        credentials: SpotifyCredentials,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> u64 {
        let id = self.next_id();
        let tx = self.tx.clone();
        thread::spawn(move || {
            let progress = |fraction: f32| {
                let _ = tx.send(Event::Progress { id, fraction });
                wake();
            };
            let result = match track.source {
                Source::YouTube => download_youtube(&track.url, &folder, &progress),
                Source::Spotify => download_spotify(&track.url, &folder, &credentials),
            };
            let _ = tx.send(Event::Finished { id, result });
            wake();
        });
        id
    }

    /// Prochain événement disponible, s'il y en a un.
    pub fn poll(&self) -> Option<Event> {
        self.rx.try_recv().ok()
    }
}

// ---------------------------------------------------------------------------
// Processus externes
// ---------------------------------------------------------------------------

/// Prépare la commande d'un outil en ligne de commande : sorties en UTF-8
/// (yt-dlp et spotdl sont écrits en Python) et pas de fenêtre de console.
fn tool(program: &str) -> Command {
    let mut command = Command::new(program);
    command
        .env("PYTHONIOENCODING", "utf-8")
        .env("PYTHONUTF8", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

fn spawn_error(program: &str, error: std::io::Error) -> String {
    if error.kind() == std::io::ErrorKind::NotFound {
        format!("{program} introuvable : installez-le (pip install {program}) et vérifiez qu'il est dans le PATH.")
    } else {
        format!("impossible de lancer {program} : {error}")
    }
}

/// Lit stderr dans un thread à part (évite qu'un tuyau plein bloque l'outil).
fn collect_stderr(child: &mut std::process::Child) -> thread::JoinHandle<String> {
    let mut stderr = child.stderr.take().expect("stderr redirigé");
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stderr.read_to_end(&mut bytes);
        String::from_utf8_lossy(&bytes).into_owned()
    })
}

/// Message d'erreur le plus parlant de la sortie d'un outil.
fn tool_error(program: &str, output: &str) -> String {
    let lines: Vec<&str> = output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("Deprecated Feature"))
        .collect();
    let line = lines
        .iter()
        .rev()
        .find(|line| line.contains("ERROR") || line.contains("Error"))
        .or(lines.last())
        .copied()
        .unwrap_or("erreur inconnue");
    format!("{program} : {}", line.trim_start_matches("ERROR:").trim())
}

// ---------------------------------------------------------------------------
// YouTube (yt-dlp)
// ---------------------------------------------------------------------------

fn search_youtube(query: &str) -> Result<Vec<SearchResult>, String> {
    let output = tool(YT_DLP)
        .args(["--no-warnings", "--flat-playlist", "--dump-single-json"])
        .arg(format!("ytsearch{SEARCH_LIMIT}:{query}"))
        .output()
        .map_err(|error| spawn_error(YT_DLP, error))?;
    if !output.status.success() {
        return Err(tool_error(YT_DLP, &String::from_utf8_lossy(&output.stderr)));
    }
    let json: Value =
        serde_json::from_slice(&output.stdout).map_err(|error| format!("réponse de yt-dlp illisible : {error}"))?;
    Ok(parse_youtube(&json))
}

fn parse_youtube(json: &Value) -> Vec<SearchResult> {
    let text = |entry: &Value, key: &str| entry.get(key).and_then(Value::as_str).unwrap_or_default().to_string();
    json.get("entries")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let url = entry
                .get("url")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| {
                    let id = entry.get("id")?.as_str()?;
                    Some(format!("https://www.youtube.com/watch?v={id}"))
                })?;
            let mut artist = text(entry, "channel");
            if artist.is_empty() {
                artist = text(entry, "uploader");
            }
            Some(SearchResult {
                source: Source::YouTube,
                title: text(entry, "title"),
                artist,
                album: String::new(),
                duration: entry.get("duration").and_then(Value::as_f64).map(|secs| secs as f32),
                url,
                note: String::new(),
            })
        })
        .collect()
}

fn download_youtube(url: &str, folder: &Path, progress: &dyn Fn(f32)) -> Result<(), String> {
    let mut child = tool(YT_DLP)
        .args([
            "--no-warnings",
            "--no-playlist",
            "--newline",
            "--extract-audio",
            "--audio-format",
            "mp3",
            "--audio-quality",
            "0",
            "--embed-metadata",
            "--output",
            "%(title)s.%(ext)s",
            "--paths",
        ])
        .arg(folder)
        .arg("--")
        .arg(url)
        .spawn()
        .map_err(|error| spawn_error(YT_DLP, error))?;
    let stderr = collect_stderr(&mut child);

    let stdout = child.stdout.take().expect("stdout redirigé");
    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
        if let Some(fraction) = parse_progress(&line) {
            progress(fraction);
        }
    }

    let status = child.wait().map_err(|error| error.to_string())?;
    let stderr = stderr.join().unwrap_or_default();
    if status.success() {
        Ok(())
    } else {
        Err(tool_error(YT_DLP, &stderr))
    }
}

/// `[download]  42.3% of    3.21MiB at …` → 0,423.
fn parse_progress(line: &str) -> Option<f32> {
    let rest = line.strip_prefix("[download]")?;
    let percent = rest.split_whitespace().next()?.strip_suffix('%')?;
    percent.parse::<f32>().ok().map(|p| (p / 100.0).clamp(0.0, 1.0))
}

// ---------------------------------------------------------------------------
// Spotify (API Web + spotdl)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct Token {
    access_token: String,
}

fn spotify_http_error(error: ureq::Error) -> String {
    match error {
        ureq::Error::StatusCode(400 | 401) => {
            "Spotify : identifiants refusés (vérifiez le Client ID et le secret).".into()
        }
        ureq::Error::StatusCode(403) => "Spotify : accès refusé à cette application.".into(),
        ureq::Error::StatusCode(429) => "Spotify : trop de requêtes, réessayez dans un moment.".into(),
        ureq::Error::StatusCode(code) => format!("Spotify : erreur HTTP {code}."),
        other => format!("Spotify injoignable : {other}"),
    }
}

fn search_spotify(query: &str, credentials: &SpotifyCredentials) -> Result<Vec<SearchResult>, String> {
    if !credentials.is_set() {
        return Err("Spotify : renseignez d'abord le Client ID et le secret de votre application.".into());
    }
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(15)))
        .build()
        .into();

    let token: Token = agent
        .post("https://accounts.spotify.com/api/token")
        .send_form([
            ("grant_type", "client_credentials"),
            ("client_id", credentials.client_id.trim()),
            ("client_secret", credentials.client_secret.trim()),
        ])
        .map_err(spotify_http_error)?
        .body_mut()
        .read_json()
        .map_err(|error| format!("Spotify : réponse illisible ({error})"))?;

    let json: Value = agent
        .get("https://api.spotify.com/v1/search")
        .query("q", query)
        .query("type", "track")
        .query("limit", SEARCH_LIMIT.to_string())
        .header("Authorization", format!("Bearer {}", token.access_token))
        .call()
        .map_err(spotify_http_error)?
        .body_mut()
        .read_json()
        .map_err(|error| format!("Spotify : réponse illisible ({error})"))?;
    Ok(parse_spotify(&json))
}

fn parse_spotify(json: &Value) -> Vec<SearchResult> {
    let items = json.pointer("/tracks/items").and_then(Value::as_array);
    items
        .into_iter()
        .flatten()
        .filter_map(|track| {
            let url = track.pointer("/external_urls/spotify")?.as_str()?.to_string();
            let artists: Vec<&str> = track
                .get("artists")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|artist| artist.get("name")?.as_str())
                .collect();
            Some(SearchResult {
                source: Source::Spotify,
                title: track.get("name")?.as_str()?.to_string(),
                artist: artists.join(", "),
                album: track
                    .pointer("/album/name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                duration: track
                    .get("duration_ms")
                    .and_then(Value::as_f64)
                    .map(|ms| (ms / 1000.0) as f32),
                url,
                note: String::new(),
            })
        })
        .collect()
}

fn download_spotify(url: &str, folder: &Path, credentials: &SpotifyCredentials) -> Result<(), String> {
    if !credentials.is_set() {
        return Err("Spotify : identifiants manquants.".into());
    }
    // Le dossier est le répertoire courant : un `{` dans son chemin ne
    // perturbe pas le modèle de nom de fichier de spotdl.
    let output = tool(SPOTDL)
        .current_dir(folder)
        .args([
            "download",
            url,
            "--format",
            "mp3",
            "--output",
            "{artists} - {title}.{output-ext}",
        ])
        .args(["--client-id", credentials.client_id.trim()])
        .args(["--client-secret", credentials.client_secret.trim()])
        .output()
        .map_err(|error| spawn_error(SPOTDL, error))?;
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    // spotdl peut se terminer « avec succès » sans rien avoir téléchargé.
    let downloaded = text.contains("Downloaded") || text.contains("Skipping");
    if output.status.success() && downloaded {
        Ok(())
    } else {
        Err(tool_error(SPOTDL, &text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_lines() {
        assert_eq!(
            parse_progress("[download]  42.5% of    3.21MiB at  1.00MiB/s ETA 00:02"),
            Some(0.425)
        );
        assert_eq!(parse_progress("[download] 100% of    3.21MiB"), Some(1.0));
        assert_eq!(parse_progress("[download] Destination: x.webm"), None);
        assert_eq!(parse_progress("[ExtractAudio] Destination: x.mp3"), None);
    }

    #[test]
    fn youtube_results() {
        let json: Value = serde_json::from_str(
            r#"{"entries": [
                {"id": "abc", "url": "https://www.youtube.com/watch?v=abc", "title": "Titre", "channel": "Chaîne", "duration": 322.0},
                {"id": "def", "title": "Sans url", "uploader": "Quelqu'un"}
            ]}"#,
        )
        .unwrap();
        let results = parse_youtube(&json);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].artist, "Chaîne");
        assert_eq!(results[0].duration, Some(322.0));
        assert_eq!(results[1].url, "https://www.youtube.com/watch?v=def");
        assert_eq!(results[1].artist, "Quelqu'un");
    }

    #[test]
    fn spotify_results() {
        let json: Value = serde_json::from_str(
            r#"{"tracks": {"items": [{
                "name": "One More Time",
                "artists": [{"name": "Daft Punk"}, {"name": "Romanthony"}],
                "album": {"name": "Discovery"},
                "duration_ms": 320357,
                "external_urls": {"spotify": "https://open.spotify.com/track/0DiWol3AO6WpXZgp0goxAV"}
            }]}}"#,
        )
        .unwrap();
        let results = parse_spotify(&json);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].artist, "Daft Punk, Romanthony");
        assert_eq!(results[0].album, "Discovery");
        assert!((results[0].duration.unwrap() - 320.357).abs() < 1e-3);
    }

    #[test]
    fn error_message_prefers_error_lines() {
        let output = "Deprecated Feature: blah\nERROR: [youtube] abc: Video unavailable\n";
        assert_eq!(
            tool_error("yt-dlp", output),
            "yt-dlp : [youtube] abc: Video unavailable"
        );
    }
}
