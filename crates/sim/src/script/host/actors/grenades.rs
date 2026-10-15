//! SP AI grenade awareness: every 250 ms an actor near a live
//! grenade (`ai_eventDistGrenadePing`, busy distance while it has an enemy)
//! may notice it — always without an enemy, else with `grenadeawareness`
//! chance. It sets `grenade`, hears `grenade danger` when a hostile threw it,
//! and, inside the blast (explosion radius + 128), throws it back from 64
//! units (`grenade_return_throw`, the fuse stretched to give it time), else
//! stays at a cover node that shields it, else runs out of the blast, else
//! cowers (`grenade_cower`). Fork-only; hooks in
//! `actors.rs`.

use bevy_ecs::prelude::World;

use super::{ActorGoal, GoalTarget, Hold, brain, distance, hostile, live_actors, number, store};
use crate::frame::FrameWorld;
use crate::script::{RoundScript, Value};

/// Grenade danger pings this often.
const PING_MS: i64 = 250;
/// An actor stays this much farther than the explosion radius to be safe.
const SAFE_MARGIN: f32 = 128.0;
/// Close enough to pick the grenade up and throw it back.
const THROWBACK_DIST: f32 = 64.0;
/// A grenade about to be thrown back with less fuse than this gets
/// `FUSE_GRACE_MS` more.
const FUSE_SHORT_MS: i64 = 3_000;
const FUSE_GRACE_MS: i64 = 1_000;
/// A thrown-back grenade flies at the enemy, else this far ahead.
const THROWBACK_RANGE: f32 = 1_000.0;

/// A live grenade: its script entity, where it is (or lands), when it
/// explodes (level ms), its weapon, the thrower's team, and the projectile
/// slot of a player's grenade.
struct Danger {
    object: u64,
    at: [f32; 3],
    due: i64,
    weapon: u32,
    team: Value,
    projectile: Option<i32>,
}

fn dangers(world: &mut World) -> Vec<Danger> {
    let mut dangers: Vec<Danger> = super::super::weapons::campaign::live_grenades(world)
        .into_iter()
        .map(|g| Danger {
            object: g.model,
            at: g.at,
            due: g.due,
            weapon: g.weapon,
            team: g.team,
            projectile: None,
        })
        .collect();
    let tracked: Vec<(crate::ProjectileId, u64)> = {
        let runtime = world.resource::<RoundScript>();
        runtime
            .missiles
            .iter()
            .filter(|(_, object)| {
                runtime
                    .entities
                    .get(object)
                    .is_some_and(|e| e.classname.as_ref() == "grenade")
            })
            .map(|(id, object)| (*id, *object))
            .collect()
    };
    if tracked.is_empty() {
        return dangers;
    }
    let projectiles = crate::frame::collect_projectiles(world);
    for (id, object) in tracked {
        let Some(p) = projectiles.iter().find(|p| p.id == id && p.live) else {
            continue;
        };
        let Some(due) = p.detonate_at_ms else {
            continue;
        };
        let team = match super::super::players::player_object(world, p.owner.0) {
            Value::Object(owner) => super::super::players::entity_field(world, owner, "team"),
            _ => Value::Undefined,
        };
        let at = match world.resource_mut::<RoundScript>().object_field(object, "origin") {
            Value::Vector(at) => at,
            _ => p.origin,
        };
        dangers.push(Danger {
            object,
            at,
            due: i64::from(due),
            weapon: p.weapon,
            team,
            projectile: Some(p.entnum),
        });
    }
    dangers
}

/// Once per tick: drops `grenade` from actors whose grenade is gone (and
/// ends their flight), then on the ping lets actors notice new ones.
pub(super) fn advance(world: &mut World) {
    let now = super::super::players::now_ms(world);
    let dangers = dangers(world);
    let ids = live_actors(&world.resource::<RoundScript>());
    let ping = now / PING_MS != (now - i64::from(crate::MATCH_TICK_MS)) / PING_MS;
    for id in ids {
        let held = match world.resource_mut::<RoundScript>().object_field(id, "grenade") {
            Value::Object(grenade) => Some(grenade),
            _ => None,
        };
        if let Some(grenade) = held {
            if !dangers.iter().any(|d| d.object == grenade) {
                forget(world, id);
            }
            continue;
        }
        if ping {
            notice(world, id, &dangers);
        }
    }
}

/// The grenade is gone: `grenade` clears and a flight hands back the goal it replaced.
fn forget(world: &mut World, id: u64) {
    let mut brain = brain(world, id);
    let mut runtime = world.resource_mut::<RoundScript>();
    runtime.set_object_field(id, "grenade", Value::Undefined);
    if let Some((to, saved)) = brain.grenade_flight.take() {
        // A goal a script set during the flight stands.
        let fleeing = runtime
            .campaign
            .actor_goals
            .get(&id)
            .is_some_and(|goal| goal.target == GoalTarget::Pos(to));
        match saved {
            Some(mut goal) if fleeing => {
                goal.routed_to = None;
                runtime.campaign.actor_goals.insert(id, goal);
            }
            None if fleeing => {
                runtime.campaign.actor_goals.remove(&id);
            }
            _ => {}
        }
        drop(runtime);
        store(world, id, brain);
    }
}

