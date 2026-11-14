// p.n = boxes, p.a = padded length (a power of two).
@group(0) @binding(0) var<storage, read> boxes: array<u32>;
@group(0) @binding(1) var<storage, read_write> keys: array<u32>;
@group(0) @binding(2) var<storage, read_write> ids: array<u32>;

@compute @workgroup_size(64)
fn keys_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x + gid.y * 65535u * 64u;
    if i >= p.a {
        return;
    }
    if i < p.n {
        keys[i] = ord(boxes[i * 6u]);
        ids[i] = i;
    } else {
        keys[i] = 0xffffffffu;
        ids[i] = 0xffffffffu;
    }
}
