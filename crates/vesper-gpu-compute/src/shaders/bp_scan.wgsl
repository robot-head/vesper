// p.n = boxes. Exclusive prefix sum of counts[0..n] into offsets[0..n],
// offsets[n] = total; one workgroup walks 256-element chunks with a carry.
@group(0) @binding(0) var<storage, read> counts: array<u32>;
@group(0) @binding(1) var<storage, read_write> offsets: array<u32>;
var<workgroup> chunk: array<u32, 256>;
var<workgroup> carry: u32;

@compute @workgroup_size(256)
fn scan_main(@builtin(local_invocation_index) li: u32) {
    if li == 0u {
        carry = 0u;
    }
    workgroupBarrier();
    let chunks = (p.n + 255u) / 256u;
    for (var c = 0u; c < chunks; c++) {
        let i = c * 256u + li;
        var v = 0u;
        if i < p.n {
            v = counts[i];
        }
        chunk[li] = v;
        workgroupBarrier();
        // Hillis-Steele inclusive scan.
        for (var d = 1u; d < 256u; d = d << 1u) {
            var add = 0u;
            if li >= d {
                add = chunk[li - d];
            }
            workgroupBarrier();
            chunk[li] = chunk[li] + add;
            workgroupBarrier();
        }
        let base = carry;
        if i < p.n {
            offsets[i] = base + chunk[li] - v;
        }
        workgroupBarrier();
        if li == 255u {
            carry = base + chunk[255];
        }
        workgroupBarrier();
    }
    if li == 0u {
        offsets[p.n] = carry;
    }
}
