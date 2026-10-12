//! SP actors: entities spawned from `actor_*` spawners. Bodies are script
//! entities like drones; their brain is the bot pipeline (`crates/bots`),
//! which routes them over the level's path nodes ([`ActorNav`]).
use bevy_ecs::prelude::World;
use std::sync::Arc;

use super::arrays::new_array;
use super::entities::EntityKind;
use super::natives::engine::entity_id;
use crate::script::{Namespace, NativeRegistry, RoundScript, Value};

mod campaign;
pub(crate) use campaign::ThreatBias;

/// Spawner flag 1: the entity spawns AI instead of being one.
const SPAWNER: i32 = 1;
/// The same for a `script_vehicle` (its flag 1 marks a drivable vehicle).
pub(crate) const VEHICLE_SPAWNER: i32 = 2;

/// Routes over the level's path nodes, built by the bot brain.
pub trait ActorNav: Send + Sync {
    /// Waypoints from `from` to `to`, `to` last; `None` when no route joins them.
    fn route(&self, from: [f32; 3], to: [f32; 3]) -> Option<Vec<[f32; 3]>>;
    /// The traverse (negotiation begin, end) node indices whose begin node is at `at`.
    fn traversal_at(&self, at: [f32; 3]) -> Option<(usize, usize)>;
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

/// A state the engine holds the actor in until it ends, instead of choosing
/// its animscript from its situation.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Hold {
    /// `animcustom`: until the function returns.
    Custom,
    /// `animscripted`: until the clip has played out (seen on an earlier tick,
    /// so a scene that restarts the clip on its `end` keeps the hold).
    Scripted {
        started: bool,
        ended: bool,
    },
    Pain,
    Death,
}

#[derive(Clone, Debug, Default, PartialEq)]
enum Orient {
    /// Face the way the actor moves; hold still otherwise.
    #[default]
    Default,
    Current,
    Motion,
    Angle(f32),
    Point([f32; 3]),
    Enemy,
}

/// Which animscript runs the actor, and how its animation moves it.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ActorBrain {
    /// `stop`, `move`, `cover_left`, ...; empty before the first.
    script: Arc<str>,
    thread: Option<u64>,
    hold: Option<Hold>,
    /// `animmode`: anything but `none`/`normal` moves the actor by its root motion.
    anim_mode: Arc<str>,
    orient: Orient,
    /// Level time the enemy was last seen.
    enemy_seen_ms: i64,
    /// The hostile seen last: who, its origin then, level time.
    last_seen: Option<(u64, [f32; 3], i64)>,
    /// Negotiation (start, end) nodes while the actor crosses a traverse.
    traverse: Option<(u64, u64)>,
}

/// Movement without an animation tree (an aitype with no animscripts).
const RUN_SPEED: f32 = 170.0;
// ponytail: one turn rate for every orient mode; per-anim turn rates if turns look wrong.
const TURN_RATE: f32 = 360.0;
/// `nearnode`: how close counts as at the node.
const NEAR_NODE: f32 = 32.0;
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
/// `PathData` keeps five string keys; the map's `node_*` entities (same origin)
/// carry the rest (`script_linkto`, `script_parameters`, ...).
pub(crate) fn spawn_path_nodes(
    world: &mut World,
    nodes: &[asset_core::PathNode],
    entities: &[Vec<(String, String)>],
) -> Result<(), String> {
    let at = |origin: [f32; 3]| origin.map(|v| v.round() as i32);
    let mut extra = std::collections::HashMap::new();
    for pairs in entities {
        let field = |name: &str| pairs.iter().find(|(k, _)| k.eq_ignore_ascii_case(name));
        let Some((_, origin)) = field("origin") else {
            continue;
        };
        if field("classname").is_some_and(|(_, c)| c.starts_with("node_")) {
            extra.insert(at(super::entities::vector(origin)), pairs);
        }
    }
    let mut runtime = world.resource_mut::<RoundScript>();
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
        for (key, value) in extra
            .get(&at(node.origin))
            .into_iter()
            .flat_map(|p| p.iter())
        {
            let key = key.to_ascii_lowercase();
            if !matches!(
                key.as_str(),
                "classname" | "origin" | "angles" | "angle" | "spawnflags"
            ) && runtime.object_field(id, &key) == Value::Undefined
            {
                runtime.set_object_field(id, &key, Value::string(value));
            }
        }
        runtime.campaign.nodes.push(id);
    }
    Ok(())
}

