//! The three spellings of the shim's ABI agree: the Rust exports
//! (`symbols()`), the C header and the `.vsys` library of
//! `@vesper/cranelift`. The header also compiles as C.
mod support;

use std::collections::BTreeSet;
use support::*;

fn names_in(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for (i, _) in text.match_indices("vcl_") {
        let rest = &text[i..];
        let end = rest
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(rest.len());
        // A function name is followed by its parameter list.
        if rest[end..].starts_with('(') {
            out.insert(rest[..end].to_string());
        }
    }
    out
}

#[test]
fn exports_header_and_vsys_name_the_same_functions() {
    let exported: BTreeSet<String> = vesper_cranelift_sys::symbols()
        .into_iter()
        .map(|(n, _)| n.to_string())
        .collect();
    let header = std::fs::read_to_string(
        repo().join("crates/vesper-cranelift-sys/include/vesper_cranelift.h"),
    )
    .unwrap();
    let library = vsys(&package_source(), ("x86_64-unknown-linux-gnu", "sysv64"));
    assert_eq!(names_in(&header), exported, "header vs exports");
    assert_eq!(names_in(&library), exported, "vsys vs exports");
    // The package carries the same library as a file (its Loom `vsys`
    // field), so packages that depend on it import it (#1275).
    let file =
        std::fs::read_to_string(repo().join("packages/vesper-cranelift/cranelift.vsys")).unwrap();
    let directive = vsys(&package_source(), ("@TARGET@", "@ABI@"));
    assert_eq!(
        file.replace("\r\n", "\n"),
        directive,
        "packages/vesper-cranelift/cranelift.vsys differs from the directive"
    );
    // The .vsys library parses as a library with four linear handles.
    let lib = vesper_ffi::sys::parse(&library, Default::default()).unwrap();
    assert_eq!(lib.extern_types().len(), 4);
}

#[test]
fn type_codes_round_trip() {
    use vesper_cranelift_sys::codes;
    for (code, name, ty) in codes::TYPE_LANES {
        assert_eq!(codes::ty(*code), Some(*ty), "{name}");
        assert_eq!(codes::ty_code(*ty), *code);
        let v = code | (4 << 8);
        if let Some(vt) = ty.by(16) {
            assert_eq!(codes::ty(v), Some(vt), "{name}x16");
            assert_eq!(codes::ty_code(vt), v);
        }
    }
    assert_eq!(codes::ty(0), None);
    assert_eq!(codes::ty(4 | (9 << 8)), None);
    assert_eq!(codes::ty(4 | (1 << 16)), None);
    // Every opcode table is dense in names and has no duplicate codes.
    for table in [
        codes::BINARY,
        codes::BINARY_IMM,
        codes::UNARY,
        codes::CONVERT,
        codes::OVERFLOW,
        codes::LOAD,
        codes::STORE,
        codes::TERNARY,
        codes::INTCC,
        codes::FLOATCC,
    ] {
        let set: BTreeSet<u32> = table.iter().map(|(c, _)| *c).collect();
        assert_eq!(set.len(), table.len());
    }
    for (c, _) in codes::INTCC {
        assert!(codes::intcc(*c).is_some());
    }
    for (c, _) in codes::FLOATCC {
        assert!(codes::floatcc(*c).is_some());
    }
    assert!(codes::mem_flags(8 | 16).is_none(), "both endiannesses");
}

#[test]
fn header_compiles_as_c() {
    if cfg!(windows)
        || std::process::Command::new("cc")
            .arg("--version")
            .output()
            .is_err()
    {
        eprintln!("no cc; header compile check skipped");
        return;
    }
    let temp = Temp::new("header");
    let c = temp.0.join("use.c");
    std::fs::write(
        &c,
        "#include \"vesper_cranelift.h\"\nint main(void) { return vcl_module_new(0, 0, 0, 0, 0, 0) == 0 ? 0 : 1; }\n",
    )
    .unwrap();
    let out = std::process::Command::new("cc")
        .args([
            "-std=c11",
            "-Wall",
            "-Wextra",
            "-Werror",
            "-fsyntax-only",
            "-I",
        ])
        .arg(repo().join("crates/vesper-cranelift-sys/include"))
        .arg(&c)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
