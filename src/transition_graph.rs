//! Graphe de nœuds d'une transition (éditeur façon « Blueprint ») et sa
//! traduction en étapes exécutables.
//!
//! * Les nœuds **événement** (marqueur, début / fin de la musique, manuel)
//!   n'ont qu'une sortie d'exécution.
//! * Les nœuds **action** (lancer, régler, couper…) ont une entrée et une sortie.
//! * Les nœuds de **flux** : *Délai* (attend des secondes ou des temps avant
//!   de passer à la suite), *Quantifier* (attend le prochain temps, la
//!   prochaine mesure…) et *Séquence* (plusieurs sorties, exécutées dans
//!   l'ordre, sans attente).
//!
//! Comme dans Unreal, une sortie d'exécution ne mène qu'à une entrée ; une
//! entrée peut recevoir plusieurs liens. La compilation parcourt le graphe
//! depuis chaque événement, calcule l'instant de chaque action (somme des
//! délais traversés) et produit des [`Step`] : même déclencheur, et délai
//! relatif à l'étape précédente du groupe. Une action atteinte par un délai
//! en temps ou une quantification garde son chemin de temps ([`Timing`]),
//! calculé à l'exécution sur le tempo et la grille du départ.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::markers::Triggers;
use crate::transitions::{Action, Control, ESTIMATED_BEAT_SECS, Side, Step, TimeOp, Timing, Transition};

/// Groupe de réglages d'un nœud groupé : les 4 stems, ou les 3 bandes d'EQ.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Group {
    Stems,
    Eq,
}

impl Group {
    pub fn controls(self) -> &'static [Control] {
        match self {
            Group::Stems => &[Control::Vocals, Control::Drums, Control::Bass, Control::Instruments],
            Group::Eq => &[Control::High, Control::Mid, Control::Low],
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Group::Stems => "STEMS",
            Group::Eq => "HML",
        }
    }
}

fn default_secs() -> f32 {
    1.0
}

/// Niveau visé par un nœud de réglage, durée du fondu, et niveau de départ
/// éventuel (posé d'abord, sans fondu).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Level {
    pub db: f32,
    pub secs: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<f32>,
}

/// Unité d'un délai.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum DelayUnit {
    #[default]
    Secs,
    /// Temps, au tempo de la platine de départ.
    Beats,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum NodeKind {
    /// Événement : déclencheur (marqueur, début / fin, manuel).
    Event {
        trigger: usize,
    },
    /// Attend `amount` secondes ou temps avant de continuer.
    Delay {
        #[serde(alias = "secs")]
        amount: f32,
        #[serde(default)]
        unit: DelayUnit,
    },
    /// Attend le prochain multiple de `beats` temps sur la grille du départ.
    Quantize {
        beats: u32,
    },
    /// Exécute ses sorties l'une après l'autre (sans attente).
    Sequence {
        outputs: usize,
    },
    Action {
        action: Action,
    },
    /// Réglage d'une platine (piste, bande d'EQ, volume), comme sur la
    /// platine : niveau visé (fader) et/ou coupure (`Some(true)`) ou
    /// rétablissement (`Some(false)`) par le bouton. Un seul nœud peut donc
    /// rétablir et régler à la fois.
    Control {
        side: Side,
        control: Control,
        #[serde(default)]
        level: Option<Level>,
        #[serde(default)]
        cut: Option<bool>,
    },
    /// Plusieurs réglages d'une platine dans un même nœud (les 4 stems, ou
    /// les 3 bandes d'EQ) : pour chacun, niveau visé (dB) et/ou coupure,
    /// dans l'ordre de [`Group::controls`] ; un fondu commun de `secs`.
    Group {
        side: Side,
        group: Group,
        #[serde(default)]
        levels: [Option<f32>; 4],
        #[serde(default)]
        cuts: [Option<bool>; 4],
        #[serde(default = "default_secs")]
        secs: f32,
        /// Niveaux de départ (dB), posés avant les fondus.
        #[serde(default)]
        starts: [Option<f32>; 4],
    },
}

impl NodeKind {
    /// A une entrée d'exécution.
    pub fn has_input(&self) -> bool {
        !matches!(self, NodeKind::Event { .. })
    }

