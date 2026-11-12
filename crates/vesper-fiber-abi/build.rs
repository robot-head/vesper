//! Platform gate for the direct pinned-fiber modules.
//!
//! `vesper_direct_fibers` is set where a context switch, pinned stack and
//! floating-point environment implementation exists: Linux x86_64/AArch64
//! (System V / AAPCS64, ELF) and macOS AArch64 (Apple arm64 ABI, Mach-O).
//! The Pulse scheduler adapter stays Linux-only and is gated separately;
//! this flag says nothing about io_uring or scheduler portability.
fn main() {
    println!("cargo::rustc-check-cfg=cfg(vesper_direct_fibers)");
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let supported = matches!(
        (os.as_str(), arch.as_str()),
        ("linux", "x86_64") | ("linux", "aarch64") | ("macos", "aarch64")
    );
    if supported {
        println!("cargo::rustc-cfg=vesper_direct_fibers");
    }
    println!("cargo::rerun-if-changed=build.rs");
}