/// Nodes whose string field `key` is `value`.
fn matching_nodes(world: &mut World, args: &[Value]) -> Result<Vec<Value>, String> {
    let value = Value::string(&super::args::string(args, 0)?);
    let key = super::args::string(args, 1)?.to_ascii_lowercase();
    let mut runtime = world.resource_mut::<RoundScript>();
    let nodes = runtime.campaign.nodes.clone();
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
    node: Option<u64>,
) -> Result<Value, String> {
    let id = actor(world, receiver)?;
    let mut runtime = world.resource_mut::<RoundScript>();
    runtime.set_object_field(id, "goalpos", Value::Vector(at));
    runtime.set_object_field(id, "node", node.map_or(Value::Undefined, Value::Object));
    runtime.campaign.actor_goals.insert(
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
    match world
        .resource_mut::<RoundScript>()
        .object_field(*id, "origin")
    {
        Value::Vector(origin) => Ok((*id, origin)),
        _ => Err("goal has no origin".into()),
    }
}

fn health(runtime: &mut RoundScript, id: u64) -> i32 {
    match runtime.object_field(id, "health") {
        Value::Int(health) => health,
        Value::Float(health) => health as i32,
        _ => 0,
    }
}

fn live_actors(runtime: &RoundScript) -> Vec<u64> {
    runtime
        .entities
        .iter()
        .filter(|(id, e)| e.kind == EntityKind::Actor && runtime.script_is_defined(id))
        .map(|(id, _)| *id)
        .collect()
}

fn yaw_to(from: [f32; 3], to: [f32; 3]) -> f32 {
    (to[1] - from[1]).atan2(to[0] - from[0]).to_degrees()
}

/// Once per tick, before threads run: picks each actor's animscript, then moves it.
pub(crate) fn advance(world: &mut World) {
    suppress(world);
    look(world);
    run_scripts(world);
    move_actors(world);
}

/// Each actor looks around every `SIGHT_PERIOD` ticks (staggered by id).
const SIGHT_PERIOD: u64 = 4;
/// An enemy out of sight stays the enemy this long.
const ENEMY_MEMORY_MS: i64 = 5_000;
const DEFAULT_SIGHT_DIST: f32 = 8192.0;
// ponytail: one forward half-angle (90 degrees) unless `fovcosine` is set.
const DEFAULT_FOV_COS: f32 = 0.0;
/// Nearest hostiles traced per look.
const SIGHT_TRACES: usize = 3;

fn hostile(a: &Value, b: &Value) -> bool {
    let (Value::String(a), Value::String(b)) = (a, b) else {
        return false;
    };
    matches!((&**a, &**b), ("allies", "axis") | ("axis", "allies"))
}

fn truthy(value: &Value) -> bool {
    matches!(value, Value::Int(n) if *n != 0) || matches!(value, Value::Float(n) if *n != 0.0)
}

/// Where an entity is looked at and shot at: actors and players at eye height.
fn eye(world: &mut World, id: u64) -> Option<[f32; 3]> {
    match super::players::entity_field(world, id, "origin") {
        Value::Vector([x, y, z]) => Some([x, y, z + EYE_HEIGHT]),
        _ => None,
    }
}

fn clear_sight(world: &mut World, from: [f32; 3], to: [f32; 3]) -> bool {
    let trace = super::presence::settled(world).trace_world(
        from,
        to,
        [0.0; 3],
        [0.0; 3],
        crate::bullet_collision::MASK_SHOT,
    );
    trace.fraction >= 1.0 && trace.startsolid == 0
}

/// Hostile players and actors that can be fought: `(object, team)`.
fn combatants(world: &mut World) -> Vec<(u64, Value)> {
    let mut runtime = world.resource_mut::<RoundScript>();
    let mut out: Vec<u64> = runtime.players.values().map(|slot| slot.object).collect();
    out.extend(
        live_actors(&runtime)
            .into_iter()
            .filter(|id| health(&mut runtime, *id) > 0),
    );
    let mut found = Vec::new();
    for id in out {
        if !truthy(&runtime.object_field(id, "ignoreme")) {
            found.push((id, runtime.object_field(id, "team")));
        }
    }
    found
}

/// Picks each actor's `enemy`: the nearest hostile it can see, kept while seen
/// or remembered; notifies `enemy` when it changes.
fn look(world: &mut World) {
    let tick = u64::from(world.resource::<crate::step::StepRequest>().tick.0);
    let now = super::players::now_ms(world);
    let ids: Vec<u64> = {
        let mut runtime = world.resource_mut::<RoundScript>();
        live_actors(&runtime)
            .into_iter()
            .filter(|id| (id + tick) % SIGHT_PERIOD == 0 && health(&mut runtime, *id) > 0)
            .collect()
    };
    if ids.is_empty() {
        return;
    }
    let all = combatants(world);
    for id in ids {
        let (team, current, ignore_all, sight_sq, fov, angles, group, threat, favorite) = {
            let mut runtime = world.resource_mut::<RoundScript>();
            let sight_sq = number(runtime.object_field(id, "maxsightdistsqrd"))
                .unwrap_or(DEFAULT_SIGHT_DIST * DEFAULT_SIGHT_DIST);
            let fov = number(runtime.object_field(id, "fovcosine")).unwrap_or(DEFAULT_FOV_COS);
            let angles = match runtime.object_field(id, "angles") {
                Value::Vector(angles) => angles,
                _ => [0.0; 3],
            };
            let current = match runtime.object_field(id, "enemy") {
                Value::Object(enemy) => Some(enemy),
                _ => None,
            };
            let favorite = match runtime.object_field(id, "favoriteenemy") {
                Value::Object(enemy) => Some(enemy),
                _ => None,
            };
            (
                runtime.object_field(id, "team"),
                current,
                truthy(&runtime.object_field(id, "ignoreall")),
                sight_sq,
                fov,
                angles,
                campaign::group_of(&mut runtime, id),
                runtime.campaign.threat.clone(),
                favorite,
            )
        };
        let Some(from) = eye(world, id) else {
            continue;
        };
        let forward = [angles[1].to_radians().cos(), angles[1].to_radians().sin()];
        let mut candidates: Vec<(f32, u64, [f32; 3])> = Vec::new();
        if !ignore_all {
            for (other, other_team) in &all {
                if *other == id || !hostile(&team, other_team) {
                    continue;
                }
                let target = campaign::group_of(&mut world.resource_mut::<RoundScript>(), *other);
                if threat.ignores(&group, &target) {
                    continue;
                }
                let Some(at) = eye(world, *other) else {
                    continue;
                };
                let d = [at[0] - from[0], at[1] - from[1], at[2] - from[2]];
                let dist_sq = d[0] * d[0] + d[1] * d[1] + d[2] * d[2];
                let planar = d[0].hypot(d[1]).max(f32::EPSILON);
                let in_view = (d[0] * forward[0] + d[1] * forward[1]) / planar >= fov;
                if dist_sq <= sight_sq && (in_view || current == Some(*other)) {
                    let score = dist_sq.sqrt() - threat.bias(&group, &target);
                    candidates.push((score, *other, at));
                }
            }
        }
        candidates.sort_by(|a, b| {
            (Some(a.1) != favorite)
                .cmp(&(Some(b.1) != favorite))
                .then(a.0.total_cmp(&b.0))
        });
        let seen = candidates
            .iter()
            .take(SIGHT_TRACES)
            .find(|(_, _, at)| clear_sight(world, from, *at))
            .map(|(_, other, at)| (*other, *at));
        let mut brain = brain(world, id);
        let still_live = current.is_some_and(|enemy| all.iter().any(|(other, _)| *other == enemy));
        let enemy = match seen {
            Some((other, at)) => {
                brain.enemy_seen_ms = now;
                brain.last_seen = Some((other, [at[0], at[1], at[2] - EYE_HEIGHT], now));
                world.resource_mut::<RoundScript>().set_object_field(
                    id,
                    "lastenemysightpos",
                    Value::Vector(at),
                );
                // A remembered enemy is not dropped for a nearer one seen once.
                match current {
                    Some(enemy)
                        if enemy != other
                            && still_live
                            && candidates.iter().any(|c| c.1 == enemy) =>
                    {
                        Some(enemy)
                    }
                    _ => Some(other),
                }
            }
            None if !ignore_all && still_live && now - brain.enemy_seen_ms < ENEMY_MEMORY_MS => {
                current
            }
            None => None,
        };
        store(world, id, brain);
        if enemy != current {
            world.resource_mut::<RoundScript>().set_object_field(
                id,
                "enemy",
                enemy.map_or(Value::Undefined, Value::Object),
            );
            crate::script::runtime::raise(world, Value::Object(id), "enemy", Vec::new());
        }
    }
}

/// A hostile bullet passing this close to an actor's eye suppresses it.
const SUPPRESS_RADIUS: f32 = 64.0;
/// `suppressionmeter` gained per near miss.
const SUPPRESS_STEP: f32 = 0.25;
/// `suppressionmeter` lost per second.
const SUPPRESS_DECAY: f32 = 0.33;

fn segment_distance(point: [f32; 3], from: [f32; 3], to: [f32; 3]) -> f32 {
    let d = [to[0] - from[0], to[1] - from[1], to[2] - from[2]];
    let p = [point[0] - from[0], point[1] - from[1], point[2] - from[2]];
    let len_sq = d[0] * d[0] + d[1] * d[1] + d[2] * d[2];
    let t = if len_sq > 0.0 {
        ((p[0] * d[0] + p[1] * d[1] + p[2] * d[2]) / len_sq).clamp(0.0, 1.0)
    } else {
        0.0
    };
    distance(
        point,
        [from[0] + d[0] * t, from[1] + d[1] * t, from[2] + d[2] * t],
    )
}

fn shooter_team(world: &mut World, number: i32) -> Value {
    let object =
        match u32::try_from(number).map(|client| super::players::player_object(world, client)) {
            Ok(Value::Object(id)) => Some(id),
            _ => world
                .resource::<RoundScript>()
                .entities
                .iter()
                .find(|(_, entity)| entity.number == number)
                .map(|(id, _)| *id),
        };
    object.map_or(Value::Undefined, |id| {
        world.resource_mut::<RoundScript>().object_field(id, "team")
    })
}

/// Last tick's hostile bullets that passed near an actor fill its
/// `suppressionmeter`, which drains over time.
// ponytail: only shots that hit something raise a hit event, so a bullet into
// the sky does not suppress; record shot segments if AI ignores skyward fire.
fn suppress(world: &mut World) {
    let tick = world.resource::<crate::step::StepRequest>().tick.0;
    let shots: Vec<(i32, [f32; 3], [f32; 3])> = crate::frame::FrameWorld::from_world(world)
        .entity_events()
        .iter()
        .filter(|record| {
            record.tick.0 + 1 == tick
                && matches!(
                    entity_iw4::entity_event_action(record.event),
                    Ok(entity_iw4::EntityEventAction::BulletHit)
                )
        })
        .map(|record| {
            (
                record.payload.number,
                record.payload.origin2,
                record.payload.origin,
            )
        })
        .collect();
    let shots: Vec<(Value, [f32; 3], [f32; 3])> = shots
        .into_iter()
        .map(|(number, from, to)| (shooter_team(world, number), from, to))
        .collect();
    let decay = SUPPRESS_DECAY * crate::MATCH_TICK_MS as f32 / 1000.0;
    let mut runtime = world.resource_mut::<RoundScript>();
    for id in live_actors(&runtime) {
        let meter = number(runtime.object_field(id, "suppressionmeter")).unwrap_or(0.0);
        let mut next = meter - decay;
        if !truthy(&runtime.object_field(id, "ignoresuppression"))
            && let Value::Vector([x, y, z]) = runtime.object_field(id, "origin")
        {
            let team = runtime.object_field(id, "team");
            let near = shots
                .iter()
                .filter(|(shooter, from, to)| {
                    hostile(&team, shooter)
                        && segment_distance([x, y, z + EYE_HEIGHT], *from, *to) < SUPPRESS_RADIUS
                })
                .count();
            next += SUPPRESS_STEP * near as f32;
        }
        let next = next.clamp(0.0, 1.0);
        if next != meter {
            runtime.set_object_field(id, "suppressionmeter", Value::Float(next));
        }
    }
}

/// At the claimed node (`node`) and, with `facing`, turned its way.
// ponytail: "turned its way" is within 45 degrees of the node's yaw; tune if
// cover reactions pick the wrong set.
fn near_claim_node(world: &mut World, receiver: &Value, facing: bool) -> Result<Value, String> {
    let id = actor(world, receiver)?;
    let mut runtime = world.resource_mut::<RoundScript>();
    let Value::Object(node) = runtime.object_field(id, "node") else {
        return Ok(Value::Int(0));
    };
    let (
        Value::Vector(origin),
        Value::Vector(at),
        Value::Vector(angles),
        Value::Vector(node_angles),
    ) = (
        runtime.object_field(id, "origin"),
        runtime.object_field(node, "origin"),
        runtime.object_field(id, "angles"),
        runtime.object_field(node, "angles"),
    )
    else {
        return Ok(Value::Int(0));
    };
    let aligned = !facing || math_iw4::angle_subtract(angles[1], node_angles[1]).abs() < 45.0;
    Ok(Value::Int((near(origin, at) && aligned).into()))
}

/// `shoot([accuracy scale], [point])`: one shot of the actor's weapon at the
/// point or its enemy, missing by chance per `accuracy`.
fn shoot(world: &mut World, receiver: &Value, args: &[Value]) -> Result<Value, String> {
    let id = actor(world, receiver)?;
    let (name, accuracy, enemy) = {
        let mut runtime = world.resource_mut::<RoundScript>();
        let name = match runtime.object_field(id, "weapon") {
            Value::String(name) => name.to_string(),
            _ => return Ok(Value::Undefined),
        };
        let accuracy = number(runtime.object_field(id, "accuracy")).unwrap_or(1.0);
        let enemy = match runtime.object_field(id, "enemy") {
            Value::Object(enemy) => Some(enemy),
            _ => None,
        };
        (name, accuracy, enemy)
    };
    if name.is_empty() || name == "none" {
        return Ok(Value::Undefined);
    }
    let weapon =
        crate::script_player::weapon_named(&crate::frame::FrameWorld::from_world(world), &name)?;
    let scale = match args.first() {
        Some(value) => number(value.clone()).unwrap_or(1.0),
        None => 1.0,
    };
    let from = super::turrets::muzzle(world, id);
    let target = match args.get(1) {
        Some(Value::Vector(at)) => Some(*at),
        _ => enemy.and_then(|enemy| eye(world, enemy)),
    };
    let forward = match world
        .resource_mut::<RoundScript>()
        .object_field(id, "angles")
    {
        Value::Vector(angles) => math_iw4::angle_vectors(angles).0,
        _ => [1.0, 0.0, 0.0],
    };
    let point = match target {
        Some(at) => {
            let roll = super::natives::math::random_unit(world);
            if roll < (accuracy * scale).clamp(0.0, 1.0) {
                at
            } else {
                // ponytail: a miss passes 16-48 units off the target in a random
                // direction; per-weapon AI spread graphs would replace this.
                let angle = super::natives::math::random_unit(world) * std::f32::consts::TAU;
                let off = 16.0 + super::natives::math::random_unit(world) * 32.0;
                [
                    at[0] + angle.cos() * off,
                    at[1] + angle.sin() * off,
                    at[2] + (roll - 0.5) * off,
                ]
            }
        }
        None => std::array::from_fn(|k| from[k] + forward[k] * 1000.0),
    };
    let dir = glam::Vec3::from_array(std::array::from_fn(|k| point[k] - from[k]))
        .try_normalize()
        .map_or(forward, |d| d.to_array());
    super::turrets::fire_bullet(world, id, from, dir, weapon, None);
    Ok(Value::Undefined)
}

/// The animscript for the actor's situation: `move` on a route, its cover
/// script at a claimed cover node, `combat` with an enemy, else `stop`.
fn desired_script(world: &mut World, id: u64) -> String {
    let mut runtime = world.resource_mut::<RoundScript>();
    if runtime
        .campaign
        .actor_goals
        .get(&id)
        .is_some_and(|goal| !goal.route.is_empty())
    {
        return "move".into();
    }
    let cover = match (
        runtime.object_field(id, "node"),
        runtime.object_field(id, "origin"),
    ) {
        (Value::Object(node), Value::Vector(origin)) => match (
            runtime.object_field(node, "origin"),
            runtime.object_field(node, "type"),
        ) {
            (Value::Vector(at), Value::String(kind)) if near(origin, at) => cover_script(&kind),
            _ => None,
        },
        _ => None,
    };
    let enemy =
        matches!(runtime.object_field(id, "enemy"), Value::Object(enemy) if runtime.live(&enemy));
    drop(runtime);
    if let Some(script) = cover
        && has_entry(world, &format!("animscripts/{script}::main"))
    {
        return script.into();
    }
    if enemy {
        return "combat".into();
    }
    "stop".into()
}

fn cover_script(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "Cover Stand" => "cover_stand",
        "Cover Crouch" | "Cover Crouch Window" => "cover_crouch",
        "Cover Prone" => "cover_prone",
        "Cover Left" | "Cover Wide Left" => "cover_left",
        "Cover Right" | "Cover Wide Right" => "cover_right",
        _ => return None,
    })
}

