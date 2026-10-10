//! SP actors: entities spawned from `actor_*` spawners. Bodies are script
//! entities like drones; their brain is meant to be the bot pipeline
//! (`crates/bots`), so for now they stand where they spawn.
use bevy_ecs::prelude::World;

use super::arrays::new_array;
use super::entities::EntityKind;
use super::natives::engine::{entity_id, spawn_entity};
use crate::script::{Namespace, NativeRegistry, Runtime, Value};

/// Spawner flag 1: the entity spawns AI instead of being one.
const SPAWNER: i32 = 1;

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
    registry.register(Function, "isai", |world, _, args| {
        let actor = world
            .resource::<Runtime>()
            .entity(super::args::arg(args, 0)?)
            .is_some_and(|(_, e)| e.kind == EntityKind::Actor);
        Ok(Value::Int(actor.into()))
    });
}
