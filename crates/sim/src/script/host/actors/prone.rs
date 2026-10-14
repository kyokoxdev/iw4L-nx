//! SP actor prone: `setproneanimnodes` names the legs-down, aim and
//! legs-up nodes and weights them in, `enterprone`/`exitprone` blend them in or
//! out over the given seconds, and `updateprone` weights the feet up/down
//! anims while prone. Fork-only; registered from `actors.rs`.

use bevy_ecs::prelude::World;

use super::super::args::{arg, float};
use super::super::script_anim::set_weight;
use super::{actor, brain, store};
use crate::script::{Namespace, NativeRegistry, Value};

// ponytail: nodes blend in flat; pitch the body to the ground
// slope and split legs-up/legs-down weight by it. Add when prone on slopes
// looks wrong.
fn blend_nodes(world: &mut World, receiver: &Value, weight: f32, blend: f32) -> Result<(), String> {
    let id = actor(world, receiver)?;
    let nodes = brain(world, id)
        .prone_nodes
        .ok_or("Must call SetProneAnimNodes before calling EnterProne")?;
    for node in &nodes {
        set_weight(world, receiver, node, weight, blend, 1.0)?;
    }
    Ok(())
}

fn set_prone(world: &mut World, receiver: &Value, prone: bool) -> Result<(), String> {
    let id = actor(world, receiver)?;
    let mut state = brain(world, id);
    state.prone = prone;
    store(world, id, state);
    Ok(())
}

pub(super) fn register(registry: &mut NativeRegistry) {
    use Namespace::Method;
    // `setproneanimnodes(down_angle, up_angle, legs_down, aim, legs_up)`.
    registry.register(Method, "setproneanimnodes", |world, receiver, args| {
        let id = actor(world, receiver)?;
        if float(args, 0)? >= 0.0 {
            return Err("Down angle (parameter 1) must be set to be less than 0.".into());
        }
        if float(args, 1)? <= 0.0 {
            return Err("Up angle (parameter 2) must be set to be greater than 0.".into());
        }
        let nodes = [
            arg(args, 2)?.clone(),
            arg(args, 3)?.clone(),
            arg(args, 4)?.clone(),
        ];
        let mut state = brain(world, id);
        state.prone_nodes = Some(nodes.clone());
        store(world, id, state);
        for node in &nodes {
            set_weight(world, receiver, node, 1.0, 0.0, 1.0)?;
        }
        Ok(Value::Undefined)
    });
    // `enterprone(seconds, on_back)`.
    registry.register(Method, "enterprone", |world, receiver, args| {
        blend_nodes(world, receiver, 1.0, float(args, 0)?)?;
        set_prone(world, receiver, true)?;
        Ok(Value::Undefined)
    });
    // `exitprone(seconds)`.
    registry.register(Method, "exitprone", |world, receiver, args| {
        set_prone(world, receiver, false)?;
        if brain(world, actor(world, receiver)?).prone_nodes.is_some() {
            blend_nodes(world, receiver, 0.0, float(args, 0)?)?;
        }
        Ok(Value::Undefined)
    });
    // `updateprone(up_anim, down_anim, weight, blend, rate)`: only while prone.
    registry.register(Method, "updateprone", |world, receiver, args| {
        if !brain(world, actor(world, receiver)?).prone {
            return Ok(Value::Undefined);
        }
        let (weight, blend, rate) = (float(args, 2)?, float(args, 3)?, float(args, 4)?);
        for anim in [arg(args, 0)?.clone(), arg(args, 1)?.clone()] {
            set_weight(world, receiver, &anim, weight, blend, rate)?;
        }
        Ok(Value::Undefined)
    });
}
