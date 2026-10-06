use crate::WorldPipelineWarmup;

use super::*;
use bevy::platform::collections::HashSet;

pub fn cached_lighting_port_variant(
    port: &crate::AdmittedExactPort,
    packed_buffers: &[VertexBufferLayout],
) -> (Option<String>, Vec<VertexBufferLayout>) {
    if port.abi().vertex_type != asset_iw4::vertex_decl::PACKED_VERTEX_TYPE {
        return (None, Vec::new());
    }
    let Some(location) = cached_lighting_location(port.abi()) else {
        return (None, Vec::new());
    };
    let buffers = cached_lighting_vertex_buffers(packed_buffers, location);
    let source = lighting_texcoord_register(port.abi()).and_then(|register| {
        inject_cached_lighting_attribute(&port.module().source, location, Some(&register))
    });
    (source, buffers)
}

fn adapt_admitted_port(port: &crate::AdmittedExactPort) -> ExactColourPortGpu {
    let (constants, textures) = split_bind_layout(port.wgpu_layout());
    let vertex_buffers = crate::vertex_layouts_from_contract(port.wgpu_layout());
    let (cached_source, cached_vertex_buffers) =
        cached_lighting_port_variant(port, &vertex_buffers);

    let cached_source = cached_source
        .filter(|_| !cached_vertex_buffers.is_empty())
        .map(Arc::<str>::from);
    ExactColourPortGpu {
        constants_layout: crate::bind_group_layout_from_entries(
            "iw4_exact_colour_constants",
            &constants,
            true,
        ),
        textures_layout: crate::bind_group_layout_from_entries(
            "iw4_exact_colour_textures",
            &textures,
            true,
        ),
        vertex_buffers,
        cached_source,
        cached_vertex_buffers,
        port: port.clone(),
    }
}

pub(super) fn init_or_update_pipeline(
    world: Option<Res<InstalledRenderWorld>>,
    mut pipeline: ResMut<ExactColourPipeline>,
) {
    let Some(world) = world else {
        return;
    };
    if world.ports.is_empty() {
        if !pipeline.ports.is_empty() {
            pipeline.ports.clear();
            pipeline.by_id.clear();
        }
        return;
    }
    if pipeline.generation == world.generation && pipeline.ports.len() == world.ports.len() {
        return;
    }
    pipeline.ports = world.ports.iter().map(adapt_admitted_port).collect();
    pipeline.generation = world.generation;
    pipeline.rebuild_index();
}

pub(super) fn kick_extracted_colour_pipelines(
    world: Option<Res<InstalledRenderWorld>>,
    frame: Option<Res<PublishedRenderFrame>>,
    views: Query<(&ViewTarget, Option<&Msaa>), (With<Camera3d>, With<ViewUpscalingPipeline>)>,
    device: Res<RenderDevice>,
    mut registry: ResMut<ExactPipelineRegistry>,
    pipeline: Res<ExactColourPipeline>,
    mut kicked: ResMut<ExactPipelineKickCache>,
    mut warmup: ResMut<WorldPipelineWarmup>,
) {
    registry.poll();

    let mut learned_new = false;
    for (key, state) in registry.take_discovered() {
        if pipeline.get(key.port).is_some() {
            request_exact_pipeline(&mut registry, &pipeline, &device, key);
            learned_new |= learned::remember(&key, state);
        }
    }
    if learned_new {
        learned::save();
    }
    let extracted = world
        .as_deref()
        .zip(frame.as_deref())
        .map(|(world, frame)| ExtractedColourRefs::new(world, frame));
    kick_admitted_pipelines(
        extracted,
        &views,
        &device,
        &mut registry,
        &pipeline,
        &mut kicked,
        &mut warmup,
    );
    registry.flush(&device);
}

