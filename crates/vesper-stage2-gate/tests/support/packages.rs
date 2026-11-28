//! The package tables of the Stage 2 gate: every directory under
//! `packages/` is built by Stage 2 as a compiler package, is a known gap, or
//! is covered elsewhere ([`STAGE2_PACKAGES_COVERED_ELSEWHERE`]).
use super::*;

pub enum CompilerExecCheck {
    Exact(u64),
    NonZero,
}

/// Every compiler package Stage 2 builds: (stem, package, path, probe
/// expression, execution check). Each probe is a one-line wrapper package
/// whose `main` calls the expression, so Stage 2 compiles the package and
/// every package it depends on.
pub const COMPILER_PACKAGES: &[(&str, &str, &str, &str, CompilerExecCheck)] = &[
    (
        "lsp-format",
        "@vesper/lsp-format",
        "packages/vesper-lsp-format",
        "lsp_formatting(in zero_input().as_str()).len()",
        CompilerExecCheck::Exact(1),
    ),
    (
        "lsp-publish",
        "@vesper/lsp-publish",
        "packages/vesper-lsp-publish",
        "lsp_diagnostic_payload(in zero_input().as_str()).len()",
        CompilerExecCheck::Exact(1),
    ),
    (
        "front",
        "@vesper/front",
        "packages/vesper-front",
        "parse_tables(in \"fn main() -> u64 { 1 }\").len()",
        CompilerExecCheck::NonZero,
    ),
    (
        "sugar",
        "@vesper/sugar",
        "packages/vesper-sugar",
        "match_rule_words(in \"a $x:expra 1\", 12884901897, 100000).len()",
        CompilerExecCheck::NonZero,
    ),
    (
        "backend-checks",
        "@vesper/backend-checks",
        "packages/vesper-backend-checks",
        "backend_verify_count(in \"\")",
        CompilerExecCheck::Exact(0),
    ),
    (
        "bounds-hoist",
        "@vesper/bounds-hoist",
        "packages/vesper-bounds-hoist",
        "decide_text(in \"\").len()",
        CompilerExecCheck::Exact(1),
    ),
    (
        "check-dyn",
        "@vesper/check-dyn",
        "packages/vesper-check-dyn",
        "check_dyn_count(in \"\", 0, 0)",
        CompilerExecCheck::Exact(0),
    ),
    (
        "check-effects",
        "@vesper/check-effects",
        "packages/vesper-check-effects",
        "check_effects(in \"\").len()",
        CompilerExecCheck::Exact(1),
    ),
    (
        "check-fip",
        "@vesper/check-fip",
        "packages/vesper-check-fip",
        "check_fip_count(in \"\")",
        CompilerExecCheck::Exact(0),
    ),
    (
        "check-loops-atomics",
        "@vesper/check-loops-atomics",
        "packages/vesper-check-loops-atomics",
        "check_loops_control(in \"\").len()",
        CompilerExecCheck::Exact(1),
    ),
    (
        "check-pact",
        "@vesper/check-pact",
        "packages/vesper-check-pact",
        "check_pact_mono_plan(in \"\").len()",
        CompilerExecCheck::Exact(12),
    ),
    (
        "check-regions-seqs",
        "@vesper/check-regions-seqs",
        "packages/vesper-check-regions-seqs",
        "regions_seqs_types(in \"\").len()",
        CompilerExecCheck::Exact(1),
    ),
    (
        "check-tensor-types",
        "@vesper/check-tensor-types",
        "packages/vesper-check-tensor-types",
        "check_tensor_types(in zero_input().as_str()).len()",
        CompilerExecCheck::Exact(3),
    ),
    (
        "codegen-aggregates",
        "@vesper/codegen-aggregates",
        "packages/vesper-codegen-aggregates",
        "aggregate_decisions(in \"\").len()",
        CompilerExecCheck::Exact(1),
    ),
    (
        "codegen-calls",
        "@vesper/codegen-calls",
        "packages/vesper-codegen-calls",
        "codegen_calls_text(in \"\").len()",
        CompilerExecCheck::Exact(1),
    ),
    (
        "codegen-control",
        "@vesper/codegen-control",
        "packages/vesper-codegen-control",
        "control_session_plans(in \"\").len()",
        CompilerExecCheck::Exact(1),
    ),
    (
        "codegen-export",
        "@vesper/codegen-export",
        "packages/vesper-codegen-export",
        "export_plan(in zero_input().as_str()).len()",
        CompilerExecCheck::Exact(4),
    ),
    (
        "codegen-freestanding",
        "@vesper/codegen-freestanding",
        "packages/vesper-codegen-freestanding",
        "freestanding_constants()",
        CompilerExecCheck::NonZero,
    ),
    (
        "codegen-scalar",
        "@vesper/codegen-scalar",
        "packages/vesper-codegen-scalar",
        "codegen_scalar_decisions(in \"\").len()",
        CompilerExecCheck::Exact(1),
    ),
    (
        "codegen-seq",
        "@vesper/codegen-seq",
        "packages/vesper-codegen-seq",
        "codegen_seq_decisions(in \"\").len()",
        CompilerExecCheck::Exact(1),
    ),
    (
        "codegen-walk",
        "@vesper/codegen-walk",
        "packages/vesper-codegen-walk",
        "native_walk_decisions(in \"\").len()",
        CompilerExecCheck::Exact(1),
    ),
    (
        "driver",
        "@vesper/driver",
        "packages/vesper-driver",
        "widen_u64(cmd_check())",
        CompilerExecCheck::Exact(1),
    ),
    (
        "ebpf-backend",
        "@vesper/ebpf-backend",
        "packages/vesper-ebpf-backend",
        "ebpf_backend_words(in \"\").len()",
        CompilerExecCheck::Exact(11),
    ),
    (
        "egraph",
        "@vesper/egraph",
        "packages/vesper-egraph",
        "egraph_math(in \"\").len()",
        CompilerExecCheck::Exact(0),
    ),
    (
        "ffi-lowering",
        "@vesper/ffi-lowering",
        "packages/vesper-ffi-lowering",
        "{ let _ = s_new(); 0 }",
        CompilerExecCheck::Exact(0),
    ),
    (
        "ir-check",
        "@vesper/ir-check",
        "packages/vesper-ir-check",
        "{ let _ = paths_new(); 0 }",
        CompilerExecCheck::Exact(0),
    ),
    (
        "ir-inline",
        "@vesper/ir-inline",
        "packages/vesper-ir-inline",
        "inline_program_words(in \"\".as_bytes()).len()",
        CompilerExecCheck::NonZero,
    ),
    (
        "ir-opt",
        "@vesper/ir-opt",
        "packages/vesper-ir-opt",
        "ir_opt_words(in \"\".as_bytes()).len()",
        CompilerExecCheck::NonZero,
    ),
    (
        "ir-opt-policy",
        "@vesper/ir-opt-policy",
        "packages/vesper-ir-opt-policy",
        "ir_opt_policy_entry(in \"\").len()",
        CompilerExecCheck::NonZero,
    ),
    (
        "optimize",
        "@vesper/optimize",
        "packages/vesper-optimize",
        "optimize_verify(in \"\").len()",
        CompilerExecCheck::Exact(1),
    ),
    (
        "doc",
        "@vesper/doc",
        "packages/vesper-doc",
        "format()",
        CompilerExecCheck::Exact(1),
    ),
    (
        "lsp",
        "@vesper/lsp",
        "packages/vesper-lsp",
        "lsp_frames(in zero_input().as_str()).len()",
        CompilerExecCheck::Exact(3),
    ),
    (
        "lsp-features",
        "@vesper/lsp-features",
        "packages/vesper-lsp-features",
        "lsp_document_symbols(in zero_input().as_str()).len()",
        CompilerExecCheck::Exact(8),
    ),
    (
        "lsp-proof-run",
        "@vesper/lsp-proof-run",
        "packages/vesper-lsp-proof-run",
        "job_obligations(in \"\".as_bytes())",
        CompilerExecCheck::Exact(0),
    ),
    (
        "memo",
        "@vesper/memo",
        "packages/vesper-memo",
        "memo_replay(in \"\").len()",
        CompilerExecCheck::Exact(1),
    ),
    (
        "test-runner",
        "@vesper/test-runner",
        "packages/vesper-test-runner",
        "format()",
        CompilerExecCheck::NonZero,
    ),
    (
        "lean-extract",
        "@vesper/lean-extract",
        "packages/vesper-lean-extract",
        "extract_head_words(in \"\")",
        CompilerExecCheck::NonZero,
    ),
    (
        "check-collect",
        "@vesper/check-collect",
        "packages/vesper-check-collect",
        "collect_text(in \"\").len()",
        CompilerExecCheck::NonZero,
    ),
    (
        "check-world",
        "@vesper/check-world",
        "packages/vesper-check-world",
        "typing_input_words(in \"\").len()",
        CompilerExecCheck::NonZero,
    ),
    (
        "preludes",
        "@vesper/preludes",
        "packages/vesper-preludes",
        "strings_source().as_bytes().len()",
        CompilerExecCheck::NonZero,
    ),
    (
        "check-data",
        "@vesper/check-data",
        "packages/vesper-check-data",
        "check_data_sizes(in \"\").len()",
        CompilerExecCheck::Exact(10),
    ),
    (
        "ast",
        "@vesper/ast",
        "packages/vesper-ast",
        "ast_count(in \"fn main() -> u64 { 1 }\")",
        CompilerExecCheck::NonZero,
    ),
    (
        "check-layout",
        "@vesper/check-layout",
        "packages/vesper-check-layout",
        "check_layout_words(in \"\").len()",
        CompilerExecCheck::Exact(8),
    ),
    (
        "check-core-typing",
        "@vesper/check-core-typing",
        "packages/vesper-check-core-typing",
        "core_typing_exprs(in \"\").len()",
        CompilerExecCheck::Exact(1),
    ),
    (
        "check-names-packages",
        "@vesper/check-names-packages",
        "packages/vesper-check-names-packages",
        "module_count(in \"\")",
        CompilerExecCheck::Exact(0),
    ),
    (
        "place-ir",
        "@vesper/place-ir",
        "packages/vesper-place-ir",
        "place_ir_words(in \"\").len()",
        CompilerExecCheck::NonZero,
    ),
    (
        "place-ir-lower",
        "@vesper/place-ir-lower",
        "packages/vesper-place-ir-lower",
        "ir_lower_words(in \"\").len()",
        CompilerExecCheck::NonZero,
    ),
    (
        "vsys",
        "@vesper/vsys",
        "packages/vesper-vsys",
        "run_vsys_check(in \"\").len()",
        CompilerExecCheck::Exact(2),
    ),
    (
        "clif",
        "@vesper/clif",
        "packages/vesper-clif",
        "inc(1)",
        CompilerExecCheck::Exact(2),
    ),
    (
        "cranelift",
        "@vesper/cranelift",
        "packages/vesper-cranelift",
        "{ let _ = ty_i8(); 0 }",
        CompilerExecCheck::Exact(0),
    ),
    (
        "crypto",
        "@vesper/crypto",
        "packages/vesper-crypto",
        "zeros(4).len()",
        CompilerExecCheck::Exact(4),
    ),
    (
        "native-clif-opt",
        "@vesper/native-clif-opt",
        "packages/vesper-native-clif-opt",
        "clif_opt_words(in \"\").len()",
        CompilerExecCheck::Exact(1),
    ),
    (
        "native-cold-outline",
        "@vesper/native-cold-outline",
        "packages/vesper-native-cold-outline",
        "outline_words(in \"\").len()",
        CompilerExecCheck::Exact(1),
    ),
    (
        "library-ir",
        "@vesper/library-ir",
        "packages/vesper-library-ir",
        "library_ir_text(in \"\").len()",
        CompilerExecCheck::NonZero,
    ),
    (
        "native-vectorize",
        "@vesper/native-vectorize",
        "packages/vesper-native-vectorize",
        "vectorize_words(in \"\").len()",
        CompilerExecCheck::Exact(2),
    ),
    (
        "verified-codegen",
        "@vesper/verified-codegen",
        "packages/vesper-verified-codegen",
        "select_text(in \"\").len()",
        CompilerExecCheck::Exact(1),
    ),
    (
        "proof-audit",
        "@vesper/proof-audit",
        "packages/vesper-proof-audit",
        "HELP.len()",
        CompilerExecCheck::NonZero,
    ),
    (
        "run",
        "@vesper/run",
        "packages/vesper-run",
        "run_words(in \"\".as_bytes()).len()",
        CompilerExecCheck::Exact(6),
    ),
    (
        "verify",
        "@vesper/verify",
        "packages/vesper-verify",
        "discover(in \"\".as_bytes()).err.len()",
        CompilerExecCheck::Exact(0),
    ),
    (
        "wasm-backend",
        "@vesper/wasm-backend",
        "packages/vesper-wasm-backend",
        "wasm_backend_compile(in \"\").len()",
        CompilerExecCheck::Exact(4),
    ),
    (
        "wit-component",
        "@vesper/wit-component",
        "packages/vesper-wit-component",
        "wit_component_wrap(in \"\").len()",
        CompilerExecCheck::Exact(2),
    ),
];

