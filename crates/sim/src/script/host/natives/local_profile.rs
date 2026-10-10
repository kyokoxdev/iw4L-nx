use super::super::args::{arg, kind, string};
use super::player::player;
use crate::LocalPlayerProfile;
use crate::script::{Namespace::Method, NativeRegistry, Runtime, Value};

/// Campaign profile fields, lowercase, with the value a fresh profile reads.
const SP_FIELDS: &[(&str, i32)] = &[
    ("haseverplayed_sp", 0),
    ("takecoverwarnings", -1),
    ("highestmission", 0),
    ("autoaim", 0),
    ("invertedpitch", 0),
    ("cheatpoints", 0),
    ("missionhighestdifficulty", 0),
];

fn sp_default(name: &str) -> Option<Value> {
    SP_FIELDS
        .iter()
        .find(|(field, _)| name.eq_ignore_ascii_case(field))
        .map(|(_, value)| Value::Int(*value))
}

pub(crate) fn register(registry: &mut NativeRegistry) {
    registry.register(
        Method,
        "getlocalplayerprofiledata",
        |world, receiver, args| {
            if player(world, receiver)? != 0 {
                return Ok(Value::Undefined);
            }
            let name = string(args, 0)?;
            if let Some(value) = world.resource::<LocalPlayerProfile>().get(&name) {
                return Ok(Value::Int(i32::from(value)));
            }
            let fresh = sp_default(&name)
                .ok_or_else(|| format!("unknown local player profile field {name}"))?;
            let stored = world
                .resource::<Runtime>()
                .sp_profile
                .get(&name.to_ascii_lowercase())
                .cloned();
            Ok(stored.unwrap_or(fresh))
        },
    );
    registry.register(
        Method,
        "setlocalplayerprofiledata",
        |world, receiver, args| {
            if player(world, receiver)? != 0 {
                return Ok(Value::Undefined);
            }
            if args.len() < 2 {
                return Err("setlocalplayerprofiledata expects at least two arguments".into());
            }
            let name = string(args, 0)?;
            if world.resource::<LocalPlayerProfile>().get(&name).is_none() {
                if sp_default(&name).is_none() {
                    return Err(format!("unknown local player profile field {name}"));
                }
                let value = arg(args, 1)?.clone();
                world
                    .resource_mut::<Runtime>()
                    .sp_profile
                    .insert(name.to_ascii_lowercase(), value);
                return Ok(Value::Undefined);
            }
            let value = match arg(args, 1)? {
                Value::Int(value) => *value as u8,
                other => return Err(format!("parameter 2 is {}, not an int", kind(other))),
            };
            world.resource_mut::<LocalPlayerProfile>().set(&name, value);
            Ok(Value::Undefined)
        },
    );
}
