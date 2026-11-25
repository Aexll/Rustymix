//! Éditeur de nœuds des transitions, façon « Blueprint » d'Unreal Engine.
//!
//! Commandes (reprises d'Unreal) :
//! * clic droit sur le fond : recherche pour ajouter un nœud ;
//! * glisser depuis une broche : relier ; relâcher dans le vide ouvre la
//!   recherche et relie le nouveau nœud ; Alt + clic sur une broche : rompre ;
//!   Ctrl + glisser une sortie : déplacer son lien ;
//! * clic droit glissé (ou bouton du milieu) : déplacer la vue ; molette : zoom ;
//! * glisser sur le fond : sélection rectangulaire (Maj / Ctrl : ajouter) ;
//! * Suppr : supprimer ; Ctrl + C / X / V : copier, couper, coller ;
//!   Ctrl + D (ou W) : dupliquer ; Ctrl + Z / Y : annuler, rétablir ;
//!   Ctrl + A : tout sélectionner ; Origine : tout cadrer ; F : cadrer la
//!   sélection ; C : commentaire autour de la sélection ; F7 à F10 :
//!   déclencheurs manuels.

use std::collections::{HashMap, HashSet};

use egui::emath::TSTransform;
use egui::epaint::CubicBezierShape;
use egui::{
    Align, Align2, Color32, ComboBox, Context, CornerRadius, DragValue, Event, FontId, Frame, Id, Key, LayerId, Layout,
    Modifiers, Order, PointerButton, Pos2, Rect, RichText, ScrollArea, Sense, Shape, Stroke, StrokeKind, TextEdit, Ui,
    UiBuilder, Vec2, pos2, vec2,
};

use super::deck::{format_time, key_label};
use super::icons::{Icon, icon_button};
use super::theme;
use super::transitions::{capitalize, marker_color, play_button};
use super::widgets::{
    Orientation, TransportIcon, fader, fader_ghost, knob, knob_ghost, toggle_button, transport_button,
};
use crate::analysis::Key as MusicalKey;
use crate::markers::NUM_TRIGGERS;
use crate::markers::{MARKER_END, MARKER_START, NUM_MARKERS, Triggers};
use crate::transition_graph::{Comment, Compiled, DelayUnit, Graph, Group, Level, Link, Node, NodeKind};
use crate::transitions::{
    ACTION_KINDS, ALL_TRIGGERS, Action, Control, MANUAL_FIRST, MANUAL_KEYS, MIN_DB, Side, Transition, beat_label,
    filter_label, is_manual, trigger_long_name, trigger_short_name,
};

/// Pas de la grille (les nœuds s'y calent quand on les lâche).
const GRID: f32 = 16.0;
const HEADER_H: f32 = 28.0;
/// Hauteur d'une rangée de broches d'exécution.
const ROW_H: f32 = 22.0;
/// Distance des broches au bord du nœud.
const PIN_INSET: f32 = 14.0;
const ZOOM_MIN: f32 = 0.2;
const ZOOM_MAX: f32 = 1.5;
/// Durée de la surbrillance d'un nœud qui vient de s'exécuter.
const FLASH_SECS: f64 = 0.8;
const UNDO_LIMIT: usize = 100;

const BACKGROUND: Color32 = Color32::from_rgb(0x1d, 0x1e, 0x22);
const GRID_MINOR: Color32 = Color32::from_rgb(0x27, 0x28, 0x2d);
const GRID_MAJOR: Color32 = Color32::from_rgb(0x12, 0x12, 0x15);
const NODE_BODY: Color32 = Color32::from_rgba_premultiplied(0x16, 0x17, 0x1a, 0xf0);
const NODE_BORDER: Color32 = Color32::from_rgb(0x3a, 0x3c, 0x44);
const SELECTED: Color32 = Color32::from_rgb(0xf1, 0xb0, 0x00);
const WIRE: Color32 = Color32::from_rgb(0xe4, 0xe4, 0xe8);
const WIRE_ACTIVE: Color32 = Color32::from_rgb(0xff, 0xd6, 0x5a);

/// État d'exécution d'une étape, vu par l'éditeur.
#[derive(Clone, Copy, PartialEq)]
pub enum StepStatus {
    Pending,
    Done,
    Skipped,
    /// Programmée : exécution dans ce nombre de secondes.
    Scheduled(f64),
}

/// Ce que l'éditeur doit savoir des platines et de l'exécution.
pub struct RunView<'a> {
    /// Platines de départ et de destination.
    pub from: usize,
    pub to: usize,
    /// Positions des marqueurs du départ (non posés : `None`).
    pub from_markers: Triggers,
    /// État de chaque étape de la transition.
    pub statuses: &'a [StepStatus],
    /// Instant du dernier déclenchement de chaque déclencheur.
    pub fired_at: &'a [Option<f64>],
    pub now: f64,
    pub can_fire: bool,
    pub can_activate: bool,
    pub active: bool,
    /// Tonalité d'origine du morceau de chaque platine (slider de tonalité).
    pub keys: [Option<MusicalKey>; 2],
    /// Déclencheur dont les étapes sont en cours d'enregistrement.
    pub recording: Option<usize>,
    pub can_record: bool,
}

/// Ce que l'application doit faire après l'affichage de l'éditeur.
#[derive(Default)]
pub struct NodeOutcome {
    /// Déclencheurs à lancer maintenant.
    pub fire: Vec<usize>,
    pub toggle_active: bool,
    /// Démarrer (ou arrêter) l'enregistrement d'étapes sur ce déclencheur.
    pub record: Option<usize>,
    /// Les étapes de la transition ont changé.
    pub steps_changed: bool,
    /// Quelque chose est à enregistrer.
    pub changed: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PinRef {
    Out { node: u64, pin: usize },
    In { node: u64 },
}

impl PinRef {
    fn node(self) -> u64 {
        match self {
            PinRef::Out { node, .. } | PinRef::In { node } => node,
        }
    }
}

/// Interaction en cours avec la souris.
enum Drag {
    None,
    Pan,
    Marquee {
        start: Pos2,
    },
    Link {
        from: PinRef,
    },
    Nodes,
    /// Déplacement (avec les nœuds qu'il contient) ou redimensionnement d'un commentaire.
    Comment {
        id: u64,
        resize: bool,
        members: Vec<u64>,
    },
}

/// Fenêtre de recherche ouverte par un clic droit (ou un lien lâché dans le vide).
struct Search {
    screen: Pos2,
    scene: Pos2,
    query: String,
    selected: usize,
    from: Option<PinRef>,
    focus: bool,
    opened_at: f64,
    /// La saisie vient de changer : présélectionner la meilleure entrée.
    rank: bool,
}

struct Clipboard {
    nodes: Vec<Node>,
    links: Vec<Link>,
}

/// Ce que crée une entrée de la recherche.
#[derive(Clone, Copy)]
enum NewItem {
    Node(NodeKind),
    Comment,
}

struct Entry {
    category: &'static str,
    name: String,
    keywords: &'static str,
    item: NewItem,
}

pub struct NodeEditor {
    /// Image où l'éditeur a été dessiné pour la dernière fois.
    drawn_frame: Option<u64>,
    /// Rectangle du canevas à l'image précédente (la vue suit le panneau).
    last_outer: Option<Rect>,
    /// Transition affichée (la vue est réinitialisée quand elle change).
    transition: Option<u64>,
    /// Coordonnées du graphe → écran.
    to_global: TSTransform,
    frame_pending: Option<bool>,
    selection: HashSet<u64>,
    selected_comments: HashSet<u64>,
    drag: Drag,
    search: Option<Search>,
    clipboard: Option<Clipboard>,
    undo: Vec<Graph>,
    redo: Vec<Graph>,
    /// Dernier état validé (pour l'annulation).
    committed: Option<Graph>,
    /// Taille de chaque nœud à l'image précédente.
    sizes: HashMap<u64, Vec2>,
    editing_comment: Option<u64>,
    show_help: bool,
    /// Le pointeur est sur l'éditeur : les raccourcis des platines sont coupés.
    hovered: bool,
}

impl Default for NodeEditor {
    fn default() -> Self {
        Self {
            drawn_frame: None,
            last_outer: None,
            transition: None,
            to_global: TSTransform::IDENTITY,
            frame_pending: Some(false),
            selection: HashSet::new(),
            selected_comments: HashSet::new(),
            drag: Drag::None,
            search: None,
            clipboard: None,
            undo: Vec::new(),
            redo: Vec::new(),
            committed: None,
            sizes: HashMap::new(),
            editing_comment: None,
            show_help: false,
            hovered: false,
        }
    }
}

impl NodeEditor {
    /// Vrai si l'éditeur, affiché à l'image précédente, prend le clavier
    /// (pointeur dessus, recherche ouverte).
    pub fn captures_keyboard(&self, ctx: &Context) -> bool {
        let shown = self
            .drawn_frame
            .is_some_and(|frame| frame + 1 >= ctx.cumulative_frame_nr());
        shown && (self.hovered || self.search.is_some())
    }

    /// Dessine l'éditeur pour `transition` dans `ui` ; met à jour son graphe
    /// et ses étapes.
    pub fn ui(&mut self, ui: &mut Ui, transition: &mut Transition, view: &RunView) -> NodeOutcome {
        let ctx = ui.ctx().clone();
        let mut outcome = NodeOutcome::default();
        let shown_before = self
            .drawn_frame
            .is_some_and(|frame| frame + 1 >= ctx.cumulative_frame_nr());
        if !shown_before {
            // Réaffiché après une absence : pas de recherche ni de glisser en suspens.
            self.search = None;
            self.drag = Drag::None;
        }
        self.drawn_frame = Some(ctx.cumulative_frame_nr());
        if self.transition != Some(transition.id) {
            *self = NodeEditor {
                drawn_frame: self.drawn_frame,
                transition: Some(transition.id),
                show_help: self.show_help,
                clipboard: self.clipboard.take(),
                ..NodeEditor::default()
            };
        }

        // Le graphe suit les étapes : s'il n'existe pas encore, ou si les
        // étapes ont été modifiées ailleurs (vue liste, enregistrement), il
        // est reconstruit.
        let stale = match &transition.graph {
            None => true,
            Some(graph) => graph.compile().steps != transition.steps,
        };
        if stale {
            let graph = Graph::from_steps(transition, &view.from_markers);
            let compiled = graph.compile();
            if compiled.steps != transition.steps {
                transition.steps = compiled.steps;
                outcome.steps_changed = true;
            }
            transition.graph = Some(graph);
            outcome.changed = true;
            self.frame_pending = Some(false);
        }
        let mut graph = transition.graph.take().unwrap_or_default();
        graph.normalize();
        let before = graph.clone();

        let area = ui.max_rect();
        self.toolbar(ui, transition, &mut graph, view, &mut outcome);
        self.canvas(ui, &mut graph, view, &mut outcome);
        self.hovered = ctx.pointer_hover_pos().is_some_and(|pos| area.contains(pos));

        if graph != before {
            let compiled = graph.compile();
            if compiled.steps != transition.steps {
                transition.steps = compiled.steps;
                outcome.steps_changed = true;
            }
            outcome.changed = true;
        }
        // Annulation : un état est validé quand plus aucun bouton n'est enfoncé.
        let pointer_down = ctx.input(|input| input.pointer.any_down());
        match &self.committed {
            None => self.committed = Some(graph.clone()),
            Some(committed) if !pointer_down && *committed != graph => {
                let previous = self.committed.replace(graph.clone()).expect("état validé");
                self.undo.push(previous);
                if self.undo.len() > UNDO_LIMIT {
                    self.undo.remove(0);
                }
                self.redo.clear();
            }
            _ => {}
        }
        transition.graph = Some(graph);
        outcome
    }

