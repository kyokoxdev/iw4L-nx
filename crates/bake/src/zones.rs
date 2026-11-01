//! B3: per zone, the material programs and exact colour pipelines the Switch
//! build can create, from the same load and compile path as a match.

use std::collections::HashSet;
use std::sync::Arc;

use asset_transport::{GamesRoot, LoadProgress, NamespaceTrees};
use master_protocol::ContentFlags;

/// One zone's bake set.
pub struct ZoneSet {
    pub zone: String,
    pub modules: usize,
    pub keys: Vec<render_gpu::ExactColourPipelineKey>,
}

/// Forces the Switch shader ABI; must run before anything reads it (the
/// `gpu_contract` switches latch on first read).
pub fn force_switch_abi() {
    // SAFETY: called first thing in main, before any other thread exists.
    unsafe {
        std::env::set_var("IW4L_BOUND_SLOTS", "1");
        std::env::set_var("IW4L_SHADER_SRGB", "1");
    }
}

pub fn families(games: &GamesRoot) -> Result<frame::AssetFamilies, String> {
    let trees = NamespaceTrees::discover(games);
    let have = [
        (asset_core::AssetNamespace::Iw4, ContentFlags::IW4),
        (asset_core::AssetNamespace::Iw5, ContentFlags::IW5),
        (asset_core::AssetNamespace::T5, ContentFlags::T5),
        (asset_core::AssetNamespace::T6, ContentFlags::T6),
    ]
    .into_iter()
    .filter(|(ns, _)| trees.get(*ns).is_some())
    .fold(0u8, |have, (_, flag)| have | flag.0);
    frame::AssetFamilies::from_inventory(have, &frame::UiMenuDvars::default())
}

// ponytail: one view signature, the lens default; take the Switch's from the
// device's learned keys (exact-demand-v2) once a copy is in.
const VIEW_SIG: [(bevy::render::render_resource::TextureFormat, u32); 1] =
    [render_gpu::LENS_VIEW_SIGNATURE];

pub fn bake_zone(
    games: &GamesRoot,
    families: frame::AssetFamilies,
    zone: &str,
) -> Result<ZoneSet, String> {
    let found = asset_transport::find_zone_file(games, zone)?;
    let common_mp =
        asset_transport::find_runtime_common_mp(games, &found.path).map(|zone| zone.path);
    let prepared = match bevy::tasks::futures_lite::future::block_on(assets::load_prepared_match(
        Ok(found.path),
        common_mp,
        LoadProgress::default(),
        families,
    )) {
        assets::MatchLoadOutcome::Ready(prepared) => prepared,
        assets::MatchLoadOutcome::Canceled => return Err("map walk canceled".into()),
        assets::MatchLoadOutcome::Refused(reason) => return Err(format!("refused: {reason}")),
    };
    let catalog = Arc::new(asset_material::compile_material_catalog(
        &prepared.materials.population,
    ));
    let (programs, table) = render_frontend::compile_generation_headless(&catalog)?;
    let ports: Vec<render_gpu::AdmittedExactPort> = programs
        .ports()
        .iter()
        .map(|port| render_gpu::AdmittedExactPort {
            id: port.id(),
            abi: port.abi().clone(),
            module: port.shared_module(),
            layout: port.wgpu_layout().clone(),
        })
        .collect();
    let (modules, keys) = render_gpu::bake_colour_pipeline_keys(&table, &ports, &VIEW_SIG);
    Ok(ZoneSet {
        zone: found.zone_name,
        modules,
        keys,
    })
}

/// Prints each zone's counts and the union over all of them.
pub fn run(games: &GamesRoot, zones: &[String]) -> Result<(), String> {
    let families = families(games)?;
    let mut union = HashSet::new();
    let mut failed = 0;
    for zone in zones {
        match bake_zone(games, families, zone) {
            Ok(set) => {
                println!(
                    "{}: {} modules, {} pipelines",
                    set.zone,
                    set.modules,
                    set.keys.len()
                );
                union.extend(set.keys);
            }
            Err(error) => {
                failed += 1;
                eprintln!("{zone}: {error}");
            }
        }
    }
    println!(
        "{} zones ({failed} failed), {} distinct pipelines",
        zones.len(),
        union.len()
    );
    Ok(())
}
