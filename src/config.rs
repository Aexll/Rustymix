//! Configuration persistante (dossiers de la bibliothèque), stockée en TOML
//! dans le dossier de configuration de l'OS.
//!
//! * Windows : `%APPDATA%\Rustymix\config\config.toml`
//! * Linux : `~/.config/rustymix/config.toml`

use std::fs;
use std::path::PathBuf;

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct Config {
    /// Dossiers racine de la bibliothèque, scannés récursivement.
    #[serde(default)]
    pub folders: Vec<PathBuf>,
    /// Dernier dossier choisi pour installer des sons.
    #[serde(default)]
    pub install_folder: Option<PathBuf>,
    /// Identifiants d'une application Spotify (recherche et spotdl).
    #[serde(default)]
    pub spotify_client_id: String,
    #[serde(default)]
    pub spotify_client_secret: String,
}

impl Config {
    /// Chemin du fichier de configuration. La variable d'environnement
    /// `RUSTYMIX_CONFIG` permet d'en imposer un autre (installation portable, tests).
    pub fn path() -> Option<PathBuf> {
        if let Some(path) = std::env::var_os("RUSTYMIX_CONFIG") {
            return Some(PathBuf::from(path));
        }
        ProjectDirs::from("", "", "Rustymix").map(|dirs| dirs.config_dir().join("config.toml"))
    }

    /// Fichier de cache des analyses (tempo, tonalité), à côté de la configuration.
    pub fn analysis_cache_path() -> Option<PathBuf> {
        Self::path().map(|path| path.with_file_name("analysis.toml"))
    }

    /// Fichier des marqueurs de chaque morceau, à côté de la configuration.
    pub fn markers_path() -> Option<PathBuf> {
        Self::path().map(|path| path.with_file_name("markers.toml"))
    }

    /// Transitions planifiées, à côté de la configuration.
    pub fn transitions_path() -> Option<PathBuf> {
        Self::path().map(|path| path.with_file_name("transitions.json"))
    }

    /// Cache des stems (pistes séparées), à côté de la configuration.
    pub fn stems_dir() -> Option<PathBuf> {
        Self::path().map(|path| path.with_file_name("stems"))
    }

    /// Charge la configuration ; retourne la configuration par défaut si le
    /// fichier est absent ou illisible.
    pub fn load() -> Self {
        Self::path()
            .and_then(|path| fs::read_to_string(path).ok())
            .and_then(|text| toml::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> Result<(), String> {
        let path = Self::path().ok_or("dossier de configuration introuvable")?;
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let text = toml::to_string_pretty(self).map_err(|e| e.to_string())?;
        fs::write(path, text).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toml_round_trip() {
        let config = Config {
            folders: vec![PathBuf::from("C:\\Musique"), PathBuf::from("/home/dj/sets")],
            install_folder: Some(PathBuf::from("C:\\Musique\\Nouveaux")),
            spotify_client_id: "id".into(),
            spotify_client_secret: "secret".into(),
        };
        let text = toml::to_string_pretty(&config).unwrap();
        assert_eq!(toml::from_str::<Config>(&text).unwrap(), config);
        // Un fichier vide donne la configuration par défaut.
        assert_eq!(toml::from_str::<Config>("").unwrap(), Config::default());
    }
}
