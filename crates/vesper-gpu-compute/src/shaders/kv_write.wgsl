// Stores one token's K and V ([kv_dim] each) at `position` of a paged
// sequence: page `table[position / 16]`, slot `position % 16`. A page holds
// 16 tokens of `kv_dim` floats in `keys` and in `values`.
struct Params {
    position: u32,
    kv_dim: u32,
}
var<immediate> p: Params;
@group(0) @binding(0) var<storage, read> k: array<f32>;
@group(0) @binding(1) var<storage, read> v: array<f32>;
@group(0) @binding(2) var<storage, read> table: array<u32>;
@group(0) @binding(3) var<storage, read_write> keys: array<f32>;
@group(0) @binding(4) var<storage, read_write> values: array<f32>;

@compute @workgroup_size(64)
fn kv_write(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = id.x;
    if i >= p.kv_dim {
        return;
    }
    let page = table[p.position / 16u];
    let base = (page * 16u + p.position % 16u) * p.kv_dim;
    keys[base + i] = k[i];
    values[base + i] = v[i];
}