/// Compiler packages over `Ffi[Cranelift]` (native Cranelift glue over
/// vesper-cranelift-sys): (stem, package, path, probe `src/lib.ves`). Stage 2
/// declares the `Ffi$Cranelift` operations from cranelift.vsys
/// (packages/vesper-stage2/src/vcl_decls.ves), and the Wasm backend lowers
/// them to imports of `vesper:cranelift/vcl@0.1.0` (#2210). The fixed point
/// only builds them: running one needs the Cranelift component composed in
/// (crates/vesper-cranelift-component/tests/wasm_imports.rs).
pub const STAGE2_CRANELIFT_PACKAGES: &[(&str, &str, &str, &str)] = &[
    (
        "codegen-base",
        "@vesper/codegen-base",
        "packages/vesper-codegen-base",
        "use @vesper/codegen-base::word_of;\nuse @vesper/codegen-base::arg_value;\npub fn main() -> u64 eff [Ffi[Cranelift]] { word_of(arg_value(0, 0)) }\n",
    ),
    (
        "codegen-control-emit",
        "@vesper/codegen-control-emit",
        "packages/vesper-codegen-control-emit",
        "use @vesper/codegen-control-emit::control_hook;\npub fn main() -> u64 eff [Ffi[Cranelift]] { control_hook(0) }\n",
    ),
    (
        "codegen-numeric",
        "@vesper/codegen-numeric",
        "packages/vesper-codegen-numeric",
        "use @vesper/codegen-numeric::numeric_hook;\npub fn main() -> u64 eff [Ffi[Cranelift]] { numeric_hook(0) }\n",
    ),
    (
        "native-lib",
        "@vesper/native-lib",
        "packages/vesper-native-lib",
        "use @vesper/native-lib::library_object;\npub fn main() -> u64 eff [Ffi[Cranelift]] { let req: ChunkSeq[u8] = ChunkSeq::new[u8](); library_object(in req.as_slice()).len() }\n",
    ),
];

