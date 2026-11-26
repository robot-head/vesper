//! `@vesper/verify` (packages/vesper-verify, SH5 lane 18, #1341) against
//! src/verify_commands.rs: theorem discovery over every Lean file of the
//! repository, prefixes of each (which end inside comments, strings,
//! attributes and scopes) and hand-written edge cases, and the
//! contract-binding source over hand-written cases. A driver executable
//! built from the package prints one record per input, which must equal
//! Rust's byte for byte.
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::Instant,
};

#[allow(
    dead_code,
    reason = "only the discovery and binding pieces are compared"
)]
#[path = "../src/verify_commands.rs"]
mod verify_commands;

fn repo() -> PathBuf {
    fs::canonicalize(env!("CARGO_MANIFEST_DIR")).unwrap()
}

const DRIVER: &str = r#"use @vesper/verify::{bindings, discover};
use @vesper/proof-audit::strs::{Strs, byte, count, end, push, put_str, start, strs_new};
use @vesper-std/env::{arg_bytes, arg_count};
use @vesper-std/fs::{IoError, read_path, write_stdout};

fn put_u64(inout out: ChunkSeq[u8], n: u64) {
    if n >= 10 {
        put_u64(inout out, n / 10);
    }
    out.push(wrapping_add(48, wrapping_u8(n % 10)));
}

fn put_names(inout out: ChunkSeq[u8], in names: Strs) {
    for i in 0..count(in names) {
        out.extend(in names.bytes[start(in names, i)..end(in names, i)]);
        out.push(10);
    }
}

fn put_err(inout out: ChunkSeq[u8], in err: ChunkSeq[u8]) {
    put_str(inout out, in "err ");
    put_u64(inout out, err.len());
    out.push(10);
    out.extend(in err.as_slice());
    out.push(10);
}

// One `bind` case file: `C\tname`, `P\tfunction\ttheorem`,
// `W\tfunction\ttheorem` lines.
fn bind_case(inout out: ChunkSeq[u8], in text: Slice[u8]) {
    let mut contracts = strs_new();
    let mut pk = strs_new();
    let mut pv = strs_new();
    let mut wk = strs_new();
    let mut wv = strs_new();
    let mut from: u64 = 0;
    for i in 0..text.len() {
        if byte(in text, i) == 10 {
            let tag = byte(in text, from);
            let mut tab: u64 = wrapping_add(from, 2);
            for k in wrapping_add(from, 2)..i {
                if byte(in text, k) == 9 && tab == wrapping_add(from, 2) {
                    tab = k;
                }
            }
            if tag == 67 {
                push(inout contracts, in text[wrapping_add(from, 2)..i]);
            } else if tag == 80 {
                push(inout pk, in text[wrapping_add(from, 2)..tab]);
                push(inout pv, in text[wrapping_add(tab, 1)..i]);
            } else {
                push(inout wk, in text[wrapping_add(from, 2)..tab]);
                push(inout wv, in text[wrapping_add(tab, 1)..i]);
            }
            from = wrapping_add(i, 1);
        }
    }
    let b = bindings(in contracts, in pk, in pv, in wk, in wv);
    if b.failed {
        put_err(inout out, in b.err);
    } else {
        put_str(inout out, in "ok ");
        put_u64(inout out, b.source.len());
        out.push(10);
        out.extend(in b.source.as_slice());
        put_names(inout out, in b.names);
    }
}

