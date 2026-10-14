//! Campaign weapons: level `weapon_*` items and the stand-in owner of
//! ownerless level projectiles. Fork-only; kept out of the parent file so
//! upstream merges touch it as little as possible.

use super::super::entities::EntityKind;
use crate::frame::FrameWorld;
use crate::script::{RoundScript, Value};
use crate::world::ClientId;
use bevy_ecs::prelude::World;

/// Stand-in owner for a projectile a campaign level fires without one
/// (`magicbullet` without an owner, vehicle rockets): the level's one player.
// ponytail: world rockets count as the player's; give projectiles an optional
// owner if friendly-fire scripts react to them.
pub(crate) fn owner(world: &World) -> Option<ClientId> {
    let runtime = world.resource::<RoundScript>();
    if runtime.campaign.nodes.is_empty() {
        return None;
    }
    runtime.players.keys().next().copied().map(ClientId)
}

/// Puts an ownerless weapon item in the world; its item number.
pub(crate) fn place_weapon(
    world: &mut World,
    name: &str,
    origin: [f32; 3],
    angles: [f32; 3],
) -> Result<Option<i32>, String> {
    let tick = world.resource::<crate::step::StepRequest>().tick;
    let mut frame = FrameWorld::from_world(world);
    let weapon = crate::script_player::weapon_named(&frame, name)?;
    Ok(crate::item::place_weapon(
        &mut frame, tick, weapon, origin, angles,
    ))
}

fn combat_facts(
    world: &mut World,
    args: &[Value],
) -> Result<weapon_iw4::WeaponCombatFacts, String> {
    let name = super::super::args::string(args, 0)?;
    if name == "none" || name.is_empty() {
        return Ok(Default::default());
    }
    let frame = FrameWorld::from_world(world);
    let index = frame
        .weapon_index_by_script_name(&name)
        .ok_or_else(|| format!("unknown weapon '{name}'"))?;
    Ok(frame.weapon_combat_row(index).unwrap_or_default())
}

/// Fire-mode queries animscripts pick bursts by.
/// Gravity a manual magic grenade falls by (units/s²).
const GRENADE_GRAVITY: f32 = 800.0;

/// A magic grenade: explodes at `at` once level time reaches `due` (ms).
#[derive(Clone, Debug)]
pub(crate) struct MagicGrenade {
    due: i64,
    at: [f32; 3],
    weapon: u32,
    /// The script model flying to `at`, removed when it explodes.
    model: Option<u64>,
    /// Its flight under projectile physics (bounces, rolls, comes to rest),
    /// stepped each tick from level time `stepped`; none when the weapon has
    /// no projectile facts (the model then lands on `at`).
    flight: Option<(crate::equipment::ProjectileState, i32)>,
    /// The thrower's team (undefined for a level's magic grenade).
    team: Value,
}

/// A live magic grenade AI can see: its model, where it comes to rest, when it
/// explodes (level ms), its weapon and the thrower's team.
pub(crate) struct LiveGrenade {
    pub model: u64,
    pub at: [f32; 3],
    pub due: i64,
    pub weapon: u32,
    pub team: Value,
}

pub(crate) fn live_grenades(world: &World) -> Vec<LiveGrenade> {
    world
        .resource::<RoundScript>()
        .campaign
        .magic_grenades
        .iter()
        .filter_map(|g| {
            Some(LiveGrenade {
                model: g.model?,
                at: g.at,
                due: g.due,
                weapon: g.weapon,
                team: g.team.clone(),
            })
        })
        .collect()
}

/// Pushes back the explosion of the grenade flying as `model` by `ms`.
pub(crate) fn delay_grenade(world: &mut World, model: u64, ms: i64) {
    let mut runtime = world.resource_mut::<RoundScript>();
    if let Some(g) = runtime
        .campaign
        .magic_grenades
        .iter_mut()
        .find(|g| g.model == Some(model))
    {
        g.due += ms;
    }
}

/// Takes the grenade flying as `model` out of the world (picked up): its
/// weapon and the fuse it had left (ms).
pub(crate) fn take_grenade(world: &mut World, model: u64) -> Option<(u32, i64)> {
    let now = super::super::players::now_ms(world);
    let mut runtime = world.resource_mut::<RoundScript>();
    let index = runtime
        .campaign
        .magic_grenades
        .iter()
        .position(|g| g.model == Some(model))?;
    let g = runtime.campaign.magic_grenades.remove(index);
    if runtime.entities.contains_key(&model) {
        runtime.request_delete(model);
    }
    Some((g.weapon, (g.due - now).max(0)))
}

