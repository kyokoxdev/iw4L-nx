//! SP actor awareness: engagement distances, how much of a player enemy an
//! actor sees when it opens fire, where its enemy will likely come from, and
//! the door nodes on its route. Fork-only; registered from `actors.rs`.

use bevy_ecs::prelude::World;

use super::super::args::{arg, float};
use super::super::natives::engine::entity_id;
use super::campaign::vector_field;
use super::{EYE_HEIGHT, actor, brain, clear_sight, eye, number, store, yaw_to};
use crate::script::{Namespace, NativeRegistry, RoundScript, Value};

/// Points up a player enemy's body an actor checks before a volley, from the
/// eye down (fraction of the way from the feet, score each).
const SIGHT_POINTS: [(f32, f32); 4] = [(1.0, 10.0), (0.75, 30.0), (0.5, 30.0), (0.25, 30.0)];

/// How often the likely enemy approach is worked out again (ms).
const LIKELY_PATH_MS: i64 = 500;

/// `checkcoverexitposwithpath` looks this far along the route at most.
const EXIT_PATH_RANGE: f32 = 128.0;

/// `setruntopos`: this close (planar) counts as arrived.
const RUN_TO_ARRIVED: f32 = 16.0;

/// `getdroptofloorposition`: an AI-sized box dropped from just under head
/// height down this far.
const DROP_DEPTH: f32 = 1024.0;
const DROP_START: f32 = 42.0;
const DROP_BOX: ([f32; 3], [f32; 3]) = ([-15.0, -15.0, 0.0], [15.0, 15.0, 30.0]);

/// `flagenemyunattackable`: how long the enemy is given up on (ms).
const UNATTACKABLE_MS: i64 = 5000;

/// A sentient is indoors when its nearest node within this range is flagged so.
const INDOOR_NODE_RANGE: f32 = 192.0;
const INDOOR_NODE_FLAG: i32 = 0x2000;

/// The share of the actor's shot accuracy its sight of a player enemy allows:
/// 0.5 seeing none of the checked points, 1 seeing them all. 1 for other enemies.
fn player_sight_accuracy(world: &mut World, id: u64) -> f32 {
    let (enemy, fov, sight_sq, yaw) = {
        let mut runtime = world.resource_mut::<RoundScript>();
        let enemy = match runtime.object_field(id, "enemy") {
            Value::Object(enemy) => enemy,
            _ => return 1.0,
        };
        let fov = number(runtime.object_field(id, "fovcosine")).unwrap_or(super::DEFAULT_FOV_COS);
        let sight_sq = number(runtime.object_field(id, "maxsightdistsqrd"))
            .unwrap_or(super::DEFAULT_SIGHT_DIST * super::DEFAULT_SIGHT_DIST);
        let yaw = match runtime.object_field(id, "angles") {
            Value::Vector(angles) => angles[1],
            _ => 0.0,
        };
        (enemy, fov, sight_sq, yaw)
    };
    let Some(client) = world.resource::<RoundScript>().player_client(enemy) else {
        return 1.0;
    };
    let Some((feet, height)) = crate::frame::FrameWorld::from_world(world)
        .player(crate::ClientId(client))
        .map(|ps| (ps.origin, ps.view_height_current))
    else {
        return 1.0;
    };
    let Some(from) = eye(world, id) else {
        return 1.0;
    };
    let forward = [yaw.to_radians().cos(), yaw.to_radians().sin()];
    let mut score = 0.0;
    for (up, points) in SIGHT_POINTS {
        let at = [feet[0], feet[1], feet[2] + height * up];
        let d: [f32; 3] = std::array::from_fn(|k| at[k] - from[k]);
        let planar = d[0].hypot(d[1]).max(f32::EPSILON);
        let in_view = (d[0] * forward[0] + d[1] * forward[1]) / planar >= fov;
        if in_view
            && d.iter().map(|c| c * c).sum::<f32>() <= sight_sq
            && clear_sight(world, from, at)
        {
            score += points;
        }
    }
    0.5 + score * 0.01 * 0.5
}

