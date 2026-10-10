//! Script animation trees on entities: `useanimtree`, the `setanim` family,
//! notetrack notifies and `animscripted` root motion.
//!
//! A tree is the compiled `animtrees/<name>.atr` (only the animations scripts
//! name, like the script loader keeps). Every node has an `XAnimNodeState`;
//! clips are resolved on first use. The authority advances trees once per tick
//! and publishes the active nodes as the entity's semantic `XAnimTreeSnapshot`.
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use bevy_ecs::prelude::World;
use xmodel_runtime::{
    AnimClip, XAnimNodeId, XAnimNodeState, XAnimSemanticNode, XAnimSemanticNodeKind,
    XAnimTreeSnapshot,
};

use super::args::{float, vector};
use super::arrays::new_array;
use crate::script::{Namespace, NativeRegistry, Runtime, Value};

const ANIMFLAG_ADDITIVE: u16 = anim_iw4::ANIMFLAG_ADDITIVE;
/// Blend time when a call leaves it out.
const DEFAULT_BLEND: f32 = 0.2;

#[derive(Clone, Debug)]
pub struct AnimTreeNode {
    pub name: Arc<str>,
    pub parent: Option<u16>,
    pub flags: u16,
    pub first_child: u16,
    pub child_count: u16,
}

/// One compiled `animtrees/<name>.atr`; parents come before their children.
#[derive(Debug)]
pub struct AnimTree {
    pub nodes: Vec<AnimTreeNode>,
    by_name: HashMap<Arc<str>, u16>,
}

impl AnimTree {
    pub fn new(nodes: Vec<AnimTreeNode>) -> Self {
        let by_name = nodes
            .iter()
            .enumerate()
            .map(|(index, node)| (node.name.clone(), index as u16))
            .collect();
        Self { nodes, by_name }
    }

    pub fn index(&self, name: &str) -> Option<u16> {
        self.by_name.get(name).copied()
    }

    fn children(&self, node: u16) -> std::ops::Range<u16> {
        let node = &self.nodes[node as usize];
        node.first_child..node.first_child + node.child_count
    }

    fn is_leaf(&self, node: u16) -> bool {
        self.nodes[node as usize].child_count == 0
    }

    /// `node` and every node under it.
    fn subtree(&self, node: u16, out: &mut Vec<u16>) {
        out.push(node);
        for child in self.children(node) {
            self.subtree(child, out);
        }
    }
}

pub type ClipSource = Arc<dyn Fn(&str) -> Option<Arc<AnimClip>> + Send + Sync>;

/// Every script tree of the level and where their clips come from.
#[derive(Clone, Default)]
pub struct AnimLibrary {
    pub trees: BTreeMap<String, Arc<AnimTree>>,
    pub clips: Option<ClipSource>,
}

impl std::fmt::Debug for AnimLibrary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnimLibrary")
            .field("trees", &self.trees.keys().collect::<Vec<_>>())
            .field("clips", &self.clips.is_some())
            .finish()
    }
}

impl AnimLibrary {
    pub(crate) fn clip(&self, name: &str) -> Option<Arc<AnimClip>> {
        self.clips.as_ref()?(name)
    }
}

/// `animscripted`: the entity follows the clip's root motion from `origin`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Scripted {
    pub node: u16,
    pub origin: [f32; 3],
    pub angles: [f32; 3],
}

#[derive(Clone)]
pub(crate) struct EntityAnim {
    pub tree: Arc<AnimTree>,
    pub tree_name: Arc<str>,
    states: Vec<XAnimNodeState>,
    notify: Vec<Option<Arc<str>>>,
    clips: Vec<Option<Option<Arc<AnimClip>>>>,
    pub scripted: Option<Scripted>,
    pub revision: u32,
}

impl std::fmt::Debug for EntityAnim {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EntityAnim")
            .field("tree", &self.tree_name)
            .field("revision", &self.revision)
            .finish()
    }
}

impl PartialEq for EntityAnim {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.tree, &other.tree) && self.revision == other.revision
    }
}

