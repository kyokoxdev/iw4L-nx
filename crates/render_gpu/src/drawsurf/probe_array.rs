//! Reflection probe cube array for world draws under `bound_slots`.
//!
//! World runs that differ only in reflection probe used to need their own
//! texture bind group, so each one broke the draw before it (most of the
//! world's texture breaks on Switch). The variant here samples every probe
//! from one cube array and takes the layer from a per-vertex stream, so those
//! runs share one bind group and fold into one draw.

use bevy::mesh::VertexBufferLayout;
use bevy::render::render_resource::{
    BindGroupLayoutDescriptor, BindingType, TextureViewDimension, VertexAttribute, VertexFormat,
    VertexStepMode,
};
use render_material::{PassProgramAbi, SamplerSource, SamplerTextureDimension};

/// Vertex buffer slot of the per-vertex probe layer (`u32`).
pub(super) const PROBE_VERTEX_SLOT: usize = 2;

/// The port's reflection probe sampler: its ordinal in `abi.samplers` and register.
fn probe_sampler(abi: &PassProgramAbi) -> Option<(usize, u16)> {
    let mut probes = abi.samplers.iter().enumerate().filter(|(_, slot)| {
        slot.source == SamplerSource::SurfaceReflectionProbe
            && slot.dimension == SamplerTextureDimension::Cube
    });
    let (ordinal, slot) = probes.next()?;
    probes.next().is_none().then_some((ordinal, slot.register))
}

pub(super) struct ProbeArrayVariant {
    pub source: std::sync::Arc<str>,
    pub vertex_buffers: Vec<VertexBufferLayout>,
    pub textures_layout: BindGroupLayoutDescriptor,
}

/// The probe-array twin of a port, or `None` when it samples no probe cube or
/// its vertex streams already reach the probe slot.
pub(super) fn probe_array_variant(
    abi: &PassProgramAbi,
    source: &str,
    vertex_buffers: &[VertexBufferLayout],
    textures_layout: &BindGroupLayoutDescriptor,
) -> Option<ProbeArrayVariant> {
    let (ordinal, register) = probe_sampler(abi)?;
    if vertex_buffers.len() > PROBE_VERTEX_SLOT {
        return None;
    }
    let attribute = super::smodel_cached::cached_lighting_location(abi)?;
    let varying = u32::try_from(abi.varyings.len()).ok()? + 1;
    let source = rewrite_wgsl(source, register, attribute, varying)?;

    let mut buffers = vertex_buffers.to_vec();
    buffers.resize_with(PROBE_VERTEX_SLOT, || VertexBufferLayout {
        array_stride: 0,
        step_mode: VertexStepMode::Vertex,
        attributes: Vec::new(),
    });
    buffers.push(VertexBufferLayout {
        array_stride: 4,
        step_mode: VertexStepMode::Vertex,
        attributes: vec![VertexAttribute {
            format: VertexFormat::Uint32,
            offset: 0,
            shader_location: attribute,
        }],
    });

    let mut layout = textures_layout.clone();
    let (texture_binding, _) = d3d9_sm3::bound_slot_bindings(u32::try_from(ordinal).ok()?);
    let entry = layout
        .entries
        .iter_mut()
        .find(|entry| entry.binding == texture_binding)?;
    let BindingType::Texture { view_dimension, .. } = &mut entry.ty else {
        return None;
    };
    *view_dimension = TextureViewDimension::CubeArray;
    layout.label = "iw4_exact_colour_textures_probe_array".into();

    Some(ProbeArrayVariant {
        source: source.into(),
        vertex_buffers: buffers,
        textures_layout: layout,
    })
}

