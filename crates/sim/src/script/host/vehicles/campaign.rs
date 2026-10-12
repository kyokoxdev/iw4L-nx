//! Campaign vehicles: map `script_vehicle`s, spawner `vehicle_dospawn()`,
//! ground paths over `info_vehicle_node`s. Fork-only; kept out of the parent
//! file so upstream merges touch it as little as possible.

use super::super::args::string;
use super::super::entities::EntityKind;
use super::{Heli, MPH, heli, spawn_vehicle, vec_field};
use crate::script::Namespace::Method;
use crate::script::{NativeRegistry, RoundScript, Value};
use bevy_ecs::prelude::World;

/// A level with AI path nodes is a campaign level; its vehicle paths are ground paths.
pub(super) fn sp_level(world: &World) -> bool {
    !world.resource::<RoundScript>().campaign.nodes.is_empty()
}

/// A vehicle node's `speed` key, in mph.
pub(super) fn node_speed(runtime: &mut RoundScript, node: u64) -> Option<f32> {
    match runtime.object_field(node, "speed") {
        Value::Int(speed) => Some(speed as f32),
        Value::Float(speed) => Some(speed),
        Value::String(speed) => speed.parse().ok(),
        _ => None,
    }
}

// ponytail: a node's speed applies with the vehicle's current accel (or the
// speed itself as accel); node `lookahead` and spline paths are not read.
pub(super) fn take_speed(vehicle: &mut Heli, mph: f32) {
    vehicle.max_speed = mph.max(0.0) * MPH;
    if vehicle.accel <= 0.0 {
        vehicle.accel = vehicle.max_speed.max(MPH);
    }
    if vehicle.decel <= 0.0 {
        vehicle.decel = vehicle.accel;
    }
}

/// Map `script_vehicle`s that are not spawners are live vehicles from the start.
pub(crate) fn init_map_vehicles(world: &mut World) {
    let ids: Vec<u64> = {
        let mut runtime = world.resource_mut::<RoundScript>();
        let ids: Vec<u64> = runtime
            .entities
            .iter()
            .filter(|(_, e)| {
                e.kind == EntityKind::Map && super::super::entities::is_vehicle_class(&e.classname)
            })
            .map(|(id, _)| *id)
            .collect();
        ids.into_iter()
            .filter(|id| {
                !matches!(runtime.object_field(*id, "spawnflags"),
                    Value::Int(f) if f & super::super::actors::VEHICLE_SPAWNER != 0)
            })
            .collect()
    };
    for id in ids {
        let origin = vec_field(&mut world.resource_mut::<RoundScript>(), id, "origin");
        let Ok(presence) = super::super::presence::spawn_presence(world, origin) else {
            continue;
        };
        let mut runtime = world.resource_mut::<RoundScript>();
        let entity = runtime.entities.get_mut(&id).unwrap();
        entity.kind = EntityKind::Vehicle;
        entity.can_damage = true;
        entity.presence = Some(presence);
        runtime.new_models.push(id);
        runtime.vehicles.insert(
            id,
            Heli {
                slot: u8::MAX,
                ..Heli::default()
            },
        );
    }
}

/// SP `vehicle_dospawn()`: a live vehicle at the spawner, carrying its keys
/// but `classname`/`targetname`.
pub(super) fn spawn_from_spawner(world: &mut World, spawner: u64) -> Result<Value, String> {
    let (origin, angles, model, classname, fields) = {
        let mut runtime = world.resource_mut::<RoundScript>();
        let entity = &runtime.entities[&spawner];
        if !super::super::entities::is_vehicle_class(&entity.classname)
            || entity.kind == EntityKind::Vehicle
        {
            return Err("receiver is not a vehicle spawner".into());
        }
        let classname = entity.classname.clone();
        let model = match runtime.object_field(spawner, "model") {
            Value::String(model) => model.to_string(),
            _ => String::new(),
        };
        (
            vec_field(&mut runtime, spawner, "origin"),
            vec_field(&mut runtime, spawner, "angles"),
            model,
            classname,
            runtime.objects.get(&spawner).cloned().unwrap_or_default(),
        )
    };
    let vehicle = spawn_vehicle(
        world,
        &classname,
        origin,
        angles,
        &model,
        Some(Heli::default()),
    )?;
    let Value::Object(id) = vehicle else {
        return Ok(vehicle);
    };
    let mut runtime = world.resource_mut::<RoundScript>();
    let skip = ["classname", "targetname"].map(|name| runtime.symbol(name));
    let body = runtime.objects.get_mut(&id).unwrap();
    for (field, value) in fields {
        if !skip.contains(&field) {
            body.entry(field).or_insert(value);
        }
    }
    Ok(vehicle)
}

/// Names the T5 refusals also claim; registered after them.
pub(crate) fn register_sp(registry: &mut NativeRegistry) {
    registry.register(Method, "setvehicleteam", |world, receiver, args| {
        heli(world, receiver)?;
        let team = string(args, 0)?;
        let object = super::super::natives::engine::entity_id(world, receiver)?;
        world.resource_mut::<RoundScript>().set_object_field(
            object,
            "script_team",
            Value::string(&team),
        );
        Ok(Value::Undefined)
    });
}
