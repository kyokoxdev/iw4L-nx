//! Campaign turrets: level `misc_turret`s, and the fire event turrets, actors
//! and vehicles raise when they shoot. Fork-only; kept out of the parent file
//! so upstream merges touch it as little as possible.

use super::super::entities::EntityKind;
use super::Turret;
use crate::frame::FrameWorld;
use crate::script::{RoundScript, Value};
use bevy_ecs::prelude::World;

/// A campaign level's `misc_turret` entities are turrets of their `weaponinfo`.
pub(crate) fn init_map_turrets(world: &mut World) {
    let turrets: Vec<(u64, String, [f32; 3])> = {
        let mut runtime = world.resource_mut::<RoundScript>();
        let ids: Vec<u64> = runtime
            .entities
            .iter()
            .filter(|(_, e)| e.kind == EntityKind::Map && &*e.classname == "misc_turret")
            .map(|(id, _)| *id)
            .collect();
        ids.into_iter()
            .filter_map(|id| {
                let Value::String(info) = runtime.object_field(id, "weaponinfo") else {
                    return None;
                };
                let origin = match runtime.object_field(id, "origin") {
                    Value::Vector(v) => v,
                    _ => [0.0; 3],
                };
                Some((id, info.to_string(), origin))
            })
            .collect()
    };
    for (id, info, origin) in turrets {
        let weapon = match crate::script_player::weapon_named(&FrameWorld::from_world(world), &info)
        {
            Ok(weapon) => weapon,
            Err(message) => {
                diag::warn!(Sim, "gsc: level turret: {message}");
                continue;
            }
        };
        let Ok(presence) = super::super::presence::spawn_presence(world, origin) else {
            continue;
        };
        let mut runtime = world.resource_mut::<RoundScript>();
        let entity = runtime.entities.get_mut(&id).unwrap();
        entity.kind = EntityKind::Spawned;
        entity.presence = Some(presence);
        runtime.new_models.push(id);
        // Fields as `spawnturret` sets them.
        runtime.engine.turrets.insert(
            id,
            Turret {
                weapon,
                mode: "manual".into(),
                owner: None,
                team: None,
                carried: false,
                fire_enabled: true,
                manual: None,
                target: None,
                firing: false,
                on_target: false,
                aim: [1.0, 0.0, 0.0],
                aim_initialized: false,
                scan_direction: 1.0,
                arcs: [180.0, 180.0, 90.0, 90.0],
                drop_pitch: 0.0,
                convergence: [0.0; 2],
                minimap: false,
                solid: false,
                minigun: false,
                mode_change_wait: 0.0,
            },
        );
    }
}

/// `EV_FIRE_WEAPON` on the shooting entity: clients play the fire sound at the
/// muzzle and the flash on its `tag_flash` bone, carried in `event_parm`.
pub(super) fn fire_event(world: &mut World, object: u64, from: [f32; 3], weapon: u32) {
    let Some(presence) = world
        .resource::<RoundScript>()
        .entities
        .get(&object)
        .and_then(|e| e.presence)
    else {
        return;
    };
    let frame = super::super::presence::settled(world);
    let Some(number) = frame.gentity_number(presence) else {
        return;
    };
    let bone = frame
        .entity_collision_capabilities()
        .iter()
        .find(|row| row.owner.script_model() == Some(presence))
        .and_then(|row| row.dobj.as_ref()?.tag_bone("tag_flash"))
        .and_then(|bone| i32::try_from(bone).ok())
        .unwrap_or(-1);
    let tick = world.resource::<crate::step::StepRequest>().tick;
    FrameWorld::from_world(world).push_entity_event(
        tick,
        crate::EventAudience::All,
        entity_iw4::EntityEventKind::FIRE_WEAPON,
        crate::EntityEventPayload {
            number,
            weapon,
            event_parm: bone,
            origin: from,
            ..Default::default()
        },
    );
}
