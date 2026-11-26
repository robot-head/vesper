//! The Vesper lockfile reader (`lock_json.ves`, through `lock_json`)
//! against `serde_json::from_slice::<Solution>`: the same solution
//! (compared as `serde_json::to_string` text) or the same error text, on
//! generated lockfiles, hand-written edge cases and byte-level damage of
//! both.
#![cfg(vesper_port)]

use semver::{Version, VersionReq};
use std::collections::{BTreeMap, BTreeSet};
use vesper_loom_calculus::*;

fn rust(src: &[u8]) -> String {
    match serde_json::from_slice::<Solution>(src) {
        Ok(s) => serde_json::to_string(&s).unwrap(),
        Err(e) => format!("!{e}"),
    }
}

/// The inputs whose results differ, with both results.
fn diffs(inputs: &[Vec<u8>]) -> Vec<String> {
    let mut out = vec![];
    for src in inputs {
        let want = rust(src);
        let got = lock_json(src);
        if got != want {
            out.push(format!(
                "on {:?}\n  vesper {got:?}\n  rust   {want:?}",
                String::from_utf8_lossy(src)
            ));
        }
    }
    out
}

fn assert_same(what: &str, inputs: &[Vec<u8>]) {
    let d = diffs(inputs);
    let accepted = inputs.iter().filter(|s| !rust(s).starts_with('!')).count();
    eprintln!("{what}: {accepted} of {} accepted", inputs.len());
    assert!(
        d.is_empty(),
        "{what}: {} of {} differ:\n{}",
        d.len(),
        inputs.len(),
        d.iter().take(20).cloned().collect::<Vec<_>>().join("\n")
    );
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }
    fn pick<'a>(&mut self, xs: &[&'a str]) -> &'a str {
        xs[self.below(xs.len())]
    }
    fn some(&mut self, xs: &[&str], max: usize) -> BTreeSet<String> {
        let n = self.below(max + 1);
        (0..n).map(|_| self.pick(xs).to_owned()).collect()
    }
}

const NAMES: &[&str] = &["@org/app", "@org/core", "@org/net", "std/bytes", "@b/x"];
const CAPS: &[&str] = &[
    "Net",
    "Fail",
    "Ffi[LibA]",
    "Map[K,V]",
    "q\"uote",
    "tab\there",
    "é",
    "\u{1F600}",
];
const FEATURES: &[&str] = &["default", "tls", "x_y", "", "back\\slash"];
const VERSIONS: &[&str] = &["1.0.0", "1.2.3-alpha.1", "0.9.2", "2.0.0+build.01"];
const REQS: &[&str] = &[
    "^1",
    "~1.2",
    ">=1.2.3-alpha.1",
    "*",
    "0.9",
    ">1.0, <1.5",
    "1.*",
    "=1.2.3",
];
const SYS: &[&str] = &[
    "liburing ^2.6.0",
    "zlib >=1.2, <2",
    "bad",
    "zlib\u{3000}1.2",
    "libc nope",
];
const PATHS: &[&str] = &["build.vsp", "proto/", "a\\b", ""];

fn name(s: &str) -> PackageName {
    PackageName::parse(s).unwrap()
}