/// The launch velocity that carries a grenade from `from` onto `at` on a
/// gravity arc, and its flight time (s), at most `fuse_ms`.
fn arc(from: [f32; 3], at: [f32; 3], fuse_ms: i32) -> ([f32; 3], f32) {
    let d: [f32; 3] = std::array::from_fn(|k| at[k] - from[k]);
    let reach = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
    let fuse = (fuse_ms as f32 / 1000.0).max(0.1);
    let t = (reach / 600.0).clamp(0.3, 1.5).min(fuse);
    ([d[0] / t, d[1] / t, d[2] / t + 0.5 * GRENADE_GRAVITY * t], t)
}

/// A thrown grenade: its script model, its physics flight, and where that
/// flight comes to rest by the fuse.
struct Flight {
    model: Option<u64>,
    state: Option<(crate::equipment::ProjectileState, i32)>,
    lands: Option<[f32; 3]>,
}

/// A script model of the weapon's world model thrown from `from` at
/// `velocity`. With projectile facts it flies under the weapon's own bounce
/// physics (ownerless: it hits players and actors alike); without, it rides
/// `movegravity` onto `at`.
// ponytail: the model does not spin; read the weapon's spin rates if it looks stiff.
fn fly(
    world: &mut World,
    weapon: u32,
    from: [f32; 3],
    velocity: [f32; 3],
    at: [f32; 3],
    fuse_ms: i32,
) -> Flight {
    let tick = world.resource::<crate::step::StepRequest>().tick;
    let time = crate::level_time_ms(tick);
    let frame = FrameWorld::from_world(world);
    let bounces = frame
        .equipment_facts_for(weapon)
        .is_some_and(|f| f.parallel_bounce.is_some() && f.perpendicular_bounce.is_some());
    let state = bounces.then(|| crate::equipment::ProjectileState {
        id: crate::ProjectileId(u32::MAX),
        owner: ClientId(u32::MAX),
        owner_life: crate::LifeSequence::default(),
        weapon,
        origin: from,
        velocity,
        pos: entity_iw4::init_grenade_pos(from, velocity, time),
        apos: Default::default(),
        entnum: -1,
        launch_time: time,
        spawn_time_ms: time,
        detonate_at_ms: None,
        cleanup_at_ms: i32::MAX,
        travel_distance: 0.0,
        live: true,
        stuck_pane: None,
        grounded: false,
        guide: Default::default(),
        attached_to: None,
    });
    let lands = state.and_then(|state| {
        crate::equipment::predict_projectile(&frame, state, time, fuse_ms.max(0))
    });
    let model = spawn_model(world, weapon, from);
    if state.is_none()
        && let Some(model) = model
    {
        ride_arc(world, model, from, at, fuse_ms);
    }
    Flight {
        model,
        state: state.map(|state| (state, time)),
        lands,
    }
}

fn spawn_model(world: &mut World, weapon: u32, from: [f32; 3]) -> Option<u64> {
    use crate::script::Namespace::{Function, Method};
    let model = FrameWorld::from_world(world)
        .weapon_world_model(weapon)?
        .0
        .to_owned();
    let natives = world.resource::<RoundScript>().restart.clone()?;
    let native = |namespace, name| natives.natives.get(namespace, name);
    let (spawn, set_model) = (native(Function, "spawn")?, native(Method, "setmodel")?);
    let entity = spawn(
        world,
        &Value::Undefined,
        &[Value::string("script_model"), Value::Vector(from)],
    )
    .ok()?;
    set_model(world, &entity, &[Value::string(&model)]).ok()?;
    match entity {
        Value::Object(id) => Some(id),
        _ => None,
    }
}

/// `movegravity` onto `at` and stay there.
fn ride_arc(world: &mut World, model: u64, from: [f32; 3], at: [f32; 3], fuse_ms: i32) {
    let Some(move_gravity) = world
        .resource::<RoundScript>()
        .restart
        .clone()
        .and_then(|r| r.natives.get(crate::script::Namespace::Method, "movegravity"))
    else {
        return;
    };
    let (velocity, t) = arc(from, at, fuse_ms);
    let _ = move_gravity(
        world,
        &Value::Object(model),
        &[Value::Vector(velocity), Value::Float(t)],
    );
}

