// Rotary position embedding on interleaved pairs (2i, 2i + 1) of each head:
// angle position * freqs[i / 2], with freqs[i / 2] = base^(-2i / head_dim)
// computed on the host by the CPU formula (the device's `pow` is not
// accurate enough: at position 2047 one ulp of frequency moves the angle by
// about 1e-4). One invocation per pair of `len / 2`.
struct Params {
    len: u32,
    head_dim: u32,
    position: u32,
}
var<immediate> p: Params;
@group(0) @binding(0) var<storage, read_write> x: array<f32>;
@group(0) @binding(1) var<storage, read> freqs: array<f32>;

@compute @workgroup_size(64)
fn rope(@builtin(global_invocation_id) id: vec3<u32>) {
    let e = id.x * 2u;
    if e + 1u >= p.len {
        return;
    }
    let i = e % p.head_dim;
    let angle = f32(p.position) * freqs[i / 2u];
    let c = cos(angle);
    let s = sin(angle);
    let v0 = x[e];
    let v1 = x[e + 1u];
    x[e] = v0 * c - v1 * s;
    x[e + 1u] = v0 * s + v1 * c;
}
