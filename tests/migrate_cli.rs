//! Integration B (#136): `vesper migrate` lifting C headers and Rust crates.
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "vesper-migrate-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    fn write(&self, name: &str, text: &str) -> String {
        let path = self.0.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, text).unwrap();
        path.to_string_lossy().into_owned()
    }
    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn vesper(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vesper"))
        .args(args)
        .output()
        .unwrap()
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

const HEADER: &str = "#pragma once\n#include <stdint.h>\n#include <stddef.h>\n\nuint32_t crc32_update(uint32_t crc, const uint8_t *data, size_t len);\nint32_t add_i32(int32_t a, int32_t b);\nvoid *opaque_handle(void);\n";

#[test]
fn migrate_lifts_a_c_header_into_ves_and_vsys() {
    let temp = Temp::new();
    let header = temp.write("codec.h", HEADER);
    let artifact = temp.write("libcodec.a", "not really an archive");
    let out = temp.path("out");
    let o = vesper(&[
        "migrate",
        "--source",
        &header,
        "-o",
        out.to_str().unwrap(),
        "--artifact",
        &artifact,
        "--target",
        "aarch64-unknown-linux-gnu",
    ]);
    let t = text(&o);
    assert!(o.status.success(), "{t}");
    assert!(
        t.contains("2 with a C ABI") || t.contains("with a C ABI"),
        "{t}"
    );
    let ves = fs::read_to_string(out.join("codec.ves")).unwrap();
    assert!(ves.contains("vesper.migrate/1"), "{ves}");
    assert!(ves.contains("crc32_update"), "{ves}");
    let vsys = fs::read_to_string(out.join("codec.vsys")).unwrap();
    assert!(vsys.contains("sys library CodecAbi ("), "{vsys}");
    assert!(vsys.contains("aarch64-unknown-linux-gnu"), "{vsys}");
    assert!(vsys.contains("CSlice[u8]"), "{vsys}");
    let digest = blake3::hash(b"not really an archive").to_hex().to_string();
    assert!(vsys.contains(&digest), "{vsys}");
    let report = fs::read_to_string(out.join("codec.migrate.txt")).unwrap();
    assert!(report.contains("opaque_handle"), "{report}");
    // The spec parses with the Bridge.
    vesper_ffi::sys::parse(&vsys, vesper_ffi::sys::Limits::default()).unwrap();
    // Without --artifact no spec is written.
    let out2 = temp.path("out2");
    let o = vesper(&["migrate", "--source", &header, "-o", out2.to_str().unwrap()]);
    assert!(o.status.success());
    assert!(text(&o).contains("no .vsys written"));
    assert!(!out2.join("codec.vsys").exists());
}

#[test]
fn migrate_lifts_rust_crates_and_files() {
    let temp = Temp::new();
    temp.write(
        "legacy/Cargo.toml",
        "[package]\nname = \"legacy\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    temp.write(
        "legacy/src/lib.rs",
        "#[unsafe(no_mangle)]\npub extern \"C\" fn add(a: u32, b: u32) -> u32 { a.wrapping_add(b) }\n\npub fn scale(x: &mut u64) { *x *= 2; }\n",
    );
    let out = temp.path("out");
    let o = vesper(&[
        "migrate",
        "--source",
        temp.path("legacy").to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
    ]);
    assert!(o.status.success(), "{}", text(&o));
    let ves = fs::read_to_string(out.join("legacy.ves")).unwrap();
    assert!(ves.contains("add") && ves.contains("scale"), "{ves}");
    let file = temp.write("single.rs", "pub fn id(x: u64) -> u64 { x }\n");
    let o = vesper(&["migrate", "--source", &file, "-o", out.to_str().unwrap()]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(out.join("single.ves").exists());
}

#[test]
fn migrate_rejects_bad_inputs_with_typed_messages() {
    let temp = Temp::new();
    let bad = temp.write("macro.h", "#define LIMIT 4\nint32_t f(int32_t a);\n");
    let other = temp.write("notes.txt", "hello");
    let header = temp.write("ok.h", HEADER);
    let out = temp.path("out");
    let out = out.to_str().unwrap();
    for (args, message) in [
        (vec!["migrate", "-o", out], "migrate requires --source"),
        (
            vec!["migrate", "--source", header.as_str()],
            "migrate requires -o",
        ),
        (
            vec!["migrate", "--source", bad.as_str(), "-o", out],
            "macro.h",
        ),
        (
            vec!["migrate", "--source", other.as_str(), "-o", out],
            "expected a C header",
        ),
        (
            vec![
                "migrate",
                "--source",
                header.as_str(),
                "-o",
                out,
                "--target",
                "riscv64",
            ],
            "unsupported .vsys target",
        ),
        (
            vec![
                "migrate",
                "--source",
                header.as_str(),
                "-o",
                out,
                "--link",
                "weird",
            ],
            "--link must be static or dynamic",
        ),
        (vec!["migrate", "--bogus"], "unknown migrate option"),
    ] {
        let o = vesper(&args);
        assert!(!o.status.success(), "{args:?}: {}", text(&o));
        assert!(text(&o).contains(message), "{args:?}: {}", text(&o));
    }
    assert!(!Path::new(out).join("macro.ves").exists());
}
