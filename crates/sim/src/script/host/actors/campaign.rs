//! SP traverse nodes: an actor whose route crosses a negotiation begin node plays
//! that node's `animscript` (`animscripts/traverse/<name>::main`) and ends at the
//! end node. Without such an animscript the link is walked like any other.
//! SP cover nodes (`Cover *` path nodes) are claimed by `usecovernode`, which
//! sends the actor to them; a goal volume (`info_volume`) bounds where the
//! actor's goal may be. Fork-only; hooks in `actors.rs`.

use bevy_ecs::prelude::World;

use super::super::args::{arg, vector};
use super::super::natives::engine::entity_id;
use super::super::triggers::entity_bounds;
use super::{
    ActorBrain, DEFAULT_GOAL_HEIGHT, DEFAULT_GOAL_RADIUS, GoalTarget, actor, brain, cover_script,
    distance, has_entry, number, origin_of, set_goal,
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
    runtime.campaign.actor_brains.entry(id).or_default().traverse = Some((start, end));
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
pub(super) fn finish_traverse(world: &mut World, id: u64, brain: &mut ActorBrain, thread_done: bool) {
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

/// `getnegotiationstartnode` / `getnegotiationendnode`: the traverse being crossed, else undefined.
pub(super) fn negotiation_node(world: &mut World, receiver: &Value, start: bool) -> Result<Value, String> {
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
        let Value::String(kind) = runtime.object_field(node, "type") else {
            continue;
        };
        let (Some(origin), Some(angles)) = (
            vector_field(&mut runtime, node, "origin"),
            vector_field(&mut runtime, node, "angles"),
        ) else {
            continue;
        };
        if cover_script(&kind).is_none() || !inside(area, origin) {
            continue;
        }
        let yaw = angles[1].to_radians();
        covers.push(Cover {
            id: node,
            origin,
            forward: [yaw.cos(), yaw.sin(), 0.0],
        });
    }
    best_cover(from, enemy, &covers)
}

pub(super) fn register(registry: &mut NativeRegistry) {
    use Namespace::Method;
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
}