fn near(a: [f32; 3], b: [f32; 3]) -> bool {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt() <= NEAR_NODE
        && (a[2] - b[2]).abs() <= DEFAULT_GOAL_HEIGHT
}

enum Start<'a> {
    Entry(&'a str),
    Function(u32),
}

/// Ends the running animscript (`killanimscript`, its `end_script`) and starts `script`.
fn switch(world: &mut World, id: u64, brain: &mut ActorBrain, script: &str, start: Start) {
    let old = std::mem::replace(&mut brain.script, script.into());
    if let Some(thread) = brain.thread.take() {
        // A queued thread has not reached its `endon` yet.
        crate::script::runtime::campaign::stop_thread(world, thread);
    }
    let receiver = Value::Object(id);
    if &*old == "scripted"
        && let Some(anim) = world
            .resource_mut::<RoundScript>()
            .entities
            .get_mut(&id)
            .and_then(|entity| entity.anim.as_mut())
    {
        // The actor stays where the clip left it; its own motor takes over.
        anim.scripted = None;
    }
    if !old.is_empty() {
        crate::script::runtime::raise(world, receiver.clone(), "killanimscript", Vec::new());
        let end = format!("animscripts/{old}::end_script");
        if has_entry(world, &end) {
            let now = super::players::now_ms(world);
            let _ = crate::script::runtime::run_now(world, &end, receiver.clone(), Vec::new(), now);
        }
    }
    {
        let mut runtime = world.resource_mut::<RoundScript>();
        let prev = if old.is_empty() { "none" } else { &old };
        runtime.set_object_field(id, "prevscript", Value::string(prev));
        runtime.set_object_field(id, "script", Value::string(script));
    }
    brain.thread = match start {
        Start::Entry(entry) if has_entry(world, entry) => {
            crate::script::runtime::campaign::start_thread(world, entry, receiver, Vec::new()).ok()
        }
        Start::Entry(_) => None,
        Start::Function(function) => {
            crate::script::runtime::campaign::start_function(world, function, receiver, Vec::new())
                .ok()
        }
    };
}