pub(super) fn kick_admitted_pipelines(
    extracted: Option<ExtractedColourRefs<'_>>,
    views: &Query<(&ViewTarget, Option<&Msaa>), (With<Camera3d>, With<ViewUpscalingPipeline>)>,
    device: &RenderDevice,
    registry: &mut ExactPipelineRegistry,
    pipeline: &ExactColourPipeline,
    kicked: &mut ExactPipelineKickCache,
    warmup: &mut WorldPipelineWarmup,
) {
    let Some(extracted) = extracted else {
        return;
    };

    let Some((_, prepared_table)) = exec_tables(extracted) else {
        return;
    };

    let view_sig: Vec<(TextureFormat, u32)> = if views.is_empty() {
        vec![crate::LENS_VIEW_SIGNATURE]
    } else {
        views
            .iter()
            .map(|(target, msaa)| (target.main_texture_format(), msaa.map_or(1, Msaa::samples)))
            .collect()
    };
    let generation_changed = kicked.generation != Some(extracted.world.generation);
    let views_changed = kicked.views != view_sig;
    let demand_changed = kicked.demand_revision != extracted.frame.pipeline_demand_revision;
    if generation_changed || views_changed {
        if kicked.generation == Some(extracted.world.generation)
            && kicked.views == [crate::LENS_VIEW_SIGNATURE]
        {
            diag::warn!(
                World,
                "exact pipelines: lens view is {:?}, spawn_world declares {:?}; {} slots were walked for the wrong target",
                view_sig,
                crate::LENS_VIEW_SIGNATURE,
                kicked.current.len()
            );
        }

        kicked.current.clear();
        kicked.scheduled.clear();
        kicked.generation = Some(extracted.world.generation);
        kicked.views = view_sig.clone();
        schedule_admitted_pipelines(
            extracted,
            prepared_table,
            &view_sig,
            registry,
            pipeline,
            device,
            &mut kicked.scheduled,
            &mut kicked.current,
        );
        let learned = learned::keys();
        let learned_n = learned.len();
        for key in learned {
            if pipeline.get(key.port).is_some() && kicked.scheduled.insert(key) {
                kicked
                    .current
                    .push(request_exact_pipeline(registry, pipeline, device, key));
            }
        }
        kicked.demand_revision = extracted.frame.pipeline_demand_revision;
        diag::info!(
            World,
            "exact pipelines: learned={learned_n} working set={} modules held={} tasks in flight={}",
            kicked.current.len(),
            registry.module_n(),
            registry.building_n()
        );
    } else if demand_changed {
        let before = kicked.current.len();
        schedule_admitted_pipelines(
            extracted,
            prepared_table,
            &view_sig,
            registry,
            pipeline,
            device,
            &mut kicked.scheduled,
            &mut kicked.current,
        );
        kicked.demand_revision = extracted.frame.pipeline_demand_revision;
        diag::info!(
            World,
            "exact pipelines: demand revision={} added={} working set={}",
            extracted.frame.pipeline_demand_revision,
            kicked.current.len().saturating_sub(before),
            kicked.current.len()
        );
    }
    *warmup = WorldPipelineWarmup {
        generation: extracted.world.world_generation,
        initialized: true,
        total: kicked.current.len() as u32,
        ready: kicked
            .current
            .iter()
            .filter(|slot| registry.is_ready(**slot))
            .count() as u32,
    };
}

