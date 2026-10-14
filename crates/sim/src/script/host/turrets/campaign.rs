//! Campaign turrets: level `misc_turret`s, and the fire event turrets, actors
//! and vehicles raise when they shoot. Fork-only; kept out of the parent file
//! so upstream merges touch it as little as possible.

use super::super::entities::EntityKind;
use super::Turret;
use crate::frame::FrameWorld;
use crate::script::{RoundScript, Value};
use bevy_ecs::prelude::World;

/// A campaign level's `misc_turret` entities are turrets of their `weaponinfo`.
pub(crate) fn init_map_turrets(world: &mut World) {
    let turrets: Vec<(u64, String, [f32; 3])> = {
        let mut runtime = world.resource_mut::<RoundScript>();
        let ids: Vec<u64> = runtime
            .entities
            .iter()
            .filter(|(_, e)| e.kind == EntityKind::Map && &*e.classname == "misc_turret")
            .map(|(id, _)| *id)
            .collect();
        ids.into_iter()
            .filter_map(|id| {
                let Value::String(info) = runtime.object_field(id, "weaponinfo") else {
                    return None;
                };
                let origin = match runtime.object_field(id, "origin") {
                    Value::Vector(v) => v,
                    _ => [0.0; 3],
                };
                Some((id, info.to_string(), origin))
            })
            .collect()
    };
    for (id, info, origin) in turrets {
        let weapon = match crate::script_player::weapon_named(&FrameWorld::from_world(world), &info)
        {
            Ok(weapon) => weapon,
            Err(message) => {
                diag::warn!(Sim, "gsc: level turret: {message}");
                continue;
            }
        };
        let Ok(presence) = super::super::presence::spawn_presence(world, origin) else {
            continue;
        };
        let mut runtime = world.resource_mut::<RoundScript>();
        let entity = runtime.entities.get_mut(&id).unwrap();
        entity.kind = EntityKind::Spawned;
        entity.presence = Some(presence);
        runtime.new_models.push(id);
        // Fields as `spawnturret` sets them.
        runtime.engine.turrets.insert(
            id,
            Turret {
                weapon,
                mode: "manual".into(),
                owner: None,
                team: None,
                carried: false,
                fire_enabled: true,
                manual: None,
                target: None,
                firing: false,
                on_target: false,
                aim: [1.0, 0.0, 0.0],
                aim_initialized: false,
                scan_direction: 1.0,
                arcs: [180.0, 180.0, 90.0, 90.0],
                drop_pitch: 0.0,
                convergence: [0.0; 2],
                minimap: false,
                solid: false,
                minigun: false,
                mode_change_wait: 0.0,
            },
        );
    }
}

/// `EV_FIRE_WEAPON` on the shooting entity: clients play the fire sound at the
/// muzzle and the flash on its `tag_flash` bone, carried in `event_parm`.
pub(super) fn fire_event(world: &mut World, object: u64, from: [f32; 3], weapon: u32) {
    let Some(presence) = world
        .resource::<RoundScript>()
        .entities
        .get(&object)
        .and_then(|e| e.presence)
    else {
        return;
    };
    let frame = super::super::presence::settled(world);
    let Some(number) = frame.gentity_number(presence) else {
        return;
    };
    let bone = frame
        .entity_collision_capabilities()
        .iter()
        .find(|row| row.owner.script_model() == Some(presence))
        .and_then(|row| row.dobj.as_ref()?.tag_bone("tag_flash"))
        .and_then(|bone| i32::try_from(bone).ok())
        .unwrap_or(-1);
    let tick = world.resource::<crate::step::StepRequest>().tick;
    FrameWorld::from_world(world).push_entity_event(
        tick,
        crate::EventAudience::All,
        entity_iw4::EntityEventKind::FIRE_WEAPON,
        crate::EntityEventPayload {
            number,
            weapon,
            event_parm: bone,
            origin: from,
            ..Default::default()
        },
    );
}

