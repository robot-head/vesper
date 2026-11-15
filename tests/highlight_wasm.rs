//! `vesper-chroma.wasm` (§43.1 item 4): the `vesper-highlight` crate built
//! for `wasm32-unknown-unknown` as a `cdylib` with a size profile, run under
//! Wasmtime with no imports, compared record-for-record with the native
//! build, and measured (stripped size, re-highlight time for a 4 KiB edit).
//!
//! The module comes from `VESPER_CHROMA_WASM` if set; otherwise the test
//! builds it with the Cargo that runs it, when the `wasm32-unknown-unknown`
//! standard library is installed. Without either (Bazel, or a toolchain
//! without the target) it reports a skip.
use std::{
    path::{Path, PathBuf},
    process::Command,
    time::Instant,
};
use vesper_highlight::{highlight_tier0_tokens, wire};
use wasmtime::{Engine, Instance, Memory, Module, Store, TypedFunc};

/// The design's size target for the module.
const TARGET_BYTES: usize = 26 * 1024;
/// Regression ceiling for the measured module; the size target is not met.
const CEILING_BYTES: usize = 96 * 1024;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn build_module() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("VESPER_CHROMA_WASM") {
        return Some(PathBuf::from(path));
    }
    let Ok(cargo) = std::env::var("CARGO") else {
        eprintln!("CARGO unset (not run by Cargo); vesper-chroma.wasm build skipped");
        return None;
    };
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let libdir = Command::new(&rustc)
        .args([
            "--print",
            "target-libdir",
            "--target",
            "wasm32-unknown-unknown",
        ])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()));
    if !libdir.as_deref().is_some_and(Path::is_dir) {
        eprintln!(
            "wasm32-unknown-unknown standard library not installed; vesper-chroma.wasm skipped"
        );
        return None;
    }
    let target_dir = option_env!("CARGO_TARGET_TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("vesper-chroma-wasm");
    let status = Command::new(cargo)
        .current_dir(root())
        .args([
            "rustc",
            "--locked",
            "--quiet",
            "-p",
            "vesper-highlight",
            "--lib",
            "--release",
            "--target",
            "wasm32-unknown-unknown",
            "--crate-type",
            "cdylib",
        ])
        .env("CARGO_TARGET_DIR", &target_dir)
        .env("CARGO_PROFILE_RELEASE_OPT_LEVEL", "z")
        .env("CARGO_PROFILE_RELEASE_LTO", "true")
        .env("CARGO_PROFILE_RELEASE_CODEGEN_UNITS", "1")
        .env("CARGO_PROFILE_RELEASE_PANIC", "abort")
        .env("CARGO_PROFILE_RELEASE_STRIP", "true")
        .env("CARGO_PROFILE_RELEASE_DEBUG", "0")
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .status()
        .expect("run cargo");
    assert!(status.success(), "building vesper-chroma.wasm failed");
    Some(target_dir.join("wasm32-unknown-unknown/release/vesper_highlight.wasm"))
}

struct Chroma {
    store: Store<()>,
    memory: Memory,
    alloc: TypedFunc<u32, u32>,
    highlight: TypedFunc<(u32, u32), u32>,
    output: TypedFunc<(), u32>,
    error: TypedFunc<(), u32>,
}

impl Chroma {
    fn new(bytes: &[u8]) -> Self {
        let engine = Engine::default();
        let module = Module::new(&engine, bytes).expect("valid module");
        let imports: Vec<_> = module
            .imports()
            .map(|i| format!("{}::{}", i.module(), i.name()))
            .collect();
        assert!(imports.is_empty(), "no WASI or host imports: {imports:?}");
        let mut store = Store::new(&engine, ());
        let instance = Instance::new(&mut store, &module, &[]).expect("instantiate");
        let memory = instance
            .get_memory(&mut store, "memory")
            .expect("memory export");
        let alloc = instance.get_typed_func(&mut store, "chroma_alloc").unwrap();
        let highlight = instance.get_typed_func(&mut store, "highlight").unwrap();
        let output = instance
            .get_typed_func(&mut store, "chroma_output_ptr")
            .unwrap();
        let error = instance.get_typed_func(&mut store, "chroma_error").unwrap();
        Self {
            store,
            memory,
            alloc,
            highlight,
            output,
            error,
        }
    }

