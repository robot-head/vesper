//! Compile the Vesper port into a static library and check the committed
//! binding (see docs/maintenance/porting-a-crate.md). Every build links it:
//! the virtqueue walks are Vesper-only, and `vesper-port` switches the other
//! ported modules to it.
//!
//! The port is one library written as several files (`PARTS`), which this
//! script joins into `$OUT_DIR/aether.ves` in order (BUILD.bazel's
//! `:aether_source` does the same).

/// The files of the port, in order.
const PARTS: &[&str] = &[
    "util",
    "slot_plan",
    "pvh",
    "hotset",
    "packed",
    "vq",
    "seccomp",
    "guest_mem",
    "block",
    "net",
    "vsock",
    "vmgenid",
    "mmio",
];

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    {
        let mut source = String::new();
        for part in PARTS {
            let path = format!("vesper/src/{part}.ves");
            println!("cargo:rerun-if-changed={path}");
            source.push_str(&std::fs::read_to_string(&path).expect("read a port file"));
            source.push('\n');
        }
        let out = std::path::PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
        let joined = out.join("aether.ves");
        std::fs::write(&joined, source).expect("write the joined port");
        let port = vesper_port_harness::Port::new("aether", &joined)
            .binding("port/binding.rs")
            .rust_types("crate::port_types");
        #[cfg(feature = "vesper-port-divergent")]
        let port = DIVERGENT_EDITS
            .iter()
            .fold(port, |port, (from, to)| port.edit(from, to));
        port.build();
    }
    // The hardware random-number instructions: one package per target,
    // all with the same exports (src/hw_rng.rs; BUILD.bazel selects the
    // same way). Other targets have none.
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let rng = match (arch.as_str(), os.as_str()) {
        ("x86_64", _) => Some("x86_64"),
        ("aarch64", "linux") => Some("aarch64-linux"),
        ("aarch64", "macos") => Some("aarch64-macos"),
        _ => None,
    };
    if let Some(dir) = rng {
        vesper_port_harness::Port::new("aether_rng", format!("vesper-rng/{dir}"))
            .binding("port/rng_binding.rs")
            .build();
    }
}

/// One deliberate bug per ported module (mirrored in BUILD.bazel). Each
/// must make its module's tests fail.
#[cfg(feature = "vesper-port-divergent")]
const DIVERGENT_EDITS: &[(&str, &str)] = &[
    // slot_plan: touching ranges count as overlapping.
    (
        "            if start < reach_end {",
        "            if start <= reach_end {",
    ),
    // pvh: a segment may end one byte past the file.
    (
        "        if add_overflows(offset, filesz) || wrapping_add(offset, filesz) > n {",
        "        if add_overflows(offset, filesz) || wrapping_add(offset, filesz) > wrapping_add(n, 1) {",
    ),
    // hotset: equal indices count as increasing.
    (
        "        if have && last >= i {",
        "        if have && last > i {",
    ),
    // packed: a used descriptor counts as available.
    ("        if !avail || used {", "        if !avail {"),
    // vq: a chain may hold one buffer more than the limit.
    ("    if count >= max_desc {", "    if count > max_desc {"),
    // seccomp: allow-list validator accepts an unaligned seccomp_data load offset.
    (
        "        if k % 4 != 0 || k >= SECCOMP_DATA_BYTES {",
        "        if k >= SECCOMP_DATA_BYTES {",
    ),
    // guest_mem: touching regions count as overlapping.
    (
        "        if r > 0 && gpa < prev_end {",
        "        if r > 0 && gpa <= prev_end {",
    ),
    // block: discard accepts unaligned sector/count.
    (
        "            if sector % BLK_DISCARD_SECTOR_ALIGNMENT != 0",
        "            if false && sector % BLK_DISCARD_SECTOR_ALIGNMENT != 0",
    ),
    // net: TX validation accepts oversized frames.
    (
        "    if payload_len > max_frame {",
        "    if false && payload_len > max_frame {",
    ),
    // vsock: peer credit wraps instead of saturating to 0.
    (
        "    if peer_buf_alloc >= used {",
        "    if true || peer_buf_alloc >= used {",
    ),
    // vmgenid: allows an ID that crosses a 4 KiB page boundary.
    (
        "    wrapping_add(in_page, VMGENID_BYTES_LEN) <= VMGENID_PAGE_SIZE",
        "    true || wrapping_add(in_page, VMGENID_BYTES_LEN) <= VMGENID_PAGE_SIZE",
    ),
    // mmio: accepts FEATURES_OK without VIRTIO_F_VERSION_1.
    (
        "|| (driver_features & MMIO_VIRTIO_F_VERSION_1) != MMIO_VIRTIO_F_VERSION_1",
        "|| false",
    ),
];
