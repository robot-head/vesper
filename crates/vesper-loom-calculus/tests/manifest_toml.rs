//! The Vesper manifest decoder (`manifest_toml.ves`, through
//! `manifest_toml`) against `toml::from_str::<Manifest>`: the same
//! manifest (compared as `serde_json::to_string` text) or the same error
//! text.
#![cfg(vesper_port)]

use vesper_loom_calculus::Manifest;

fn rust(src: &str) -> String {
    match toml::from_str::<Manifest>(src) {
        Ok(m) => serde_json::to_string(&m).unwrap(),
        Err(e) => format!("!{e}"),
    }
}

fn check(src: &str) {
    assert_eq!(
        vesper_loom_calculus::manifest_toml(src),
        rust(src),
        "on {src:?}"
    );
}

const BASE: &str = "[package]\nname = \"@o/a\"\nversion = \"1.0.0\"\nedition = \"2026\"\n";

fn cases() -> Vec<String> {
    let tails = [
        "",
        "bogus = 1\n",
        "[bogus]\n",
        "license = \"MIT\"\ncapability_ceiling = [\"Net\", \"Fail\", \"Net\"]\nforbid_unsafe_ffi = false\nvsys = [\"b.vsys\", \"a.vsys\"]\n",
        "capability_ceiling = [\"a\", 1]\n",
        "forbid_unsafe_ffi = \"yes\"\n",
        "license = 1979-05-27\n",
        "license = 1979-05-27T07:32:00.123456789123-00:00\n",
        "vsys = \"x\"\n",
        "vsys = [[\"x\"]]\n",
        "[dependencies]\n\"@o/b\" = { version = \"^x\" }\n",
        "[dependencies]\n\"bad\" = { version = \"^1\" }\n",
        "[dependencies]\n\"@o/b\" = { }\n",
        "[dependencies]\n\"@o/b\" = \"^1\"\n",
        "[dependencies]\n\"@o/z\" = { version = \">= 1.2, < 2\", effects = [\"Net\"], optional = true, features = [\"f\"], path = \"../z\" }\n\"@o/b\" = { version = \"1.x\" }\n\"std/c\" = { version = \"*\" }\n",
        "[dependencies]\n\"@o/b\" = { version = \"~1.2.3-rc.1+b\", bogus = 1 }\n",
        "[dependencies.\"@o/b\"]\nversion = \"^1\"\neffects = \"x\"\n",
        "[features]\nf = { deps = [\"x\"] }\n",
        "[features]\nz = { enables_effects = [\"Net\"], deps = [\"@o/b\"], features = [\"a\"], sys_deps = [\"libz ^1\"] }\na = {}\n",
        "[features]\nf = 1\n",
        "features = 1979-05-27\n",
        "dependencies = 1979-05-27\n",
        "[build-sandbox]\nfuel_limit = -1\n",
        "[build-sandbox]\nfuel_limit = 1.5\n",
        "[build-sandbox]\nfuel_limit = 1e3\n",
        "[build-sandbox]\nfuel_limit = -0.0\n",
        "[build-sandbox]\nfuel_limit = 0.000123\n",
        "[build-sandbox]\nfuel_limit = inf\n",
        "[build-sandbox]\nfuel_limit = 0.1000000000000000055511151231257827\n",
        "[build-sandbox]\nfuel_limit = 1234567890123456789.0\n",
        "[build-sandbox]\nfuel_limit = 1e-400\n",
        "[build-sandbox]\nfuel_limit = 1e300\n",
        // decode_bits's undecided cases: more than 19 digits on a boundary.
        "[build-sandbox]\nfuel_limit = 1.00000000000000011102230246251565404236316680908203125\n",
        "[build-sandbox]\nfuel_limit = 1.00000000000000011102230246251565404236316680908203126\n",
        "[build-sandbox]\nfuel_limit = 1.00000000000000033306690738754696212708950042724609375\n",
        "[build-sandbox]\nfuel_limit = 9007199254740993.0000000000000000000001\n",
        "[build-sandbox]\nfuel_limit = 2.4703282292062327208828439643411068618252990130716238221279284125033775364e-324\n",
        "[build-sandbox]\nfuel_limit = 2.4703282292062327208828439643411068618252990130716238221279284125033775363e-324\n",
        "[build-sandbox]\nfuel_limit = 1.7976931348623158079372897140530341507993413271003782693617377898044e308\n",
        "[build-sandbox]\nfuel_limit = 1.7976931348623157e308\n",
        "[build-sandbox]\nfuel_limit = -0.000000000000000000000000000000000000000000000000000000000000000001234567890123456789012345\n",
        "[build-sandbox]\nfuel_limit = 4.9e-324\n",
        "[build-sandbox]\nfuel_limit = 2.2250738585072014e-308\n",
        "[build-sandbox]\nfuel_limit = -nan\n",
        "[build-sandbox]\nscript = \"b.ves\"\nwasi_read_paths = [\"b\", \"a\"]\nfuel_limit = 18446744073709551615\nallow_network = true\n",
        "[build-sandbox]\nfuel_limit = 9223372036854775807\n",
        "build-sandbox = 1979-05-27\n",
        "[resolver]\n",
        "[resolver.policy]\nmin_repro_attestations = 5000000000\n",
        "[resolver.policy]\noptimize = [\"b\", \"a\", \"b\"]\nmin_repro_attestations = 3\nblock_unvetted_namespaces = false\n",
        "[resolver.policy.x]\n",
        "[resolver]\npolicy = 1979-05-27\n",
        "[verification]\nexport_lean4_specs = [true]\n",
        "[verification]\nexport_lean4_specs = [\"b\", \"a\"]\n",
        "[verification]\nbogus = 1\n",
        "[[package.vsys]]\n",
        "[package.capability_ceiling]\n",
        "license = \"a\\tb\\u00e9\\u0001\\\"\\\\\"\nedition2 = 1\n",
        "license = 'a\u{301}b\u{200b}'\nforbid_unsafe_ffi = 'é\u{7f}'\n",
    ];
    let mut out: Vec<String> = tails.iter().map(|t| format!("{BASE}{t}")).collect();
    out.extend(
        [
            "",
            "[package]\n",
            "package = 1\n",
            "package = 1979-05-27\n",
            "[package]\nname = 1\n",
            "[package]\nname = \"bad\"\nversion = \"1.0.0\"\nedition = \"2026\"\n",
            "[package]\nname = \"@o/a\"\nversion = \"1.x\"\nedition = \"2026\"\n",
            "[package]\nname = \"@o/a\"\nversion = 1\nedition = \"2026\"\n",
            "[package]\nname = \"@o/a\"\nversion = \"1.0.0\"\nedition = 2026\n",
            "[package.x]\n",
            "[package.name]\n",
            "[dependencies]\n",
            "package = []\n",
            "package = [\"@o/a\", \"1.0.0\", \"2026\"]\n",
            "package = [\"@o/a\", \"1.0.0\", \"2026\", \"MIT\"]\n",
            "package = [\"@o/a\", \"1.0.0\", \"2026\", \"MIT\", [], false, [], 1, 2]\n",
            "package = [\"@o/a\", \"1.0.0\", \"2026\", \"MIT\"]\n[features]\nf = [[\"x\"], [], [], [], 5]\ng = [1]\n",
            "package = [\"@o/a\", \"1.0.0\", \"2026\", \"MIT\"]\ndependencies = { \"@o/b\" = [\"^1\"] }\n",
            "package = [\"@o/a\", \"1.0.0\", \"2026\", \"MIT\"]\nresolver = [[]]\n",
            "package = [\"@o/a\", \"1.0.0\", \"2026\", \"MIT\"]\nresolver = [[[], 1]]\nverification = [[\"b\", \"a\"]]\n",
            "package = [\"@o/a\", \"1.0.0\", \"2026\", \"MIT\"]\n[[build-sandbox]]\n",
        ]
        .map(String::from),
    );
    out
}