pub fn main() -> u64 eff [Ffi[StdFs], Ffi[StdEnv]] {
    let mut out: ChunkSeq[u8] = ChunkSeq::new[u8]();
    let bind = match arg_bytes(1) {
        Option::Some(m) => m.len() == 4,
        Option::None => false,
    };
    for i in 2..arg_count() {
        let path = match arg_bytes(i) {
            Option::Some(a) => a,
            Option::None => ChunkSeq::new[u8](),
        };
        let text = handle Fail[IoError] in {
            read_path(in path.as_slice())
        } with {
            return v => v,
            raise(_e, _k) => ChunkSeq::new[u8](),
        };
        if bind {
            bind_case(inout out, in text.as_slice());
        } else {
            let f = discover(in text.as_slice());
            if f.failed {
                put_err(inout out, in f.err);
            } else {
                put_str(inout out, in "ok ");
                put_u64(inout out, count(in f.names));
                out.push(10);
                put_names(inout out, in f.names);
            }
        }
    }
    handle Fail[IoError] in {
        write_stdout(in out.as_slice());
    } with {
        return v => v,
        raise(_e, _k) => {},
    }
    0
}
"#;

struct Dir(PathBuf);
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn build_driver(dir: &Path) -> PathBuf {
    let package = dir.join("driver");
    fs::create_dir_all(package.join("src")).unwrap();
    let root = repo();
    let dep = |p: &str| serde_json::to_string(root.join(p).to_str().unwrap()).unwrap();
    fs::write(
        package.join("Loom.toml"),
        format!(
            "[package]\nname = \"@test/verify-driver\"\nversion = \"0.1.0\"\nedition = \"2026\"\n\n\
             [dependencies]\n\
             \"@vesper/verify\" = {{ version = \"^0.1\", path = {} }}\n\
             \"@vesper/proof-audit\" = {{ version = \"^0.1\", path = {} }}\n\
             \"@vesper-std/env\" = {{ version = \"^0.1\", path = {} }}\n\
             \"@vesper-std/fs\" = {{ version = \"^0.1\", path = {} }}\n",
            dep("packages/vesper-verify"),
            dep("packages/vesper-proof-audit"),
            dep("packages/vesper-std/env"),
            dep("packages/vesper-std/fs"),
        ),
    )
    .unwrap();
    fs::write(package.join("src/main.ves"), DRIVER).unwrap();
    let binary = dir.join("verify-port");
    let output = Command::new(env!("CARGO_BIN_EXE_vesper"))
        .arg("build")
        .arg(&package)
        .arg("-o")
        .arg(&binary)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    binary
}

fn record_err(out: &mut String, message: &str) {
    out.push_str(&format!("err {}\n{message}\n", message.len()));
}

fn rust_discover(text: &str) -> String {
    let mut out = String::new();
    match verify_commands::discover_theorems(text) {
        Ok(names) => {
            out.push_str(&format!("ok {}\n", names.len()));
            for n in names {
                out.push_str(&n);
                out.push('\n');
            }
        }
        Err(e) => {
            assert_eq!(e.stage, "proofs");
            record_err(&mut out, &e.message);
        }
    }
    out
}

/// A binding case: contracts, `--contract-proof` and `--contract-witness`.
struct Bind {
    contracts: Vec<&'static str>,
    proofs: Vec<(&'static str, &'static str)>,
    witnesses: Vec<(&'static str, &'static str)>,
}

fn rust_bind(case: &Bind) -> String {
    let contracts: Vec<String> = case.contracts.iter().map(|s| s.to_string()).collect();
    let map = |v: &[(&str, &str)]| -> BTreeMap<String, String> {
        v.iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    };
    let mut out = String::new();
    match verify_commands::contract_bindings(&contracts, &map(&case.proofs), &map(&case.witnesses))
    {
        Ok((source, names)) => {
            out.push_str(&format!("ok {}\n{source}", source.len()));
            for n in names {
                out.push_str(&n);
                out.push('\n');
            }
        }
        Err(e) => {
            assert_eq!(e.stage, "contracts");
            record_err(&mut out, &e.message);
        }
    }
    out
}

fn bind_text(case: &Bind) -> String {
    let mut text = String::new();
    for c in &case.contracts {
        text.push_str(&format!("C\t{c}\n"));
    }
    // Map keys are unique, as in a BTreeMap.
    let unique = |v: &[(&str, &str)]| -> BTreeMap<String, String> {
        v.iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    };
    for (k, v) in unique(&case.proofs) {
        text.push_str(&format!("P\t{k}\t{v}\n"));
    }
    for (k, v) in unique(&case.witnesses) {
        text.push_str(&format!("W\t{k}\t{v}\n"));
    }
    text
}

