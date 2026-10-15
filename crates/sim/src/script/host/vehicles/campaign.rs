//! Campaign vehicles: map `script_vehicle`s, spawner `vehicle_dospawn()`,
//! ground paths over `info_vehicle_node`s. Fork-only; kept out of the parent
//! file so upstream merges touch it as little as possible.

use super::super::args::{float, string};
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
    vehicle.path_speed = vehicle.max_speed;
    if vehicle.accel <= 0.0 {
        vehicle.accel = vehicle.max_speed.max(MPH);
    }
    if vehicle.decel <= 0.0 {
        vehicle.decel = vehicle.accel;
    }
}

/// Vehicle object fields holding a pending `setswitchnode`.
const SWITCH_FROM: &str = "switchnode_from";
const SWITCH_TO: &str = "switchnode_to";

/// The node a vehicle that just reached `reached` switches to, once.
pub(super) fn take_switch(runtime: &mut RoundScript, vehicle: u64, reached: u64) -> Option<u64> {
    if runtime.object_field(vehicle, SWITCH_FROM) != Value::Object(reached) {
        return None;
    }
    let Value::Object(to) = runtime.object_field(vehicle, SWITCH_TO) else {
        return None;
    };
    runtime.set_object_field(vehicle, SWITCH_FROM, Value::Undefined);
    runtime.set_object_field(vehicle, SWITCH_TO, Value::Undefined);
    Some(to)
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

/// Top speed a player drives at (mph) and how fast it is reached (mph/s).
const DRIVE_MPH: f32 = 60.0;
const DRIVE_ACCEL_MPH: f32 = 25.0;
/// Coasting loss (mph/s) and steering rate at full lock (degrees/s).
const DRIVE_DRAG_MPH: f32 = 10.0;
const DRIVE_TURN: f32 = 90.0;
/// Ground is searched this far above and below the vehicle.
const GROUND_PROBE: f32 = 128.0;

/// Player-driven vehicles (`mountvehicle`): the stick sets throttle and
/// steering; the vehicle follows the ground and stops at walls.
// ponytail: arcade handling, one tune for snowmobile and boat; read the
// vehicle's physics def if a level needs real handling.
pub(super) fn drive(world: &mut World) {
    let drivers: Vec<(u32, u64)> = world
        .resource::<RoundScript>()
        .campaign
        .drivers
        .iter()
        .map(|(client, vehicle)| (*client, *vehicle))
        .collect();
    for (client, id) in drivers {
        let (forward, right) = {
            let input = &world.resource::<crate::step::StepRequest>().input;
            input
                .cmds
                .iter()
                .rev()
                .find(|command| command.client.0 == client)
                .map_or((0.0, 0.0), |command| {
                    (
                        f32::from(command.command.forwardmove) / 127.0,
                        f32::from(command.command.rightmove) / 127.0,
                    )
                })
        };
        let dt = crate::MATCH_TICK_MS as f32 / 1000.0;
        let (origin, mut angles, speed) = {
            let mut runtime = world.resource_mut::<RoundScript>();
            if !runtime.entities.contains_key(&id) || !runtime.vehicles.contains_key(&id) {
                runtime.campaign.drivers.remove(&client);
                continue;
            }
            let origin = vec_field(&mut runtime, id, "origin");
            let angles = vec_field(&mut runtime, id, "angles");
            let vehicle = runtime.vehicles.get_mut(&id).unwrap();
            vehicle.goal = None;
            vehicle.path_running = false;
            let mut speed = vehicle.speed + forward * DRIVE_ACCEL_MPH * MPH * dt;
            if forward.abs() < 0.1 {
                speed -= speed.signum() * (DRIVE_DRAG_MPH * MPH * dt).min(speed.abs());
            }
            let speed = speed.clamp(-DRIVE_MPH * MPH * 0.25, DRIVE_MPH * MPH);
            (origin, angles, speed)
        };
        angles[1] = math_iw4::angle_normalize_360(
            angles[1]
                - right * DRIVE_TURN * dt * (speed / (DRIVE_MPH * MPH)).clamp(-1.0, 1.0).signum(),
        );
        let yaw = angles[1].to_radians();
        let step = [yaw.cos() * speed * dt, yaw.sin() * speed * dt];
        let lift = [origin[0], origin[1], origin[2] + 32.0];
        let to = [lift[0] + step[0], lift[1] + step[1], lift[2]];
        let settled = super::super::presence::settled(world);
        let blocked = settled.trace_world(
            lift,
            to,
            [-24.0, -24.0, 0.0],
            [24.0, 24.0, 32.0],
            crate::bullet_collision::MASK_PLAYER_SOLID,
        );
        let (next, speed) = if blocked.fraction < 1.0 {
            (origin, 0.0)
        } else {
            let top = [to[0], to[1], origin[2] + GROUND_PROBE];
            let floor = settled.trace_world(
                top,
                [to[0], to[1], origin[2] - GROUND_PROBE],
                [0.0; 3],
                [0.0; 3],
                crate::bullet_collision::MASK_PLAYER_SOLID,
            );
            let z = if floor.fraction < 1.0 {
                top[2] + (origin[2] - GROUND_PROBE - top[2]) * floor.fraction
            } else {
                origin[2]
            };
            ([to[0], to[1], z], speed)
        };
        let mut runtime = world.resource_mut::<RoundScript>();
        runtime.set_object_field(id, "origin", Value::Vector(next));
        runtime.set_object_field(id, "angles", Value::Vector(angles));
        if let Some(vehicle) = runtime.vehicles.get_mut(&id) {
            vehicle.speed = speed;
        }
    }
}

/// The driver's stick on `vehicle`: (throttle, steering), each in -1..1.
fn driver_stick(world: &mut World, receiver: &Value) -> Result<(f32, f32), String> {
    let id = super::super::natives::engine::entity_id(world, receiver)?;
    let Some(client) = world
        .resource::<RoundScript>()
        .campaign
        .drivers
        .iter()
        .find(|(_, vehicle)| **vehicle == id)
        .map(|(client, _)| *client)
    else {
        return Ok((0.0, 0.0));
    };
    let input = &world.resource::<crate::step::StepRequest>().input;
    Ok(input
        .cmds
        .iter()
        .rev()
        .find(|command| command.client.0 == client)
        .map_or((0.0, 0.0), |command| {
            (
                f32::from(command.command.forwardmove) / 127.0,
                -f32::from(command.command.rightmove) / 127.0,
            )
        }))
}

/// `mountvehicle(vehicle)`: the player drives it from its `tag_player`.
fn mount(world: &mut World, receiver: &Value, args: &[Value]) -> Result<Value, String> {
    let client = super::super::natives::player::player(world, receiver)?;
    let vehicle = super::super::args::arg(args, 0)?.clone();
    heli(world, &vehicle)?;
    let id = super::super::natives::engine::entity_id(world, &vehicle)?;
    super::super::natives::player::link_to(
        world,
        receiver,
        &[vehicle.clone(), Value::string("tag_player")],
        super::super::players::LinkView::Delta,
    )?;
    let mut runtime = world.resource_mut::<RoundScript>();
    runtime.campaign.drivers.insert(client, id);
    if let Some(slot) = runtime.players.get(&client).map(|slot| slot.object) {
        runtime.set_object_field(slot, "vehicle", vehicle);
    }
    Ok(Value::Undefined)
}

fn dismount(world: &mut World, receiver: &Value) -> Result<Value, String> {
    let client = super::super::natives::player::player(world, receiver)?;
    super::super::players::unlink_player(world, client);
    let mut runtime = world.resource_mut::<RoundScript>();
    if let Some(id) = runtime.campaign.drivers.remove(&client)
        && let Some(vehicle) = runtime.vehicles.get_mut(&id)
    {
        vehicle.speed = 0.0;
    }
    if let Some(slot) = runtime.players.get(&client).map(|slot| slot.object) {
        runtime.set_object_field(slot, "vehicle", Value::Undefined);
    }
    Ok(Value::Undefined)
}

/// Names the T5 refusals also claim; registered after them.
pub(crate) fn register_sp(registry: &mut NativeRegistry) {
    registry.register(Method, "mountvehicle", mount);
    registry.register(Method, "vehicle_getthrottle", |world, receiver, _| {
        Ok(Value::Float(driver_stick(world, receiver)?.0.max(0.0)))
    });
    registry.register(Method, "vehicle_getsteering", |world, receiver, _| {
        Ok(Value::Float(driver_stick(world, receiver)?.1))
    });
    registry.register(Method, "dismountvehicle", |world, receiver, _| {
        dismount(world, receiver)
    });
    // `resumespeed(accel)`: back to the speed its path set, after a `vehicle_setspeed`.
    registry.register(Method, "resumespeed", |world, receiver, args| {
        let accel = float(args, 0)?.max(1.0) * MPH;
        let vehicle = heli(world, receiver)?;
        vehicle.max_speed = vehicle.path_speed;
        vehicle.accel = accel;
        Ok(Value::Undefined)
    });
    // `setswitchnode(from, to)`: reaching `from` the vehicle goes on to `to`
    // instead of `from`'s target; one already past `from` turns to `to` now.
    registry.register(Method, "setswitchnode", |world, receiver, args| {
        use super::super::natives::engine::entity_id;
        heli(world, receiver)?;
        let vehicle = entity_id(world, receiver)?;
        let from = entity_id(world, super::super::args::arg(args, 0)?)?;
        let to = entity_id(world, super::super::args::arg(args, 1)?)?;
        let mut runtime = world.resource_mut::<RoundScript>();
        let goal = super::node(&mut runtime, to)?;
        let heading = runtime.vehicles[&vehicle].path_node;
        if heading.is_some() && super::next_node(&mut runtime, from)? == heading {
            let last = super::next_node(&mut runtime, to)?.is_none();
            let v = runtime.vehicles.get_mut(&vehicle).unwrap();
            v.path_node = Some(to);
            let stop = !v.ground || last;
            super::set_goal(v, goal, stop);
        } else {
            runtime.set_object_field(vehicle, SWITCH_FROM, Value::Object(from));
            runtime.set_object_field(vehicle, SWITCH_TO, Value::Object(to));
        }
        Ok(Value::Undefined)
    });
    // `setwaitspeed(mph)`: the speed it holds while waiting (0 when its driver dies).
    // ponytail: taken as its speed now; the path's speed comes back on `resumespeed`.
    registry.register(Method, "setwaitspeed", |world, receiver, args| {
        let mph = float(args, 0)?;
        if mph < 0.0 {
            return Err("Cannot have a negative wait speed on a vehicle".into());
        }
        heli(world, receiver)?.max_speed = mph * MPH;
        Ok(Value::Undefined)
    });
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