    fn toolbar(
        &mut self,
        ui: &mut Ui,
        transition: &Transition,
        graph: &mut Graph,
        view: &RunView,
        outcome: &mut NodeOutcome,
    ) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            let hint = if view.active {
                "Active — clic : désactiver"
            } else {
                "Activer : les événements « marqueur » se déclenchent pendant la lecture du départ                  (les deux morceaux doivent être chargés)"
            };
            if icon_button(ui, Icon::Power, theme::GREEN, view.active, view.can_activate, hint).clicked() {
                outcome.toggle_active = true;
            }
            match view.recording {
                Some(marker) => {
                    let hint = format!(
                        "Enregistrement : {} — les réglages modifiés sur les platines deviennent des                          étapes. Clic : arrêter",
                        trigger_long_name(marker)
                    );
                    if icon_button(ui, Icon::Record, theme::RED, true, true, &hint).clicked() {
                        outcome.record = Some(marker);
                    }
                    ui.label(
                        RichText::new(trigger_short_name(marker))
                            .monospace()
                            .color(marker_color(marker)),
                    );
                }
                None => {
                    let response = icon_button(
                        ui,
                        Icon::Record,
                        theme::RED,
                        false,
                        view.can_record,
                        "Enregistrer des étapes depuis les platines (volume, EQ, kills, stems, lecture,                          crossfader, SYNC, MATCH) sur un déclencheur",
                    );
                    egui::Popup::menu(&response).show(|ui| {
                        let triggers = [MARKER_START]
                            .into_iter()
                            .chain(0..NUM_MARKERS)
                            .chain([MARKER_END])
                            .chain(MANUAL_FIRST..ALL_TRIGGERS);
                        for trigger in triggers {
                            let text = RichText::new(trigger_short_name(trigger)).monospace().color(marker_color(trigger));
                            if ui.button(text).on_hover_text(capitalize(&trigger_long_name(trigger))).clicked() {
                                outcome.record = Some(trigger);
                            }
                        }
                    });
                }
            }
            ui.add_space(4.0);
            ui.separator();
            ui.add_space(4.0);
            if icon_button(ui, Icon::Fit, theme::TEXT_DIM, false, true, "Tout cadrer (Origine ; F : la sélection)")
                .clicked()
            {
                self.frame_pending = Some(false);
            }
            if icon_button(
                ui,
                Icon::Arrange,
                theme::TEXT_DIM,
                false,
                true,
                "Réorganiser : un événement par ligne (annulable)",
            )
            .clicked()
            {
                let mut source = transition.clone();
                source.graph = None;
                *graph = Graph::from_steps(&source, &view.from_markers);
                self.selection.clear();
                self.frame_pending = Some(false);
            }
            if icon_button(ui, Icon::Undo, theme::TEXT_DIM, false, !self.undo.is_empty(), "Annuler (Ctrl + Z)").clicked() {
                self.undo(graph);
            }
            if icon_button(ui, Icon::Redo, theme::TEXT_DIM, false, !self.redo.is_empty(), "Rétablir (Ctrl + Y)").clicked() {
                self.redo(graph);
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if icon_button(ui, Icon::Help, theme::TEXT_DIM, self.show_help, true, "Commandes").clicked() {
                    self.show_help = !self.show_help;
                }
                ui.label(
                    RichText::new(format!("{:.0} %", self.to_global.scaling * 100.0))
                        .size(11.0)
                        .color(theme::TEXT_DIM)
                        .monospace(),
                );
            });
        });
        if self.show_help {
            Frame::new()
                .fill(theme::PANEL_LIGHT)
                .corner_radius(4.0)
                .inner_margin(8.0)
                .show(ui, |ui| {
                    egui::Grid::new("node_editor_help")
                        .num_columns(2)
                        .spacing([16.0, 2.0])
                        .show(ui, |ui| {
                            for (keys, what) in HELP {
                                ui.label(RichText::new(*keys).monospace().color(theme::YELLOW));
                                ui.label(*what);
                                ui.end_row();
                            }
                        });
                });
        }
    }

    // -----------------------------------------------------------------------
    // Canevas
    // -----------------------------------------------------------------------

    fn canvas(&mut self, ui: &mut Ui, graph: &mut Graph, view: &RunView, outcome: &mut NodeOutcome) {
        let ctx = ui.ctx().clone();
        let size = ui.available_size_before_wrap().max(vec2(200.0, 200.0));
        let (outer, _) = ui.allocate_exact_size(size, Sense::hover());
        let layer = LayerId::new(ui.layer_id().order, ui.scope_id().with("node_canvas"));
        ctx.set_sublayer(ui.layer_id(), layer);

        // La vue suit le canevas quand le panneau bouge (fenêtre déplacée) ;
        // un changement de taille brutal (panneau détaché ou rancré) recadre.
        if let Some(last) = self.last_outer.replace(outer) {
            self.to_global.translation += outer.min - last.min;
            let ratio = |a: f32, b: f32| (a / b.max(1.0) - 1.0).abs();
            if ratio(outer.width(), last.width()) > 0.25 || ratio(outer.height(), last.height()) > 0.25 {
                self.frame_pending.get_or_insert(false);
            }
        }
        if let Some(selection_only) = self.frame_pending.take() {
            self.frame(graph, outer, selection_only);
        }

        // Survol du canevas (pas d'une fenêtre ou d'un menu par-dessus).
        let pointer_global = ctx.input(|input| input.pointer.latest_pos());
        let over_canvas = pointer_global.is_some_and(|pos| {
            outer.contains(pos)
                && ctx
                    .layer_id_at(pos)
                    .is_some_and(|top| top == layer || top == ui.layer_id())
        });

        // Vue : clic droit glissé ou bouton du milieu pour déplacer, molette pour zoomer.
        let (secondary, middle, decided, origin, delta) = ctx.input(|input| {
            (
                input.pointer.secondary_down(),
                input.pointer.middle_down(),
                input.pointer.is_decidedly_dragging(),
                input.pointer.press_origin(),
                input.pointer.delta(),
            )
        });
        if matches!(self.drag, Drag::None)
            && (secondary || middle)
            && decided
            && origin.is_some_and(|origin| outer.contains(origin))
            && self.search.is_none()
        {
            self.drag = Drag::Pan;
        }
        if matches!(self.drag, Drag::Pan) {
            if secondary || middle {
                self.to_global.translation += delta;
            } else {
                self.drag = Drag::None;
            }
        }
        if over_canvas
            && self.search.is_none()
            && let Some(pointer) = pointer_global
        {
            let (scroll, pinch) = ctx.input(|input| (input.smooth_scroll_delta, input.zoom_delta()));
            let factor = pinch * ((scroll.x + scroll.y) * 0.0025).exp();
            if factor != 1.0 {
                let scale = (self.to_global.scaling * factor).clamp(ZOOM_MIN, ZOOM_MAX);
                let factor = scale / self.to_global.scaling;
                self.to_global = TSTransform::from_translation(pointer.to_vec2())
                    * TSTransform::from_scaling(factor)
                    * TSTransform::from_translation(-pointer.to_vec2())
                    * self.to_global;
            }
        }

        if over_canvas {
            // La molette sert au zoom : les faders et potentiomètres des nœuds
            // ne la reçoivent pas (on les règle en les glissant).
            ctx.input_mut(|input| input.smooth_scroll_delta = Vec2::ZERO);
        }
        let scale = self.to_global.scaling;
        let visible = self.to_global.inverse() * outer;
        let mut canvas = ui.new_child(UiBuilder::new().layer_id(layer).max_rect(visible));
        canvas.set_clip_rect(self.to_global.inverse() * outer.intersect(ui.clip_rect()));
        ctx.set_transform_layer(layer, self.to_global);
        let pointer = pointer_global.map(|pos| self.to_global.inverse() * pos);
        let delta_scene = delta / scale;
        let modifiers = ctx.input(|input| input.modifiers);
        let primary_down = ctx.input(|input| input.pointer.primary_down());

        // Fond : grille, sélection rectangulaire, clic droit.
        let background = canvas.interact(visible, Id::unique("node_canvas_background"), Sense::click_and_drag());
        self.paint_grid(&canvas, visible, scale);
        self.paint_overlay(&canvas, visible, scale, graph);
        let compiled = graph.compile();

        // Commentaires (derrière les nœuds).
        self.comments(&mut canvas, graph, pointer, delta_scene, primary_down);

        // Liens.
        for link in &graph.links {
            let (Some(from), Some(to)) = (graph.node(link.from), graph.node(link.to)) else {
                continue;
            };
            let start = pin_pos(
                from,
                PinRef::Out {
                    node: from.id,
                    pin: link.pin,
                },
            );
            let end = pin_pos(to, PinRef::In { node: to.id });
            let active = self.activity(&compiled, view, from).flash > 0.0;
            wire(canvas.painter(), start, end, active);
        }

        // Nœuds.
        let mut pins: Vec<(PinRef, Pos2)> = Vec::new();
        let mut node_rects: Vec<(u64, Rect)> = Vec::new();
        let mut requests: Vec<NodeRequest> = Vec::new();
        for index in 0..graph.nodes.len() {
            let connected_in = graph.links.iter().any(|link| link.to == graph.nodes[index].id);
            let connected_out: Vec<bool> = (0..graph.nodes[index].kind.outputs())
                .map(|pin| graph.link_from(graph.nodes[index].id, pin).is_some())
                .collect();
            let activity = self.activity(&compiled, view, &graph.nodes[index]);
            let status = node_status(&compiled, view, graph.nodes[index].id);
            let context = NodeContext {
                selected: self.selection.contains(&graph.nodes[index].id),
                reached: compiled.reached(graph.nodes[index].id),
                connected_in,
                connected_out,
                activity,
                status,
                link_drag: match self.drag {
                    Drag::Link { from } => Some(from),
                    _ => None,
                },
                pointer,
            };
            let node = &mut graph.nodes[index];
            let size = self.sizes.get(&node.id).copied();
            let (rect, request) = draw_node(&mut canvas, node, size, &context, view, &mut pins);
            self.sizes.insert(node.id, rect.size());
            node_rects.push((node.id, rect));
            requests.extend(request);
        }

        // Lien en cours de création.
        if let Drag::Link { from } = self.drag
            && let Some(pointer) = pointer
            && let Some(node) = graph.node(from.node())
        {
            let start = pin_pos(node, from);
            let target = pins
                .iter()
                .filter(|(pin, _)| compatible(from, *pin))
                .map(|(_, pos)| *pos)
                .find(|pos| pos.distance(pointer) < 14.0)
                .unwrap_or(pointer);
            match from {
                PinRef::Out { .. } => wire(canvas.painter(), start, target, false),
                PinRef::In { .. } => wire(canvas.painter(), target, start, false),
            }
        }

        // Sélection rectangulaire.
        if let Drag::Marquee { start } = self.drag
            && let Some(pointer) = pointer
        {
            let rect = Rect::from_two_pos(start, pointer);
            canvas
                .painter()
                .rect_filled(rect, 0.0, Color32::from_rgba_unmultiplied(0xf1, 0xb0, 0x00, 18));
            canvas
                .painter()
                .rect_stroke(rect, 0.0, Stroke::new(1.0 / scale, SELECTED), StrokeKind::Inside);
        }

        // --- Demandes venant des nœuds ---
        for request in requests {
            match request {
                NodeRequest::Select(id) => self.select(id, modifiers),
                NodeRequest::StartMove(id) => {
                    if !self.selection.contains(&id) {
                        if !(modifiers.shift || modifiers.command) {
                            self.selection.clear();
                            self.selected_comments.clear();
                        }
                        self.selection.insert(id);
                    }
                    self.drag = Drag::Nodes;
                }
                NodeRequest::StartLink(pin) => self.drag = Drag::Link { from: pin },
                NodeRequest::MoveLink(pin) => {
                    if let PinRef::Out { node, pin: index } = pin {
                        graph.links.retain(|link| !(link.from == node && link.pin == index));
                    }
                    self.drag = Drag::Link { from: pin };
                }
                NodeRequest::BreakLinks(pin) => break_links(graph, pin),
                NodeRequest::Delete(id) => {
                    let ids = if self.selection.contains(&id) {
                        self.selection.iter().copied().collect()
                    } else {
                        vec![id]
                    };
                    graph.remove_nodes(&ids);
                    self.selection.retain(|node| !ids.contains(node));
                }
                NodeRequest::Duplicate(id) => {
                    if !self.selection.contains(&id) {
                        self.selection = HashSet::from([id]);
                    }
                    self.duplicate(graph);
                }
                NodeRequest::Fire(trigger) => outcome.fire.push(trigger),
                NodeRequest::Pruned => graph.prune_links(),
            }
        }

        // --- Fond ---
        if background.drag_started_by(PointerButton::Primary)
            && let Some(start) = pointer
        {
            self.drag = Drag::Marquee { start };
        }
        if background.clicked_by(PointerButton::Primary) {
            if !(modifiers.shift || modifiers.command) {
                self.selection.clear();
                self.selected_comments.clear();
            }
            self.editing_comment = None;
        }
        if background.secondary_clicked()
            && let (Some(screen), Some(scene)) = (pointer_global, pointer)
        {
            self.open_search(screen, scene, None, ctx.input(|input| input.time));
        }

        // --- Fin des glisser ---
        match &self.drag {
            Drag::Nodes if primary_down => {
                for node in graph.nodes.iter_mut().filter(|node| self.selection.contains(&node.id)) {
                    node.pos[0] += delta_scene.x;
                    node.pos[1] += delta_scene.y;
                }
                for comment in graph
                    .comments
                    .iter_mut()
                    .filter(|comment| self.selected_comments.contains(&comment.id))
                {
                    comment.rect[0] += delta_scene.x;
                    comment.rect[1] += delta_scene.y;
                }
            }
            Drag::Nodes => {
                // Lâchés : calés sur la grille.
                for node in graph.nodes.iter_mut().filter(|node| self.selection.contains(&node.id)) {
                    node.pos = [snap(node.pos[0]), snap(node.pos[1])];
                }
                self.drag = Drag::None;
            }
            Drag::Marquee { start } if !primary_down => {
                if let Some(end) = pointer {
                    let rect = Rect::from_two_pos(*start, end);
                    if !(modifiers.shift || modifiers.command) {
                        self.selection.clear();
                        self.selected_comments.clear();
                    }
                    for (id, node_rect) in &node_rects {
                        if rect.intersects(*node_rect) {
                            self.selection.insert(*id);
                        }
                    }
                }
                self.drag = Drag::None;
            }
            Drag::Link { from } if !primary_down => {
                let from = *from;
                self.drag = Drag::None;
                if let Some(pointer) = pointer {
                    let target = pins
                        .iter()
                        .filter(|(pin, _)| compatible(from, *pin))
                        .min_by(|a, b| a.1.distance(pointer).total_cmp(&b.1.distance(pointer)))
                        .filter(|(_, pos)| pos.distance(pointer) < 14.0)
                        .map(|(pin, _)| *pin);
                    match (from, target) {
                        (PinRef::Out { node, pin }, Some(PinRef::In { node: to })) => {
                            graph.connect(node, pin, to);
                        }
                        (PinRef::In { node: to }, Some(PinRef::Out { node, pin })) => {
                            graph.connect(node, pin, to);
                        }
                        _ => {
                            let over_node = node_rects.iter().any(|(_, rect)| rect.contains(pointer));
                            if !over_node && let Some(screen) = pointer_global {
                                self.open_search(screen, pointer, Some(from), ctx.input(|input| input.time));
                            }
                        }
                    }
                }
            }
            _ => {}
        }

        // --- Clavier ---
        if self.hovered && !ctx.text_edit_focused() && self.search.is_none() {
            self.keyboard(&ctx, graph, pointer, outcome);
        }

        // --- Recherche ---
        self.search_popup(&ctx, graph);
    }

    /// Grille façon Unreal : lignes fines tous les 16, marquées tous les 128.
    fn paint_grid(&self, canvas: &Ui, visible: Rect, scale: f32) {
        let painter = canvas.painter();
        painter.rect_filled(visible, 0.0, BACKGROUND);
        let thin = 1.0 / scale;
        let lines = |step: f32, color: Color32, width: f32| {
            let mut x = (visible.left() / step).floor() * step;
            while x <= visible.right() {
                painter.vline(x, visible.y_range(), Stroke::new(width, color));
                x += step;
            }
            let mut y = (visible.top() / step).floor() * step;
            while y <= visible.bottom() {
                painter.hline(visible.x_range(), y, Stroke::new(width, color));
                y += step;
            }
        };
        if GRID * scale >= 7.0 {
            lines(GRID, GRID_MINOR, thin);
        }
        lines(GRID * 8.0, GRID_MAJOR, 2.0 * thin);
    }

    /// Graphe vide : une indication discrète au centre (le reste de l'aide
    /// est derrière le bouton « ? »).
    fn paint_overlay(&self, canvas: &Ui, visible: Rect, scale: f32, graph: &Graph) {
        if graph.nodes.is_empty() {
            canvas.painter().text(
                visible.center(),
                Align2::CENTER_CENTER,
                "Clic droit : ajouter un nœud",
                FontId::proportional(13.0 / scale),
                Color32::from_white_alpha(70),
            );
        }
    }

    /// Commentaires : cadre, titre (glisser pour déplacer avec les nœuds
    /// contenus, double-clic pour renommer) et poignée de redimensionnement.
    fn comments(&mut self, canvas: &mut Ui, graph: &mut Graph, pointer: Option<Pos2>, delta: Vec2, primary_down: bool) {
        let scale = self.to_global.scaling;
        let modifiers = canvas.ctx().input(|input| input.modifiers);
        let mut start_drag = None;
        for index in 0..graph.comments.len() {
            let comment = &graph.comments[index];
            let id = comment.id;
            let rect = Rect::from_min_size(
                pos2(comment.rect[0], comment.rect[1]),
                vec2(comment.rect[2], comment.rect[3]),
            );
            let title = Rect::from_min_size(rect.min, vec2(rect.width(), 30.0));
            let handle = Rect::from_min_size(rect.max - vec2(16.0, 16.0), vec2(16.0, 16.0));
            let selected = self.selected_comments.contains(&id);

            let painter = canvas.painter();
            painter.rect_filled(rect, 6.0, Color32::from_white_alpha(10));
            painter.rect_filled(
                title,
                CornerRadius {
                    nw: 6,
                    ne: 6,
                    sw: 0,
                    se: 0,
                },
                Color32::from_white_alpha(24),
            );
            painter.rect_stroke(
                rect,
                6.0,
                Stroke::new(
                    if selected { 2.0 / scale } else { 1.0 / scale },
                    if selected {
                        SELECTED
                    } else {
                        Color32::from_white_alpha(40)
                    },
                ),
                StrokeKind::Inside,
            );
            for i in 0..3 {
                let offset = 4.0 + i as f32 * 4.0;
                painter.line_segment(
                    [rect.max - vec2(offset, 3.0), rect.max - vec2(3.0, offset)],
                    Stroke::new(1.0 / scale, Color32::from_white_alpha(60)),
                );
            }

            let title_response = canvas.interact(title, Id::unique(("comment_title", id)), Sense::click_and_drag());
            let handle_response = canvas.interact(handle, Id::unique(("comment_resize", id)), Sense::drag());
            if self.editing_comment == Some(id) {
                let mut text = graph.comments[index].text.clone();
                let mut child = canvas.new_child(UiBuilder::new().max_rect(title.shrink2(vec2(8.0, 4.0))));
                let edit = child.add(TextEdit::singleline(&mut text).desired_width(title.width() - 16.0));
                if !edit.has_focus() && !edit.lost_focus() {
                    edit.request_focus();
                }
                if edit.lost_focus() {
                    self.editing_comment = None;
                }
                graph.comments[index].text = text;
            } else {
                canvas.painter().text(
                    title.left_center() + vec2(10.0, 0.0),
                    Align2::LEFT_CENTER,
                    &graph.comments[index].text,
                    FontId::proportional(16.0),
                    theme::TEXT,
                );
            }
            if title_response.double_clicked() {
                self.editing_comment = Some(id);
            } else if title_response.clicked() {
                if !(modifiers.shift || modifiers.command) {
                    self.selection.clear();
                    self.selected_comments.clear();
                }
                self.selected_comments.insert(id);
            }
            title_response.context_menu(|ui| {
                if ui.button("Renommer").clicked() {
                    self.editing_comment = Some(id);
                    ui.close();
                }
                if ui.button("Supprimer le commentaire").clicked() {
                    self.selected_comments = HashSet::from([id]);
                    ui.close();
                }
            });
            if title_response.drag_started_by(PointerButton::Primary) {
                start_drag = Some((id, false, rect));
            }
            if handle_response.drag_started_by(PointerButton::Primary) {
                start_drag = Some((id, true, rect));
            }
        }
        if let Some((id, resize, rect)) = start_drag {
            // Nœuds entièrement dans le cadre : ils suivent le commentaire.
            let members = graph
                .nodes
                .iter()
                .filter(|node| {
                    let size = self
                        .sizes
                        .get(&node.id)
                        .copied()
                        .unwrap_or(vec2(node_width(&node.kind), 80.0));
                    rect.contains_rect(Rect::from_min_size(pos2(node.pos[0], node.pos[1]), size))
                })
                .map(|node| node.id)
                .collect();
            self.drag = Drag::Comment { id, resize, members };
        }
        if let Drag::Comment { id, resize, members } = &self.drag {
            if primary_down && pointer.is_some() {
                if let Some(comment) = graph.comments.iter_mut().find(|comment| comment.id == *id) {
                    if *resize {
                        comment.rect[2] = (comment.rect[2] + delta.x).max(160.0);
                        comment.rect[3] = (comment.rect[3] + delta.y).max(80.0);
                    } else {
                        comment.rect[0] += delta.x;
                        comment.rect[1] += delta.y;
                        for node in graph.nodes.iter_mut().filter(|node| members.contains(&node.id)) {
                            node.pos[0] += delta.x;
                            node.pos[1] += delta.y;
                        }
                    }
                }
            } else {
                let (id, members) = (*id, members.clone());
                if let Some(comment) = graph.comments.iter_mut().find(|comment| comment.id == id) {
                    for value in &mut comment.rect {
                        *value = snap(*value);
                    }
                }
                for node in graph.nodes.iter_mut().filter(|node| members.contains(&node.id)) {
                    node.pos = [snap(node.pos[0]), snap(node.pos[1])];
                }
                self.drag = Drag::None;
            }
        }
        // « Supprimer le commentaire » du menu contextuel.
        if !self.selected_comments.is_empty()
            && canvas
                .ctx()
                .input(|input| input.key_pressed(Key::Delete) || input.key_pressed(Key::Backspace))
            && self.hovered
            && !canvas.ctx().text_edit_focused()
        {
            graph
                .comments
                .retain(|comment| !self.selected_comments.contains(&comment.id));
            self.selected_comments.clear();
        }
    }

    /// Activité d'un nœud pendant l'exécution (surbrillance, progression d'un délai).
    fn activity(&self, compiled: &Compiled, view: &RunView, node: &Node) -> Activity {
        let mut activity = Activity::default();
        let Some(timing) = compiled.timing.get(&node.id) else {
            return activity;
        };
        for &(trigger, start, end) in timing {
            let Some(fired) = view.fired_at.get(trigger).copied().flatten() else {
                continue;
            };
            let since_end = view.now - (fired + end);
            if (0.0..FLASH_SECS).contains(&since_end) {
                let done = match node.kind {
                    NodeKind::Action { .. } => {
                        matches!(node_status(compiled, view, node.id), Some(StepStatus::Done))
                    }
                    _ => true,
                };
                if done {
                    activity.flash = activity.flash.max(1.0 - (since_end / FLASH_SECS) as f32);
                }
            }
            if end > start {
                let elapsed = view.now - (fired + start);
                if (0.0..end - start).contains(&elapsed) {
                    activity.progress = Some((elapsed / (end - start)) as f32);
                }
            }
        }
        activity
    }

    fn select(&mut self, id: u64, modifiers: Modifiers) {
        if modifiers.command {
            if !self.selection.remove(&id) {
                self.selection.insert(id);
            }
        } else if modifiers.shift {
            self.selection.insert(id);
        } else {
            self.selection = HashSet::from([id]);
            self.selected_comments.clear();
        }
    }

    fn open_search(&mut self, screen: Pos2, scene: Pos2, from: Option<PinRef>, time: f64) {
        self.search = Some(Search {
            screen,
            scene,
            query: String::new(),
            selected: 0,
            from,
            focus: true,
            opened_at: time,
            rank: false,
        });
    }

    /// Cadre tous les nœuds (ou la sélection) dans la vue.
    fn frame(&mut self, graph: &Graph, outer: Rect, selection_only: bool) {
        let bounds = graph
            .nodes
            .iter()
            .filter(|node| !selection_only || self.selection.contains(&node.id))
            .map(|node| {
                let size = self
                    .sizes
                    .get(&node.id)
                    .copied()
                    .unwrap_or(vec2(node_width(&node.kind), 120.0));
                Rect::from_min_size(pos2(node.pos[0], node.pos[1]), size)
            })
            .chain(graph.comments.iter().filter(|_| !selection_only).map(|comment| {
                Rect::from_min_size(
                    pos2(comment.rect[0], comment.rect[1]),
                    vec2(comment.rect[2], comment.rect[3]),
                )
            }))
            .reduce(|a, b| a.union(b));
        let Some(bounds) = bounds else {
            if !selection_only {
                self.to_global = TSTransform::from_translation(outer.min.to_vec2() + vec2(80.0, 80.0));
            }
            return;
        };
        let bounds = bounds.expand(60.0);
        let scale = (outer.width() / bounds.width())
            .min(outer.height() / bounds.height())
            .clamp(ZOOM_MIN, 1.0);
        self.to_global = TSTransform::from_translation(outer.center().to_vec2() - scale * bounds.center().to_vec2())
            * TSTransform::from_scaling(scale);
    }

    // -----------------------------------------------------------------------
    // Clavier, presse-papiers, annulation
    // -----------------------------------------------------------------------

    fn keyboard(&mut self, ctx: &Context, graph: &mut Graph, pointer: Option<Pos2>, outcome: &mut NodeOutcome) {
        let events = ctx.input(|input| input.events.clone());
        for event in events {
            match event {
                Event::Copy => self.copy(graph),
                Event::Cut => {
                    self.copy(graph);
                    self.delete_selection(graph);
                }
                Event::Paste(_) => self.paste(graph, pointer),
                Event::Key {
                    key,
                    pressed: true,
                    repeat,
                    modifiers,
                    ..
                } => match key {
                    Key::Delete | Key::Backspace => self.delete_selection(graph),
                    Key::Z if modifiers.command && modifiers.shift => self.redo(graph),
                    Key::Z if modifiers.command => self.undo(graph),
                    Key::Y if modifiers.command => self.redo(graph),
                    Key::A if modifiers.command => {
                        self.selection = graph.nodes.iter().map(|node| node.id).collect();
                    }
                    Key::D | Key::W if modifiers.command && !repeat => self.duplicate(graph),
                    Key::Home => self.frame_pending = Some(false),
                    Key::F if !modifiers.command => {
                        self.frame_pending = Some(!self.selection.is_empty());
                    }
                    Key::C if !modifiers.command && !repeat => self.comment_selection(graph, pointer),
                    Key::Escape => {
                        self.selection.clear();
                        self.selected_comments.clear();
                        self.drag = Drag::None;
                    }
                    Key::F7 | Key::F8 | Key::F9 | Key::F10 if !repeat => {
                        let n = match key {
                            Key::F7 => 0,
                            Key::F8 => 1,
                            Key::F9 => 2,
                            _ => 3,
                        };
                        outcome.fire.push(MANUAL_FIRST + n);
                    }
                    _ => {}
                },
                _ => {}
            }
        }
    }

    fn delete_selection(&mut self, graph: &mut Graph) {
        let ids: Vec<u64> = self.selection.drain().collect();
        graph.remove_nodes(&ids);
        graph
            .comments
            .retain(|comment| !self.selected_comments.contains(&comment.id));
        self.selected_comments.clear();
    }

    fn copy(&mut self, graph: &Graph) {
        if self.selection.is_empty() {
            return;
        }
        let nodes: Vec<Node> = graph
            .nodes
            .iter()
            .filter(|node| self.selection.contains(&node.id))
            .cloned()
            .collect();
        let links = graph
            .links
            .iter()
            .filter(|link| self.selection.contains(&link.from) && self.selection.contains(&link.to))
            .copied()
            .collect();
        self.clipboard = Some(Clipboard { nodes, links });
    }

    /// Colle le presse-papiers, son coin haut gauche au pointeur.
    fn paste(&mut self, graph: &mut Graph, at: Option<Pos2>) {
        let Some(clipboard) = &self.clipboard else { return };
        let origin = clipboard
            .nodes
            .iter()
            .map(|node| pos2(node.pos[0], node.pos[1]))
            .reduce(|a, b| pos2(a.x.min(b.x), a.y.min(b.y)))
            .unwrap_or(Pos2::ZERO);
        let target = at.unwrap_or(origin + vec2(32.0, 32.0));
        let offset = vec2(snap(target.x - origin.x), snap(target.y - origin.y));
        let mut ids = HashMap::new();
        let mut pasted = HashSet::new();
        for node in &clipboard.nodes {
            let id = graph.add_node(node.kind, [node.pos[0] + offset.x, node.pos[1] + offset.y]);
            ids.insert(node.id, id);
            pasted.insert(id);
        }
        for link in &clipboard.links {
            if let (Some(&from), Some(&to)) = (ids.get(&link.from), ids.get(&link.to)) {
                graph.connect(from, link.pin, to);
            }
        }
        self.selection = pasted;
        self.selected_comments.clear();
    }

    fn duplicate(&mut self, graph: &mut Graph) {
        let saved = self.clipboard.take();
        self.copy(graph);
        let at = graph
            .nodes
            .iter()
            .filter(|node| self.selection.contains(&node.id))
            .map(|node| pos2(node.pos[0], node.pos[1]))
            .reduce(|a, b| pos2(a.x.min(b.x), a.y.min(b.y)))
            .map(|pos| pos + vec2(32.0, 32.0));
        self.paste(graph, at);
        self.clipboard = saved;
    }

    /// Commentaire autour de la sélection (ou au pointeur).
    fn comment_selection(&mut self, graph: &mut Graph, pointer: Option<Pos2>) {
        let bounds = graph
            .nodes
            .iter()
            .filter(|node| self.selection.contains(&node.id))
            .map(|node| {
                let size = self
                    .sizes
                    .get(&node.id)
                    .copied()
                    .unwrap_or(vec2(node_width(&node.kind), 120.0));
                Rect::from_min_size(pos2(node.pos[0], node.pos[1]), size)
            })
            .reduce(|a, b| a.union(b));
        let rect = match bounds {
            Some(bounds) => Rect::from_min_max(bounds.min - vec2(24.0, 48.0), bounds.max + vec2(24.0, 24.0)),
            None => Rect::from_min_size(pointer.unwrap_or(Pos2::ZERO), vec2(400.0, 240.0)),
        };
        let id = graph.new_id();
        graph.comments.push(Comment {
            id,
            rect: [
                snap(rect.min.x),
                snap(rect.min.y),
                snap(rect.width()),
                snap(rect.height()),
            ],
            text: "Commentaire".into(),
        });
        self.editing_comment = Some(id);
    }

    fn undo(&mut self, graph: &mut Graph) {
        if let Some(previous) = self.undo.pop() {
            self.redo.push(std::mem::replace(graph, previous));
            self.after_history(graph);
        }
    }

    fn redo(&mut self, graph: &mut Graph) {
        if let Some(next) = self.redo.pop() {
            self.undo.push(std::mem::replace(graph, next));
            self.after_history(graph);
        }
    }

    fn after_history(&mut self, graph: &Graph) {
        self.committed = Some(graph.clone());
        let ids: HashSet<u64> = graph.nodes.iter().map(|node| node.id).collect();
        self.selection.retain(|id| ids.contains(id));
        self.drag = Drag::None;
    }

    // -----------------------------------------------------------------------
    // Recherche (clic droit)
    // -----------------------------------------------------------------------

    fn search_popup(&mut self, ctx: &Context, graph: &mut Graph) {
        let Some(search) = &mut self.search else { return };
        let entries: Vec<Entry> = entries()
            .into_iter()
            .filter(|entry| match (search.from, entry.item) {
                (Some(PinRef::Out { .. }), NewItem::Node(kind)) => kind.has_input(),
                (Some(_), NewItem::Comment) => false,
                _ => true,
            })
            .filter(|entry| matches(entry, &search.query))
            .collect();
        // Nouvelle saisie : l'entrée dont le nom commence par le texte tapé
        // passe devant celles trouvées par leurs mots-clés (« stems » → STEMS).
        if std::mem::take(&mut search.rank) {
            let query = normalize(search.query.trim());
            search.selected = entries
                .iter()
                .position(|entry| !query.is_empty() && normalize(&entry.name).starts_with(&query))
                .unwrap_or(0);
        }

        // Navigation au clavier (avant le champ, qui garderait les flèches).
        let (down, up, enter, escape) = ctx.input_mut(|input| {
            (
                input.consume_key(Modifiers::NONE, Key::ArrowDown),
                input.consume_key(Modifiers::NONE, Key::ArrowUp),
                input.consume_key(Modifiers::NONE, Key::Enter),
                input.consume_key(Modifiers::NONE, Key::Escape),
            )
        });
        if down {
            search.selected = (search.selected + 1).min(entries.len().saturating_sub(1));
        }
        if up {
            search.selected = search.selected.saturating_sub(1);
        }
        search.selected = search.selected.min(entries.len().saturating_sub(1));
        let mut chosen = enter.then_some(search.selected).filter(|_| !entries.is_empty());

        let title = match search.from {
            Some(_) => "Ajouter un nœud relié",
            None => "Toutes les actions pour cette transition",
        };
        let area = egui::Area::new(Id::unique("node_editor_search"))
            .order(Order::Foreground)
            .fixed_pos(search.screen)
            .constrain(true)
            .show(ctx, |ui| {
                Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_width(310.0);
                    ui.label(RichText::new(title).strong());
                    let edit = ui.add(
                        TextEdit::singleline(&mut search.query)
                            .hint_text("Rechercher…")
                            .desired_width(f32::INFINITY),
                    );
                    if search.focus {
                        edit.request_focus();
                        search.focus = false;
                    }
                    if edit.changed() {
                        search.selected = 0;
                        search.rank = true;
                    }
                    ui.add_space(2.0);
                    ScrollArea::vertical()
                        .id_salt(("node_search_list", search.opened_at.to_bits()))
                        .max_height(340.0)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            if entries.is_empty() {
                                ui.label(RichText::new("Aucun nœud ne correspond.").color(theme::TEXT_DIM));
                            }
                            let mut category = "";
                            for (index, entry) in entries.iter().enumerate() {
                                if entry.category != category {
                                    category = entry.category;
                                    ui.add_space(3.0);
                                    ui.label(RichText::new(category).small().strong().color(theme::TEXT_DIM));
                                }
                                let selected = index == search.selected;
                                let response = ui.selectable_label(selected, format!("   {}", entry.name));
                                if selected && (down || up) {
                                    response.scroll_to_me(None);
                                }
                                if response.clicked() {
                                    chosen = Some(index);
                                }
                                if response.hovered() {
                                    search.selected = index;
                                }
                            }
                        });
                });
            });

        let time = ctx.input(|input| input.time);
        let clicked_outside = ctx.input(|input| input.pointer.any_pressed())
            && ctx
                .pointer_interact_pos()
                .is_some_and(|pos| !area.response.rect.contains(pos))
            && time - search.opened_at > 0.1;
        if let Some(index) = chosen {
            let entry = &entries[index];
            let (scene, from) = (search.scene, search.from);
            self.search = None;
            self.create(graph, entry.item, scene, from);
        } else if escape || clicked_outside {
            self.search = None;
        }
    }

    /// Crée un nœud (ou un commentaire) à `scene`, relié à `from` si besoin.
    fn create(&mut self, graph: &mut Graph, item: NewItem, scene: Pos2, from: Option<PinRef>) {
        let kind = match item {
            NewItem::Comment => {
                self.comment_selection(graph, Some(scene));
                return;
            }
            NewItem::Node(kind) => kind,
        };
        let width = node_width(&kind);
        let pin_y = HEADER_H + ROW_H / 2.0;
        // La broche reliée tombe sous le pointeur.
        let pos = match from {
            Some(PinRef::Out { .. }) => scene - vec2(PIN_INSET, pin_y),
            Some(PinRef::In { .. }) => scene - vec2(width - PIN_INSET, pin_y),
            None => scene,
        };
        let id = graph.add_node(kind, [snap(pos.x), snap(pos.y)]);
        match from {
            Some(PinRef::Out { node, pin }) => {
                graph.connect(node, pin, id);
            }
            Some(PinRef::In { node }) => {
                graph.connect(id, 0, node);
            }
            None => {}
        }
        self.selection = HashSet::from([id]);
        self.selected_comments.clear();
    }
}