/// Every `.lean` file of the repository (outside build output), sorted.
fn lean_files() -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            if name == "target" || name == ".git" || name == ".lake" || name == "node_modules" {
                continue;
            }
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "lean") {
                out.push(path);
            }
        }
    }
    let mut out = vec![];
    walk(&repo(), &mut out);
    out.sort();
    out
}

/// Hand-written proofs files: the unit tests' inputs and the edges of the
/// tokenizer and scope walk.
const EDGES: &[&str] = &[
    "theorem plain : True := trivial\n\n@[simp] theorem simp_one : True := trivial\n\n@[simp,\n  aesop safe [constructors]]\nprotected theorem prot_thm (x : Nat) : x = x := rfl\n\nnoncomputable lemma helper' : True := trivial\n\nnamespace Outer.Inner\n@[simp] theorem scoped_thm : True := trivial\ntheorem _root_.root_override : True := trivial\nend Outer.Inner\n\ntheorem after_ns.{u} : True := trivial\n",
    "namespace A\ntheorem first : True := trivial\nsection S\ntheorem in_section : True := trivial\nend\ntheorem still_in_a : True := trivial\nend\ntheorem second : True := trivial\n\nnamespace B\nsection\ntheorem in_anon_section : True := trivial\nend\ntheorem still_in_b : True := trivial\nend B\ntheorem third : True := trivial\n",
    "namespace VesperProofs.Structs\ntheorem move_start_frame (s : Segment) (lemma : Nat) :\n    (move_start s 1).2.end = s.end := by\n  simp [move_start]\ntheorem next_one : True := trivial\nend VesperProofs.Structs\ntheorem top_after : True := trivial\n",
    "private theorem hidden : False := by sorry\n",
    "@[simp] private lemma hidden : False := by sorry\n",
    "theorem «weird name» : True := trivial\n",
    "example : False := by sorry\n",
    "",
    "theorem",
    "lemma",
    "theorem 3x : True := trivial\n",
    "theorem .x : True := trivial\n",
    "theorem def : True := trivial\n",
    "theorem _root_.bad..name : True := trivial\n",
    "theorem a : True := trivial\ntheorem a : True := trivial\n",
    "namespace\n",
    "namespace\nFoo\n",
    "namespace 1\n",
    "namespace «Foo»\n",
    "namespace end\n",
    "namespace .Foo\n",
    "section «S»\nend\n",
    "section 1x\nend\n",
    "section Foo..Bar\nend\n",
    "end\n",
    "end Foo\n",
    "namespace A\nend B\n",
    "namespace A.B\nend A.B\ntheorem t : True := trivial\n",
    "namespace A.B\nend B\nend A\ntheorem t : True := trivial\n",
    "namespace A.B\nend A\n",
    "namespace A\nsection S\nend A\n",
    "namespace A\nend «A»\n",
    "namespace A\nend A..\n",
    "mutual\ntheorem m : True := trivial\nend\n",
    "namespace N\nmutual\ntheorem m : True := trivial\nend\nend N\n",
    "/- unterminated",
    "/- /- nested -/ still open",
    "/- /- nested -/ -/ theorem after_comment : True := trivial\n",
    "\"unterminated",
    "\"a \\\" quoted\\\n string\" theorem after_string : True := trivial\n",
    "'a' theorem after_char : True := trivial\n",
    "'\\n' theorem after_escape : True := trivial\n",
    "'' theorem quote_pair : True := trivial\n",
    "@[simp\n",
    "@[a [b] \"]\" '\\]' -- ]\n /- ] -/ c] theorem after_attr : True := trivial\n",
    "@[a \"unterminated",
    "@[a /- unterminated",
    "«unterminated",
    "theorem x : «a\nb» = 1 := rfl\ntheorem y : True := trivial\n",
    "(theorem inside_paren : True := trivial)\ntheorem outside : True := trivial\n",
    "[theorem in_bracket]\n{theorem in_brace}\n)]}\ntheorem after_closes : True := trivial\n",
    "private def d := 1\ntheorem not_private : True := trivial\n",
    "private (x) theorem reset_by_paren : True := trivial\n",
    "private @[simp] theorem attr_keeps_private : True := trivial\n",
    "private protected theorem modifiers_keep_private : True := trivial\n",
    "private . theorem dot_resets : True := trivial\n",
    "x.theorem not_decl : True := trivial\ntheorem real : True := trivial\n",
    "theorem a.b.c : True := trivial\nnamespace N\ntheorem _root_.r : True := trivial\ntheorem n : True := trivial\nend N\n",
    "theorem tab\tsep : True := trivial\ntheorem\tafter_tab : True := trivial\n",
    "theorem crlf : True := trivial\r\ntheorem crlf2 : True := trivial\r\n",
    "theorem ✓ : True := trivial\n",
    "theorem α : True := trivial\n",
    "-- theorem commented : True\ntheorem after_line_comment : True := trivial\n",
    "section\nend\nend\n",
    "namespace A\nsection\nend A\n",
    "theorem x' : True := trivial\ntheorem x'' : True := trivial\n",
    "theorem _root_ : True := trivial\n",
    "theorem _root_.x : True := trivial\n",
    "theorem _root_.«x» : True := trivial\n",
];

