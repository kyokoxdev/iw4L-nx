//! SP traverse nodes: an actor whose route crosses a negotiation begin node plays
//! that node's `animscript` (`animscripts/traverse/<name>::main`) and ends at the
//! end node. Without such an animscript the link is walked like any other.
//! SP cover nodes (`Cover *` path nodes) are claimed by `usecovernode`, which
//! sends the actor to them; a goal volume (`info_volume`) bounds where the
//! actor's goal may be. Fork-only; hooks in `actors.rs`.

use std::collections::{BTreeMap, BTreeSet};

use bevy_ecs::prelude::World;

use super::super::args::{arg, string, vector};
use super::super::natives::engine::entity_id;
use super::super::triggers::entity_bounds;
use super::{
    ActorBrain, DEFAULT_GOAL_HEIGHT, DEFAULT_GOAL_RADIUS, GoalTarget, actor, brain, clear_sight,
    cover_script, distance, has_entry, number, origin_of, set_goal,
};
use crate::script::{Namespace, NativeRegistry, RoundScript, Value};

/// Shuffle searches this far around the actor.
const SHUFFLE_RADIUS: f32 = 256.0;

/// Marks the actor as crossing the traverse `(start, end)` (node indices) the motor reached.
pub(super) fn begin_traverse(runtime: &mut RoundScript, id: u64, (start, end): (usize, usize)) {
    let (Some(&start), Some(&end)) = (
        runtime.campaign.nodes.get(start),
        runtime.campaign.nodes.get(end),
    ) else {
        return;
    };
    runtime
        .campaign
        .actor_brains
        .entry(id)
        .or_default()
        .traverse = Some((start, end));
}

/// `traverse/<animscript>` while the actor crosses a traverse. A traverse whose
/// animscript is missing is dropped, so its link is walked.
pub(super) fn traverse_script(world: &mut World, brain: &mut ActorBrain) -> Option<String> {
    let (start, _) = brain.traverse?;
    let anim = match world
        .resource_mut::<RoundScript>()
        .object_field(start, "animscript")
    {
        Value::String(anim) => anim.to_string(),
        _ => String::new(),
    };
    let script = format!("traverse/{anim}");
    if !anim.is_empty() && has_entry(world, &format!("animscripts/{script}::main")) {
        return Some(script);
    }
    brain.traverse = None;
    None
}

/// Ends the traverse once its animscript returns: the actor stands at the end node.
pub(super) fn finish_traverse(
    world: &mut World,
    id: u64,
    brain: &mut ActorBrain,
    thread_done: bool,
) {
    let Some((_, end)) = brain.traverse else {
        return;
    };
    if !thread_done || !brain.script.starts_with("traverse/") {
        return;
    }
    let mut runtime = world.resource_mut::<RoundScript>();
    if let Value::Vector(at) = runtime.object_field(end, "origin") {
        runtime.set_object_field(id, "origin", Value::Vector(at));
    }
    brain.traverse = None;
}

/// Threat groups and their pairs. An actor scores a hostile at its distance minus
/// `bias(actor group, its group)`, so a positive bias draws it; a pair in `ignore`
/// is never picked.
#[derive(Clone, Debug, Default)]
pub(crate) struct ThreatBias {
    groups: BTreeSet<String>,
    /// `(viewer, target)`: viewers in the first group never pick the second.
    ignore: BTreeSet<(String, String)>,
    /// `(target, viewer)` -> bias.
    bias: BTreeMap<(String, String), f32>,
}

impl ThreatBias {
    pub(super) fn ignores(&self, viewer: &str, target: &str) -> bool {
        self.ignore
            .contains(&(viewer.to_string(), target.to_string()))
    }

    pub(super) fn bias(&self, viewer: &str, target: &str) -> f32 {
        self.bias
            .get(&(target.to_string(), viewer.to_string()))
            .copied()
            .unwrap_or(0.0)
    }

    /// `setthreatbiasagainstall`: `target`'s bias for every group that exists now.
    // ponytail: groups created later get no bias; set it again after creating them.
    fn set_against_all(&mut self, target: &str, bias: f32) {
        for viewer in &self.groups {
            self.bias.insert((target.to_string(), viewer.clone()), bias);
        }
    }
}

