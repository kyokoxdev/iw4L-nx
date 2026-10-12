//! SP traverse nodes: an actor whose route crosses a negotiation begin node plays
//! that node's `animscript` (`animscripts/traverse/<name>::main`) and ends at the
//! end node. Without such an animscript the link is walked like any other.
//! Fork-only; hooks in `actors.rs`.

use bevy_ecs::prelude::World;

use super::{ActorBrain, actor, brain, has_entry};
use crate::script::{RoundScript, Value};

/// Marks the actor as crossing the traverse `(start, end)` (node indices) the motor reached.
pub(super) fn begin_traverse(runtime: &mut RoundScript, id: u64, (start, end): (usize, usize)) {
    let (Some(&start), Some(&end)) = (
        runtime.campaign.nodes.get(start),
        runtime.campaign.nodes.get(end),
    ) else {
        return;
    };
    runtime.campaign.actor_brains.entry(id).or_default().traverse = Some((start, end));
}

/// `traverse/<animscript>` while the actor crosses a traverse. A traverse whose
/// animscript is missing is dropped, so its link is walked.
pub(super) fn traverse_script(world: &mut World, brain: &mut ActorBrain) -> Option<String> {
    let (start, _) = brain.traverse?;
    let anim = match world
        .resource_mut::<RoundScript>()
        .object_field(start, "animscript")
    {
        Value::String(anim) => anim.to_string(),
        _ => String::new(),
    };
    let script = format!("traverse/{anim}");
    if !anim.is_empty() && has_entry(world, &format!("animscripts/{script}::main")) {
        return Some(script);
    }
    brain.traverse = None;
    None
}

/// Ends the traverse once its animscript returns: the actor stands at the end node.
pub(super) fn finish_traverse(world: &mut World, id: u64, brain: &mut ActorBrain, thread_done: bool) {
    let Some((_, end)) = brain.traverse else {
        return;
    };
    if !thread_done || !brain.script.starts_with("traverse/") {
        return;
    }
    let mut runtime = world.resource_mut::<RoundScript>();
    if let Value::Vector(at) = runtime.object_field(end, "origin") {
        runtime.set_object_field(id, "origin", Value::Vector(at));
    }
    brain.traverse = None;
}

/// `getnegotiationstartnode` / `getnegotiationendnode`: the traverse being crossed, else undefined.
pub(super) fn negotiation_node(world: &mut World, receiver: &Value, start: bool) -> Result<Value, String> {
    let id = actor(world, receiver)?;
    Ok(match brain(world, id).traverse {
        Some((s, e)) => Value::Object(if start { s } else { e }),
        None => Value::Undefined,
    })
}