/// `turret useby(player)`: the player mans the turret until it is used again
/// or deleted. Players only; actor gunners are not modelled.
pub(crate) fn register(registry: &mut crate::script::NativeRegistry) {
    use super::super::natives::engine::entity_id;
    use crate::script::Namespace::Method;
    registry.register(Method, "useturret", use_turret);
    registry.register(Method, "startfiring", |world, receiver, _| {
        let object = entity_id(world, receiver)?;
        let mut runtime = world.resource_mut::<RoundScript>();
        if !runtime.engine.turrets.contains_key(&object) {
            return Err("startfiring: not a turret".into());
        }
        runtime.campaign.turret_firing.entry(object).or_insert(0);
        Ok(Value::Undefined)
    });
    registry.register(Method, "stopfiring", |world, receiver, _| {
        let object = entity_id(world, receiver)?;
        let mut runtime = world.resource_mut::<RoundScript>();
        runtime.campaign.turret_firing.remove(&object);
        Ok(Value::Undefined)
    });
    registry.register(Method, "stopuseturret", |world, receiver, _| {
        stop_use_turret(world, receiver)
    });
    registry.register(Method, "canuseturret", |world, receiver, args| {
        super::super::natives::engine::entity_id(world, receiver)?;
        Ok(Value::Int(
            super::turret_of(world, super::super::args::arg(args, 0)?)
                .is_ok()
                .into(),
        ))
    });
    registry.register(Method, "getturret", |world, receiver, _| {
        let gunner = super::super::natives::engine::entity_id(world, receiver)?;
        Ok(world
            .resource::<RoundScript>()
            .campaign
            .turret_gunners
            .iter()
            .find(|(_, (g, _))| *g == gunner)
            .map_or(Value::Undefined, |(t, _)| Value::Object(*t)))
    });
    registry.register(
        crate::script::Namespace::Method,
        "useby",
        |world, receiver, args| {
            let object = super::turret_of(world, receiver)?;
            let user = super::super::args::arg(args, 0)?.clone();
            let runtime = world.resource::<RoundScript>();
            let Some(client) = runtime.player_client_of(&user) else {
                return Ok(Value::Undefined);
            };
            if runtime
                .campaign
                .turret_users
                .get(&object)
                .is_some_and(|(c, _)| *c == client)
            {
                release(world, object, client);
                return Ok(Value::Undefined);
            }
            let player = runtime.players.get(&client).map(|slot| slot.object);
            let mut runtime = world.resource_mut::<RoundScript>();
            runtime.campaign.turret_users.insert(object, (client, 0));
            if let Some(turret) = runtime.engine.turrets.get_mut(&object) {
                turret.owner = player;
            }
            set_weapons_held(world, client, true);
            Ok(Value::Undefined)
        },
    );
}

/// `actor useturret(turret)`: the turret takes the actor's side and aims at
/// whatever hostile it acquires; `stopuseturret` ends it. A turret with an
/// `aiScript` runs it on the actor (`animscripts/saw/stand`), which fires it;
/// one without fires on its own while the actor lives.
// ponytail: the gunner stays where it stood (its turret node), not placed on
// the turret's tag.
fn use_turret(world: &mut World, receiver: &Value, args: &[Value]) -> Result<Value, String> {
    let gunner = super::super::natives::engine::entity_id(world, receiver)?;
    let object = super::turret_of(world, super::super::args::arg(args, 0)?)?;
    let mut runtime = world.resource_mut::<RoundScript>();
    let team = match runtime.object_field(gunner, "team") {
        Value::String(team) => Some(std::sync::Arc::<str>::from(&*team)),
        _ => None,
    };
    if let Some(turret) = runtime.engine.turrets.get_mut(&object) {
        turret.owner = Some(gunner);
        turret.team = team;
        turret.mode = "auto_ai".into();
        turret.fire_enabled = true;
    }
    runtime.campaign.turret_gunners.insert(object, (gunner, 0));
    Ok(Value::Int(1))
}

fn stop_use_turret(world: &mut World, receiver: &Value) -> Result<Value, String> {
    let gunner = super::super::natives::engine::entity_id(world, receiver)?;
    let mut runtime = world.resource_mut::<RoundScript>();
    let manned: Vec<u64> = runtime
        .campaign
        .turret_gunners
        .iter()
        .filter(|(_, (g, _))| *g == gunner)
        .map(|(t, _)| *t)
        .collect();
    for object in manned {
        runtime.campaign.turret_gunners.remove(&object);
        if let Some(turret) = runtime.engine.turrets.get_mut(&object) {
            turret.owner = None;
            turret.mode = "manual".into();
            turret.firing = false;
        }
    }
    Ok(Value::Undefined)
}

/// The animscript the actor manning a turret runs: its weapon's `aiScript`.
pub(crate) fn gunner_script(world: &mut World, gunner: u64) -> Option<String> {
    let (weapon, scripts) = {
        let runtime = world.resource::<RoundScript>();
        let (turret, _) = runtime
            .campaign
            .turret_gunners
            .iter()
            .find(|(_, (g, _))| *g == gunner)?;
        (
            runtime.engine.turrets.get(turret)?.weapon,
            runtime.campaign.turret_ai_scripts.clone(),
        )
    };
    let name = crate::script_player::weapon_name(&FrameWorld::from_world(world), weapon);
    scripts.get(&name).cloned()
}