/// Rewrite bound-slot pass WGSL so sampler `register` is a cube array whose
/// layer is a flat varying fed by vertex attribute `attribute`.
fn rewrite_wgsl(source: &str, register: u16, attribute: u32, varying: u32) -> Option<String> {
    let texture = format!("sm3_tex_s{register}");
    let mut out = replace_once(
        source,
        &format!("var {texture}: texture_cube<f32>;"),
        &format!("var {texture}: texture_cube_array<f32>;"),
    )?;
    let base = format!(
        "    @interpolate(flat) @location({}) sm3_constant_base: u32,\n",
        varying - 1
    );
    out = replace_once(
        &out,
        &base,
        &format!("{base}    @interpolate(flat) @location({varying}) sm3_probe_layer: u32,\n"),
    )?;
    out = replace_once(
        &out,
        "    @builtin(instance_index) sm3_instance: u32,\n",
        &format!(
            "    @location({attribute}) sm3_probe_attr: u32,\n    @builtin(instance_index) sm3_instance: u32,\n"
        ),
    )?;
    out = replace_once(
        &out,
        "        sm3_constant_base,\n    );\n}\n",
        "        sm3_constant_base,\n        sm3_probe_attr,\n    );\n}\n",
    )?;
    // The probe is read in the pixel stage only, where `varyings` is in scope.
    let pixel = out.find("\nfn sm3_pixel_main(")?;
    let mut rewritten = String::with_capacity(out.len() + 256);
    let mut at = 0;
    while let Some(found) = out[at..].find(&format!("({texture}, ")) {
        let open = at + found;
        if open < pixel {
            return None;
        }
        let close = matching_paren(&out, open)?;
        let call = &out[open + 1..close];
        let args = split_args(call);
        let name_start = out[..open].rfind(|c: char| !c.is_ascii_alphanumeric())? + 1;
        let layer = "varyings.sm3_probe_layer";
        let args = match &out[name_start..open] {
            // (t, s, coord[, level]) and (t, s, coord, ddx, ddy): layer after coord.
            "textureSample" | "textureSampleLevel" | "textureSampleGrad" | "textureSampleBias"
                if args.len() >= 3 =>
            {
                let mut args = args;
                args.insert(3, layer);
                args.join(", ")
            }
            _ => return None,
        };
        rewritten.push_str(&out[at..=open]);
        rewritten.push_str(&args);
        at = close;
    }
    rewritten.push_str(&out[at..]);
    Some(rewritten)
}

fn replace_once(source: &str, from: &str, to: &str) -> Option<String> {
    (source.matches(from).count() == 1).then(|| source.replacen(from, to, 1))
}

fn matching_paren(source: &str, open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (offset, byte) in source.as_bytes()[open..].iter().enumerate() {
        match byte {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(open + offset);
                }
            }
            _ => {}
        }
    }
    None
}

