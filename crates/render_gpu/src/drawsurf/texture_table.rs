use std::num::NonZeroU32;

use super::gpu_contract::{
    TEXTURE_TABLE_2D_CAPACITY, TEXTURE_TABLE_3D_CAPACITY, TEXTURE_TABLE_CUBE_CAPACITY,
    TEXTURE_TABLE_SAMPLER_CAPACITY,
};
use super::gpu_resources::{DecodedSampler, UploadedTextureBind, UploadedTextureIdentity};
use bevy::platform::collections::HashMap;
use bevy::prelude::*;
use bevy::render::render_resource::{
    BindGroup, BindGroupEntry, BindGroupLayoutDescriptor, BindingResource, BindingType, Extent3d,
    Sampler, SamplerDescriptor, TextureDescriptor, TextureDimension, TextureFormat, TextureUsages,
    TextureView, TextureViewDescriptor, TextureViewDimension, TextureViewId, WgpuSampler,
    WgpuTextureView,
};
use bevy::render::renderer::RenderDevice;
use d3d9_sm3::{TEXTURE_TABLE_BINDING_2D, TEXTURE_TABLE_BINDING_3D, TEXTURE_TABLE_BINDING_CUBE};
use render_material::{MaterialGenerationId, SamplerTextureDimension};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct SamplerKey {
    sampler_state: u8,
    packed_word: u32,
    anisotropy_clamp: u16,
    uses_mipmaps: bool,
}

