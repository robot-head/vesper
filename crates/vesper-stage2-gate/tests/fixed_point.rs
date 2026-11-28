//! The Stage 2 fixed point (#1397, #2671), booted from the committed seed
//! with no Rust compiler: the seed builds today's `@vesper/stage2`, which
//! must rebuild itself byte for byte (in process, under the pinned wasmtime
//! CLI, and precompiled under the CLI), and every compiler package must come
//! out the same from each Stage 2 of the chain
//! ([`support::packages::check_compiler_packages_fixed_point`]).
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[path = "support/native_object.rs"]
mod native_object;
mod support;

use support::packages::*;
use support::*;

#[test]
fn stage2_self_compilation_fixed_point() {
    with_large_stack(|| {
        let dir = fresh("fixed-point");
        let chain = bootstrap(&dir);
        let last = chain.last();
        let cli = wasmtime_cli_bin();
        let cwasm = dir.join("stage2_last.cwasm");
        if let Some(cli) = &cli {
            let last_path = dir.join("stage2_last.wasm");
            std::fs::write(&last_path, &last.bytes).unwrap();
            for (precompiled, artifact, name) in [
                (false, last_path.clone(), "stage3_cli.wasm"),
                (true, cwasm.clone(), "stage3_cwasm.wasm"),
            ] {
                if precompiled {
                    precompile_stage2_wasmtime_cli(cli, &last_path, &cwasm);
                }
                let out = dir.join(name);
                run_stage2_wasmtime_cli(
                    cli,
                    &artifact,
                    precompiled,
                    &repo(),
                    &dir,
                    STAGE2_PACKAGE,
                    &out,
                );
                let got = std::fs::read(&out).expect("read the CLI's stage 3");
                assert!(
                    got == last.bytes,
                    "Stage 2 wasmtime CLI fixed point mismatch (precompiled: {precompiled}): {} ({} bytes) != {name} ({} bytes)",
                    last.label,
                    last.bytes.len(),
                    got.len()
                );
                remove_stage2_sidecars(&out);
            }
        }
        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        native_object::check_stage2_native_object(last, &dir);
        check_compiler_packages_fixed_point(
            &dir,
            &chain,
            cli.as_deref().map(|c| (c, cwasm.as_path())),
        );
        std::fs::remove_dir_all(dir).unwrap();
    });
}
