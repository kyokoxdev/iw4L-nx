//! Thread control the campaign host needs (actor animscripts start, poll and
//! stop threads) and its per-tick scheduler work. Fork-only; kept out of the
//! parent file so upstream merges touch it as little as possible.

use super::{Fault, RoundScript, Value, entry, find_thread, kill, spawn_thread};
use bevy_ecs::prelude::World;
use std::collections::BTreeMap;
use std::sync::Arc;

/// Queues a thread of the named entry (`start` without the restart plan); its serial.
pub(crate) fn start_thread(
    world: &mut World,
    name: &str,
    receiver: Value,
    args: Vec<Value>,
) -> Result<u64, Fault> {
    let (program, function, location) = entry(world, name)?;
    spawn_thread(world, &program, function, receiver, args).map_err(|m| Fault::at(&location, m))
}

/// Queues a thread of a script function reference; its serial.
pub(crate) fn start_function(
    world: &mut World,
    function: u32,
    receiver: Value,
    args: Vec<Value>,
) -> Result<u64, String> {
    let program = world
        .resource::<RoundScript>()
        .program
        .clone()
        .ok_or("no loaded GSC program")?;
    spawn_thread(world, &program, function as usize, receiver, args)
}

/// The thread has not ended. Not for the thread that is running now.
pub(crate) fn running(world: &mut World, serial: u64) -> bool {
    find_thread(world, serial).is_some()
}

/// Ends a thread that is not running now, queued or waiting.
pub(crate) fn stop_thread(world: &mut World, serial: u64) {
    if let Some(entity) = find_thread(world, serial) {
        kill(world, entity, serial);
    }
}

/// Campaign work at the top of each scheduler tick: script anim trees, actors,
/// then notifies timed by `playsound` and the like come due.
pub(super) fn advance(world: &mut World, now: i64) {
    super::super::host::script_anim::advance(world);
    super::super::host::actors::advance(world);
    super::super::host::mission::advance(world);
    super::super::host::weapons::campaign::advance_magic_grenades(world);
    let mut runtime = world.resource_mut::<RoundScript>();
    let (due, later): (Vec<_>, Vec<_>) = std::mem::take(&mut runtime.campaign.timed_notifies)
        .into_iter()
        .partition(|(at, _, _)| *at <= now);
    runtime.campaign.timed_notifies = later;
    runtime.pending_notifies.extend(
        due.into_iter()
            .map(|(_, receiver, name)| (receiver, name, Vec::new())),
    );
}

