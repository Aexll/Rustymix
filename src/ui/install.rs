//! Fenêtre d'installation de sons : recherche sur YouTube ou Spotify puis
//! téléchargement dans un dossier choisi par l'utilisateur.
//!
//! Mode « Similaires » : à partir d'un morceau (sélection de la
//! bibliothèque, résultat de recherche, ou saisi), propose des morceaux
//! proches trouvés par Deezer (voir `recommend`), installables par YouTube.

use std::path::PathBuf;

use egui::{Align, Context, Layout, RichText, ScrollArea, TextEdit, Ui};

use super::icons::{self, Icon};
use super::theme;
use crate::config::Config;
use crate::downloader::{Downloader, Event, SearchResult, Source, SpotifyCredentials};
use crate::library::{Library, TrackInfo};
use crate::recommend::{Seed, normalize};

/// Recommandations : morceau de départ, demande en cours, propositions.
#[derive(Default)]
struct Similar {
    /// Saisie « Artiste - Titre ».
    seed: String,
    request: Option<u64>,
    /// Morceau reconnu par Deezer.
    found: Option<String>,
    results: Vec<SearchResult>,
    error: Option<String>,
    /// Morceaux déjà dans la bibliothèque (même ordre que `results`), et
    /// taille de la bibliothèque lors du calcul.
    installed: Vec<bool>,
    checked: Option<usize>,
}

/// Téléchargement en cours ou terminé, affiché sous les résultats.
struct Download {
    id: u64,
    track: SearchResult,
    fraction: f32,
    done: Option<Result<(), String>>,
}

pub struct InstallWindow {
    pub folder: PathBuf,
    source: Source,
    query: String,
    downloader: Downloader,
    searching: bool,
    search_id: Option<u64>,
    results: Vec<SearchResult>,
    search_error: Option<String>,
    downloads: Vec<Download>,
    show_spotify_settings: bool,
    /// Mode « Similaires » (sinon recherche YouTube / Spotify).
    similar_mode: bool,
    similar: Similar,
}

/// Ce qu'a produit cette image : rien, ou une installation terminée (pour
/// déclencher un rescan de la bibliothèque) et/ou une configuration à enregistrer.
#[derive(Default)]
pub struct InstallOutcome {
    pub track_installed: bool,
    pub config_changed: bool,
}

impl InstallWindow {
    pub fn new(folder: PathBuf) -> Self {
        Self {
            folder,
            source: Source::YouTube,
            query: String::new(),
            downloader: Downloader::default(),
            searching: false,
            search_id: None,
            results: Vec::new(),
            search_error: None,
            downloads: Vec::new(),
            show_spotify_settings: false,
            similar_mode: false,
            similar: Similar::default(),
        }
    }

    pub fn is_animating(&self) -> bool {
        self.searching || self.similar.request.is_some() || self.downloads.iter().any(|d| d.done.is_none())
    }

    /// Passe en mode « Similaires » et cherche des morceaux proches de
    /// `title` / `artist`.
    pub fn find_similar(&mut self, title: &str, artist: &str, ctx: &Context) {
        self.similar_mode = true;
        self.similar.seed = if artist.trim().is_empty() {
            title.trim().to_string()
        } else {
            format!("{} - {}", artist.trim(), title.trim())
        };
        self.launch_similar(ctx);
    }

    fn launch_similar(&mut self, ctx: &Context) {
        let text = self.similar.seed.trim();
        if text.is_empty() {
            return;
        }
        let seed = Seed {
            title: text.to_string(),
            artist: String::new(),
        };
        let similar = &mut self.similar;
        similar.results.clear();
        similar.installed.clear();
        similar.checked = None;
        similar.found = None;
        similar.error = None;
        let ctx = ctx.clone();
        similar.request = Some(self.downloader.similar(seed, move || ctx.request_repaint()));
    }

    /// Marque les propositions déjà présentes dans la bibliothèque (titre et
    /// artiste retrouvés dans le titre, l'artiste ou le nom de fichier).
    fn check_installed(&mut self, library: &Library) {
        let similar = &mut self.similar;
        if similar.checked == Some(library.len()) && similar.installed.len() == similar.results.len() {
            return;
        }
        let keys: Vec<String> = (0..library.len())
            .filter_map(|index| library.track(index))
            .map(|track| normalize(&format!("{} {} {}", track.title, track.artist, track.file_name)))
            .collect();
        similar.installed = similar
            .results
            .iter()
            .map(|result| {
                let (title, artist) = (normalize(&result.title), normalize(&result.artist));
                !title.is_empty() && keys.iter().any(|key| key.contains(&title) && key.contains(&artist))
            })
            .collect();
        similar.checked = Some(library.len());
    }

