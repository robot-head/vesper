// p.a = padded length, p.b = j (partner distance), p.c = k (sequence size).
@group(0) @binding(0) var<storage, read_write> keys: array<u32>;
@group(0) @binding(1) var<storage, read_write> ids: array<u32>;

@compute @workgroup_size(64)
fn sort_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x + gid.y * 65535u * 64u;
    if i >= p.a {
        return;
    }
    let l = i ^ p.b;
    if l <= i {
        return;
    }
    let ki = keys[i];
    let kl = keys[l];
    let ii = ids[i];
    let il = ids[l];
    let greater = ki > kl || (ki == kl && ii > il);
    let ascending = (i & p.c) == 0u;
    if greater == ascending {
        keys[i] = kl;
        keys[l] = ki;
        ids[i] = il;
        ids[l] = ii;
    }
}
