//! Panneau de la bibliothèque : gestion des dossiers, recherche et tableau
//! triable des morceaux. Les lignes peuvent être glissées vers une platine.
//!
//! Mode « Suggestions » : les morceaux sont classés selon leur compatibilité
//! avec celui d'une platine (tempo, tonalité, énergie ; voir `suggest`).

use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;

use egui::{
    Align, Align2, Button, Color32, FontId, Label, Layout, Pos2, Rect, RichText, Sense, Shape, Stroke, TextEdit, Ui,
    pos2, vec2,
};
use egui_extras::{Column, TableBuilder};

use super::icons::{self, Icon};
use super::theme;
use crate::library::{Library, SortColumn, TrackInfo};
use crate::suggest::{KeyFit, MAX_TEMPO_PERCENT, Match, Reference, rate};

/// Platine de référence des suggestions.
pub struct SuggestSource {
    pub reference: Reference,
    pub path: PathBuf,
    pub title: String,
    pub playing: bool,
}

/// Classement calculé, réutilisé tant que rien ne change.
struct Ranking {
    key: u64,
    rows: Vec<(usize, Match)>,
}

const ROW_HEIGHT: f32 = 22.0;

/// Charge utile du glisser-déposer d'un morceau vers une platine.
pub struct DraggedTrack(pub TrackInfo);

/// Demande de l'utilisateur à traiter par l'application.
pub enum LibraryAction {
    AddFolders,
    RemoveFolder(usize),
    Rescan,
    /// L'utilisateur veut installer un son (recherche + téléchargement).
    Install,
    /// Proposer des morceaux similaires à celui-ci (panneau d'installation).
    Similar(TrackInfo),
}

#[derive(Default)]
pub struct LibraryView {
    /// Morceau sélectionné (indice dans la bibliothèque).
    selected: Option<usize>,
    /// Faire défiler le tableau jusqu'à la sélection à la prochaine image.
    scroll_to_selection: bool,
    search: String,
    /// Donner le focus au champ de recherche à la prochaine image.
    pub focus_search: bool,
    /// Morceaux déjà joués pendant cette session (titre assombri).
    pub played: HashSet<PathBuf>,
    /// Mode suggestions, et platine de référence choisie (`None` : celle qui
    /// joue).
    suggest: bool,
    suggest_for: Option<usize>,
    ranking: Option<Ranking>,
}