const HELP: &[(&str, &str)] = &[
    (
        "Clic droit",
        "rechercher et ajouter un nœud (ou relâcher un lien dans le vide)",
    ),
    (
        "Glisser une broche",
        "relier ; Alt + clic : rompre ; Ctrl + glisser une sortie : déplacer son lien",
    ),
    ("Clic droit glissé / milieu", "déplacer la vue ; molette : zoom"),
    ("Glisser sur le fond", "sélection rectangulaire (Maj / Ctrl : ajouter)"),
    ("Suppr", "supprimer la sélection"),
    ("Ctrl + C / X / V", "copier, couper, coller au pointeur"),
    ("Ctrl + D", "dupliquer"),
    ("Ctrl + Z / Ctrl + Y", "annuler, rétablir"),
    ("Ctrl + A", "tout sélectionner"),
    ("Origine / F", "tout cadrer / cadrer la sélection"),
    (
        "C",
        "commentaire autour de la sélection (double-clic sur son titre : renommer)",
    ),
    ("F7 à F10", "déclencheurs manuels 1 à 4"),
];

/// Activité d'un nœud pendant l'exécution.
#[derive(Clone, Copy, Default)]
struct Activity {
    /// Surbrillance de 1 (vient de s'exécuter) à 0.
    flash: f32,
    /// Progression d'un délai en cours, de 0 à 1.
    progress: Option<f32>,
}