/// The node its enemy will likely come from: walking the route from where the
/// enemy was last seen toward the actor, the first node the actor can see.
// ponytail: one route's nodes, sight from the actor's eye; search the
// node graph outward from the enemy against precomputed node visibility.
fn likely_enemy_node(world: &mut World, id: u64) -> Option<[f32; 3]> {
    let (enemy_at, origin, nav) = {
        let mut runtime = world.resource_mut::<RoundScript>();
        let Value::Object(enemy) = runtime.object_field(id, "enemy") else {
            return None;
        };
        let origin = vector_field(&mut runtime, id, "origin")?;
        let nav = runtime.campaign.actor_nav.clone()?;
        (enemy, origin, nav)
    };
    let enemy_at = match brain(world, id).last_seen {
        Some((seen, at, _)) if seen == enemy_at => at,
        _ => match super::super::players::entity_field(world, enemy_at, "origin") {
            Value::Vector(at) => at,
            _ => return None,
        },
    };
    let from = eye(world, id)?;
    let route = nav.0.route(enemy_at, origin, &[])?;
    // The route ends at the actor itself, which says nothing.
    route[..route.len().saturating_sub(1)]
        .iter()
        .copied()
        .find(|node| clear_sight(world, from, [node[0], node[1], node[2] + EYE_HEIGHT]))
}

/// `reacquirestep(dist)`: steps `dist` to one side of the line to its enemy
/// (random side first) to a spot inside its goal that sees the enemy, and
/// walks there. False when neither side works.
// ponytail: route-only feasibility; add a 10-unit capsule sweep.
fn reacquire_step(world: &mut World, id: u64, dist: f32) -> bool {
    let (enemy, origin, goal, sight_sq) = {
        let mut runtime = world.resource_mut::<RoundScript>();
        let Value::Object(enemy) = runtime.object_field(id, "enemy") else {
            return false;
        };
        let Some(origin) = vector_field(&mut runtime, id, "origin") else {
            return false;
        };
        let goal = vector_field(&mut runtime, id, "goalpos").map(|at| {
            let radius = number(runtime.object_field(id, "goalradius"))
                .unwrap_or(super::DEFAULT_GOAL_RADIUS);
            (at, radius)
        });
        let sight_sq = number(runtime.object_field(id, "maxsightdistsqrd"))
            .unwrap_or(super::DEFAULT_SIGHT_DIST * super::DEFAULT_SIGHT_DIST);
        (enemy, origin, goal, sight_sq)
    };
    let Some(target) = eye(world, enemy) else {
        return false;
    };
    let (x, y) = (target[0] - origin[0], target[1] - origin[1]);
    let len = x.hypot(y);
    if len < 1.0 {
        return false;
    }
    let side = [y / len, -x / len];
    let first = if super::super::natives::math::random_unit(world) < 0.5 {
        1.0
    } else {
        -1.0
    };
    for sign in [first, -first] {
        let step = [
            origin[0] + side[0] * sign * dist,
            origin[1] + side[1] * sign * dist,
            origin[2],
        ];
        if goal.is_some_and(|(at, radius)| (step[0] - at[0]).hypot(step[1] - at[1]) > radius) {
            continue;
        }
        let from = [step[0], step[1], step[2] + EYE_HEIGHT];
        let d: [f32; 3] = std::array::from_fn(|k| target[k] - from[k]);
        if d.iter().map(|c| c * c).sum::<f32>() > sight_sq || !clear_sight(world, from, target) {
            continue;
        }
        if walk_within_goal(world, id, origin, step) {
            return true;
        }
    }
    false
}

