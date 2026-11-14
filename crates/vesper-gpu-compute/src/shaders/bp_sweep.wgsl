// p.n = boxes, p.a = mode (0 count, 1 write), p.b = pair capacity.
@group(0) @binding(0) var<storage, read> boxes: array<u32>;
@group(0) @binding(1) var<storage, read> keys: array<u32>;
@group(0) @binding(2) var<storage, read> ids: array<u32>;
@group(0) @binding(3) var<storage, read_write> counts: array<u32>;
@group(0) @binding(4) var<storage, read> offsets: array<u32>;
@group(0) @binding(5) var<storage, read_write> pairs: array<u32>;

@compute @workgroup_size(64)
fn sweep_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x + gid.y * 65535u * 64u;
    if i >= p.n {
        return;
    }
    let a = ids[i];
    let max_x = ord(boxes[a * 6u + 3u]);
    let min_y = ord(boxes[a * 6u + 1u]);
    let min_z = ord(boxes[a * 6u + 2u]);
    let max_y = ord(boxes[a * 6u + 4u]);
    let max_z = ord(boxes[a * 6u + 5u]);
    var count = 0u;
    var base = 0u;
    if p.a == 1u {
        base = offsets[i];
    }
    for (var j = i + 1u; j < p.n; j++) {
        if keys[j] > max_x {
            break;
        }
        let b = ids[j];
        let overlap = ord(boxes[b * 6u + 1u]) <= max_y && min_y <= ord(boxes[b * 6u + 4u])
            && ord(boxes[b * 6u + 2u]) <= max_z && min_z <= ord(boxes[b * 6u + 5u]);
        if overlap {
            if p.a == 1u {
                let o = base + count;
                if o < p.b {
                    pairs[o * 2u] = min(a, b);
                    pairs[o * 2u + 1u] = max(a, b);
                }
            }
            count += 1u;
        }
    }
    if p.a == 0u {
        counts[i] = count;
    }
}