/// Ce que l'utilisateur a demandé sur un nœud.
enum NodeRequest {
    Select(u64),
    StartMove(u64),
    StartLink(PinRef),
    MoveLink(PinRef),
    BreakLinks(PinRef),
    Delete(u64),
    Duplicate(u64),
    Fire(usize),
    /// Une séquence a perdu une sortie : liens à nettoyer.
    Pruned,
}

struct NodeContext {
    selected: bool,
    /// Atteint depuis un événement (sinon il ne s'exécutera jamais).
    reached: bool,
    connected_in: bool,
    connected_out: Vec<bool>,
    activity: Activity,
    status: Option<StepStatus>,
    link_drag: Option<PinRef>,
    pointer: Option<Pos2>,
}

/// État d'exécution d'un nœud action (sa première étape).
fn node_status(compiled: &Compiled, view: &RunView, id: u64) -> Option<StepStatus> {
    compiled
        .step_nodes
        .iter()
        .position(|node| *node == id)
        .and_then(|index| view.statuses.get(index).copied())
}

fn node_width(kind: &NodeKind) -> f32 {
    match kind {
        NodeKind::Event { .. } => 224.0,
        NodeKind::Action {
            action: Action::Crossfader { .. },
        } => 224.0,
        NodeKind::Action {
            action: Action::Jump { .. },
        } => 256.0,
        NodeKind::Action {
            action: Action::Loop { .. },
        } => 224.0,
        NodeKind::Group {
            group: Group::Stems, ..
        } => 240.0,
        NodeKind::Group { group: Group::Eq, .. } => 184.0,
        NodeKind::Quantize { .. } => 208.0,
        _ => 176.0,
    }
}

/// Rangées de broches d'exécution sous l'en-tête.
fn pin_rows(kind: &NodeKind) -> usize {
    kind.outputs().max(1)
}

/// Centre d'une broche.
fn pin_pos(node: &Node, pin: PinRef) -> Pos2 {
    let origin = pos2(node.pos[0], node.pos[1]);
    let y = origin.y + HEADER_H + ROW_H / 2.0;
    match pin {
        PinRef::In { .. } => pos2(origin.x + PIN_INSET, y),
        PinRef::Out { pin, .. } => pos2(origin.x + node_width(&node.kind) - PIN_INSET, y + pin as f32 * ROW_H),
    }
}

fn compatible(from: PinRef, to: PinRef) -> bool {
    match (from, to) {
        (PinRef::Out { node: a, .. }, PinRef::In { node: b })
        | (PinRef::In { node: b }, PinRef::Out { node: a, .. }) => a != b,
        _ => false,
    }
}

fn break_links(graph: &mut Graph, pin: PinRef) {
    match pin {
        PinRef::Out { node, pin } => graph.links.retain(|link| !(link.from == node && link.pin == pin)),
        PinRef::In { node } => graph.links.retain(|link| link.to != node),
    }
}

fn snap(value: f32) -> f32 {
    (value / GRID).round() * GRID
}

/// Couleur d'en-tête : événements en rouge, flux en gris, actions par famille.
fn header_color(kind: &NodeKind) -> Color32 {
    match kind {
        NodeKind::Event { .. } => Color32::from_rgb(0x9c, 0x22, 0x22),
        NodeKind::Delay { .. } | NodeKind::Quantize { .. } | NodeKind::Sequence { .. } => {
            Color32::from_rgb(0x55, 0x58, 0x60)
        }
        // Par famille : pistes (stems), EQ, volume et crossfader, lecture, tempo et tonalité.
        NodeKind::Control { control, .. } if control.stem().is_some() => Color32::from_rgb(0x1c, 0x7c, 0x8a),
        NodeKind::Control { control, .. } if control.eq_band().is_some() => Color32::from_rgb(0x24, 0x5a, 0xa8),
        NodeKind::Control { .. } => Color32::from_rgb(0x3b, 0x4f, 0x9a),
        NodeKind::Group {
            group: Group::Stems, ..
        } => Color32::from_rgb(0x1c, 0x7c, 0x8a),
        NodeKind::Group { group: Group::Eq, .. } => Color32::from_rgb(0x24, 0x5a, 0xa8),
        NodeKind::Action { action } => match action {
            Action::Crossfader { .. } => Color32::from_rgb(0x3b, 0x4f, 0x9a),
            Action::Play { .. } | Action::Stop { .. } | Action::Jump { .. } | Action::Loop { .. } => {
                Color32::from_rgb(0x1f, 0x7a, 0x5c)
            }
            Action::Filter { .. } => Color32::from_rgb(0x8a, 0x5a, 0x1c),
            _ => Color32::from_rgb(0x6c, 0x3f, 0x9e),
        },
    }
}

fn node_title(kind: &NodeKind) -> String {
    match kind {
        NodeKind::Event { trigger } => match *trigger {
            MARKER_START => "Début de la musique".into(),
            MARKER_END => "Fin de la musique".into(),
            t if is_manual(t) => format!("Manuel {} ({})", t - MANUAL_FIRST + 1, MANUAL_KEYS[t - MANUAL_FIRST]),
            t => format!("Marqueur {}", t + 1),
        },
        NodeKind::Delay { .. } => "Délai".into(),
        NodeKind::Quantize { .. } => "Quantifier".into(),
        NodeKind::Sequence { .. } => "Séquence".into(),
        // Un nœud par élément de la platine : « Voix », « EQ High »… ; la
        // lecture regroupe lancer et arrêter, le tempo SYNC, la tonalité MATCH.
        NodeKind::Control { control, .. } => control.label().into(),
        NodeKind::Group { group, .. } => group.label().into(),
        NodeKind::Action { action } => match action {
            Action::Play { .. } | Action::Stop { .. } => "Lecture".into(),
            Action::Sync { .. } | Action::Tempo { .. } => "Tempo".into(),
            Action::Match { .. } | Action::Key { .. } => "Tonalité".into(),
            _ => ACTION_KINDS[action.kind()].0.into(),
        },
    }
}

/// Fil d'exécution (courbe de Bézier, comme dans Unreal).
fn wire(painter: &egui::Painter, start: Pos2, end: Pos2, active: bool) {
    let bend = ((end.x - start.x).abs() * 0.5).clamp(60.0, 260.0);
    let points = [start, start + vec2(bend, 0.0), end - vec2(bend, 0.0), end];
    let width = if active { 4.5 } else { 2.6 };
    if active {
        painter.add(CubicBezierShape::from_points_stroke(
            points,
            false,
            Color32::TRANSPARENT,
            Stroke::new(10.0, WIRE_ACTIVE.gamma_multiply(0.25)),
        ));
    }
    painter.add(CubicBezierShape::from_points_stroke(
        points,
        false,
        Color32::TRANSPARENT,
        Stroke::new(width, if active { WIRE_ACTIVE } else { WIRE }),
    ));
}

