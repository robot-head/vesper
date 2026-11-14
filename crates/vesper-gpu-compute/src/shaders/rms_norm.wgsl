// out[r, i] = weight[i] * (x[r, i] / sqrt(mean_i(x[r, i]^2) + eps)), one
// 256-invocation workgroup per row with a tree reduction of the squares.
struct Params {
    n: u32,
    eps: f32,
}
var<immediate> p: Params;
@group(0) @binding(0) var<storage, read> x: array<f32>;
@group(0) @binding(1) var<storage, read> weight: array<f32>;
@group(0) @binding(2) var<storage, read_write> out: array<f32>;
var<workgroup> partial: array<f32, 256>;

@compute @workgroup_size(256)
fn rms_norm(
    @builtin(local_invocation_index) li: u32,
    @builtin(workgroup_id) wg: vec3<u32>,
) {
    let base = wg.x * p.n;
    var s = 0.0;
    for (var i = li; i < p.n; i += 256u) {
        let v = x[base + i];
        s += v * v;
    }
    partial[li] = s;
    workgroupBarrier();
    for (var stride = 128u; stride > 0u; stride = stride >> 1u) {
        if li < stride {
            partial[li] = partial[li] + partial[li + stride];
        }
        workgroupBarrier();
    }
    let ss = partial[0] / f32(p.n) + p.eps;
    let inv = 1.0 / sqrt(ss);
    for (var i = li; i < p.n; i += 256u) {
        out[base + i] = weight[i] * (inv * x[base + i]);
    }
}