    /// Actions exécutées par ce nœud, dans l'ordre.
    pub fn actions(&self) -> Vec<Action> {
        match *self {
            NodeKind::Action { action } => vec![action],
            NodeKind::Control {
                side,
                control,
                level,
                cut,
            } => {
                let mut actions = Vec::new();
                if cut == Some(false) {
                    actions.push(Action::Unmute { side, control });
                }
                if let Some(Level { db, secs, from }) = level {
                    actions.push(Action::Fade {
                        side,
                        control,
                        db,
                        secs,
                        from,
                    });
                }
                if cut == Some(true) {
                    actions.push(Action::Mute { side, control });
                }
                actions
            }
            NodeKind::Group {
                side,
                group,
                levels,
                cuts,
                secs,
                starts,
            } => {
                // Rétablissements, puis fondus, puis coupures.
                let controls = group.controls().iter().copied().enumerate();
                let mut actions: Vec<Action> = controls
                    .clone()
                    .filter(|(index, _)| cuts[*index] == Some(false))
                    .map(|(_, control)| Action::Unmute { side, control })
                    .collect();
                actions.extend(controls.clone().filter_map(|(index, control)| {
                    levels[index].map(|db| Action::Fade {
                        side,
                        control,
                        db,
                        secs,
                        from: starts[index],
                    })
                }));
                actions.extend(
                    controls
                        .filter(|(index, _)| cuts[*index] == Some(true))
                        .map(|(_, control)| Action::Mute { side, control }),
                );
                actions
            }
            _ => Vec::new(),
        }
    }

    /// Nœud d'une étape : les réglages (niveau, coupure) deviennent des
    /// nœuds « réglage », le reste des nœuds action.
    pub fn for_action(action: Action) -> NodeKind {
        match action {
            Action::Fade {
                side,
                control,
                db,
                secs,
                from,
            } => NodeKind::Control {
                side,
                control,
                level: Some(Level { db, secs, from }),
                cut: None,
            },
            Action::Mute { side, control } | Action::Unmute { side, control } => NodeKind::Control {
                side,
                control,
                level: None,
                cut: Some(matches!(action, Action::Mute { .. })),
            },
            action => NodeKind::Action { action },
        }
    }

    /// Nombre de sorties d'exécution.
    pub fn outputs(&self) -> usize {
        match self {
            NodeKind::Sequence { outputs } => *outputs,
            _ => 1,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub id: u64,
    /// Position du coin haut gauche, en coordonnées du graphe.
    pub pos: [f32; 2],
    pub kind: NodeKind,
}

/// Lien d'exécution : sortie `pin` du nœud `from` → entrée du nœud `to`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Link {
    pub from: u64,
    pub pin: usize,
    pub to: u64,
}

/// Cadre de commentaire : déplacer son titre déplace les nœuds qu'il contient.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Comment {
    pub id: u64,
    /// `[x, y, largeur, hauteur]`.
    pub rect: [f32; 4],
    pub text: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Graph {
    pub nodes: Vec<Node>,
    pub links: Vec<Link>,
    #[serde(default)]
    pub comments: Vec<Comment>,
    #[serde(default)]
    pub next_id: u64,
}

/// Résultat de la compilation.
#[derive(Debug, Default)]
pub struct Compiled {
    pub steps: Vec<Step>,
    /// Nœud action d'où vient chaque étape.
    pub step_nodes: Vec<u64>,
    /// Instants des nœuds atteints, relatifs à leur déclencheur :
    /// nœud → (déclencheur, début, fin) en secondes (fin > début pour un délai).
    pub timing: HashMap<u64, Vec<(usize, f64, f64)>>,
}

impl Compiled {
    /// Vrai si le nœud est atteint depuis un événement.
    pub fn reached(&self, node: u64) -> bool {
        self.timing.contains_key(&node)
    }
}

impl Graph {
    pub fn new_id(&mut self) -> u64 {
        let used = self
            .nodes
            .iter()
            .map(|node| node.id)
            .chain(self.comments.iter().map(|comment| comment.id))
            .max()
            .unwrap_or(0);
        self.next_id = self.next_id.max(used) + 1;
        self.next_id
    }

