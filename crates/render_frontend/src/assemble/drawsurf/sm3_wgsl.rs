use d3d9_sm3::{
    ConstantSlot, DeclType, PassLoweringAbi, PassWgsl, SamplerSlot, Sm3ProgramIr, Sm3Wgsl,
    VaryingLink, VertexInput, lower_pass_to_wgsl,
};
use render_material::MATERIAL_ALPHA_TESTS;

use super::sm3_abi::{ConstantSource, PassProgramAbi};

pub use d3d9_sm3::{PASS_FRAGMENT_ENTRY, PASS_VERTEX_ENTRY};

pub type ValidatedSm3Wgsl = Sm3Wgsl;

pub type ValidatedPassWgsl = PassWgsl;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Sm3WgslError {
    Emit(d3d9_sm3::Sm3WgslError),
    WgslParse(String),
    WgslValidation(String),
}

impl From<d3d9_sm3::Sm3WgslError> for Sm3WgslError {
    fn from(value: d3d9_sm3::Sm3WgslError) -> Self {
        Self::Emit(value)
    }
}

fn known_decl_type(decl_type: DeclType) -> DeclType {
    match decl_type {
        DeclType::Unknown(_) => unreachable!("the ABI refuses unknown declaration types"),
        known => known,
    }
}

pub fn pass_lowering_abi(abi: &PassProgramAbi) -> PassLoweringAbi {
    PassLoweringAbi {
        vertex_inputs: abi
            .vertex_inputs
            .iter()
            .map(|input| VertexInput {
                register: input.register,
                location: input.attribute.map(|attribute| attribute.location),
                decl_type: input
                    .attribute
                    .map(|attribute| known_decl_type(attribute.layout.decl_type)),
                semantic: input.semantic,
            })
            .collect(),
        position: VaryingLink {
            semantic: abi.position.semantic,
            vertex_register: abi.position.vertex_register,
            pixel_register: abi.position.pixel_register,
            location: abi.position.location,
        },
        varyings: abi
            .varyings
            .iter()
            .map(|varying| VaryingLink {
                semantic: varying.semantic,
                vertex_register: varying.vertex_register,
                pixel_register: varying.pixel_register,
                location: varying.location,
            })
            .collect(),
        vertex_constants: abi
            .vertex_constants
            .iter()
            .map(|binding| ConstantSlot {
                register: binding.register,
                program_defined: binding.source == ConstantSource::ProgramDefined,
            })
            .collect(),
        pixel_constants: abi
            .pixel_constants
            .iter()
            .map(|binding| ConstantSlot {
                register: binding.register,
                program_defined: binding.source == ConstantSource::ProgramDefined,
            })
            .collect(),
        samplers: abi
            .samplers
            .iter()
            .map(|binding| SamplerSlot {
                register: binding.register,
                dimension: binding.dimension,
                depth_compare: binding.depth_compare,
            })
            .collect(),
        alpha_tests: MATERIAL_ALPHA_TESTS.to_vec(),
        bound_slots: super::gpu_contract::bound_slots(),
        depth_to_colour: abi.depth_to_colour,
    }
}

pub(crate) fn validate_wgsl(source: &str) -> Result<(), Sm3WgslError> {
    let module = naga::front::wgsl::parse_str(source)
        .map_err(|error| Sm3WgslError::WgslParse(error.emit_to_string(source)))?;
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .map_err(|error| Sm3WgslError::WgslValidation(error.to_string()))?;
    Ok(())
}

pub fn lower_pass_to_validated_wgsl(
    abi: &PassProgramAbi,
    vertex: &Sm3ProgramIr,
    pixel: &Sm3ProgramIr,
) -> Result<ValidatedPassWgsl, Sm3WgslError> {
    let lowered = lower_pass_to_wgsl(&pass_lowering_abi(abi), vertex, pixel)?;
    validate_wgsl(&lowered.source)?;
    Ok(lowered)
}