fn notice(world: &mut World, id: u64, dangers: &[Danger]) {
    // Scenes, custom anims, pain and death keep the actor.
    if brain(world, id).hold.is_some_and(|hold| hold != Hold::Arrival) {
        return;
    }
    let (origin, team, enemy, awareness, no_return) = {
        let mut runtime = world.resource_mut::<RoundScript>();
        let Value::Vector(origin) = runtime.object_field(id, "origin") else {
            return;
        };
        let enemy = match runtime.object_field(id, "enemy") {
            Value::Object(enemy) if runtime.live(&enemy) => Some(enemy),
            _ => None,
        };
        (
            origin,
            runtime.object_field(id, "team"),
            enemy,
            number(runtime.object_field(id, "grenadeawareness")).unwrap_or(0.0),
            super::truthy(&runtime.object_field(id, "nogrenadereturnthrow")),
        )
    };
    let reach = super::events::reach(world, "grenade danger", enemy.is_some());
    let Some(danger) = dangers
        .iter()
        .filter(|d| distance(origin, d.at) <= reach)
        .min_by(|a, b| distance(origin, a.at).total_cmp(&distance(origin, b.at)))
    else {
        return;
    };
    if awareness <= 0.0
        || (enemy.is_some() && super::super::natives::math::random_unit(world) > awareness)
    {
        return;
    }
    let grenade = Value::Object(danger.object);
    world
        .resource_mut::<RoundScript>()
        .set_object_field(id, "grenade", grenade.clone());
    if hostile(&team, &danger.team) {
        crate::script::runtime::raise(world, Value::Object(id), "grenade danger", vec![grenade]);
    }
    let radius = FrameWorld::from_world(world)
        .equipment_facts_for(danger.weapon)
        .map_or(256.0, |facts| facts.explosion_radius.max(0) as f32);
    let safe = radius + SAFE_MARGIN;
    if distance(origin, danger.at) >= safe {
        return;
    }
    if distance(origin, danger.at) <= THROWBACK_DIST
        && !no_return
        && super::has_entry(world, "animscripts/grenade_return_throw::main")
    {
        if danger.due - super::super::players::now_ms(world) < FUSE_SHORT_MS {
            delay(world, danger, FUSE_GRACE_MS);
        }
        super::switch_to(world, id, "grenade_return_throw", Some(Hold::Custom));
        return;
    }
    // Takes cover where it is when its cover node shields it from the grenade.
    // ponytail: cover arc only; the shield fraction and the move to another
    // cover out of the blast are not decoded.
    if super::campaign::covered_from(world, id, danger.at) {
        return;
    }
    if !flee(world, id, origin, danger.at, safe) {
        super::switch_to(world, id, "grenade_cower", Some(Hold::Custom));
    }
}

/// Routes the actor to the nearest point `safe` away from `at`, keeping its
/// goal to hand back; false when no route leads there.
fn flee(world: &mut World, id: u64, origin: [f32; 3], at: [f32; 3], safe: f32) -> bool {
    let away = glam::Vec2::new(origin[0] - at[0], origin[1] - at[1]);
    let away = away.try_normalize().unwrap_or(glam::Vec2::X) * safe;
    let to = [at[0] + away.x, at[1] + away.y, origin[2]];
    let mut runtime = world.resource_mut::<RoundScript>();
    let Some(nav) = runtime.campaign.actor_nav.clone() else {
        return false;
    };
    let blocked: Vec<super::Bounds> = runtime.campaign.blocked_paths.values().copied().collect();
    let Some(route) = nav.0.route(origin, to, &blocked) else {
        return false;
    };
    let saved = runtime.campaign.actor_goals.insert(
        id,
        ActorGoal {
            target: GoalTarget::Pos(to),
            route,
            routed_to: Some(to),
            settled: true,
            stepping: false,
        },
    );
    drop(runtime);
    let mut brain = brain(world, id);
    brain.grenade_flight = Some((to, saved));
    store(world, id, brain);
    true
}

fn delay(world: &mut World, danger: &Danger, ms: i64) {
    match danger.projectile {
        Some(number) => {
            if let Some(p) = FrameWorld::from_world(world).projectile_mut_by_number(number)
                && let Some(due) = p.detonate_at_ms.as_mut()
            {
                *due += i32::try_from(ms).unwrap_or(0);
            }
        }
        None => super::super::weapons::campaign::delay_grenade(world, danger.object, ms),
    }
}

/// `pickupgrenade`: the actor takes its `grenade` out of the world, to throw
/// back with the fuse it had left (`throwgrenade`).
pub(super) fn pick_up(world: &mut World, id: u64) {
    let Value::Object(grenade) = world.resource_mut::<RoundScript>().object_field(id, "grenade")
    else {
        return;
    };
    let now = super::super::players::now_ms(world);
    let Some(danger) = dangers(world).into_iter().find(|d| d.object == grenade) else {
        return;
    };
    let taken = match danger.projectile {
        Some(number) => FrameWorld::from_world(world)
            .despawn_projectile(number)
            .map(|p| (p.weapon, (danger.due - now).max(0))),
        None => super::super::weapons::campaign::take_grenade(world, grenade),
    };
    let target = throwback_target(world, id);
    let mut brain = brain(world, id);
    brain.held_grenade = taken;
    if brain.grenade_target.is_none() {
        brain.grenade_target = target;
    }
    store(world, id, brain);
}

fn throwback_target(world: &mut World, id: u64) -> Option<[f32; 3]> {
    if let Value::Object(enemy) = world.resource_mut::<RoundScript>().object_field(id, "enemy")
        && let Value::Vector(at) = super::super::players::entity_field(world, enemy, "origin")
    {
        return Some(at);
    }
    let mut runtime = world.resource_mut::<RoundScript>();
    let (Value::Vector(origin), Value::Vector(angles)) =
        (runtime.object_field(id, "origin"), runtime.object_field(id, "angles"))
    else {
        return None;
    };
    let yaw = angles[1].to_radians();
    Some([
        origin[0] + yaw.cos() * THROWBACK_RANGE,
        origin[1] + yaw.sin() * THROWBACK_RANGE,
        origin[2],
    ])
}
