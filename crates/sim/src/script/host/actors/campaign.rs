//! SP traverse nodes: an actor whose route crosses a negotiation begin node plays
//! that node's `animscript` (`animscripts/traverse/<name>::main`) and ends at the
//! end node. Without such an animscript the link is walked like any other.
//! SP cover nodes (`Cover *` path nodes) are claimed by `usecovernode`, which
//! sends the actor to them; a goal volume (`info_volume`) bounds where the
//! actor's goal may be. Fork-only; hooks in `actors.rs`.

use std::collections::{BTreeMap, BTreeSet};

use bevy_ecs::prelude::World;

use super::super::args::{arg, string, vector};
use super::super::natives::engine::entity_id;
use super::super::triggers::entity_bounds;
use super::{
    ActorBrain, DEFAULT_GOAL_HEIGHT, DEFAULT_GOAL_RADIUS, GoalTarget, actor, brain, clear_sight,
    cover_script, distance, has_entry, number, origin_of, set_goal,
};
use crate::script::{Namespace, NativeRegistry, RoundScript, Value};

/// A `badplace_cylinder`/`badplace_brush` area.
#[derive(Clone, Debug)]
pub(crate) struct BadPlace {
    name: String,
    bounds: super::Bounds,
    /// Teams it keeps out; empty = every team.
    teams: Vec<String>,
    /// Level time it lapses (ms); `None` lasts until `badplace_delete`.
    until_ms: Option<i64>,
}

/// The badplace boxes an actor of `team` routes around at `now`.
pub(super) fn badplaces_for(runtime: &RoundScript, team: &Value, now: i64) -> Vec<super::Bounds> {
    let team = match team {
        Value::String(team) => team.to_string(),
        _ => String::new(),
    };
    runtime
        .campaign
        .badplaces
        .iter()
        .filter(|place| place.until_ms.is_none_or(|until| now < until))
        .filter(|place| place.teams.is_empty() || place.teams.iter().any(|t| *t == team))
        .map(|place| place.bounds)
        .collect()
}