impl LibraryView {
    pub fn selected_track<'a>(&self, library: &'a Library) -> Option<&'a TrackInfo> {
        self.selected.and_then(|index| library.track(index))
    }

    /// À appeler quand la bibliothèque est vidée (rescan).
    pub fn clear_selection(&mut self) {
        self.selected = None;
    }

    /// Lignes affichées : la vue de la bibliothèque, ou le classement des suggestions.
    fn rows(&self, library: &Library) -> Vec<usize> {
        match (&self.ranking, self.suggest) {
            (Some(ranking), true) => ranking.rows.iter().map(|(index, _)| *index).collect(),
            _ => library.view().to_vec(),
        }
    }

    /// Déplace la sélection de `delta` lignes dans la vue courante.
    pub fn move_selection(&mut self, library: &Library, delta: isize) {
        let view = self.rows(library);
        if view.is_empty() {
            return;
        }
        let current = self
            .selected
            .and_then(|selected| view.iter().position(|&index| index == selected));
        let row = match current {
            Some(row) => (row as isize + delta).clamp(0, view.len() as isize - 1) as usize,
            None => 0,
        };
        self.selected = Some(view[row]);
        self.scroll_to_selection = true;
    }

    pub fn show(
        &mut self,
        ui: &mut Ui,
        library: &mut Library,
        folders: &[PathBuf],
        sources: &[Option<SuggestSource>; 2],
    ) -> Option<LibraryAction> {
        library.refresh();
        let deck = self.reference_deck(sources);
        let action = self.toolbar(ui, library, folders, sources, deck);
        ui.add_space(4.0);

        if folders.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.label(
                    RichText::new("Ajoutez un dossier de musique pour remplir la bibliothèque.")
                        .color(theme::TEXT_DIM)
                        .size(15.0),
                );
            });
            return action;
        }

        // La recherche a pu changer dans la barre d'outils.
        library.refresh();
        let source = deck.and_then(|deck| sources[deck].as_ref());
        if self.suggest {
            self.rank(library, source, deck);
        } else {
            self.ranking = None;
        }
        let (clicked, similar) = self.table(ui, library, source);
        if let Some(track) = similar {
            return Some(LibraryAction::Similar(track));
        }
        if let Some(column) = clicked {
            // Trier par une colonne quitte les suggestions.
            self.suggest = false;
            self.ranking = None;
            library.toggle_sort(column);
        }
        action
    }

    /// Platine de référence : celle choisie, sinon celle qui joue (seule),
    /// sinon la première qui a un morceau.
    fn reference_deck(&self, sources: &[Option<SuggestSource>; 2]) -> Option<usize> {
        if let Some(deck) = self.suggest_for.filter(|&deck| sources[deck].is_some()) {
            return Some(deck);
        }
        let playing: Vec<usize> = (0..2)
            .filter(|&deck| sources[deck].as_ref().is_some_and(|s| s.playing))
            .collect();
        match playing[..] {
            [deck] => Some(deck),
            _ => (0..2).find(|&deck| sources[deck].is_some()),
        }
    }

    /// Classe les morceaux de la vue selon leur compatibilité avec `source`
    /// (les morceaux déjà joués en dernier) ; recalculé seulement si la vue,
    /// les analyses ou la référence ont changé.
    fn rank(&mut self, library: &Library, source: Option<&SuggestSource>, deck: Option<usize>) {
        let Some(source) = source else {
            self.ranking = Some(Ranking {
                key: 0,
                rows: Vec::new(),
            });
            return;
        };
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        library.view().hash(&mut hasher);
        library.pending_analysis().hash(&mut hasher);
        self.played.len().hash(&mut hasher);
        deck.hash(&mut hasher);
        source.path.hash(&mut hasher);
        source.reference.bpm.map(f32::to_bits).hash(&mut hasher);
        source
            .reference
            .key
            .map(|key| (key.pitch_class, key.minor))
            .hash(&mut hasher);
        source.reference.energy.map(f32::to_bits).hash(&mut hasher);
        let key = hasher.finish() | 1;
        if self.ranking.as_ref().is_some_and(|ranking| ranking.key == key) {
            return;
        }
        let mut rows: Vec<(usize, Match, f32)> = library
            .view()
            .iter()
            .filter_map(|&index| {
                let track = library.track(index)?;
                if track.path == source.path {
                    return None;
                }
                let found = rate(&source.reference, &track.analysis.unwrap_or_default());
                let order = if self.played.contains(&track.path) {
                    found.score - 1.0
                } else {
                    found.score
                };
                Some((index, found, order))
            })
            .collect();
        rows.sort_by(|a, b| b.2.total_cmp(&a.2));
        self.ranking = Some(Ranking {
            key,
            rows: rows.into_iter().map(|(index, found, _)| (index, found)).collect(),
        });
    }

    fn toolbar(
        &mut self,
        ui: &mut Ui,
        library: &mut Library,
        folders: &[PathBuf],
        sources: &[Option<SuggestSource>; 2],
        deck: Option<usize>,
    ) -> Option<LibraryAction> {
        let mut action = None;
        ui.horizontal(|ui| {
            // Suggestions : bouton, puis platine de référence.
            let hint = "Suggestions : classer les morceaux selon leur compatibilité avec la platine \
                        choisie (tempo, tonalité, énergie)";
            if icons::icon_button(ui, Icon::Match, theme::TEXT_DIM, self.suggest, true, hint).clicked() {
                self.suggest = !self.suggest;
                self.ranking = None;
            }
            if self.suggest {
                for (index, source) in sources.iter().enumerate() {
                    let selected = deck == Some(index);
                    let color = theme::DECK_COLORS[index];
                    let text = RichText::new(theme::DECK_NAMES[index]).strong().color(if selected {
                        Color32::BLACK
                    } else if source.is_some() {
                        color
                    } else {
                        theme::TEXT_DIM
                    });
                    let button = Button::new(text)
                        .fill(if selected { color } else { Color32::TRANSPARENT })
                        .min_size(vec2(22.0, 20.0));
                    let hint = match (source, self.suggest_for == Some(index)) {
                        (None, _) => format!("Platine {} vide", theme::DECK_NAMES[index]),
                        (Some(source), true) => {
                            format!("Pour « {} » — clic : suivre la platine qui joue", source.title)
                        }
                        (Some(source), false) => format!("Pour « {} »", source.title),
                    };
                    let response = ui.add_enabled(source.is_some(), button).on_hover_text(hint);
                    if response.clicked() {
                        self.suggest_for = if self.suggest_for == Some(index) {
                            None
                        } else {
                            Some(index)
                        };
                        self.ranking = None;
                    }
                }
                ui.separator();
            }
            if ui.button("+ Ajouter un dossier…").clicked() {
                action = Some(LibraryAction::AddFolders);
            }
            if ui.add_enabled(!folders.is_empty(), Button::new("Rescanner")).clicked() {
                action = Some(LibraryAction::Rescan);
            }
            if ui
                .button("Installer…")
                .on_hover_text("Rechercher et télécharger un son (YouTube, Spotify)")
                .clicked()
            {
                action = Some(LibraryAction::Install);
            }
            ui.menu_button(format!("Dossiers ({})", folders.len()), |ui| {
                if folders.is_empty() {
                    ui.label("Aucun dossier");
                }
                for (index, folder) in folders.iter().enumerate() {
                    ui.horizontal(|ui| {
                        if ui.small_button("×").on_hover_text("Retirer ce dossier").clicked() {
                            action = Some(LibraryAction::RemoveFolder(index));
                        }
                        ui.label(folder.display().to_string());
                    });
                }
            });

            ui.separator();
            if library.is_scanning() {
                ui.spinner();
                ui.label(format!("Scan en cours… {} morceaux", library.len()));
            } else if library.pending_analysis() > 0 {
                ui.spinner();
                ui.label(format!(
                    "{} morceaux — analyse tempo / tonalité : {} restants",
                    library.len(),
                    library.pending_analysis()
                ));
            } else if library.query().is_empty() {
                ui.label(RichText::new(format!("{} morceaux", library.len())).color(theme::TEXT_DIM));
            } else {
                ui.label(
                    RichText::new(format!("{} / {} morceaux", library.view().len(), library.len()))
                        .color(theme::TEXT_DIM),
                );
            }

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let response = ui.add(
                    TextEdit::singleline(&mut self.search)
                        .hint_text("Rechercher… (Ctrl+F)")
                        .desired_width(280.0),
                );
                if self.focus_search {
                    response.request_focus();
                    self.focus_search = false;
                }
                if response.changed() {
                    library.set_query(&self.search);
                }
            });
        });
        action
    }

    /// Dessine le tableau ; retourne la colonne dont l'en-tête a été cliqué
    /// et le morceau dont on demande des similaires (clic droit).
    fn table(
        &mut self,
        ui: &mut Ui,
        library: &Library,
        source: Option<&SuggestSource>,
    ) -> (Option<SortColumn>, Option<TrackInfo>) {
        let suggest = self.suggest;
        let mut similar = None;
        if suggest && source.is_none() {
            ui.add_space(24.0);
            ui.vertical_centered(|ui| {
                ui.label(RichText::new("Chargez un morceau sur une platine").color(theme::TEXT_DIM));
            });
            return (None, None);
        }
        let view = self.rows(library);
        let matches: Vec<Option<Match>> = match (&self.ranking, suggest) {
            (Some(ranking), true) => ranking.rows.iter().map(|(_, found)| Some(*found)).collect(),
            _ => vec![None; view.len()],
        };
        let (sort_column, ascending) = library.sort();
        let mut clicked_header = None;

        let mut table = TableBuilder::new(ui)
            .striped(true)
            .resizable(true)
            .sense(Sense::click_and_drag())
            .cell_layout(Layout::left_to_right(Align::Center));
        if suggest {
            table = table.column(Column::exact(104.0));
        }
        let mut table = table
            .column(Column::initial(320.0).at_least(120.0).clip(true))
            .column(Column::initial(220.0).at_least(80.0).clip(true))
            .column(Column::exact(64.0))
            .column(Column::exact(70.0))
            .column(Column::exact(140.0))
            .column(Column::remainder().at_least(120.0).clip(true))
            .min_scrolled_height(0.0)
            .auto_shrink(false);

        if std::mem::take(&mut self.scroll_to_selection)
            && let Some(row) = self
                .selected
                .and_then(|selected| view.iter().position(|&index| index == selected))
        {
            table = table.scroll_to_row(row, None);
        }

        let headers = [
            ("Titre", SortColumn::Title),
            ("Artiste", SortColumn::Artist),
            ("Durée", SortColumn::Duration),
            ("BPM", SortColumn::Bpm),
            ("Tonalité", SortColumn::Key),
            ("Fichier", SortColumn::FileName),
        ];

        table
            .header(24.0, |mut header| {
                if suggest {
                    header.col(|ui| {
                        let title = source.map_or(String::new(), |source| source.title.clone());
                        ui.label(RichText::new("Mix").strong())
                            .on_hover_text(format!("Compatibilité avec « {title} » — meilleurs en premier"));
                    });
                }
                for (label, column) in headers {
                    header.col(|ui| {
                        let button = Button::new(RichText::new(label).strong()).frame(false);
                        let response = ui.add(button).on_hover_text("Trier par cette colonne");
                        if response.clicked() {
                            clicked_header = Some(column);
                        }
                        if sort_column == column && !suggest {
                            sort_arrow(ui, response.rect.right_center() + vec2(9.0, 0.0), ascending);
                        }
                    });
                }
            })
            .body(|body| {
                body.rows(ROW_HEIGHT, view.len(), |mut row| {
                    let index = view[row.index()];
                    let Some(track) = library.track(index) else { return };
                    row.set_selected(self.selected == Some(index));

                    let played = self.played.contains(&track.path);
                    if let Some(found) = matches.get(row.index()).copied().flatten() {
                        row.col(|ui| match_cell(ui, &found, played));
                    }
                    let cell = |ui: &mut Ui, text: &str, dim: bool| {
                        let text = RichText::new(text);
                        let text = if played {
                            text.color(theme::PLAYED)
                        } else if dim {
                            text.color(theme::TEXT_DIM)
                        } else {
                            text
                        };
                        ui.add(Label::new(text).truncate().selectable(false));
                    };
                    row.col(|ui| cell(ui, &track.title, false));
                    row.col(|ui| cell(ui, &track.artist, false));
                    row.col(|ui| cell(ui, &format_duration(track.duration), true));
                    // « … » tant que l'analyse est en cours, « – » si rien n'a été détecté.
                    let (bpm, key) = match track.analysis {
                        None => ("…".to_string(), "…".to_string()),
                        Some(analysis) => (
                            analysis.bpm.map_or("–".to_string(), |bpm| format!("{bpm:.1}")),
                            analysis
                                .key
                                .map_or("–".to_string(), |key| format!("{} ({})", key.name(), key.camelot())),
                        ),
                    };
                    row.col(|ui| cell(ui, &bpm, false));
                    row.col(|ui| cell(ui, &key, false));
                    row.col(|ui| cell(ui, &track.file_name, true));

                    let response = row.response();
                    if response.clicked() {
                        self.selected = Some(index);
                    }
                    if response.drag_started() {
                        self.selected = Some(index);
                        egui::DragAndDrop::set_payload(&response.ctx, DraggedTrack(track.clone()));
                    }
                    response.context_menu(|ui| {
                        if ui.button("Morceaux similaires…").clicked() {
                            self.selected = Some(index);
                            similar = Some(track.clone());
                            ui.close();
                        }
                    });
                });
            });

        (clicked_header, similar)
    }
}