/// Turrets manned by actors without an animscript fire while on a target, one
/// shot per fire time.
pub(super) fn gunner_turrets(world: &mut World) {
    let gunners: Vec<(u64, u64, i64)> = world
        .resource::<RoundScript>()
        .campaign
        .turret_gunners
        .iter()
        .map(|(turret, (gunner, next))| (*turret, *gunner, *next))
        .collect();
    let now = super::super::players::now_ms(world);
    for (object, gunner, next) in gunners {
        let state = {
            let mut runtime = world.resource_mut::<RoundScript>();
            let alive = runtime.live(&gunner)
                && !matches!(runtime.object_field(gunner, "health"), Value::Int(h) if h <= 0);
            match runtime.engine.turrets.get(&object) {
                Some(turret) if alive => Some((
                    turret.on_target && turret.fire_enabled,
                    turret.weapon,
                    turret.aim,
                )),
                _ => None,
            }
        };
        let Some((firing, weapon, aim)) = state else {
            world
                .resource_mut::<RoundScript>()
                .campaign
                .turret_gunners
                .remove(&object);
            continue;
        };
        if !firing || now < next || gunner_script(world, gunner).is_some() {
            continue;
        }
        let interval = FrameWorld::from_world(world)
            .combat_facts_for(weapon)
            .map_or(100, |facts| facts.fire_time_ms.max(1));
        if let Some(entry) = world
            .resource_mut::<RoundScript>()
            .campaign
            .turret_gunners
            .get_mut(&object)
        {
            entry.1 = now + i64::from(interval);
        }
        let from = super::muzzle(world, object);
        super::fire_bullet(world, object, from, aim, weapon, None);
    }
}

/// Turrets under `startfiring` shoot where they aim at their weapon's fire rate.
pub(super) fn scripted_fire(world: &mut World) {
    let now = super::super::players::now_ms(world);
    let firing: Vec<(u64, i64)> = world
        .resource::<RoundScript>()
        .campaign
        .turret_firing
        .iter()
        .map(|(turret, next)| (*turret, *next))
        .collect();
    for (object, next) in firing {
        let turret = world
            .resource::<RoundScript>()
            .engine
            .turrets
            .get(&object)
            .map(|t| (t.weapon, t.aim, t.fire_enabled));
        let Some((weapon, aim, enabled)) = turret else {
            world
                .resource_mut::<RoundScript>()
                .campaign
                .turret_firing
                .remove(&object);
            continue;
        };
        if !enabled || now < next {
            continue;
        }
        let interval = FrameWorld::from_world(world)
            .combat_facts_for(weapon)
            .map_or(100, |facts| facts.fire_time_ms.max(1));
        world
            .resource_mut::<RoundScript>()
            .campaign
            .turret_firing
            .insert(object, now + i64::from(interval));
        let from = super::muzzle(world, object);
        super::fire_bullet(world, object, from, aim, weapon, None);
    }
}

fn set_weapons_held(world: &mut World, client: u32, held: bool) {
    let mut frame = FrameWorld::from_world(world);
    let id = crate::world::ClientId(client);
    if frame.client_meta(id).is_some() {
        frame.client_meta_mut(id).controls.weapons_disabled = held;
    }
}

fn release(world: &mut World, object: u64, client: u32) {
    let mut runtime = world.resource_mut::<RoundScript>();
    runtime.campaign.turret_users.remove(&object);
    if let Some(turret) = runtime.engine.turrets.get_mut(&object) {
        turret.firing = false;
    }
    set_weapons_held(world, client, false);
}

/// Manned turrets aim along their player's view and fire while attack is held,
/// one shot per the weapon's fire time.
pub(super) fn player_turrets(world: &mut World) {
    let users: Vec<(u64, u32, i64)> = world
        .resource::<RoundScript>()
        .campaign
        .turret_users
        .iter()
        .map(|(turret, (client, next))| (*turret, *client, *next))
        .collect();
    let now = super::super::players::now_ms(world);
    for (object, client, next) in users {
        let id = crate::world::ClientId(client);
        let live = {
            let runtime = world.resource::<RoundScript>();
            runtime.engine.turrets.contains_key(&object) && runtime.entities.contains_key(&object)
        };
        let (view, held) = {
            let mut frame = FrameWorld::from_world(world);
            let view = frame.player(id).map(|ps| ps.viewangles);
            (view, crate::script_player::buttons(&mut frame, id))
        };
        let Some(view) = view.filter(|_| live) else {
            release(world, object, client);
            continue;
        };
        let aim = math_iw4::angles_to_axis(view)[0];
        let firing = held & playerstate_iw4::buttons::ATTACK != 0;
        let weapon = {
            let mut runtime = world.resource_mut::<RoundScript>();
            let turret = runtime.engine.turrets.get_mut(&object).unwrap();
            turret.aim = aim;
            turret.aim_initialized = true;
            turret.firing = firing;
            turret.weapon
        };
        if !firing || now < next {
            continue;
        }
        let interval = FrameWorld::from_world(world)
            .combat_facts_for(weapon)
            .map_or(100, |facts| facts.fire_time_ms.max(1));
        if let Some(user) = world
            .resource_mut::<RoundScript>()
            .campaign
            .turret_users
            .get_mut(&object)
        {
            user.1 = now + i64::from(interval);
        }
        let from = super::muzzle(world, object);
        super::fire_bullet(world, object, from, aim, weapon, Some(id));
    }
}