/// Compiler packages over the WASI host libraries (StdFs, StdEnv,
/// StdProcess, Blake3 and HybridCommands, whose wasm32-wasip2 sources import
/// nothing or WASI calls): (stem, package, path, probe `src/lib.ves`, the
/// probe's exit code). Both Stage 2 compilers must build each one to the same
/// bytes, and the probe must run to that code. @vesper/driver-commands left
/// the gaps when its engine calls moved to @vesper/driver-engines (#1996):
/// `vesper lsp` there is refused, prints its refusal to WASI stderr (#2507),
/// and exits with [`DRIVER_REFUSAL_EXIT`].
pub const STAGE2_HOST_PACKAGES: &[(&str, &str, &str, &str, u64)] = &[(
    "driver-commands",
    "@vesper/driver-commands",
    "packages/vesper-driver-commands",
    "use @vesper/driver-commands::run;\nuse @vesper/driver::{args_new, args_push};\npub fn main() -> u64 eff [Ffi[StdFs], Ffi[StdEnv], Ffi[StdProcess], Ffi[Blake3], Ffi[HybridCommands]] { let mut a = args_new(); args_push(inout a, in \"lsp\".as_bytes()); wrapping_u64(run(in a)) }\n",
    DRIVER_REFUSAL_EXIT,
)];

