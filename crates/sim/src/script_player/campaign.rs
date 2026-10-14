//! Campaign stance permissions on the player's command. Fork-only; one hook in
//! `script_player.rs`.
use playerstate_iw4::buttons;

pub(crate) const STAND: u8 = 1;
pub(crate) const CROUCH: u8 = 2;
pub(crate) const PRONE: u8 = 4;
pub(crate) const FIRE: u8 = 8;
pub(crate) const MELEE: u8 = 16;

/// Strips the stance buttons of refused stances; with standing refused, holds
/// the lowest stance still allowed.
// ponytail: the server overrides the held stance bits; the client's own stance
// toggle can disagree until it presses again. Mirror into the client if seen.
pub(super) fn deny_stances(denied: u8, held: &mut u32) {
    if denied & FIRE != 0 {
        *held &= !buttons::ATTACK;
    }
    if denied & MELEE != 0 {
        *held &= !buttons::MELEE_CHARGE;
    }
    if denied & PRONE != 0 {
        *held &= !buttons::PRONE;
    }
    if denied & CROUCH != 0 {
        *held &= !buttons::CROUCH;
    }
    if denied & STAND != 0 && *held & (buttons::PRONE | buttons::CROUCH) == 0 {
        *held |= if denied & CROUCH == 0 {
            buttons::CROUCH
        } else if denied & PRONE == 0 {
            buttons::PRONE
        } else {
            0
        };
    }
}
