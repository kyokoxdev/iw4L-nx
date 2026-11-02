//! The recording device: wgpu's Noop backend with `deko3d-bake`, whose
//! `create_render_pipeline` records each pipeline's GLSL instead.

use std::collections::BTreeMap;

use bevy::render::renderer::{
    RenderAdapter, RenderAdapterInfo, RenderDevice, RenderInstance, RenderQueue,
};
use bevy::render::settings::RenderResources;
use wgpu_hal::deko3d_shader::record;

/// What the deko3d adapter reports (`wgpu-hal/src/deko3d/mod.rs` `features`
/// and `capabilities`). Bevy takes the adapter's full set, and its shader defs
/// follow these, so the bake's device must match the Switch's.
// ponytail: copied from the fork; move both into deko3d_shader.rs with the
// next fork push so they can't drift.
fn deko3d_caps() -> (wgpu::Features, wgpu::Limits) {
    let features = wgpu::Features::TEXTURE_COMPRESSION_BC
        | wgpu::Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES
        | wgpu::Features::DEPTH32FLOAT_STENCIL8
        | wgpu::Features::DEPTH_CLIP_CONTROL
        | wgpu::Features::POLYGON_MODE_LINE
        | wgpu::Features::INDIRECT_FIRST_INSTANCE;
    let limits = wgpu::Limits {
        max_sampled_textures_per_shader_stage: 32, // DK_NUM_TEXTURE_BINDINGS
        max_samplers_per_shader_stage: 32,
        max_uniform_buffers_per_shader_stage: 16, // DK_NUM_UNIFORM_BUFS
        max_storage_buffers_per_shader_stage: 16, // DK_NUM_STORAGE_BUFS
        max_storage_textures_per_shader_stage: 8, // DK_NUM_IMAGE_BINDINGS
        max_uniform_buffer_binding_size: 0x10000, // DK_UNIFORM_BUF_MAX_SIZE
        max_color_attachments: 8,                 // DK_MAX_RENDER_TARGETS
        max_vertex_buffers: 16,                   // DK_MAX_VERTEX_BUFFERS
        max_vertex_attributes: 32,                // DK_MAX_VERTEX_ATTRIBS
        ..wgpu::Limits::default()
    };
    (features, limits)
}

pub fn noop_resources() -> Result<RenderResources, String> {
    let (features, limits) = deko3d_caps();
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::NOOP,
        backend_options: wgpu::BackendOptions {
            noop: wgpu::NoopBackendOptions {
                enable: true,
                features: Some(features),
                limits: Some(limits),
                ..Default::default()
            },
            ..Default::default()
        },
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    let adapter = bevy::tasks::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
        .map_err(|e| format!("noop adapter: {e}"))?;
    let (device, queue) = bevy::tasks::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_features: adapter.features() - wgpu::Features::all_experimental_mask(),
        required_limits: adapter.limits(),
        ..Default::default()
    }))
    .map_err(|e| format!("noop device: {e}"))?;
    let info = adapter.get_info();
    Ok(RenderResources(
        RenderDevice::new(device),
        RenderQueue::new(queue),
        RenderAdapterInfo::new(info),
        RenderAdapter::new(adapter),
        RenderInstance::new(instance),
    ))
}

/// Each stage's GLSL by pack key.
#[derive(Default)]
pub struct Stages {
    pub glsl: BTreeMap<u64, (crate::uam::Stage, String)>,
    pub pipelines: usize,
    pub failed: Vec<(Option<String>, String)>,
    pub labels: Vec<String>,
}

impl Stages {
    pub fn drain(&mut self) {
        for recorded in record::take() {
            self.pipelines += 1;
            self.labels.extend(recorded.label.clone());
            match recorded.result {
                Ok((vertex, fragment)) => {
                    let fragment = fragment.map(|f| (crate::uam::Stage::Fragment, f));
                    for (stage, glsl) in
                        std::iter::once((crate::uam::Stage::Vertex, vertex)).chain(fragment)
                    {
                        self.glsl.entry(glsl.key).or_insert((stage, glsl.text));
                    }
                }
                Err(e) => self.failed.push((recorded.label, e.to_string())),
            }
        }
    }
}