/// The exit code of @vesper/driver-commands when a command is refused on
/// wasm32-wasip2 and printed cleanly to stderr (#2507).
pub const DRIVER_REFUSAL_EXIT: u64 = 1;

/// Compiler packages Stage 2 cannot build yet: (stem, package, path, probe
/// `src/lib.ves`, reason). `stage2_self_compilation_fixed_point` checks that
/// each one still fails, so a gap that closes moves into [`COMPILER_PACKAGES`]
/// or [`STAGE2_HOST_PACKAGES`]. None is left since #1996.
pub const STAGE2_PACKAGE_GAPS: &[(&str, &str, &str, &str, &str)] = &[];

/// Directories under `packages/` that are in none of [`COMPILER_PACKAGES`],
/// [`STAGE2_CRANELIFT_PACKAGES`] and [`STAGE2_PACKAGE_GAPS`], with where they
/// are covered instead.
pub const STAGE2_PACKAGES_COVERED_ELSEWHERE: &[(&str, &str)] = &[
    (
        "vesper-compiler",
        "Stage 1 front end, checked as one unit; bootstrap/stage1.lock (tests/stage2_seed_runner.rs)",
    ),
    (
        "vesper-driver-engines",
        "the command engines still in Rust (#1996), built under Stage 2 as a dependency of the @vesper/driver-commands probe in STAGE2_HOST_PACKAGES (src-wasip2 refuses each engine)",
    ),
    (
        "vesper-export-build",
        "`vesper build --lib`'s interface and binding from Stage 2's front end (#2270) as a native executable beside the Rust CLI (tests/self_host_codegen_export.rs); Stage 2 builds it inside @vesper/port-builder (//:vesper-port-builder.wasm, #2666)",
    ),
    (
        "vesper-port-builder",
        "`vesper build --lib` for the ports as one program: Stage 2 builds it into //:vesper-port-builder.wasm (tools/bazel/stage2_port_builder.bzl), composed with the Cranelift component and compared with the Rust CLI by crates/vesper-port-harness/tests/stage2_builder.rs (#2666)",
    ),
    (
        "vesper-stage2",
        "Stage 2 itself: stage2.wasm == stage3.wasm",
    ),
    (
        "vesper-std",
        "standard library packages, built as dependencies of the probes",
    ),
];