/// Broche d'exécution : flèche pleine si reliée, contour sinon.
fn exec_pin(painter: &egui::Painter, center: Pos2, connected: bool, hovered: bool) {
    let s = if hovered { 7.5 } else { 6.0 };
    let points = vec![
        center + vec2(-s, -s),
        center + vec2(s * 0.15, -s),
        center + vec2(s, 0.0),
        center + vec2(s * 0.15, s),
        center + vec2(-s, s),
    ];
    let color = if hovered { Color32::WHITE } else { WIRE };
    if connected {
        painter.add(Shape::convex_polygon(points, color, Stroke::NONE));
    } else {
        painter.add(Shape::closed_line(points, Stroke::new(1.6, color)));
    }
}

/// Petite icône d'en-tête.
fn header_icon(painter: &egui::Painter, center: Pos2, kind: &NodeKind) {
    let white = Color32::from_white_alpha(230);
    match kind {
        NodeKind::Event { .. } => {
            painter.add(Shape::convex_polygon(
                vec![
                    center + vec2(0.0, -6.0),
                    center + vec2(6.0, 0.0),
                    center + vec2(0.0, 6.0),
                    center + vec2(-6.0, 0.0),
                ],
                white,
                Stroke::NONE,
            ));
        }
        NodeKind::Delay { .. } => {
            painter.circle_stroke(center, 6.0, Stroke::new(1.5, white));
            painter.line_segment([center, center + vec2(0.0, -4.0)], Stroke::new(1.5, white));
            painter.line_segment([center, center + vec2(3.0, 1.5)], Stroke::new(1.5, white));
        }
        NodeKind::Quantize { .. } => {
            // Grille : des temps, le premier marqué.
            for i in 0..4 {
                let x = center.x - 6.0 + i as f32 * 4.0;
                let top = if i == 0 { center.y - 6.0 } else { center.y - 3.0 };
                painter.line_segment([pos2(x, top), pos2(x, center.y + 6.0)], Stroke::new(1.5, white));
            }
        }
        NodeKind::Sequence { .. } => {
            for i in 0..3 {
                let y = center.y - 4.0 + i as f32 * 4.0;
                painter.line_segment(
                    [pos2(center.x - 6.0, y), pos2(center.x + 6.0, y)],
                    Stroke::new(1.5, white),
                );
            }
        }
        NodeKind::Action { .. } | NodeKind::Control { .. } | NodeKind::Group { .. } => {
            painter.text(center, Align2::CENTER_CENTER, "ƒ", FontId::proportional(15.0), white);
        }
    }
}

/// Dessine un nœud et son contenu éditable ; retourne son rectangle et les
/// demandes de l'utilisateur.
fn draw_node(
    canvas: &mut Ui,
    node: &mut Node,
    size: Option<Vec2>,
    context: &NodeContext,
    view: &RunView,
    pins: &mut Vec<(PinRef, Pos2)>,
) -> (Rect, Vec<NodeRequest>) {
    let mut requests = Vec::new();
    let id = node.id;
    let width = node_width(&node.kind);
    let origin = pos2(node.pos[0], node.pos[1]);
    let previous = Rect::from_min_size(origin, size.unwrap_or(vec2(width, HEADER_H + 2.0 * ROW_H)));

    // Fond du nœud (sélection, déplacement, menu) : sous les widgets du contenu.
    let frame = canvas.interact(previous, Id::unique(("node", id)), Sense::click_and_drag());
    let background = canvas.painter().add(Shape::Noop);

    // En-tête.
    let header = Rect::from_min_size(origin, vec2(width, HEADER_H));
    let title_color = if context.reached || matches!(node.kind, NodeKind::Event { .. }) {
        theme::TEXT
    } else {
        theme::TEXT_DIM
    };
    {
        let painter = canvas.painter();
        header_icon(painter, header.left_center() + vec2(14.0, 0.0), &node.kind);
        painter.text(
            header.left_center() + vec2(28.0, 0.0),
            Align2::LEFT_CENTER,
            node_title(&node.kind),
            FontId::proportional(14.0),
            title_color,
        );
        // État d'exécution.
        let badge = match context.status {
            Some(StepStatus::Done) => Some(("✔".to_string(), theme::GREEN)),
            Some(StepStatus::Skipped) => Some(("↷".to_string(), theme::YELLOW)),
            Some(StepStatus::Scheduled(remaining)) => Some((format!("{remaining:.1} s"), theme::YELLOW)),
            _ => None,
        };
        if let Some((text, color)) = badge {
            let inset = if node_side(&node.kind).is_some() { 48.0 } else { 10.0 };
            painter.text(
                header.right_center() - vec2(inset, 0.0),
                Align2::RIGHT_CENTER,
                text,
                FontId::monospace(12.0),
                color,
            );
        }
    }

    // Platine visée : un petit interrupteur sans texte, dans l'en-tête
    // (pastille à gauche : platine A, à droite : platine B, à sa couleur).
    if let Some(side) = node_side(&node.kind) {
        let deck_of = |side: Side| if side == Side::From { view.from } else { view.to };
        let other = if side == Side::From { Side::To } else { Side::From };
        let rect = Rect::from_center_size(pos2(header.right() - 24.0, header.center().y), vec2(30.0, 14.0));
        let response = canvas.interact(rect, Id::unique(("node_side_toggle", id)), Sense::click());
        let deck = deck_of(side);
        side_toggle(
            canvas.painter(),
            rect,
            deck == 0,
            theme::DECK_COLORS[deck],
            response.hovered(),
        );
        if response
            .on_hover_text(format!(
                "Platine visée : {} ({})\nClic : {} ({})",
                side.label(),
                theme::DECK_NAMES[deck_of(side)],
                other.label(),
                theme::DECK_NAMES[deck_of(other)]
            ))
            .clicked()
        {
            set_node_side(&mut node.kind, other);
        }
    }

    // Broches d'exécution.
    let rows = pin_rows(&node.kind);
    let mut pin_shapes = Vec::new();
    if node.kind.has_input() {
        pin_shapes.push((PinRef::In { node: id }, context.connected_in));
    }
    for pin in 0..node.kind.outputs() {
        pin_shapes.push((
            PinRef::Out { node: id, pin },
            context.connected_out.get(pin).copied().unwrap_or(false),
        ));
    }
    let mut pin_responses = Vec::new();
    for (pin, connected) in pin_shapes {
        let center = pin_pos(node, pin);
        let rect = Rect::from_center_size(center, vec2(20.0, 20.0));
        let response = canvas.interact(
            rect,
            Id::unique(("pin", id, format!("{pin:?}"))),
            Sense::click_and_drag(),
        );
        pins.push((pin, center));
        pin_responses.push((pin, center, connected, response));
    }
    if let NodeKind::Sequence { outputs } = node.kind {
        for pin in 0..outputs {
            let center = pin_pos(node, PinRef::Out { node: id, pin });
            canvas.painter().text(
                center - vec2(14.0, 0.0),
                Align2::RIGHT_CENTER,
                format!("Puis {pin}"),
                FontId::proportional(12.0),
                theme::TEXT_DIM,
            );
        }
    }

    // Contenu.
    let body_top = origin.y + HEADER_H + rows as f32 * ROW_H + 4.0;
    let body_rect = Rect::from_min_max(
        pos2(origin.x + 12.0, body_top),
        pos2(origin.x + width - 12.0, body_top + 600.0),
    );
    let mut body = canvas.new_child(
        UiBuilder::new()
            .max_rect(body_rect)
            .layout(Layout::top_down(Align::Min))
            .id_salt(("node_body", id)),
    );
    body.spacing_mut().item_spacing = vec2(6.0, 4.0);
    body.spacing_mut().interact_size.y = 18.0;
    match &mut node.kind {
        NodeKind::Event { trigger } => {
            // Une ligne : déclencheur, position (ou touche), bouton « lancer ».
            body.horizontal(|ui| {
                trigger_combo(ui, ("event_trigger", id), trigger, true);
                let (info, hint) = if is_manual(*trigger) {
                    let key = MANUAL_KEYS[*trigger - MANUAL_FIRST];
                    (RichText::new(key).color(theme::TEXT_DIM), format!("Touche {key}"))
                } else {
                    match view.from_markers.get(*trigger).copied().flatten() {
                        Some(secs) => (
                            RichText::new(format_time(secs)).color(theme::TEXT_DIM),
                            "Position sur le morceau de départ".to_string(),
                        ),
                        None => (
                            RichText::new("—").color(theme::RED),
                            "Marqueur non posé sur le départ".to_string(),
                        ),
                    }
                };
                ui.label(info.monospace().small()).on_hover_text(hint);
                let trigger = *trigger;
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if play_button(ui, None, Color32::from_rgb(0xe0, 0x50, 0x50), true, view.can_fire)
                        .on_hover_text("Lancer maintenant tout ce qui suit (délais compris)")
                        .clicked()
                    {
                        requests.push(NodeRequest::Fire(trigger));
                    }
                });
            });
        }
        NodeKind::Delay { amount, unit } => {
            // Durée et unité (secondes ou temps), centrées.
            let (row, _) = body.allocate_exact_size(vec2(body_rect.width(), 22.0), Sense::hover());
            let mut child = body.new_child(
                UiBuilder::new()
                    .max_rect(Rect::from_center_size(row.center(), vec2(118.0, 22.0)))
                    .layout(Layout::left_to_right(Align::Center))
                    .id_salt(("delay_row", id)),
            );
            child.spacing_mut().item_spacing.x = 4.0;
            let beats = *unit == DelayUnit::Beats;
            let range = if beats { 0.0..=256.0 } else { 0.0..=120.0 };
            child
                .add(DragValue::new(amount).range(range).speed(0.05).max_decimals(2))
                .on_hover_text(if beats {
                    "Attente, en temps (au tempo du départ)"
                } else {
                    "Attente, en secondes"
                });
            let label = if beats { "temps" } else { "s" };
            if child
                .add(
                    egui::Button::new(RichText::new(label).color(theme::TEXT_DIM))
                        .small()
                        .frame(false),
                )
                .on_hover_text("Unité — clic : secondes ↔ temps")
                .clicked()
            {
                *unit = if beats { DelayUnit::Secs } else { DelayUnit::Beats };
                if !beats {
                    *amount = amount.round().max(1.0);
                }
            }
            if let Some(progress) = context.activity.progress {
                let (rect, _) = body.allocate_exact_size(vec2(body_rect.width(), 6.0), Sense::hover());
                body.painter().rect_filled(rect, 3.0, theme::WAVE_BACKGROUND);
                let mut filled = rect;
                filled.set_width(rect.width() * progress);
                body.painter().rect_filled(filled, 3.0, WIRE_ACTIVE);
            }
        }
        NodeKind::Quantize { beats } => {
            // Prochaine limite : temps, mesure, 4 ou 8 mesures.
            const CHOICES: [(&str, u32, &str); 4] = [
                ("Temps", 1, "le prochain temps"),
                ("Mesure", 4, "la prochaine mesure (4 temps)"),
                ("4 mes.", 16, "le prochain bloc de 4 mesures"),
                ("8 mes.", 32, "la prochaine phrase de 8 mesures"),
            ];
            let (strip, _) = body.allocate_exact_size(vec2(body_rect.width(), 46.0), Sense::hover());
            let cell = vec2((strip.width() - 6.0) / 2.0, 20.0);
            for (index, (label, value, hint)) in CHOICES.into_iter().enumerate() {
                let min = strip.min + vec2((index % 2) as f32 * (cell.x + 6.0), (index / 2) as f32 * (cell.y + 6.0));
                let rect = Rect::from_min_size(min, cell);
                if toggle_button(
                    &mut body,
                    rect,
                    Id::unique(("quantize", id, index)),
                    label,
                    *beats == value,
                    theme::TEXT,
                )
                .on_hover_text(format!("Attendre {hint} sur la grille du départ"))
                .clicked()
                {
                    *beats = value;
                }
            }
        }
        NodeKind::Sequence { outputs } => {
            body.horizontal(|ui| {
                if ui.small_button("+ sortie").clicked() && *outputs < 8 {
                    *outputs += 1;
                }
                if ui
                    .add_enabled(*outputs > 1, egui::Button::new("− sortie").small())
                    .clicked()
                {
                    *outputs -= 1;
                    requests.push(NodeRequest::Pruned);
                }
            });
        }
        NodeKind::Action { action } => {
            action_node(&mut body, id, action, view);
            if !context.reached {
                body.label(RichText::new("Non relié à un événement").small().color(theme::TEXT_DIM));
            }
        }
        NodeKind::Control {
            side,
            control,
            level,
            cut,
        } => {
            let deck = if *side == Side::From { view.from } else { view.to };
            control_node(&mut body, id, theme::DECK_COLORS[deck], *control, level, cut);
            if !context.reached {
                body.vertical_centered(|ui| {
                    ui.label(RichText::new("Non relié à un événement").small().color(theme::TEXT_DIM));
                });
            }
        }
        NodeKind::Group {
            side,
            group,
            levels,
            cuts,
            secs,
            starts,
        } => {
            let deck = if *side == Side::From { view.from } else { view.to };
            group_node(
                &mut body,
                id,
                theme::DECK_COLORS[deck],
                *group,
                levels,
                cuts,
                secs,
                starts,
            );
            if !context.reached {
                body.vertical_centered(|ui| {
                    ui.label(RichText::new("Non relié à un événement").small().color(theme::TEXT_DIM));
                });
            }
        }
    }
    let bottom = body.min_rect().bottom().max(origin.y + HEADER_H + rows as f32 * ROW_H);
    let rect = Rect::from_min_max(origin, pos2(origin.x + width, bottom + 10.0));

    // Fond, en-tête, contour (peints sous le contenu).
    let mut shapes = Vec::new();
    shapes.push(Shape::rect_filled(
        rect.translate(vec2(4.0, 5.0)),
        9.0,
        Color32::from_black_alpha(90),
    ));
    if context.activity.flash > 0.0 {
        shapes.push(Shape::rect_filled(
            rect.expand(6.0),
            12.0,
            WIRE_ACTIVE.gamma_multiply(0.35 * context.activity.flash),
        ));
    }
    let alpha = if context.reached || !matches!(node.kind, NodeKind::Action { .. }) {
        1.0
    } else {
        0.75
    };
    shapes.push(Shape::rect_filled(rect, 8.0, NODE_BODY.gamma_multiply(alpha)));
    let header_fill = header_color(&node.kind).gamma_multiply(if alpha < 1.0 { 0.55 } else { 1.0 });
    shapes.push(Shape::rect_filled(
        header,
        CornerRadius {
            nw: 8,
            ne: 8,
            sw: 0,
            se: 0,
        },
        header_fill,
    ));
    // Dégradé discret vers la droite, comme les en-têtes d'Unreal.
    let mut mesh = egui::Mesh::default();
    let fade = Color32::from_black_alpha(110);
    let gradient = Rect::from_min_max(
        pos2(header.center().x, header.top()),
        pos2(header.right() - 8.0, header.bottom()),
    );
    mesh.colored_vertex(gradient.left_top(), Color32::TRANSPARENT);
    mesh.colored_vertex(gradient.right_top(), fade);
    mesh.colored_vertex(gradient.right_bottom(), fade);
    mesh.colored_vertex(gradient.left_bottom(), Color32::TRANSPARENT);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    shapes.push(Shape::mesh(mesh));
    // Bout droit arrondi, de la couleur de fin du dégradé.
    let darken = |c: u8| (c as f32 * (1.0 - 110.0 / 255.0)) as u8;
    let end_color = Color32::from_rgb(
        darken(header_fill.r()),
        darken(header_fill.g()),
        darken(header_fill.b()),
    );
    shapes.push(Shape::rect_filled(
        Rect::from_min_max(pos2(header.right() - 8.0, header.top()), header.right_bottom()),
        CornerRadius {
            nw: 0,
            ne: 8,
            sw: 0,
            se: 0,
        },
        end_color,
    ));
    let (border_width, border) = if context.selected {
        (2.5, SELECTED)
    } else if context.activity.flash > 0.0 {
        (2.0, WIRE_ACTIVE)
    } else {
        (1.0, NODE_BORDER)
    };
    shapes.push(Shape::rect_stroke(
        rect,
        8.0,
        Stroke::new(border_width, border),
        StrokeKind::Outside,
    ));
    canvas.painter().set(background, Shape::Vec(shapes));

    // Broches (par-dessus).
    for (pin, center, connected, response) in &pin_responses {
        let target = context.link_drag.is_some_and(|from| compatible(from, *pin))
            && context.pointer.is_some_and(|pointer| pointer.distance(*center) < 14.0);
        exec_pin(canvas.painter(), *center, *connected, response.hovered() || target);
        let response = response.clone().on_hover_text(match pin {
            PinRef::In { .. } => "Entrée d'exécution — glisser pour relier, Alt + clic pour rompre",
            PinRef::Out { .. } => "Sortie d'exécution — glisser pour relier, Alt + clic pour rompre",
        });
        let alt = canvas.ctx().input(|input| input.modifiers.alt);
        let command = canvas.ctx().input(|input| input.modifiers.command);
        if response.clicked() && alt {
            requests.push(NodeRequest::BreakLinks(*pin));
        } else if response.drag_started_by(PointerButton::Primary) {
            if alt {
                requests.push(NodeRequest::BreakLinks(*pin));
            } else if command && *connected && matches!(pin, PinRef::Out { .. }) {
                requests.push(NodeRequest::MoveLink(*pin));
            } else {
                requests.push(NodeRequest::StartLink(*pin));
            }
        }
        response.context_menu(|ui| {
            if ui.button("Rompre les liens de cette broche").clicked() {
                requests.push(NodeRequest::BreakLinks(*pin));
                ui.close();
            }
        });
    }

    // Fond du nœud : sélection, déplacement, menu contextuel.
    if frame.drag_started_by(PointerButton::Primary) {
        requests.push(NodeRequest::StartMove(id));
    } else if frame.clicked() {
        requests.push(NodeRequest::Select(id));
    }
    let event_trigger = match node.kind {
        NodeKind::Event { trigger } => Some(trigger),
        _ => None,
    };
    frame.context_menu(|ui| {
        ui.label(RichText::new(node_title(&node.kind)).strong());
        ui.separator();
        if let Some(trigger) = event_trigger
            && ui
                .add_enabled(view.can_fire, egui::Button::new("Déclencher maintenant"))
                .clicked()
        {
            requests.push(NodeRequest::Fire(trigger));
            ui.close();
        }
        if ui.button("Dupliquer (Ctrl + D)").clicked() {
            requests.push(NodeRequest::Duplicate(id));
            ui.close();
        }
        if ui.button("Rompre tous les liens").clicked() {
            if node.kind.has_input() {
                requests.push(NodeRequest::BreakLinks(PinRef::In { node: id }));
            }
            for pin in 0..node.kind.outputs() {
                requests.push(NodeRequest::BreakLinks(PinRef::Out { node: id, pin }));
            }
            ui.close();
        }
        if ui.button("Supprimer (Suppr)").clicked() {
            requests.push(NodeRequest::Delete(id));
            ui.close();
        }
    });
    (rect, requests)
}

