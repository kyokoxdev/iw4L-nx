//! The recording device: wgpu's Noop backend with `deko3d-bake`, whose
//! `create_render_pipeline` records each pipeline's GLSL instead.

use std::collections::BTreeMap;

use bevy::render::renderer::RenderDevice;
use wgpu_hal::deko3d_shader::record;

pub fn noop_device() -> Result<RenderDevice, String> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::NOOP,
        backend_options: wgpu::BackendOptions {
            noop: wgpu::NoopBackendOptions {
                enable: true,
                ..Default::default()
            },
            ..Default::default()
        },
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    let adapter = bevy::tasks::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
        .map_err(|e| format!("noop adapter: {e}"))?;
    let (device, _queue) = bevy::tasks::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_features: adapter.features() - wgpu::Features::all_experimental_mask(),
        required_limits: adapter.limits(),
        ..Default::default()
    }))
    .map_err(|e| format!("noop device: {e}"))?;
    Ok(RenderDevice::new(device))
}

/// Each stage's GLSL by pack key.
#[derive(Default)]
pub struct Stages {
    pub glsl: BTreeMap<u64, (crate::uam::Stage, String)>,
    pub pipelines: usize,
    pub failed: Vec<(Option<String>, String)>,
}

impl Stages {
    pub fn drain(&mut self) {
        for recorded in record::take() {
            self.pipelines += 1;
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
