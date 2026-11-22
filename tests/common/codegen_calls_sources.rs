//! The Vesper programs embedded in the feature tests the `call` family
//! covers (SH4 lane 7, #1281): the string literals of each test file that
//! parse and check, shared by tests/self_host_codegen_calls*.rs.
#![allow(dead_code)]

use vesper::check::World;

// The test crate's `check_harness` (Cargo and Bazel runfiles).
use crate::check_harness::repo;

/// The test files whose Vesper source strings exercise the decisions this
/// package ports: function-value instances, `dyn` vtables and boxed
/// objects, the Fail status and capability ABIs, register results, fip
/// caller slots, wide integers and floats, handle drops.
const SOURCE_TESTS: &[&str] = &[
    "tests/native_fail_status.rs",
    "tests/capability_verifiers.rs",
    "tests/dyn_trait.rs",
    "tests/dyn_box.rs",
    "tests/dyn_effects.rs",
    "tests/dyn_fn.rs",
    "tests/fip.rs",
    "tests/integer_widths.rs",
    "tests/wide_integers.rs",
    "tests/native_block_param_homes.rs",
    "tests/native_hoist_calls.rs",
    "tests/ffi_handles.rs",
    "tests/checked_arithmetic.rs",
    "tests/arrays.rs",
    "tests/capability_effects.rs",
    "tests/floats.rs",
    "tests/native_handlers.rs",
    "tests/vector_f32.rs",
    "tests/native_export_f32.rs",
    "tests/native_spans.rs",
];

/// The string literals of a Rust test file that look like Vesper sources
/// (tests/self_host_check_dyn.rs's extractor).
fn rust_string_literals(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut out = vec![];
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'/' && bytes.get(i + 1) == Some(&b'/') {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if c == b'\'' {
            if bytes.get(i + 2) == Some(&b'\'') {
                i += 3;
                continue;
            }
            if bytes.get(i + 1) == Some(&b'\\')
                && let Some(end) = text[i + 2..].find('\'')
            {
                i += end + 3;
                continue;
            }
            i += 1;
            continue;
        }
        if c == b'r'
            && matches!(bytes.get(i + 1), Some(b'#' | b'"'))
            && !bytes[i.saturating_sub(1)].is_ascii_alphanumeric()
        {
            let mut j = i + 1;
            let mut hashes = 0;
            while bytes.get(j) == Some(&b'#') {
                hashes += 1;
                j += 1;
            }
            if bytes.get(j) == Some(&b'"') {
                let close = format!("\"{}", "#".repeat(hashes));
                if let Some(end) = text[j + 1..].find(&close) {
                    out.push(text[j + 1..j + 1 + end].to_owned());
                    i = j + 1 + end + close.len();
                    continue;
                }
            }
            i += 1;
            continue;
        }
        if c == b'"' {
            let mut j = i + 1;
            let mut s = String::new();
            let mut ok = true;
            while j < bytes.len() && bytes[j] != b'"' {
                if bytes[j] == b'\\' {
                    let e = bytes.get(j + 1).copied().unwrap_or(b' ');
                    j += 2;
                    match e {
                        b'n' => s.push('\n'),
                        b't' => s.push('\t'),
                        b'r' => s.push('\r'),
                        b'0' => s.push('\0'),
                        b'\\' => s.push('\\'),
                        b'"' => s.push('"'),
                        b'\'' => s.push('\''),
                        b'\n' => {
                            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                                j += 1;
                            }
                        }
                        b'u' => {
                            let end = text[j..].find('}').map(|e| j + e);
                            match end.and_then(|e| {
                                u32::from_str_radix(&text[j + 1..e], 16)
                                    .ok()
                                    .map(|v| (e, v))
                            }) {
                                Some((e, v)) => {
                                    match char::from_u32(v) {
                                        Some(ch) => s.push(ch),
                                        None => ok = false,
                                    }
                                    j = e + 1;
                                }
                                None => ok = false,
                            }
                        }
                        b'x' => {
                            match u8::from_str_radix(&text[j..(j + 2).min(text.len())], 16) {
                                Ok(v) if v < 128 => s.push(v as char),
                                _ => ok = false,
                            }
                            j += 2;
                        }
                        _ => ok = false,
                    }
                } else {
                    let ch = text[j..].chars().next().unwrap();
                    s.push(ch);
                    j += ch.len_utf8();
                }
            }
            if ok {
                out.push(s);
            }
            i = j + 1;
            continue;
        }
        i += 1;
    }
    out.retain(|s| {
        s.len() >= 6
            && [
                "fn ", "struct ", "enum ", "trait ", "use ", "impl ", "effect ", "model ", "const ",
            ]
            .iter()
            .any(|k| s.contains(k))
    });
    out
}

/// Every distinct source string of [`SOURCE_TESTS`] that parses and checks
/// clean, with its world.
pub fn source_worlds() -> Vec<(String, World)> {
    let mut seen = std::collections::BTreeSet::new();
    let mut out = vec![];
    let quiet = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    for file in SOURCE_TESTS.iter().copied() {
        let Ok(text) = std::fs::read_to_string(repo().join(file)) else {
            continue;
        };
        // A `format!` template's braces are doubled; its de-doubled text is
        // the program when it has no placeholders.
        let literals = rust_string_literals(&text).into_iter().flat_map(|s| {
            let plain = s
                .contains("{{")
                .then(|| s.replace("{{", "{").replace("}}", "}"));
            std::iter::once(s).chain(plain)
        });
        for (i, src) in literals.enumerate() {
            if !seen.insert(src.clone()) {
                continue;
            }
            let r = std::panic::catch_unwind(|| {
                let parsed = vesper::parser::parse(&src);
                if !parsed.diagnostics.is_empty() {
                    return None;
                }
                let checked = vesper::check::check(&parsed.module);
                checked.diagnostics.is_empty().then_some(checked.world)
            });
            if let Ok(Some(w)) = r {
                out.push((format!("{file}#{i}"), w));
            }
        }
    }
    std::panic::set_hook(quiet);
    out
}
