// Compiled as a separate helper by the integration test, with a real static C
// library. All imports use total-bit Rust types and the descriptor's raw ABI.
unsafe extern "C" {
    fn bridge_sum(a: u32, b: u32) -> u32;
    fn bridge_negative() -> i16;
    fn bridge_bool() -> u8;
    fn bridge_state() -> u32;
    fn bridge_address() -> *mut u32;
    fn bridge_copy(src: *const u8, src_len: usize, dst: *mut u8, dst_len: usize) -> usize;
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
}