/// `badplace_*(name, duration, <shape>, team...)`: `duration` in seconds, -1 or 0
/// for until deleted; `bad_guys`/`good_guys` name axis/allies.
fn add_badplace(
    world: &mut World,
    args: &[Value],
    bounds: super::Bounds,
    teams_from: usize,
) -> Result<Value, String> {
    let name = string(args, 0)?.to_string();
    let duration = number(arg(args, 1)?.clone()).unwrap_or(-1.0);
    let now = super::super::players::now_ms(world);
    let teams = args
        .get(teams_from..)
        .unwrap_or_default()
        .iter()
        .filter_map(|team| match team {
            Value::String(team) => Some(match &*team.to_string() {
                "bad_guys" => "axis".to_owned(),
                "good_guys" => "allies".to_owned(),
                other => other.to_owned(),
            }),
            _ => None,
        })
        .collect();
    let mut runtime = world.resource_mut::<RoundScript>();
    let places = &mut runtime.campaign.badplaces;
    places.retain(|place| place.until_ms.is_none_or(|until| now < until));
    if !name.is_empty() {
        places.retain(|place| place.name != name);
    }
    places.push(BadPlace {
        name,
        bounds,
        teams,
        until_ms: (duration > 0.0).then(|| now + (duration * 1000.0) as i64),
    });
    for goal in runtime.campaign.actor_goals.values_mut() {
        goal.routed_to = None;
    }
    Ok(Value::Undefined)
}

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
    runtime
        .campaign
        .actor_brains
        .entry(id)
        .or_default()
        .traverse = Some((start, end));
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
pub(super) fn finish_traverse(
    world: &mut World,
    id: u64,
    brain: &mut ActorBrain,
    thread_done: bool,
) {
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

/// Threat groups and their pairs. An actor scores a hostile at its distance minus
/// `bias(actor group, its group)`, so a positive bias draws it; a pair in `ignore`
/// is never picked.
#[derive(Clone, Debug, Default)]
pub(crate) struct ThreatBias {
    groups: BTreeSet<String>,
    /// `(viewer, target)`: viewers in the first group never pick the second.
    ignore: BTreeSet<(String, String)>,
    /// `(target, viewer)` -> bias.
    bias: BTreeMap<(String, String), f32>,
}

impl ThreatBias {
    pub(super) fn ignores(&self, viewer: &str, target: &str) -> bool {
        self.ignore
            .contains(&(viewer.to_string(), target.to_string()))
    }

    pub(super) fn bias(&self, viewer: &str, target: &str) -> f32 {
        self.bias
            .get(&(target.to_string(), viewer.to_string()))
            .copied()
            .unwrap_or(0.0)
    }

    /// `setthreatbiasagainstall`: `target`'s bias for every group that exists now.
    // ponytail: groups created later get no bias; set it again after creating them.
    fn set_against_all(&mut self, target: &str, bias: f32) {
        for viewer in &self.groups {
            self.bias.insert((target.to_string(), viewer.clone()), bias);
        }
    }
}

/// An entity's threat group, `""` when it has none.
pub(super) fn group_of(runtime: &mut RoundScript, id: u64) -> String {
    match runtime.object_field(id, "threatbiasgroup") {
        Value::String(group) => group.to_string(),
        _ => String::new(),
    }
}

/// `getnegotiationstartnode` / `getnegotiationendnode`: the traverse being crossed, else undefined.
pub(super) fn negotiation_node(
    world: &mut World,
    receiver: &Value,
    start: bool,
) -> Result<Value, String> {
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

pub(super) fn vector_field(runtime: &mut RoundScript, id: u64, name: &str) -> Option<[f32; 3]> {
    match runtime.object_field(id, name) {
        Value::Vector(at) => Some(at),
        _ => None,
    }
}

/// Where the actor's `enemy` (actor or player) stands.
fn enemy_origin(world: &mut World, id: u64) -> Option<[f32; 3]> {
    let Value::Object(enemy) = world
        .resource_mut::<RoundScript>()
        .object_field(id, "enemy")
    else {
        return None;
    };
    match super::super::players::entity_field(world, enemy, "origin") {
        Value::Vector(at) => Some(at),
        _ => None,
    }
}

/// World-space box of an `info_volume` (its origin plus its brush bounds).
pub(super) fn volume_box(world: &mut World, volume: u64) -> ([f32; 3], [f32; 3]) {
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
    yaw: f32,
    forward: [f32; 3],
    /// Bearings of an enemy (degrees off its yaw, 0..360) it covers against,
    /// from..to through 0.
    bearings: (f32, f32),
    /// An enemy nearer than this outflanks it.
    min_enemy_dist: f32,
    /// Spawnflag the cover search favours.
    priority: bool,
    /// Recently marked dangerous for the searching actor's team.
    dangerous: bool,
    /// An ally searching, and the node is out of a steady player's line of fire.
    clear_of_player: bool,
    /// Other known hostiles past `engagemindist` it covers against.
    other_threats: u8,
}

/// `Cover Left`/`Cover Right` spawnflag for the wider arc.
const WIDE_COVER: i32 = 0x200;
const PRIORITY_COVER: i32 = 0x40;

/// Cover arcs per node type: `(from, to, min enemy distance)`.
fn cover_arc(kind: &str, spawnflags: i32) -> (f32, f32, f32) {
    let wide = spawnflags & WIDE_COVER != 0;
    match kind {
        "Cover Right" if wide => (310.0, 45.0, 0.0),
        "Cover Left" if wide => (315.0, 50.0, 0.0),
        "Cover Right" => (310.0, 15.0, 0.0),
        "Cover Left" => (345.0, 50.0, 0.0),
        "Cover Prone" | "Conceal Prone" => (315.0, 45.0, 800.0),
        "Cover Stand" | "Cover Crouch" | "Conceal Stand" | "Conceal Crouch" => (315.0, 45.0, 0.0),
        "Cover Crouch Window" => (330.0, 30.0, 0.0),
        _ => (300.0, 60.0, 0.0),
    }
}

/// Whether the cover shields against an enemy at `enemy`: far enough, and in
/// the arc in front of it.
fn covers_against(cover: &Cover, enemy: [f32; 3]) -> bool {
    if distance(cover.origin, enemy) < cover.min_enemy_dist {
        return false;
    }
    let bearing = ((enemy[1] - cover.origin[1])
        .atan2(enemy[0] - cover.origin[0])
        .to_degrees()
        - cover.yaw)
        .rem_euclid(360.0);
    let (from, to) = cover.bearings;
    if from <= to {
        (from..=to).contains(&bearing)
    } else {
        bearing >= from || bearing <= to
    }
}

/// `ai_coverScore_*` weights.
const SCORE_DISTANCE: f32 = 16.0;
const SCORE_TARGET_DIR: f32 = 8.0;
const SCORE_VISIBILITY: f32 = 8.0;
const SCORE_NODE_ANGLE: f32 = 4.0;
const SCORE_PRIORITY: f32 = 8.0;
const SCORE_DANGEROUS: f32 = 32.0;
const SCORE_PLAYER_LOS: f32 = 8.0;
/// Beyond this a cover's distance from the actor earns nothing.
const SCORE_FAR: f32 = 1300.0;
/// Covers nearer the actor than this are neither toward nor away from its enemy.
const SCORE_TARGET_DIR_MIN: f32 = 64.0;

/// What covers are scored from: the actor's origin and, with an enemy, where
/// it stands and the engagement pull (−1 back off … 1 close in).
#[derive(Clone, Copy, Debug)]
struct Scoring {
    from: [f32; 3],
    enemy: Option<([f32; 3], f32)>,
}

/// Engagement pull at `dist` (2D) from the enemy, from the actor's
/// `engage*` fields: inside the min falloff −1, inside min −0.5, past the max
/// falloff 1, past max 0.5.
fn engagement_pull(runtime: &mut RoundScript, id: u64, dist: f32) -> f32 {
    let mut field = |name| number(runtime.object_field(id, name)).unwrap_or(0.0);
    if dist < field("engageminfalloffdist") {
        -1.0
    } else if dist < field("engagemindist") {
        -0.5
    } else if dist > field("engagemaxfalloffdist") {
        1.0
    } else if dist > field("engagemaxdist") {
        0.5
    } else {
        0.0
    }
}

/// Cover score; `sees_enemy` is whether the cover has sight of the
/// enemy (true without one).
fn cover_score(cover: &Cover, scoring: &Scoring, sees_enemy: bool) -> f32 {
    let far = SCORE_FAR * SCORE_FAR;
    let near = (0..3)
        .map(|k| (cover.origin[k] - scoring.from[k]).powi(2))
        .sum::<f32>()
        .min(far);
    let mut score = SCORE_DISTANCE * (1.0 - near / far);
    if cover.priority {
        score += SCORE_PRIORITY;
    }
    if cover.dangerous {
        score -= SCORE_DANGEROUS;
    }
    if cover.clear_of_player {
        score += SCORE_PLAYER_LOS;
    }
    score += SCORE_NODE_ANGLE * f32::from(cover.other_threats);
    if sees_enemy {
        score += SCORE_VISIBILITY;
    }
    if let Some((enemy, pull)) = scoring.enemy {
        if covers_against(cover, enemy) {
            score += SCORE_NODE_ANGLE;
        }
        let to_enemy = glam::Vec2::new(enemy[0] - scoring.from[0], enemy[1] - scoring.from[1])
            .normalize_or_zero();
        let step = glam::Vec2::new(
            cover.origin[0] - scoring.from[0],
            cover.origin[1] - scoring.from[1],
        );
        if step.length() >= SCORE_TARGET_DIR_MIN {
            let toward = to_enemy.dot(step);
            if toward > 0.2 {
                score += SCORE_TARGET_DIR * pull;
            } else if toward < -0.2 {
                score -= SCORE_TARGET_DIR * pull;
            }
        }
    }
    score
}

/// The highest scoring cover, `(cover, sees enemy)`.
fn best_cover(scoring: &Scoring, covers: &[(Cover, bool)]) -> Option<u64> {
    covers
        .iter()
        .max_by(|a, b| cover_score(&a.0, scoring, a.1).total_cmp(&cover_score(&b.0, scoring, b.1)))
        .map(|(cover, _)| cover.id)
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

/// Covers a search looks at, at most.
const COVER_SEARCH_MAX: usize = 256;
/// A cover search reaches no farther than this from its centre.
const COVER_SEARCH_RADIUS: f32 = 716.8;

/// Where `findbestcovernode` looks: a goal radius past the search reach is
/// searched around the actor, pulled toward the goal far enough to stay
/// inside it; a smaller goal is searched whole.
// ponytail: the wider 1024 reach (an unnamed actor flag) and the node-goal
// shrink (radius − 128, at least 64) are left out; neither flag is decoded.
fn cover_search(runtime: &mut RoundScript, id: u64) -> Area {
    let origin = vector_field(runtime, id, "origin").unwrap_or([0.0; 3]);
    let goal = vector_field(runtime, id, "goalpos").unwrap_or(origin);
    let radius = number(runtime.object_field(id, "goalradius")).unwrap_or(DEFAULT_GOAL_RADIUS);
    let height = number(runtime.object_field(id, "goalheight")).unwrap_or(DEFAULT_GOAL_HEIGHT);
    let (center, radius) = search_centre(origin, goal, radius);
    Area::Sphere {
        center,
        radius,
        height,
    }
}

/// Centre and radius of a cover search for an actor at `origin` with a goal
/// of `radius` at `goal`.
fn search_centre(origin: [f32; 3], goal: [f32; 3], radius: f32) -> ([f32; 3], f32) {
    if radius <= COVER_SEARCH_RADIUS {
        return (goal, radius);
    }
    let to_goal = glam::Vec3::from(goal) - glam::Vec3::from(origin);
    let pull = (to_goal.length() - (radius - COVER_SEARCH_RADIUS)).max(0.0);
    let at = glam::Vec3::from(origin) + to_goal.normalize_or_zero() * pull;
    ([at.x, at.y, goal[2]], COVER_SEARCH_RADIUS)
}

/// The nearest [`COVER_SEARCH_MAX`] covers inside `search`.
// ponytail: nearest first; the original keeps the first 256 its node tree yields.
fn search_covers(runtime: &mut RoundScript, search: &Area) -> Vec<Cover> {
    let center = match search {
        Area::Sphere { center, .. } => Some(*center),
        _ => None,
    };
    let mut covers: Vec<Cover> = runtime
        .campaign
        .nodes
        .clone()
        .into_iter()
        .filter_map(|node| cover_at(runtime, node))
        .filter(|cover| inside(search, cover.origin))
        .collect();
    if let Some(center) = center {
        let planar = |at: [f32; 3]| (at[0] - center[0]).hypot(at[1] - center[1]);
        covers.sort_by(|a, b| planar(a.origin).total_cmp(&planar(b.origin)));
    }
    covers.truncate(COVER_SEARCH_MAX);
    covers
}

/// The best scoring free cover node inside `area` for `id`, from the covers in `search`.
// ponytail: covers traced to the enemy from standing eye height; read
// precomputed node sight instead if traces cost too much.
fn find_cover(
    world: &mut World,
    id: u64,
    area: &Area,
    search: &Area,
    skip: Option<u64>,
) -> Option<u64> {
    let now = super::super::players::now_ms(world);
    let enemy = enemy_origin(world, id);
    let known = brain(world, id).known;
    let enemy_eye = enemy.map(|at| [at[0], at[1], at[2] + super::EYE_HEIGHT]);
    let mut runtime = world.resource_mut::<RoundScript>();
    let from = vector_field(&mut runtime, id, "origin").unwrap_or([0.0; 3]);
    let team = runtime.object_field(id, "team");
    let min_dist = number(runtime.object_field(id, "engagemindist")).unwrap_or(0.0);
    let current = runtime.object_field(id, "enemy");
    let others: Vec<[f32; 3]> = known
        .into_iter()
        .filter(|(other, seen, _)| {
            now - seen < super::ENEMY_MEMORY_MS
                && Value::Object(*other) != current
                && runtime.live(other)
        })
        .map(|(_, _, at)| at)
        .collect();
    let scoring = Scoring {
        from,
        enemy: enemy.map(|at| {
            let dist = (at[0] - from[0]).hypot(at[1] - from[1]);
            (at, engagement_pull(&mut runtime, id, dist))
        }),
    };
    let mut covers = Vec::new();
    for mut cover in search_covers(&mut runtime, search) {
        let node = cover.id;
        if Some(node) == skip || !free_for(&mut runtime, id, node) {
            continue;
        }
        if inside(area, cover.origin) {
            cover.dangerous = node_marked(&runtime, node, &team, now);
            cover.clear_of_player = team == Value::string("allies")
                && runtime
                    .campaign
                    .node_los
                    .get(&node)
                    .is_none_or(|until| *until < now);
            let covered = others
                .iter()
                .filter(|at| {
                    distance(cover.origin, **at) > min_dist && covers_against(&cover, **at)
                })
                .count();
            cover.other_threats = covered.min(u8::MAX.into()) as u8;
            covers.push(cover);
        }
    }
    drop(runtime);
    let covers: Vec<(Cover, bool)> = covers
        .into_iter()
        .map(|cover| {
            let eye = [
                cover.origin[0],
                cover.origin[1],
                cover.origin[2] + super::EYE_HEIGHT,
            ];
            let sees = enemy_eye.is_none_or(|enemy| clear_sight(world, eye, enemy));
            (cover, sees)
        })
        .collect();
    best_cover(&scoring, &covers)
}

/// The cover at a path node, if the node is a cover kind with an origin and angles.
fn cover_at(runtime: &mut RoundScript, node: u64) -> Option<Cover> {
    let Value::String(kind) = runtime.object_field(node, "type") else {
        return None;
    };
    let origin = vector_field(runtime, node, "origin")?;
    let angles = vector_field(runtime, node, "angles")?;
    if cover_script(&kind).is_none() {
        return None;
    }
    let spawnflags = match runtime.object_field(node, "spawnflags") {
        Value::Int(flags) => flags,
        _ => 0,
    };
    let (from, to, min_enemy_dist) = cover_arc(&kind, spawnflags);
    let radians = angles[1].to_radians();
    Some(Cover {
        id: node,
        origin,
        yaw: angles[1],
        forward: [radians.cos(), radians.sin(), 0.0],
        bearings: (from, to),
        min_enemy_dist,
        priority: spawnflags & PRIORITY_COVER != 0,
        dangerous: false,
        clear_of_player: false,
        other_threats: 0,
    })
}

/// The actor is at its claimed cover node and the node shields it from `at`.
pub(super) fn covered_from(world: &mut World, id: u64, at: [f32; 3]) -> bool {
    if super::near_claim_node(world, &Value::Object(id), false) != Ok(Value::Int(1)) {
        return false;
    }
    let mut runtime = world.resource_mut::<RoundScript>();
    let Value::Object(node) = runtime.object_field(id, "node") else {
        return false;
    };
    cover_at(&mut runtime, node).is_some_and(|cover| covers_against(&cover, at))
}

/// A player holding still marks the path nodes in its line of fire, and
/// allies' cover search prefers the others.
const LOS_HALF_WIDTH: f32 = 25.0;
const LOS_RANGE: f32 = 150.0;
/// The aim counts as held within this cosine (10 degrees) of where it rests.
const LOS_STEADY_COS: f32 = 0.9848;
const LOS_PERIOD_MS: i64 = 750;
const LOS_MARK_MS: i64 = 1500;
/// Height around the aim line's midpoint the marked nodes are taken from.
const LOS_HEIGHT: f32 = 80.0;

/// Where a player rests its aim: eased origin and planar view direction, and
/// when it last marked nodes.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct PlayerLos {
    origin: [f32; 3],
    dir: [f32; 2],
    marked_ms: i64,
}

impl PlayerLos {
    /// True when the aim has held long enough to mark again.
    fn steady(&mut self, origin: [f32; 3], dir: [f32; 2], now: i64) -> bool {
        let moved = (origin[0] - self.origin[0]).hypot(origin[1] - self.origin[1]);
        let turned = self.dir[0] * dir[0] + self.dir[1] * dir[1];
        if moved > LOS_HALF_WIDTH || turned < LOS_STEADY_COS {
            *self = PlayerLos {
                origin,
                dir,
                marked_ms: now,
            };
            return false;
        }
        for (eased, to) in self.origin.iter_mut().zip(origin) {
            *eased += (to - *eased) * 0.25;
        }
        for (eased, to) in self.dir.iter_mut().zip(dir) {
            *eased += (to - *eased) * 0.25;
        }
        if now - self.marked_ms < LOS_PERIOD_MS {
            return false;
        }
        self.marked_ms = now;
        true
    }
}

/// Planar distance from `p` to the segment `a`..`b`.
fn segment_distance(p: [f32; 3], a: [f32; 3], b: [f32; 3]) -> f32 {
    let (p, a, b) = (
        glam::Vec2::new(p[0], p[1]),
        glam::Vec2::new(a[0], a[1]),
        glam::Vec2::new(b[0], b[1]),
    );
    let ab = b - a;
    let t = if ab.length_squared() > 0.0 {
        ((p - a).dot(ab) / ab.length_squared()).clamp(0.0, 1.0)
    } else {
        0.0
    };
    p.distance(a + ab * t)
}

// ponytail: marks only; allies already holding a marked node are not moved off it.
pub(super) fn mark_player_los(world: &mut World) {
    let now = super::super::players::now_ms(world);
    let clients: Vec<u32> = world
        .resource::<RoundScript>()
        .players
        .keys()
        .copied()
        .collect();
    for client in clients {
        let Some((origin, height, angles)) = crate::frame::FrameWorld::from_world(world)
            .player(crate::world::ClientId(client))
            .map(|ps| (ps.origin, ps.view_height_current, ps.viewangles))
        else {
            continue;
        };
        let yaw = angles[1].to_radians();
        let steady = world
            .resource_mut::<RoundScript>()
            .campaign
            .player_los
            .entry(client)
            .or_default()
            .steady(origin, [yaw.cos(), yaw.sin()], now);
        if !steady {
            continue;
        }
        let eye = [origin[0], origin[1], origin[2] + height];
        let (forward, _, _) = math_iw4::angle_vectors(angles);
        let end: [f32; 3] = std::array::from_fn(|k| eye[k] + forward[k] * LOS_RANGE);
        let trace = super::super::presence::settled(world).trace_world(
            eye,
            end,
            [0.0; 3],
            [0.0; 3],
            crate::bullet_collision::MASK_SHOT,
        );
        let end: [f32; 3] = std::array::from_fn(|k| eye[k] + (end[k] - eye[k]) * trace.fraction);
        let mid_z = (eye[2] + end[2]) * 0.5;
        let mut runtime = world.resource_mut::<RoundScript>();
        let campaign = &mut runtime.campaign;
        for (node, at) in campaign.nodes.iter().zip(&campaign.node_origins) {
            if (at[2] - mid_z).abs() <= LOS_HEIGHT
                && segment_distance(*at, eye, end) <= LOS_HALF_WIDTH
            {
                campaign.node_los.insert(*node, now + LOS_MARK_MS);
            }
        }
    }
}

/// A dangerous-node mark lasts this long; a node is dangerous only past it,
/// i.e. once marked again inside the window, which extends it.
const DANGER_MARK_MS: i64 = 15_000;
const DANGER_REMARK_MS: i64 = 60_000;
/// An actor's death marks the path node nearest it within this.
const DANGER_NODE_RANGE: f32 = 192.0;

/// Teams that mark and read dangerous nodes.
fn danger_team(team: &Value) -> Option<String> {
    match team {
        Value::String(team) => {
            let team = team.to_string();
            matches!(team.as_str(), "axis" | "allies" | "neutral").then_some(team)
        }
        _ => None,
    }
}

fn remark(until: Option<i64>, now: i64) -> i64 {
    now + if until.is_some_and(|until| until > now) {
        DANGER_REMARK_MS
    } else {
        DANGER_MARK_MS
    }
}

fn dangerous_until(until: Option<i64>, now: i64) -> bool {
    until.is_some_and(|until| until > now + DANGER_MARK_MS)
}

/// The cover search's test: any mark still running.
fn node_marked(runtime: &RoundScript, node: u64, team: &Value, now: i64) -> bool {
    danger_team(team).is_some_and(|team| {
        runtime
            .campaign
            .node_danger
            .get(&(node, team))
            .is_some_and(|until| *until > now)
    })
}

fn node_dangerous(runtime: &RoundScript, node: u64, team: &Value, now: i64) -> bool {
    danger_team(team).is_some_and(|team| {
        dangerous_until(
            runtime.campaign.node_danger.get(&(node, team)).copied(),
            now,
        )
    })
}

/// A dead actor marks the node nearest it and that node's links dangerous for its team.
pub(super) fn mark_death_danger(world: &mut World, id: u64) {
    let now = super::super::players::now_ms(world);
    let mut runtime = world.resource_mut::<RoundScript>();
    let Some(team) = danger_team(&runtime.object_field(id, "team")) else {
        return;
    };
    let Some(at) = vector_field(&mut runtime, id, "origin") else {
        return;
    };
    let Some(index) = super::awareness::nearest_node(&runtime, at, DANGER_NODE_RANGE)
        .and_then(|node| runtime.campaign.nodes.iter().position(|n| *n == node))
    else {
        return;
    };
    let campaign = &mut runtime.campaign;
    let links = campaign.node_links.get(index).into_iter().flatten();
    let marked: Vec<u64> = std::iter::once(index)
        .chain(links.map(|k| usize::from(*k)))
        .filter_map(|k| campaign.nodes.get(k).copied())
        .collect();
    for node in marked {
        let until = campaign
            .node_danger
            .entry((node, team.clone()))
            .or_default();
        *until = remark(Some(*until), now);
    }
}

/// Pain marks the actor's claimed node for its team (once alone is not dangerous).
// ponytail: claimed node only; mark the claimed node's
// neighbours too if actors keep returning to bad cover.
pub(super) fn mark_pain_danger(world: &mut World, id: u64) {
    let now = super::super::players::now_ms(world);
    let mut runtime = world.resource_mut::<RoundScript>();
    let Some(team) = danger_team(&runtime.object_field(id, "team")) else {
        return;
    };
    if let Value::Object(node) = runtime.object_field(id, "node") {
        runtime
            .campaign
            .node_danger
            .insert((node, team), now + DANGER_MARK_MS);
    }
}

pub(super) fn register(registry: &mut NativeRegistry) {
    use Namespace::{Function, Method};
    registry.register(Function, "badplace_cylinder", |world, _, args| {
        let at = vector(args, 2)?;
        let radius = number(arg(args, 3)?.clone()).unwrap_or(0.0);
        let height = number(arg(args, 4)?.clone()).unwrap_or(0.0);
        let bounds = (
            [at[0] - radius, at[1] - radius, at[2]],
            [at[0] + radius, at[1] + radius, at[2] + height],
        );
        add_badplace(world, args, bounds, 5)
    });
    registry.register(Function, "badplace_brush", |world, _, args| {
        let id = entity_id(world, arg(args, 2)?)?;
        let (lo, hi) = entity_bounds(world, id);
        let at = match world
            .resource_mut::<RoundScript>()
            .object_field(id, "origin")
        {
            Value::Vector(at) => at,
            _ => [0.0; 3],
        };
        let bounds = (
            std::array::from_fn(|k| at[k] + lo[k]),
            std::array::from_fn(|k| at[k] + hi[k]),
        );
        add_badplace(world, args, bounds, 3)
    });
    registry.register(Function, "badplace_delete", |world, _, args| {
        let name = match arg(args, 0)? {
            Value::Int(n) => n.to_string(),
            _ => string(args, 0)?.to_string(),
        };
        let mut runtime = world.resource_mut::<RoundScript>();
        runtime
            .campaign
            .badplaces
            .retain(|place| place.name != name);
        for goal in runtime.campaign.actor_goals.values_mut() {
            goal.routed_to = None;
        }
        Ok(Value::Undefined)
    });
    registry.register(Function, "createthreatbiasgroup", |world, _, args| {
        let name = string(args, 0)?;
        world
            .resource_mut::<RoundScript>()
            .campaign
            .threat
            .groups
            .insert(name);
        Ok(Value::Undefined)
    });
    registry.register(Function, "threatbiasgroupexists", |world, _, args| {
        let name = string(args, 0)?;
        let runtime = world.resource::<RoundScript>();
        Ok(Value::Int(
            runtime.campaign.threat.groups.contains(&name).into(),
        ))
    });
    registry.register(Function, "setignoremegroup", |world, _, args| {
        let pair = (string(args, 0)?, string(args, 1)?);
        world
            .resource_mut::<RoundScript>()
            .campaign
            .threat
            .ignore
            .insert(pair);
        Ok(Value::Undefined)
    });
    registry.register(Function, "setthreatbias", |world, _, args| {
        let key = (string(args, 0)?, string(args, 1)?);
        let bias = number(arg(args, 2)?.clone()).ok_or("setthreatbias expects a number")?;
        world
            .resource_mut::<RoundScript>()
            .campaign
            .threat
            .bias
            .insert(key, bias);
        Ok(Value::Undefined)
    });
    registry.register(Function, "setthreatbiasagainstall", |world, _, args| {
        let target = string(args, 0)?;
        let bias =
            number(arg(args, 1)?.clone()).ok_or("setthreatbiasagainstall expects a number")?;
        world
            .resource_mut::<RoundScript>()
            .campaign
            .threat
            .set_against_all(&target, bias);
        Ok(Value::Undefined)
    });
    registry.register(Function, "getthreatbias", |world, _, args| {
        let (target, viewer) = (string(args, 0)?, string(args, 1)?);
        let runtime = world.resource::<RoundScript>();
        Ok(Value::Float(runtime.campaign.threat.bias(&viewer, &target)))
    });
    registry.register(Method, "setthreatbiasgroup", |world, receiver, args| {
        let id = entity_id(world, receiver)?;
        let group = string(args, 0)?;
        world.resource_mut::<RoundScript>().set_object_field(
            id,
            "threatbiasgroup",
            Value::string(&group),
        );
        Ok(Value::Undefined)
    });
    registry.register(Method, "getthreatbiasgroup", |world, receiver, _| {
        let id = entity_id(world, receiver)?;
        let group = group_of(&mut world.resource_mut::<RoundScript>(), id);
        Ok(Value::string(&group))
    });
    registry.register(Method, "findbestcovernode", |world, receiver, _| {
        let id = actor(world, receiver)?;
        let area = goal_area(world, id);
        let search = cover_search(&mut world.resource_mut::<RoundScript>(), id);
        Ok(find_cover(world, id, &area, &search, None).map_or(Value::Undefined, Value::Object))
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
        Ok(find_cover(world, id, &area, &area, skip).map_or(Value::Undefined, Value::Object))
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
    // At its claimed node, and that node is dangerous for its team.
    registry.register(Method, "atdangerousnode", |world, receiver, _| {
        let id = actor(world, receiver)?;
        if super::near_claim_node(world, receiver, false)? != Value::Int(1) {
            return Ok(Value::Int(0));
        }
        let now = super::super::players::now_ms(world);
        let mut runtime = world.resource_mut::<RoundScript>();
        let (Value::Object(node), team) = (
            runtime.object_field(id, "node"),
            runtime.object_field(id, "team"),
        ) else {
            return Ok(Value::Int(0));
        };
        Ok(Value::Int(
            node_dangerous(&runtime, node, &team, now).into(),
        ))
    });
    // Whether the actor's claimed node covers it against its enemy.
    registry.register(Method, "iscovervalidagainstenemy", |world, receiver, _| {
        let id = actor(world, receiver)?;
        let Some(enemy) = enemy_origin(world, id) else {
            return Ok(Value::Int(0));
        };
        let mut runtime = world.resource_mut::<RoundScript>();
        let Value::Object(node) = runtime.object_field(id, "node") else {
            return Ok(Value::Int(0));
        };
        let valid = cover_at(&mut runtime, node).is_some_and(|cover| covers_against(&cover, enemy));
        Ok(Value::Int(valid.into()))
    });
    registry.register(Method, "getvalidcoverpeekouts", |world, receiver, _| {
        let node = entity_id(world, receiver)?;
        let cover = cover_at(&mut world.resource_mut::<RoundScript>(), node);
        let modes = cover.map_or_else(Vec::new, |cover| peekouts(world, &cover));
        super::super::arrays::new_array(world, modes.into_iter().map(Value::string).collect())
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

/// How far a peek steps out sideways, then looks ahead.
const PEEK_STEP: f32 = 32.0;
/// Eye height a peek is traced at.
const PEEK_HEIGHT: f32 = 40.0;

/// Sides of a cover an actor can step out of: the step sideways is clear and
/// so is the view ahead from there.
// ponytail: two traces per side at one height, no "over" for crouch walls;
// add it if crouch covers never fire over the top.
fn peekouts(world: &mut World, cover: &Cover) -> Vec<&'static str> {
    let [fx, fy, _] = cover.forward;
    let from = [
        cover.origin[0],
        cover.origin[1],
        cover.origin[2] + PEEK_HEIGHT,
    ];
    [("left", -1.0), ("right", 1.0)]
        .into_iter()
        .filter(|(_, sign)| {
            let side = [
                from[0] + fy * sign * PEEK_STEP,
                from[1] - fx * sign * PEEK_STEP,
                from[2],
            ];
            let ahead = [side[0] + fx * PEEK_STEP, side[1] + fy * PEEK_STEP, side[2]];
            clear_sight(world, from, side) && clear_sight(world, side, ahead)
        })
        .map(|(mode, _)| mode)
        .collect()
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

    fn cover(id: u64, origin: [f32; 3], yaw: f32, kind: &str) -> Cover {
        let (from, to, min_enemy_dist) = cover_arc(kind, 0);
        Cover {
            id,
            origin,
            yaw,
            forward: [yaw.to_radians().cos(), yaw.to_radians().sin(), 0.0],
            bearings: (from, to),
            min_enemy_dist,
            priority: false,
            dangerous: false,
            clear_of_player: false,
            other_threats: 0,
        }
    }

    /// The aim marks after holding 750 ms, again 750 ms later; moving resets it.
    #[test]
    fn player_los_marks_a_held_aim() {
        let mut los = PlayerLos::default();
        assert!(!los.steady([0.0; 3], [1.0, 0.0], 0));
        assert!(!los.steady([5.0, 0.0, 0.0], [1.0, 0.0], 500));
        assert!(los.steady([5.0, 0.0, 0.0], [1.0, 0.0], 800));
        assert!(!los.steady([5.0, 0.0, 0.0], [1.0, 0.0], 1000));
        assert!(los.steady([5.0, 0.0, 0.0], [1.0, 0.0], 1600));
        assert!(!los.steady([100.0, 0.0, 0.0], [1.0, 0.0], 3000));
        assert!(!los.steady([100.0, 0.0, 0.0], [0.0, 1.0], 4000));
        assert!(
            segment_distance([50.0, 20.0, 0.0], [0.0; 3], [150.0, 0.0, 60.0]) <= LOS_HALF_WIDTH
        );
        assert!(segment_distance([200.0, 0.0, 0.0], [0.0; 3], [150.0, 0.0, 0.0]) > LOS_HALF_WIDTH);
    }

    /// A small goal is searched whole; a big one around the actor, pulled
    /// inside the goal when the actor stands near its edge or outside.
    #[test]
    fn cover_search_stays_inside_the_goal() {
        let goal = [0.0, 0.0, 10.0];
        assert_eq!(search_centre([500.0, 0.0, 0.0], goal, 512.0), (goal, 512.0));
        let deep = search_centre([100.0, 0.0, 0.0], goal, 2048.0);
        assert_eq!(deep, ([100.0, 0.0, 10.0], COVER_SEARCH_RADIUS));
        let (outside, _) = search_centre([3000.0, 0.0, 0.0], goal, 2048.0);
        assert!((outside[0] - (2048.0 - COVER_SEARCH_RADIUS)).abs() < 0.01);
    }

    /// One mark is not dangerous; a second inside its 15 s makes it so for 60 s.
    #[test]
    fn nodes_turn_dangerous_on_a_second_mark() {
        let first = remark(None, 0);
        assert!(!dangerous_until(Some(first), 0));
        let second = remark(Some(first), 5_000);
        assert!(dangerous_until(Some(second), 5_000));
        assert!(dangerous_until(Some(second), 49_000));
        assert!(!dangerous_until(Some(second), 50_000));
        assert_eq!(remark(Some(first), 20_000), 35_000);
    }

    /// A dangerous cover loses to an equal safe one.
    #[test]
    fn best_cover_avoids_dangerous_nodes() {
        let safe = cover(1, [100.0, 0.0, 0.0], 0.0, "Cover Stand");
        let dangerous = Cover {
            id: 2,
            dangerous: true,
            ..safe
        };
        let scoring = Scoring {
            from: [0.0; 3],
            enemy: None,
        };
        assert_eq!(
            best_cover(&scoring, &[(dangerous, true), (safe, true)]),
            Some(1)
        );
        let shields_more = Cover {
            id: 3,
            other_threats: 1,
            ..safe
        };
        assert_eq!(
            best_cover(&scoring, &[(safe, true), (shields_more, true)]),
            Some(3)
        );
    }

    /// A cover shields against an enemy in its arc in front; prone cover not
    /// against a near one.
    #[test]
    fn cover_arcs_face_the_enemy() {
        let enemy = [500.0, 0.0, 0.0];
        assert!(covers_against(
            &cover(1, [0.0; 3], 0.0, "Cover Stand"),
            enemy
        ));
        assert!(!covers_against(
            &cover(1, [0.0; 3], 180.0, "Cover Stand"),
            enemy
        ));
        // Cover Right reaches 50 degrees to its right, 15 to its left.
        assert!(covers_against(
            &cover(1, [0.0; 3], 40.0, "Cover Right"),
            enemy
        ));
        assert!(!covers_against(
            &cover(1, [0.0; 3], -40.0, "Cover Right"),
            enemy
        ));
        assert!(!covers_against(
            &cover(1, [0.0; 3], 0.0, "Cover Prone"),
            enemy
        ));
    }

    /// Of two covers equally near, the one facing the enemy wins; with the
    /// enemy too far, a cover toward it beats a nearer one behind.
    #[test]
    fn best_cover_scores_facing_and_engagement() {
        let facing = cover(1, [100.0, 0.0, 0.0], 0.0, "Cover Stand");
        let away = cover(2, [-100.0, 0.0, 0.0], 180.0, "Cover Stand");
        let hold = Scoring {
            from: [0.0; 3],
            enemy: Some(([1000.0, 0.0, 0.0], 0.0)),
        };
        assert_eq!(best_cover(&hold, &[(away, true), (facing, true)]), Some(1));
        let toward = cover(1, [300.0, 0.0, 0.0], 0.0, "Cover Stand");
        let behind = cover(2, [-100.0, 0.0, 0.0], 0.0, "Cover Stand");
        let close_in = Scoring {
            enemy: Some(([1000.0, 0.0, 0.0], 1.0)),
            ..hold
        };
        assert_eq!(
            best_cover(&close_in, &[(behind, true), (toward, true)]),
            Some(1)
        );
    }

    /// `set_against_all` sets the target's bias for each existing group, and only those.
    #[test]
    fn threat_bias_set_against_all_covers_existing_groups() {
        let mut threat = ThreatBias::default();
        threat.groups.insert("axis".into());
        threat.groups.insert("allies".into());
        threat.set_against_all("leapfrog", -200.0);
        assert_eq!(threat.bias("axis", "leapfrog"), -200.0);
        assert_eq!(threat.bias("allies", "leapfrog"), -200.0);
        assert_eq!(threat.bias("axis", "allies"), 0.0);
    }

    /// A positive bias draws the viewer toward that group; ignores are one-way pairs.
    #[test]
    fn threat_bias_draws_and_ignores_by_group() {
        let mut threat = ThreatBias::default();
        threat
            .bias
            .insert(("just_player".into(), "bridge_attackers".into()), 3000.0);
        threat.ignore.insert(("axis".into(), "allies".into()));
        assert_eq!(threat.bias("bridge_attackers", "just_player"), 3000.0);
        assert_eq!(threat.bias("bridge_attackers", "allies"), 0.0);
        assert!(threat.ignores("axis", "allies"));
        assert!(!threat.ignores("allies", "axis"));
    }
}
