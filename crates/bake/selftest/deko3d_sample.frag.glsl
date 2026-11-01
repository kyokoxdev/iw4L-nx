#version 460 core
#extension GL_ARB_shader_storage_buffer_object : require
struct VsOut {
    vec4 pos;
    vec2 uv;
};
layout(std430, binding = 0) readonly buffer Sm3ConstantArena_block_0Fragment {
    vec4 c[];
} _group_0_binding_0_fs;

layout(binding = 0) uniform sampler2D _group_1_binding_0_fs;

layout(binding = 1) uniform samplerCube _group_1_binding_2_fs;

layout(location = 0) smooth in vec2 _vs2fs_location0;
layout(location = 0) out vec4 _fs2p_location0;

void main() {
    VsOut in_ = VsOut(gl_FragCoord, _vs2fs_location0);
    vec4 a = texture(_group_1_binding_0_fs, vec2(in_.uv));
    vec4 b = texture(_group_1_binding_2_fs, vec3(vec3(in_.uv, 1.0)));
    vec4 _e15 = _group_0_binding_0_fs.c[1];
    _fs2p_location0 = ((a * b) * _e15);
    return;
}

