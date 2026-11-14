// Grouped-query decode attention over a paged K/V sequence. One workgroup
// per query head h, which reads KV head h / (query_heads / kv_heads). Token
// t lives in page table[t / 16], slot t % 16. Two passes in the order of
// vesper-tensor's AttentionMode::TwoPass: scores q.k / sqrt(head_dim), a
// max-subtracted softmax, then the probability-weighted sum of values.
struct Params {
    query_heads: u32,
    kv_heads: u32,
    head_dim: u32,
    tokens: u32,
}
const MAX_TOKENS: u32 = 2048u;
var<immediate> p: Params;
@group(0) @binding(0) var<storage, read> q: array<f32>;
@group(0) @binding(1) var<storage, read> keys: array<f32>;
@group(0) @binding(2) var<storage, read> values: array<f32>;
@group(0) @binding(3) var<storage, read> table: array<u32>;
@group(0) @binding(4) var<storage, read_write> out: array<f32>;
var<workgroup> scores: array<f32, 2048>;
var<workgroup> partial: array<f32, 64>;

fn token_base(t: u32, kv_head: u32) -> u32 {
    let page = table[t / 16u];
    return ((page * 16u + t % 16u) * p.kv_heads + kv_head) * p.head_dim;
}

@compute @workgroup_size(64)
fn attention(
    @builtin(local_invocation_index) li: u32,
    @builtin(workgroup_id) wg: vec3<u32>,
) {
    let h = wg.x;
    let kv_head = h / (p.query_heads / p.kv_heads);
    let qbase = h * p.head_dim;
    let tokens = min(p.tokens, MAX_TOKENS);
    let scale = sqrt(f32(p.head_dim));
    var local_max = -3.4028235e38;
    for (var t = li; t < tokens; t += 64u) {
        let kb = token_base(t, kv_head);
        var s = 0.0;
        for (var d = 0u; d < p.head_dim; d++) {
            s += q[qbase + d] * keys[kb + d];
        }
        s = s / scale;
        scores[t] = s;
        local_max = max(local_max, s);
    }
    partial[li] = local_max;
    workgroupBarrier();
    for (var stride = 32u; stride > 0u; stride = stride >> 1u) {
        if li < stride {
            partial[li] = max(partial[li], partial[li + stride]);
        }
        workgroupBarrier();
    }
    let m = partial[0];
    workgroupBarrier();
    var local_sum = 0.0;
    for (var t = li; t < tokens; t += 64u) {
        let e = exp(scores[t] - m);
        scores[t] = e;
        local_sum += e;
    }
    partial[li] = local_sum;
    workgroupBarrier();
    for (var stride = 32u; stride > 0u; stride = stride >> 1u) {
        if li < stride {
            partial[li] = partial[li] + partial[li + stride];
        }
        workgroupBarrier();
    }
    let total = partial[0];
    for (var t = li; t < tokens; t += 64u) {
        scores[t] = scores[t] / total;
    }
    workgroupBarrier();
    for (var d = li; d < p.head_dim; d += 64u) {
        var acc = 0.0;
        for (var t = 0u; t < tokens; t++) {
            acc += scores[t] * values[token_base(t, kv_head) + d];
        }
        out[qbase + d] = acc;
    }
}