fn schedule_admitted_pipelines(
    extracted: ExtractedColourRefs<'_>,
    prepared_table: &render_material::PreparedMaterialTable,
    view_sig: &[(TextureFormat, u32)],
    registry: &mut ExactPipelineRegistry,
    pipeline: &ExactColourPipeline,
    device: &RenderDevice,
    scheduled: &mut HashSet<ExactColourPipelineKey>,
    current: &mut Vec<ExactPipelineSlot>,
) {
    // Switch: Mesa keeps ~1MB per GL program and the heap backs GPU memory,
    // so the full working set (~1500 on mp_rust) cannot fit. Build only what
    // draws discover; the overlay still waits for the visible set.
    // ponytail: no eviction, built set only grows; add LRU eviction if a
    // match's built count nears the heap.
    // Exception: the world's emissive passes (tech 5: the sky, emissive
    // surfaces) are few, and an unbuilt sky pass left the sky black in play
    // until it compiled.
    let world_emissive_only = cfg!(target_os = "horizon");
    for (main_format, samples) in view_sig.iter().copied() {
        let techs = lighting_iw4::LIT_TECH_NO_SHADOW_DIR_SLOTS
            .into_iter()
            .chain(lighting_iw4::LIT_TECH_NO_SHADOW_LOCAL_SLOTS)
            .chain(lighting_iw4::LIT_TECH_SHADOW_DIR_SLOTS)
            .chain(lighting_iw4::LIT_TECH_SHADOW_SPOT_SLOTS)
            .chain([super::super::SUN_SHADOW_CASTER_TECH, 5])
            .filter(|&tech| !world_emissive_only || tech == 5);
        for tech in techs {
            let shadow = tech == super::super::SUN_SHADOW_CASTER_TECH;
            for (material, prepared_pass) in
                prepared_table.passes_for_tech(super::super::TechType(tech))
            {
                let state = super::super::state::GfxPassState::from_bits(prepared_pass.state);
                let state0 = state.apply_change_state_0_host(AlphaMode::Opaque, false);
                let state1 = state.apply_change_state_1_host();
                let target = exact_colour_target_format(
                    if shadow {
                        SHADOWMAP_SUN_COLOR_FORMAT
                    } else {
                        main_format
                    },
                    state0.srgb_write,
                );
                for port in prepared_pass.port.iter().flatten() {
                    use asset_iw4::vertex_decl::{
                        PACKED_VERTEX_TYPE, STATICMODELCACHE_VERTEX_TYPE,
                    };
                    if world_emissive_only
                        && (port.vertex_type == PACKED_VERTEX_TYPE
                            || port.vertex_type == STATICMODELCACHE_VERTEX_TYPE
                            || !extracted.frame.pipeline_world_materials.contains(&material.0))
                    {
                        continue;
                    }
                    if tech != 5 && port.vertex_type != PACKED_VERTEX_TYPE {
                        let used = if port.vertex_type == STATICMODELCACHE_VERTEX_TYPE {
                            extracted
                                .frame
                                .pipeline_smodel_materials
                                .contains(&material.0)
                        } else {
                            extracted
                                .frame
                                .pipeline_world_materials
                                .contains(&material.0)
                        };
                        if !used {
                            continue;
                        }
                    }
                    let key = ExactColourPipelineKey {
                        target,
                        depth_format: if shadow {
                            SHADOWMAP_SUN_DEPTH_FORMAT
                        } else {
                            SCENE_DEPTH_FORMAT
                        },
                        samples: if shadow { 1 } else { samples },
                        state0,
                        state1,
                        port: *port,
                        cached_lighting: false,
                        mark_mesh: false,
                        forward_z: shadow,
                    };
                    if scheduled.insert(key) {
                        current.push(request_exact_pipeline(registry, pipeline, device, key));
                    }
                }
            }
        }
    }
}

/// Exact colour pipelines draws discovered, rebuilt with the load's working
/// set so the overlay holds for them (`drawsurf::learned`). One list for every
/// map; ports a map never admits are skipped.
mod learned {
    use super::*;
    use asset_core::AssetNamespace;
    use render_material::{GfxPassStateBits, PortId};

    const RECORD: usize = 43;
    static LEARNED: crate::drawsurf::learned::Learned<RECORD> =
        crate::drawsurf::learned::Learned::new("exact-demand-v1");
    const FORMATS: [TextureFormat; 8] = [
        TextureFormat::Rgba8Unorm,
        TextureFormat::Rgba8UnormSrgb,
        TextureFormat::Bgra8Unorm,
        TextureFormat::Bgra8UnormSrgb,
        TextureFormat::Rgba16Float,
        TextureFormat::R32Float,
        TextureFormat::Depth24PlusStencil8,
        TextureFormat::Depth32Float,
    ];

    pub(super) fn remember(key: &ExactColourPipelineKey, state: GfxPassStateBits) -> bool {
        encode(key, state).is_some_and(|record| LEARNED.remember(record))
    }

    pub(super) fn save() {
        LEARNED.save();
    }