/// A step shorter than this means the grenade has come to rest.
const REST_STEP: f32 = 0.5;

/// Steps every flying grenade to this tick and moves its model there.
fn step_flights(world: &mut World) {
    let tick = world.resource::<crate::step::StepRequest>().tick;
    let now = crate::level_time_ms(tick);
    let flights: Vec<(usize, crate::equipment::ProjectileState, i32)> = world
        .resource::<RoundScript>()
        .campaign
        .magic_grenades
        .iter()
        .enumerate()
        .filter_map(|(i, g)| g.flight.map(|(state, stepped)| (i, state, stepped)))
        .filter(|(_, state, stepped)| {
            *stepped < now && state.pos.tr_type != entity_iw4::TR_STATIONARY
        })
        .collect();
    if flights.is_empty() {
        return;
    }
    let frame = FrameWorld::from_world(world);
    let stepped: Vec<(usize, crate::equipment::ProjectileState)> = flights
        .into_iter()
        .map(|(i, state, from)| {
            let next =
                crate::equipment::predict_projectile_state(&frame, state, from, now - from)
                    .filter(|next| {
                        let d: [f32; 3] = std::array::from_fn(|k| next.origin[k] - state.origin[k]);
                        (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt() >= REST_STEP
                    });
            (i, next.unwrap_or_else(|| at_rest(state)))
        })
        .collect();
    let mut runtime = world.resource_mut::<RoundScript>();
    for (i, state) in stepped {
        let grenade = &mut runtime.campaign.magic_grenades[i];
        grenade.flight = Some((state, now));
        if let Some(model) = grenade.model
            && runtime.entities.contains_key(&model)
        {
            runtime.set_object_field(model, "origin", Value::Vector(state.origin));
        }
    }
}

fn at_rest(mut state: crate::equipment::ProjectileState) -> crate::equipment::ProjectileState {
    state.velocity = [0.0; 3];
    state.pos.tr_type = entity_iw4::TR_STATIONARY;
    state.pos.tr_base = state.origin;
    state.pos.tr_delta = [0.0; 3];
    state
}

/// `magicgrenade`/`magicgrenademanual`, as a function or on an actor (whose
/// `grenadeweapon` it throws): schedules the weapon's explosion where the
/// grenade lands, after `timer` seconds (else the weapon's fuse).
fn magic_grenade(
    world: &mut World,
    receiver: &Value,
    args: &[Value],
    manual: bool,
) -> Result<Value, String> {
    use super::super::args::{float, optional, string, vector};
    // On an actor the weapon is its own: the first argument is already a position.
    let method = matches!(args.first(), Some(Value::Vector(_)));
    let (name, rest) = if method {
        let id = super::super::natives::engine::entity_id(world, receiver)?;
        let name = match world
            .resource_mut::<RoundScript>()
            .object_field(id, "grenadeweapon")
        {
            Value::String(name) => name.to_string(),
            _ => "fraggrenade".to_owned(),
        };
        (name, args)
    } else {
        (string(args, 0)?, &args[1..])
    };
    let frame = FrameWorld::from_world(world);
    let weapon = crate::script_player::weapon_named(&frame, &name)?;
    let fuse = frame
        .equipment_facts_for(weapon)
        .map_or(3000, |facts| facts.fuse_time_ms.max(0));
    let from = vector(rest, 0)?;
    let ms = optional(rest, 2, float)?.map_or(fuse, |s| (s * 1000.0) as i32);
    let (at, velocity) = if manual {
        let velocity = vector(rest, 1)?;
        let t = ms as f32 / 1000.0;
        let to = [
            from[0] + velocity[0] * t,
            from[1] + velocity[1] * t,
            from[2] + velocity[2] * t - 0.5 * GRENADE_GRAVITY * t * t,
        ];
        let trace = super::super::presence::settled(world).trace_world(
            from,
            to,
            [0.0; 3],
            [0.0; 3],
            crate::bullet_collision::MASK_SHOT,
        );
        let at = std::array::from_fn(|k| from[k] + (to[k] - from[k]) * trace.fraction);
        (at, velocity)
    } else {
        let at = vector(rest, 1)?;
        (at, arc(from, at, ms).0)
    };
    let due = super::super::players::now_ms(world) + i64::from(ms);
    let flight = fly(world, weapon, from, velocity, at, ms);
    let team = if method {
        let id = super::super::natives::engine::entity_id(world, receiver)?;
        world.resource_mut::<RoundScript>().object_field(id, "team")
    } else {
        Value::Undefined
    };
    world
        .resource_mut::<RoundScript>()
        .campaign
        .magic_grenades
        .push(MagicGrenade {
            due,
            at: flight.lands.unwrap_or(at),
            weapon,
            model: flight.model,
            flight: flight.state,
            team,
        });
    Ok(Value::Undefined)
}

/// Schedules `weapon` (by name), thrown by `team` from `from`, to explode at
/// `at` after `fuse` ms (else the weapon's fuse).
pub(crate) fn schedule_grenade(
    world: &mut World,
    name: &str,
    from: [f32; 3],
    at: [f32; 3],
    team: Value,
    fuse: Option<i64>,
) -> Result<(), String> {
    let frame = FrameWorld::from_world(world);
    let weapon = crate::script_player::weapon_named(&frame, name)?;
    let fuse = fuse.map_or_else(
        || {
            frame
                .equipment_facts_for(weapon)
                .map_or(3000, |facts| facts.fuse_time_ms.max(0))
        },
        |ms| i32::try_from(ms).unwrap_or(i32::MAX),
    );
    let due = super::super::players::now_ms(world) + i64::from(fuse);
    let flight = fly(world, weapon, from, arc(from, at, fuse).0, at, fuse);
    world
        .resource_mut::<RoundScript>()
        .campaign
        .magic_grenades
        .push(MagicGrenade {
            due,
            at: flight.lands.unwrap_or(at),
            weapon,
            model: flight.model,
            flight: flight.state,
            team,
        });
    Ok(())
}

/// Explodes the magic grenades that are due.
pub(crate) fn advance_magic_grenades(world: &mut World) {
    step_flights(world);
    let now = super::super::players::now_ms(world);
    let due: Vec<MagicGrenade> = {
        let mut runtime = world.resource_mut::<RoundScript>();
        let (due, later) = std::mem::take(&mut runtime.campaign.magic_grenades)
            .into_iter()
            .partition(|g| g.due <= now);
        runtime.campaign.magic_grenades = later;
        due
    };
    let tick = world.resource::<crate::step::StepRequest>().tick;
    for grenade in due {
        // A flying grenade goes off where its flight has it now.
        let at = grenade.flight.map_or(grenade.at, |(state, _)| state.origin);
        if let Some(model) = grenade.model {
            let mut runtime = world.resource_mut::<RoundScript>();
            if runtime.entities.contains_key(&model) {
                runtime.request_delete(model);
            }
        }
        let mut frame = FrameWorld::from_world(world);
        let Some(facts) = frame.equipment_facts_for(grenade.weapon) else {
            continue;
        };
        frame.push_entity_event(
            tick,
            crate::EventAudience::All,
            entity_iw4::EntityEventKind::GRENADE_EXPLODE,
            crate::EntityEventPayload {
                number: i32::from(trace_iw4::ENTITYNUM_WORLD),
                attacker_entity_num: i32::from(trace_iw4::ENTITYNUM_WORLD),
                weapon: grenade.weapon,
                origin: at,
                direction: [0.0, 0.0, 1.0],
                ..Default::default()
            },
        );
        world
            .resource_mut::<RoundScript>()
            .blasts
            .push(super::super::entity_damage::ScriptBlast {
                origin: at,
                radius: facts.explosion_radius.max(0) as f32,
                max: facts.explosion_inner_damage.max(0) as f32,
                min: facts.explosion_outer_damage.max(0) as f32,
                attacker: None,
                inflictor: None,
                means: "MOD_GRENADE_SPLASH",
                weapon: grenade.weapon,
            });
    }
}

pub(crate) fn register_sp(registry: &mut crate::script::NativeRegistry) {
    use crate::script::Namespace::{Function, Method};
    for namespace in [Function, Method] {
        registry.register(namespace, "magicgrenade", |world, receiver, args| {
            magic_grenade(world, receiver, args, false)
        });
        registry.register(namespace, "magicgrenademanual", |world, receiver, args| {
            magic_grenade(world, receiver, args, true)
        });
    }
    registry.register(Function, "weaponhasthermalscope", |world, _, args| {
        let name = super::super::args::string(args, 0)?;
        let frame = FrameWorld::from_world(world);
        Ok(Value::Int(
            frame
                .weapon_index_by_script_name(&name)
                .is_some_and(|w| frame.weapon_thermal_scope(w))
                .into(),
        ))
    });
    registry.register(Function, "weaponisauto", |world, _, args| {
        Ok(Value::Int(
            (combat_facts(world, args)?.fire_type == 0).into(),
        ))
    });
    registry.register(Function, "weaponburstcount", |world, _, args| {
        let fire_type = combat_facts(world, args)?.fire_type;
        Ok(Value::Int(if (2..=4).contains(&fire_type) {
            fire_type
        } else {
            0
        }))
    });
    registry.register(Function, "weaponisboltaction", |world, _, args| {
        Ok(Value::Int(combat_facts(world, args)?.bolt_action.into()))
    });
}

/// A level's `weapon_<name>` entities become the weapon items they stand for.
pub(crate) fn place_map_weapons(world: &mut World) {
    let weapons: Vec<(u64, String)> = world
        .resource::<RoundScript>()
        .entities
        .iter()
        .filter(|(_, e)| e.kind == EntityKind::Map)
        .filter_map(|(id, e)| Some((*id, e.classname.strip_prefix("weapon_")?.to_owned())))
        .collect();
    for (id, name) in weapons {
        let (origin, angles) = {
            let mut runtime = world.resource_mut::<RoundScript>();
            let vector = |value| match value {
                Value::Vector(v) => v,
                _ => [0.0; 3],
            };
            (
                vector(runtime.object_field(id, "origin")),
                vector(runtime.object_field(id, "angles")),
            )
        };
        match place_weapon(world, &name, origin, angles) {
            Ok(Some(number)) => {
                if let Some(entity) = world.resource_mut::<RoundScript>().entities.get_mut(&id) {
                    entity.kind = EntityKind::Item(number);
                }
            }
            Ok(None) => {}
            Err(message) => diag::warn!(Sim, "gsc: level weapon {name}: {message}"),
        }
    }
}

/// The missile `launch` returned was fired for the player as a stand-in
/// owner: its impact is nobody's.
pub(crate) fn mark_ownerless(world: &mut World, missile: &Value) {
    let Value::Object(object) = *missile else {
        return;
    };
    let mut runtime = world.resource_mut::<RoundScript>();
    let id = runtime
        .missiles
        .iter()
        .find_map(|(id, o)| (*o == object).then_some(*id));
    if let Some(id) = id {
        runtime.campaign.ownerless_missiles.insert(id);
    }
}

/// SP missile impacts: an owned projectile that deals explosion damage
/// notifies its owner `projectile_impact` (weapon, position, explosion
/// radius); a projectile-type weapon's impact is also an AI event heard along
/// the line from the owner.
pub(crate) fn projectile_impacts(world: &mut World, notes: &[super::WeaponNote]) {
    if !world.resource::<RoundScript>().campaign.active {
        return;
    }
    let mut runtime = world.resource_mut::<RoundScript>();
    let runtime = &mut *runtime;
    let missiles = &runtime.missiles;
    runtime
        .campaign
        .ownerless_missiles
        .retain(|id| missiles.contains_key(id));
    for note in notes {
        let super::WeaponNote::Detonated {
            id,
            owner,
            weapon,
            origin,
        } = *note
        else {
            continue;
        };
        if world
            .resource_mut::<RoundScript>()
            .campaign
            .ownerless_missiles
            .remove(&id)
        {
            continue;
        }
        let Value::Object(player) = super::super::players::player_object(world, owner.0) else {
            continue;
        };
        let frame = FrameWorld::from_world(world);
        let facts = frame.equipment_facts_for(weapon);
        let projectile = frame
            .combat_facts_for(weapon)
            .is_some_and(|facts| facts.weap_type == weapon_iw4::WEAPTYPE_PROJECTILE);
        if let Some(facts) = facts.filter(|facts| facts.explosion_inner_damage != 0) {
            let name = super::super::players::script_weapon(world, owner.0, weapon);
            let args = vec![
                super::weapon_name(world, name),
                Value::Vector(origin),
                Value::Float(facts.explosion_radius as f32),
            ];
            super::raise(world, Value::Object(player), "projectile_impact", args);
        }
        if projectile {
            super::super::actors::projectile_impact(world, player, origin);
        }
    }
}
