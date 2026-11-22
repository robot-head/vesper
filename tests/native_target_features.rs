//! `--target-features`: an explicit-target build uses the CPU features it
//! is given (Rust's names, as cargo's `CARGO_CFG_TARGET_FEATURE`), so the
//! port harness builds a port's object for the same CPU as the Rust it is
//! measured against. Its own test binary: the setting is process-wide.
use vesper::{check, native, parser};

fn enabled(isa: &dyn cranelift_codegen::isa::TargetIsa, name: &str) -> bool {
    isa.isa_flags()
        .iter()
        .any(|v| v.name == name && v.as_bool() == Some(true))
}

#[test]
fn explicit_targets_use_the_given_features() {
    let p = parser::parse("fn main() -> u64 { 0 }");
    let c = check::check(&p.module);
    assert!(c.diagnostics.is_empty(), "{:?}", c.diagnostics);
    let x86 = Some("x86_64-unknown-linux-gnu");
    // The baseline: no SSSE3, as rustc's x86-64 default.
    let isa = native::object_isa_optimized(&c.world, x86, true).unwrap();
    assert!(!enabled(&*isa, "has_ssse3"));
    assert!(!enabled(&*isa, "has_avx2"));
    // Cargo's list under `-C target-cpu=native` on an AVX2 machine; names
    // Cranelift has no setting for are ignored.
    native::set_target_features("fxsr,sse,sse2,ssse3,sse4.1,avx,avx2,xsave,lzcnt");
    let isa = native::object_isa_optimized(&c.world, x86, true).unwrap();
    for flag in ["has_ssse3", "has_sse41", "has_avx", "has_avx2", "has_lzcnt"] {
        assert!(enabled(&*isa, flag), "{flag}");
    }
    assert!(!enabled(&*isa, "has_bmi2"));
    // Another architecture takes only its own features.
    let arm = native::object_isa_optimized(&c.world, Some("aarch64-unknown-linux-gnu"), true);
    assert!(arm.is_ok());
    native::set_target_features("");
    let isa = native::object_isa_optimized(&c.world, x86, true).unwrap();
    assert!(!enabled(&*isa, "has_ssse3"));
}