/// How a `setanim` call places the node among its siblings and parents.
#[derive(Clone, Copy, Default)]
pub(crate) struct SetMode {
    /// Fade the siblings out.
    pub knob: bool,
    /// Leave the parents' weights alone.
    pub limited: bool,
    pub restart: bool,
    /// Knob every ancestor below this node too.
    pub knob_all_to: Option<u16>,
}

impl EntityAnim {
    pub(crate) fn new(tree_name: Arc<str>, tree: Arc<AnimTree>) -> Self {
        let n = tree.nodes.len();
        let mut states = vec![XAnimNodeState::default(); n];
        // The root is always on.
        states[0].weight = 1.0;
        states[0].goal_weight = 1.0;
        Self {
            tree,
            tree_name,
            states,
            notify: vec![None; n],
            clips: vec![None; n],
            scripted: None,
            revision: 0,
        }
    }

    fn clip(&mut self, library: &AnimLibrary, node: u16) -> Option<Arc<AnimClip>> {
        if !self.tree.is_leaf(node) {
            return None;
        }
        let tree = &self.tree_name;
        self.clips[node as usize]
            .get_or_insert_with(|| {
                let name = &self.tree.nodes[node as usize].name;
                let clip = library.clip(name);
                let found = if clip.is_some() { "bound" } else { "missing" };
                diag::info!(Sim, "gsc: script anim {tree}/{name}: clip {found}");
                clip
            })
            .clone()
    }

    fn restart(&mut self, node: u16) {
        let mut nodes = Vec::new();
        self.tree.subtree(node, &mut nodes);
        for node in nodes {
            let state = &mut self.states[node as usize];
            state.time = 0.0;
            state.old_time = 0.0;
            state.cycle_count = 0;
            state.old_cycle_count = 0;
        }
    }

    fn set_goal(&mut self, node: u16, weight: f32, blend: f32) {
        let state = &mut self.states[node as usize];
        state.goal_weight = weight;
        state.goal_time = blend;
        if blend <= 0.0 {
            state.weight = weight;
        }
    }

    fn knob_siblings(&mut self, node: u16, blend: f32) {
        let Some(parent) = self.tree.nodes[node as usize].parent else {
            return;
        };
        for sibling in self.tree.children(parent) {
            if sibling != node {
                self.clear(sibling, blend);
            }
        }
    }

    /// `clearanim`: the node and everything under it fade to zero.
    pub(crate) fn clear(&mut self, node: u16, blend: f32) {
        let mut nodes = Vec::new();
        self.tree.subtree(node, &mut nodes);
        for node in nodes {
            if self.states[node as usize].goal_weight != 0.0 {
                self.set_goal(node, 0.0, blend);
            }
        }
        self.revision = self.revision.wrapping_add(1);
    }

    pub(crate) fn set(
        &mut self,
        node: u16,
        weight: f32,
        blend: f32,
        rate: f32,
        mode: SetMode,
        notify: Option<Arc<str>>,
    ) {
        let off = {
            let state = &self.states[node as usize];
            state.weight == 0.0 && state.goal_weight == 0.0
        };
        if off || mode.restart {
            self.restart(node);
        }
        self.set_goal(node, weight, blend);
        self.states[node as usize].rate = rate;
        if notify.is_some() {
            self.notify[node as usize] = notify;
        }
        if mode.knob {
            self.knob_siblings(node, blend);
        }
        let mut current = self.tree.nodes[node as usize].parent;
        while let Some(parent) = current {
            if let Some(root) = mode.knob_all_to {
                if parent == root {
                    break;
                }
                self.knob_siblings(parent, blend);
                self.set_goal(parent, 1.0, blend);
            } else if !mode.limited && self.states[parent as usize].goal_weight == 0.0 {
                // A parent that was off comes on at full weight at once.
                if self.states[parent as usize].weight == 0.0 {
                    self.restart(parent);
                }
                self.set_goal(parent, 1.0, 0.0);
            }
            current = self.tree.nodes[parent as usize].parent;
        }
        self.revision = self.revision.wrapping_add(1);
    }

