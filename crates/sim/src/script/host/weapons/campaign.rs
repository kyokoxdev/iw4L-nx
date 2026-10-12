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