fn manifest(rng: &mut Rng, own: &str) -> Manifest {
    let mut dependencies = BTreeMap::new();
    for _ in 0..rng.below(3) {
        dependencies.insert(
            name(rng.pick(NAMES)),
            Dependency {
                version: VersionReq::parse(rng.pick(REQS)).unwrap(),
                effects: rng.some(CAPS, 2),
                optional: rng.chance(30),
                features: rng.some(FEATURES, 2),
                path: rng.chance(30).then(|| rng.pick(PATHS).to_owned()),
            },
        );
    }
    let mut features = BTreeMap::new();
    for _ in 0..rng.below(3) {
        features.insert(
            rng.pick(FEATURES).to_owned(),
            Feature {
                enables_effects: rng.some(CAPS, 2),
                deps: (0..rng.below(2)).map(|_| name(rng.pick(NAMES))).collect(),
                features: rng.some(FEATURES, 1),
                sys_deps: rng.some(SYS, 2),
            },
        );
    }
    Manifest {
        package: Package {
            name: name(own),
            version: Version::parse(rng.pick(VERSIONS)).unwrap(),
            edition: "2026".into(),
            license: rng.chance(30).then(|| "MIT".to_owned()),
            capability_ceiling: rng.some(CAPS, 3),
            forbid_unsafe_ffi: rng.chance(70),
            vsys: if rng.chance(20) {
                vec!["b.vsys".into(), "a.vsys".into()]
            } else {
                vec![]
            },
            objects: if rng.chance(10) {
                vec!["x.o".into()]
            } else {
                vec![]
            },
        },
        features,
        dependencies,
        build_sandbox: rng.chance(25).then(|| BuildSandbox {
            script: rng.chance(50).then(|| rng.pick(PATHS).to_owned()),
            wasi_read_paths: rng.some(PATHS, 2),
            fuel_limit: rng.chance(50).then(|| rng.next() >> rng.below(64)),
            allow_network: rng.chance(20),
        }),
        resolver: rng.chance(15).then(|| Resolver {
            policy: ResolverPolicy {
                optimize: vec!["size".into(), "speed".into()],
                min_repro_attestations: rng.chance(50).then(|| rng.next() as u32),
                block_unvetted_namespaces: rng.chance(50),
            },
        }),
        verification: rng.chance(15).then(|| Verification {
            export_lean4_specs: rng.some(FEATURES, 2),
        }),
    }
}

fn solution(rng: &mut Rng) -> Solution {
    let packages = (0..rng.below(4))
        .map(|_| Selection {
            manifest: {
                let name = rng.pick(NAMES);
                manifest(rng, name)
            },
            features: rng.some(FEATURES, 2),
        })
        .collect();
    Solution {
        schema_version: if rng.chance(10) { 2 } else { SCHEMA_VERSION },
        roots: (0..rng.below(3))
            .map(|_| RootRequirement {
                name: name(rng.pick(NAMES)),
                version: VersionReq::parse(rng.pick(REQS)).unwrap(),
                features: rng.some(FEATURES, 2),
            })
            .collect(),
        packages,
        system_packages: (0..rng.below(3))
            .map(|_| SystemSelection {
                name: rng.pick(&["zlib", "libc", "é"]).to_owned(),
                version: Version::parse(rng.pick(VERSIONS)).unwrap(),
            })
            .collect(),
    }
}

/// Generated lockfiles, compact and pretty.
fn generated(n: usize) -> Vec<Vec<u8>> {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let mut out = vec![];
    for _ in 0..n {
        let s = solution(&mut rng);
        out.push(serde_json::to_vec(&s).unwrap());
        out.push(serde_json::to_vec_pretty(&s).unwrap());
    }
    out
}

const MANIFEST: &str = r#"{"package":{"name":"@o/a","version":"1.0.0","edition":"2026"}}"#;