pub fn compiler_package_probe(
    stem: &str,
    pkg_name: &str,
    pkg_rel: &str,
    expr: &str,
) -> (String, String) {
    let dep_guest = format!("/repo/{pkg_rel}");
    let manifest = format!(
        "[package]\nname = \"@vesper-test/probe-{stem}\"\nversion = \"0.1.0\"\nedition = \"2026\"\n\n[dependencies]\n\"{pkg_name}\" = {{ version = \"^0.1\", path = \"{dep_guest}\" }}\n"
    );
    let fn_name = expr
        .split(&['(', ' '][..])
        .find(|s| !s.is_empty() && *s != "{" && *s != "let" && *s != "_" && *s != "=")
        .unwrap_or("");
    let use_line = if expr.starts_with("widen_u64(") {
        format!("use {pkg_name}::cmd_check;")
    } else if expr.starts_with('{') {
        let inner_fn = expr.split("= ").nth(1).unwrap().split('(').next().unwrap();
        format!("use {pkg_name}::{inner_fn};")
    } else {
        format!("use {pkg_name}::{fn_name};")
    };
    let helper = if expr.contains("zero_input()") {
        "fn zero_input() -> String { let mut z = String::new(); for _ in 0..256 { z.push_char(0); } z }\n"
    } else {
        ""
    };
    let lib_src = format!("{use_line}\n{helper}pub fn main() -> u64 {{ {expr} }}\n");
    (manifest, lib_src)
}

/// Runs Stage 2 under the wasmtime CLI with the repository mapped at both
/// `.` and `/repo`, so a probe wrapper under `/out` resolves its compiler
/// package dependency the same way it does in process.
pub fn run_stage2_probe_wasmtime_cli(
    cli: &Path,
    stage2_cwasm: &Path,
    out_dir: &Path,
    out_wasm: &Path,
) {
    remove_stage2_sidecars(out_wasm);
    let out_name = out_wasm
        .file_name()
        .expect("out filename")
        .to_string_lossy()
        .into_owned();
    let exec = std::process::Command::new(cli)
        .args(["run", "--allow-precompiled"])
        .arg("--dir")
        .arg(format!("{}::.", repo().display()))
        .arg("--dir")
        .arg(format!("{}::/repo", repo().display()))
        .arg("--dir")
        .arg(format!("{}::/out", out_dir.display()))
        .args(["--env", "VESPER_STAGE2_SYMS=1"])
        .args(["--argv0", "stage2"])
        .arg(stage2_cwasm)
        .args(["/out/wrap", &format!("/out/{out_name}"), "main"])
        .output()
        .expect("run stage2 under wasmtime CLI");
    let notes = read_stage2_sidecar_notes(out_wasm);
    assert!(
        exec.status.success(),
        "wasmtime CLI stage2 failed (status={}):\nstdout={}\nstderr={}{notes}",
        exec.status,
        String::from_utf8_lossy(&exec.stdout),
        String::from_utf8_lossy(&exec.stderr),
    );
}

/// Builds the probe in `<dir>/wrap` with `compiler` into `<dir>/out.wasm`
/// and returns the component bytes.
pub fn build_compiler_package_probe(compiler: &Compiler, dir: &Path) -> Result<Vec<u8>, String> {
    let out_wasm = dir.join("out.wasm");
    compiler.build(dir, &["/out/wrap", "/out/out.wasm"], &out_wasm)
}

/// The manifest of a probe given as a whole `src/lib.ves`: the package
/// itself, and each other compiler package the probe `use`s to reach the
/// code it builds.
pub fn source_probe_manifest(stem: &str, pkg_name: &str, pkg_rel: &str, lib_src: &str) -> String {
    let (mut manifest, _) = compiler_package_probe(stem, pkg_name, pkg_rel, "");
    for other in lib_src
        .split("use @vesper/")
        .skip(1)
        .filter_map(|rest| rest.split("::").next())
        .filter(|other| format!("@vesper/{other}") != *pkg_name)
    {
        manifest.push_str(&format!(
            "\"@vesper/{other}\" = {{ version = \"^0.1\", path = \"/repo/packages/vesper-{other}\" }}\n"
        ));
    }
    manifest
}

pub fn write_probe(wrap_dir: &Path, manifest: &str, lib_src: &str) {
    let _ = std::fs::remove_dir_all(wrap_dir);
    std::fs::create_dir_all(wrap_dir.join("src")).unwrap();
    std::fs::write(wrap_dir.join("Loom.toml"), manifest).unwrap();
    std::fs::write(wrap_dir.join("src/lib.ves"), lib_src).unwrap();
}