/// An entity's threat group, `""` when it has none.
pub(super) fn group_of(runtime: &mut RoundScript, id: u64) -> String {
    match runtime.object_field(id, "threatbiasgroup") {
        Value::String(group) => group.to_string(),
        _ => String::new(),
    }
}

/// `getnegotiationstartnode` / `getnegotiationendnode`: the traverse being crossed, else undefined.
pub(super) fn negotiation_node(
    world: &mut World,
    receiver: &Value,
    start: bool,
) -> Result<Value, String> {
    let id = actor(world, receiver)?;
    Ok(match brain(world, id).traverse {
        Some((s, e)) => Value::Object(if start { s } else { e }),
        None => Value::Undefined,
    })
}

/// Where an actor's goal may be: anywhere, inside a goal volume's box, or
/// within the goal radius and height of the goal point.
enum Area {
    Any,
    Box([f32; 3], [f32; 3]),
    Sphere {
        center: [f32; 3],
        radius: f32,
        height: f32,
    },
}

fn inside(area: &Area, at: [f32; 3]) -> bool {
    match area {
        Area::Any => true,
        Area::Box(lo, hi) => (0..3).all(|k| lo[k] <= at[k] && at[k] <= hi[k]),
        Area::Sphere {
            center,
            radius,
            height,
        } => {
            (0..2)
                .map(|k| (at[k] - center[k]).powi(2))
                .sum::<f32>()
                .sqrt()
                <= *radius
                && (at[2] - center[2]).abs() <= *height
        }
    }
}

fn vector_field(runtime: &mut RoundScript, id: u64, name: &str) -> Option<[f32; 3]> {
    match runtime.object_field(id, name) {
        Value::Vector(at) => Some(at),
        _ => None,
    }
}

/// World-space box of an `info_volume` (its origin plus its brush bounds).
fn volume_box(world: &mut World, volume: u64) -> ([f32; 3], [f32; 3]) {
    let at = vector_field(&mut world.resource_mut::<RoundScript>(), volume, "origin")
        .unwrap_or([0.0; 3]);
    let (lo, hi) = entity_bounds(world, volume);
    (
        std::array::from_fn(|k| at[k] + lo[k]),
        std::array::from_fn(|k| at[k] + hi[k]),
    )
}

/// The actor's goal area: its goal volume, else its goal radius around `goalpos`.
fn goal_area(world: &mut World, id: u64) -> Area {
    let volume = match world
        .resource_mut::<RoundScript>()
        .object_field(id, "goalvolume")
    {
        Value::Object(volume) => Some(volume),
        _ => None,
    };
    if let Some(volume) = volume {
        let (lo, hi) = volume_box(world, volume);
        return Area::Box(lo, hi);
    }
    let mut runtime = world.resource_mut::<RoundScript>();
    let Some(center) = vector_field(&mut runtime, id, "goalpos") else {
        return Area::Any;
    };
    Area::Sphere {
        center,
        radius: number(runtime.object_field(id, "goalradius")).unwrap_or(DEFAULT_GOAL_RADIUS),
        height: number(runtime.object_field(id, "goalheight")).unwrap_or(DEFAULT_GOAL_HEIGHT),
    }
}

/// A cover node an actor may take: where it is and which way it faces.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Cover {
    id: u64,
    origin: [f32; 3],
    forward: [f32; 3],
}

/// Whether the cover faces the enemy (its forward points toward the enemy).
fn faces(cover: &Cover, enemy: Option<[f32; 3]>) -> bool {
    enemy.is_some_and(|enemy| {
        (0..2)
            .map(|k| cover.forward[k] * (enemy[k] - cover.origin[k]))
            .sum::<f32>()
            > 0.0
    })
}

/// The nearest cover to `from`, passing over covers that face the enemy
/// unless every candidate does.
fn best_cover(from: [f32; 3], enemy: Option<[f32; 3]>, covers: &[Cover]) -> Option<u64> {
    covers
        .iter()
        .min_by(|a, b| {
            faces(a, enemy)
                .cmp(&faces(b, enemy))
                .then(distance(from, a.origin).total_cmp(&distance(from, b.origin)))
        })
        .map(|cover| cover.id)
}

