use crate::{CommandSpec, ConsoleCommand, ConsoleRegistry};
use bevy::prelude::*;
use frame::UiMenuDvars;
use ui::frontend::maps::map_label;

/// Rows in `iw4l_campaign` (`ui_sp_0`..). Retail ships 19 levels.
// ponytail: fixed rows, no paging; add pages if an install lists more levels.
const ROWS: usize = 20;

/// Story order; levels not listed sort after, by name.
const MISSION_ORDER: &[&str] = &[
    "trainer",
    "roadkill",
    "cliffhanger",
    "airport",
    "favela",
    "invasion",
    "favela_escape",
    "arcadia",
    "oilrig",
    "gulag",
    "dcburning",
    "contingency",
    "dcemp",
    "dc_whitehouse",
    "estate",
    "boneyard",
    "af_caves",
    "af_chase",
    "ending",
];

pub(crate) fn register(registry: &mut ConsoleRegistry) {
    for name in ["ui_campaign", "ui_campaign_start"] {
        registry.register(CommandSpec::new(name));
    }
}

/// Campaign levels of the install, Special Ops excluded, in story order.
fn campaign_levels(root: &asset_transport::GamesRoot) -> Vec<String> {
    let mut levels: Vec<String> = asset_transport::list_sp_maps(root)
        .into_iter()
        .filter(|stem| !stem.starts_with("so_"))
        .collect();
    sort_story(&mut levels);
    levels
}

fn sort_story(levels: &mut [String]) {
    levels.sort_by_key(|stem| {
        (
            MISSION_ORDER
                .iter()
                .position(|known| known == stem)
                .unwrap_or(MISSION_ORDER.len()),
            stem.clone(),
        )
    });
}

pub(crate) fn route(
    mut commands: MessageReader<ConsoleCommand>,
    identity: Option<Res<frame::LaunchIdentity>>,
    mut transition: ResMut<session::SessionSwapRequest>,
    mut dvars: ResMut<UiMenuDvars>,
    mut levels: Local<Vec<String>>,
    mut echo: crate::feature_dispatch::ConsoleEcho,
) {
    for command in commands.read() {
        match command.name.as_str() {
            "ui_campaign" => {
                *levels = identity
                    .as_ref()
                    .map(|identity| {
                        campaign_levels(&asset_transport::GamesRoot(identity.games_root.clone()))
                    })
                    .unwrap_or_default();
                for row in 0..ROWS {
                    dvars.set(
                        &format!("ui_sp_{row}"),
                        levels
                            .get(row)
                            .map(|level| map_label(level))
                            .unwrap_or_default(),
                    );
                }
                dvars.set(
                    "ui_sp_status",
                    if levels.is_empty() {
                        "No campaign levels installed"
                    } else {
                        ""
                    },
                );
            }
            "ui_campaign_start" => {
                let Some(level) = command
                    .args
                    .first()
                    .and_then(|arg| arg.parse::<usize>().ok())
                    .and_then(|row| levels.get(row))
                else {
                    echo.write("menu: Level is unavailable");
                    continue;
                };
                match transition.request_zone(level.clone()) {
                    Ok(id) => echo.write(format!("menu: starting campaign {level} (swap #{id})")),
                    Err(error) => echo.write(format!("menu: {error}")),
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn story_order_then_unknown_by_name() {
        let mut levels = ["zz_mod", "boneyard", "aa_mod", "trainer"].map(String::from);
        super::sort_story(&mut levels);
        assert_eq!(levels, ["trainer", "boneyard", "aa_mod", "zz_mod"]);
    }
}