    /// Records for `src`, or the error code.
    fn run(&mut self, src: &[u8]) -> Result<Vec<u8>, u32> {
        let len = src.len() as u32;
        let ptr = self.alloc.call(&mut self.store, len).unwrap();
        self.memory
            .write(&mut self.store, ptr as usize, src)
            .unwrap();
        let n = self.highlight.call(&mut self.store, (ptr, len)).unwrap();
        if n == u32::MAX {
            return Err(self.error.call(&mut self.store, ()).unwrap());
        }
        let out = self.output.call(&mut self.store, ()).unwrap() as usize;
        let mut records = vec![0u8; n as usize * wire::RECORD_BYTES];
        self.memory.read(&self.store, out, &mut records).unwrap();
        Ok(records)
    }
}

fn four_kib_source() -> String {
    let unit =
        std::fs::read_to_string(root().join("tests/fixtures/highlight/wal_txn.ves")).unwrap();
    let mut src = String::new();
    while src.len() < 4096 {
        src.push_str(&unit);
    }
    let mut cut = 4096;
    while !src.is_char_boundary(cut) {
        cut -= 1;
    }
    src.truncate(cut);
    src
}

#[test]
fn wasm_module_matches_native_and_is_measured() {
    let Some(path) = build_module() else {
        return;
    };
    let bytes = std::fs::read(&path).expect("read vesper-chroma.wasm");
    eprintln!(
        "vesper-chroma.wasm: {} bytes ({:.1} KiB) stripped, opt-level z, LTO, panic=abort; target {} KiB",
        bytes.len(),
        bytes.len() as f64 / 1024.0,
        TARGET_BYTES / 1024
    );
    assert!(bytes.len() <= CEILING_BYTES, "{} bytes", bytes.len());
    let mut chroma = Chroma::new(&bytes);

    let mut samples: Vec<String> = ["wal_txn.ves", "abort_across_inout.ves"]
        .iter()
        .map(|f| std::fs::read_to_string(root().join("tests/fixtures/highlight").join(f)).unwrap())
        .collect();
    samples.push(String::new());
    samples.push("fn main( -> u64 { let x = \"é🦀".into());
    samples.push(four_kib_source());
    for src in &samples {
        let native = wire::encode(&highlight_tier0_tokens(src).unwrap()).unwrap();
        assert_eq!(chroma.run(src.as_bytes()).unwrap(), native);
    }
    assert_eq!(
        chroma.run(&[0xff, 0xfe]),
        Err(2),
        "invalid UTF-8 is error 2"
    );

    // Re-highlight after a one-byte edit of a 4 KiB buffer (the playground's
    // keystroke path: copy the buffer in, highlight, read the records).
    let base = four_kib_source();
    let mut edits = vec![];
    for i in 0..64 {
        let mut s = base.clone();
        let mut at = (i * 61) % s.len();
        while !s.is_char_boundary(at) {
            at -= 1;
        }
        s.insert(at, 'x');
        edits.push(s);
    }
    for e in &edits {
        chroma.run(e.as_bytes()).unwrap();
    }
    let rounds = 8;
    let start = Instant::now();
    for _ in 0..rounds {
        for e in &edits {
            std::hint::black_box(chroma.run(e.as_bytes()).unwrap());
        }
    }
    let per = start.elapsed() / (rounds * edits.len() as u32);
    eprintln!(
        "re-highlight of a {} byte buffer after a 1-byte edit under Wasmtime 50 (Cranelift): {per:?}",
        base.len()
    );
}
