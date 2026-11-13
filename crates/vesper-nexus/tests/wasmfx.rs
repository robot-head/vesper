//! WasmFX: exercised where Wasmtime supports stack switching (x86_64 Linux
//! and macOS), reported unavailable with the exact configuration elsewhere.

use vesper_nexus::wasmfx::{PROBE_CONFIG, WasmFxSupport, YIELD_SUM_WAT, probe};

#[test]
fn yield_effect_lowered_to_cont_new_resume_suspend() {
    for instr in ["cont.new", "resume", "suspend", "(tag $yield"] {
        assert!(YIELD_SUM_WAT.contains(instr), "{instr}");
    }
    let supported = cfg!(all(
        target_arch = "x86_64",
        any(target_os = "linux", target_os = "macos")
    ));
    match probe() {
        WasmFxSupport::Exercised { sum } => {
            assert!(supported, "unexpectedly exercised on this target");
            assert_eq!(sum, 1 + 2 + 3);
            eprintln!("WasmFX exercised: handler summed {sum} ({PROBE_CONFIG})");
        }
        WasmFxSupport::Unavailable { config, reason } => {
            assert!(!supported, "WasmFX should run on this target: {reason}");
            eprintln!("WasmFX unavailable with {config}: {reason}");
        }
    }
}
