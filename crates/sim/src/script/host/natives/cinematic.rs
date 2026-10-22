//! IW4 singleplayer video builtins. The sim only records what plays and for
//! how long; `console::cinematic` shows it on the local client.
use bevy_ecs::prelude::World;
use std::collections::BTreeMap;

use crate::ScriptCinematic;
use crate::script::host::args::string;
use crate::script::{Namespace, NativeRegistry, RoundScript, Value};

fn now_ms(world: &World) -> i32 {
    crate::level_time_ms(world.resource::<crate::step::StepRequest>().tick)
}

// ponytail: arguments after the name are ignored; read them once the SP scripts show what they carry.
fn play(world: &mut World, args: &[Value], looped: bool) -> Result<Value, String> {
    let name = string(args, 0)?;
    let start_ms = now_ms(world);
    let fullscreen = world
        .resource::<crate::script::MatchScript>()
        .dvars
        .get("cg_cinematicfullscreen")
        .map_or(true, |v| v != "0");
    let mut runtime = world.resource_mut::<RoundScript>();
    runtime.engine.cinematic = Some(cinematic(
        &runtime.campaign.videos,
        &name,
        looped,
        fullscreen,
        start_ms,
    )?);
    Ok(Value::Undefined)
}

fn cinematic(
    videos: &BTreeMap<String, i32>,
    name: &str,
    looped: bool,
    fullscreen: bool,
    start_ms: i32,
) -> Result<ScriptCinematic, String> {
    let name = name.to_ascii_lowercase();
    let length_ms = *videos
        .get(&name)
        .ok_or_else(|| format!("video/{name}.bik not found"))?;
    Ok(ScriptCinematic {
        name,
        looped,
        fullscreen,
        start_ms,
        end_ms: (!looped).then(|| start_ms.saturating_add(length_ms)),
    })
}

pub(crate) fn register(registry: &mut NativeRegistry) {
    use Namespace::Function;
    registry.register(Function, "cinematicingame", |world, _, args| {
        play(world, args, false)
    });
    registry.register(Function, "cinematicingamesync", |world, _, args| {
        play(world, args, false)
    });
    registry.register(Function, "cinematicingameloop", |world, _, args| {
        play(world, args, true)
    });
    registry.register(Function, "cinematicingameloopresident", |world, _, args| {
        play(world, args, true)
    });
    registry.register(Function, "stopcinematicingame", |world, _, args| {
        if !args.is_empty() {
            return Err("stopcinematicingame expects no arguments".into());
        }
        world.resource_mut::<RoundScript>().engine.cinematic = None;
        Ok(Value::Undefined)
    });
    registry.register(Function, "iscinematicplaying", |world, _, args| {
        if !args.is_empty() {
            return Err("iscinematicplaying expects no arguments".into());
        }
        let now = now_ms(world);
        let playing = world
            .resource::<RoundScript>()
            .engine
            .cinematic
            .as_ref()
            .is_some_and(|cinematic| cinematic.playing(now));
        Ok(Value::Int(i32::from(playing)))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cinematic_plays_for_its_video_length_unless_looped() {
        let videos = BTreeMap::from([("briefing".to_owned(), 1500)]);
        let once = cinematic(&videos, "Briefing", false, true, 1000).unwrap();
        assert_eq!(once.name, "briefing");
        assert!(once.fullscreen);
        assert!(once.playing(2499));
        assert!(!once.playing(2500));
        let looped = cinematic(&videos, "briefing", true, false, 1000).unwrap();
        assert!(!looped.fullscreen);
        assert!(looped.playing(i32::MAX));
        assert!(cinematic(&videos, "missing", false, true, 0).is_err());
    }
}