    pub(crate) fn time(&self, node: u16) -> f32 {
        self.states[node as usize].time
    }

    pub(crate) fn set_time(&mut self, node: u16, time: f32) {
        let state = &mut self.states[node as usize];
        state.time = time;
        state.old_time = time;
        self.revision = self.revision.wrapping_add(1);
    }

    pub(crate) fn weight(&self, node: u16) -> f32 {
        self.states[node as usize].weight
    }

    /// One step: weights blend, leaves advance, crossed notetracks return as
    /// `(flag, notetrack)` in time order.
    pub(crate) fn update(&mut self, library: &AnimLibrary, dt: f32) -> Vec<(Arc<str>, Arc<str>)> {
        let n = self.states.len();
        for node in 0..n {
            let parent_has_weight = self.tree.nodes[node]
                .parent
                .is_none_or(|parent| self.states[parent as usize].weight != 0.0);
            let state = &mut self.states[node];
            let (weight, goal_time) = anim_iw4::advance_goal_weight(
                state.weight,
                state.goal_weight,
                state.goal_time,
                dt,
                parent_has_weight,
            );
            state.weight = weight;
            state.goal_time = goal_time;
            state.old_time = state.time;
            state.old_cycle_count = state.cycle_count;
            if weight == 0.0 && state.goal_weight == 0.0 {
                self.notify[node] = None;
            }
        }
        let mut notes: Vec<(f32, Arc<str>, Arc<str>)> = Vec::new();
        for node in 0..n as u16 {
            if self.states[node as usize].weight == 0.0 || !self.tree.is_leaf(node) {
                continue;
            }
            let Some(clip) = self.clip(library, node) else {
                continue;
            };
            let state = &mut self.states[node as usize];
            let (time, cycle) = anim_iw4::advance_leaf_time(
                state.old_time,
                state.cycle_count,
                state.rate,
                clip.frequency(),
                dt,
                clip.looping,
            );
            state.time = time;
            state.cycle_count = cycle;
            let old = state.old_time;
            let wrapped = cycle != state.old_cycle_count;
            if let Some(flag) = self.flag_of(node) {
                crossed_notes(&clip, old, time, wrapped, |at, note| {
                    notes.push((at, flag.clone(), note));
                });
            }
        }
        if !notes.is_empty() || n > 0 {
            self.revision = self.revision.wrapping_add(1);
        }
        notes.sort_by(|a, b| a.0.total_cmp(&b.0));
        notes.into_iter().map(|(_, flag, note)| (flag, note)).collect()
    }

    /// The nearest notify flag on the node or above it.
    fn flag_of(&self, node: u16) -> Option<Arc<str>> {
        let mut current = Some(node);
        while let Some(at) = current {
            if let Some(flag) = &self.notify[at as usize] {
                return Some(flag.clone());
            }
            current = self.tree.nodes[at as usize].parent;
        }
        None
    }

    /// The nodes with weight, with every parent kept so indices stay ordered.
    pub(crate) fn snapshot(&self) -> Option<XAnimTreeSnapshot> {
        let n = self.states.len();
        let mut keep = vec![false; n];
        for node in (0..n).rev() {
            let state = &self.states[node];
            let live = state.weight > 0.0 || state.goal_weight > 0.0;
            if live && (node == 0 || !self.tree.is_leaf(node as u16) || self.has_clip(node)) {
                let mut current = Some(node as u16);
                while let Some(at) = current {
                    if std::mem::replace(&mut keep[at as usize], true) {
                        break;
                    }
                    current = self.tree.nodes[at as usize].parent;
                }
            }
        }
        let mut remap = vec![u16::MAX; n];
        let mut nodes = Vec::new();
        for node in 0..n {
            if !keep[node] {
                continue;
            }
            let def = &self.tree.nodes[node];
            remap[node] = nodes.len() as u16;
            let leaf = self.tree.is_leaf(node as u16);
            nodes.push(XAnimSemanticNode {
                parent: def.parent.map(|p| XAnimNodeId(remap[p as usize])),
                kind: if leaf {
                    XAnimSemanticNodeKind::Leaf
                } else if def.flags & ANIMFLAG_ADDITIVE != 0 {
                    XAnimSemanticNodeKind::Additive
                } else {
                    XAnimSemanticNodeKind::Blend
                },
                clip: leaf.then(|| def.name.to_string()),
                parts: None,
                state: self.states[node],
            });
        }
        // A tree of blend nodes only poses nothing; the bind pose says so plainly.
        nodes
            .iter()
            .any(|node| node.kind == XAnimSemanticNodeKind::Leaf)
            .then_some(XAnimTreeSnapshot {
                definition_revision: 1,
                state_revision: self.revision,
                nodes,
            })
    }