#[test]
fn manifest_cases_match_the_toml_crate() {
    for case in cases() {
        check(&case);
    }
}

/// The repository checkout, or `None` (with a note) when the test runs
/// without it, as under Bazel, whose runfiles hold only declared inputs.
fn repository() -> Option<std::path::PathBuf> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    if root.join("Cargo.lock").is_file() && root.join("packages").is_dir() {
        Some(root)
    } else {
        eprintln!("no repository checkout next to the crate; not compared");
        None
    }
}

/// Every Loom.toml in the repository decodes to the same manifest.
#[test]
fn repository_manifests_match_the_toml_crate() {
    let Some(root) = repository() else { return };
    let mut seen = 0;
    let mut stack = vec![root.join("packages"), root.join("crates")];
    while let Some(path) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&path) else {
            continue;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                if p.file_name().is_some_and(|n| n != "target") {
                    stack.push(p);
                }
            } else if p.file_name().is_some_and(|n| n == "Loom.toml")
                && let Ok(text) = std::fs::read_to_string(&p)
            {
                check(&text);
                seen += 1;
            }
        }
    }
    assert!(seen > 50, "only {seen} manifests");
}

/// Random edits of the cases: inserted fragments, deletions and swapped
/// lines, deterministically seeded.
#[test]
fn mutated_manifests_match_the_toml_crate() {
    const ALPHABET: &[&str] = &[
        "[",
        "]",
        "{",
        "}",
        "=",
        ",",
        ".",
        "\"",
        "'",
        "\n",
        " ",
        "#",
        "1",
        "-1",
        "1.5",
        "1.00000000000000011102230246251565404236316680908203125",
        "9007199254740993.00000000000000000001",
        "true",
        "1979-05-27",
        "[]",
        "{}",
        "\"x\"",
        "\"@o/c\"",
        "\"^1\"",
        "\"1.0.0\"",
        "name",
        "version",
        "features",
        "deps",
        "dependencies",
        "package",
        "x",
        "é",
        "[package]\n",
        "[features]\n",
        "[dependencies]\n",
        "[build-sandbox]\n",
        "[resolver.policy]\n",
        "[verification]\n",
        "[[a]]\n",
        "optional = true\n",
        "fuel_limit = ",
        "path = \"p\"\n",
    ];
    let cases = cases();
    let mut state: u64 = std::env::var("VESPER_TOML_SEED")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0x51_7cc1_b727_220a);
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let iterations: u64 = std::env::var("VESPER_TOML_FUZZ")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20_000);
    for _ in 0..iterations {
        let base = &cases[(next() % cases.len() as u64) as usize];
        let mut s: Vec<char> = base.chars().collect();
        for _ in 0..1 + next() % 4 {
            let at = (next() % (s.len() as u64 + 1)) as usize;
            match next() % 3 {
                0 => {
                    let piece = ALPHABET[(next() % ALPHABET.len() as u64) as usize];
                    for (i, c) in piece.chars().enumerate() {
                        s.insert(at + i, c);
                    }
                }
                1 if at < s.len() => {
                    s.remove(at);
                }
                _ => {
                    let text: String = s.iter().collect();
                    let mut lines: Vec<&str> = text.split('\n').collect();
                    if lines.len() > 1 {
                        let i = (next() % lines.len() as u64) as usize;
                        let j = (next() % lines.len() as u64) as usize;
                        lines.swap(i, j);
                    }
                    s = lines.join("\n").chars().collect();
                }
            }
        }
        check(&s.into_iter().collect::<String>());
    }
}