/// Platine visée par un nœud, s'il en a une.
fn node_side(kind: &NodeKind) -> Option<Side> {
    match kind {
        NodeKind::Control { side, .. } | NodeKind::Group { side, .. } => Some(*side),
        NodeKind::Action { action } => action.side(),
        _ => None,
    }
}

fn set_node_side(kind: &mut NodeKind, new: Side) {
    match kind {
        NodeKind::Control { side, .. } | NodeKind::Group { side, .. } => *side = new,
        NodeKind::Action { action } => action.set_side(new),
        _ => {}
    }
}

/// Interrupteur de platine : pastille à gauche (platine A) ou à droite (B),
/// comme les platines à l'écran.
fn side_toggle(painter: &egui::Painter, rect: Rect, left: bool, color: Color32, hovered: bool) {
    painter.rect_filled(rect, 7.0, Color32::from_black_alpha(if hovered { 150 } else { 110 }));
    painter.rect_stroke(
        rect,
        7.0,
        Stroke::new(1.0, Color32::from_white_alpha(if hovered { 110 } else { 50 })),
        StrokeKind::Inside,
    );
    let x = if left { rect.left() + 7.0 } else { rect.right() - 7.0 };
    painter.circle_filled(pos2(x, rect.center().y), 5.0, color);
}

/// Couleur des positions de départ.
const START_COLOR: Color32 = theme::YELLOW;

/// Mode « départ » d'un nœud : ses composants règlent la position de départ
/// au lieu de la position visée (état d'affichage, non enregistré).
fn start_mode(ui: &Ui, id: u64) -> bool {
    ui.ctx()
        .data(|data| data.get_temp(Id::unique(("node_start_mode", id))))
        .unwrap_or(false)
}

fn set_start_mode(ui: &Ui, id: u64, on: bool) {
    ui.ctx()
        .data_mut(|data| data.insert_temp(Id::unique(("node_start_mode", id)), on));
}

/// Valeur montrée par un composant à double valeur (positions 0..1) : le
/// départ en mode départ (la valeur visée tant qu'il n'y en a pas), sinon
/// la valeur visée.
fn shown(editing: bool, target: f32, start: Option<f32>) -> f32 {
    if editing { start.unwrap_or(target) } else { target }
}

/// Repère de l'autre valeur : la visée en mode départ, sinon le départ.
fn ghost(editing: bool, target: Option<f32>, start: Option<f32>, color: Color32) -> Option<(f32, Color32)> {
    if editing {
        target.map(|target| (target, color))
    } else {
        start.map(|start| (start, START_COLOR))
    }
}

/// Durée du fondu, centrée sous les composants ; avec `start` (mode départ,
/// départ déjà posé), un bouton à gauche bascule le mode départ. Retourne
/// `true` si l'utilisateur demande de retirer le départ.
fn centered_duration(ui: &mut Ui, id: u64, secs: &mut f32, start: Option<(&mut bool, bool)>) -> bool {
    let mut clear = false;
    let width = ui.available_width();
    let (row, _) = ui.allocate_exact_size(vec2(width, 24.0), Sense::hover());
    let inner = if start.is_some() { 108.0 } else { 80.0 };
    let mut child = ui.new_child(
        UiBuilder::new()
            .max_rect(Rect::from_center_size(row.center(), vec2(inner, 24.0)))
            .layout(Layout::left_to_right(Align::Center))
            .id_salt(("node_duration", id)),
    );
    child.spacing_mut().item_spacing.x = 4.0;
    if let Some((editing, has_start)) = start {
        let accent = if *editing || has_start {
            START_COLOR
        } else {
            theme::TEXT_DIM
        };
        let hint = if *editing {
            "Mode départ : les composants règlent la position de départ (posée au déclenchement,\n\
             avant le fondu). Clic : revenir à la position visée · clic droit : retirer le départ"
        } else {
            "Position de départ : clic pour la régler (le composant part de là au déclenchement,\n\
             puis va vers la position visée) · clic droit : retirer le départ"
        };
        let response = icon_button(&mut child, Icon::Start, accent, *editing, true, hint);
        if response.clicked() {
            *editing = !*editing;
        }
        response.context_menu(|ui| {
            if ui
                .add_enabled(has_start, egui::Button::new("Retirer le départ"))
                .clicked()
            {
                clear = true;
                *editing = false;
                ui.close();
            }
        });
    }
    child.label(RichText::new("en").color(theme::TEXT_DIM));
    child
        .add(
            DragValue::new(secs)
                .range(0.0..=60.0)
                .speed(0.05)
                .suffix(" s")
                .max_decimals(1),
        )
        .on_hover_text("Durée du fondu (0 : immédiat)");
    clear
}

/// Un réglage avec les composants de la platine, dans `component` (fader ou
/// potentiomètre : niveau visé, `None` = inchangé ; en mode départ, niveau
/// de départ) et `button` (coupure : éteint = ne change rien, rouge = coupe,
/// couleur de la platine = rétablit).
#[allow(clippy::too_many_arguments)]
fn level_widget(
    ui: &mut Ui,
    salt: (u64, usize),
    control: Control,
    color: Color32,
    component: Rect,
    button: Rect,
    db: &mut Option<f32>,
    cut: &mut Option<bool>,
    secs: f32,
    start: &mut Option<f32>,
    editing: bool,
) {
    let eq = control.eq_band().is_some();
    let neutral = if eq { 0.5 } else { 1.0 };
    let target = db.map(|db| control.position_for_db(db));
    let start_position = start.filter(|_| db.is_some()).map(|db| control.position_for_db(db));
    let mut position = shown(editing, target.unwrap_or(neutral), start_position);
    let tint = if editing {
        START_COLOR
    } else if db.is_some() {
        color
    } else {
        theme::TEXT_DIM
    };
    let response = if eq {
        knob(ui, component, Id::unique(("node_knob", salt)), &mut position, 0.5, tint)
    } else {
        fader(
            ui,
            component,
            Id::unique(("node_fader", salt)),
            &mut position,
            1.0,
            Orientation::Vertical,
            tint,
        )
    };
    if let Some((value, mark)) = ghost(editing, target, start_position, color) {
        if eq {
            knob_ghost(ui.painter(), component, value, mark);
        } else {
            fader_ghost(ui.painter(), component, value, Orientation::Vertical, mark);
        }
    }
    let level = |db: f32| {
        if db <= MIN_DB {
            "coupé".to_string()
        } else {
            format!("{db:+.1} dB")
        }
    };
    let value = match (*db, start_position.and(*start)) {
        (Some(db), Some(from)) => format!("{} → {} en {secs:.1} s", level(from), level(db)),
        (Some(db), None) => format!("{} en {secs:.1} s", level(db)),
        (None, _) => "inchangé".to_string(),
    };
    let response = response.on_hover_text(format!(
        "{} — {value}\nGlisser : régler · double-clic : 0 dB · clic droit : ne plus régler",
        control.label()
    ));
    if response.changed() {
        let new = control.db_for_position(position);
        if editing {
            *start = Some(new);
            // Un départ sans niveau visé : on vise le neutre.
            if db.is_none() {
                *db = Some(control.db_for_position(neutral));
            }
        } else {
            *db = Some(new);
        }
    }
    response.context_menu(|ui| {
        if ui
            .add_enabled(db.is_some(), egui::Button::new("Ne pas régler le niveau"))
            .clicked()
        {
            *db = None;
            *start = None;
            ui.close();
        }
        if ui
            .add_enabled(start.is_some(), egui::Button::new("Retirer le départ"))
            .clicked()
        {
            *start = None;
            ui.close();
        }
    });

    let (active, button_color, state) = match *cut {
        Some(true) => (true, theme::RED, "coupe"),
        Some(false) => (true, color, "rétablit"),
        None => (false, color, "ne change rien"),
    };
    if toggle_button(
        ui,
        button,
        Id::unique(("node_cut", salt)),
        control.button_label(),
        active,
        button_color,
    )
    .on_hover_text(format!(
        "{} — bouton : {state}\nClic : ne rien changer → couper → rétablir → …",
        control.label()
    ))
    .clicked()
    {
        *cut = match *cut {
            None => Some(true),
            Some(true) => Some(false),
            Some(false) => None,
        };
    }
}

