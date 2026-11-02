mod artifact_cache;
mod asset_graph;
mod gltf_export;
mod iwd;
mod lane;
mod lane_capability;
pub mod loading_screen;
mod map_load_process;
mod map_scripts;
mod match_load;
pub mod plugin;
pub mod prepared;
pub mod session_load;

pub use artifact_cache::{cache_flight, cache_get, cache_put, fnv1a64, fnv1a64_more};
pub(crate) use asset_graph::stamp_match_destructible_death;
pub use asset_graph::{
    AssetEdge, AssetEdgeCensus, AssetEdgeReason, AssetGraphCensus, DESTRUCTIBLE_DEATH_HINTS,
    DeathClipEdge, DeathHuskEdge, DestructibleDeathHint, DestructibleDeathRow, FpvMeshIndex,
    FpvMeshSpace, FxIndex, FxModelIndex, FxModelSpace, FxSpace, LoadedSoundIndex, LoadedSoundSpace,
    MapXModelIndex, MapXModelSpace, MaterialIndex, MaterialSpace, ProjectileModelIndex,
    ProjectileModelSpace, SoundAliasIndex, SoundAliasSpace, TechniqueSetIndex, TechniqueSetSpace,
    TracerIndex, TracerSpace, WorldWeaponIndex, WorldWeaponSpace, XAnimIndex, XAnimSpace,
    resolve_after_absorb,
};
pub use fastfile_iw4::GlyphCapture;
pub use gltf_export::{GltfExportSummary, export_prepared_world_gltf};
pub use iwd::{IwdSoundIndex, NamespaceSoundIwd, NamespaceTree, NamespaceTrees};
pub use lane::{CommonCensus, FilmVisionCatalog, LANE_GAPS, LaneGap, LoadedWorld, ZoneLane, lane};
pub use lane_capability::{LaneStatus, PreparedCapability, lane_status};
pub use lighting_iw4::MODEL_LIGHTING_TILE_BYTES;
pub use loading_screen::{LoadingPreviewSource, LoadingScreen};
pub use map_load_process::MapLoadProcess;
pub use match_load::{
    CurrentMatchLoad, MapLoadApproval, MatchLoadAbort, MatchLoadAccepted, MatchLoadBusy,
    MatchLoadDispatch, MatchLoadRequest, PreparedMatchReady, PreparedMatchSound,
};
pub use plugin::AssetPlugin;
pub use prepared::{
    BoundWeapons, MapFacts, MatchMaterials, MatchType10SoundHints, PreparedBodies,
    PreparedBodyClips, PreparedDestructibleDeath, PreparedFpvMeshes, PreparedGaps,
    PreparedLocalizedStrings, PreparedMap, PreparedProjectileMeshes, PreparedWeapons,
    PreparedWorldWeapons, PreparedXAnims, PreparedXModelWalkCensus, SessionCompass,
};
pub use session_load::{
    MatchLoadOutcome, MatchMaterialSeed, PreparedMatch, PreparedWorld, ShellCommon,
    apply_match_material_map, clear_resident_map, forget_common, load_match_material_catalog,
    load_match_material_seed, load_pool, load_prepared_match, load_shell_common, load_workers,
    publish_process_cpus,
};

pub mod image_handles;

mod script_sources;
pub use script_sources::{ScriptSourceOrigin, ScriptSources, ScriptTable};

/// Installs a baked shader pack from the SD card for the deko3d backend
/// (`docs/deko3d_implementation_plan.md`): `common` once at boot, the map's
/// zone under one slot that each load replaces. No-op off Switch and on GL.
pub fn install_shader_pack(name: &str, slot: &str) {
    #[cfg(target_os = "horizon")]
    {
        if std::env::var("IW4L_BACKEND").is_ok_and(|v| v == "gl") {
            return;
        }
        let path = format!("sdmc:/switch/iw4l/shaders/dksh/{name}.dkpk");
        let result = std::fs::read(&path)
            .map_err(|e| e.to_string())
            .and_then(|bytes| wgpu_hal::deko3d_shader::packs::install(slot, bytes));
        match result {
            Ok(count) => diag::info!(World, "shader pack {name}: {count} shaders"),
            Err(error) => {
                wgpu_hal::deko3d_shader::packs::remove(slot);
                diag::warn!(World, "shader pack {path}: {error}");
            }
        }
    }
    #[cfg(not(target_os = "horizon"))]
    let _ = (name, slot);
}