/// Inputs with names past the 4096-byte message bound, and with more than
/// 128 theorems.
fn generated() -> Vec<String> {
    let long = "a".repeat(1100);
    let mut out = vec![
        format!("theorem «{}» : True := trivial\n", "é".repeat(3000)),
        format!("namespace «{}»\n", "x".repeat(5000)),
        format!("theorem {long} : True := trivial\n"),
        format!("namespace {long}\n"),
        format!("namespace A\nend {}\n", "b".repeat(5000)),
    ];
    let mut many = String::new();
    for i in 0..130 {
        many.push_str(&format!("theorem t{i} : True := trivial\n"));
    }
    out.push(many);
    let mut exactly = String::new();
    for i in 0..128 {
        exactly.push_str(&format!(
            "theorem t{i} : True := trivial\ntheorem t{i} : True := trivial\n"
        ));
    }
    out.push(exactly);
    out
}

fn bind_cases() -> Vec<Bind> {
    vec![
        Bind {
            contracts: vec![],
            proofs: vec![],
            witnesses: vec![],
        },
        Bind {
            contracts: vec!["f"],
            proofs: vec![],
            witnesses: vec![],
        },
        Bind {
            contracts: vec!["f"],
            proofs: vec![("f", "P.f_ok")],
            witnesses: vec![("f", "P.f_pre")],
        },
        Bind {
            contracts: vec!["g", "f"],
            proofs: vec![("g", "g_ok"), ("f", "A.B.f_ok")],
            witnesses: vec![("f", "f_pre"), ("g", "N.g_pre")],
        },
        Bind {
            contracts: vec!["f"],
            proofs: vec![("h", "h_ok")],
            witnesses: vec![],
        },
        Bind {
            contracts: vec!["f"],
            proofs: vec![],
            witnesses: vec![("z", "z_pre")],
        },
        Bind {
            contracts: vec!["f", "g"],
            proofs: vec![("f", "f_ok")],
            witnesses: vec![("f", "f_pre"), ("g", "g_pre")],
        },
        Bind {
            contracts: vec!["f", "g"],
            proofs: vec![("f", "f_ok"), ("g", "g_ok")],
            witnesses: vec![("g", "g_pre")],
        },
        Bind {
            contracts: vec!["f", "g"],
            proofs: vec![("b", "x"), ("a", "y")],
            witnesses: vec![],
        },
        Bind {
            contracts: vec!["m.n", "f"],
            proofs: vec![("m.n", "T.m_ok"), ("f", "f_ok")],
            witnesses: vec![("m.n", "m_pre"), ("f", "W.f_pre")],
        },
        Bind {
            contracts: vec![],
            proofs: vec![],
            witnesses: vec![("w", "w")],
        },
    ]
}