/// Nœud « réglage » (piste, bande d'EQ, volume), avec les composants de la
/// platine : le fader (ou le potentiomètre) règle le niveau visé, le bouton
/// coupe ou rétablit. Les deux peuvent être utilisés dans le même nœud.
fn control_node(
    ui: &mut Ui,
    id: u64,
    color: Color32,
    control: Control,
    level: &mut Option<Level>,
    cut: &mut Option<bool>,
) {
    let eq = control.eq_band().is_some();
    let width = ui.available_width();
    let (strip, _) = ui.allocate_exact_size(vec2(width, if eq { 56.0 } else { 110.0 }), Sense::hover());
    let center = strip.center().x;
    let (component, button) = if eq {
        // Comme la ligne d'EQ de la platine : kill à gauche, potentiomètre à droite.
        (
            Rect::from_center_size(pos2(center + 26.0, strip.center().y), vec2(50.0, 50.0)),
            Rect::from_center_size(pos2(center - 30.0, strip.center().y), vec2(42.0, 20.0)),
        )
    } else {
        // Comme un stem : fader vertical, bouton de coupure dessous.
        (
            Rect::from_center_size(pos2(center, strip.top() + 43.0), vec2(34.0, 86.0)),
            Rect::from_center_size(pos2(center, strip.bottom() - 9.0), vec2(54.0, 18.0)),
        )
    };
    let mut editing = start_mode(ui, id) && level.is_some();
    let secs = level.map_or(1.0, |level| level.secs);
    let mut db = level.map(|level| level.db);
    let mut from = level.and_then(|level| level.from);
    level_widget(
        ui,
        (id, 0),
        control,
        color,
        component,
        button,
        &mut db,
        cut,
        secs,
        &mut from,
        editing,
    );
    *level = db.map(|db| Level { db, secs, from });
    if let Some(level) = level {
        if centered_duration(ui, id, &mut level.secs, Some((&mut editing, level.from.is_some()))) {
            level.from = None;
        }
    } else {
        editing = false;
    }
    set_start_mode(ui, id, editing);
}

/// Nœud groupé : les 4 stems côte à côte (comme sur la platine), ou les 3
/// lignes d'EQ (HIGH, MID, LOW) ; un fondu commun.
#[allow(clippy::too_many_arguments)]
fn group_node(
    ui: &mut Ui,
    id: u64,
    color: Color32,
    group: Group,
    levels: &mut [Option<f32>; 4],
    cuts: &mut [Option<bool>; 4],
    secs: &mut f32,
    starts: &mut [Option<f32>; 4],
) {
    let controls = group.controls();
    let width = ui.available_width();
    let mut editing = start_mode(ui, id);
    match group {
        Group::Stems => {
            let (strip, _) = ui.allocate_exact_size(vec2(width, 110.0), Sense::hover());
            let gap = 6.0;
            let column = ((width - gap * 3.0) / 4.0).min(48.0);
            let left = strip.center().x - (column * 4.0 + gap * 3.0) / 2.0;
            for (index, control) in controls.iter().copied().enumerate() {
                let x = left + index as f32 * (column + gap) + column / 2.0;
                let component = Rect::from_center_size(pos2(x, strip.top() + 43.0), vec2(30.0, 86.0));
                let button = Rect::from_center_size(pos2(x, strip.bottom() - 9.0), vec2(column, 18.0));
                level_widget(
                    ui,
                    (id, index),
                    control,
                    color,
                    component,
                    button,
                    &mut levels[index],
                    &mut cuts[index],
                    *secs,
                    &mut starts[index],
                    editing,
                );
            }
        }
        Group::Eq => {
            let row = 48.0;
            let (strip, _) = ui.allocate_exact_size(vec2(width, row * controls.len() as f32), Sense::hover());
            let center = strip.center().x;
            for (index, control) in controls.iter().copied().enumerate() {
                let y = strip.top() + row * (index as f32 + 0.5);
                let component = Rect::from_center_size(pos2(center + 26.0, y), vec2(44.0, 44.0));
                let button = Rect::from_center_size(pos2(center - 28.0, y), vec2(42.0, 20.0));
                level_widget(
                    ui,
                    (id, index),
                    control,
                    color,
                    component,
                    button,
                    &mut levels[index],
                    &mut cuts[index],
                    *secs,
                    &mut starts[index],
                    editing,
                );
            }
        }
    }
    let count = controls.len();
    if levels.iter().take(count).any(Option::is_some) {
        let has_start = starts.iter().take(count).any(Option::is_some);
        if centered_duration(ui, id, secs, Some((&mut editing, has_start))) {
            *starts = [None; 4];
        }
    } else {
        editing = false;
    }
    set_start_mode(ui, id, editing);
}

/// Contenu d'un nœud action : lecture, tempo (avec SYNC), tonalité (avec
/// MATCH), crossfader, marqueur — avec les composants de la platine.
fn action_node(ui: &mut Ui, id: u64, action: &mut Action, view: &RunView) {
    let deck = action
        .side()
        .map(|side| if side == Side::From { view.from } else { view.to });
    let color = deck.map_or(theme::TEXT, |deck| theme::DECK_COLORS[deck]);
    match action {
        Action::Crossfader { .. } => crossfader_node(ui, id, action, view.from, view.to),
        Action::Tempo { .. } | Action::Sync { .. } => tempo_node(ui, id, action, color),
        Action::Key { .. } | Action::Match { .. } => {
            key_node(ui, id, action, color, deck.and_then(|deck| view.keys[deck]))
        }
        Action::Play { .. } | Action::Stop { .. } => transport_node(ui, id, action),
        Action::Jump { .. } => jump_node(ui, id, action),
        Action::Filter { .. } => filter_node(ui, id, action, color),
        Action::Loop { .. } => loop_node(ui, id, action, color),
        // Réglages : nœuds « réglage » (voir `NodeKind::Control`).
        Action::Fade { .. } | Action::Mute { .. } | Action::Unmute { .. } => {}
    }
}

/// Crossfader horizontal, de la platine de départ à celle de destination.
fn crossfader_node(ui: &mut Ui, id: u64, action: &mut Action, from: usize, to: usize) {
    let Action::Crossfader {
        toward,
        secs,
        from: start,
    } = action
    else {
        return;
    };
    let mut editing = start_mode(ui, id);
    let width = ui.available_width();
    let (strip, _) = ui.allocate_exact_size(vec2(width, 44.0), Sense::hover());
    let rail = Rect::from_center_size(pos2(strip.center().x, strip.top() + 13.0), vec2(width.min(196.0), 26.0));
    let mut value = shown(editing, *toward, *start);
    let tint = if editing { START_COLOR } else { theme::TEXT };
    let response = fader(
        ui,
        rail,
        Id::unique(("node_crossfader", id)),
        &mut value,
        0.5,
        Orientation::Horizontal,
        tint,
    );
    if let Some((mark, color)) = ghost(editing, Some(*toward), *start, theme::TEXT) {
        fader_ghost(ui.painter(), rail, mark, Orientation::Horizontal, color);
    }
    let text = match *start {
        Some(start) => format!("{:.0} % → {:.0} %", start * 100.0, *toward * 100.0),
        None => format!("{:.0} %", *toward * 100.0),
    };
    response.on_hover_text(format!("Crossfader vers la destination : {text}"));
    if value != shown(editing, *toward, *start) {
        if editing {
            *start = Some(value);
        } else {
            *toward = value;
        }
    }
    let painter = ui.painter();
    let y = strip.bottom() - 7.0;
    painter.text(
        pos2(rail.left(), y),
        Align2::LEFT_CENTER,
        theme::DECK_NAMES[from],
        FontId::proportional(12.0),
        theme::DECK_COLORS[from],
    );
    painter.text(
        pos2(rail.right(), y),
        Align2::RIGHT_CENTER,
        theme::DECK_NAMES[to],
        FontId::proportional(12.0),
        theme::DECK_COLORS[to],
    );
    if centered_duration(ui, id, secs, Some((&mut editing, start.is_some()))) {
        *start = None;
    }
    set_start_mode(ui, id, editing);
}

/// Filtre : le potentiomètre de la platine (à gauche passe-bas, à droite
/// passe-haut), le réglage visé, et le départ éventuel.
fn filter_node(ui: &mut Ui, id: u64, action: &mut Action, color: Color32) {
    let Action::Filter { value, secs, from, .. } = action else {
        return;
    };
    let mut editing = start_mode(ui, id);
    let width = ui.available_width();
    let (strip, _) = ui.allocate_exact_size(vec2(width, 60.0), Sense::hover());
    let knob_rect = Rect::from_center_size(strip.center(), vec2(54.0, 54.0));
    let to_knob = |value: f32| (value.clamp(-1.0, 1.0) + 1.0) / 2.0;
    let target = to_knob(*value);
    let start = from.map(to_knob);
    let mut position = shown(editing, target, start);
    let before = position;
    let tint = if editing { START_COLOR } else { color };
    let response = knob(ui, knob_rect, Id::unique(("node_filter", id)), &mut position, 0.5, tint);
    if let Some((mark, mark_color)) = ghost(editing, Some(target), start, color) {
        knob_ghost(ui.painter(), knob_rect, mark, mark_color);
    }
    let text = match *from {
        Some(from) => format!("{} → {}", filter_label(from), filter_label(*value)),
        None => filter_label(*value),
    };
    response.on_hover_text(format!(
        "Filtre : {text}\nÀ gauche : passe-bas · à droite : passe-haut · double-clic : neutre"
    ));
    if position != before {
        let new = ((position * 2.0 - 1.0) * 100.0).round() / 100.0;
        if editing {
            *from = Some(new);
        } else {
            *value = new;
        }
    }
    let painter = ui.painter();
    for (x, label) in [(knob_rect.left() - 14.0, "LP"), (knob_rect.right() + 14.0, "HP")] {
        painter.text(
            pos2(x, strip.center().y + 14.0),
            Align2::CENTER_CENTER,
            label,
            FontId::proportional(10.0),
            theme::TEXT_DIM,
        );
    }
    if centered_duration(ui, id, secs, Some((&mut editing, from.is_some()))) {
        *from = None;
    }
    set_start_mode(ui, id, editing);
}

/// Boucle : longueur en temps (8 à ¼), ou sortie de boucle.
fn loop_node(ui: &mut Ui, id: u64, action: &mut Action, color: Color32) {
    const LENGTHS: [f32; 6] = [8.0, 4.0, 2.0, 1.0, 0.5, 0.25];
    let Action::Loop { beats, .. } = action else { return };
    let width = ui.available_width();
    let (strip, _) = ui.allocate_exact_size(vec2(width, 22.0), Sense::hover());
    let count = LENGTHS.len() + 1;
    let gap = 3.0;
    let cell = (strip.width() - gap * (count - 1) as f32) / count as f32;
    for index in 0..count {
        let rect = Rect::from_min_size(strip.min + vec2(index as f32 * (cell + gap), 0.0), vec2(cell, 20.0));
        let (label, value, hint) = match LENGTHS.get(index) {
            Some(&length) => (
                beat_label(length),
                Some(length),
                format!(
                    "Boucler {} temps (calée sur la grille ; une boucle en cours garde son début)",
                    beat_label(length)
                ),
            ),
            None => ("×".to_string(), None, "Sortir de la boucle".to_string()),
        };
        let selected = *beats == value;
        let fill = if value.is_none() { theme::RED } else { color };
        if toggle_button(ui, rect, Id::unique(("node_loop", id, index)), &label, selected, fill)
            .on_hover_text(hint)
            .clicked()
        {
            *beats = value;
        }
    }
}

/// Tempo : le bouton SYNC au-dessus du fader de tempo, comme sur la platine.
/// SYNC allumé : calage unique sur l'autre platine ; fader : tempo visé (et
/// tempo de départ éventuel).
fn tempo_node(ui: &mut Ui, id: u64, action: &mut Action, color: Color32) {
    let Some(side) = action.side() else { return };
    let sync = matches!(action, Action::Sync { .. });
    let mut editing = start_mode(ui, id) && !sync;
    let width = ui.available_width();
    let (strip, _) = ui.allocate_exact_size(vec2(width, 118.0), Sense::hover());
    let button = Rect::from_center_size(pos2(strip.center().x, strip.top() + 10.0), vec2(56.0, 20.0));
    let rail = Rect::from_center_size(pos2(strip.center().x, strip.top() + 72.0), vec2(34.0, 88.0));
    if toggle_button(ui, button, Id::unique(("node_sync", id)), "SYNC", sync, color)
        .on_hover_text(
            "SYNC : cale une fois le tempo sur celui de l'autre platine.\nClic : régler le tempo au fader à la place.",
        )
        .clicked()
    {
        *action = if sync {
            Action::Tempo {
                side,
                percent: 0.0,
                secs: 8.0,
                from: None,
            }
        } else {
            Action::Sync { side }
        };
    }
    let (percent, secs, from) = match *action {
        Action::Tempo {
            percent, secs, from, ..
        } => (percent, secs, from),
        _ => (0.0, 8.0, None),
    };
    let to_fader = |percent: f32| (0.5 + percent / 24.0).clamp(0.0, 1.0);
    let target = to_fader(percent);
    let start = from.map(to_fader);
    let mut position = shown(editing, target, start);
    let before = position;
    let tint = if sync {
        theme::TEXT_DIM
    } else if editing {
        START_COLOR
    } else {
        color
    };
    let format = |percent: f32| {
        if percent == 0.0 {
            "tempo d'origine".to_string()
        } else {
            format!("{percent:+.1} %")
        }
    };
    let value = match (sync, from) {
        (true, _) => "SYNC".to_string(),
        (false, Some(from)) => format!("{} → {}", format(from), format(percent)),
        (false, None) => format(percent),
    };
    fader(
        ui,
        rail,
        Id::unique(("node_tempo", id)),
        &mut position,
        0.5,
        Orientation::Vertical,
        tint,
    )
    .on_hover_text(format!(
        "Tempo visé : {value}\nGlisser : régler (coupe SYNC) · double-clic : tempo d'origine"
    ));
    if !sync && let Some((mark, mark_color)) = ghost(editing, Some(target), start, color) {
        fader_ghost(ui.painter(), rail, mark, Orientation::Vertical, mark_color);
    }
    if position != before {
        let new = (((position - 0.5) * 24.0) * 10.0).round() / 10.0;
        *action = if editing {
            Action::Tempo {
                side,
                percent,
                secs,
                from: Some(new),
            }
        } else {
            Action::Tempo {
                side,
                percent: new,
                secs,
                from: if sync { None } else { from },
            }
        };
    }
    if let Action::Tempo { secs, from, .. } = action {
        if centered_duration(ui, id, secs, Some((&mut editing, from.is_some()))) {
            *from = None;
        }
    } else {
        editing = false;
    }
    set_start_mode(ui, id, editing);
}