/// Cellule « Mix » : note (barre), tempo à rattraper, accord de tonalité
/// (pastille) et énergie (flèche) ; le détail au survol.
fn match_cell(ui: &mut Ui, found: &Match, played: bool) {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 18.0), Sense::hover());
    let painter = ui.painter();
    let fade = if played { 0.45 } else { 1.0 };
    let y = rect.center().y;

    // Note.
    let bar = Rect::from_min_size(pos2(rect.left(), y - 3.0), vec2(30.0, 6.0));
    painter.rect_filled(bar, 3.0, theme::WAVE_BACKGROUND);
    let color = if found.score >= 0.75 {
        theme::GREEN
    } else if found.score >= 0.5 {
        theme::YELLOW
    } else {
        theme::RED
    };
    let mut filled = bar;
    filled.set_width((bar.width() * found.score).max(2.0));
    painter.rect_filled(filled, 3.0, color.gamma_multiply(fade));

    // Tempo à rattraper.
    let tempo = match (found.tempo_percent, found.tempo_multiple) {
        (Some(percent), multiple) => {
            let prefix = match multiple {
                Some(m) if m > 1.0 => "×2 ",
                Some(_) => "½ ",
                None => "",
            };
            // Arrondi au pour cent, sans « -0 ».
            let rounded = percent.round();
            if rounded == 0.0 {
                format!("{prefix}0%")
            } else {
                format!("{prefix}{rounded:+.0}%")
            }
        }
        (None, _) => "–".to_string(),
    };
    let tempo_color = if found.tempo_percent.is_some() && !found.tempo_reachable() {
        theme::RED
    } else {
        theme::TEXT_DIM
    };
    painter.text(
        pos2(bar.right() + 6.0, y),
        Align2::LEFT_CENTER,
        tempo,
        FontId::monospace(11.0),
        tempo_color.gamma_multiply(fade),
    );

    // Tonalité : pastille pleine (même), atténuée (compatible), jaune (MATCH), rouge.
    let dot = pos2(rect.right() - 22.0, y);
    match found.key {
        Some(KeyFit::Same) => {
            painter.circle_filled(dot, 4.5, theme::GREEN.gamma_multiply(fade));
        }
        Some(KeyFit::Compatible) => {
            painter.circle_filled(dot, 4.5, theme::GREEN.gamma_multiply(0.55 * fade));
        }
        Some(KeyFit::Shift(_)) => {
            painter.circle_filled(dot, 4.5, theme::YELLOW.gamma_multiply(fade));
        }
        Some(KeyFit::Clash) => {
            painter.circle_filled(dot, 4.5, theme::RED.gamma_multiply(fade));
        }
        None => {
            painter.circle_stroke(dot, 4.0, Stroke::new(1.0, theme::TEXT_DIM));
        }
    }

    // Énergie.
    let arrow = pos2(rect.right() - 8.0, y);
    let ink = theme::TEXT_DIM.gamma_multiply(fade);
    match found.energy_delta {
        Some(delta) if delta > 0.08 => {
            painter.add(Shape::convex_polygon(
                vec![arrow + vec2(0.0, -5.0), arrow + vec2(4.5, 3.0), arrow + vec2(-4.5, 3.0)],
                ink,
                Stroke::NONE,
            ));
        }
        Some(delta) if delta < -0.08 => {
            painter.add(Shape::convex_polygon(
                vec![
                    arrow + vec2(0.0, 5.0),
                    arrow + vec2(-4.5, -3.0),
                    arrow + vec2(4.5, -3.0),
                ],
                ink,
                Stroke::NONE,
            ));
        }
        Some(_) => {
            painter.hline((arrow.x - 4.0)..=(arrow.x + 4.0), y, Stroke::new(2.0, ink));
        }
        None => {}
    }

    response.on_hover_text(match_details(found));
}

