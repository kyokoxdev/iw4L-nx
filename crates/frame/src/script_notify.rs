use bevy::prelude::*;

#[derive(Message, Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExitLevelCalled;

/// A campaign level asked for the next level (or its own restart); empty = menu.
#[derive(Message, Clone, Debug, PartialEq, Eq)]
pub struct CampaignLevelCalled(pub String);

pub fn register_script_notify(app: &mut App) {
    app.add_message::<ExitLevelCalled>();
    app.add_message::<CampaignLevelCalled>();
}