/// @vesper-std/process on WASI (src-wasip2): a start is refused with
/// Unsupported and `id` is 0.
const PROCESS_PROBE_MANIFEST: &str = "[package]\nname = \"@vesper-test/probe-process-wasi\"\nversion = \"0.1.0\"\nedition = \"2026\"\n\n[dependencies]\n\"@vesper-std/process\" = { version = \"^0.1\", path = \"/repo/packages/vesper-std/process\" }\n";
const PROCESS_PROBE_SOURCE: &str = "use @vesper-std/process::{command, arg, spawn, release, id};\n\
        pub fn main() -> u64 eff [Ffi[StdProcess]] {\n\
            let mut c = command(in \"cc\");\n\
            arg(inout c, in \"--version\");\n\
            let r = match spawn(c) {\n\
                Result::Ok(ch) => {\n\
                    release(ch);\n\
                    1000\n\
                },\n\
                Result::Err(e) => widen_u64(e.kind),\n\
            };\n\
            wrapping_add(r, id())\n\
        }\n";
/// `IoKind::Unsupported`, what the process probe answers.
const PROCESS_PROBE_ANSWER: u64 = 11;

/// @vesper-std/hash-sys on WASI (src-wasip2, #2323): delegates Ffi[Blake3]
/// to portable @vesper-std/hash::blake3 with zero Wasm imports.
const HASH_PROBE_MANIFEST: &str = "[package]\nname = \"@vesper-test/probe-hash-sys-wasi\"\nversion = \"0.1.0\"\nedition = \"2026\"\n\n[dependencies]\n\"@vesper-std/hash-sys\" = { version = \"^0.1\", path = \"/repo/packages/vesper-std/hash/sys\" }\n";
const HASH_PROBE_SOURCE: &str = "use @vesper-std/hash-sys::{hash, derive_key_hash, hasher_new, hasher_new_keyed, hasher_new_derive_key, update, finalize, finalize_xof, hasher_free};\n\
        pub fn main() -> u64 eff [Ffi[Blake3]] {\n\
            let d0 = hash(in \"\".as_bytes());\n\
            let d1 = match hasher_new() {\n\
                Option::Some(h0) => {\n\
                    let mut h = h0;\n\
                    update(inout h, in \"\".as_bytes());\n\
                    let mut xof = ChunkSeq::new[u8]();\n\
                    finalize_xof(in h, 4, inout xof);\n\
                    let d = finalize(in h);\n\
                    hasher_free(h);\n\
                    if xof.len() != 4 { return 1; }\n\
                    d\n\
                },\n\
                Option::None => return 2,\n\
            };\n\
            if d0.w0 != d1.w0 || d0.w1 != d1.w1 || d0.w2 != d1.w2 || d0.w3 != d1.w3 { return 3; }\n\
            let bad_key = match hasher_new_keyed(in \"short\".as_bytes()) {\n\
                Option::Some(h) => { hasher_free(h); false },\n\
                Option::None => true,\n\
            };\n\
            if !bad_key { return 4; }\n\
            let mut bad_utf8 = ChunkSeq::new[u8]();\n\
            bad_utf8.push(255);\n\
            let mut dk_out = ChunkSeq::new[u8]();\n\
            if derive_key_hash(in bad_utf8.as_slice(), in \"x\".as_bytes(), 8, inout dk_out) { return 5; }\n\
            if !derive_key_hash(in \"ctx\".as_bytes(), in \"x\".as_bytes(), 8, inout dk_out) { return 6; }\n\
            if dk_out.len() != 8 { return 7; }\n\
            let ok_dk = match hasher_new_derive_key(in \"ctx\".as_bytes()) {\n\
                Option::Some(h) => { hasher_free(h); true },\n\
                Option::None => false,\n\
            };\n\
            if !ok_dk { return 8; }\n\
            d0.w0 & 255\n\
        }\n";
/// BLAKE3("")[0] (0xaf), what the hash probe answers.
const HASH_PROBE_ANSWER: u64 = 175;

/// Probes of the standard library's WASI sources that run as well as build:
/// (label, manifest, `src/lib.ves`, answer).
pub const WASI_PROBES: &[(&str, &str, &str, u64)] = &[
    (
        "@vesper-std/process (WASI probe)",
        PROCESS_PROBE_MANIFEST,
        PROCESS_PROBE_SOURCE,
        PROCESS_PROBE_ANSWER,
    ),
    (
        "@vesper-std/hash-sys (WASI probe)",
        HASH_PROBE_MANIFEST,
        HASH_PROBE_SOURCE,
        HASH_PROBE_ANSWER,
    ),
];