fn brain(world: &World, id: u64) -> ActorBrain {
    world
        .resource::<RoundScript>()
        .campaign
        .actor_brains
        .get(&id)
        .cloned()
        .unwrap_or_default()
}

fn store(world: &mut World, id: u64, brain: ActorBrain) {
    world
        .resource_mut::<RoundScript>()
        .campaign
        .actor_brains
        .insert(id, brain);
}

fn switch_to(world: &mut World, id: u64, script: &str, hold: Option<Hold>) {
    let mut brain = brain(world, id);
    let entry = format!("animscripts/{script}::main");
    switch(world, id, &mut brain, script, Start::Entry(&entry));
    brain.hold = hold;
    store(world, id, brain);
}

fn run_scripts(world: &mut World) {
    let library = world
        .resource::<RoundScript>()
        .campaign
        .anim_library
        .clone();
    let ids = live_actors(world.resource::<RoundScript>());
    world
        .resource_mut::<RoundScript>()
        .campaign
        .actor_brains
        .retain(|id, _| ids.contains(id));
    for id in ids {
        let mut brain = brain(world, id);
        if health(&mut world.resource_mut::<RoundScript>(), id) <= 0 {
            if brain.hold != Some(Hold::Death) {
                switch_to(world, id, "death", Some(Hold::Death));
            }
            continue;
        }
        let thread_done = brain
            .thread
            .is_none_or(|thread| !crate::script::runtime::campaign::running(world, thread));
        campaign::finish_traverse(world, id, &mut brain, thread_done);
        brain.hold = match brain.hold {
            Some(Hold::Scripted { started, ended }) => {
                let state = world
                    .resource_mut::<RoundScript>()
                    .entities
                    .get_mut(&id)
                    .and_then(|entity| entity.anim.as_mut())
                    .and_then(|anim| anim.scripted_ended(&library));
                match state {
                    Some(true) if ended => None,
                    Some(now_ended) => Some(Hold::Scripted {
                        started: true,
                        ended: now_ended,
                    }),
                    None if started || thread_done => None,
                    None => Some(Hold::Scripted { started, ended }),
                }
            }
            Some(Hold::Custom | Hold::Pain) if thread_done => None,
            Some(Hold::Death) => None,
            hold => hold,
        };
        if brain.hold.is_none() {
            let desired = campaign::traverse_script(world, &mut brain)
                .unwrap_or_else(|| desired_script(world, id));
            if *brain.script != *desired {
                let entry = format!("animscripts/{desired}::main");
                switch(world, id, &mut brain, &desired, Start::Entry(&entry));
            }
        }
        store(world, id, brain);
    }
}

fn approach_yaw(from: f32, to: f32, max: f32) -> f32 {
    let delta = math_iw4::angle_subtract(to, from).clamp(-max, max);
    math_iw4::angle_normalize_360(from + delta)
}

/// Moves each actor: along its goal route at its animation's speed while its
/// animscript is `move` (`animmode` none), else by its root motion; turns it
/// per `orientmode`. Notifies `goal` on arrival (inside `goalradius`/`goalheight`)
/// and `bad_path` when no route exists.
fn move_actors(world: &mut World) {
    let dt = crate::MATCH_TICK_MS as f32 / 1000.0;
    let mut notes = Vec::new();
    let mut runtime = world.resource_mut::<RoundScript>();
    let nav = runtime.campaign.actor_nav.clone();
    let ids = live_actors(&runtime);
    let gone: Vec<u64> = runtime
        .campaign
        .actor_goals
        .keys()
        .copied()
        .filter(|id| !runtime.live(id))
        .collect();
    for id in gone {
        runtime.campaign.actor_goals.remove(&id);
    }
    for id in ids {
        let Value::Vector(mut origin) = runtime.object_field(id, "origin") else {
            continue;
        };
        let mut angles = match runtime.object_field(id, "angles") {
            Value::Vector(angles) => angles,
            _ => [0.0; 3],
        };
        let brain = runtime
            .campaign
            .actor_brains
            .get(&id)
            .cloned()
            .unwrap_or_default();
        let (delta, turn, has_tree, scripted) = match &runtime.entities[&id].anim {
            Some(anim) => (
                anim.root_delta.0,
                anim.root_delta.1,
                true,
                anim.scripted.is_some(),
            ),
            None => ([0.0; 3], 0.0, false, false),
        };
        if scripted {
            continue;
        }
        let code_moves = matches!(&*brain.anim_mode, "" | "none" | "normal");
        let walking = code_moves && (brain.script.is_empty() || &*brain.script == "move");
        let root_motion = !code_moves || brain.traverse.is_some();
        let start = origin;
        let mut heading = None;
        let mut path_goal = None;
        let mut traverse = None;
        if let Some(target) = runtime
            .campaign
            .actor_goals
            .get(&id)
            .map(|goal| goal.target.clone())
        {
            let goal_pos = match target {
                GoalTarget::Pos(at) => Some(at),
                GoalTarget::Entity(e) => match runtime.object_field(e, "origin") {
                    Value::Vector(at) if runtime.live(&e) => Some(at),
                    _ => None,
                },
            };
            let Some(goal_pos) = goal_pos else {
                runtime.campaign.actor_goals.remove(&id);
                continue;
            };
            let radius =
                number(runtime.object_field(id, "goalradius")).unwrap_or(DEFAULT_GOAL_RADIUS);
            let height =
                number(runtime.object_field(id, "goalheight")).unwrap_or(DEFAULT_GOAL_HEIGHT);
            let goal = runtime.campaign.actor_goals.get_mut(&id).unwrap();
            let planar =
                ((origin[0] - goal_pos[0]).powi(2) + (origin[1] - goal_pos[1]).powi(2)).sqrt();
            if planar <= radius.max(4.0) && (origin[2] - goal_pos[2]).abs() <= height {
                goal.route.clear();
                if !goal.settled {
                    goal.settled = true;
                    goal.routed_to = Some(goal_pos);
                    notes.push((id, "goal"));
                }
            } else {
                if goal
                    .routed_to
                    .is_none_or(|at| distance(at, goal_pos) > REROUTE)
                    && let Some(nav) = &nav
                {
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
                if !goal.route.is_empty() {
                    path_goal = Some(goal_pos);
                }
                if let Some(&next) = goal.route.first() {
                    heading = Some([next[0] - origin[0], next[1] - origin[1]]);
                }
                if walking {
                    let mut left = if has_tree {
                        delta[0].hypot(delta[1])
                    } else {
                        RUN_SPEED * dt
                    };
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
                            traverse = nav.as_ref().and_then(|nav| nav.0.traversal_at(origin));
                            if traverse.is_some() {
                                break;
                            }
                        } else {
                            let t = left / d;
                            origin = std::array::from_fn(|k| origin[k] + (next[k] - origin[k]) * t);
                            left = 0.0;
                        }
                    }
                }
            }
        }
        if let Some(link) = traverse {
            campaign::begin_traverse(&mut runtime, id, link);
        }
        if root_motion {
            let world_delta = super::script_anim::rotate([0.0, angles[1], 0.0], delta);
            origin = std::array::from_fn(|k| origin[k] + world_delta[k]);
        }
        let face = |at: [f32; 3]| yaw_to(origin, at);
        let motion = heading
            .filter(|_| walking)
            .map(|[x, y]| y.atan2(x).to_degrees());
        let enemy = match runtime.object_field(id, "enemy") {
            Value::Object(enemy) => match runtime.object_field(enemy, "origin") {
                Value::Vector(at) => Some(face(at)),
                _ => None,
            },
            _ => None,
        };
        let want = match brain.orient {
            Orient::Angle(yaw) => Some(yaw),
            Orient::Point(at) => Some(face(at)),
            Orient::Current => None,
            Orient::Motion | Orient::Default => motion,
            Orient::Enemy => enemy.or(motion),
        };
        let yaw = match want {
            Some(want) => approach_yaw(angles[1], want, TURN_RATE * dt),
            None if root_motion => math_iw4::angle_normalize_360(angles[1] + turn),
            None => angles[1],
        };
        let moved = origin != start || yaw != angles[1];
        angles[1] = yaw;
        if let Some([x, y]) = heading {
            let len = x.hypot(y).max(f32::EPSILON);
            runtime.set_object_field(id, "lookaheaddir", Value::Vector([x / len, y / len, 0.0]));
        }
        runtime.set_object_field(
            id,
            "pathgoalpos",
            path_goal.map_or(Value::Undefined, Value::Vector),
        );
        runtime.set_object_field(
            id,
            "velocity",
            Value::Vector(std::array::from_fn(|k| (origin[k] - start[k]) / dt)),
        );
        if moved {
            runtime.set_object_field(id, "origin", Value::Vector(origin));
            runtime.set_object_field(id, "angles", Value::Vector(angles));
        }
    }
    for (id, note) in notes {
        crate::script::runtime::raise(world, Value::Object(id), note, Vec::new());
    }
}

