//! Campaign level flow: mission failure, success and level changes, and the
//! level restart after the player dies. Fork-only.

use super::args::string;
use crate::frame::FrameWorld;
use crate::script::{Namespace, NativeRegistry, RoundScript, Value};
use bevy_ecs::prelude::World;

/// From the player's death or `missionfailed` to the level restart.
// ponytail: fixed delay, no death screen or retry prompt; add the deadquote
// screen and restart from the last checkpoint once saves exist.
const FAIL_RESTART_MS: i64 = 4_000;

/// The level to load once the time comes; an empty name leaves to the menu.
#[derive(Clone, Debug, Default)]
pub(crate) struct LevelChange {
    pub(crate) due_ms: i64,
    pub(crate) level: String,
    pub(crate) sent: bool,
}

fn current_level(world: &World) -> String {
    world
        .resource::<crate::script::MatchScript>()
        .dvars
        .get("mapname")
        .cloned()
        .unwrap_or_default()
}

/// The first request wins: a death after `missionsuccess` does not undo it.
fn request(world: &mut World, level: String, delay_ms: i64) {
    let due_ms = super::players::now_ms(world) + delay_ms;
    let mut runtime = world.resource_mut::<RoundScript>();
    if runtime.campaign.level_change.is_none() {
        diag::info!(Sim, "campaign: load level '{level}' in {delay_ms} ms");
        runtime.campaign.level_change = Some(LevelChange {
            due_ms,
            level,
            sent: false,
        });
    }
}

/// A dead campaign player restarts the level.
pub(crate) fn advance(world: &mut World) {
    let clients: Vec<u32> = world
        .resource::<RoundScript>()
        .players
        .keys()
        .copied()
        .collect();
    let dead = {
        let frame = FrameWorld::from_world(world);
        frame.bootstrap_ref().campaign
            && clients.into_iter().any(|c| {
                frame
                    .client_meta(crate::ClientId(c))
                    .is_some_and(|m| m.lifecycle == crate::ClientLifecycle::Dead)
            })
    };
    if dead {
        let level = current_level(world);
        request(world, level, FAIL_RESTART_MS);
    }
}

/// The level to load now, once.
pub(crate) fn take_due(world: &mut World) -> Option<String> {
    let now = super::players::now_ms(world);
    let mut runtime = world.resource_mut::<RoundScript>();
    let change = runtime.campaign.level_change.as_mut()?;
    if change.sent || change.due_ms > now {
        return None;
    }
    change.sent = true;
    Some(change.level.clone())
}

pub(crate) fn register(registry: &mut NativeRegistry) {
    use Namespace::Function;
    registry.register(Function, "missionfailed", |world, _, _| {
        let level = current_level(world);
        request(world, level, FAIL_RESTART_MS);
        Ok(Value::Undefined)
    });
    // `missionsuccess(level, [keep weapons])`, `changelevel(level, [keep weapons])`.
    // ponytail: the next level starts with its own loadout; carry the player's
    // weapons when `keep weapons` is set once levels hand state over.
    for name in ["missionsuccess", "changelevel"] {
        registry.register(Function, name, |world, _, args| {
            let level = string(args, 0)?;
            request(world, level, 0);
            Ok(Value::Undefined)
        });
    }
}