/// A node is free for `id` unless another live actor holds it: a claim lapses
/// when its claimer's `node` moves on.
// ponytail: claims are checked, not released; a dead claimer's node frees on the next check.
fn free_for(runtime: &mut RoundScript, id: u64, node: u64) -> bool {
    match runtime.object_field(node, "claimedby") {
        Value::Object(claimer) if claimer != id && runtime.live(&claimer) => {
            runtime.object_field(claimer, "node") == Value::Object(node)
        }
        _ => true,
    }
}

/// The best free cover node inside `area` for `id`, nearest `goalpos` (else its origin).
fn find_cover(world: &mut World, id: u64, area: &Area, skip: Option<u64>) -> Option<u64> {
    let mut runtime = world.resource_mut::<RoundScript>();
    let from = vector_field(&mut runtime, id, "goalpos")
        .or_else(|| vector_field(&mut runtime, id, "origin"))
        .unwrap_or([0.0; 3]);
    let enemy = match runtime.object_field(id, "enemy") {
        Value::Object(enemy) => vector_field(&mut runtime, enemy, "origin"),
        _ => None,
    };
    let mut covers = Vec::new();
    for node in runtime.campaign.nodes.clone() {
        if Some(node) == skip || !free_for(&mut runtime, id, node) {
            continue;
        }
        if let Some(cover) = cover_at(&mut runtime, node).filter(|c| inside(area, c.origin)) {
            covers.push(cover);
        }
    }
    best_cover(from, enemy, &covers)
}

/// The cover at a path node, if the node is a cover kind with an origin and angles.
fn cover_at(runtime: &mut RoundScript, node: u64) -> Option<Cover> {
    let Value::String(kind) = runtime.object_field(node, "type") else {
        return None;
    };
    let origin = vector_field(runtime, node, "origin")?;
    let angles = vector_field(runtime, node, "angles")?;
    if cover_script(&kind).is_none() {
        return None;
    }
    let yaw = angles[1].to_radians();
    Some(Cover {
        id: node,
        origin,
        forward: [yaw.cos(), yaw.sin(), 0.0],
    })
}