/// Hand-written cases: every shape serde accepts or refuses.
fn edge_cases() -> Vec<Vec<u8>> {
    let sel = format!(r#"{{"manifest":{MANIFEST}}}"#);
    let mut cases: Vec<String> = vec![
        String::new(),
        " ".into(),
        "{".into(),
        "{}".into(),
        "[]".into(),
        "[1]".into(),
        "[1, [], []]".into(),
        "[1, [], [], [], 5]".into(),
        "[1, [], [],]".into(),
        "[1, [], [] 5]".into(),
        "null".into(),
        "nul".into(),
        "true".into(),
        "tru".into(),
        "false".into(),
        "-".into(),
        "-0".into(),
        "-0.0".into(),
        "0".into(),
        "01".into(),
        "1.".into(),
        "1.5".into(),
        "1e".into(),
        "1e+".into(),
        "1e5".into(),
        "1e400".into(),
        "-1e400".into(),
        "1e-400".into(),
        "123456789012345678901234567890".into(),
        "1234567890123456789.5".into(),
        "0.1234567890123456789012345".into(),
        "1e99999999999".into(),
        "0e99999999999".into(),
        "1e-99999999999".into(),
        "\"x\"".into(),
        "\"\\u00e9\"".into(),
        "\"\\ud83d\\ude00\"".into(),
        "\"\\ud83d\"".into(),
        "\"\\ud83dx\"".into(),
        "\"\\ud83d\\x\"".into(),
        "\"\\ude00\"".into(),
        "\"\\u12\"".into(),
        "\"\\u12g4\"".into(),
        "\"\\q\"".into(),
        "\"a\tb\"".into(),
        "\"unterminated".into(),
        "\"esc\\".into(),
        "{\"schema_version\":1}".into(),
        "{\"schema_version\":1,\"roots\":[],\"packages\":[]}".into(),
        "{\"schema_version\":1,\"roots\":[],\"packages\":[],}".into(),
        "{\"schema_version\":1,\"roots\":[],\"packages\":[]} x".into(),
        "{\"schema_version\":1,\"roots\":[],\"packages\":[]}\n\n".into(),
        "{\"schema_version\":1 \"roots\":[]}".into(),
        "{\"schema_version\" 1}".into(),
        "{\"schema_version\":}".into(),
        "{1:2}".into(),
        "{\"a\":1}".into(),
        "{\"schema_version\":1,\"schema_version\":1}".into(),
        "{\"schema_version\":-1}".into(),
        "{\"schema_version\":4294967296}".into(),
        "{\"schema_version\":1.0}".into(),
        "{\"schema_version\":\"1\"}".into(),
        "{\"schema_version\":null}".into(),
        "{\"schema_version\":[]}".into(),
        "{\"schema_version\":{}}".into(),
        "{\"schema_version\":1,\"roots\":{}}".into(),
        "{\"schema_version\":1,\"roots\":[\"@acme/app\"]}".into(),
        "{\"schema_version\":1,\"roots\":[{\"name\":\"bad\"}]}".into(),
        "{\"schema_version\":1,\"roots\":[{\"name\":\"@a/b\" , \"version\":\"^1\"} , ],\"packages\":[]}"
            .into(),
        "{\"schema_version\":1,\"roots\":[{\"name\":\"@a/b\",\"version\":\"x\"}]}".into(),
        "{\"schema_version\":1,\"roots\":[{\"name\":\"@a/b\",\"version\":\"1.2.3.4\"}]}".into(),
        "{\"schema_version\":1,\"roots\":[{\"name\":\"@a/b\",\"version\":\"^1\",\"features\":[\"b\",\"a\",\"b\"]}],\"packages\":[]}"
            .into(),
        "{\"schema_version\":1,\"roots\":[[\"@a/b\",\"^1\"]],\"packages\":[]}".into(),
        "{\"schema_version\":1,\"roots\":[[\"@a/b\"]],\"packages\":[]}".into(),
        "{\"schema_version\":1,\"roots\":[[\"@a/b\",\"^1\",[],1]],\"packages\":[]}".into(),
        "{\"schema_version\":1,\"roots\":[],\"packages\":[],\"system_packages\":[{\"name\":\"z\",\"version\":\"1\"}]}"
            .into(),
        "{\"schema_version\":1,\"roots\":[],\"packages\":[],\"system_packages\":[[\"z\",\"1.0.0\"]]}".into(),
        "{\"schema_version\":1,\"roots\":[],\"packages\":[],\"system_packages\":null}".into(),
        format!("{{\"schema_version\":1,\"roots\":[],\"packages\":[{sel}]}}"),
        format!("{{\"schema_version\":1,\"roots\":[],\"packages\":[{sel},{sel}]}}"),
        format!("[1,[],[{sel}]]"),
        "{\"schema_version\":1,\"roots\":[],\"packages\":[{\"manifest\":{}}]}".into(),
        "{\"schema_version\":1,\"roots\":[],\"packages\":[{\"manifest\":{\"package\":{\"name\":\"@o/a\",\"version\":\"1.0.0\",\"edition\":\"2026\",\"license\":null,\"vsys\":[\"b\",\"a\"],\"objects\":[]}}}]}"
            .into(),
        "{\"schema_version\":1,\"roots\":[],\"packages\":[{\"manifest\":{\"package\":[\"@o/a\",\"1.0.0\",\"2026\"]}}]}"
            .into(),
        "{\"schema_version\":1,\"roots\":[],\"packages\":[{\"manifest\":{\"package\":[\"@o/a\",\"1.0.0\",\"2026\",null,[],true,[],[]]}}]}"
            .into(),
        "{\"schema_version\":1,\"roots\":[],\"packages\":[{\"manifest\":{\"package\":{\"name\":\"@o/a\",\"version\":\"1.0.0\",\"edition\":\"2026\",\"forbid_unsafe_ffi\":1}}}]}"
            .into(),
        "{\"schema_version\":1,\"roots\":[],\"packages\":[{\"manifest\":{\"package\":{\"name\":\"@o/a\",\"version\":\"1.0.0\",\"edition\":\"2026\"},\"dependencies\":{\"@o/b\":{\"version\":\"^1\"},\"@o/a\":{\"version\":\"*\",\"path\":\"x\"},\"@o/b\":{\"version\":\"~2\"}}}}]}"
            .into(),
        "{\"schema_version\":1,\"roots\":[],\"packages\":[{\"manifest\":{\"package\":{\"name\":\"@o/a\",\"version\":\"1.0.0\",\"edition\":\"2026\"},\"dependencies\":{\"bad\" :{\"version\":\"^1\"}}}}]}"
            .into(),
        "{\"schema_version\":1,\"roots\":[],\"packages\":[{\"manifest\":{\"package\":{\"name\":\"@o/a\",\"version\":\"1.0.0\",\"edition\":\"2026\"},\"features\":{\"b\":{},\"a\":{\"deps\":[\"bad\" , \"@x/y\"]}}}}]}"
            .into(),
        "{\"schema_version\":1,\"roots\":[],\"packages\":[{\"manifest\":{\"package\":{\"name\":\"@o/a\",\"version\":\"1.0.0\",\"edition\":\"2026\"},\"build-sandbox\":{\"fuel_limit\":-5}}}]}"
            .into(),
        "{\"schema_version\":1,\"roots\":[],\"packages\":[{\"manifest\":{\"package\":{\"name\":\"@o/a\",\"version\":\"1.0.0\",\"edition\":\"2026\"},\"build-sandbox\":{\"fuel_limit\":1.5e3}}}]}"
            .into(),
        "{\"schema_version\":1,\"roots\":[],\"packages\":[{\"manifest\":{\"package\":{\"name\":\"@o/a\",\"version\":\"1.0.0\",\"edition\":\"2026\"},\"resolver\":{\"policy\":{\"min_repro_attestations\":99999999999}}}}]}"
            .into(),
        "{\"schema_version\":1,\"roots\":[],\"packages\":[{\"manifest\":{\"package\":{\"name\":\"@o/a\",\"version\":\"1.0.0\",\"edition\":\"2026\"},\"resolver\":{}}}]}"
            .into(),
        "{\"schema_version\":1,\"roots\":[],\"packages\":[{\"manifest\":{\"package\":{\"name\":\"@o/a\",\"version\":\"1.0.0\",\"edition\":\"2026\"},\"verification\":{\"bogus\":1}}}]}"
            .into(),
        "{\"schema_version\":1,\"roots\":[],\"packages\":[{\"manifest\":{\"package\":{\"name\":\"@o/a\",\"version\":\"1.0.0\",\"edition\":\"2026\"},\"verification\":{\"export_lean4_specs\":[1.25]}}}]}"
            .into(),
        "{\"schema_version\":1,\"roots\":[],\"packages\":[{\"features\":[],\"manifest\":{\"package\":{\"name\":\"@o/a\",\"version\":\"1.0.0\",\"edition\":\"2026\",\"capability_ceiling\":[\"\\u0000\",\"\\u001f\",\"\\\"\"]}}}]}"
            .into(),
        "{\"schema_version\":1,\"roots\":[],\"packages\":[{\"manifest\":{\"package\":{\"name\":\"@o/a\",\"version\":\"1.0.0\",\"edition\":\"2026\",\"capability_ceiling\":[\"x\" 1]}}}]}"
            .into(),
        "{\"schema_version\":1,\"roots\":[],\"packages\":[{\"manifest\":{\"package\":{\"name\":\"@o/a\",\"version\":\"1.0.0\",\"edition\":\"2026\",\"license\":nul}}}]}"
            .into(),
        "{\"schema_version\":1,\"roots\":[],\"packages\":[{\"manifest\":{\"package\":{\"name\":\"@o/a\",\"version\":\"1.0.0\",\"edition\":\"2026\",\"license\":\"x\"}},\"extra\":1}]}"
            .into(),
        // The fixture of tests/self_host_cli_commands.rs.
        "{\n  \"schema_version\": 1,\n  \"roots\": [\"@acme/app\"],\n  \"packages\": []\n}".into(),
        "{not-json".into(),
    ];
    for big in [
        1.5,
        100.0,
        1e16,
        1e15,
        0.0001,
        0.00001,
        1e-7,
        123.456,
        5e-324,
        1.7976931348623157e308,
    ] {
        cases.push(format!("{{\"schema_version\":{big:e}}}"));
        cases.push(format!("{{\"schema_version\":-{big}}}"));
    }
    let mut out: Vec<Vec<u8>> = cases.into_iter().map(String::into_bytes).collect();
    // Invalid UTF-8 in and out of strings.
    out.push(b"{\"schema_version\":1,\"roots\":[{\"name\":\"@a/\xff\"}]}".to_vec());
    out.push(b"{\"schema_version\":1,\"roots\":[{\"name\":\"\\u00e9\xc3\"}]}".to_vec());
    out.push(b"{\"sch\xe2\x82ema\":1}".to_vec());
    out.push(b"\xef\xbb\xbf{}".to_vec());
    out
}

/// Byte-level damage: truncations, deletions, insertions and swaps.
fn damaged(bases: &[Vec<u8>], seed: u64, per: usize) -> Vec<Vec<u8>> {
    const BYTES: &[u8] = b"\"\\,:[]{}01-.eE nt\n\x00\xff";
    let mut rng = Rng(seed);
    let mut out = vec![];
    for base in bases {
        if base.is_empty() {
            continue;
        }
        for _ in 0..per {
            let mut s = base.clone();
            let at = rng.below(s.len());
            match rng.below(4) {
                0 => s.truncate(at),
                1 => {
                    s.remove(at);
                }
                2 => s.insert(at, BYTES[rng.below(BYTES.len())]),
                _ => s[at] = BYTES[rng.below(BYTES.len())],
            }
            out.push(s);
        }
    }
    out
}

#[test]
fn generated_lockfiles_match_serde_json() {
    assert_same("generated", &generated(200));
}

#[test]
fn edge_cases_match_serde_json() {
    assert_same("edge cases", &edge_cases());
}

#[test]
fn damaged_lockfiles_match_serde_json() {
    let mut bases = generated(40);
    bases.extend(edge_cases());
    assert_same("damaged", &damaged(&bases, 0x2545_f491_4f6c_dd1d, 25));
}