    fn has_clip(&self, node: usize) -> bool {
        matches!(self.clips[node], Some(Some(_)))
    }
}

/// Notetracks the clip crosses going from `old` to `new` (normalized time).
/// Every pass of the clip's end is an `end`, looping or not.
fn crossed_notes(
    clip: &AnimClip,
    old: f32,
    new: f32,
    wrapped: bool,
    mut emit: impl FnMut(f32, Arc<str>),
) {
    // (from, to]; a notetrack at 0 fires as the clip leaves its first frame.
    let mut visit = |from: f32, to: f32, base: f32| {
        if to <= from {
            return;
        }
        for note in &clip.notifies {
            let at = note.time;
            if (at > from || (at == 0.0 && from == 0.0)) && at <= to {
                emit(base + at, note.name.as_str().into());
            }
        }
    };
    if wrapped {
        visit(old, 1.0, 0.0);
        visit(0.0, new, 1.0);
    } else {
        visit(old, new, 0.0);
    }
    let has_end = clip.notifies.iter().any(|note| note.name == "end");
    if !has_end && (wrapped || (new >= 1.0 && old < 1.0)) {
        emit(1.0, "end".into());
    }
}

fn library(world: &World) -> Arc<AnimLibrary> {
    world.resource::<Runtime>().anim_library.clone()
}

fn entity_id(world: &World, receiver: &Value) -> Result<u64, String> {
    match receiver {
        Value::Object(id) if world.resource::<Runtime>().entities.contains_key(id) => Ok(*id),
        _ => Err("receiver is not an entity".into()),
    }
}

fn anim_name(args: &[Value], index: usize) -> Result<Arc<str>, String> {
    match args.get(index) {
        Some(Value::Animation { name, .. }) => Ok(name.clone()),
        Some(other) => Err(format!(
            "argument {} is {}, not an animation",
            index + 1,
            super::args::kind(other)
        )),
        None => Err(format!("argument {} is missing", index + 1)),
    }
}

fn anim_value(args: &[Value], index: usize) -> Result<(Arc<str>, Arc<str>), String> {
    match args.get(index) {
        Some(Value::Animation { tree, name }) => Ok((tree.clone(), name.clone())),
        _ => anim_name(args, index).map(|name| ("".into(), name)),
    }
}

fn opt_float(args: &[Value], index: usize, default: f32) -> Result<f32, String> {
    match args.get(index) {
        None | Some(Value::Undefined) => Ok(default),
        Some(_) => float(args, index),
    }
}

/// The entity's tree, made from the animation's tree if it has none yet
/// (`animscripted` on a model that never called `useanimtree`).
fn with_anim<T>(
    world: &mut World,
    receiver: &Value,
    anim_tree: Option<&str>,
    change: impl FnOnce(&mut EntityAnim) -> Result<T, String>,
) -> Result<T, String> {
    let id = entity_id(world, receiver)?;
    let library = library(world);
    let mut runtime = world.resource_mut::<Runtime>();
    let entity = runtime.entities.get_mut(&id).unwrap();
    if entity.anim.is_none() {
        let tree_name = anim_tree
            .filter(|name| !name.is_empty())
            .ok_or("entity has no animtree; call useanimtree first")?;
        let tree = library
            .trees
            .get(tree_name)
            .ok_or_else(|| format!("animtree {tree_name} is not loaded"))?;
        entity.anim = Some(Box::new(EntityAnim::new(tree_name.into(), tree.clone())));
    }
    change(entity.anim.as_mut().unwrap())
}

