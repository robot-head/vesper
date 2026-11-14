// One triangle from @builtin(vertex_index); no vertex buffers.
@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    var corners = array<vec2<f32>, 3>(
        vec2<f32>(0.0, -0.75),
        vec2<f32>(0.75, 0.75),
        vec2<f32>(-0.75, 0.75),
    );
    return vec4<f32>(corners[index], 0.0, 1.0);
}

@fragment
fn fs_main() -> @location(0) vec4<f32> {
    return vec4<f32>(1.0, 0.25, 0.0, 1.0);
}