pub(super) fn register(registry: &mut NativeRegistry) {
    use Namespace::{Function, Method};
    registry.register(Function, "createthreatbiasgroup", |world, _, args| {
        let name = string(args, 0)?;
        world
            .resource_mut::<RoundScript>()
            .campaign
            .threat
            .groups
            .insert(name);
        Ok(Value::Undefined)
    });
    registry.register(Function, "threatbiasgroupexists", |world, _, args| {
        let name = string(args, 0)?;
        let runtime = world.resource::<RoundScript>();
        Ok(Value::Int(
            runtime.campaign.threat.groups.contains(&name).into(),
        ))
    });
    registry.register(Function, "setignoremegroup", |world, _, args| {
        let pair = (string(args, 0)?, string(args, 1)?);
        world
            .resource_mut::<RoundScript>()
            .campaign
            .threat
            .ignore
            .insert(pair);
        Ok(Value::Undefined)
    });
    registry.register(Function, "setthreatbias", |world, _, args| {
        let key = (string(args, 0)?, string(args, 1)?);
        let bias = number(arg(args, 2)?.clone()).ok_or("setthreatbias expects a number")?;
        world
            .resource_mut::<RoundScript>()
            .campaign
            .threat
            .bias
            .insert(key, bias);
        Ok(Value::Undefined)
    });
    registry.register(Function, "setthreatbiasagainstall", |world, _, args| {
        let target = string(args, 0)?;
        let bias =
            number(arg(args, 1)?.clone()).ok_or("setthreatbiasagainstall expects a number")?;
        world
            .resource_mut::<RoundScript>()
            .campaign
            .threat
            .set_against_all(&target, bias);
        Ok(Value::Undefined)
    });
    registry.register(Function, "getthreatbias", |world, _, args| {
        let (target, viewer) = (string(args, 0)?, string(args, 1)?);
        let runtime = world.resource::<RoundScript>();
        Ok(Value::Float(runtime.campaign.threat.bias(&viewer, &target)))
    });
    registry.register(Method, "setthreatbiasgroup", |world, receiver, args| {
        let id = entity_id(world, receiver)?;
        let group = string(args, 0)?;
        world.resource_mut::<RoundScript>().set_object_field(
            id,
            "threatbiasgroup",
            Value::string(&group),
        );
        Ok(Value::Undefined)
    });
    registry.register(Method, "getthreatbiasgroup", |world, receiver, _| {
        let id = entity_id(world, receiver)?;
        let group = group_of(&mut world.resource_mut::<RoundScript>(), id);
        Ok(Value::string(&group))
    });
    registry.register(Method, "findbestcovernode", |world, receiver, _| {
        let id = actor(world, receiver)?;
        let area = goal_area(world, id);
        Ok(find_cover(world, id, &area, None).map_or(Value::Undefined, Value::Object))
    });
    registry.register(Method, "findshufflecovernode", |world, receiver, _| {
        let id = actor(world, receiver)?;
        let mut runtime = world.resource_mut::<RoundScript>();
        let center = vector_field(&mut runtime, id, "origin").unwrap_or([0.0; 3]);
        let skip = match runtime.object_field(id, "node") {
            Value::Object(node) => Some(node),
            _ => None,
        };
        drop(runtime);
        let area = Area::Sphere {
            center,
            radius: SHUFFLE_RADIUS,
            height: DEFAULT_GOAL_HEIGHT,
        };
        Ok(find_cover(world, id, &area, skip).map_or(Value::Undefined, Value::Object))
    });
    // Claims the node and goes to it; the cover animscript plays once there.
    registry.register(Method, "usecovernode", |world, receiver, args| {
        let id = actor(world, receiver)?;
        let (node, at) = origin_of(world, arg(args, 0)?)?;
        let mut runtime = world.resource_mut::<RoundScript>();
        let is_cover = matches!(runtime.object_field(node, "type"), Value::String(kind) if cover_script(&kind).is_some());
        if !is_cover || !free_for(&mut runtime, id, node) {
            return Ok(Value::Int(0));
        }
        runtime.set_object_field(node, "claimedby", Value::Object(id));
        drop(runtime);
        set_goal(world, receiver, GoalTarget::Pos(at), at, Some(node))?;
        Ok(Value::Int(1))
    });
    // Valid when the cover does not face the actor's enemy (the order `best_cover` prefers).
    registry.register(
        Method,
        "iscovervalidagainstenemy",
        |world, receiver, args| {
            let id = actor(world, receiver)?;
            let (node, _) = origin_of(world, arg(args, 0)?)?;
            let mut runtime = world.resource_mut::<RoundScript>();
            let enemy = match runtime.object_field(id, "enemy") {
                Value::Object(enemy) => vector_field(&mut runtime, enemy, "origin"),
                _ => None,
            };
            let valid = cover_at(&mut runtime, node).is_some_and(|cover| !faces(&cover, enemy));
            Ok(Value::Int(valid.into()))
        },
    );
    registry.register(Method, "getvalidcoverpeekouts", |world, receiver, _| {
        let node = entity_id(world, receiver)?;
        let cover = cover_at(&mut world.resource_mut::<RoundScript>(), node);
        let modes = cover.map_or_else(Vec::new, |cover| peekouts(world, &cover));
        super::super::arrays::new_array(world, modes.into_iter().map(Value::string).collect())
    });
    registry.register(Method, "isingoal", |world, receiver, args| {
        let id = actor(world, receiver)?;
        let at = vector(args, 0)?;
        let area = goal_area(world, id);
        Ok(Value::Int(inside(&area, at).into()))
    });
    registry.register(Method, "setgoalvolume", set_goal_volume);
    registry.register(Method, "setgoalvolumeauto", set_goal_volume);
    registry.register(Method, "getgoalvolume", |world, receiver, _| {
        let id = actor(world, receiver)?;
        Ok(world
            .resource_mut::<RoundScript>()
            .object_field(id, "goalvolume"))
    });
    registry.register(Method, "cleargoalvolume", |world, receiver, _| {
        let id = actor(world, receiver)?;
        world
            .resource_mut::<RoundScript>()
            .set_object_field(id, "goalvolume", Value::Undefined);
        Ok(Value::Undefined)
    });
}

/// How far a peek steps out sideways, then looks ahead.
const PEEK_STEP: f32 = 32.0;
/// Eye height a peek is traced at.
const PEEK_HEIGHT: f32 = 40.0;