/// Campaign-only `RoundScript` state, grouped so the parent struct carries one
/// fork field.
#[derive(Clone, Debug, Default)]
pub(crate) struct CampaignState {
    /// Campaign level entries held until the local player begins: SP level
    /// scripts expect the player entity before `main` runs.
    pub(crate) deferred_entries: Vec<String>,
    pub(crate) videos: Arc<BTreeMap<String, i32>>,
    pub(crate) turret_ai_scripts: Arc<BTreeMap<String, String>>,
    /// The level is a campaign level (it has AI path nodes).
    pub(crate) active: bool,
    /// Boxes of entities that `disconnectpaths` took out of actor routes.
    pub(crate) blocked_paths: BTreeMap<u64, crate::script::Bounds>,
    /// `badplace_*`: areas kept out of the routes of actors on their teams.
    pub(crate) badplaces: Vec<crate::script::host::actors::BadPlace>,
    /// Dead actors, oldest first (`actors::MAX_CORPSES` are kept).
    pub(crate) corpses: std::collections::VecDeque<u64>,
    /// Vehicles players drive (`mountvehicle`), by client.
    pub(crate) drivers: BTreeMap<u32, u64>,
    /// `addaieventlistener`: the AI event types each entity hears.
    pub(crate) ai_listeners: BTreeMap<u64, std::collections::BTreeSet<Arc<str>>>,
    /// Projectiles fired for the player as a stand-in owner (`campaign::owner`).
    pub(crate) ownerless_missiles: std::collections::BTreeSet<crate::ProjectileId>,
    /// `magicgrenade`s waiting to explode.
    pub(crate) magic_grenades: Vec<super::super::host::weapons::campaign::MagicGrenade>,
    /// Turrets actors man (`useturret`): the gunner and its next shot time (ms).
    pub(crate) turret_gunners: BTreeMap<u64, (u64, i64)>,
    /// Turrets players man (`useby`): the client and its next shot time (ms).
    pub(crate) turret_users: BTreeMap<u64, (u32, i64)>,
    /// Turrets `startfiring` keeps firing: the next shot time (ms).
    pub(crate) turret_firing: BTreeMap<u64, i64>,
    /// The target each turret last had: (target, aim point, level time ms),
    /// for suppression fire after it drops out of sight.
    pub(crate) turret_seen: BTreeMap<u64, (u64, [f32; 3], i64)>,
    /// `playerlinktoblend` in progress, by client: (origin, view) at the
    /// link, level time it started and its length (ms).
    pub(crate) link_blends: BTreeMap<u32, ([f32; 3], [f32; 3], i64, i64)>,
    /// Objects `enableinvulnerability` took out of damage.
    pub(crate) invulnerable: std::collections::BTreeSet<u64>,
    /// Players `enablehealthshield(false)` took the health shield from.
    pub(crate) health_shield_off: std::collections::BTreeSet<u64>,
    /// Running health shield windows: the level time (ms) each ends.
    pub(crate) health_shield: BTreeMap<u64, i64>,
    /// Players `enabledeathshield` keeps alive at 1 health.
    pub(crate) death_shield: std::collections::BTreeSet<u64>,
    pub(crate) anim_library: Arc<super::super::host::script_anim::AnimLibrary>,
    /// Notetrack notifies seen so far, `(flag, note)`; each logs once.
    pub(crate) anim_notes_seen: std::collections::BTreeSet<(Arc<str>, Arc<str>)>,
    /// Notifies due at a level time (ms), e.g. SP `playsound` done notifies.
    pub(crate) timed_notifies: Vec<(i64, Value, Arc<str>)>,
    /// SP path node objects, in `PathData` order.
    pub(crate) nodes: Vec<u64>,
    /// Their origins, in the same order.
    pub(crate) node_origins: Vec<[f32; 3]>,
    /// Their linked node indices, in the same order.
    pub(crate) node_links: Vec<Vec<u16>>,
    /// `makeentitysentient`: non-AI entities actors fight, with their team.
    pub(crate) sentients: BTreeMap<u64, Value>,
    /// `target_set`: lock-on targets and their offsets, oldest first.
    pub(crate) targets: Vec<(u64, [f32; 3])>,
    /// Where each player holds its aim (`actors::PlayerLos`), by client.
    pub(crate) player_los: BTreeMap<u32, super::super::host::actors::PlayerLos>,
    /// Nodes in a steady player's line of fire: the level time (ms) it lapses.
    pub(crate) node_los: BTreeMap<u64, i64>,
    /// Dangerous-node marks: level time (ms) per `(node, team)`.
    pub(crate) node_danger: BTreeMap<(u64, String), i64>,
    /// SP actor goals (`setgoalpos`/`node`/`entity`).
    pub(crate) actor_goals: BTreeMap<u64, super::super::host::actors::ActorGoal>,
    /// SP actors' animscript state and motor modes.
    pub(crate) actor_brains: BTreeMap<u64, super::super::host::actors::ActorBrain>,
    /// Routes over the path nodes; installed by the bot brain.
    pub(crate) actor_nav: Option<super::super::host::actors::NavHandle>,
    /// Threat groups, ignore pairs and biases (`createthreatbiasgroup` and friends).
    pub(crate) threat: super::super::host::actors::ThreatBias,
    /// `missionfailed`/`missionsuccess`/`changelevel` or the player's death.
    pub(crate) level_change: Option<super::super::host::mission::LevelChange>,
    /// `savegame`/`savegamenocommit` this tick.
    pub(crate) save: Option<super::super::host::mission::SaveRequest>,
    /// A restored checkpoint: the tick it continues from (taken by the host
    /// clock) and when it was loaded (ms).
    pub(crate) resume_tick: Option<crate::Tick>,
    pub(crate) loaded_ms: i64,
}

impl CampaignState {
    /// Objects only the engine holds: path nodes live for the level even when
    /// no script keeps one.
    pub(crate) fn roots(&self, pending: &mut Vec<Value>) {
        pending.extend(self.nodes.iter().copied().map(Value::Object));
        pending.extend(
            self.timed_notifies
                .iter()
                .map(|(_, receiver, _)| receiver.clone()),
        );
    }
}
