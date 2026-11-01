#version 460 core
#extension GL_ARB_shader_storage_buffer_object : require
#extension GL_ARB_shader_draw_parameters : require
struct VsOut {
    vec4 pos;
    vec2 uv;
};
layout(std430, binding = 0) readonly buffer Sm3ConstantArena_block_0Vertex {
    vec4 c[];
} _group_0_binding_0_vs;

layout(location = 0) in vec4 _p2vs_location0;
layout(location = 0) smooth out vec2 _vs2fs_location0;

void main() {
    vec4 v0_ = _p2vs_location0;
    uint inst = (uint(gl_InstanceID) + uint(gl_BaseInstanceARB));
    vec4 m = _group_0_binding_0_vs.c[(inst * 4u)];
    VsOut _tmp_return = VsOut((v0_ * m), v0_.xy);
    gl_Position = _tmp_return.pos;
    _vs2fs_location0 = _tmp_return.uv;
    return;
}