/// Sides of a cover an actor can step out of: the step sideways is clear and
/// so is the view ahead from there.
// ponytail: two traces per side at one height, no "over" for crouch walls;
// add it if crouch covers never fire over the top.
fn peekouts(world: &mut World, cover: &Cover) -> Vec<&'static str> {
    let [fx, fy, _] = cover.forward;
    let from = [
        cover.origin[0],
        cover.origin[1],
        cover.origin[2] + PEEK_HEIGHT,
    ];
    [("left", -1.0), ("right", 1.0)]
        .into_iter()
        .filter(|(_, sign)| {
            let side = [
                from[0] + fy * sign * PEEK_STEP,
                from[1] - fx * sign * PEEK_STEP,
                from[2],
            ];
            let ahead = [side[0] + fx * PEEK_STEP, side[1] + fy * PEEK_STEP, side[2]];
            clear_sight(world, from, side) && clear_sight(world, side, ahead)
        })
        .map(|(mode, _)| mode)
        .collect()
}

/// `setgoalvolume(volume)`: the goal must stay in the volume. A goal already
/// inside keeps its place; otherwise the actor goes to the volume's nearest point.
fn set_goal_volume(world: &mut World, receiver: &Value, args: &[Value]) -> Result<Value, String> {
    let id = actor(world, receiver)?;
    let volume = entity_id(world, arg(args, 0)?)?;
    world
        .resource_mut::<RoundScript>()
        .set_object_field(id, "goalvolume", Value::Object(volume));
    let (lo, hi) = volume_box(world, volume);
    let mut runtime = world.resource_mut::<RoundScript>();
    let goal = vector_field(&mut runtime, id, "goalpos");
    let origin = vector_field(&mut runtime, id, "origin").unwrap_or([0.0; 3]);
    drop(runtime);
    let area = Area::Box(lo, hi);
    if goal.is_some_and(|at| inside(&area, at)) {
        return Ok(Value::Undefined);
    }
    let at: [f32; 3] = std::array::from_fn(|k| origin[k].clamp(lo[k], hi[k]));
    set_goal(world, receiver, GoalTarget::Pos(at), at, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A cover facing the enemy loses to a farther one facing away, unless it is the only one.
    #[test]
    fn best_cover_passes_over_covers_facing_the_enemy() {
        let facing = Cover {
            id: 1,
            origin: [10.0, 0.0, 0.0],
            forward: [1.0, 0.0, 0.0],
        };
        let away = Cover {
            id: 2,
            origin: [50.0, 0.0, 0.0],
            forward: [-1.0, 0.0, 0.0],
        };
        let enemy = Some([500.0, 0.0, 0.0]);
        assert_eq!(best_cover([0.0; 3], enemy, &[facing, away]), Some(2));
        assert_eq!(best_cover([0.0; 3], enemy, &[facing]), Some(1));
        assert_eq!(best_cover([0.0; 3], None, &[facing, away]), Some(1));
    }

    /// `set_against_all` sets the target's bias for each existing group, and only those.
    #[test]
    fn threat_bias_set_against_all_covers_existing_groups() {
        let mut threat = ThreatBias::default();
        threat.groups.insert("axis".into());
        threat.groups.insert("allies".into());
        threat.set_against_all("leapfrog", -200.0);
        assert_eq!(threat.bias("axis", "leapfrog"), -200.0);
        assert_eq!(threat.bias("allies", "leapfrog"), -200.0);
        assert_eq!(threat.bias("axis", "allies"), 0.0);
    }

    /// A positive bias draws the viewer toward that group; ignores are one-way pairs.
    #[test]
    fn threat_bias_draws_and_ignores_by_group() {
        let mut threat = ThreatBias::default();
        threat
            .bias
            .insert(("just_player".into(), "bridge_attackers".into()), 3000.0);
        threat.ignore.insert(("axis".into(), "allies".into()));
        assert_eq!(threat.bias("bridge_attackers", "just_player"), 3000.0);
        assert_eq!(threat.bias("bridge_attackers", "allies"), 0.0);
        assert!(threat.ignores("axis", "allies"));
        assert!(!threat.ignores("allies", "axis"));
    }
}