/// Engine fields a fresh actor carries before its scripts run.
fn init_fields(world: &mut World, id: u64) {
    let mut runtime = world.resource_mut::<RoundScript>();
    let yaw = match runtime.object_field(id, "angles") {
        Value::Vector(angles) => angles[1],
        _ => 0.0,
    };
    let forward = [yaw.to_radians().cos(), yaw.to_radians().sin(), 0.0];
    let health = health(&mut runtime, id);
    let fields = [
        ("maxhealth", Value::Int(health)),
        ("script", Value::string("init")),
        ("prevscript", Value::string("none")),
        ("movemode", Value::string("run")),
        ("combatmode", Value::string("cover")),
        ("lookaheaddir", Value::Vector(forward)),
        ("lookaheaddist", Value::Float(0.0)),
        ("lookforward", Value::Vector(forward)),
        ("desiredangle", Value::Float(yaw)),
        ("facemotion", Value::Int(1)),
        ("stairsstate", Value::string("none")),
        ("groundtype", Value::string("dirt")),
        ("damageyaw", Value::Float(0.0)),
        ("damagelocation", Value::string("none")),
        ("damagedir", Value::Vector([0.0; 3])),
        ("damagemod", Value::string("MOD_UNKNOWN")),
        ("damageweapon", Value::string("none")),
        ("damagetaken", Value::Int(0)),
        ("relativedir", Value::Int(0)),
        ("suppressionmeter", Value::Float(0.0)),
        ("alertlevel", Value::string("noncombat")),
        ("alertlevelint", Value::Int(0)),
        ("velocity", Value::Vector([0.0; 3])),
        ("lookaheadhitsstairs", Value::Int(0)),
        ("teammovewaittime", Value::Int(0)),
        // ponytail: never set; trace muzzle-to-wall if AI aim through walls shows.
        ("gunblockedbywall", Value::Int(0)),
        ("doingambush", Value::Int(0)),
    ];
    for (name, value) in fields {
        if runtime.object_field(id, name) == Value::Undefined {
            runtime.set_object_field(id, name, value);
        }
    }
    default_aim_limits(&mut runtime, id);
}

// ponytail: one aim cone for every actor (45 degrees each way); per-aitype
// limits if an aim looks clipped.
fn default_aim_limits(runtime: &mut RoundScript, id: u64) {
    for (name, limit) in [
        ("rightaimlimit", 45.0),
        ("leftaimlimit", -45.0),
        ("upaimlimit", 45.0),
        ("downaimlimit", -45.0),
    ] {
        runtime.set_object_field(id, name, Value::Float(limit));
    }
}

const EYE_HEIGHT: f32 = 64.0;
const ACTOR_RADIUS: f32 = 15.0;
const ACTOR_HEIGHT: f32 = 72.0;
const STEP_HEIGHT: f32 = 18.0;

/// The highest stance a node's type lets an actor take at it.
fn highest_stance(world: &mut World, node: &Value) -> Result<&'static str, String> {
    let Value::Object(node) = node else {
        return Err("receiver is not a node".into());
    };
    let kind = world
        .resource_mut::<RoundScript>()
        .object_field(*node, "type");
    let Value::String(kind) = kind else {
        return Err("receiver is not a node".into());
    };
    Ok(match &*kind {
        "Cover Prone" | "Conceal Prone" => "prone",
        "Cover Crouch" | "Cover Crouch Window" | "Conceal Crouch" => "crouch",
        _ => "stand",
    })
}

/// IW4 hit location of a humanoid bone, by the bone's name.
fn hit_location(tag: &str) -> &'static str {
    let tag = tag.to_ascii_lowercase();
    let side = |right: &'static str, left: &'static str| {
        if tag.ends_with("_ri") { right } else { left }
    };
    match tag.as_str() {
        "j_helmet" => "helmet",
        "j_head" | "j_head_end" | "tag_eye" => "head",
        "j_neck" => "neck",
        t if t.starts_with("j_spine4")
            || t.starts_with("j_spineupper")
            || t.starts_with("j_clavicle") =>
        {
            "torso_upper"
        }
        t if t.starts_with("j_spine") || t == "j_mainroot" || t == "pelvis" => "torso_lower",
        t if t.starts_with("j_shoulder") || t.starts_with("j_bicep") => {
            side("right_arm_upper", "left_arm_upper")
        }
        t if t.starts_with("j_elbow") || t.starts_with("j_forearm") => {
            side("right_arm_lower", "left_arm_lower")
        }
        t if t.starts_with("j_wrist")
            || t.starts_with("j_thumb")
            || t.starts_with("j_index")
            || t.starts_with("j_mid")
            || t.starts_with("j_ring")
            || t.starts_with("j_pinky") =>
        {
            side("right_hand", "left_hand")
        }
        t if t.starts_with("j_hip") => side("right_leg_upper", "left_leg_upper"),
        t if t.starts_with("j_knee") => side("right_leg_lower", "left_leg_lower"),
        t if t.starts_with("j_ankle") || t.starts_with("j_ball") => side("right_foot", "left_foot"),
        "" => "none",
        _ => "torso_upper",
    }
}

/// Records the hit on the actor's damage fields and, when it survives, puts
/// it in `pain` unless a scene or another hold owns it.
pub(crate) fn damaged(
    world: &mut World,
    id: u64,
    hit: &super::entity_damage::EntityHit,
    attacker: &Value,
    weapon: &str,
    tag: &str,
) {
    let alive = {
        let mut runtime = world.resource_mut::<RoundScript>();
        let yaw = match runtime.object_field(id, "angles") {
            Value::Vector(angles) => angles[1],
            _ => 0.0,
        };
        let from = hit.dir[1].atan2(hit.dir[0]).to_degrees() + 180.0;
        let fields = [
            ("damagetaken", Value::Int(hit.amount)),
            ("damagedir", Value::Vector(hit.dir)),
            ("damagelocation", Value::string(hit_location(tag))),
            (
                "damageyaw",
                Value::Float(math_iw4::angle_subtract(from, yaw)),
            ),
            ("damagemod", Value::string(hit.means)),
            ("damageweapon", Value::string(weapon)),
        ];
        for (name, value) in fields {
            runtime.set_object_field(id, name, value);
        }
        if *attacker != Value::Undefined {
            runtime.set_object_field(id, "lastattacker", attacker.clone());
        }
        health(&mut runtime, id) > 0
    };
    let pain_allowed = !matches!(
        world
            .resource_mut::<RoundScript>()
            .object_field(id, "allowpain"),
        Value::Int(0)
    );
    if alive && pain_allowed && brain(world, id).hold.is_none() {
        switch_to(world, id, "pain", Some(Hold::Pain));
    }
}

fn has_entry(world: &World, name: &str) -> bool {
    world
        .resource::<RoundScript>()
        .program
        .as_ref()
        .is_some_and(|program| program.names.contains_key(name))
}

