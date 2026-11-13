// Compiled as a separate helper by the integration test, with a real static C
// library. All imports use total-bit Rust types and the descriptor's raw ABI.
unsafe extern "C" {
    fn bridge_sum(a: u32, b: u32) -> u32;
    fn bridge_negative() -> i16;
    fn bridge_bool() -> u8;
    fn bridge_state() -> u32;
    fn bridge_address() -> *mut u32;
    fn bridge_copy(src: *const u8, src_len: usize, dst: *mut u8, dst_len: usize) -> usize;
    fn bridge_crc_update(crc: *mut u32, data: *const u8, len: usize);
    fn bridge_split(value: u64, out: *mut [u32; 2]) -> i32;
}
fn main() {
    let src = [1u8, 0, 255, 128];
    let mut dst = [0u8; 4];
    // SAFETY: this controlled fixture implements the exact declared C ABI;
    // pointers reference disjoint live arrays of the specified lengths. Its
    // C code retains no pointers. This does not authorize arbitrary libraries.
    let (sum, negative, bad_bool, state, count, address) = unsafe {
        (
            bridge_sum(20, 22),
            bridge_negative() as u16 as u64,
            bridge_bool(),
            bridge_state(),
            bridge_copy(src.as_ptr(), src.len(), dst.as_mut_ptr(), dst.len()),
            bridge_address() as usize as u64,
        )
    };
    println!("{sum} {negative} {bad_bool} {state} {count} {address}");
    println!("{} {} {} {}", dst[0], dst[1], dst[2], dst[3]);
    let mut crc = 7u32;
    let mut pair = [0u32; 2];
    // SAFETY: `crc` and `pair` are distinct live locals of the declared
    // pointee layouts (u32; wire struct of two native u32); `src` is a live
    // array of the given length. The C code retains none of them.
    let split = unsafe {
        bridge_crc_update(&mut crc, src.as_ptr(), src.len());
        bridge_split(0x0000_0005_0000_0009, &mut pair)
    };
    println!("{crc} {split} {} {}", pair[0], pair[1]);
}
