// Two overlapping triangles: instance 0 is the near (z = 0.25) green
// triangle on the left, drawn first; instance 1 is the far (z = 0.75) red
// triangle on the right, drawn second. With a LESS depth test the overlap
// stays green.
struct Out {
    @builtin(position) pos: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) v: u32, @builtin(instance_index) i: u32) -> Out {
    var corners = array<vec2<f32>, 3>(
        vec2<f32>(-0.6, 0.7),
        vec2<f32>(0.6, 0.7),
        vec2<f32>(0.0, -0.7),
    );
    var out: Out;
    let shift = select(-0.25, 0.25, i == 1u);
    let z = select(0.25, 0.75, i == 1u);
    out.pos = vec4<f32>(corners[v].x + shift, corners[v].y, z, 1.0);
    out.color = select(vec4<f32>(0.0, 1.0, 0.0, 1.0), vec4<f32>(1.0, 0.0, 0.0, 1.0), i == 1u);
    return out;
}

@fragment
fn fs_main(in: Out) -> @location(0) vec4<f32> {
    return in.color;
}