fn spawners(world: &mut World) -> Vec<u64> {
    let mut runtime = world.resource_mut::<RoundScript>();
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

/// Starts held campaign entries; precache stays open until their first wait.
pub(crate) fn start_deferred(world: &mut World) {
    let entries = std::mem::take(
        &mut world
            .resource_mut::<RoundScript>()
            .campaign
            .deferred_entries,
    );
    if entries.is_empty() {
        return;
    }
    world.resource_mut::<RoundScript>().loading = true;
    super::vehicles::campaign::init_map_vehicles(world);
    super::weapons::campaign::place_map_weapons(world);
    super::turrets::campaign::init_map_turrets(world);
    init_spawners(world);
    for entry in entries {
        if let Err(fault) = crate::script::runtime::start(world, &entry, Value::level(), Vec::new())
        {
            world.resource_mut::<RoundScript>().fault = Some(fault);
            return;
        }
    }
}

/// A joining campaign player is on allies (squads, friendly fire key on it) and
/// skips the MP connect callback; whether this is a campaign join.
pub(crate) fn join_campaign(world: &mut World, player: u64) -> bool {
    let mut runtime = world.resource_mut::<RoundScript>();
    if runtime.campaign.deferred_entries.is_empty() {
        return false;
    }
    runtime.set_object_field(player, "team", Value::string("allies"));
    true
}

/// Before level main: each aitype the spawners use runs its `precache` once
/// (it may register spawn behaviour, `_rambo`), then each spawner its `spawner`.
pub(crate) fn init_spawners(world: &mut World) {
    let now = super::players::now_ms(world);
    let mut types: Vec<String> = spawners(world)
        .into_iter()
        .map(|id| aitype(&world.resource::<RoundScript>().entities[&id].classname))
        .collect();
    types.sort();
    types.dedup();
    for aitype in types {
        let entry = format!("{aitype}::precache");
        if has_entry(world, &entry) {
            let _ = crate::script::runtime::run_now(world, &entry, Value::level(), Vec::new(), now);
        }
    }
    for spawner in spawners(world) {
        let mut runtime = world.resource_mut::<RoundScript>();
        // A spawner without a `count` key spawns once.
        if runtime.object_field(spawner, "count") == Value::Undefined {
            runtime.set_object_field(spawner, "count", Value::Int(1));
        }
        let classname = runtime.entities[&spawner].classname.clone();
        drop(runtime);
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
        let mut runtime = world.resource_mut::<RoundScript>();
        let origin = runtime.object_field(spawner, "origin");
        let angles = runtime.object_field(spawner, "angles");
        let classname = runtime.entities[&spawner].classname.clone();
        let fields = runtime.objects.get(&spawner).cloned().unwrap_or_default();
        (origin, angles, classname, fields)
    };
    let Value::Vector(origin) = origin else {
        return Err("spawner has no origin".into());
    };
    let id = spawn_script_model(world, origin)?;
    let mut runtime = world.resource_mut::<RoundScript>();
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
    if actor {
        init_fields(world, id);
    }
    Ok(id)
}

/// `dospawn`/`stalingradspawn`: spends one of the spawner's `count`, then runs
/// `animscripts/init::main` on the actor and notifies the spawner `spawned`.
fn spawn_actor(world: &mut World, receiver: &Value) -> Result<Value, String> {
    let spawner = entity_id(world, receiver)?;
    let count = match world
        .resource_mut::<RoundScript>()
        .object_field(spawner, "count")
    {
        Value::Int(n) => n,
        _ => 1,
    };
    if count <= 0 {
        return Ok(Value::Undefined);
    }
    world
        .resource_mut::<RoundScript>()
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
    match world.resource::<RoundScript>().entities.get(&id) {
        Some(entity) if entity.kind == EntityKind::Actor => Ok(id),
        _ => Err("receiver is not an actor".into()),
    }
}

/// Live actors whose team is one of `teams` ("all" or none given: any).
fn actors(world: &mut World, teams: &[Value]) -> Vec<Value> {
    let any = teams.is_empty() || teams.iter().any(|t| *t == Value::string("all"));
    let mut runtime = world.resource_mut::<RoundScript>();
    let ids: Vec<u64> = runtime
        .entities
        .iter()
        .filter(|(id, e)| e.kind == EntityKind::Actor && runtime.script_is_defined(id))
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

/// A `script_model`, as `spawn("script_model", origin)` makes it.
fn spawn_script_model(world: &mut World, origin: [f32; 3]) -> Result<u64, String> {
    let presence = super::presence::spawn_presence(world, origin)?;
    let number = crate::frame::FrameWorld::from_world(world).gentity_number(presence);
    let mut runtime = world.resource_mut::<RoundScript>();
    let id = runtime.create_entity(EntityKind::Spawned, "script_model")?;
    runtime.set_object_field(id, "origin", Value::Vector(origin));
    runtime.set_object_field(id, "angles", Value::Vector([0.0; 3]));
    runtime.set_object_field(id, "spawnflags", Value::Int(0));
    let entity = runtime.entities.get_mut(&id).unwrap();
    entity.presence = Some(presence);
    if let Some(number) = number {
        entity.number = number;
    }
    Ok(id)
}

pub(crate) fn register(registry: &mut NativeRegistry) {
    use Namespace::{Function, Method};
    campaign::register(registry);
    // SP aitype `spawner()`: the team `getspawnerteamarray` filters on.
    registry.register(Method, "setspawnerteam", |world, receiver, args| {
        let id = entity_id(world, receiver)?;
        let team = super::args::string(args, 0)?;
        world
            .resource_mut::<RoundScript>()
            .set_object_field(id, "team", Value::string(&team));
        Ok(Value::Undefined)
    });
    // SP `playsound(alias, notify, stop_on_death)`: the notify fires when the
    // sound ends. Replaces the MP one-argument form.
    // ponytail: fixed length; use the alias's sound length once sim carries it.
    for name in ["playsound", "playsoundasmaster"] {
        registry.register(Method, name, |world, receiver, args| {
            const SOUND_NOTIFY_MS: i64 = 2000;
            let id = entity_id(world, receiver)?;
            let alias = super::args::string(args, 0)?;
            if args.len() > 3 {
                return Err("expected a sound alias and at most a notify and a flag".into());
            }
            let origin = match world
                .resource_mut::<RoundScript>()
                .object_field(id, "origin")
            {
                Value::Vector(origin) => origin,
                _ => [0.0; 3],
            };
            super::natives::engine::play_sound_at(world, origin, &alias)?;
            if args.len() > 1 {
                let name = super::args::string(args, 1)?;
                let now = i64::from(crate::level_time_ms(
                    world.resource::<crate::step::StepRequest>().tick,
                ));
                world
                    .resource_mut::<RoundScript>()
                    .campaign
                    .timed_notifies
                    .push((now + SOUND_NOTIFY_MS, receiver.clone(), name.into()));
            }
            Ok(Value::Undefined)
        });
    }
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
        let mut runtime = world.resource_mut::<RoundScript>();
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
        let mut runtime = world.resource_mut::<RoundScript>();
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
            .resource::<RoundScript>()
            .campaign
            .nodes
            .iter()
            .copied()
            .map(Value::Object)
            .collect();
        new_array(world, nodes)
    });
    registry.register(Method, "setgoalpos", |world, receiver, args| {
        let at = super::args::vector(args, 0)?;
        set_goal(world, receiver, GoalTarget::Pos(at), at, None)
    });
    registry.register(Method, "setgoalnode", |world, receiver, args| {
        let (node, at) = origin_of(world, super::args::arg(args, 0)?)?;
        set_goal(world, receiver, GoalTarget::Pos(at), at, Some(node))
    });
    registry.register(Method, "setgoalentity", |world, receiver, args| {
        let (target, at) = origin_of(world, super::args::arg(args, 0)?)?;
        set_goal(world, receiver, GoalTarget::Entity(target), at, None)
    });
    registry.register(Method, "animcustom", |world, receiver, args| {
        let id = actor(world, receiver)?;
        let Some(Value::Function(function)) = args.first() else {
            return Err("animcustom expects a function".into());
        };
        let mut brain = brain(world, id);
        switch(world, id, &mut brain, "custom", Start::Function(*function));
        brain.hold = Some(Hold::Custom);
        store(world, id, brain);
        Ok(Value::Undefined)
    });
    registry.register(Method, "animscripted", |world, receiver, args| {
        let Ok(id) = actor(world, receiver) else {
            return super::script_anim::scripted_native(world, receiver, args);
        };
        const INIT: &str = "animscripts/scripted::init";
        if !has_entry(world, INIT) {
            return super::script_anim::scripted_native(world, receiver, args);
        }
        let now = super::players::now_ms(world);
        crate::script::runtime::run_now(world, INIT, receiver.clone(), args.to_vec(), now)
            .map_err(|fault| format!("{INIT}: {fault:?}"))?;
        if let Some(anim) = world
            .resource_mut::<RoundScript>()
            .entities
            .get_mut(&id)
            .and_then(|entity| entity.anim.as_mut())
        {
            anim.scripted = None;
        }
        switch_to(
            world,
            id,
            "scripted",
            Some(Hold::Scripted {
                started: false,
                ended: false,
            }),
        );
        Ok(Value::Undefined)
    });
    registry.register(Method, "animmode", |world, receiver, args| {
        let id = actor(world, receiver)?;
        let mode = super::args::string(args, 0)?.to_ascii_lowercase();
        let mut brain = brain(world, id);
        brain.anim_mode = mode.into();
        store(world, id, brain);
        Ok(Value::Undefined)
    });
    registry.register(Method, "orientmode", |world, receiver, args| {
        let id = actor(world, receiver)?;
        let mode = super::args::string(args, 0)?.to_ascii_lowercase();
        let orient = match mode.as_str() {
            "face angle" => Orient::Angle(
                number(super::args::arg(args, 1)?.clone()).ok_or("face angle expects a yaw")?,
            ),
            "face point" => Orient::Point(super::args::vector(args, 1)?),
            "face direction" => {
                let [x, y, _] = super::args::vector(args, 1)?;
                Orient::Angle(y.atan2(x).to_degrees())
            }
            "face current" => Orient::Current,
            "face motion" => Orient::Motion,
            "face enemy" | "face enemy or motion" => Orient::Enemy,
            _ => Orient::Default,
        };
        let mut brain = brain(world, id);
        brain.orient = orient;
        store(world, id, brain);
        Ok(Value::Undefined)
    });
    registry.register(Method, "teleport", |world, receiver, args| {
        let id = entity_id(world, receiver)?;
        let origin = super::args::vector(args, 0)?;
        let mut runtime = world.resource_mut::<RoundScript>();
        runtime.set_object_field(id, "origin", Value::Vector(origin));
        if let Some(angles) = args.get(1).filter(|v| **v != Value::Undefined) {
            runtime.set_object_field(id, "angles", angles.clone());
        }
        Ok(Value::Undefined)
    });
    registry.register(Method, "nearnode", |world, receiver, args| {
        let id = entity_id(world, receiver)?;
        let (_, at) = origin_of(world, super::args::arg(args, 0)?)?;
        let origin = match world
            .resource_mut::<RoundScript>()
            .object_field(id, "origin")
        {
            Value::Vector(origin) => origin,
            _ => return Ok(Value::Int(0)),
        };
        Ok(Value::Int(near(origin, at).into()))
    });
    registry.register(Method, "nearclaimnode", |world, receiver, _| {
        near_claim_node(world, receiver, false)
    });
    registry.register(Method, "nearclaimnodeandangle", |world, receiver, _| {
        near_claim_node(world, receiver, true)
    });
    registry.register(Method, "getmuzzleangle", |world, receiver, _| {
        let id = entity_id(world, receiver)?;
        Ok(world
            .resource_mut::<RoundScript>()
            .object_field(id, "angles"))
    });
    // Motion direction (`lookaheaddir`) relative to facing, degrees in -180..180.
    registry.register(Method, "getmotionangle", |world, receiver, _| {
        let id = entity_id(world, receiver)?;
        let mut runtime = world.resource_mut::<RoundScript>();
        let (Value::Vector(dir), Value::Vector(angles)) = (
            runtime.object_field(id, "lookaheaddir"),
            runtime.object_field(id, "angles"),
        ) else {
            return Ok(Value::Float(0.0));
        };
        let motion = dir[1].atan2(dir[0]).to_degrees();
        Ok(Value::Float(math_iw4::angle_subtract(motion, angles[1])))
    });
    registry.register(Method, "kill", |world, receiver, _| {
        let id = entity_id(world, receiver)?;
        let mut runtime = world.resource_mut::<RoundScript>();
        if health(&mut runtime, id) <= 0 {
            return Ok(Value::Undefined);
        }
        runtime.set_object_field(id, "health", Value::Int(0));
        drop(runtime);
        crate::script::runtime::raise(world, receiver.clone(), "death", vec![Value::Undefined]);
        Ok(Value::Undefined)
    });
    registry.register(Method, "getnegotiationstartnode", |world, receiver, _| {
        campaign::negotiation_node(world, receiver, true)
    });
    registry.register(Method, "getnegotiationendnode", |world, receiver, _| {
        campaign::negotiation_node(world, receiver, false)
    });
    registry.register(Method, "gethighestnodestance", |world, receiver, _| {
        Ok(Value::string(highest_stance(world, receiver)?))
    });
    registry.register(Method, "doesnodeallowstance", |world, receiver, args| {
        let order = |stance: &str| {
            ["prone", "crouch", "stand"]
                .iter()
                .position(|s| *s == stance)
        };
        let wanted = super::args::string(args, 0)?;
        let allowed = order(&wanted) <= order(highest_stance(world, receiver)?);
        Ok(Value::Int(allowed.into()))
    });
    // ponytail: standing eye height for every pose; read `a.pose` if crouched AI aim high.
    registry.register(Method, "getshootatpos", |world, receiver, _| {
        let id = entity_id(world, receiver)?;
        Ok(
            match world
                .resource_mut::<RoundScript>()
                .object_field(id, "origin")
            {
                Value::Vector([x, y, z]) => Value::Vector([x, y, z + EYE_HEIGHT]),
                other => other,
            },
        )
    });
    for name in ["setflashbanged", "clearentitytarget", "traversemode"] {
        registry.register(Method, name, |world, receiver, _| {
            actor(world, receiver)?;
            Ok(Value::Undefined)
        });
    }
    registry.register(Method, "shoot", shoot);
    registry.register(Method, "setdefaultaimlimits", |world, receiver, _| {
        let id = actor(world, receiver)?;
        default_aim_limits(&mut world.resource_mut::<RoundScript>(), id);
        Ok(Value::Undefined)
    });
    // `seerecently(ent, seconds)`: the enemy was seen that recently; anyone else is
    // asked of sight now.
    registry.register(Method, "seerecently", |world, receiver, args| {
        let id = actor(world, receiver)?;
        let target = entity_id(world, super::args::arg(args, 0)?)?;
        let window = number(super::args::arg(args, 1)?.clone()).unwrap_or(0.0);
        let enemy = world
            .resource_mut::<RoundScript>()
            .object_field(id, "enemy");
        if enemy == Value::Object(target) {
            let now = super::players::now_ms(world);
            let seen = brain(world, id).enemy_seen_ms;
            return Ok(Value::Int((now - seen <= (window * 1000.0) as i64).into()));
        }
        let (Some(from), Some(to)) = (eye(world, id), eye(world, target)) else {
            return Ok(Value::Int(0));
        };
        Ok(Value::Int(clear_sight(world, from, to).into()))
    });
    // ponytail: memory holds the last hostile seen only; anyone else answers with
    // where it is now. Keep per-target memory if AI pre-aims at unseen players.
    registry.register(Method, "lastknownpos", |world, receiver, args| {
        let id = actor(world, receiver)?;
        let target = entity_id(world, super::args::arg(args, 0)?)?;
        match brain(world, id).last_seen {
            Some((seen, at, _)) if seen == target => Ok(Value::Vector(at)),
            _ => Ok(super::players::entity_field(world, target, "origin")),
        }
    });
    registry.register(Method, "issuppressed", |world, receiver, _| {
        let id = actor(world, receiver)?;
        let mut runtime = world.resource_mut::<RoundScript>();
        let meter = number(runtime.object_field(id, "suppressionmeter")).unwrap_or(0.0);
        let ignore = truthy(&runtime.object_field(id, "ignoresuppression"));
        Ok(Value::Int((meter > 0.0 && !ignore).into()))
    });
    registry.register(Method, "lastknowntime", |world, receiver, args| {
        let id = actor(world, receiver)?;
        let target = entity_id(world, super::args::arg(args, 0)?)?;
        Ok(Value::Int(match brain(world, id).last_seen {
            Some((seen, _, ms)) if seen == target => i32::try_from(ms).unwrap_or(i32::MAX),
            _ => 0,
        }))
    });
    // The enemy's claimed node (else the enemy) is in the line of fire.
    registry.register(Method, "canattackenemynode", |world, receiver, _| {
        let id = actor(world, receiver)?;
        let Value::Object(enemy) = world
            .resource_mut::<RoundScript>()
            .object_field(id, "enemy")
        else {
            return Ok(Value::Int(0));
        };
        let node = world
            .resource_mut::<RoundScript>()
            .object_field(enemy, "node");
        let to = match &node {
            Value::Object(node) => match world
                .resource_mut::<RoundScript>()
                .object_field(*node, "origin")
            {
                Value::Vector([x, y, z]) => Some([x, y, z + EYE_HEIGHT]),
                _ => None,
            },
            _ => eye(world, enemy),
        };
        let Some(to) = to else {
            return Ok(Value::Int(0));
        };
        let from = super::turrets::muzzle(world, id);
        Ok(Value::Int(clear_sight(world, from, to).into()))
    });
    // ponytail: a straight body-sized sweep at step height, no ground check;
    // walk the nav mesh if AI steps off ledges.
    registry.register(Method, "maymovetopoint", |world, receiver, args| {
        let id = actor(world, receiver)?;
        let to = super::args::vector(args, 0)?;
        let Value::Vector(from) = world
            .resource_mut::<RoundScript>()
            .object_field(id, "origin")
        else {
            return Ok(Value::Int(0));
        };
        let lift = |[x, y, z]: [f32; 3]| [x, y, z + STEP_HEIGHT];
        let trace = super::presence::settled(world).trace_world(
            lift(from),
            lift(to),
            [-ACTOR_RADIUS, -ACTOR_RADIUS, 0.0],
            [ACTOR_RADIUS, ACTOR_RADIUS, ACTOR_HEIGHT - STEP_HEIGHT],
            crate::bullet_collision::MASK_PLAYER_SOLID,
        );
        Ok(Value::Int(
            (trace.fraction >= 1.0 && trace.startsolid == 0).into(),
        ))
    });
    // `dropweapon(weapon, position, [velocity scale])`: the gun falls from its tag.
    registry.register(Method, "dropweapon", |world, receiver, args| {
        let id = actor(world, receiver)?;
        let name = super::args::string(args, 0)?;
        let position = super::args::string(args, 1)?;
        let tag = match position.as_str() {
            "left" => "tag_weapon_left",
            "chest" => "tag_weapon_chest",
            "back" => "tag_stowed_back",
            _ => "tag_weapon_right",
        };
        let (origin, angles) = match super::presence::tag_world(world, id, tag) {
            Some((origin, axis)) => (origin, math_iw4::axis_to_angles(axis)),
            None => {
                let mut runtime = world.resource_mut::<RoundScript>();
                let origin = match runtime.object_field(id, "origin") {
                    Value::Vector([x, y, z]) => [x, y, z + 40.0],
                    _ => return Ok(Value::Undefined),
                };
                let angles = match runtime.object_field(id, "angles") {
                    Value::Vector(angles) => angles,
                    _ => [0.0; 3],
                };
                (origin, angles)
            }
        };
        match super::weapons::campaign::place_weapon(world, &name, origin, angles)? {
            Some(number) => {
                super::natives::player::new_item_entity(world, number, &format!("weapon_{name}"))
            }
            None => Ok(Value::Undefined),
        }
    });
    registry.register(Method, "cansee", |world, receiver, args| {
        let id = entity_id(world, receiver)?;
        let target = entity_id(world, super::args::arg(args, 0)?)?;
        let (Some(from), Some(to)) = (eye(world, id), eye(world, target)) else {
            return Ok(Value::Int(0));
        };
        Ok(Value::Int(clear_sight(world, from, to).into()))
    });
    registry.register(Method, "canshootenemy", |world, receiver, _| {
        let id = actor(world, receiver)?;
        let Value::Object(enemy) = world
            .resource_mut::<RoundScript>()
            .object_field(id, "enemy")
        else {
            return Ok(Value::Int(0));
        };
        let from = super::turrets::muzzle(world, id);
        let Some(to) = eye(world, enemy) else {
            return Ok(Value::Int(0));
        };
        Ok(Value::Int(clear_sight(world, from, to).into()))
    });
    registry.register(Method, "canshoot", |world, receiver, args| {
        let id = actor(world, receiver)?;
        let to = super::args::vector(args, 0)?;
        let from = super::turrets::muzzle(world, id);
        Ok(Value::Int(clear_sight(world, from, to).into()))
    });
    registry.register(Method, "getmuzzlepos", |world, receiver, _| {
        let id = entity_id(world, receiver)?;
        Ok(Value::Vector(super::turrets::muzzle(world, id)))
    });
    registry.register(Method, "clearenemy", |world, receiver, _| {
        let id = actor(world, receiver)?;
        world
            .resource_mut::<RoundScript>()
            .set_object_field(id, "enemy", Value::Undefined);
        Ok(Value::Undefined)
    });
    registry.register(Method, "isbadguy", |world, receiver, _| {
        let id = entity_id(world, receiver)?;
        let team = world.resource_mut::<RoundScript>().object_field(id, "team");
        Ok(Value::Int((team == Value::string("axis")).into()))
    });
    registry.register(Function, "issentient", |world, _, args| {
        let sentient = match super::args::arg(args, 0)? {
            Value::Object(id) => {
                let runtime = world.resource::<RoundScript>();
                runtime.player_client(*id).is_some()
                    || runtime
                        .entities
                        .get(id)
                        .is_some_and(|e| e.kind == EntityKind::Actor)
            }
            _ => false,
        };
        Ok(Value::Int(sentient.into()))
    });
    registry.register(Function, "isspawner", |world, _, args| {
        let Value::Object(id) = super::args::arg(args, 0)? else {
            return Ok(Value::Int(0));
        };
        let id = *id;
        let mut runtime = world.resource_mut::<RoundScript>();
        let Some(entity) = runtime
            .entities
            .get(&id)
            .filter(|e| e.kind == EntityKind::Map)
        else {
            return Ok(Value::Int(0));
        };
        let flag = if entity.classname.starts_with("actor_") {
            SPAWNER
        } else if super::entities::is_vehicle_class(&entity.classname) {
            VEHICLE_SPAWNER
        } else {
            return Ok(Value::Int(0));
        };
        let flags = match runtime.object_field(id, "spawnflags") {
            Value::Int(flags) => flags,
            _ => 0,
        };
        Ok(Value::Int((flags & flag != 0).into()))
    });
    registry.register(Function, "isai", |world, _, args| {
        let actor = world
            .resource::<RoundScript>()
            .entity(super::args::arg(args, 0)?)
            .is_some_and(|(_, e)| e.kind == EntityKind::Actor);
        Ok(Value::Int(actor.into()))
    });
}
