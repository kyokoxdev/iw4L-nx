//! Campaign console commands (`notifyoncommand`, trainer key hints) that MP
//! scripts never bind. Fork-only; one hook in `controls.rs`.
use playerstate_iw4::buttons;

use crate::script::Namespace::{Function, Method};
use crate::script::host::args::{byte_string, float, optional, string};
use crate::script::host::natives::player::{link_to, player};
use crate::script::host::players::LinkView;
use crate::script::{Arc, NativeRegistry, RoundScript, Value};

/// Buttons a campaign command presses; 0 when it is not a button.
pub(super) fn command_buttons(command: &str) -> u32 {
    match command {
        "+speed" | "toggleads" => buttons::ADS,
        "+reload" => buttons::RELOAD,
        "+sprint" => buttons::SPRINT,
        "+holdbreath" => buttons::BREATH,
        "+breath_sprint" => buttons::BREATH | buttons::SPRINT,
        "+melee_breath" => buttons::MELEE_CHARGE | buttons::BREATH,
        "+moveup" => buttons::JUMP,
        "+movedown" | "gocrouch" | "togglecrouch" => buttons::CROUCH,
        "+prone" | "goprone" | "toggleprone" | "lowerstance" => buttons::PRONE,
        "+stance" => buttons::CROUCH | buttons::PRONE | buttons::STANCE_HELD,
        "+throw" => buttons::THROW,
        _ => 0,
    }
}

/// A linked player's (origin, view) part way through its `playerlinktoblend`.
pub(crate) fn blend_link(
    world: &mut bevy_ecs::prelude::World,
    client: u32,
    origin: [f32; 3],
    view: [f32; 3],
) -> ([f32; 3], [f32; 3]) {
    let now = crate::script::host::players::now_ms(world);
    let blends = &mut world.resource_mut::<RoundScript>().campaign.link_blends;
    let Some(&(from, from_view, start, ms)) = blends.get(&client) else {
        return (origin, view);
    };
    if now >= start + ms {
        blends.remove(&client);
        return (origin, view);
    }
    let t = ((now - start) as f32 / ms as f32).clamp(0.0, 1.0);
    let t = t * t * (3.0 - 2.0 * t);
    let turn = |a: f32, b: f32| ((b - a + 540.0) % 360.0) - 180.0;
    (
        std::array::from_fn(|k| from[k] + (origin[k] - from[k]) * t),
        std::array::from_fn(|k| from_view[k] + turn(from_view[k], view[k]) * t),
    )
}

/// `notifyoncommand(notify, command)`: every player is notified `notify` when
/// it presses `command`, like `notifyonplayercommand` on each of them.
pub(crate) fn register(registry: &mut NativeRegistry) {
    // `playerlinktoblend(ent, tag, blendtime, ...)`: the player eases from
    // where it stands onto the tag over `blendtime` (smoothstep; the accel and
    // decel times are not read).
    registry.register(Method, "playerlinktoblend", |world, receiver, args| {
        let client = player(world, receiver)?;
        let ms = optional(args, 2, float)?.map_or(0, |s| (s.max(0.0) * 1000.0) as i64);
        let start = crate::frame::FrameWorld::from_world(world)
            .player(crate::world::ClientId(client))
            .map(|ps| (ps.origin, ps.viewangles));
        let result = link_to(world, receiver, &args[..args.len().min(2)], LinkView::Absolute)?;
        let now = crate::script::host::players::now_ms(world);
        let blends = &mut world.resource_mut::<RoundScript>().campaign.link_blends;
        match start {
            Some((origin, view)) if ms > 0 => {
                blends.insert(client, (origin, view, now, ms));
            }
            _ => {
                blends.remove(&client);
            }
        }
        Ok(result)
    });
    // `lerpviewangleclamp(time, accel, decel, right, left, top, bottom)`:
    // the linked view's limits change at once.
    registry.register(Method, "lerpviewangleclamp", |world, receiver, args| {
        let client = player(world, receiver)?;
        let arc = |index| -> Result<f32, String> { Ok(float(args, index)?.clamp(0.0, 180.0)) };
        let clamp = [-arc(5)?, arc(6)?, -arc(3)?, arc(4)?];
        if let Some(link) = world
            .resource_mut::<RoundScript>()
            .players
            .get_mut(&client)
            .and_then(|slot| slot.link.as_mut())
        {
            link.clamp = Some(clamp);
        }
        Ok(Value::Undefined)
    });
    // (forward, right, 0) of the movement stick, each in -1..1.
    registry.register(Method, "getnormalizedmovement", |world, receiver, _| {
        let client = player(world, receiver)?;
        let input = &world.resource::<crate::step::StepRequest>().input;
        let (forward, right) = input
            .cmds
            .iter()
            .rev()
            .find(|command| command.client.0 == client)
            .map_or((0.0, 0.0), |command| {
                (
                    f32::from(command.command.forwardmove) / 127.0,
                    f32::from(command.command.rightmove) / 127.0,
                )
            });
        Ok(Value::Vector([forward, right, 0.0]))
    });
    registry.register(Function, "notifyoncommand", |world, _, args| {
        let notify: Arc<str> = byte_string(args, 0)?.symbol_key();
        let command: Arc<str> = string(args, 1)?.to_ascii_lowercase().into();
        let mut runtime = world.resource_mut::<RoundScript>();
        for slot in runtime.players.values_mut() {
            if !slot
                .commands
                .iter()
                .any(|(c, n)| *c == command && *n == notify)
            {
                slot.commands.push((command.clone(), notify.clone()));
            }
        }
        Ok(Value::Undefined)
    });
}
