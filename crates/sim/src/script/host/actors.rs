//! SP actors: entities spawned from `actor_*` spawners. Bodies are script
//! entities like drones; their brain is the bot pipeline (`crates/bots`),
//! which routes them over the level's path nodes ([`ActorNav`]).
use bevy_ecs::prelude::World;
use std::sync::Arc;

use super::arrays::new_array;
use super::entities::EntityKind;
use super::natives::engine::{entity_id, spawn_entity};
use crate::script::{Namespace, NativeRegistry, Runtime, Value};

/// Spawner flag 1: the entity spawns AI instead of being one.
const SPAWNER: i32 = 1;

/// Routes over the level's path nodes, built by the bot brain.
pub trait ActorNav: Send + Sync {
    /// Waypoints from `from` to `to`, `to` last; `None` when no route joins them.
    fn route(&self, from: [f32; 3], to: [f32; 3]) -> Option<Vec<[f32; 3]>>;
}

#[derive(Clone)]
pub(crate) struct NavHandle(pub Arc<dyn ActorNav>);

impl std::fmt::Debug for NavHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NavHandle")
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum GoalTarget {
    Pos([f32; 3]),
    Entity(u64),
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ActorGoal {
    target: GoalTarget,
    /// Waypoints left, next first.
    route: Vec<[f32; 3]>,
    /// The goal position the route was made for; none until routed.
    routed_to: Option<[f32; 3]>,
    /// `goal` or `bad_path` was sent for `routed_to`.
    settled: bool,
}

// ponytail: one constant run speed; the motor becomes anim deltas
// (`animmode`/`orientmode`) once animscripts drive movement.
const RUN_SPEED: f32 = 170.0;
/// A goal entity that strays this far from the routed spot gets a new route.
const REROUTE: f32 = 64.0;
const DEFAULT_GOAL_RADIUS: f32 = 16.0;
const DEFAULT_GOAL_HEIGHT: f32 = 80.0;

fn number(value: Value) -> Option<f32> {
    match value {
        Value::Int(n) => Some(n as f32),
        Value::Float(n) => Some(n),
        _ => None,
    }
}

fn distance(a: [f32; 3], b: [f32; 3]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

/// Node objects with the level's path nodes' fields, in `PathData` order.
pub(crate) fn spawn_path_nodes(
    world: &mut World,
    nodes: &[asset_core::PathNode],
) -> Result<(), String> {
    let mut runtime = world.resource_mut::<Runtime>();
    for node in nodes {
        let id = runtime.new_object()?;
        runtime.set_object_field(id, "origin", Value::Vector(node.origin));
        runtime.set_object_field(id, "angles", Value::Vector([0.0, node.yaw, 0.0]));
        runtime.set_object_field(id, "type", Value::string(node.kind));
        runtime.set_object_field(id, "radius", Value::Float(node.radius));
        runtime.set_object_field(id, "spawnflags", Value::Int(node.spawnflags));
        for (key, value) in &node.keys {
            runtime.set_object_field(id, key, Value::string(value));
        }
        runtime.nodes.push(id);
    }
    Ok(())
}

/// Nodes whose string field `key` is `value`.
fn matching_nodes(world: &mut World, args: &[Value]) -> Result<Vec<Value>, String> {
    let value = Value::string(&super::args::string(args, 0)?);
    let key = super::args::string(args, 1)?.to_ascii_lowercase();
    let mut runtime = world.resource_mut::<Runtime>();
    let nodes = runtime.nodes.clone();
    Ok(nodes
        .into_iter()
        .filter(|id| runtime.object_field(*id, &key) == value)
        .map(Value::Object)
        .collect())
}

fn set_goal(
    world: &mut World,
    receiver: &Value,
    target: GoalTarget,
    at: [f32; 3],
) -> Result<Value, String> {
    let id = actor(world, receiver)?;
    let mut runtime = world.resource_mut::<Runtime>();
    runtime.set_object_field(id, "goalpos", Value::Vector(at));
    runtime.actor_goals.insert(
        id,
        ActorGoal {
            target,
            route: Vec::new(),
            routed_to: None,
            settled: false,
        },
    );
    Ok(Value::Undefined)
}

fn origin_of(world: &mut World, value: &Value) -> Result<(u64, [f32; 3]), String> {
    let Value::Object(id) = value else {
        return Err(format!("{} has no origin", super::args::kind(value)));
    };
    match world.resource_mut::<Runtime>().object_field(*id, "origin") {
        Value::Vector(origin) => Ok((*id, origin)),
        _ => Err("goal has no origin".into()),
    }
}

/// Moves each actor with a goal along its route; notifies `goal` on arrival
/// (inside `goalradius`/`goalheight`) and `bad_path` when no route exists.
pub(crate) fn advance(world: &mut World) {
    let step = RUN_SPEED * crate::MATCH_TICK_MS as f32 / 1000.0;
    let mut notes = Vec::new();
    let mut runtime = world.resource_mut::<Runtime>();
    let nav = runtime.actor_nav.clone();
    let ids: Vec<u64> = runtime.actor_goals.keys().copied().collect();
    for id in ids {
        let Value::Vector(mut origin) = runtime.object_field(id, "origin") else {
            runtime.actor_goals.remove(&id);
            continue;
        };
        let target = runtime.actor_goals[&id].target.clone();
        let goal_pos = match target {
            GoalTarget::Pos(at) => Some(at),
            GoalTarget::Entity(e) => match runtime.object_field(e, "origin") {
                Value::Vector(at) if runtime.live(&e) => Some(at),
                _ => None,
            },
        };
        let Some(goal_pos) = goal_pos.filter(|_| runtime.live(&id)) else {
            runtime.actor_goals.remove(&id);
            continue;
        };
        let radius = number(runtime.object_field(id, "goalradius")).unwrap_or(DEFAULT_GOAL_RADIUS);
        let height = number(runtime.object_field(id, "goalheight")).unwrap_or(DEFAULT_GOAL_HEIGHT);
        let goal = runtime.actor_goals.get_mut(&id).unwrap();
        let planar = ((origin[0] - goal_pos[0]).powi(2) + (origin[1] - goal_pos[1]).powi(2)).sqrt();
        if planar <= radius.max(4.0) && (origin[2] - goal_pos[2]).abs() <= height {
            goal.route.clear();
            if !goal.settled {
                goal.settled = true;
                goal.routed_to = Some(goal_pos);
                notes.push((id, "goal"));
            }
            continue;
        }
        if goal
            .routed_to
            .is_none_or(|at| distance(at, goal_pos) > REROUTE)
        {
            let Some(nav) = &nav else {
                continue;
            };
            goal.settled = false;
            goal.routed_to = Some(goal_pos);
            match nav.0.route(origin, goal_pos) {
                Some(route) => goal.route = route,
                None => {
                    goal.route.clear();
                    goal.settled = true;
                    notes.push((id, "bad_path"));
                }
            }
        }
        let mut left = step;
        let mut heading = None;
        while left > 0.0
            && let Some(&next) = goal.route.first()
        {
            let d = distance(origin, next);
            if d > 0.0 {
                heading = Some([next[0] - origin[0], next[1] - origin[1]]);
            }
            if d <= left {
                origin = next;
                goal.route.remove(0);
                left -= d;
            } else {
                let t = left / d;
                origin = std::array::from_fn(|k| origin[k] + (next[k] - origin[k]) * t);
                left = 0.0;
            }
        }
        if let Some([x, y]) = heading {
            runtime.set_object_field(id, "origin", Value::Vector(origin));
            let yaw = y.atan2(x).to_degrees();
            runtime.set_object_field(id, "angles", Value::Vector([0.0, yaw, 0.0]));
        }
    }
    for (id, note) in notes {
        crate::script::runtime::raise(world, Value::Object(id), note, Vec::new());
    }
}

fn has_entry(world: &World, name: &str) -> bool {
    world
        .resource::<Runtime>()
        .program
        .as_ref()
        .is_some_and(|program| program.names.contains_key(name))
}

fn spawners(world: &mut World) -> Vec<u64> {
    let mut runtime = world.resource_mut::<Runtime>();
    let ids: Vec<u64> = runtime
        .entities
        .iter()
        .filter(|(_, e)| e.kind == EntityKind::Map && e.classname.starts_with("actor_"))
        .map(|(id, _)| *id)
        .collect();
    ids.into_iter()
        .filter(|id| matches!(runtime.object_field(*id, "spawnflags"), Value::Int(f) if f & SPAWNER != 0))
        .collect()
}

fn aitype(classname: &str) -> String {
    let aitype = classname.strip_prefix("actor_").unwrap_or(classname);
    format!("aitype/{}", aitype.to_ascii_lowercase())
}

/// Runs each spawner's `aitype/<type>::spawner` (its team) before level main.
pub(crate) fn init_spawners(world: &mut World) {
    let now = super::players::now_ms(world);
    for spawner in spawners(world) {
        let classname = world.resource::<Runtime>().entities[&spawner]
            .classname
            .clone();
        let entry = format!("{}::spawner", aitype(&classname));
        if has_entry(world, &entry) {
            let _ = crate::script::runtime::run_now(
                world,
                &entry,
                Value::Object(spawner),
                Vec::new(),
                now,
            );
        }
    }
}

/// A script model at the spawner carrying its keys (not classname/targetname),
/// with the spawner's aitype `main` run on it.
fn spawn_from(world: &mut World, spawner: u64, actor: bool) -> Result<u64, String> {
    let (origin, angles, classname, fields) = {
        let mut runtime = world.resource_mut::<Runtime>();
        let origin = runtime.object_field(spawner, "origin");
        let angles = runtime.object_field(spawner, "angles");
        let classname = runtime.entities[&spawner].classname.clone();
        let fields = runtime.objects.get(&spawner).cloned().unwrap_or_default();
        (origin, angles, classname, fields)
    };
    let Value::Vector(origin) = origin else {
        return Err("spawner has no origin".into());
    };
    let id = spawn_entity(world, "script_model", origin, 0, None)?;
    let mut runtime = world.resource_mut::<Runtime>();
    // ponytail: copies every spawner key but classname/targetname/count; narrow if a
    // level looks AI up by a spawner key the engine would not copy.
    let skip = ["classname", "targetname", "count"].map(|name| runtime.symbol(name));
    let body = runtime.objects.get_mut(&id).unwrap();
    for (field, value) in fields {
        if !skip.contains(&field) {
            body.insert(field, value);
        }
    }
    runtime.set_object_field(id, "origin", Value::Vector(origin));
    runtime.set_object_field(id, "angles", angles);
    runtime.set_object_field(id, "weapon", Value::string("none"));
    if actor {
        let entity = runtime.entities.get_mut(&id).unwrap();
        entity.kind = EntityKind::Actor;
        entity.classname = classname.clone();
        entity.can_damage = true;
    }
    let main = format!("{}::main", aitype(&classname));
    let now = super::players::now_ms(world);
    crate::script::runtime::run_now(world, &main, Value::Object(id), Vec::new(), now)
        .map_err(|fault| format!("{main}: {fault:?}"))?;
    Ok(id)
}

/// `dospawn`/`stalingradspawn`: spends one of the spawner's `count`, then runs
/// `animscripts/init::main` on the actor and notifies the spawner `spawned`.
fn spawn_actor(world: &mut World, receiver: &Value) -> Result<Value, String> {
    let spawner = entity_id(world, receiver)?;
    let count = match world
        .resource_mut::<Runtime>()
        .object_field(spawner, "count")
    {
        Value::Int(n) => n,
        _ => 1,
    };
    if count <= 0 {
        return Ok(Value::Undefined);
    }
    world
        .resource_mut::<Runtime>()
        .set_object_field(spawner, "count", Value::Int(count - 1));
    let id = spawn_from(world, spawner, true)?;
    const INIT: &str = "animscripts/init::main";
    if has_entry(world, INIT) {
        let now = super::players::now_ms(world);
        crate::script::runtime::run_now(world, INIT, Value::Object(id), Vec::new(), now)
            .map_err(|fault| format!("{INIT}: {fault:?}"))?;
    }
    let guy = Value::Object(id);
    crate::script::runtime::raise(world, Value::Object(spawner), "spawned", vec![guy.clone()]);
    Ok(guy)
}

fn actor(world: &World, receiver: &Value) -> Result<u64, String> {
    let id = entity_id(world, receiver)?;
    match world.resource::<Runtime>().entities.get(&id) {
        Some(entity) if entity.kind == EntityKind::Actor => Ok(id),
        _ => Err("receiver is not an actor".into()),
    }
}

/// Live actors whose team is one of `teams` ("all" or none given: any).
fn actors(world: &mut World, teams: &[Value]) -> Vec<Value> {
    let any = teams.is_empty() || teams.iter().any(|t| *t == Value::string("all"));
    let mut runtime = world.resource_mut::<Runtime>();
    let ids: Vec<u64> = runtime
        .entities
        .iter()
        .filter(|(id, e)| e.kind == EntityKind::Actor && !runtime.pending_deletes.contains(id))
        .map(|(id, _)| *id)
        .collect();
    ids.into_iter()
        .filter(|id| {
            let alive = matches!(runtime.object_field(*id, "health"), Value::Int(h) if h > 0);
            alive && (any || teams.contains(&runtime.object_field(*id, "team")))
        })
        .map(Value::Object)
        .collect()
}

pub(crate) fn register(registry: &mut NativeRegistry) {
    use Namespace::{Function, Method};
    registry.register(Method, "dospawn", |world, receiver, _| {
        spawn_actor(world, receiver)
    });
    registry.register(Method, "stalingradspawn", |world, receiver, _| {
        spawn_actor(world, receiver)
    });
    // SP: a drone is a script model standing in for the spawner's AI.
    registry.register(Method, "spawndrone", |world, receiver, _| {
        let spawner = entity_id(world, receiver)?;
        spawn_from(world, spawner, false).map(Value::Object)
    });
    registry.register(Function, "getspawnerarray", |world, _, _| {
        let ids = spawners(world).into_iter().map(Value::Object).collect();
        new_array(world, ids)
    });
    registry.register(Function, "getspawnerteamarray", |world, _, args| {
        let team = super::args::arg(args, 0)?.clone();
        let mut ids = spawners(world);
        let mut runtime = world.resource_mut::<Runtime>();
        ids.retain(|id| runtime.object_field(*id, "team") == team);
        new_array(world, ids.into_iter().map(Value::Object).collect())
    });
    registry.register(Function, "getaiarray", |world, _, args| {
        let ids = actors(world, args);
        new_array(world, ids)
    });
    // ponytail: species ignored (every IW4 aitype here is "human"); filter on
    // `type` once dogs spawn.
    registry.register(Function, "getaispeciesarray", |world, _, args| {
        let ids = actors(world, &args[..args.len().min(1)]);
        new_array(world, ids)
    });
    registry.register(Function, "getaicount", |world, _, args| {
        Ok(Value::Int(actors(world, args).len() as i32))
    });
    registry.register(Method, "forceteleport", |world, receiver, args| {
        let id = actor(world, receiver)?;
        let origin = super::args::vector(args, 0)?;
        let mut runtime = world.resource_mut::<Runtime>();
        runtime.set_object_field(id, "origin", Value::Vector(origin));
        if let Some(angles) = args.get(1).filter(|v| **v != Value::Undefined) {
            runtime.set_object_field(id, "angles", angles.clone());
        }
        Ok(Value::Undefined)
    });
    registry.register(
        Function,
        "getnode",
        |world, _, args| match matching_nodes(world, args)?.as_slice() {
            [] => Ok(Value::Undefined),
            [node] => Ok(node.clone()),
            _ => Err("getnode used with more than one node".into()),
        },
    );
    registry.register(Function, "getnodearray", |world, _, args| {
        let nodes = matching_nodes(world, args)?;
        new_array(world, nodes)
    });
    registry.register(Function, "getallnodes", |world, _, _| {
        let nodes = world
            .resource::<Runtime>()
            .nodes
            .iter()
            .copied()
            .map(Value::Object)
            .collect();
        new_array(world, nodes)
    });
    registry.register(Method, "setgoalpos", |world, receiver, args| {
        let at = super::args::vector(args, 0)?;
        set_goal(world, receiver, GoalTarget::Pos(at), at)
    });
    registry.register(Method, "setgoalnode", |world, receiver, args| {
        let (_, at) = origin_of(world, super::args::arg(args, 0)?)?;
        set_goal(world, receiver, GoalTarget::Pos(at), at)
    });
    registry.register(Method, "setgoalentity", |world, receiver, args| {
        let (target, at) = origin_of(world, super::args::arg(args, 0)?)?;
        set_goal(world, receiver, GoalTarget::Entity(target), at)
    });
    registry.register(Function, "isai", |world, _, args| {
        let actor = world
            .resource::<Runtime>()
            .entity(super::args::arg(args, 0)?)
            .is_some_and(|(_, e)| e.kind == EntityKind::Actor);
        Ok(Value::Int(actor.into()))
    });
}
