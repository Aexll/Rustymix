//! Application : assemble le moteur audio, les platines, le mixer et la bibliothèque.

use std::path::PathBuf;
use std::sync::atomic::Ordering::Relaxed;
use std::time::{Duration, Instant};

use egui::{
    Align, Align2, Context, FontId, Frame, Id, LayerId, Layout, Order, Rect, RichText, Sense, Stroke, StrokeKind, Ui,
    vec2,
};

use super::activity_bar::{BAR_WIDTH, BarItem, Tab, activity_bar};
use super::deck::{DeckEvents, DeckUi, LoadedTrack};
use super::icons::{Icon, icon_button};
use super::install::InstallWindow;
use super::library_view::{DraggedTrack, LibraryAction, LibraryView, SuggestSource};
use super::mixer::MixerUi;
use super::shortcuts::{Action, HELP, Shortcuts};
use super::theme;
use super::transitions::TransitionsWindow;
use super::widgets::take_bottom;
use crate::audio::engine::{BeatGrid, Command, Engine, EngineHandle, NUM_DECKS};
use crate::audio::output::OutputDevice;
use crate::config::Config;
use crate::library::{Library, TrackInfo};
use crate::loader::Loader;
use crate::markers::MarkerStore;
use crate::stems::{self, Separator, StemEvent};
use crate::transitions::TransitionStore;

const DECKS_PANEL_HEIGHT: f32 = 464.0;
const CROSSFADER_HEIGHT: f32 = 50.0;
const MIXER_WIDTH: f32 = 120.0;
const GAP: f32 = 8.0;
/// Durée d'affichage d'un message d'état.
const STATUS_DURATION: Duration = Duration::from_secs(5);
/// Vitesse de la recherche avant/arrière au clavier (× temps réel).
const KEY_SEEK_SPEED: f64 = 10.0;
/// Vitesse du crossfader au clavier, en course complète par seconde.
const KEY_CROSSFADE_SPEED: f32 = 1.0;

struct Status {
    text: String,
    is_error: bool,
    since: Instant,
}

pub struct RustymixApp {
    engine: EngineHandle,
    /// Flux de sortie ; le son s'arrête quand il est détruit.
    _stream: Option<cpal::Stream>,
    audio_info: String,
    config: Config,
    library: Library,
    library_view: LibraryView,
    decks: [DeckUi; NUM_DECKS],
    mixer: MixerUi,
    loader: Loader,
    shortcuts: Shortcuts,
    status: Option<Status>,
    show_help: bool,
    install: Option<InstallWindow>,
    /// Gestionnaire de transitions (panneaux et exécution).
    transitions: TransitionsWindow,
    /// Panneau affiché sous les platines.
    tab: Tab,
    /// Panneaux détachés dans des fenêtres flottantes (indice : `Tab::index`).
    floating: [bool; 3],
    markers: MarkerStore,
    separator: Separator,
    /// Recherche au clavier en cours sur chaque platine (pour caler la
    /// position sur un temps quand elle s'arrête, avec MAGNET).
    key_seeking: [bool; NUM_DECKS],
}

