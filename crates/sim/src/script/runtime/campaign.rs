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
    pub(crate) anim_library: Arc<super::super::host::script_anim::AnimLibrary>,
    /// Notetrack notifies seen so far, `(flag, note)`; each logs once.
    pub(crate) anim_notes_seen: std::collections::BTreeSet<(Arc<str>, Arc<str>)>,
    /// Notifies due at a level time (ms), e.g. SP `playsound` done notifies.
    pub(crate) timed_notifies: Vec<(i64, Value, Arc<str>)>,
    /// SP path node objects, in `PathData` order.
    pub(crate) nodes: Vec<u64>,
    /// SP actor goals (`setgoalpos`/`node`/`entity`).
    pub(crate) actor_goals: BTreeMap<u64, super::super::host::actors::ActorGoal>,
    /// SP actors' animscript state and motor modes.
    pub(crate) actor_brains: BTreeMap<u64, super::super::host::actors::ActorBrain>,
    /// Routes over the path nodes; installed by the bot brain.
    pub(crate) actor_nav: Option<super::super::host::actors::NavHandle>,
}
