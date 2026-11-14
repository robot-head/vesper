// Reads every texel of `src` and writes its RGBA8 complement to `dst`.
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var<storage, read_write> dst: array<u32>;

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = textureDimensions(src);
    if (id.x < size.x && id.y < size.y) {
        let texel = textureLoad(src, vec2<i32>(id.xy), 0);
        dst[id.y * size.x + id.x] = pack4x8unorm(vec4<f32>(1.0) - texel);
    }
}