impl RustymixApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        theme::apply(&cc.egui_ctx);

        let (engine, stream, audio_info, audio_error) = start_audio();
        let config = Config::load();

        let mut app = Self {
            engine,
            _stream: stream,
            audio_info,
            config,
            library: Library::new(Config::analysis_cache_path()),
            library_view: LibraryView::default(),
            decks: [DeckUi::new(0), DeckUi::new(1)],
            mixer: MixerUi::default(),
            loader: Loader::default(),
            shortcuts: Shortcuts::default(),
            status: None,
            show_help: false,
            install: None,
            transitions: TransitionsWindow::new(TransitionStore::load(Config::transitions_path())),
            tab: Tab::Library,
            floating: [false; 3],
            markers: MarkerStore::load(Config::markers_path()),
            separator: Separator::default(),
            key_seeking: [false; NUM_DECKS],
        };
        if let Some(error) = audio_error {
            app.set_status(format!("Audio indisponible : {error}"), true);
        }
        app.rescan(&cc.egui_ctx);

        // Fichiers passés en ligne de commande : le premier sur A, le second sur B.
        let files = std::env::args_os()
            .skip(1)
            .map(PathBuf::from)
            .filter(|path| path.is_file());
        for (deck, path) in files.take(NUM_DECKS).enumerate() {
            app.request_load(deck, TrackInfo::read(&path), &cc.egui_ctx);
        }
        app
    }

    fn set_status(&mut self, text: String, is_error: bool) {
        self.status = Some(Status {
            text,
            is_error,
            since: Instant::now(),
        });
    }

    // -----------------------------------------------------------------------
    // Bibliothèque et chargements
    // -----------------------------------------------------------------------

    fn rescan(&mut self, ctx: &Context) {
        let ctx = ctx.clone();
        self.library_view.clear_selection();
        self.library.rescan(&self.config.folders, move || ctx.request_repaint());
    }

    fn save_config(&mut self) {
        if let Err(error) = self.config.save() {
            self.set_status(format!("Configuration non enregistrée : {error}"), true);
        }
    }

    fn handle_library_action(&mut self, action: LibraryAction, ctx: &Context) {
        match action {
            LibraryAction::AddFolders => {
                let Some(folders) = rfd::FileDialog::new().set_title("Dossiers de musique").pick_folders() else {
                    return;
                };
                for folder in folders {
                    if !self.config.folders.contains(&folder) {
                        self.config.folders.push(folder);
                    }
                }
            }
            LibraryAction::RemoveFolder(index) => {
                if index < self.config.folders.len() {
                    self.config.folders.remove(index);
                }
            }
            LibraryAction::Rescan => {}
            LibraryAction::Install => {
                self.start_install();
                self.select_tab(Tab::Install);
                return;
            }
            LibraryAction::Similar(track) => {
                self.start_install();
                self.select_tab(Tab::Install);
                if let Some(install) = &mut self.install {
                    install.find_similar(&track.title, &track.artist, ctx);
                }
                return;
            }
        }
        self.save_config();
        self.rescan(ctx);
    }

    /// Prépare le panneau d'installation : gardé tel qu'il a été laissé s'il
    /// existe déjà, sinon dans le dossier utilisé la dernière fois (demandé
    /// seulement s'il n'y en a pas encore, ou s'il a disparu).
    fn start_install(&mut self) {
        if self.install.is_some() {
            return;
        }
        let remembered = self.config.install_folder.clone().filter(|folder| folder.is_dir());
        let folder = match remembered {
            Some(folder) => folder,
            None => {
                let mut dialog = rfd::FileDialog::new().set_title("Dossier d'installation des sons");
                if let Some(folder) = &self.config.install_folder {
                    dialog = dialog.set_directory(folder);
                }
                let Some(folder) = dialog.pick_folder() else {
                    return;
                };
                self.config.install_folder = Some(folder.clone());
                self.save_config();
                folder
            }
        };
        self.install = Some(InstallWindow::new(folder));
    }

    /// Suit les téléchargements, même panneau caché ; rescanne la
    /// bibliothèque quand un son vient d'être installé.
    fn update_install(&mut self, ctx: &Context) {
        let Some(install) = &mut self.install else { return };
        if install.update().track_installed {
            self.set_status("Son installé.".into(), false);
            self.rescan(ctx);
        }
    }

    // -----------------------------------------------------------------------
    // Panneaux : barre d'activité, panneau ancré, fenêtres détachées
    // -----------------------------------------------------------------------

    /// Affiche un panneau sous les platines ; un panneau détaché y revient.
    fn select_tab(&mut self, tab: Tab) {
        self.floating[tab.index()] = false;
        self.tab = tab;
        if tab == Tab::Library {
            return;
        }
        if tab == Tab::Install {
            self.start_install();
        }
    }

    /// Détache un panneau dans une fenêtre ; la bibliothèque prend sa place.
    fn detach_tab(&mut self, tab: Tab) {
        if !tab.detachable() {
            return;
        }
        self.floating[tab.index()] = true;
        if self.tab == tab {
            self.tab = Tab::Library;
        }
    }

    fn activity_items(&self) -> Vec<BarItem> {
        Tab::ALL
            .into_iter()
            .map(|tab| {
                let badge = match tab {
                    Tab::Nodes if self.transitions.is_recording() => Some((theme::RED, None)),
                    Tab::Nodes if self.transitions.is_running() => Some((theme::GREEN, None)),
                    Tab::Install => self
                        .install
                        .as_ref()
                        .map(|install| install.active_downloads())
                        .filter(|count| *count > 0)
                        .map(|count| (theme::DECK_COLORS[0], Some(count))),
                    _ => None,
                };
                BarItem {
                    tab,
                    active: self.tab == tab && !self.floating[tab.index()],
                    floating: self.floating[tab.index()],
                    badge,
                }
            })
            .collect()
    }

    /// En-tête discret d'un panneau : son titre (ancré) et le bouton pour le
    /// détacher ou le rancrer.
    fn panel_header(&mut self, ui: &mut Ui, tab: Tab, docked: bool) {
        ui.horizontal(|ui| {
            if docked {
                ui.label(
                    RichText::new(tab.title().to_uppercase())
                        .size(11.0)
                        .strong()
                        .color(theme::TEXT_DIM),
                );
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if docked {
                    if icon_button(
                        ui,
                        Icon::Detach,
                        theme::TEXT_DIM,
                        false,
                        true,
                        "Détacher dans une fenêtre",
                    )
                    .clicked()
                    {
                        self.detach_tab(tab);
                    }
                } else if icon_button(ui, Icon::Dock, theme::TEXT_DIM, false, true, "Ancrer sous les platines")
                    .clicked()
                {
                    self.select_tab(tab);
                }
            });
        });
        ui.add_space(2.0);
    }

    /// Contenu d'un panneau (hors bibliothèque).
    fn panel_contents(&mut self, ui: &mut Ui, tab: Tab, ctx: &Context) {
        match tab {
            Tab::Library => {}
            Tab::Nodes => {
                let outcome = self.transitions.ui(ui, &self.decks, &self.engine);
                if let Some((text, is_error)) = outcome.status {
                    self.set_status(text, is_error);
                }
                if let Some((deck, path)) = outcome.load {
                    if path.is_file() {
                        self.request_load(deck, TrackInfo::read(&path), ctx);
                    } else {
                        self.set_status(format!("Fichier introuvable : {}", path.display()), true);
                    }
                }
            }
            Tab::Install => match &mut self.install {
                Some(install) => {
                    let selected = self.library_view.selected_track(&self.library);
                    if install.ui(ui, &mut self.config, &self.library, selected).config_changed {
                        self.config.install_folder = Some(install.folder.clone());
                        self.save_config();
                    }
                }
                None => {
                    ui.add_space(24.0);
                    ui.vertical_centered(|ui| {
                        ui.label(
                            RichText::new("Choisissez le dossier où installer les sons.")
                                .size(15.0)
                                .color(theme::TEXT_DIM),
                        );
                        ui.add_space(8.0);
                        if ui.button("Choisir le dossier…").clicked() {
                            self.start_install();
                        }
                    });
                }
            },
        }
    }

    /// Panneaux détachés, chacun dans sa fenêtre ; la fermer le rancre.
    /// `drawn` : panneau déjà dessiné sous les platines à cette image (il
    /// vient d'être détaché) — sa fenêtre n'apparaît qu'à l'image suivante,
    /// un même panneau ne pouvant pas être dessiné deux fois.
    fn floating_windows(&mut self, ctx: &Context, drawn: Option<Tab>) {
        for tab in Tab::ALL.into_iter().filter(|tab| tab.detachable()) {
            if !self.floating[tab.index()] {
                continue;
            }
            if drawn == Some(tab) {
                ctx.request_repaint();
                continue;
            }
            let mut open = true;
            let size = match tab {
                Tab::Nodes => vec2(1200.0, 700.0),
                _ => vec2(580.0, 520.0),
            };
            egui::Window::new(tab.title())
                .id(egui::Id::unique(("panel_window", tab.index())))
                .open(&mut open)
                .collapsible(false)
                .resizable(true)
                .default_size(size)
                .min_size(vec2(460.0, 320.0))
                .show(ctx, |ui| {
                    self.panel_header(ui, tab, false);
                    self.panel_contents(ui, tab, ctx);
                });
            if !open {
                // Fermée : le panneau retourne dans la barre (sans s'afficher).
                self.floating[tab.index()] = false;
            }
        }
    }

    /// Raccourcis des panneaux : Ctrl + 1 à 3.
    fn tab_shortcuts(&mut self, ctx: &Context) {
        if ctx.text_edit_focused() {
            return;
        }
        let pressed = ctx.input(|input| {
            [egui::Key::Num1, egui::Key::Num2, egui::Key::Num3]
                .into_iter()
                .position(|key| input.modifiers.command && input.key_pressed(key))
        });
        if let Some(index) = pressed {
            self.select_tab(Tab::ALL[index]);
        }
    }

    /// Lance le chargement d'un morceau sur une platine (refusé si elle joue).
    fn request_load(&mut self, deck: usize, info: TrackInfo, ctx: &Context) {
        if self.engine.shared.decks[deck].playing.load(Relaxed) {
            self.set_status(
                format!(
                    "Platine {} en lecture : arrêtez-la avant d'y charger un morceau.",
                    theme::DECK_NAMES[deck]
                ),
                true,
            );
            return;
        }
        let ctx = ctx.clone();
        let id = self
            .loader
            .request(deck, info.clone(), self.engine.shared.sample_rate, move || {
                ctx.request_repaint()
            });
        self.decks[deck].loading = Some((id, info));
    }

    /// Récupère les morceaux décodés en arrière-plan et les envoie au moteur.
    fn poll_loader(&mut self, ctx: &Context) {
        while let Some(loaded) = self.loader.poll() {
            let deck = &mut self.decks[loaded.deck];
            // Une demande plus récente a remplacé celle-ci : on l'ignore.
            if deck.loading.as_ref().map(|(id, _)| *id) != Some(loaded.id) {
                continue;
            }
            deck.loading = None;

            match loaded.result {
                Ok((track, waveform, analysis)) => {
                    // Grille de temps pour le moteur (départs calés du magnet).
                    let sample_rate = track.sample_rate as f64;
                    let grid = analysis.bpm.map(|bpm| BeatGrid {
                        anchor: analysis.beat_offset as f64 * sample_rate,
                        period: 60.0 / bpm as f64 * sample_rate,
                    });
                    deck.incoming = Some(LoadedTrack {
                        id: loaded.id,
                        markers: self.markers.get(&loaded.info.path),
                        bounds: waveform.music_bounds(),
                        info: loaded.info,
                        waveform,
                        analysis,
                    });
                    // Stems déjà en cache : rechargés automatiquement.
                    let frames = track.frames();
                    let cached = deck
                        .incoming
                        .as_ref()
                        .filter(|incoming| stems::is_cached(&incoming.info.path))
                        .map(|incoming| incoming.info.path.clone());
                    // Si la platine a été relancée entre-temps, le moteur refuse
                    // le chargement et `DeckUi::sync` affiche le message.
                    self.engine.send(Command::Load {
                        deck: loaded.deck,
                        id: loaded.id,
                        track,
                        grid,
                    });
                    if let Some(path) = cached {
                        self.request_stems(loaded.deck, loaded.id, path, frames, ctx);
                    }
                }
                Err(error) => {
                    self.set_status(
                        format!("Impossible de charger « {} » : {error}", loaded.info.title),
                        true,
                    );
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Clavier
    // -----------------------------------------------------------------------

    fn handle_keyboard(&mut self, ctx: &Context, dt: f32) {
        let blocked = self.transitions.captures_keyboard(ctx);
        for action in self.shortcuts.update(ctx, blocked) {
            match action {
                Action::TogglePlay(deck) => self.engine.send(Command::TogglePlay(deck)),
                Action::Cue(deck) => self.engine.send(Command::Cue(deck)),
                Action::CueSet(deck) => self.engine.send(Command::CueSet(deck)),
                Action::CueGotoAndStop(deck) => self.engine.send(Command::CueGotoAndStop(deck)),
                Action::BassKill(deck) => self.decks[deck].kill[0] ^= true,
                Action::LoadSelected(deck) => {
                    if let Some(info) = self.library_view.selected_track(&self.library).cloned() {
                        self.request_load(deck, info, ctx);
                    }
                }
                Action::Eject(deck) => {
                    if self.engine.shared.decks[deck].playing.load(Relaxed) {
                        self.set_status(format!("Platine {} en lecture.", theme::DECK_NAMES[deck]), true);
                    } else {
                        self.decks[deck].loading = None;
                        self.engine.send(Command::Eject(deck));
                    }
                }
                Action::SelectionUp => self.library_view.move_selection(&self.library, -1),
                Action::SelectionDown => self.library_view.move_selection(&self.library, 1),
                Action::FocusSearch => {
                    self.select_tab(Tab::Library);
                    self.library_view.focus_search = true;
                }
                Action::TempoStep(deck, step) => {
                    self.decks[deck].tempo += step;
                    self.decks[deck].sync = false;
                }
                Action::Marker(deck, slot) => {
                    if self.decks[deck].marker_pressed(slot, &self.engine) {
                        self.save_markers(deck);
                    }
                }
                Action::MarkerClear(deck, slot) => {
                    if self.decks[deck].marker_clear(slot) {
                        self.save_markers(deck);
                    }
                }
                Action::TransitionTrigger(n) => self.transitions.fire_manual(n),
                Action::ToggleSync(deck) => {
                    self.decks[deck].sync ^= true;
                    let events = DeckEvents {
                        sync_enabled: self.decks[deck].sync,
                        ..Default::default()
                    };
                    self.handle_deck_events(deck, events, ctx);
                }
            }
        }

        // Actions continues tant que la touche est maintenue.
        let held = self.shortcuts.held();
        let sample_rate = self.engine.shared.sample_rate as f64;
        for deck in 0..NUM_DECKS {
            let direction = f64::from(held.seek_forward[deck] as i8 - held.seek_back[deck] as i8);
            if direction != 0.0 {
                let frames = (direction * KEY_SEEK_SPEED * dt as f64 * sample_rate) as i64;
                self.engine.send(Command::SeekRelative { deck, frames });
            }
            let seeking = direction != 0.0;
            if self.key_seeking[deck] && !seeking {
                self.decks[deck].request_snap();
            }
            self.key_seeking[deck] = seeking;
        }
        let direction = f32::from(held.crossfade_right as i8 - held.crossfade_left as i8);
        if direction != 0.0 {
            let speed = if held.shift {
                KEY_CROSSFADE_SPEED * 0.25
            } else {
                KEY_CROSSFADE_SPEED
            };
            self.mixer.crossfader = (self.mixer.crossfader + direction * speed * dt).clamp(0.0, 1.0);
        }
    }

    /// Lance la séparation en stems du morceau `id` de la platine `deck`.
    fn request_stems(&mut self, deck: usize, id: u64, path: PathBuf, frames: usize, ctx: &Context) {
        if self.decks[deck].stems_requested(id) {
            return;
        }
        self.decks[deck].stems_started(id);
        let ctx = ctx.clone();
        let sample_rate = self.engine.shared.sample_rate;
        self.separator
            .request(deck, id, path, frames, sample_rate, move || ctx.request_repaint());
    }

    /// Récupère l'avancement et les résultats des séparations en stems.
    fn poll_separator(&mut self) {
        while let Some(event) = self.separator.poll() {
            match event {
                StemEvent::Progress { deck, id, fraction } => self.decks[deck].stems_progress(id, fraction),
                StemEvent::Done { deck, id, result } => {
                    let outcome = result.as_ref().map(|_| ()).map_err(Clone::clone);
                    if !self.decks[deck].stems_finished(id, outcome) {
                        continue; // le morceau a changé entre-temps
                    }
                    match result {
                        Ok(stems) => self.engine.send(Command::SetStems { deck, id, stems }),
                        Err(error) => {
                            self.set_status(format!("Stems (platine {}) : {error}", theme::DECK_NAMES[deck]), true);
                        }
                    }
                }
            }
        }
    }

    /// Enregistre les marqueurs du morceau de la platine `deck`.
    fn save_markers(&mut self, deck: usize) {
        let Some((path, markers)) = self.decks[deck].current_markers() else {
            return;
        };
        self.markers.set(path, markers);
        if let Err(error) = self.markers.save() {
            self.set_status(format!("Marqueurs non enregistrés : {error}"), true);
        }
    }

    /// SYNC et MATCH suivent l'autre platine : une seule des deux peut
    /// suivre à la fois, et il faut que l'analyse des deux morceaux le permette.
    fn handle_deck_events(&mut self, deck: usize, events: DeckEvents, ctx: &Context) {
        if events.markers_changed {
            self.save_markers(deck);
        }
        if events.reset {
            self.set_status(format!("Platine {} réinitialisée.", theme::DECK_NAMES[deck]), false);
        }
        if events.separate
            && let Some((id, path, frames)) = self.decks[deck].separation_target()
        {
            self.request_stems(deck, id, path, frames, ctx);
        }
        // Enregistrement d'étapes de transition en cours : SYNC / MATCH aussi.
        let time = ctx.input(|input| input.time);
        if events.sync_once || events.sync_enabled {
            self.transitions.record_sync(deck, time);
        }
        if events.match_once || events.match_key_enabled {
            self.transitions.record_match(deck, time);
        }
        let other = NUM_DECKS - 1 - deck;
        let (own, target) = (self.decks[deck].link(), self.decks[other].link());
        if events.sync_enabled {
            self.decks[other].sync = false;
        }
        if (events.sync_enabled || events.sync_once) && (own.bpm.is_none() || target.bpm.is_none()) {
            self.set_status(
                "SYNC : il faut un morceau au tempo connu sur chaque platine.".into(),
                true,
            );
        }
        if events.match_key_enabled {
            self.decks[other].match_key = false;
        }
        if (events.match_key_enabled || events.match_once) && (own.key.is_none() || target.key.is_none()) {
            self.set_status(
                "MATCH : il faut un morceau à la tonalité connue sur chaque platine.".into(),
                true,
            );
        }
    }

    // -----------------------------------------------------------------------
    // Panneaux
    // -----------------------------------------------------------------------

    fn top_bar(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("RUSTYMIX")
                    .strong()
                    .size(16.0)
                    .color(theme::DECK_COLORS[0]),
            );
            ui.label(RichText::new(&self.audio_info).color(theme::TEXT_DIM).size(12.0));

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button("Raccourcis clavier").clicked() {
                    self.show_help = !self.show_help;
                }
                if self
                    .status
                    .as_ref()
                    .is_some_and(|status| status.since.elapsed() > STATUS_DURATION)
                {
                    self.status = None;
                }
                if let Some(status) = &self.status {
                    let color = if status.is_error { theme::RED } else { theme::GREEN };
                    ui.label(RichText::new(&status.text).color(color));
                }
            });
        });
    }

    /// Dessine les deux platines, le master et le crossfader.
    /// Retourne les rectangles des platines (cibles du glisser-déposer).
    fn decks_panel(&mut self, ui: &mut Ui, time: f64) -> [Rect; NUM_DECKS] {
        let full = ui.available_rect_before_wrap();
        ui.allocate_rect(full, Sense::hover());

        let mut area = full.shrink2(vec2(GAP, GAP * 0.5));
        let crossfader = take_bottom(&mut area, CROSSFADER_HEIGHT);
        take_bottom(&mut area, GAP);

        let deck_width = ((area.width() - MIXER_WIDTH - 2.0 * GAP) / 2.0).max(0.0);
        let deck_a = Rect::from_min_size(area.min, vec2(deck_width, area.height()));
        let mixer = Rect::from_min_size(deck_a.right_top() + vec2(GAP, 0.0), vec2(MIXER_WIDTH, area.height()));
        let deck_b = Rect::from_min_size(mixer.right_top() + vec2(GAP, 0.0), vec2(deck_width, area.height()));

        let rects = [deck_a, deck_b];
        for (index, rect) in rects.into_iter().enumerate() {
            let events = self.decks[index].show(ui, rect, &self.engine, time);
            self.handle_deck_events(index, events, &ui.ctx().clone());
        }
        self.mixer.show_master(ui, mixer);
        self.mixer.show_crossfader(ui, crossfader);
        rects
    }

    /// Glisser-déposer d'un morceau de la bibliothèque vers une platine.
    fn handle_drag_and_drop(&mut self, ctx: &Context, deck_rects: [Rect; NUM_DECKS]) {
        let Some(payload) = egui::DragAndDrop::payload::<DraggedTrack>(ctx) else {
            return;
        };
        let pointer = ctx.pointer_latest_pos();
        let released = ctx.input(|input| input.pointer.any_released());
        let painter = ctx.layer_painter(LayerId::new(Order::Tooltip, Id::unique("drag_preview")));
        ctx.set_cursor_icon(egui::CursorIcon::Grabbing);

        for (deck, rect) in deck_rects.into_iter().enumerate() {
            if !pointer.is_some_and(|pos| rect.contains(pos)) {
                continue;
            }
            let playing = self.engine.shared.decks[deck].playing.load(Relaxed);
            let color = if playing { theme::RED } else { theme::DECK_COLORS[deck] };
            painter.rect_filled(rect, 8.0, color.gamma_multiply(0.12));
            painter.rect_stroke(rect, 8.0, Stroke::new(2.0, color), StrokeKind::Inside);
            if released {
                self.request_load(deck, payload.0.clone(), ctx);
            }
        }

        // Étiquette qui suit le pointeur.
        if let Some(pos) = pointer {
            let galley = painter.layout_no_wrap(payload.0.title.clone(), FontId::proportional(13.0), theme::TEXT);
            let rect = Rect::from_min_size(pos + vec2(14.0, 10.0), galley.size()).expand2(vec2(8.0, 4.0));
            painter.rect_filled(rect, 4.0, theme::PANEL_LIGHT);
            painter.rect_stroke(rect, 4.0, Stroke::new(1.0, theme::OUTLINE), StrokeKind::Outside);
            painter.galley(rect.min + vec2(8.0, 4.0), galley, theme::TEXT);
        }

        if released {
            egui::DragAndDrop::clear_payload(ctx);
        }
    }

    fn help_window(&mut self, ctx: &Context) {
        egui::Window::new("Raccourcis clavier")
            .open(&mut self.show_help)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label(
                    RichText::new(
                        "Disposition par défaut de Mixxx. Les lettres correspondent à la position \
                         physique des touches d'un clavier QWERTY.",
                    )
                    .color(theme::TEXT_DIM),
                );
                ui.add_space(6.0);
                egui::Grid::new("shortcuts_grid")
                    .striped(true)
                    .num_columns(3)
                    .spacing([24.0, 6.0])
                    .show(ui, |ui| {
                        ui.label(RichText::new("Fonction").strong());
                        ui.label(RichText::new("Platine A").strong().color(theme::DECK_COLORS[0]));
                        ui.label(RichText::new("Platine B").strong().color(theme::DECK_COLORS[1]));
                        ui.end_row();
                        for (function, a, b) in HELP {
                            ui.label(*function);
                            ui.label(RichText::new(*a).monospace());
                            ui.label(RichText::new(*b).monospace());
                            ui.end_row();
                        }
                    });
            });
    }
}