/// The Stage 2 fixed point over every compiler package (#1928, part of
/// #1397).
///
/// `stage2.wasm == stage3.wasm` shows that `@vesper/stage2` rebuilds itself.
/// This extends the fixed point to the packages the compiler is made of:
/// each one in [`COMPILER_PACKAGES`] is built by stage A (Stage 2 compiled
/// by the committed seed) and, when the seed is stale, by stage B (Stage 2
/// compiled by itself), and the components must be byte-identical. A and B
/// then come from different builds of the Wasm backend, so equal bytes mean
/// neither miscompiled Stage 2 in a way these packages exercise. With the
/// pinned wasmtime CLI (always present under Bazel), the precompiled final
/// stage rebuilds every package again and must match too.
///
/// Each package in [`STAGE2_CRANELIFT_PACKAGES`], [`STAGE2_HOST_PACKAGES`]
/// and [`WASI_PROBES`] is built the same way (a host package's probe and a
/// WASI probe also run). Each package in [`STAGE2_PACKAGE_GAPS`] must still
/// fail to build, so a gap that closes is moved into [`COMPILER_PACKAGES`]
/// instead of going unnoticed.
pub fn check_compiler_packages_fixed_point(dir: &Path, chain: &Chain, cli: Option<(&Path, &Path)>) {
    let wrap_dir = dir.join("wrap");
    let out_wasm = dir.join("out.wasm");
    let mut failures = Vec::new();
    // Builds the probe in `wrap_dir` with every compiler of the chain; the
    // bytes of the first, if they all agree.
    let build_all = |pkg_name: &str, failures: &mut Vec<String>| -> Option<Vec<u8>> {
        let first = match build_compiler_package_probe(&chain.a, dir) {
            Ok(b) => b,
            Err(e) => {
                failures.push(format!("{pkg_name}: {e}"));
                return None;
            }
        };
        let mut agree = true;
        if let Some(b) = &chain.b {
            match build_compiler_package_probe(b, dir) {
                Ok(bytes) if bytes == first => {}
                Ok(bytes) => {
                    agree = false;
                    failures.push(format!(
                        "{pkg_name}: {} built {} bytes, {} built {} different bytes",
                        chain.a.label,
                        first.len(),
                        b.label,
                        bytes.len()
                    ));
                }
                Err(e) => {
                    agree = false;
                    failures.push(format!("{pkg_name}: {e}"));
                }
            }
        }
        if let Some((cli, cwasm)) = cli {
            run_stage2_probe_wasmtime_cli(cli, cwasm, dir, &out_wasm);
            let by_cli = std::fs::read(&out_wasm).unwrap_or_default();
            if by_cli != first {
                agree = false;
                failures.push(format!(
                    "{pkg_name}: {} built {} bytes, the precompiled last stage under the wasmtime CLI built {} different bytes",
                    chain.a.label,
                    first.len(),
                    by_cli.len()
                ));
            }
        }
        agree.then_some(first)
    };
    for (stem, pkg_name, pkg_rel, expr, _) in COMPILER_PACKAGES.iter() {
        let (manifest, lib_src) = compiler_package_probe(stem, pkg_name, pkg_rel, expr);
        write_probe(&wrap_dir, &manifest, &lib_src);
        build_all(pkg_name, &mut failures);
    }
    for (stem, pkg_name, pkg_rel, lib_src) in STAGE2_CRANELIFT_PACKAGES.iter() {
        write_probe(
            &wrap_dir,
            &source_probe_manifest(stem, pkg_name, pkg_rel, lib_src),
            lib_src,
        );
        build_all(pkg_name, &mut failures);
    }
    for (stem, pkg_name, pkg_rel, lib_src, code) in STAGE2_HOST_PACKAGES.iter() {
        write_probe(
            &wrap_dir,
            &source_probe_manifest(stem, pkg_name, pkg_rel, lib_src),
            lib_src,
        );
        if let Some(bytes) = build_all(pkg_name, &mut failures) {
            match run(&bytes, &Ctx::default()) {
                Ok(got) if got == *code => {}
                other => failures.push(format!(
                    "{pkg_name}: expected the probe to exit with {code}, got {other:?}"
                )),
            }
        }
    }
    for (label, manifest, lib_src, answer) in WASI_PROBES.iter() {
        write_probe(&wrap_dir, manifest, lib_src);
        if let Some(bytes) = build_all(label, &mut failures) {
            match run(&bytes, &Ctx::default()) {
                Ok(got) if got == *answer => {}
                other => failures.push(format!("{label}: expected {answer}, got {other:?}")),
            }
        }
    }
    let last = chain.last();
    for (stem, pkg_name, pkg_rel, lib_src, why) in STAGE2_PACKAGE_GAPS.iter() {
        write_probe(
            &wrap_dir,
            &source_probe_manifest(stem, pkg_name, pkg_rel, lib_src),
            lib_src,
        );
        match build_compiler_package_probe(last, dir) {
            Ok(_) => failures.push(format!(
                "{pkg_name}: Stage 2 now builds it (listed as a gap: {why}); move it from STAGE2_PACKAGE_GAPS into COMPILER_PACKAGES"
            )),
            // Every gap is in the Wasm backend: loading, assembly and the
            // checks must pass.
            Err(e) if !e.contains("[wasm-backend WErr") => failures.push(format!(
                "{pkg_name}: a gap fails before the Wasm backend (listed as: {why}): {e}"
            )),
            Err(_) => {}
        }
    }
    remove_stage2_sidecars(&out_wasm);
    let _ = std::fs::remove_dir_all(&wrap_dir);
    assert!(
        failures.is_empty(),
        "Stage 2 package fixed point broken for {} of {} packages:\n{}",
        failures.len(),
        COMPILER_PACKAGES.len()
            + STAGE2_CRANELIFT_PACKAGES.len()
            + STAGE2_HOST_PACKAGES.len()
            + WASI_PROBES.len()
            + STAGE2_PACKAGE_GAPS.len(),
        failures.join("\n")
    );
}