    fn launch_search(&mut self, config: &Config, ctx: &Context) {
        let query = self.query.trim().to_string();
        if query.is_empty() {
            return;
        }
        self.results.clear();
        self.search_error = None;
        self.searching = true;
        let credentials = SpotifyCredentials {
            client_id: config.spotify_client_id.clone(),
            client_secret: config.spotify_client_secret.clone(),
        };
        let ctx = ctx.clone();
        self.search_id = Some(
            self.downloader
                .search(self.source, query, credentials, move || ctx.request_repaint()),
        );
    }

    fn poll(&mut self, outcome: &mut InstallOutcome) {
        while let Some(event) = self.downloader.poll() {
            match event {
                Event::Search { id, result } => {
                    if self.search_id != Some(id) {
                        continue; // Une recherche plus récente l'a remplacée.
                    }
                    self.searching = false;
                    match result {
                        Ok(results) => self.results = results,
                        Err(error) => self.search_error = Some(error),
                    }
                }
                Event::Progress { id, fraction } => {
                    if let Some(download) = self.downloads.iter_mut().find(|d| d.id == id) {
                        download.fraction = fraction;
                    }
                }
                Event::Finished { id, result } => {
                    if let Some(download) = self.downloads.iter_mut().find(|d| d.id == id) {
                        download.fraction = 1.0;
                        outcome.track_installed |= result.is_ok();
                        download.done = Some(result);
                    }
                }
                Event::Similar { id, result } => {
                    if self.similar.request != Some(id) {
                        continue;
                    }
                    self.similar.request = None;
                    match result {
                        Ok((found, results)) => {
                            self.similar.found = Some(found);
                            self.similar.results = results;
                            self.similar.checked = None;
                        }
                        Err(error) => self.similar.error = Some(error),
                    }
                }
            }
        }
    }

    /// Suit les recherches et téléchargements ; à appeler à chaque image,
    /// que le panneau soit affiché ou non.
    pub fn update(&mut self) -> InstallOutcome {
        let mut outcome = InstallOutcome::default();
        self.poll(&mut outcome);
        outcome
    }

