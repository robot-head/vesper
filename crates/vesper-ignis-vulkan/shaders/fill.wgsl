// `data[i] = 0x01010101` for every invocation inside the bound range.
@group(0) @binding(0) var<storage, read_write> data: array<u32>;

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x < arrayLength(&data)) {
        data[id.x] = 0x01010101u;
    }
}