/// `f32` arithmetic, comparisons, `%` and negation, `f32` literals (also on
/// the left of an operator, and as a bits operand) and literal patterns, `widen_f64`, `nearest_f32`/`nearest_f64` from floats
/// and integers, `to_bits_*`/`from_bits_*`, and `bit_*` on several kinds
/// (#2375), folded into one word.
pub const STAGE2_FLOAT_SMOKE: &str = "\
fn mix(acc: u64, x: u64) -> u64 { wrapping_add(wrapping_mul(acc, 1000003), x) }
fn b32(x: f32) -> u64 { widen_u64(to_bits_f32(x)) }
fn b64(x: f64) -> u64 { to_bits_f64(x) }
fn is_half(x: f32) -> u64 { match x { 0.5 => 1, _ => 0 } }
fn eq32(a: f32, b: f32) -> u64 { if a == b { 1 } else { 0 } }
pub fn main() -> u64 {
    let a: f32 = 1.1;
    let b: f32 = 3.0;
    let tiny = from_bits_f32(1);
    let nan = from_bits_f32(2143289344);
    let neg_zero: f32 = -0.0;
    let mut h: u64 = 7;
    h = mix(h, b32(a + b));
    h = mix(h, b32(a - b));
    h = mix(h, b32(a * b));
    h = mix(h, b32(a / b));
    h = mix(h, b32(b % a));
    h = mix(h, b32(-a));
    h = mix(h, b32(tiny * b));
    h = mix(h, if a < b { 1 } else { 2 });
    h = mix(h, if a >= b { 3 } else { 4 });
    h = mix(h, eq32(nan, nan));
    h = mix(h, eq32(neg_zero, 0.0));
    h = mix(h, is_half(0.5));
    h = mix(h, is_half(a));
    h = mix(h, b64(widen_f64(a)));
    h = mix(h, if 0.5 == from_bits_f32(1056964608) { 5 } else { 6 });
    h = mix(h, b32(2.0 * b));
    h = mix(h, b32(-1.5 + a));
    h = mix(h, widen_u64(to_bits_f32(1.0)));
    h = mix(h, b64(from_bits_f64(4607182418800017408) + 1.0));
    h = mix(h, b32(nearest_f32(0.1)));
    h = mix(h, b32(nearest_f32(16777217)));
    let big: u64 = 18446744073709551615;
    let neg: i64 = -9007199254740993;
    h = mix(h, b32(nearest_f32(big)));
    h = mix(h, b64(nearest_f64(big)));
    h = mix(h, b64(nearest_f64(neg)));
    h = mix(h, b32(nearest_f32(neg)));
    h = mix(h, b64(from_bits_f64(4614253070214989087) * 2.0));
    let x: u32 = 2863311530;
    let y: u32 = 4042322160;
    h = mix(h, widen_u64(bit_and(x, y)));
    h = mix(h, widen_u64(bit_or(x, y)));
    h = mix(h, widen_u64(bit_xor(x, y)));
    h = mix(h, widen_u64(bit_shl(x, 36)));
    h = mix(h, widen_u64(bit_shr(x, 33)));
    let s: i64 = -12345;
    h = mix(h, wrapping_u64(bit_shr(s, 65)));
    h = mix(h, wrapping_u64(bit_shl(s, 3)));
    let k: u8 = 200;
    h = mix(h, widen_u64(bit_shl(k, 9)));
    h
}
";
