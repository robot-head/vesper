//! Compiles the official BLAKE3 C implementation vendored in
//! packages/vesper-std/hash/sys/c (upstream 1.8.7) and its `.vsys` glue
//! `vesper_blake3.c` into one static library, `vesper_std_blake3`, the link
//! name of the `.vsys` library `Blake3`.
//!
//! The SIMD kernels are the C intrinsics files, each with its own ISA flag,
//! chosen as upstream's build does: SSE2, SSE4.1, AVX2 and AVX-512 on
//! x86-64 (picked at run time by `blake3_dispatch.c`), NEON on AArch64, the
//! portable code elsewhere. Every internal `blake3_*` symbol is renamed to
//! `vesper_b3_*` with `-D`, so the library never collides with the copies
//! of the same kernels that the Rust `blake3` crate links.
use std::path::PathBuf;

/// Every global the C sources define, renamed to `vesper_b3_*`.
const RENAMED: &[&str] = &[
    "blake3_compress_in_place",
    "blake3_compress_in_place_avx512",
    "blake3_compress_in_place_portable",
    "blake3_compress_in_place_sse2",
    "blake3_compress_in_place_sse41",
    "blake3_compress_subtree_wide",
    "blake3_compress_xof",
    "blake3_compress_xof_avx512",
    "blake3_compress_xof_portable",
    "blake3_compress_xof_sse2",
    "blake3_compress_xof_sse41",
    "blake3_hash_many",
    "blake3_hash_many_avx2",
    "blake3_hash_many_avx512",
    "blake3_hash_many_neon",
    "blake3_hash_many_portable",
    "blake3_hash_many_sse2",
    "blake3_hash_many_sse41",
    "blake3_hasher_finalize",
    "blake3_hasher_finalize_seek",
    "blake3_hasher_init",
    "blake3_hasher_init_derive_key",
    "blake3_hasher_init_derive_key_raw",
    "blake3_hasher_init_keyed",
    "blake3_hasher_reset",
    "blake3_hasher_update",
    "blake3_simd_degree",
    "blake3_version",
    "blake3_xof_many",
    "blake3_xof_many_avx512",
    "blake3_xof_many_neon",
];

fn target(var: &str) -> String {
    std::env::var(var).unwrap_or_default()
}

fn new_build(dir: &PathBuf) -> cc::Build {
    let mut b = cc::Build::new();
    b.include(dir).opt_level(3).warnings(false);
    if !b.get_compiler().is_like_msvc() {
        b.flag("-std=c11");
    }
    for name in RENAMED {
        let renamed = format!("vesper_b3_{}", &name["blake3_".len()..]);
        b.define(name, Some(renamed.as_str()));
    }
    b
}

fn main() {
    // Bazel names the directory by its header (VESPER_BLAKE3_C_HEADER, an
    // absolute execpath); Cargo finds it beside the crate.
    println!("cargo:rerun-if-env-changed=VESPER_BLAKE3_C_HEADER");
    let dir = match std::env::var_os("VESPER_BLAKE3_C_HEADER") {
        Some(header) => PathBuf::from(header)
            .parent()
            .expect("the header has a directory")
            .to_path_buf(),
        None => PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap())
            .join("../../packages/vesper-std/hash/sys/c"),
    };
    println!("cargo:rerun-if-changed={}", dir.display());
    println!("cargo:rerun-if-changed=build.rs");
    let arch = target("CARGO_CFG_TARGET_ARCH");
    let msvc = target("CARGO_CFG_TARGET_ENV") == "msvc";

    let mut base = new_build(&dir);
    for f in [
        "blake3.c",
        "blake3_dispatch.c",
        "blake3_portable.c",
        "vesper_blake3.c",
    ] {
        base.file(dir.join(f));
    }
    // Kernel objects, archived with the rest into one library.
    let mut objects = Vec::new();
    match arch.as_str() {
        "x86_64" => {
            // One object per ISA, each with only its own flag, so code the
            // CPU may lack is reached only through the run-time dispatch.
            let kernels: [(&str, &[&str], &[&str]); 4] = [
                ("blake3_sse2.c", &["-msse2"], &[]),
                ("blake3_sse41.c", &["-msse4.1"], &[]),
                ("blake3_avx2.c", &["-mavx2"], &["/arch:AVX2"]),
                (
                    "blake3_avx512.c",
                    &["-mavx512f", "-mavx512vl"],
                    &["/arch:AVX512"],
                ),
            ];
            for (file, gnu, ms) in kernels {
                let mut k = new_build(&dir);
                k.file(dir.join(file));
                for flag in if msvc { ms } else { gnu } {
                    k.flag(flag);
                }
                objects.extend(k.compile_intermediates());
            }
        }
        "aarch64" => {
            base.define("BLAKE3_USE_NEON", Some("1"));
            base.file(dir.join("blake3_neon.c"));
        }
        _ => {
            for d in [
                "BLAKE3_NO_SSE2",
                "BLAKE3_NO_SSE41",
                "BLAKE3_NO_AVX2",
                "BLAKE3_NO_AVX512",
            ] {
                base.define(d, Some("1"));
            }
            base.define("BLAKE3_USE_NEON", Some("0"));
        }
    }
    base.objects(objects);
    base.compile("vesper_std_blake3");
}