/// Routes the actor to `to` while keeping its goal: it walks there though
/// already inside the goal. False without a route or a goal.
fn walk_within_goal(world: &mut World, id: u64, origin: [f32; 3], to: [f32; 3]) -> bool {
    let mut runtime = world.resource_mut::<RoundScript>();
    let Some(nav) = runtime.campaign.actor_nav.clone() else {
        return false;
    };
    let blocked: Vec<super::Bounds> = runtime.campaign.blocked_paths.values().copied().collect();
    let Some(route) = nav.0.route(origin, to, &blocked) else {
        return false;
    };
    let Some(goal) = runtime.campaign.actor_goals.get_mut(&id) else {
        return false;
    };
    if goal.routed_to.is_none() {
        // Not routed yet: the goal tick would replace the route.
        match goal.target {
            super::GoalTarget::Pos(at) => goal.routed_to = Some(at),
            super::GoalTarget::Entity(_) => return false,
        }
    }
    goal.route = route;
    goal.stepping = true;
    true
}

/// `checkcoverexitposwithpath`: whether leaving cover toward `exit` agrees
/// with the next waypoints (at most 3, up to min(distance to `exit`, 128)
/// away): two behind the exit direction, or the first one alone, refuse it.
fn exit_fits_path(origin: [f32; 3], route: &[[f32; 3]], exit: [f32; 3]) -> bool {
    if route.is_empty() {
        return false;
    }
    let limit = (0..3)
        .map(|k| (exit[k] - origin[k]).powi(2))
        .sum::<f32>()
        .min(EXIT_PATH_RANGE * EXIT_PATH_RANGE);
    let to_exit = [exit[0] - origin[0], exit[1] - origin[1]];
    let mut behind = 0;
    for (k, p) in route.iter().take(3).enumerate() {
        let d = [p[0] - origin[0], p[1] - origin[1]];
        if d[0] * to_exit[0] + d[1] * to_exit[1] < 0.0 {
            behind += 1;
            if behind > 1 {
                return false;
            }
        }
        if d[0] * d[0] + d[1] * d[1] > limit {
            return k != 0 || behind == 0;
        }
    }
    true
}

/// `findreacquiredirectpath`: a route toward the enemy (last seen spot),
/// cut where it leaves the goal radius.
// ponytail: plain route to the enemy; weigh suppression and trim
// the path to where the enemy shows.
fn reacquire_path(world: &mut World, id: u64) -> Option<Vec<[f32; 3]>> {
    let enemy = match world
        .resource_mut::<RoundScript>()
        .object_field(id, "enemy")
    {
        Value::Object(enemy) => enemy,
        _ => return None,
    };
    let target = match brain(world, id).last_seen {
        Some((seen, at, _)) if seen == enemy => at,
        _ => match super::super::players::entity_field(world, enemy, "origin") {
            Value::Vector(at) => at,
            _ => return None,
        },
    };
    let mut runtime = world.resource_mut::<RoundScript>();
    let origin = vector_field(&mut runtime, id, "origin")?;
    let goal = vector_field(&mut runtime, id, "goalpos").map(|at| {
        let radius =
            number(runtime.object_field(id, "goalradius")).unwrap_or(super::DEFAULT_GOAL_RADIUS);
        (at, radius)
    });
    let nav = runtime.campaign.actor_nav.clone()?;
    let blocked: Vec<super::Bounds> = runtime.campaign.blocked_paths.values().copied().collect();
    let route: Vec<[f32; 3]> = nav
        .0
        .route(origin, target, &blocked)?
        .into_iter()
        .take_while(|p| goal.is_none_or(|(at, r)| (p[0] - at[0]).hypot(p[1] - at[1]) <= r))
        .collect();
    (!route.is_empty()).then_some(route)
}