impl From<&DecodedSampler> for SamplerKey {
    fn from(sampler: &DecodedSampler) -> Self {
        Self {
            sampler_state: sampler.sampler_state,
            packed_word: sampler.packed_word(),
            anisotropy_clamp: sampler.anisotropy_clamp(),
            uses_mipmaps: sampler.uses_mipmaps(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TextureTableRefusal {
    TexturesFull {
        dimension: SamplerTextureDimension,
        capacity: u32,
    },
    SamplersFull {
        capacity: u32,
    },
}

pub(super) struct TextureTableBinds {
    pub scene: BindGroup,
    pub sun_caster: BindGroup,
}

struct Placeholders {
    views: [TextureView; 3],
    sampler: Sampler,
}

#[derive(Resource, Default, Deref, DerefMut)]
pub(super) struct ShadowTextureTable(pub ExactTextureTable);

#[derive(Resource, Default, Deref, DerefMut)]
pub(super) struct SceneTextureTables(pub [ExactTextureTable; 4]);

pub(super) fn scene_table_index(after_scene_resolve: bool, srgb_write: bool) -> usize {
    usize::from(after_scene_resolve) * 2 + usize::from(srgb_write)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct TableEpoch {
    pub generation: MaterialGenerationId,

    pub replaced_revision: u64,
}

/// Switch flicker probe: table resets since the last colour diagnostics line.
pub(super) static TABLE_EPOCH_OPENS: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new(0);

#[derive(Resource, Default)]
pub(super) struct ExactTextureTable {
    epoch: TableEpoch,
    views: [Vec<TextureView>; 3],
    view_index: HashMap<TextureViewId, u16>,

    sun_shadow_indices: Vec<u16>,
    samplers: Vec<Sampler>,
    sampler_index: HashMap<SamplerKey, u16>,
    placeholders: Option<Placeholders>,
    binds: Option<TextureTableBinds>,
    /// `bound_slots` groups by (slot words, group-1 layout, sun shadow masked).
    slot_groups: HashMap<(Box<[u32]>, BindGroupLayoutDescriptor, bool), BindGroup>,

    pub rebuild_n: u32,
}

const fn dimension_lane(dimension: SamplerTextureDimension) -> usize {
    match dimension {
        SamplerTextureDimension::D2 => 0,
        SamplerTextureDimension::Cube => 1,
        SamplerTextureDimension::D3 => 2,
    }
}

const fn capacity(dimension: SamplerTextureDimension) -> u32 {
    match dimension {
        SamplerTextureDimension::D2 => TEXTURE_TABLE_2D_CAPACITY,
        SamplerTextureDimension::Cube => TEXTURE_TABLE_CUBE_CAPACITY,
        SamplerTextureDimension::D3 => TEXTURE_TABLE_3D_CAPACITY,
    }
}

impl ExactTextureTable {
    pub fn epoch(&self) -> TableEpoch {
        self.epoch
    }

    pub fn open_epoch(&mut self, epoch: TableEpoch) {
        if self.epoch == epoch {
            return;
        }
        TABLE_EPOCH_OPENS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.epoch = epoch;
        for lane in &mut self.views {
            lane.clear();
        }
        self.view_index.clear();
        self.sun_shadow_indices.clear();
        self.samplers.clear();
        self.sampler_index.clear();
        self.binds = None;
        self.slot_groups.clear();
    }

    pub fn slot_word(
        &mut self,
        device: &RenderDevice,
        lane: &UploadedTextureBind,
    ) -> Result<u32, TextureTableRefusal> {
        self.ensure_placeholders(device);
        let lane_i = dimension_lane(lane.dimension);
        let texture = match self.view_index.get(&lane.view.id()) {
            Some(&index) => index,
            None => {
                let cap = capacity(lane.dimension);
                let index = u32::try_from(self.views[lane_i].len()).expect("table index fits u32");
                if index >= cap {
                    return Err(TextureTableRefusal::TexturesFull {
                        dimension: lane.dimension,
                        capacity: cap,
                    });
                }
                let index = u16::try_from(index).expect("capacities fit the 16-bit slot half");
                self.views[lane_i].push(lane.view.clone());
                self.view_index.insert(lane.view.id(), index);
                if lane.identity == UploadedTextureIdentity::Code(super::CODE_TEXTURE_SHADOWMAP_SUN)
                {
                    self.sun_shadow_indices.push(index);
                }
                self.binds = None;
                index
            }
        };
        let key = SamplerKey::from(&lane.sampler);
        let sampler = match self.sampler_index.get(&key) {
            Some(&index) => index,
            None => {
                let index = u32::try_from(self.samplers.len()).expect("table index fits u32");
                if index >= TEXTURE_TABLE_SAMPLER_CAPACITY {
                    return Err(TextureTableRefusal::SamplersFull {
                        capacity: TEXTURE_TABLE_SAMPLER_CAPACITY,
                    });
                }
                let index = u16::try_from(index).expect("capacities fit the 16-bit slot half");
                self.samplers
                    .push(device.create_sampler(&lane.sampler.descriptor()));
                self.sampler_index.insert(key, index);
                self.binds = None;
                index
            }
        };
        Ok(d3d9_sm3::texture_slot_word(texture, sampler))
    }

    /// Group 1 for `bound_slots` pipelines: binding pair `i` of `layout` takes
    /// the view and sampler of `slots[i]` (words from `slot_word`).
    /// `mask_sun_shadow` swaps the sun shadow map for a placeholder, as the
    /// `sun_caster` table does, so casters never read the target they render.
    pub fn slot_bind_group(
        &mut self,
        device: &RenderDevice,
        registry: &super::exact_pipeline::ExactPipelineRegistry,
        layout: &BindGroupLayoutDescriptor,
        slots: &[u32],
        mask_sun_shadow: bool,
    ) -> BindGroup {
        let key = (Box::from(slots), layout.clone(), mask_sun_shadow);
        if let Some(group) = self.slot_groups.get(&key) {
            return group.clone();
        }
        self.ensure_placeholders(device);
        let placeholder_2d = &self.views[0][0];
        let entries: Vec<BindGroupEntry> = layout
            .entries
            .chunks_exact(2)
            .zip(slots)
            .flat_map(|(pair, &word)| {
                let lane = match pair[0].ty {
                    BindingType::Texture { view_dimension, .. } => {
                        super::probe_array::bound_slot_lane(view_dimension)
                    }
                    _ => unreachable!("bound slot layouts pair a texture with a sampler"),
                };
                let texture = (word & 0xffff) as u16;
                let view =
                    if mask_sun_shadow && lane == 0 && self.sun_shadow_indices.contains(&texture) {
                        placeholder_2d
                    } else {
                        &self.views[lane][usize::from(texture)]
                    };
                [
                    BindGroupEntry {
                        binding: pair[0].binding,
                        resource: BindingResource::TextureView(&**view),
                    },
                    BindGroupEntry {
                        binding: pair[1].binding,
                        resource: BindingResource::Sampler(&*self.samplers[(word >> 16) as usize]),
                    },
                ]
            })
            .collect();
        let bgl = registry.bind_group_layout(device, layout);
        let group = device.create_bind_group("iw4_bound_slots", &bgl, &entries);
        self.slot_groups.insert(key, group.clone());
        group
    }

    pub fn census(&self) -> (usize, usize, usize, usize) {
        let n = |lane: &Vec<TextureView>| lane.len().saturating_sub(1);
        (
            n(&self.views[0]),
            n(&self.views[1]),
            n(&self.views[2]),
            self.samplers.len().saturating_sub(1),
        )
    }

    pub fn binds(
        &mut self,
        device: &RenderDevice,
        registry: &super::exact_pipeline::ExactPipelineRegistry,
        layout: &BindGroupLayoutDescriptor,
    ) -> &TextureTableBinds {
        self.ensure_placeholders(device);
        if self.binds.is_none() {
            let bgl = registry.bind_group_layout(device, layout);
            let placeholder_2d = self.views[0][0].clone();
            let scene = self.create_bind_group(device, &bgl, "iw4_texture_table_scene", None);
            let sun_caster = if self.sun_shadow_indices.is_empty() {
                scene.clone()
            } else {
                self.create_bind_group(
                    device,
                    &bgl,
                    "iw4_texture_table_sun_caster",
                    Some(&placeholder_2d),
                )
            };
            self.rebuild_n = self.rebuild_n.saturating_add(1);
            self.binds = Some(TextureTableBinds { scene, sun_caster });
        }
        self.binds.as_ref().expect("built above")
    }

    pub fn views_bind_group(
        &mut self,
        device: &RenderDevice,
        bgl: &bevy::render::render_resource::BindGroupLayout,
        label: &'static str,
        views: &[&TextureView],
        samplers: &[&Sampler],
        slots: &[u32],
    ) -> BindGroup {
        if super::bound_slots() {
            let entries: Vec<BindGroupEntry> = (0u32..)
                .zip(slots)
                .flat_map(|(ordinal, &word)| {
                    let (texture, sampler) = d3d9_sm3::bound_slot_bindings(ordinal);
                    [
                        BindGroupEntry {
                            binding: texture,
                            resource: BindingResource::TextureView(
                                &**views[(word & 0xffff) as usize],
                            ),
                        },
                        BindGroupEntry {
                            binding: sampler,
                            resource: BindingResource::Sampler(&**samplers[(word >> 16) as usize]),
                        },
                    ]
                })
                .collect();
            return device.create_bind_group(label, bgl, &entries);
        }
        let views: Vec<&WgpuTextureView> = views.iter().map(|v| &***v).collect();
        let samplers: Vec<&WgpuSampler> = samplers.iter().map(|v| &***v).collect();
        self.ensure_placeholders(device);
        let placeholders = self.placeholders.as_ref().expect("built above");
        device.create_bind_group(
            label,
            bgl,
            &[
                BindGroupEntry {
                    binding: TEXTURE_TABLE_BINDING_2D,
                    resource: BindingResource::TextureViewArray(&views),
                },
                BindGroupEntry {
                    binding: TEXTURE_TABLE_BINDING_CUBE,
                    resource: BindingResource::TextureViewArray(&[&*placeholders.views[1]]),
                },
                BindGroupEntry {
                    binding: TEXTURE_TABLE_BINDING_3D,
                    resource: BindingResource::TextureViewArray(&[&*placeholders.views[2]]),
                },
                BindGroupEntry {
                    binding: d3d9_sm3::TEXTURE_TABLE_BINDING_SAMPLERS,
                    resource: BindingResource::SamplerArray(&samplers),
                },
            ],
        )
    }

    fn create_bind_group(
        &self,
        device: &RenderDevice,
        bgl: &bevy::render::render_resource::BindGroupLayout,
        label: &'static str,
        mask_sun_shadow_with: Option<&TextureView>,
    ) -> BindGroup {
        let mut views_2d: Vec<&WgpuTextureView> = self.views[0].iter().map(|v| &**v).collect();
        if let Some(placeholder) = mask_sun_shadow_with {
            for &index in &self.sun_shadow_indices {
                views_2d[usize::from(index)] = &**placeholder;
            }
        }
        let views_cube: Vec<&WgpuTextureView> = self.views[1].iter().map(|v| &**v).collect();
        let views_3d: Vec<&WgpuTextureView> = self.views[2].iter().map(|v| &**v).collect();
        let samplers: Vec<&WgpuSampler> = self.samplers.iter().map(|s| &**s).collect();
        device.create_bind_group(
            label,
            bgl,
            &[
                BindGroupEntry {
                    binding: TEXTURE_TABLE_BINDING_2D,
                    resource: BindingResource::TextureViewArray(&views_2d),
                },
                BindGroupEntry {
                    binding: TEXTURE_TABLE_BINDING_CUBE,
                    resource: BindingResource::TextureViewArray(&views_cube),
                },
                BindGroupEntry {
                    binding: TEXTURE_TABLE_BINDING_3D,
                    resource: BindingResource::TextureViewArray(&views_3d),
                },
                BindGroupEntry {
                    binding: d3d9_sm3::TEXTURE_TABLE_BINDING_SAMPLERS,
                    resource: BindingResource::SamplerArray(&samplers),
                },
            ],
        )
    }

    fn ensure_placeholders(&mut self, device: &RenderDevice) {
        let placeholders = self.placeholders.get_or_insert_with(|| {
            let make = |label, layers, dimension, view_dimension| {
                let texture = device.create_texture(&TextureDescriptor {
                    label: Some(label),
                    size: Extent3d {
                        width: 1,
                        height: 1,
                        depth_or_array_layers: layers,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension,
                    format: TextureFormat::R8Unorm,
                    usage: TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                });
                texture.create_view(&TextureViewDescriptor {
                    label: Some(label),
                    dimension: Some(view_dimension),
                    ..default()
                })
            };
            Placeholders {
                views: [
                    make(
                        "iw4_texture_table_placeholder_2d",
                        1,
                        TextureDimension::D2,
                        TextureViewDimension::D2,
                    ),
                    make(
                        "iw4_texture_table_placeholder_cube",
                        6,
                        TextureDimension::D2,
                        TextureViewDimension::Cube,
                    ),
                    make(
                        "iw4_texture_table_placeholder_3d",
                        1,
                        TextureDimension::D3,
                        TextureViewDimension::D3,
                    ),
                ],
                sampler: device.create_sampler(&SamplerDescriptor {
                    label: Some("iw4_texture_table_placeholder_sampler"),
                    ..default()
                }),
            }
        });
        for (lane, view) in self.views.iter_mut().zip(placeholders.views.iter()) {
            if lane.is_empty() {
                lane.push(view.clone());
            }
        }
        if self.samplers.is_empty() {
            self.samplers.push(placeholders.sampler.clone());
        }
    }
}

pub(super) fn array_count(count: u32) -> NonZeroU32 {
    NonZeroU32::new(count).expect("texture table capacities are non-zero")
}