fn node_of(anim: &EntityAnim, name: &str) -> Result<u16, String> {
    anim.tree
        .index(name)
        .ok_or_else(|| format!("animation {name} is not in animtree {}", anim.tree_name))
}

fn set_anim(
    world: &mut World,
    receiver: &Value,
    args: &[Value],
    flagged: bool,
    mode: SetMode,
    with_root: bool,
) -> Result<Value, String> {
    let mut at = 0;
    let notify: Option<Arc<str>> = if flagged {
        at = 1;
        Some(super::args::string(args, 0)?.into())
    } else {
        None
    };
    let (tree, anim) = anim_value(args, at)?;
    let root = if with_root {
        Some(anim_name(args, at + 1)?)
    } else {
        None
    };
    let rest = at + 1 + usize::from(with_root);
    let weight = opt_float(args, rest, 1.0)?;
    let blend = opt_float(args, rest + 1, DEFAULT_BLEND)?;
    let rate = opt_float(args, rest + 2, 1.0)?;
    let library = library(world);
    with_anim(world, receiver, Some(&tree), |state| {
        let node = node_of(state, &anim)?;
        let mut mode = mode;
        if let Some(root) = &root {
            mode.knob_all_to = Some(node_of(state, root)?);
        }
        state.clip(&library, node);
        state.set(node, weight, blend, rate, mode, notify);
        Ok(Value::Undefined)
    })
}

/// `animscripted` / `startscriptedanim`: knob the clip in under `root`,
/// restart it and follow its root motion from `origin`/`angles`.
pub(crate) fn start_scripted(
    world: &mut World,
    receiver: &Value,
    notify: Arc<str>,
    origin: [f32; 3],
    angles: [f32; 3],
    (tree, anim): (Arc<str>, Arc<str>),
    root: Option<Arc<str>>,
) -> Result<(), String> {
    let library = library(world);
    let id = entity_id(world, receiver)?;
    let (start_origin, start_angles) = with_anim(world, receiver, Some(&tree), |state| {
        let node = node_of(state, &anim)?;
        let root = match &root {
            Some(root) => node_of(state, root)?,
            None => 0,
        };
        let clip = state.clip(&library, node);
        state.clear(root, DEFAULT_BLEND);
        state.set(
            node,
            1.0,
            DEFAULT_BLEND,
            1.0,
            SetMode {
                knob: true,
                restart: true,
                knob_all_to: Some(root),
                ..SetMode::default()
            },
            Some(notify),
        );
        state.scripted = Some(Scripted {
            node,
            origin,
            angles,
        });
        Ok(match clip {
            Some(clip) => root_pose(&clip, origin, angles, 0.0),
            None => (origin, angles),
        })
    })?;
    let mut runtime = world.resource_mut::<Runtime>();
    runtime.set_object_field(id, "origin", Value::Vector(start_origin));
    runtime.set_object_field(id, "angles", Value::Vector(start_angles));
    Ok(())
}

fn yaw_of(q: [f32; 4]) -> f32 {
    let [x, y, z, w] = q;
    (2.0 * (w * z + x * y)).atan2(1.0 - 2.0 * (y * y + z * z)).to_degrees()
}

fn rotate(angles: [f32; 3], v: [f32; 3]) -> [f32; 3] {
    let [pitch, yaw, roll] = angles.map(f32::to_radians);
    let rotation = glam::Quat::from_euler(glam::EulerRot::ZYX, yaw, pitch, roll);
    (rotation * glam::Vec3::from(v)).to_array()
}