/// `setruntopos` arrivals: within `RUN_TO_ARRIVED` of the spot, the actor
/// gets `runto_arrived`.
pub(super) fn run_to_arrivals(world: &mut World) {
    let waiting: Vec<(u64, [f32; 3])> = world
        .resource::<RoundScript>()
        .campaign
        .actor_brains
        .iter()
        .filter_map(|(id, b)| b.run_to.map(|at| (*id, at)))
        .collect();
    for (id, at) in waiting {
        let Value::Vector(origin) = super::super::players::entity_field(world, id, "origin") else {
            continue;
        };
        if (origin[0] - at[0]).hypot(origin[1] - at[1]) > RUN_TO_ARRIVED {
            continue;
        }
        let mut state = brain(world, id);
        state.run_to = None;
        store(world, id, state);
        crate::script::runtime::raise(world, Value::Object(id), "runto_arrived", Vec::new());
    }
}

/// Whether a hostile the actor knows of stood where `test` says when last seen.
fn knows_enemy_where(world: &mut World, id: u64, test: impl Fn([f32; 3]) -> bool) -> bool {
    let now = super::super::players::now_ms(world);
    let known = brain(world, id).known;
    known.into_iter().any(|(enemy, seen, at)| {
        now - seen < super::ENEMY_MEMORY_MS
            && world.resource::<RoundScript>().live(&enemy)
            && test(at)
    })
}

/// The actor's remaining route waypoints, next first.
fn route(runtime: &RoundScript, id: u64) -> Vec<[f32; 3]> {
    runtime
        .campaign
        .actor_goals
        .get(&id)
        .map(|goal| goal.route.clone())
        .unwrap_or_default()
}

/// The path node object at exactly `at`, if any.
fn node_at(runtime: &RoundScript, at: [f32; 3]) -> Option<u64> {
    let index = runtime
        .campaign
        .node_origins
        .iter()
        .position(|o| *o == at)?;
    runtime.campaign.nodes.get(index).copied()
}

/// The path node nearest `at` within `range`.
pub(super) fn nearest_node(runtime: &RoundScript, at: [f32; 3], range: f32) -> Option<u64> {
    runtime
        .campaign
        .node_origins
        .iter()
        .enumerate()
        .map(|(k, o)| (k, super::distance(*o, at)))
        .filter(|(_, d)| *d <= range)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .and_then(|(k, _)| runtime.campaign.nodes.get(k).copied())
}

fn set_engagement(
    world: &mut World,
    receiver: &Value,
    args: &[Value],
    (dist_field, falloff_field): (&str, &str),
    in_order: fn(f32, f32) -> bool,
) -> Result<Value, String> {
    let id = actor(world, receiver)?;
    let (dist, falloff) = (float(args, 0)?, float(args, 1)?);
    let mut runtime = world.resource_mut::<RoundScript>();
    runtime.set_object_field(id, dist_field, Value::Float(dist));
    runtime.set_object_field(id, falloff_field, Value::Float(falloff));
    if in_order(dist, falloff) {
        Ok(Value::Undefined)
    } else {
        Err(format!(
            "engagement falloff {falloff} out of order with distance {dist}"
        ))
    }
}

