//! B3: per zone, the material programs and exact colour pipelines the Switch
//! build can create, from the same load and compile path as a match.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use asset_transport::{GamesRoot, LoadProgress, NamespaceTrees};
use master_protocol::ContentFlags;

/// One zone's bake set.
pub struct ZoneSet {
    pub zone: String,
    pub modules: usize,
    pub keys: Vec<render_gpu::ExactColourPipelineKey>,
    pub stages: crate::record::Stages,
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
    device: &bevy::render::renderer::RenderDevice,
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
    render_gpu::bake_build_colour_pipelines(device, &ports, &keys);
    let mut stages = crate::record::Stages::default();
    stages.drain();
    Ok(ZoneSet {
        zone: found.zone_name,
        modules,
        keys,
        stages,
    })
}

/// Prints each zone's counts and the union over all of them; with `out`,
/// compiles each zone's GLSL and writes `<out>/switch/iw4l/shaders/dksh/<zone>.dkpk`.
pub fn run(
    games: &GamesRoot,
    zones: &[String],
    out: Option<&Path>,
    dump: Option<u64>,
) -> Result<(), String> {
    let started = std::time::Instant::now();
    let families = families(games)?;
    let device = crate::record::noop_device()?;
    let mut union = HashSet::new();
    let mut glsl = HashSet::new();
    // Identical GLSL across zones compiles once.
    let mut compiled: HashMap<u64, Result<Vec<u8>, i32>> = HashMap::new();
    let (mut compile_failed, mut pack_bytes) = (0, 0);
    let mut failed = 0;
    for zone in zones {
        match bake_zone(&device, games, families, zone) {
            Ok(set) => {
                println!(
                    "{}: {} modules, {} pipelines, recorded {} ({} failed), {} distinct GLSL",
                    set.zone,
                    set.modules,
                    set.keys.len(),
                    set.stages.pipelines,
                    set.stages.failed.len(),
                    set.stages.glsl.len()
                );
                for (label, error) in set.stages.failed.iter().take(5) {
                    eprintln!("  {}: {error}", label.as_deref().unwrap_or("?"));
                }
                if let Some(out) = out {
                    let jobs: Vec<_> = set
                        .stages
                        .glsl
                        .iter()
                        .filter(|(key, _)| !compiled.contains_key(key))
                        .map(|(&key, (stage, text))| (key, *stage, text.as_str()))
                        .collect();
                    for (key, result) in crate::uam::compile_many(&jobs) {
                        if let Err(rc) = result {
                            compile_failed += 1;
                            eprintln!("  uam rc {rc} for {key:016x}");
                        }
                        compiled.insert(key, result);
                    }
                    if let Some((key, (_, text))) =
                        dump.and_then(|key| set.stages.glsl.get_key_value(&key))
                    {
                        let dir = out.join("dump");
                        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
                        let _ = std::fs::write(dir.join(format!("{key:016x}.glsl")), text);
                        if let Some(Ok(dksh)) = compiled.get(key) {
                            let _ = std::fs::write(dir.join(format!("{key:016x}.dksh")), dksh);
                        }
                    }
                    let mut entries = BTreeMap::new();
                    for (&key, (stage, text)) in &set.stages.glsl {
                        if let Some(Ok(dksh)) = compiled.get(&key) {
                            entries.insert(key, crate::pack::Entry {
                                glsl_len: text.len() as u32,
                                stage: *stage as u8,
                                dksh: dksh.clone(),
                            });
                        }
                    }
                    let bytes = crate::pack::encode(&entries);
                    pack_bytes += bytes.len();
                    let path = out.join(format!("switch/iw4l/shaders/dksh/{}.dkpk", set.zone));
                    crate::pack::write(&path, &bytes)?;
                }
                union.extend(set.keys);
                glsl.extend(set.stages.glsl.into_keys());
            }
            Err(error) => {
                failed += 1;
                eprintln!("{zone}: {error}");
            }
        }
    }
    println!(
        "{} zones ({failed} failed), {} distinct pipelines, {} distinct GLSL",
        zones.len(),
        union.len(),
        glsl.len()
    );
    if out.is_some() {
        println!(
            "compiled {} ({compile_failed} failed), packs {:.1} MiB",
            compiled.len(),
            pack_bytes as f64 / (1 << 20) as f64
        );
    }
    println!("{:.1} s", started.elapsed().as_secs_f64());
    Ok(())
}
