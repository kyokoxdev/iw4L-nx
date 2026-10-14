//! Campaign damage gates: `enableinvulnerability` and the player's
//! `enabledeathshield`. Fork-only; hooks in `entity_damage.rs`.
use bevy_ecs::prelude::World;

use super::ScriptHit;
use crate::frame::FrameWorld;
use crate::script::Namespace::Method;
use crate::script::host::args::arg;
use crate::script::host::natives::engine::entity_id;
use crate::script::{NativeRegistry, RoundScript, Value};
use crate::world::ClientId;

pub(super) fn invulnerable(world: &World, object: u64) -> bool {
    world
        .resource::<RoundScript>()
        .campaign
        .invulnerable
        .contains(&object)
}

/// Whether the hit on the player lands; a death shield keeps it at 1 health.
pub(super) fn shield_player(world: &mut World, client: ClientId, hit: &mut ScriptHit) -> bool {
    let runtime = world.resource::<RoundScript>();
    let Some(object) = runtime.players.get(&client.0).map(|slot| slot.object) else {
        return true;
    };
    // `kill()` lands whatever shields the player has.
    if hit.means == "MOD_SUICIDE" {
        return true;
    }
    if runtime.campaign.invulnerable.contains(&object) {
        return false;
    }
    if runtime.campaign.death_shield.contains(&object) {
        let health = FrameWorld::from_world(world)
            .player(client)
            .map_or(0, |ps| ps.health);
        if health > 1 {
            hit.amount = hit.amount.min(health - 1);
        }
    }
    true
}

fn set(world: &mut World, receiver: &Value, on: bool, shield: bool) -> Result<Value, String> {
    let id = entity_id(world, receiver)?;
    let mut runtime = world.resource_mut::<RoundScript>();
    let set = if shield {
        &mut runtime.campaign.death_shield
    } else {
        &mut runtime.campaign.invulnerable
    };
    if on {
        set.insert(id);
    } else {
        set.remove(&id);
    }
    Ok(Value::Undefined)
}

pub(crate) fn register(registry: &mut NativeRegistry) {
    registry.register(Method, "enableinvulnerability", |world, receiver, _| {
        set(world, receiver, true, false)
    });
    registry.register(Method, "disableinvulnerability", |world, receiver, _| {
        set(world, receiver, false, false)
    });
    registry.register(Method, "enabledeathshield", |world, receiver, args| {
        let on = !matches!(arg(args, 0)?, Value::Int(0) | Value::Undefined);
        set(world, receiver, on, true)
    });
}