/// Where a clip played at `origin`/`angles` puts its root at `frac`.
pub(crate) fn root_pose(
    clip: &AnimClip,
    origin: [f32; 3],
    angles: [f32; 3],
    frac: f32,
) -> ([f32; 3], [f32; 3]) {
    let offset = rotate(angles, clip.abs_delta_trans(frac));
    let yaw = yaw_of(clip.abs_delta_quat(frac));
    (
        [
            origin[0] + offset[0],
            origin[1] + offset[1],
            origin[2] + offset[2],
        ],
        [angles[0], math_iw4::angle_normalize_360(angles[1] + yaw), angles[2]],
    )
}

/// Once per authority tick, before threads run: blend, advance, notify,
/// and move `animscripted` entities along their clip.
pub(crate) fn advance(world: &mut World) {
    let library = library(world);
    if library.trees.is_empty() {
        return;
    }
    let dt = crate::MATCH_TICK_MS as f32 / 1000.0;
    let mut runtime = world.resource_mut::<Runtime>();
    let ids: Vec<u64> = runtime
        .entities
        .iter()
        .filter(|(_, entity)| entity.anim.is_some())
        .map(|(id, _)| *id)
        .collect();
    for id in ids {
        let entity = runtime.entities.get_mut(&id).unwrap();
        let anim = entity.anim.as_mut().unwrap();
        let notes = anim.update(&library, dt);
        let pose = anim.scripted.clone().and_then(|scripted| {
            let clip = anim.clip(&library, scripted.node)?;
            Some(root_pose(
                &clip,
                scripted.origin,
                scripted.angles,
                anim.time(scripted.node),
            ))
        });
        if let Some((origin, angles)) = pose {
            runtime.set_object_field(id, "origin", Value::Vector(origin));
            runtime.set_object_field(id, "angles", Value::Vector(angles));
        }
        for (flag, note) in notes {
            runtime
                .pending_notifies
                .push((Value::Object(id), flag, vec![Value::string(&note)]));
        }
    }
}

fn clip_arg(world: &World, args: &[Value], index: usize) -> Result<Arc<AnimClip>, String> {
    let name = anim_name(args, index)?;
    library(world)
        .clip(&name)
        .ok_or_else(|| format!("animation {name} is not loaded"))
}