/// Top-level comma split of a call's argument list.
fn split_args(call: &str) -> Vec<&str> {
    let mut args = Vec::new();
    let (mut depth, mut start) = (0usize, 0);
    for (i, byte) in call.bytes().enumerate() {
        match byte {
            b'(' => depth += 1,
            b')' => depth -= 1,
            b',' if depth == 0 => {
                args.push(call[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    args.push(call[start..].trim());
    args
}

/// Slot `slots[i]`'s table lane for a bound-slot layout texture binding.
pub(super) fn bound_slot_lane(view_dimension: TextureViewDimension) -> usize {
    match view_dimension {
        TextureViewDimension::Cube | TextureViewDimension::CubeArray => 1,
        TextureViewDimension::D3 => 2,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shaped like `d3d9_sm3::lower_pass_to_wgsl` output under `bound_slots`.
    const PASS: &str = "struct Sm3ConstantArena { c: array<vec4<f32>> }
@group(0) @binding(0) var<storage, read> sm3_constants: Sm3ConstantArena;
@group(1) @binding(0) var sm3_tex_s0: texture_2d<f32>;
@group(1) @binding(1) var sm3_smp_s0: sampler;
@group(1) @binding(2) var sm3_tex_s1: texture_cube<f32>;
@group(1) @binding(3) var sm3_smp_s1: sampler;

struct Sm3Varyings {
    @builtin(position) @invariant position: vec4<f32>,
    @location(0) varying_0: vec4<f32>,
    @location(1) varying_1: vec4<f32>,
    @interpolate(flat) @location(2) sm3_constant_base: u32,
}

@vertex
fn sm3_vertex_main(
    @location(0) attribute_0: vec3<f32>,
    @location(1) attribute_1: vec2<f32>,
    @builtin(instance_index) sm3_instance: u32,
    @builtin(vertex_index) sm3_vertex: u32,
) -> Sm3Varyings {
    var sm3_constant_base = sm3_instance;
    let v0 = vec4<f32>(attribute_0, 1.0);
    let v1 = vec4<f32>(attribute_1, 0.0, 1.0);
    let o0 = v0 + sm3_constants.c[sm3_constant_base];
    return Sm3Varyings(
        o0,
        v1,
        v0,
        sm3_constant_base,
    );
}

fn sm3_pixel_main(varyings: Sm3Varyings) -> vec4<f32> {
    let t0 = varyings.varying_0;
    let t1 = varyings.varying_1;
    let a = textureSample(sm3_tex_s0, sm3_smp_s0, vec2<f32>(t0.x, t0.y)).x;
    let r = textureSample(sm3_tex_s1, sm3_smp_s1, vec3<f32>(t1.x, (t1.y * a), t1.z)).xyz;
    let l = textureSampleLevel(sm3_tex_s1, sm3_smp_s1, vec3<f32>(t1.x, t1.y, t1.z), t1.w).w;
    return vec4<f32>(r, l);
}

@fragment
fn sm3_fragment_main(varyings: Sm3Varyings) -> @location(0) vec4<f32> {
    let oC0 = sm3_pixel_main(varyings);
    return oC0;
}
";

    fn validate(source: &str) {
        let module = naga::front::wgsl::parse_str(source).expect("variant parses");
        let info = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .expect("variant validates");
        // Switch runs desktop GL 4.3 core through wgpu-hal's GLSL path.
        for (stage, entry) in [
            (naga::ShaderStage::Vertex, "sm3_vertex_main"),
            (naga::ShaderStage::Fragment, "sm3_fragment_main"),
        ] {
            let mut glsl = String::new();
            naga::back::glsl::Writer::new(
                &mut glsl,
                &module,
                &info,
                &naga::back::glsl::Options {
                    version: naga::back::glsl::Version::Desktop(430),
                    ..Default::default()
                },
                &naga::back::glsl::PipelineOptions {
                    shader_stage: stage,
                    entry_point: entry.into(),
                    multiview: None,
                },
                naga::proc::BoundsCheckPolicies::default(),
            )
            .expect("glsl writer")
            .write()
            .expect("glsl out");
        }
    }

    #[test]
    fn variant_samples_the_probe_layer_and_still_validates() {
        validate(PASS);
        let out = rewrite_wgsl(PASS, 1, 2, 3).expect("rewritten");
        assert!(out.contains("var sm3_tex_s1: texture_cube_array<f32>;"));
        assert!(out.contains("@location(2) sm3_probe_attr: u32,"));
        assert!(out.contains("@interpolate(flat) @location(3) sm3_probe_layer: u32,"));
        assert!(out.contains(
            "textureSample(sm3_tex_s1, sm3_smp_s1, vec3<f32>(t1.x, (t1.y * a), t1.z), varyings.sm3_probe_layer)"
        ));
        assert!(out.contains(
            "textureSampleLevel(sm3_tex_s1, sm3_smp_s1, vec3<f32>(t1.x, t1.y, t1.z), varyings.sm3_probe_layer, t1.w)"
        ));
        assert!(out.contains("textureSample(sm3_tex_s0, sm3_smp_s0, vec2<f32>(t0.x, t0.y))"));
        validate(&out);
    }

    #[test]
    fn a_probe_read_in_the_vertex_stage_has_no_variant() {
        let vertex_read = PASS.replace(
            "    let o0 = v0",
            "    let p = textureSampleLevel(sm3_tex_s1, sm3_smp_s1, v0.xyz, 0.0);\n    let o0 = v0",
        );
        assert!(rewrite_wgsl(&vertex_read, 1, 2, 3).is_none());
    }
}