    pub fn node(&self, id: u64) -> Option<&Node> {
        self.nodes.iter().find(|node| node.id == id)
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn node_mut(&mut self, id: u64) -> Option<&mut Node> {
        self.nodes.iter_mut().find(|node| node.id == id)
    }

    /// Ajoute un nœud et retourne son identifiant.
    pub fn add_node(&mut self, kind: NodeKind, pos: [f32; 2]) -> u64 {
        let id = self.new_id();
        self.nodes.push(Node { id, pos, kind });
        id
    }

    /// Relie la sortie `pin` de `from` à l'entrée de `to` ; une sortie ne
    /// mène qu'à une entrée (l'ancien lien est remplacé). Retourne `false`
    /// si le lien est impossible (même nœud, pas d'entrée, sortie absente).
    pub fn connect(&mut self, from: u64, pin: usize, to: u64) -> bool {
        let valid = from != to
            && self.node(from).is_some_and(|node| pin < node.kind.outputs())
            && self.node(to).is_some_and(|node| node.kind.has_input());
        if !valid {
            return false;
        }
        self.links.retain(|link| !(link.from == from && link.pin == pin));
        self.links.push(Link { from, pin, to });
        true
    }

    /// Supprime des nœuds et leurs liens.
    pub fn remove_nodes(&mut self, ids: &[u64]) {
        self.nodes.retain(|node| !ids.contains(&node.id));
        self.links
            .retain(|link| !ids.contains(&link.from) && !ids.contains(&link.to));
    }

    /// Supprime les liens qui partent de sorties qui n'existent plus (séquence
    /// réduite) ou de nœuds disparus.
    pub fn prune_links(&mut self) {
        let outputs: HashMap<u64, usize> = self.nodes.iter().map(|node| (node.id, node.kind.outputs())).collect();
        let inputs: HashMap<u64, bool> = self.nodes.iter().map(|node| (node.id, node.kind.has_input())).collect();
        self.links.retain(|link| {
            outputs.get(&link.from).is_some_and(|count| link.pin < *count) && inputs.get(&link.to) == Some(&true)
        });
        let mut seen = std::collections::HashSet::new();
        self.links.retain(|link| seen.insert((link.from, link.pin)));
    }

    /// Convertit les anciens nœuds action de réglage (régler, couper,
    /// rétablir) en nœuds « réglage » ; retourne `true` si le graphe a changé.
    pub fn normalize(&mut self) -> bool {
        let mut changed = false;
        for node in &mut self.nodes {
            if let NodeKind::Action { action } = node.kind {
                let kind = NodeKind::for_action(action);
                if kind != node.kind {
                    node.kind = kind;
                    changed = true;
                }
            }
        }
        changed
    }

    /// Lien qui part de la sortie `pin` de `from`.
    pub fn link_from(&self, from: u64, pin: usize) -> Option<&Link> {
        self.links.iter().find(|link| link.from == from && link.pin == pin)
    }

    /// Traduit le graphe en étapes.
    pub fn compile(&self) -> Compiled {
        let mut entries: Vec<Entry> = Vec::new();
        let mut compiled = Compiled::default();
        let mut events: Vec<&Node> = self
            .nodes
            .iter()
            .filter(|node| matches!(node.kind, NodeKind::Event { .. }))
            .collect();
        events.sort_by(|a, b| {
            let trigger = |node: &Node| match node.kind {
                NodeKind::Event { trigger } => trigger,
                _ => 0,
            };
            trigger(a)
                .cmp(&trigger(b))
                .then(a.pos[1].total_cmp(&b.pos[1]))
                .then(a.pos[0].total_cmp(&b.pos[0]))
        });
        let mut order = 0;
        for event in events {
            let NodeKind::Event { trigger } = event.kind else {
                continue;
            };
            compiled.timing.entry(event.id).or_default().push((trigger, 0.0, 0.0));
            let mut path = vec![event.id];
            let mut walk = Walk {
                entries: &mut entries,
                compiled: &mut compiled,
                order: &mut order,
            };
            self.walk(event.id, trigger, 0.0, Timing::default(), &mut path, &mut walk);
        }

        // Par déclencheur (dans l'ordre de première apparition), trié par
        // instant ; délai = écart avec l'étape précédente du groupe.
        // Les étapes à chemin musical n'entrent pas dans l'enchaînement.
        let mut triggers: Vec<usize> = Vec::new();
        for entry in &entries {
            if !triggers.contains(&entry.trigger) {
                triggers.push(entry.trigger);
            }
        }
        for trigger in triggers {
            let mut group: Vec<&Entry> = entries.iter().filter(|entry| entry.trigger == trigger).collect();
            group.sort_by(|a, b| a.at.total_cmp(&b.at).then(a.order.cmp(&b.order)));
            let mut previous = 0.0;
            for entry in group {
                let musical = entry.timing.is_musical();
                let wait = if musical { 0.0 } else { round_ms(entry.at - previous) };
                previous += wait as f64;
                compiled.steps.push(Step {
                    marker: trigger,
                    wait,
                    action: entry.action,
                    timing: if musical {
                        round_timing(entry.timing)
                    } else {
                        Timing::default()
                    },
                });
                compiled.step_nodes.push(entry.node);
            }
        }
        compiled
    }

    /// `at` : instant (estimé) du nœud `from` ; `timing` : chemin de temps
    /// depuis le déclencheur.
    fn walk(&self, from: u64, trigger: usize, at: f64, timing: Timing, path: &mut Vec<u64>, walk: &mut Walk) {
        let Some(node) = self.node(from) else { return };
        for pin in 0..node.kind.outputs() {
            let Some(link) = self.link_from(from, pin) else {
                continue;
            };
            // Boucle : on s'arrête.
            if path.contains(&link.to) {
                continue;
            }
            let Some(next) = self.node(link.to) else { continue };
            let mut next_timing = timing;
            let end = match next.kind {
                NodeKind::Event { .. } => continue,
                NodeKind::Delay { amount, unit } => {
                    let amount = amount.max(0.0);
                    match unit {
                        DelayUnit::Secs => {
                            next_timing.push(TimeOp::Secs { secs: amount });
                            at + amount as f64
                        }
                        DelayUnit::Beats => {
                            next_timing.push(TimeOp::Beats { beats: amount });
                            at + amount as f64 * ESTIMATED_BEAT_SECS
                        }
                    }
                }
                NodeKind::Quantize { beats } => {
                    next_timing.push(TimeOp::Quantize { beats: beats.max(1) });
                    at
                }
                NodeKind::Sequence { .. } => at,
                NodeKind::Action { .. } | NodeKind::Control { .. } | NodeKind::Group { .. } => {
                    for action in next.kind.actions() {
                        walk.entries.push(Entry {
                            trigger,
                            at,
                            order: *walk.order,
                            node: next.id,
                            action,
                            timing,
                        });
                        *walk.order += 1;
                    }
                    at
                }
            };
            walk.compiled
                .timing
                .entry(next.id)
                .or_default()
                .push((trigger, at, end));
            path.push(next.id);
            self.walk(next.id, trigger, end, next_timing, path, walk);
            path.pop();
        }
    }

    /// Construit un graphe à partir d'étapes : une ligne par déclencheur
    /// (événement → [délai →] action → …), disposée de gauche à droite.
    pub fn from_steps(transition: &Transition, positions: &Triggers) -> Graph {
        let mut graph = Graph::default();
        let mut row = 0.0;
        let order = transition.ordered_steps(positions);
        let mut triggers: Vec<usize> = Vec::new();
        for &index in &order {
            let marker = transition.steps[index].marker;
            if !triggers.contains(&marker) {
                triggers.push(marker);
            }
        }
        for trigger in triggers {
            let steps: Vec<Step> = order
                .iter()
                .map(|&index| transition.steps[index])
                .filter(|step| step.marker == trigger)
                .collect();
            // Ligne principale : les étapes enchaînées par leurs délais.
            let chained: Vec<&Step> = steps.iter().filter(|step| step.timing.is_empty()).collect();
            if !chained.is_empty() {
                let event = graph.add_node(NodeKind::Event { trigger }, [0.0, row]);
                let mut previous = event;
                let mut x = 320.0;
                for step in chained {
                    if step.wait > 0.0 {
                        let delay = graph.add_node(
                            NodeKind::Delay {
                                amount: step.wait,
                                unit: DelayUnit::Secs,
                            },
                            [x, row],
                        );
                        graph.connect(previous, 0, delay);
                        previous = delay;
                        x += 256.0;
                    }
                    let action = graph.add_node(NodeKind::for_action(step.action), [x, row]);
                    graph.connect(previous, 0, action);
                    previous = action;
                    x += 336.0;
                }
                row += 256.0;
            }
            // Une ligne par chemin de temps musical : événement → attentes →
            // actions de ce chemin.
            let mut timed = steps.iter().filter(|step| !step.timing.is_empty()).peekable();
            while let Some(first) = timed.next() {
                let mut actions = vec![first.action];
                while let Some(step) = timed.next_if(|step| step.timing == first.timing) {
                    actions.push(step.action);
                }
                let event = graph.add_node(NodeKind::Event { trigger }, [0.0, row]);
                let mut previous = event;
                let mut x = 320.0;
                for op in first.timing.ops() {
                    let kind = match op {
                        TimeOp::Secs { secs } => NodeKind::Delay {
                            amount: secs,
                            unit: DelayUnit::Secs,
                        },
                        TimeOp::Beats { beats } => NodeKind::Delay {
                            amount: beats,
                            unit: DelayUnit::Beats,
                        },
                        TimeOp::Quantize { beats } => NodeKind::Quantize { beats },
                    };
                    let node = graph.add_node(kind, [x, row]);
                    graph.connect(previous, 0, node);
                    previous = node;
                    x += 256.0;
                }
                for action in actions {
                    let node = graph.add_node(NodeKind::for_action(action), [x, row]);
                    graph.connect(previous, 0, node);
                    previous = node;
                    x += 336.0;
                }
                row += 256.0;
            }
        }
        graph
    }
}

/// Action atteinte pendant la compilation.
struct Entry {
    trigger: usize,
    /// Instant (estimé pour un chemin musical), en secondes.
    at: f64,
    /// Ordre de parcours (départage les actions simultanées).
    order: usize,
    node: u64,
    action: Action,
    timing: Timing,
}

/// État du parcours de la compilation.
struct Walk<'a> {
    entries: &'a mut Vec<Entry>,
    compiled: &'a mut Compiled,
    order: &'a mut usize,
}

/// Délais arrondis à la milliseconde : la conversion graphe → étapes →
/// graphe reste stable.
fn round_ms(secs: f64) -> f32 {
    ((secs.max(0.0) * 1000.0).round() / 1000.0) as f32
}

/// Chemin de temps aux durées arrondies (même raison).
fn round_timing(timing: Timing) -> Timing {
    let ops: Vec<TimeOp> = timing
        .ops()
        .map(|op| match op {
            TimeOp::Secs { secs } => TimeOp::Secs {
                secs: round_ms(secs as f64),
            },
            TimeOp::Beats { beats } => TimeOp::Beats {
                beats: (beats * 1000.0).round() / 1000.0,
            },
            op => op,
        })
        .collect();
    Timing::from(ops)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markers::NUM_TRIGGERS;
    use crate::transitions::{Control, MANUAL_FIRST, Side, templates};

    fn transition(steps: Vec<Step>) -> Transition {
        Transition {
            id: 1,
            name: String::new(),
            from: "a".into(),
            to: "b".into(),
            from_title: String::new(),
            to_title: String::new(),
            steps,
            graph: None,
        }
    }

    #[test]
    fn steps_round_trip_through_the_graph() {
        let positions = [None; NUM_TRIGGERS];
        for template in templates() {
            let source = transition(template.steps.clone());
            let graph = Graph::from_steps(&source, &positions);
            let compiled = graph.compile();
            // Même contenu par déclencheur, dans le même ordre.
            for trigger in 0..crate::transitions::ALL_TRIGGERS {
                let group = |steps: &[Step]| -> Vec<Step> {
                    steps.iter().copied().filter(|step| step.marker == trigger).collect()
                };
                assert_eq!(group(&compiled.steps), group(&source.steps), "{}", template.name);
            }
            // Stable : recompiler le graphe reconstruit donne la même chose.
            let again = Graph::from_steps(&transition(compiled.steps.clone()), &positions).compile();
            assert_eq!(again.steps, compiled.steps, "{}", template.name);
        }
    }

    #[test]
    fn control_node_can_restore_and_set_together() {
        let mut graph = Graph::default();
        let event = graph.add_node(NodeKind::Event { trigger: 0 }, [0.0, 0.0]);
        let voice = graph.add_node(
            NodeKind::Control {
                side: Side::To,
                control: Control::Vocals,
                level: Some(Level {
                    db: -6.0,
                    secs: 2.0,
                    from: Some(-40.0),
                }),
                cut: Some(false),
            },
            [0.0, 0.0],
        );
        let empty = graph.add_node(
            NodeKind::Control {
                side: Side::From,
                control: Control::High,
                level: None,
                cut: None,
            },
            [0.0, 0.0],
        );
        graph.connect(event, 0, voice);
        graph.connect(voice, 0, empty);
        let compiled = graph.compile();
        assert_eq!(
            compiled.steps.iter().map(|step| step.action).collect::<Vec<_>>(),
            vec![
                Action::Unmute {
                    side: Side::To,
                    control: Control::Vocals
                },
                Action::Fade {
                    side: Side::To,
                    control: Control::Vocals,
                    db: -6.0,
                    secs: 2.0,
                    from: Some(-40.0),
                },
            ]
        );
        assert_eq!(compiled.step_nodes, vec![voice, voice]);

        // Anciens nœuds « Régler » : convertis sans changer les étapes.
        let mut old = Graph::default();
        let event = old.add_node(NodeKind::Event { trigger: 0 }, [0.0, 0.0]);
        let fade = old.add_node(
            NodeKind::Action {
                action: Action::Fade {
                    side: Side::From,
                    control: Control::Low,
                    db: -10.0,
                    secs: 1.0,
                    from: None,
                },
            },
            [0.0, 0.0],
        );
        old.connect(event, 0, fade);
        let before = old.compile().steps;
        assert!(old.normalize());
        assert!(matches!(old.node(fade).unwrap().kind, NodeKind::Control { .. }));
        assert_eq!(old.compile().steps, before);
    }

    #[test]
    fn group_node_gives_one_step_per_setting() {
        let mut graph = Graph::default();
        let event = graph.add_node(NodeKind::Event { trigger: 1 }, [0.0, 0.0]);
        let stems = graph.add_node(
            NodeKind::Group {
                side: Side::From,
                group: Group::Stems,
                levels: [None, Some(-6.0), None, None],
                cuts: [Some(true), None, Some(false), None],
                secs: 3.0,
                starts: [None; 4],
            },
            [0.0, 0.0],
        );
        graph.connect(event, 0, stems);
        let actions: Vec<Action> = graph.compile().steps.iter().map(|step| step.action).collect();
        assert_eq!(
            actions,
            vec![
                Action::Unmute {
                    side: Side::From,
                    control: Control::Bass
                },
                Action::Fade {
                    side: Side::From,
                    control: Control::Drums,
                    db: -6.0,
                    secs: 3.0,
                    from: None,
                },
                Action::Mute {
                    side: Side::From,
                    control: Control::Vocals
                },
            ]
        );
        // Les EQ n'ont que trois réglages : la quatrième case est ignorée.
        let eq = NodeKind::Group {
            side: Side::To,
            group: Group::Eq,
            levels: [None, None, Some(-40.0), Some(0.0)],
            cuts: [None; 4],
            secs: 1.0,
            starts: [None; 4],
        };
        assert_eq!(eq.actions().len(), 1);
        // Ancien fichier sans `levels` / `cuts` / `secs`.
        let old: NodeKind = serde_json::from_str(r#"{"type":"Group","side":"To","group":"Eq"}"#).unwrap();
        assert!(old.actions().is_empty());
    }

    #[test]
    fn delays_and_sequences_give_absolute_times() {
        let mut graph = Graph::default();
        let play = |side| NodeKind::Action {
            action: Action::Play { side },
        };
        let event = graph.add_node(NodeKind::Event { trigger: MANUAL_FIRST }, [0.0, 0.0]);
        let sequence = graph.add_node(NodeKind::Sequence { outputs: 2 }, [0.0, 0.0]);
        let secs = |amount| NodeKind::Delay {
            amount,
            unit: DelayUnit::Secs,
        };
        let delay_a = graph.add_node(secs(4.0), [0.0, 0.0]);
        let a = graph.add_node(play(Side::To), [0.0, 0.0]);
        let delay_b = graph.add_node(secs(1.5), [0.0, 0.0]);
        let b = graph.add_node(
            NodeKind::Action {
                action: Action::Mute {
                    side: Side::From,
                    control: Control::Vocals,
                },
            },
            [0.0, 0.0],
        );
        let unreached = graph.add_node(play(Side::From), [0.0, 0.0]);
        assert!(graph.connect(event, 0, sequence));
        assert!(graph.connect(sequence, 0, delay_a));
        assert!(graph.connect(delay_a, 0, a));
        assert!(graph.connect(sequence, 1, delay_b));
        assert!(graph.connect(delay_b, 0, b));
        // Boucle et liens impossibles.
        assert!(graph.connect(b, 0, sequence));
        assert!(!graph.connect(a, 0, event));
        assert!(!graph.connect(a, 0, a));

        let compiled = graph.compile();
        // b (1,5 s) avant a (4 s) : délais 1,5 puis 2,5.
        assert_eq!(compiled.steps.len(), 2);
        assert_eq!(compiled.step_nodes, vec![b, a]);
        assert_eq!((compiled.steps[0].wait, compiled.steps[1].wait), (1.5, 2.5));
        assert!(compiled.steps.iter().all(|step| step.marker == MANUAL_FIRST));
        assert!(!compiled.reached(unreached));
        assert_eq!(compiled.timing[&delay_a], vec![(MANUAL_FIRST, 0.0, 4.0)]);

        // Une sortie ne mène qu'à une entrée : relier à nouveau remplace.
        assert!(graph.connect(sequence, 0, unreached));
        assert_eq!(
            graph
                .links
                .iter()
                .filter(|link| link.from == sequence && link.pin == 0)
                .count(),
            1
        );
        // Séquence réduite : le lien de la sortie disparue est retiré.
        graph.node_mut(sequence).unwrap().kind = NodeKind::Sequence { outputs: 1 };
        graph.prune_links();
        assert!(graph.link_from(sequence, 1).is_none());
    }

    #[test]
    fn beat_delays_and_quantize_keep_a_musical_timing() {
        let mut graph = Graph::default();
        let event = graph.add_node(NodeKind::Event { trigger: 2 }, [0.0, 0.0]);
        let play = graph.add_node(
            NodeKind::Action {
                action: Action::Play { side: Side::To },
            },
            [0.0, 0.0],
        );
        let bar = graph.add_node(NodeKind::Quantize { beats: 4 }, [0.0, 0.0]);
        let wait = graph.add_node(
            NodeKind::Delay {
                amount: 8.0,
                unit: DelayUnit::Beats,
            },
            [0.0, 0.0],
        );
        let stop = graph.add_node(
            NodeKind::Action {
                action: Action::Stop { side: Side::From },
            },
            [0.0, 0.0],
        );
        assert!(graph.connect(event, 0, play));
        assert!(graph.connect(play, 0, bar));
        assert!(graph.connect(bar, 0, wait));
        assert!(graph.connect(wait, 0, stop));
        let compiled = graph.compile();
        assert_eq!(compiled.steps.len(), 2);
        assert!(compiled.steps[0].timing.is_empty());
        let ops: Vec<TimeOp> = compiled.steps[1].timing.into();
        assert_eq!(ops, vec![TimeOp::Quantize { beats: 4 }, TimeOp::Beats { beats: 8.0 }]);
        assert_eq!(compiled.steps[1].wait, 0.0);
        // Reconstruit depuis les étapes : même résultat.
        let rebuilt = Graph::from_steps(&transition(compiled.steps.clone()), &[None; NUM_TRIGGERS]);
        assert_eq!(rebuilt.compile().steps, compiled.steps);
        // Ancien délai en secondes (« secs ») : relu tel quel.
        let old: NodeKind = serde_json::from_str(r#"{"type":"Delay","secs":2.5}"#).unwrap();
        assert_eq!(
            old,
            NodeKind::Delay {
                amount: 2.5,
                unit: DelayUnit::Secs
            }
        );
        // Les étapes sans chemin musical s'écrivent comme avant.
        let json = serde_json::to_string(&compiled.steps[0]).unwrap();
        assert!(!json.contains("timing"), "{json}");
    }
}