pub(super) fn register(registry: &mut NativeRegistry) {
    use Namespace::Method;
    // `setengagementmindist(dist, falloff)`: falloff ≤ dist.
    registry.register(Method, "setengagementmindist", |world, receiver, args| {
        set_engagement(
            world,
            receiver,
            args,
            ("engagemindist", "engageminfalloffdist"),
            |dist, falloff| falloff <= dist,
        )
    });
    // `setengagementmaxdist(dist, falloff)`: falloff ≥ dist.
    registry.register(Method, "setengagementmaxdist", |world, receiver, args| {
        set_engagement(
            world,
            receiver,
            args,
            ("engagemaxdist", "engagemaxfalloffdist"),
            |dist, falloff| falloff >= dist,
        )
    });
    registry.register(Method, "updateplayersightaccuracy", |world, receiver, _| {
        let id = actor(world, receiver)?;
        let accuracy = player_sight_accuracy(world, id);
        let mut brain = brain(world, id);
        brain.sight_accuracy = Some(accuracy);
        store(world, id, brain);
        Ok(Value::Undefined)
    });
    // Angles from the actor toward where its enemy will likely come from;
    // undefined without an enemy or such a node.
    registry.register(
        Method,
        "getanglestolikelyenemypath",
        |world, receiver, _| {
            let id = actor(world, receiver)?;
            let now = super::super::players::now_ms(world);
            let node = match brain(world, id).likely_enemy {
                Some((due, node)) if now < due => node,
                _ => {
                    let node = likely_enemy_node(world, id);
                    let mut state = brain(world, id);
                    state.likely_enemy = Some((now + LIKELY_PATH_MS, node));
                    store(world, id, state);
                    node
                }
            };
            let Some(node) = node else {
                return Ok(Value::Undefined);
            };
            let mut runtime = world.resource_mut::<RoundScript>();
            let origin = vector_field(&mut runtime, id, "origin").unwrap_or(node);
            if (node[0] - origin[0]).hypot(node[1] - origin[1]) < 1.0 {
                return Ok(runtime.object_field(id, "angles"));
            }
            Ok(Value::Vector(math_iw4::vect_to_angles(
                std::array::from_fn(|k| node[k] - origin[k]),
            )))
        },
    );
    // The next `Door`/`Door Interior` node on the actor's route.
    registry.register(Method, "getdoorpathnode", |world, receiver, _| {
        let id = actor(world, receiver)?;
        let mut runtime = world.resource_mut::<RoundScript>();
        for at in route(&runtime, id) {
            let Some(node) = node_at(&runtime, at) else {
                continue;
            };
            if let Value::String(kind) = runtime.object_field(node, "type")
                && matches!(&*kind, "Door" | "Door Interior")
            {
                return Ok(Value::Object(node));
            }
        }
        Ok(Value::Undefined)
    });
    // Whether the route reaches `node` heading the way the node faces.
    registry.register(
        Method,
        "comparenodedirtopathdir",
        |world, receiver, args| {
            let id = actor(world, receiver)?;
            let node = entity_id(world, arg(args, 0)?)?;
            let mut runtime = world.resource_mut::<RoundScript>();
            let (Some(at), Some(origin)) = (
                vector_field(&mut runtime, node, "origin"),
                vector_field(&mut runtime, id, "origin"),
            ) else {
                return Ok(Value::Int(0));
            };
            let yaw = match runtime.object_field(node, "angles") {
                Value::Vector(angles) => angles[1],
                _ => 0.0,
            };
            let route = route(&runtime, id);
            let Some(k) = route.iter().position(|p| *p == at) else {
                return Ok(Value::Int(0));
            };
            let before = if k == 0 { origin } else { route[k - 1] };
            let heading = yaw_to(before, at);
            Ok(Value::Int(
                ((heading - yaw).to_radians().cos() > 0.0).into(),
            ))
        },
    );
    registry.register(Method, "reacquirestep", |world, receiver, args| {
        let id = actor(world, receiver)?;
        let dist = float(args, 0)?;
        Ok(Value::Int(reacquire_step(world, id, dist).into()))
    });
    // Its enemy is given up on for 5 s: another enemy wins the threat pick.
    registry.register(Method, "flagenemyunattackable", |world, receiver, _| {
        let id = actor(world, receiver)?;
        let Value::Object(enemy) = world
            .resource_mut::<RoundScript>()
            .object_field(id, "enemy")
        else {
            return Ok(Value::Undefined);
        };
        let now = super::super::players::now_ms(world);
        let mut state = brain(world, id);
        state
            .unattackable
            .retain(|&(e, until)| e != enemy && now < until);
        state.unattackable.push((enemy, now + UNATTACKABLE_MS));
        store(world, id, state);
        Ok(Value::Undefined)
    });
    // `setruntopos(pos)`: runs to `pos` inside its goal, then `runto_arrived`.
    // A new spot notifies `goal_changed`.
    registry.register(Method, "setruntopos", |world, receiver, args| {
        let id = actor(world, receiver)?;
        let to = super::super::args::vector(args, 0)?;
        let mut state = brain(world, id);
        let changed = state.run_to != Some(to);
        state.run_to = Some(to);
        store(world, id, state);
        let origin = {
            let mut runtime = world.resource_mut::<RoundScript>();
            runtime.set_object_field(id, "pathgoalpos", Value::Vector(to));
            vector_field(&mut runtime, id, "origin")
        };
        if let Some(origin) = origin {
            walk_within_goal(world, id, origin, to);
        }
        if changed {
            crate::script::runtime::raise(world, Value::Object(id), "goal_changed", Vec::new());
        }
        Ok(Value::Undefined)
    });
    // `getenemyinfo(ent)`: the actor learns where `ent` is, as if it just saw it.
    registry.register(Method, "getenemyinfo", |world, receiver, args| {
        let id = actor(world, receiver)?;
        let target = entity_id(world, arg(args, 0)?)?;
        let Value::Vector(at) = super::super::players::entity_field(world, target, "origin") else {
            return Ok(Value::Undefined);
        };
        let now = super::super::players::now_ms(world);
        let mut state = brain(world, id);
        state.known.retain(|k| k.0 != target);
        state.known.push((target, now, at));
        store(world, id, state);
        Ok(Value::Undefined)
    });
    registry.register(Method, "isknownenemyinradius", |world, receiver, args| {
        let id = actor(world, receiver)?;
        let (center, radius) = (super::super::args::vector(args, 0)?, float(args, 1)?);
        let near = knows_enemy_where(world, id, |at| super::distance(at, center) <= radius);
        Ok(Value::Int(near.into()))
    });
    registry.register(Method, "isknownenemyinvolume", |world, receiver, args| {
        let id = actor(world, receiver)?;
        let volume = entity_id(world, arg(args, 0)?)?;
        let (lo, hi) = super::campaign::volume_box(world, volume);
        let inside = knows_enemy_where(world, id, |at| {
            (0..3).all(|k| lo[k] <= at[k] && at[k] <= hi[k])
        });
        Ok(Value::Int(inside.into()))
    });
    // `getdroptofloorposition([pos])`: the floor under `pos` (default its
    // origin); undefined when the box starts in solid.
    registry.register(Method, "getdroptofloorposition", |world, receiver, args| {
        let id = entity_id(world, receiver)?;
        let at = match args.first() {
            Some(Value::Undefined) | None => {
                match super::super::players::entity_field(world, id, "origin") {
                    Value::Vector(at) => at,
                    _ => return Ok(Value::Undefined),
                }
            }
            Some(_) => super::super::args::vector(args, 0)?,
        };
        let (mins, maxs) = DROP_BOX;
        let trace = super::super::presence::settled(world).trace_world(
            [at[0], at[1], at[2] + DROP_START],
            [at[0], at[1], at[2] - DROP_DEPTH],
            mins,
            maxs,
            crate::bullet_collision::MASK_PLAYER_SOLID,
        );
        if trace.startsolid != 0 {
            return Ok(Value::Undefined);
        }
        Ok(Value::Vector(trace.endpos))
    });
    // `getgroundposition(pos, radius, [down 1000], [up max(radius, 64)])`: where a
    // box `radius` wide and `2 * radius` tall, swept from `up` above to `down`
    // below `pos`, comes to rest.
    registry.register(Namespace::Function, "getgroundposition", |world, _, args| {
        let at = super::super::args::vector(args, 0)?;
        let radius = float(args, 1)?;
        let down = if args.len() > 2 { float(args, 2)? } else { 1000.0 };
        let up = if args.len() > 3 { float(args, 3)? } else { radius.max(64.0) };
        let trace = super::super::presence::settled(world).trace_world(
            [at[0], at[1], at[2] + up],
            [at[0], at[1], at[2] - down],
            [-radius, -radius, 0.0],
            [radius, radius, 2.0 * radius],
            crate::bullet_collision::MASK_PLAYER_SOLID,
        );
        if trace.startsolid != 0 {
            return Err("GetGroundPosition: check starts in solid".into());
        }
        Ok(Value::Vector(trace.endpos))
    });
    registry.register(
        Method,
        "checkcoverexitposwithpath",
        |world, receiver, args| {
            let id = actor(world, receiver)?;
            let exit = super::super::args::vector(args, 0)?;
            let mut runtime = world.resource_mut::<RoundScript>();
            let origin = vector_field(&mut runtime, id, "origin").unwrap_or(exit);
            let fits = exit_fits_path(origin, &route(&runtime, id), exit);
            Ok(Value::Int(fits.into()))
        },
    );
    registry.register(Method, "findreacquiredirectpath", |world, receiver, _| {
        let id = actor(world, receiver)?;
        let path = reacquire_path(world, id);
        let mut state = brain(world, id);
        state.reacquire_path = path;
        store(world, id, state);
        Ok(Value::Undefined)
    });
    // Walks the route `findreacquiredirectpath` found; 0 without one.
    registry.register(Method, "reacquiremove", |world, receiver, _| {
        let id = actor(world, receiver)?;
        let mut state = brain(world, id);
        let Some(path) = state.reacquire_path.take() else {
            return Ok(Value::Int(0));
        };
        store(world, id, state);
        let Some(&to) = path.last() else {
            return Ok(Value::Int(0));
        };
        let mut runtime = world.resource_mut::<RoundScript>();
        let origin = vector_field(&mut runtime, id, "origin").unwrap_or(to);
        drop(runtime);
        Ok(Value::Int(walk_within_goal(world, id, origin, to).into()))
    });
    registry.register(Namespace::Function, "isenemyteam", |_, _, args| {
        Ok(Value::Int(
            super::hostile(arg(args, 0)?, arg(args, 1)?).into(),
        ))
    });
    // An actor playing `animscripted` or `animcustom`; 0 for anything else.
    registry.register(Method, "isinscriptedstate", |world, receiver, _| {
        let Ok(id) = actor(world, receiver) else {
            return Ok(Value::Int(0));
        };
        let scripted = matches!(
            brain(world, id).hold,
            Some(super::Hold::Scripted { .. } | super::Hold::Custom)
        );
        Ok(Value::Int(scripted.into()))
    });
    // Whether the sentient's nearest node is flagged indoors.
    registry.register(Method, "isindoor", |world, receiver, _| {
        let id = entity_id(world, receiver)?;
        let Value::Vector(at) = super::super::players::entity_field(world, id, "origin") else {
            return Ok(Value::Int(0));
        };
        let mut runtime = world.resource_mut::<RoundScript>();
        let indoor = nearest_node(&runtime, at, INDOOR_NODE_RANGE).is_some_and(|node| {
            matches!(runtime.object_field(node, "spawnflags"), Value::Int(flags) if flags & INDOOR_NODE_FLAG != 0)
        });
        Ok(Value::Int(indoor.into()))
    });
}

#[cfg(test)]
mod tests {
    use super::exit_fits_path;

    #[test]
    fn cover_exit_must_follow_the_route() {
        let o = [0.0; 3];
        let ahead = [[50.0, 0.0, 0.0], [200.0, 0.0, 0.0]];
        let back = [[-50.0, 0.0, 0.0], [-80.0, 0.0, 0.0], [-300.0, 0.0, 0.0]];
        assert!(exit_fits_path(o, &ahead, [64.0, 10.0, 0.0]));
        assert!(!exit_fits_path(o, &back, [64.0, 10.0, 0.0]));
        // First waypoint already past the exit distance, behind it: refused.
        assert!(!exit_fits_path(o, &[[-300.0, 0.0, 0.0]], [64.0, 0.0, 0.0]));
        assert!(!exit_fits_path(o, &[], [64.0, 0.0, 0.0]));
    }
}
