// gate[i] = silu(gate[i]) * up[i] with silu(v) = v * (1 / (1 + exp(-v))).
struct Params {
    n: u32,
}
var<immediate> p: Params;
@group(0) @binding(0) var<storage, read_write> gate: array<f32>;
@group(0) @binding(1) var<storage, read> up: array<f32>;

@compute @workgroup_size(64)
fn swiglu(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = id.x;
    if i >= p.n {
        return;
    }
    let v = gate[i];
    gate[i] = v * (1.0 / (1.0 + exp(-v))) * up[i];
}