    pub(super) fn keys() -> Vec<ExactColourPipelineKey> {
        LEARNED
            .records()
            .iter()
            .filter_map(decode)
            .collect()
    }

    fn encode(key: &ExactColourPipelineKey, state: GfxPassStateBits) -> Option<[u8; RECORD]> {
        let format = |format| FORMATS.iter().position(|&f| f == format).map(|i| i as u8);
        if key.cached_lighting {
            return None;
        }
        let namespace = match state.namespace {
            AssetNamespace::Iw4 => 0,
            AssetNamespace::T5 => 1,
            AssetNamespace::Iw5 => 2,
        };
        let port = key.port;
        let mut out = [0; RECORD];
        out[0..8].copy_from_slice(&port.vertex_program_hash.to_le_bytes());
        out[8..16].copy_from_slice(&port.pixel_program_hash.to_le_bytes());
        out[16..20].copy_from_slice(&port.vertex_decl_slot.to_le_bytes());
        out[20..28].copy_from_slice(&port.arguments_hash.to_le_bytes());
        out[28..32].copy_from_slice(&state.word0.to_le_bytes());
        out[32..36].copy_from_slice(&state.word1.to_le_bytes());
        out[36..43].copy_from_slice(&[
            port.vertex_type,
            port.custom_sampler_flags,
            namespace,
            format(key.target)?,
            format(key.depth_format)?,
            u8::try_from(key.samples).ok()?,
            u8::from(key.mark_mesh) | u8::from(key.forward_z) << 1,
        ]);
        Some(out)
    }

    /// The key as the draw that discovered it built it.
    fn decode(record: &[u8; RECORD]) -> Option<ExactColourPipelineKey> {
        let u64_at = |at: usize| u64::from_le_bytes(record[at..at + 8].try_into().unwrap());
        let u32_at = |at: usize| u32::from_le_bytes(record[at..at + 4].try_into().unwrap());
        let namespace = match record[38] {
            0 => AssetNamespace::Iw4,
            1 => AssetNamespace::T5,
            2 => AssetNamespace::Iw5,
            _ => return None,
        };
        let host = crate::drawsurf::state::GfxPassState::from_bits(GfxPassStateBits {
            namespace,
            word0: u32_at(28),
            word1: u32_at(32),
        });
        Some(ExactColourPipelineKey {
            target: *FORMATS.get(usize::from(record[39]))?,
            depth_format: *FORMATS.get(usize::from(record[40]))?,
            samples: u32::from(record[41]),
            state0: host.apply_change_state_0_host(AlphaMode::Opaque, false),
            state1: host.apply_change_state_1_host(),
            port: PortId {
                vertex_program_hash: u64_at(0),
                pixel_program_hash: u64_at(8),
                vertex_decl_slot: u32_at(16),
                vertex_type: record[36],
                custom_sampler_flags: record[37],
                arguments_hash: u64_at(20),
            },
            cached_lighting: false,
            mark_mesh: record[42] & 1 != 0,
            forward_z: record[42] & 2 != 0,
        })
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn record_round_trips_the_key() {
            let state = GfxPassStateBits {
                namespace: AssetNamespace::T5,
                word0: 0x0001_2345,
                word1: 0x8000_0040,
            };
            let host = crate::drawsurf::state::GfxPassState::from_bits(state);
            let key = ExactColourPipelineKey {
                target: TextureFormat::Rgba8UnormSrgb,
                depth_format: TextureFormat::Depth24PlusStencil8,
                samples: 1,
                state0: host.apply_change_state_0_host(AlphaMode::Opaque, false),
                state1: host.apply_change_state_1_host(),
                port: PortId {
                    vertex_program_hash: 1,
                    pixel_program_hash: u64::MAX,
                    vertex_decl_slot: 7,
                    vertex_type: 2,
                    custom_sampler_flags: 3,
                    arguments_hash: 0xdead_beef,
                },
                cached_lighting: false,
                mark_mesh: false,
                forward_z: true,
            };
            let record = encode(&key, state).expect("encodable key");
            assert!(decode(&record) == Some(key));
        }
    }
}