impl eframe::App for RustymixApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let (dt, time) = ctx.input(|input| (input.stable_dt.min(0.1), input.time));

        // --- Logique : moteur, chargements, clavier ---
        self.engine.collect_garbage();
        self.library.poll();
        self.poll_loader(&ctx);
        self.poll_separator();
        self.handle_keyboard(&ctx, dt);
        for (text, is_error) in self
            .transitions
            .tick(&mut self.decks, &mut self.mixer, &self.engine, time)
        {
            self.set_status(text, is_error);
        }
        for index in 0..NUM_DECKS {
            let other = self.decks[NUM_DECKS - 1 - index].link();
            self.decks[index].follow(&other);
            if let Some(error) = self.decks[index].sync(&self.engine, dt) {
                self.set_status(error, true);
            }
            // Un morceau lancé (même brièvement) est marqué « déjà joué ».
            if self.engine.shared.decks[index].playing.load(Relaxed)
                && let Some(path) = self.decks[index].current_path()
                && !self.library_view.played.contains(path)
            {
                self.library_view.played.insert(path.to_path_buf());
            }
        }
        self.mixer.sync(&self.engine, dt);

        // --- Interface ---
        egui::Panel::top("top_bar")
            .frame(Frame::new().fill(theme::BACKGROUND).inner_margin(vec2(10.0, 6.0)))
            .show_separator_line(false)
            .show(ui, |ui| self.top_bar(ui));

        let deck_rects = egui::Panel::top("decks")
            .frame(Frame::new().fill(theme::BACKGROUND))
            .exact_size(DECKS_PANEL_HEIGHT)
            .show_separator_line(false)
            .show(ui, |ui| self.decks_panel(ui, time))
            .inner;

        // Barre d'activité (à gauche, sous les platines) puis panneau choisi.
        let items = self.activity_items();
        let clicked = egui::Panel::left("activity_bar")
            .frame(Frame::new().fill(theme::BACKGROUND))
            .exact_size(BAR_WIDTH)
            .resizable(false)
            .show_separator_line(false)
            .show(ui, |ui| activity_bar(ui, &items))
            .inner;
        if let Some(tab) = clicked {
            self.select_tab(tab);
        }
        self.tab_shortcuts(&ctx);

        let tab = self.tab;
        let action = egui::CentralPanel::default()
            .frame(Frame::new().fill(theme::PANEL).inner_margin(vec2(10.0, 8.0)))
            .show(ui, |ui| {
                if tab == Tab::Library {
                    let references = std::array::from_fn(|deck| {
                        let ui = &self.decks[deck];
                        Some(SuggestSource {
                            reference: ui.suggestion_reference()?,
                            path: ui.current_path()?.to_path_buf(),
                            title: ui.current_title().unwrap_or_default().to_string(),
                            playing: self.engine.shared.decks[deck].playing.load(Relaxed),
                        })
                    });
                    self.library_view
                        .show(ui, &mut self.library, &self.config.folders, &references)
                } else {
                    self.panel_header(ui, tab, true);
                    self.panel_contents(ui, tab, &ctx);
                    None
                }
            })
            .inner;
        if let Some(action) = action {
            self.handle_library_action(action, &ctx);
        }
        self.floating_windows(&ctx, (tab != Tab::Library).then_some(tab));

        self.handle_drag_and_drop(&ctx, deck_rects);
        self.help_window(&ctx);
        self.update_install(&ctx);

        // ~60 images/s tant que quelque chose bouge, repos sinon.
        let animating = self.decks.iter().any(|deck| deck.is_animating(&self.engine))
            || self.mixer.is_animating()
            || self.library.is_scanning()
            || self.status.is_some()
            || self.install.as_ref().is_some_and(|install| install.is_animating())
            || self.transitions.is_running()
            || self.transitions.is_recording();
        if animating {
            ctx.request_repaint();
        } else {
            ctx.request_repaint_after(Duration::from_millis(200));
        }
    }
}

/// Ouvre la sortie audio par défaut et y installe le moteur.
/// Sans périphérique, l'application reste utilisable (mais muette).
fn start_audio() -> (EngineHandle, Option<cpal::Stream>, String, Option<String>) {
    match OutputDevice::open_default() {
        Ok(device) => {
            let sample_rate = device.sample_rate();
            let info = format!("{} — {} Hz", device.name, sample_rate);
            let (engine, handle) = Engine::new(sample_rate);
            match device.start(engine) {
                Ok(stream) => (handle, Some(stream), info, None),
                Err(error) => (handle, None, "Pas de sortie audio".into(), Some(error)),
            }
        }
        Err(error) => {
            let (_engine, handle) = Engine::new(48_000);
            (handle, None, "Pas de sortie audio".into(), Some(error))
        }
    }
}