    /// Contenu du panneau (ancré sous les platines ou dans une fenêtre).
    /// `selected` : morceau sélectionné dans la bibliothèque (départ des
    /// recommandations).
    pub fn ui(
        &mut self,
        ui: &mut Ui,
        config: &mut Config,
        library: &Library,
        selected: Option<&TrackInfo>,
    ) -> InstallOutcome {
        let ctx = ui.ctx().clone();
        let mut outcome = InstallOutcome::default();
        self.folder_row(ui, &mut outcome, &ctx);
        ui.add_space(6.0);
        if self.similar_mode {
            self.similar_row(ui, selected, &ctx);
            self.check_installed(library);
        } else {
            self.search_row(ui, config, &ctx);
            if self.source == Source::Spotify {
                outcome.config_changed |= self.spotify_settings(ui, config);
            }
        }
        ui.separator();
        let error = if self.similar_mode {
            &self.similar.error
        } else {
            &self.search_error
        };
        if let Some(error) = error {
            ui.colored_label(theme::RED, error);
        }
        // Les téléchargements gardent leur place en bas.
        let downloads_height = if self.downloads.is_empty() {
            0.0
        } else {
            40.0 + 22.0 * self.downloads.len().min(5) as f32
        };
        let results_height = (ui.available_height() - downloads_height).max(120.0);
        ScrollArea::vertical()
            .id_salt("install_results")
            .max_height(results_height)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                if self.similar_mode {
                    self.similar_list(ui, config, &ctx);
                } else {
                    self.results_list(ui, config, &ctx);
                }
            });
        if !self.downloads.is_empty() {
            ui.separator();
            ui.label(RichText::new("Téléchargements").strong());
            ScrollArea::vertical().id_salt("install_downloads").show(ui, |ui| {
                self.downloads_list(ui);
            });
        }
        outcome
    }

    /// Nombre de téléchargements en cours (badge de la barre latérale).
    pub fn active_downloads(&self) -> usize {
        self.downloads.iter().filter(|download| download.done.is_none()).count()
    }

    fn folder_row(&mut self, ui: &mut Ui, outcome: &mut InstallOutcome, _ctx: &Context) {
        ui.horizontal(|ui| {
            ui.label("Dossier :");
            ui.label(RichText::new(self.folder.display().to_string()).color(theme::TEXT_DIM));
            if ui.small_button("Changer…").clicked()
                && let Some(folder) = rfd::FileDialog::new()
                    .set_title("Dossier d'installation")
                    .set_directory(&self.folder)
                    .pick_folder()
            {
                self.folder = folder;
                outcome.config_changed = true;
            }
        });
    }

    /// YouTube, Spotify, ou Similaires.
    fn mode_selector(&mut self, ui: &mut Ui) {
        for source in [Source::YouTube, Source::Spotify] {
            if ui
                .selectable_label(!self.similar_mode && self.source == source, source.name())
                .clicked()
            {
                self.source = source;
                self.similar_mode = false;
            }
        }
        if ui
            .selectable_label(self.similar_mode, "Similaires")
            .on_hover_text("Morceaux proches d'un morceau choisi (recommandations Deezer, installés par YouTube)")
            .clicked()
        {
            self.similar_mode = true;
        }
    }

    /// Mode « Similaires » : morceau de départ et recherche.
    fn similar_row(&mut self, ui: &mut Ui, selected: Option<&TrackInfo>, ctx: &Context) {
        ui.horizontal(|ui| {
            self.mode_selector(ui);
            let hint = match selected {
                Some(track) => format!("Prendre la sélection de la bibliothèque : « {} »", track.title),
                None => "Sélectionnez un morceau dans la bibliothèque".to_string(),
            };
            if icons::icon_button(ui, Icon::Load, theme::TEXT_DIM, false, selected.is_some(), &hint).clicked()
                && let Some(track) = selected
            {
                self.find_similar(&track.title, &track.artist, ctx);
            }
            let response = ui.add(
                TextEdit::singleline(&mut self.similar.seed)
                    .hint_text("Artiste - Titre")
                    .desired_width(260.0),
            );
            let busy = self.similar.request.is_some();
            let clicked = ui.add_enabled(!busy, egui::Button::new("Proposer")).clicked();
            let enter = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if clicked || enter {
                self.launch_similar(ctx);
            }
            if busy {
                ui.spinner();
            }
        });
    }

    /// Propositions : titre, artiste, raison ; déjà installé ou bouton Installer.
    fn similar_list(&mut self, ui: &mut Ui, config: &Config, ctx: &Context) {
        if let Some(found) = &self.similar.found {
            ui.label(RichText::new(format!("Proches de {found}")).color(theme::TEXT_DIM));
            ui.add_space(2.0);
        }
        if self.similar.results.is_empty() && self.similar.request.is_none() && self.similar.error.is_none() {
            ui.label(
                RichText::new("Choisissez un morceau (sélection de la bibliothèque, ou « Artiste - Titre »).")
                    .color(theme::TEXT_DIM),
            );
            return;
        }
        let mut chain = None;
        for (index, result) in self.similar.results.clone().into_iter().enumerate() {
            let installed = self.similar.installed.get(index).copied().unwrap_or(false);
            ui.horizontal(|ui| {
                ui.set_min_height(28.0);
                ui.vertical(|ui| {
                    let title = RichText::new(&result.title).strong();
                    ui.label(if installed { title.color(theme::TEXT_DIM) } else { title });
                    let mut subtitle = result.artist.clone();
                    if !result.album.is_empty() {
                        subtitle = format!("{subtitle} — {}", result.album);
                    }
                    if let Some(duration) = result.duration {
                        subtitle = format!("{subtitle} · {}", format_duration(duration));
                    }
                    ui.label(RichText::new(subtitle).color(theme::TEXT_DIM).small());
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let hint = format!("Morceaux proches de « {} »", result.title);
                    if icons::icon_button(ui, Icon::Match, theme::TEXT_DIM, false, true, &hint).clicked() {
                        chain = Some((result.title.clone(), result.artist.clone()));
                    }
                    if installed {
                        ui.label(RichText::new("installé").color(theme::GREEN).small());
                    } else {
                        let already = self
                            .downloads
                            .iter()
                            .any(|d| d.track.url == result.url && d.done.is_none());
                        if ui
                            .add_enabled(!already, egui::Button::new("Installer"))
                            .on_hover_text("Cherché et installé par YouTube (premier résultat)")
                            .clicked()
                        {
                            self.start_download(result.clone(), config, ctx);
                        }
                    }
                    ui.label(RichText::new(&result.note).color(theme::TEXT_DIM).small());
                });
            });
            ui.separator();
        }
        if let Some((title, artist)) = chain {
            self.find_similar(&title, &artist, ctx);
        }
    }

    fn search_row(&mut self, ui: &mut Ui, config: &Config, ctx: &Context) {
        ui.horizontal(|ui| {
            self.mode_selector(ui);
            if self.source == Source::Spotify {
                let configured = !config.spotify_client_id.trim().is_empty();
                let label = if configured {
                    "Identifiants…"
                } else {
                    "Identifiants (requis)…"
                };
                let button = if configured {
                    ui.button(label)
                } else {
                    ui.button(RichText::new(label).color(theme::YELLOW))
                };
                if button.clicked() {
                    self.show_spotify_settings ^= true;
                }
            }

            let response = ui.add(
                TextEdit::singleline(&mut self.query)
                    .hint_text("Titre, artiste…")
                    .desired_width(260.0),
            );
            let search_clicked = ui
                .add_enabled(!self.searching, egui::Button::new("Rechercher"))
                .clicked();
            let enter_pressed = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if search_clicked || enter_pressed {
                self.launch_search(config, ctx);
            }
            if self.searching {
                ui.spinner();
            }
        });
    }

    /// Réglages Spotify (Client ID / secret) ; retourne `true` si modifiés.
    fn spotify_settings(&mut self, ui: &mut Ui, config: &mut Config) -> bool {
        if !self.show_spotify_settings {
            return false;
        }
        let mut changed = false;
        egui::Frame::new()
            .fill(theme::PANEL_LIGHT)
            .inner_margin(8.0)
            .corner_radius(4.0)
            .show(ui, |ui| {
                ui.label(
                    RichText::new(
                        "Identifiants d'une application Spotify (gratuite, developer.spotify.com) : \
                         nécessaires pour la recherche. Le téléchargement passe par spotdl.",
                    )
                    .color(theme::TEXT_DIM)
                    .small(),
                );
                egui::Grid::new("spotify_credentials").num_columns(2).show(ui, |ui| {
                    ui.label("Client ID");
                    changed |= ui.text_edit_singleline(&mut config.spotify_client_id).changed();
                    ui.end_row();
                    ui.label("Secret");
                    changed |= ui
                        .add(TextEdit::singleline(&mut config.spotify_client_secret).password(true))
                        .changed();
                    ui.end_row();
                });
            });
        changed
    }

    fn results_list(&mut self, ui: &mut Ui, config: &Config, ctx: &Context) {
        if self.results.is_empty() && !self.searching && self.search_error.is_none() {
            ui.label(RichText::new("Aucun résultat pour l'instant.").color(theme::TEXT_DIM));
            return;
        }
        let mut chain = None;
        for result in self.results.clone() {
            ui.horizontal(|ui| {
                ui.set_min_height(28.0);
                ui.vertical(|ui| {
                    ui.label(RichText::new(&result.title).strong());
                    let mut subtitle = result.artist.clone();
                    if !result.album.is_empty() {
                        subtitle = format!("{subtitle} — {}", result.album);
                    }
                    if let Some(duration) = result.duration {
                        subtitle = format!("{subtitle} · {}", format_duration(duration));
                    }
                    ui.label(RichText::new(subtitle).color(theme::TEXT_DIM).small());
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let hint = format!("Morceaux proches de « {} »", result.title);
                    if icons::icon_button(ui, Icon::Match, theme::TEXT_DIM, false, true, &hint).clicked() {
                        chain = Some((result.title.clone(), result.artist.clone()));
                    }
                    let already = self
                        .downloads
                        .iter()
                        .any(|d| d.track.url == result.url && d.done.is_none());
                    if ui.add_enabled(!already, egui::Button::new("Installer")).clicked() {
                        self.start_download(result.clone(), config, ctx);
                    }
                });
            });
            ui.separator();
        }
        if let Some((title, artist)) = chain {
            self.find_similar(&title, &artist, ctx);
        }
    }

    fn start_download(&mut self, track: SearchResult, config: &Config, ctx: &Context) {
        let credentials = SpotifyCredentials {
            client_id: config.spotify_client_id.clone(),
            client_secret: config.spotify_client_secret.clone(),
        };
        let ctx = ctx.clone();
        let id = self
            .downloader
            .download(track.clone(), self.folder.clone(), credentials, move || {
                ctx.request_repaint()
            });
        self.downloads.insert(
            0,
            Download {
                id,
                track,
                fraction: 0.0,
                done: None,
            },
        );
    }

    fn downloads_list(&mut self, ui: &mut Ui) {
        let mut remove = None;
        for (index, download) in self.downloads.iter().enumerate() {
            ui.horizontal(|ui| {
                ui.set_min_width(260.0);
                ui.label(format!(
                    "[{}] {} — {}",
                    download.track.source.name(),
                    download.track.title,
                    download.track.artist
                ));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.small_button("×").clicked() {
                        remove = Some(index);
                    }
                    match &download.done {
                        None => {
                            ui.add(egui::ProgressBar::new(download.fraction).desired_width(100.0));
                        }
                        Some(Ok(())) => {
                            ui.colored_label(theme::GREEN, "Installé");
                        }
                        Some(Err(error)) => {
                            ui.colored_label(theme::RED, error);
                        }
                    }
                });
            });
        }
        if let Some(index) = remove
            && self.downloads[index].done.is_some()
        {
            self.downloads.remove(index);
        }
    }
}

fn format_duration(seconds: f32) -> String {
    let total = seconds.round().max(0.0) as u32;
    format!("{}:{:02}", total / 60, total % 60)
}