/// Tonalité : le bouton MATCH au-dessus du slider de tonalité de la platine
/// (un cran par demi-ton, chaque cran avec sa note).
fn key_node(ui: &mut Ui, id: u64, action: &mut Action, color: Color32, key: Option<MusicalKey>) {
    const STEP: f32 = 13.0;
    const RANGE: i32 = 6;
    let Some(side) = action.side() else { return };
    let matching = matches!(action, Action::Match { .. });
    let width = ui.available_width();
    let height = STEP * (2 * RANGE + 1) as f32;
    let (strip, _) = ui.allocate_exact_size(vec2(width, 28.0 + height), Sense::hover());
    let button = Rect::from_center_size(pos2(strip.center().x, strip.top() + 10.0), vec2(64.0, 20.0));
    if toggle_button(ui, button, Id::unique(("node_match", id)), "MATCH", matching, color)
        .on_hover_text("MATCH : accorde une fois la tonalité sur celle de l'autre platine.\nClic : choisir la note au slider à la place.")
        .clicked()
    {
        *action = if matching {
            Action::Key { side, semitones: 0 }
        } else {
            Action::Match { side }
        };
    }
    let slider = Rect::from_min_size(pos2(strip.center().x - 30.0, strip.top() + 26.0), vec2(60.0, height));
    let response = ui.interact(slider, Id::unique(("node_key", id)), Sense::click_and_drag());
    if response.double_clicked() {
        *action = Action::Key { side, semitones: 0 };
    } else if (response.clicked() || response.dragged())
        && let Some(pointer) = response.interact_pointer_pos()
    {
        let offset = (RANGE - ((pointer.y - slider.top()) / STEP).floor() as i32).clamp(-RANGE, RANGE);
        *action = Action::Key {
            side,
            semitones: offset,
        };
    }
    let selected = match *action {
        Action::Key { semitones, .. } => Some(semitones),
        _ => None,
    };
    let painter = ui.painter();
    painter.rect_filled(slider, 4.0, theme::WAVE_BACKGROUND);
    painter.rect_stroke(slider, 4.0, Stroke::new(1.0, theme::OUTLINE), StrokeKind::Inside);
    for offset in -RANGE..=RANGE {
        let row = Rect::from_min_size(
            pos2(slider.left() + 2.0, slider.top() + (RANGE - offset) as f32 * STEP),
            vec2(slider.width() - 4.0, STEP - 1.0),
        );
        let text = match key {
            Some(key) => key_label(key.transposed(offset)),
            None if offset == 0 => "0".to_string(),
            None => format!("{offset:+}"),
        };
        let (fill, text_color) = if selected == Some(offset) {
            (Some(if matching { theme::TEXT_DIM } else { color }), Color32::BLACK)
        } else if offset == 0 {
            (Some(theme::PANEL_LIGHT), theme::TEXT)
        } else {
            (None, theme::TEXT_DIM)
        };
        if let Some(fill) = fill {
            painter.rect_filled(row, 3.0, fill);
        }
        painter.text(
            row.center(),
            Align2::CENTER_CENTER,
            text,
            FontId::proportional(10.0),
            text_color,
        );
    }
    let value = match selected {
        _ if matching => "MATCH".to_string(),
        Some(0) | None => "tonalité d'origine".to_string(),
        Some(n) => format!("{n:+} demi-ton(s)"),
    };
    response.on_hover_text(format!(
        "Tonalité visée : {value}\nClic ou glisser sur une note · double-clic : origine"
    ));
}

/// Boutons de lecture de la platine : lancer ou arrêter.
fn transport_node(ui: &mut Ui, id: u64, action: &mut Action) {
    let Some(side) = action.side() else { return };
    let playing = matches!(action, Action::Play { .. });
    let width = ui.available_width();
    let (strip, _) = ui.allocate_exact_size(vec2(width, 46.0), Sense::hover());
    let center = strip.center();
    let play = Rect::from_center_size(center - vec2(31.0, 0.0), vec2(56.0, 44.0));
    let stop = Rect::from_center_size(center + vec2(31.0, 0.0), vec2(56.0, 44.0));
    if transport_button(
        ui,
        play,
        Id::unique(("node_play", id)),
        TransportIcon::Play,
        playing,
        theme::GREEN,
    )
    .on_hover_text("Lancer la lecture")
    .clicked()
    {
        *action = Action::Play { side };
    }
    if transport_button(
        ui,
        stop,
        Id::unique(("node_stop", id)),
        TransportIcon::Pause,
        !playing,
        theme::CUE,
    )
    .on_hover_text("Arrêter la lecture")
    .clicked()
    {
        *action = Action::Stop { side };
    }
}

/// Rangée de marqueurs, comme sous la vue d'ensemble de la platine.
fn jump_node(ui: &mut Ui, id: u64, action: &mut Action) {
    let Action::Jump { marker, .. } = action else { return };
    if is_manual(*marker) || *marker >= NUM_TRIGGERS {
        *marker = 0;
    }
    let width = ui.available_width();
    let (strip, _) = ui.allocate_exact_size(vec2(width, 48.0), Sense::hover());
    let gap = 3.0;
    let cell = (width - gap * 7.0) / 8.0;
    for slot in 0..NUM_MARKERS {
        let rect = Rect::from_min_size(strip.min + vec2(slot as f32 * (cell + gap), 0.0), vec2(cell, 20.0));
        if toggle_button(
            ui,
            rect,
            Id::unique(("node_jump", id, slot)),
            &(slot + 1).to_string(),
            *marker == slot,
            theme::MARKER_COLORS[slot],
        )
        .clicked()
        {
            *marker = slot;
        }
    }
    let half = (width - gap) / 2.0;
    for (index, (slot, label)) in [(MARKER_START, "DÉBUT"), (MARKER_END, "FIN")].into_iter().enumerate() {
        let rect = Rect::from_min_size(strip.min + vec2(index as f32 * (half + gap), 26.0), vec2(half, 20.0));
        if toggle_button(
            ui,
            rect,
            Id::unique(("node_jump", id, slot)),
            label,
            *marker == slot,
            theme::SPECIAL_MARKER,
        )
        .clicked()
        {
            *marker = slot;
        }
    }
}

/// Liste déroulante d'un déclencheur (début, marqueurs, fin, manuels).
fn trigger_combo(ui: &mut Ui, id: impl std::hash::Hash + std::fmt::Debug, trigger: &mut usize, with_manual: bool) {
    let manual = if with_manual { MANUAL_FIRST..ALL_TRIGGERS } else { 0..0 };
    ComboBox::from_id_salt(id)
        .selected_text(RichText::new(trigger_short_name(*trigger)).color(marker_color(*trigger)))
        .width(120.0)
        .show_ui(ui, |ui| {
            for slot in [MARKER_START]
                .into_iter()
                .chain(0..NUM_MARKERS)
                .chain([MARKER_END])
                .chain(manual)
            {
                ui.selectable_value(
                    trigger,
                    slot,
                    RichText::new(capitalize(&trigger_long_name(slot))).color(marker_color(slot)),
                );
            }
        });
}

/// Entrées de la recherche, par catégorie.
fn entries() -> Vec<Entry> {
    let mut list = Vec::new();
    let event = |name: String, keywords, trigger| Entry {
        category: "Événements",
        name,
        keywords,
        item: NewItem::Node(NodeKind::Event { trigger }),
    };
    list.push(event(
        "Début de la musique".into(),
        "start depart intro marqueur",
        MARKER_START,
    ));
    for slot in 0..NUM_MARKERS {
        list.push(event(format!("Marqueur {}", slot + 1), "marqueur hot cue repere", slot));
    }
    list.push(event("Fin de la musique".into(), "end outro marqueur", MARKER_END));
    for (n, key) in MANUAL_KEYS.iter().enumerate() {
        list.push(event(
            format!("Manuel {} ({key})", n + 1),
            "manuel bouton touche declencheur",
            MANUAL_FIRST + n,
        ));
    }
    list.push(Entry {
        category: "Flux",
        name: "Délai".into(),
        keywords: "attendre attente delay pause secondes",
        item: NewItem::Node(NodeKind::Delay {
            amount: 2.0,
            unit: DelayUnit::Secs,
        }),
    });
    list.push(Entry {
        category: "Flux",
        name: "Délai en temps".into(),
        keywords: "attendre attente delay temps beats mesures rythme tempo",
        item: NewItem::Node(NodeKind::Delay {
            amount: 4.0,
            unit: DelayUnit::Beats,
        }),
    });
    list.push(Entry {
        category: "Flux",
        name: "Quantifier".into(),
        keywords: "quantize quantifier caler temps mesure grille prochain attendre",
        item: NewItem::Node(NodeKind::Quantize { beats: 4 }),
    });
    list.push(Entry {
        category: "Flux",
        name: "Séquence".into(),
        keywords: "sequence ordre branches plusieurs sorties",
        item: NewItem::Node(NodeKind::Sequence { outputs: 2 }),
    });
    list.push(Entry {
        category: "Flux",
        name: "Commentaire".into(),
        keywords: "commentaire note cadre groupe",
        item: NewItem::Comment,
    });
    let action = |category, name: String, keywords, action| Entry {
        category,
        name,
        keywords,
        item: NewItem::Node(NodeKind::Action { action }),
    };
    // Réglages : nœuds neutres (on règle au fader, on coupe au bouton).
    let control = |category, control: Control, keywords| Entry {
        category,
        name: control.label().into(),
        keywords,
        item: NewItem::Node(NodeKind::Control {
            side: Side::From,
            control,
            level: None,
            cut: None,
        }),
    };
    for stem in [Control::Vocals, Control::Drums, Control::Bass, Control::Instruments] {
        list.push(control(
            "Pistes (stems)",
            stem,
            "stem piste couper regler retablir niveau fader voix vocal chant batterie basse instruments",
        ));
    }
    for band in [Control::High, Control::Mid, Control::Low] {
        list.push(control(
            "Platine",
            band,
            "eq egaliseur bande kill couper regler retablir aigus mediums basses potentiometre",
        ));
    }
    list.push(control(
        "Platine",
        Control::Volume,
        "volume fader niveau fondu fade couper regler",
    ));
    let group = |category, group: Group, keywords| Entry {
        category,
        name: group.label().into(),
        keywords,
        item: NewItem::Node(NodeKind::Group {
            side: Side::From,
            group,
            levels: [None; 4],
            cuts: [None; 4],
            secs: 1.0,
            starts: [None; 4],
        }),
    };
    list.push(group(
        "Pistes (stems)",
        Group::Stems,
        "stems pistes toutes groupe voix batterie basse instruments",
    ));
    list.push(group(
        "Platine",
        Group::Eq,
        "hml eq egaliseur groupe high mid low aigus mediums basses kill",
    ));
    list.push(action(
        "Platine",
        "Lecture".into(),
        "lancer arreter play stop pause lecture demarrer",
        Action::Play { side: Side::To },
    ));
    list.push(action(
        "Platine",
        "Tempo".into(),
        "tempo sync vitesse bpm pitch fader caler",
        Action::Sync { side: Side::To },
    ));
    list.push(action(
        "Platine",
        "Tonalité".into(),
        "tonalite match key note accorder transposer",
        Action::Match { side: Side::To },
    ));
    list.push(action(
        "Platine",
        "Aller au marqueur".into(),
        "sauter jump marqueur position hot cue",
        Action::Jump {
            side: Side::To,
            marker: 0,
        },
    ));
    list.push(action(
        "Mixer",
        "Crossfader".into(),
        "crossfader fondu croise mixer",
        Action::Crossfader {
            toward: 1.0,
            secs: 8.0,
            from: None,
        },
    ));
    list.push(action(
        "Effets",
        "Filtre".into(),
        "filtre filter passe-bas passe-haut lowpass highpass balayage sweep effet",
        Action::Filter {
            side: Side::From,
            value: 0.6,
            secs: 8.0,
            from: None,
        },
    ));
    list.push(action(
        "Effets",
        "Boucle".into(),
        "boucle loop roll temps repeter",
        Action::Loop {
            side: Side::From,
            beats: Some(4.0),
        },
    ));
    list
}

/// Recherche insensible à la casse et aux accents ; tous les mots doivent
/// apparaître (catégorie, nom ou mots-clés).
fn matches(entry: &Entry, query: &str) -> bool {
    let haystack = normalize(&format!("{} {} {}", entry.category, entry.name, entry.keywords));
    normalize(query).split_whitespace().all(|word| haystack.contains(word))
}

fn normalize(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .map(|c| match c {
            'à' | 'â' | 'ä' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'î' | 'ï' => 'i',
            'ô' | 'ö' => 'o',
            'ù' | 'û' | 'ü' => 'u',
            'ç' => 'c',
            other => other,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_ignores_case_and_accents() {
        let all = entries();
        let find = |query: &str| -> Vec<String> {
            all.iter()
                .filter(|entry| matches(entry, query))
                .map(|entry| entry.name.clone())
                .collect()
        };
        assert_eq!(find("delai"), vec!["Délai".to_string(), "Délai en temps".to_string()]);
        assert_eq!(find("quantifier mesure"), vec!["Quantifier".to_string()]);
        assert!(find("filtre").contains(&"Filtre".to_string()));
        assert!(find("loop").contains(&"Boucle".to_string()));
        assert!(find("couper voix").contains(&"Voix".to_string()));
        assert!(find("kill").contains(&"EQ Low".to_string()));
        assert!(find("arreter").contains(&"Lecture".to_string()));
        assert!(find("hml").contains(&"HML".to_string()));
        assert!(find("stems").contains(&"STEMS".to_string()));
        assert!(find("MARQUEUR 3").contains(&"Marqueur 3".to_string()));
        assert!(find("f7").iter().any(|name| name.starts_with("Manuel 1")));
        assert!(find("zzz").is_empty());
    }

    #[test]
    fn pins_line_up_with_node_rows() {
        let node = Node {
            id: 1,
            pos: [100.0, 50.0],
            kind: NodeKind::Sequence { outputs: 3 },
        };
        let input = pin_pos(&node, PinRef::In { node: 1 });
        let second = pin_pos(&node, PinRef::Out { node: 1, pin: 1 });
        assert_eq!(input, pos2(100.0 + PIN_INSET, 50.0 + HEADER_H + ROW_H / 2.0));
        assert_eq!(second.y - input.y, ROW_H);
        assert!(compatible(PinRef::Out { node: 1, pin: 0 }, PinRef::In { node: 2 }));
        assert!(!compatible(PinRef::Out { node: 1, pin: 0 }, PinRef::In { node: 1 }));
        assert!(!compatible(PinRef::In { node: 1 }, PinRef::In { node: 2 }));
        assert_eq!(snap(23.0), 16.0);
    }
}