#[test]
fn vesper_port_matches_rust_verify_pieces() {
    let dir =
        Dir(std::env::temp_dir().join(format!("vesper-self-host-verify-{}", std::process::id())));
    let _ = fs::remove_dir_all(&dir.0);
    fs::create_dir_all(dir.0.join("in")).unwrap();
    let binary = build_driver(&dir.0);

    // Discovery inputs: whole files (at most 128 KiB, as `read_source`
    // reads them), prefixes cut on character boundaries, edges.
    let mut inputs: Vec<String> = vec![];
    let files = lean_files();
    assert!(files.len() >= 150, "{} Lean files", files.len());
    for path in &files {
        let Ok(text) = fs::read_to_string(path) else {
            continue;
        };
        if text.len() > 128 * 1024 {
            continue;
        }
        for k in 1..=7 {
            let mut cut = text.len() * k / 8;
            while !text.is_char_boundary(cut) {
                cut -= 1;
            }
            inputs.push(text[..cut].to_owned());
        }
        inputs.push(text);
    }
    inputs.extend(EDGES.iter().map(|s| s.to_string()));
    inputs.extend(generated());
    let mut paths = vec![];
    let mut expected = String::new();
    for (i, text) in inputs.iter().enumerate() {
        // Names relative to `in/`: 1600 absolute paths would pass Windows'
        // 32,767-character command line limit.
        let name = format!("{i}.lean");
        fs::write(dir.0.join("in").join(&name), text).unwrap();
        paths.push(name);
    }
    // Rust in this test's process; the port as one process that reads
    // every file itself.
    let started = Instant::now();
    for text in &inputs {
        expected.push_str(&rust_discover(text));
    }
    let rust = started.elapsed();
    let started = Instant::now();
    let output = Command::new(&binary)
        .current_dir(dir.0.join("in"))
        .arg("discover")
        .args(&paths)
        .output()
        .unwrap();
    let port = started.elapsed();
    assert!(output.status.success(), "{output:?}");
    let actual = String::from_utf8(output.stdout).unwrap();
    let ok = expected.matches("\nok ").count() + usize::from(expected.starts_with("ok "));
    if actual != expected {
        // Find the inputs that differ, one process each.
        let mut shown = 0;
        for (path, text) in paths.iter().zip(&inputs) {
            let one = Command::new(&binary)
                .current_dir(dir.0.join("in"))
                .arg("discover")
                .arg(path)
                .output()
                .unwrap();
            let one = String::from_utf8_lossy(&one.stdout).into_owned();
            let want = rust_discover(text);
            if one != want {
                shown += 1;
                let head: String = text.chars().take(300).collect();
                eprintln!("input {path}:\n{head:?}\nrust {want:?}\nport {one:?}\n");
                if shown == 6 {
                    break;
                }
            }
        }
    }
    assert!(actual == expected, "the port differs from Rust");
    eprintln!(
        "discovery: {} inputs ({} found theorems), 0 diffs; Rust {rust:?}, port {port:?} (one process)",
        inputs.len(),
        ok,
    );
    assert!(ok >= 100, "only {ok} inputs found theorems");

    // Binding cases.
    let cases = bind_cases();
    let mut paths = vec![];
    let mut expected = String::new();
    for (i, case) in cases.iter().enumerate() {
        let path = dir.0.join("in").join(format!("bind{i}.txt"));
        fs::write(&path, bind_text(case)).unwrap();
        paths.push(path);
        expected.push_str(&rust_bind(case));
    }
    let output = Command::new(&binary)
        .arg("bind")
        .args(&paths)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(String::from_utf8(output.stdout).unwrap(), expected);
    eprintln!("bindings: {} cases, 0 diffs", cases.len());
}