pub(crate) fn dxbc_alpha_tests() -> Vec<(String, String)> {
    MATERIAL_ALPHA_TESTS
        .iter()
        .enumerate()
        .map(|(index, test)| {
            let operator = match test.func {
                d3d9_sm3::CompareFunc::Never => return (index, Some("discard;".to_owned())),
                d3d9_sm3::CompareFunc::Always => return (index, Some(String::new())),
                d3d9_sm3::CompareFunc::Less => "<",
                d3d9_sm3::CompareFunc::Equal => "==",
                d3d9_sm3::CompareFunc::LessEqual => "<=",
                d3d9_sm3::CompareFunc::Greater => ">",
                d3d9_sm3::CompareFunc::NotEqual => "!=",
                d3d9_sm3::CompareFunc::GreaterEqual => ">=",
                d3d9_sm3::CompareFunc::Unknown(_) => return (index, None),
            };
            (
                index,
                Some(format!(
                    "if !(round(clamp(dx_colour.a, 0.0, 1.0) * {:.1}) {operator} {:.1}) {{ discard; }}",
                    d3d9_sm3::abi::ALPHA_REF_SCALE,
                    f32::from(test.reference)
                )),
            )
        })
        .filter_map(|(index, test)| {
            Some((d3d9_sm3::pass_fragment_alpha_test_entry(index), test?))
        })
        .collect()
}

#[cfg(test)]
mod instance_prologue_tests {
    use d3d9_sm3::{Semantic, ShaderStage, Sm3Register, Sm3RegisterFile, decode_sm3_program};

    use super::*;

    fn words(tokens: &[u32]) -> Vec<u8> {
        tokens.iter().flat_map(|token| token.to_le_bytes()).collect()
    }

    /// `vs_3_0: dcl_position v0; dcl_position o0; mov o0, c0` and
    /// `ps_3_0: mov oC0, c0`, lowered and run through naga.
    #[test]
    fn instanced_pass_validates() {
        let vertex = words(&[
            0xFFFE_0300, 0x0200_001F, 0x8000_0000, 0x900F_0000, 0x0200_001F, 0x8000_0000,
            0xE00F_0000, 0x0200_0001, 0xE00F_0000, 0xA0E4_0000, 0x0000_FFFF,
        ]);
        let pixel = words(&[0xFFFF_0300, 0x0200_0001, 0x800F_0800, 0xA0E4_0000, 0x0000_FFFF]);
        let vertex = decode_sm3_program(&vertex, ShaderStage::Vertex).unwrap();
        let pixel = decode_sm3_program(&pixel, ShaderStage::Pixel).unwrap();
        let position = Semantic { usage: 0, usage_index: 0 };
        let constant = ConstantSlot { register: 0, program_defined: false };
        let abi = PassLoweringAbi {
            vertex_inputs: vec![VertexInput {
                register: Sm3Register { file: Sm3RegisterFile::Input, index: 0 },
                location: Some(0),
                decl_type: Some(DeclType::Float4),
                semantic: position,
            }],
            position: VaryingLink {
                semantic: position,
                vertex_register: Sm3Register { file: Sm3RegisterFile::Output, index: 0 },
                pixel_register: None,
                location: 0,
            },
            varyings: Vec::new(),
            vertex_constants: vec![constant],
            pixel_constants: vec![constant],
            samplers: Vec::new(),
            alpha_tests: MATERIAL_ALPHA_TESTS.to_vec(),
            bound_slots: false,
            depth_to_colour: false,
        };
        let lowered = lower_pass_to_wgsl(&abi, &vertex, &pixel).unwrap();
        assert!(lowered.source.contains(d3d9_sm3::wgsl::INSTANCE_BASE_PROLOGUE));
        validate_wgsl(&lowered.source).unwrap_or_else(|error| panic!("{error:?}\n{}", lowered.source));
    }
}
