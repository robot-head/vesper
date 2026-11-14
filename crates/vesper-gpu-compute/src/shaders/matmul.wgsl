// out[r, c] (+)= sum_i w[c, i] * a[r, i]: `a` is [m, k] and `w` is [n, k],
// row-major (the llama2.c weight layout, so m = 1 is the decoder's matvec).
// Each invocation sums one output in ascending i, the order of
// vesper-tensor's CPU `row_dot`.
struct Params {
    m: u32,
    n: u32,
    k: u32,
    accumulate: u32,
}
var<immediate> p: Params;
@group(0) @binding(0) var<storage, read> a: array<f32>;
@group(0) @binding(1) var<storage, read> w: array<f32>;
@group(0) @binding(2) var<storage, read_write> out: array<f32>;

@compute @workgroup_size(64)
fn matmul(@builtin(global_invocation_id) id: vec3<u32>) {
    let col = id.x;
    let row = id.y;
    if col >= p.n || row >= p.m {
        return;
    }
    var s = 0.0;
    for (var i = 0u; i < p.k; i++) {
        s += w[col * p.k + i] * a[row * p.k + i];
    }
    let o = row * p.n + col;
    if p.accumulate == 1u {
        out[o] = out[o] + s;
    } else {
        out[o] = s;
    }
}