/// Détail d'une suggestion, pour l'infobulle.
fn match_details(found: &Match) -> String {
    let tempo = match (found.tempo_percent, found.tempo_multiple) {
        (Some(percent), _) if percent.abs() > MAX_TEMPO_PERCENT => {
            format!("tempo trop éloigné ({percent:+.1} %, au-delà de ±12 %)")
        }
        (Some(percent), Some(multiple)) => format!(
            "tempo {percent:+.1} % pour caler, joué {}",
            if multiple > 1.0 { "au double" } else { "à la moitié" }
        ),
        (Some(percent), None) => format!("tempo {percent:+.1} % pour caler"),
        (None, _) => "tempo inconnu".to_string(),
    };
    let key = match found.key {
        Some(KeyFit::Same) => "même tonalité".to_string(),
        Some(KeyFit::Compatible) => "tonalité compatible".to_string(),
        Some(KeyFit::Shift(n)) => format!("tonalité compatible transposée de {n:+} demi-ton(s) (MATCH)"),
        Some(KeyFit::Clash) => "tonalités qui se heurtent".to_string(),
        None => "tonalité inconnue".to_string(),
    };
    let energy = match found.energy_delta {
        Some(delta) if delta > 0.08 => "plus d'énergie",
        Some(delta) if delta < -0.08 => "moins d'énergie",
        Some(_) => "même énergie",
        None => "énergie inconnue",
    };
    format!("Compatibilité {:.0} %\n{tempo}\n{key}\n{energy}", found.score * 100.0)
}

/// Petit triangle indiquant le sens du tri (dessiné : les polices par défaut
/// n'ont pas de glyphe fiable pour cela).
fn sort_arrow(ui: &Ui, center: Pos2, ascending: bool) {
    let direction = if ascending { -1.0 } else { 1.0 };
    let points = vec![
        center + vec2(0.0, direction * 3.5),
        center + vec2(-4.0, -direction * 3.0),
        center + vec2(4.0, -direction * 3.0),
    ];
    ui.painter()
        .add(Shape::convex_polygon(points, theme::TEXT_DIM, Stroke::NONE));
}

/// `m:ss`, ou vide si la durée est inconnue.
fn format_duration(duration: Option<f32>) -> String {
    match duration {
        Some(secs) => {
            let total = secs.round() as u32;
            format!("{}:{:02}", total / 60, total % 60)
        }
        None => String::new(),
    }
}