pub(crate) fn register(registry: &mut NativeRegistry) {
    use Namespace::{Function, Method};
    registry.register(Method, "useanimtree", |world, receiver, args| {
        let tree: Arc<str> = match args.first() {
            Some(Value::AnimationTree(tree)) => tree.clone(),
            _ => return Err("useanimtree expects #animtree".into()),
        };
        let library = library(world);
        let loaded = library
            .trees
            .get(&*tree)
            .ok_or_else(|| format!("animtree {tree} is not loaded"))?
            .clone();
        let id = entity_id(world, receiver)?;
        let mut runtime = world.resource_mut::<Runtime>();
        let entity = runtime.entities.get_mut(&id).unwrap();
        if !entity
            .anim
            .as_ref()
            .is_some_and(|anim| Arc::ptr_eq(&anim.tree, &loaded))
        {
            entity.anim = Some(Box::new(EntityAnim::new(tree, loaded)));
        }
        Ok(Value::Undefined)
    });
    macro_rules! setters {
        ($($name:literal => $flagged:expr, $knob:expr, $limited:expr, $restart:expr, $root:expr;)*) => {
            $(registry.register(Method, $name, |world, receiver, args| {
                set_anim(world, receiver, args, $flagged, SetMode {
                    knob: $knob,
                    limited: $limited,
                    restart: $restart,
                    knob_all_to: None,
                }, $root)
            });)*
        };
    }
    setters! {
        "setanim" => false, false, false, false, false;
        "setanimknob" => false, true, false, false, false;
        "setanimlimited" => false, false, true, false, false;
        "setanimknoblimited" => false, true, true, false, false;
        "setanimrestart" => false, false, false, true, false;
        "setanimknobrestart" => false, true, false, true, false;
        "setanimlimitedrestart" => false, false, true, true, false;
        "setanimknoblimitedrestart" => false, true, true, true, false;
        "setanimknoball" => false, true, false, false, true;
        "setanimknoballrestart" => false, true, false, true, true;
        "setflaggedanim" => true, false, false, false, false;
        "setflaggedanimknob" => true, true, false, false, false;
        "setflaggedanimlimited" => true, false, true, false, false;
        "setflaggedanimknoblimited" => true, true, true, false, false;
        "setflaggedanimrestart" => true, false, false, true, false;
        "setflaggedanimknobrestart" => true, true, false, true, false;
        "setflaggedanimlimitedrestart" => true, false, true, true, false;
        "setflaggedanimknoblimitedrestart" => true, true, true, true, false;
        "setflaggedanimknoball" => true, true, false, false, true;
        "setflaggedanimknoballrestart" => true, true, false, true, true;
    }
    registry.register(Method, "clearanim", |world, receiver, args| {
        let (tree, anim) = anim_value(args, 0)?;
        let blend = opt_float(args, 1, DEFAULT_BLEND)?;
        with_anim(world, receiver, Some(&tree), |state| {
            let node = node_of(state, &anim)?;
            state.clear(node, blend);
            Ok(Value::Undefined)
        })
    });
    registry.register(Method, "getanimtime", |world, receiver, args| {
        let (tree, anim) = anim_value(args, 0)?;
        with_anim(world, receiver, Some(&tree), |state| {
            Ok(Value::Float(state.time(node_of(state, &anim)?)))
        })
    });
    registry.register(Method, "getanimweight", |world, receiver, args| {
        let (tree, anim) = anim_value(args, 0)?;
        with_anim(world, receiver, Some(&tree), |state| {
            Ok(Value::Float(state.weight(node_of(state, &anim)?)))
        })
    });
    registry.register(Method, "setanimtime", |world, receiver, args| {
        let (tree, anim) = anim_value(args, 0)?;
        let time = float(args, 1)?.clamp(0.0, 1.0);
        with_anim(world, receiver, Some(&tree), |state| {
            state.set_time(node_of(state, &anim)?, time);
            Ok(Value::Undefined)
        })
    });
    for name in ["animscripted", "startscriptedanim"] {
        registry.register(Method, name, |world, receiver, args| {
            let notify: Arc<str> = super::args::string(args, 0)?.into();
            let origin = vector(args, 1)?;
            let angles = vector(args, 2)?;
            let anim = anim_value(args, 3)?;
            // args[4] is the actor's animmode; models have none.
            let root = match args.get(5) {
                Some(Value::Animation { name, .. }) => Some(name.clone()),
                _ => None,
            };
            start_scripted(world, receiver, notify, origin, angles, anim, root)?;
            Ok(Value::Undefined)
        });
    }
    registry.register(Method, "stopanimscripted", |world, receiver, _| {
        let id = entity_id(world, receiver)?;
        if let Some(anim) = world
            .resource_mut::<Runtime>()
            .entities
            .get_mut(&id)
            .and_then(|entity| entity.anim.as_mut())
        {
            anim.scripted = None;
        }
        Ok(Value::Undefined)
    });
    registry.register(Function, "getanimlength", |world, _, args| {
        Ok(Value::Float(clip_arg(world, args, 0)?.duration()))
    });
    registry.register(Function, "animhasnotetrack", |world, _, args| {
        let clip = clip_arg(world, args, 0)?;
        let note = super::args::string(args, 1)?;
        Ok(Value::Int(i32::from(
            clip.notifies.iter().any(|n| n.name == note),
        )))
    });
    registry.register(Function, "getnotetracktimes", |world, _, args| {
        let clip = clip_arg(world, args, 0)?;
        let note = super::args::string(args, 1)?;
        let times = clip
            .notifies
            .iter()
            .filter(|n| n.name == note)
            .map(|n| Value::Float(n.time))
            .collect();
        new_array(world, times)
    });
    registry.register(Function, "getstartorigin", |world, _, args| {
        let clip = clip_arg(world, args, 2)?;
        Ok(Value::Vector(
            root_pose(&clip, vector(args, 0)?, vector(args, 1)?, 0.0).0,
        ))
    });
    registry.register(Function, "getstartangles", |world, _, args| {
        let clip = clip_arg(world, args, 2)?;
        Ok(Value::Vector(
            root_pose(&clip, vector(args, 0)?, vector(args, 1)?, 0.0).1,
        ))
    });
    registry.register(Function, "getmovedelta", |world, _, args| {
        let clip = clip_arg(world, args, 0)?;
        let start = opt_float(args, 1, 0.0)?;
        let end = opt_float(args, 2, 1.0)?;
        let from = clip.abs_delta_trans(start);
        let to = clip.abs_delta_trans(end);
        let yaw = -yaw_of(clip.abs_delta_quat(start));
        Ok(Value::Vector(rotate(
            [0.0, yaw, 0.0],
            [to[0] - from[0], to[1] - from[1], to[2] - from[2]],
        )))
    });
    registry.register(Function, "getangledelta", |world, _, args| {
        let clip = clip_arg(world, args, 0)?;
        let start = opt_float(args, 1, 0.0)?;
        let end = opt_float(args, 2, 1.0)?;
        Ok(Value::Float(math_iw4::angle_subtract(
            yaw_of(clip.abs_delta_quat(end)),
            yaw_of(clip.abs_delta_quat(start)),
        )))
    });
    // SP scripts also call these bare; a bare call's receiver is `self`.
    for name in ["useanimtree", "setanim", "setanimknob", "clearanim"] {
        let native = registry.get(Method, name).unwrap();
        registry.register(Function, name, native);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> Arc<AnimTree> {
        let node = |name: &str, parent: Option<u16>, first_child: u16, child_count: u16| {
            AnimTreeNode {
                name: name.into(),
                parent,
                flags: 0,
                first_child,
                child_count,
            }
        };
        // root -> body -> {idle, walk}
        Arc::new(AnimTree::new(vec![
            node("root", None, 1, 1),
            node("body", Some(0), 2, 2),
            node("idle", Some(1), 0, 0),
            node("walk", Some(1), 0, 0),
        ]))
    }

    fn clip(name: &str, looping: bool) -> Arc<AnimClip> {
        Arc::new(AnimClip {
            name: name.into(),
            framerate: 10.0,
            numframes: 10,
            looping,
            tracks: Vec::new(),
            notifies: vec![xmodel_runtime::ClipNotify {
                name: "fire".into(),
                time: 0.5,
            }],
            delta_translation: xmodel_runtime::Translation::Default,
            delta_rotation: xmodel_runtime::Rotation::Default,
        })
    }

    #[test]
    fn knob_fades_siblings_and_notetracks_fire_in_order() {
        let library = AnimLibrary {
            trees: BTreeMap::new(),
            clips: Some(Arc::new(|name: &str| Some(clip(name, name == "idle")))),
        };
        let mut anim = EntityAnim::new("t".into(), tree());
        anim.set(2, 1.0, 0.0, 1.0, SetMode::default(), Some("a".into()));
        assert_eq!(anim.weight(1), 1.0, "parent comes on with its child");
        anim.set(
            3,
            1.0,
            1.0,
            1.0,
            SetMode {
                knob: true,
                ..SetMode::default()
            },
            Some("b".into()),
        );
        // 1 s clip: half a second crosses `fire` on both.
        let notes = anim.update(&library, 0.6);
        assert!(notes.contains(&("a".into(), "fire".into())));
        assert!(notes.contains(&("b".into(), "fire".into())));
        assert!(anim.states[2].goal_weight == 0.0, "knob fades the sibling");
        // walk is not looping: it parks short of the end, then notifies `end` once.
        let ends = (0..4)
            .map(|_| anim.update(&library, 0.6))
            .filter(|notes| notes.contains(&("b".into(), "end".into())))
            .count();
        assert_eq!(ends, 1);
        let snapshot = anim.snapshot().unwrap();
        assert!(snapshot.nodes.iter().any(|n| n.clip.as_deref() == Some("walk")));
    }
}
