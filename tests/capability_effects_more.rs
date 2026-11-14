//! Wave-seven capability effects (design §23.2, §32.1-§32.2, §36): the
//! built-in secret operations and their bitwise intrinsics, `ConstDenorm`,
//! `RtBus`, interprocedural secret-parameter inference, taint through
//! handler resumes, the policy inputs, and the Lean model of the taint rules.
use std::path::{Path, PathBuf};
use std::process::Command;
use vesper::capability_effects::{self, Capability};
use vesper::{check, native, numeric::IntegerKind, parser};

fn dir(kind: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/capability_effects/more")
        .join(kind)
}

fn fixtures(kind: &str) -> Vec<PathBuf> {
    let dir = dir(kind);
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "ves"))
        .collect();
    files.sort();
    assert!(!files.is_empty(), "no fixtures in {}", dir.display());
    files
}

fn checked(source: &str) -> check::Checked {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "parse: {:?}", p.diagnostics);
    check::check(&p.module)
}

fn codes(source: &str) -> Vec<String> {
    checked(source)
        .diagnostics
        .iter()
        .map(|d| d.code.clone())
        .collect()
}

fn vesper(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_vesper"))
        .args(args)
        .output()
        .unwrap()
}

/// The evaluator's value for `main`, unoptimized and optimized (they must
/// agree).
fn evaluate(world: &check::World) -> String {
    let mut values = vec![];
    for optimize in [false, true] {
        let mut runtime = vesper::runtime::Runtime::new(world);
        runtime.optimize = optimize;
        values.push(runtime.run("main", vec![]).unwrap().to_string());
    }
    assert_eq!(values[0], values[1], "optimizer changed the value");
    values.remove(0)
}

fn stdout(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

const SECRETS: &str = "copy struct SecU8 { v: u8 } copy struct SecU32 { v: u32 } \
                       copy struct SecU64 { value: u64 } copy struct SecMask64 { bits: u64 } ";

#[test]
fn accepted_fixtures_check_and_agree_on_both_backends() {
    let expected = [
        ("bit_intrinsics.ves", "534"),
        ("const_denorm.ves", "1"),
        ("inferred_params.ves", "71"),
        ("resume_public.ves", "42"),
        ("rtbus.ves", "64"),
        ("secret_ops.ves", "1"),
    ];
    let files = fixtures("accept");
    assert_eq!(files.len(), expected.len());
    for (path, (name, value)) in files.iter().zip(expected) {
        assert!(path.ends_with(name), "{}", path.display());
        let p = path.to_str().unwrap();
        let out = vesper(&["check", p]);
        assert!(
            out.status.success(),
            "{p}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        for backend in ["run", "jit"] {
            let out = vesper(&[backend, p]);
            assert!(
                out.status.success(),
                "{backend} {p}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            assert_eq!(stdout(&out), value, "{backend} {p}");
        }
    }
}

/// `// expect: E0xxx` on the first line of a rejected fixture.
fn expected_code(source: &str) -> String {
    source
        .lines()
        .next()
        .and_then(|l| l.strip_prefix("// expect: "))
        .expect("rejected fixtures start with `// expect: E0xxx`")
        .trim()
        .to_string()
}

#[test]
fn rejected_fixtures_report_their_rule_inside_the_source() {
    let mut seen = std::collections::BTreeSet::new();
    for path in fixtures("reject") {
        let source = std::fs::read_to_string(&path).unwrap();
        let code = expected_code(&source);
        let c = checked(&source);
        assert!(
            c.diagnostics.iter().any(|d| d.code == code),
            "{}: expected {code}, got {:?}",
            path.display(),
            c.diagnostics
        );
        for d in &c.diagnostics {
            // Diagnostics never point into the secret prelude, whose spans
            // lie after the source.
            assert!(
                d.span.start < d.span.end && d.span.end <= source.len(),
                "{}: {d:?}",
                path.display()
            );
        }
        let out = vesper(&["check", path.to_str().unwrap()]);
        assert!(!out.status.success(), "{}", path.display());
        assert!(
            String::from_utf8_lossy(&out.stderr).contains(&format!("[{code}]")),
            "{}: {}",
            path.display(),
            String::from_utf8_lossy(&out.stderr)
        );
        seen.insert(code);
    }
    // Every new rule has a negative fixture (E0700/E0701/E0100 for the new
    // ConstDenorm capability, E0702/E0703 for inference and resumes).
    for code in [
        "E0100", "E0200", "E0700", "E0701", "E0702", "E0703", "E0707", "E0720", "E0724", "E0725",
        "E0732",
    ] {
        assert!(seen.contains(code), "no fixture for {code}");
    }
}

#[test]
fn fixtures_are_canonically_formatted() {
    let out = vesper(&["fmt", "--check", dir("").to_str().unwrap()]);
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn const_denorm_is_a_reserved_capability() {
    assert_eq!(
        Capability::from_name("ConstDenorm"),
        Some(Capability::ConstDenorm)
    );
    assert!(Capability::ALL.contains(&Capability::ConstDenorm));
    assert!(capability_effects::is_capability_effect("ConstDenorm"));
    // RtBus is an ordinary resumable effect, not a capability.
    assert!(!capability_effects::is_capability_effect("RtBus"));
    // Callers are never asked for it, and it takes no arguments.
    assert!(codes("fn f() -> u64 eff [ConstDenorm] { 1 } fn main() -> u64 { f() }").is_empty());
    assert!(
        codes("fn f() -> u64 eff [ConstDenorm[u64]] { 1 } fn main() -> u64 { f() }")
            .contains(&"E0400".to_string())
    );
    // A DetFloat closure inside a ConstDenorm body is rejected too.
    let closure = "fn f(x: f64) -> f64 eff [ConstDenorm] { let g = |y: f64| -> f64 eff [DetFloat] { y * 2.0 }; g(x) } \
                   fn main() -> f64 { f(1.0) }";
    assert!(
        codes(closure).contains(&"E0732".to_string()),
        "{:?}",
        codes(closure)
    );
}

#[test]
fn bitwise_intrinsics_match_rust_semantics() {
    let kinds = [
        IntegerKind::U8,
        IntegerKind::U32,
        IntegerKind::U64,
        IntegerKind::I64,
    ];
    let samples: [i128; 9] = [0, 1, 2, 7, 63, 64, 65, 200, 255];
    for kind in kinds {
        for &a in &samples {
            for &b in &samples {
                let (a, b) = (kind.wrap(a * 0x0101_0101), kind.wrap(b));
                let got = |n| capability_effects::bitwise(n, kind, a, b).unwrap();
                let expect = match kind {
                    IntegerKind::U8 => {
                        let (x, y) = (a as u8, b as u8);
                        [
                            x & y,
                            x | y,
                            x ^ y,
                            x.wrapping_shl(y as u32),
                            x.wrapping_shr(y as u32),
                        ]
                        .map(i128::from)
                    }
                    IntegerKind::U32 => {
                        let (x, y) = (a as u32, b as u32);
                        [x & y, x | y, x ^ y, x.wrapping_shl(y), x.wrapping_shr(y)].map(i128::from)
                    }
                    IntegerKind::U64 => {
                        let (x, y) = (a as u64, b as u64);
                        [
                            x & y,
                            x | y,
                            x ^ y,
                            x.wrapping_shl(y as u32),
                            x.wrapping_shr(y as u32),
                        ]
                        .map(i128::from)
                    }
                    IntegerKind::I64 => {
                        let (x, y) = (a as i64, b as i64);
                        [
                            x & y,
                            x | y,
                            x ^ y,
                            x.wrapping_shl(y as u32),
                            x.wrapping_shr(y as u32),
                        ]
                        .map(i128::from)
                    }
                };
                let names = ["bit_and", "bit_or", "bit_xor", "bit_shl", "bit_shr"];
                for (n, e) in names.iter().zip(expect) {
                    assert_eq!(got(n), e, "{n} {kind:?} {a} {b}");
                }
            }
        }
    }
    assert_eq!(
        capability_effects::bitwise("wrapping_add", IntegerKind::U64, 1, 1),
        None
    );
}

/// The evaluator and the Cranelift JIT agree with Rust on generated bitwise
/// programs, including shifts at and beyond the width and negative i64.
#[test]
fn bitwise_intrinsics_agree_across_backends() {
    let cases: [(&str, &str, i128, i128); 10] = [
        ("u8", "bit_shl", 129, 9),
        ("u8", "bit_shr", 255, 7),
        ("u32", "bit_shl", 4_294_967_295, 31),
        ("u32", "bit_xor", 4_294_967_295, 65_535),
        ("u64", "bit_shl", 3, 64),
        ("u64", "bit_shr", 18_446_744_073_709_551_615, 63),
        ("u64", "bit_and", 18_446_744_073_709_551_615, 12_345),
        ("i64", "bit_shr", -9_223_372_036_854_775_808, 1),
        ("i64", "bit_or", -8, 3),
        ("i64", "bit_shl", -1, 63),
    ];
    for (ty, op, a, b) in cases {
        let kind = IntegerKind::from_type(&vesper::ast::Type::named(ty)).unwrap();
        let expected = capability_effects::bitwise(op, kind, a, b).unwrap();
        let source =
            format!("fn main() -> {ty} {{ let a: {ty} = {a}; let b: {ty} = {b}; {op}(a, b) }}");
        let c = checked(&source);
        assert!(c.diagnostics.is_empty(), "{source}: {:?}", c.diagnostics);
        let jit = native::jit(&c.world, "main").unwrap().to_string();
        assert_eq!(jit, expected.to_string(), "jit {source}");
        assert_eq!(evaluate(&c.world), expected.to_string(), "run {source}");
    }
}

#[test]
fn secret_operations_exist_only_when_declared_and_used() {
    // Not mentioned: nothing is added, so existing programs are unchanged.
    let c = checked(&format!("{SECRETS} fn main() -> u64 {{ 1 }}"));
    assert!(c.diagnostics.is_empty(), "{:?}", c.diagnostics);
    assert!(!c.world.functions.keys().any(|f| f.starts_with("Sec")));
    assert!(!c.world.functions.contains_key("load_sec_u64le"));
    // Mentioned: only the named operations are added.
    let c = checked(&format!(
        "{SECRETS} fn f(a: SecU64, b: SecU64) -> SecU64 eff [ConstTime] {{ a.bit_xor(b) }} fn main() -> u64 {{ 1 }}"
    ));
    assert!(c.diagnostics.is_empty(), "{:?}", c.diagnostics);
    for f in [
        "SecU64::bit_xor",
        "SecU8::bit_xor",
        "SecU32::bit_xor",
        "SecMask64::bit_xor",
    ] {
        assert!(c.world.functions.contains_key(f), "{f}");
    }
    assert!(!c.world.functions.contains_key("SecU64::ct_eq"));
    // Every added function is a ConstTime function whose spans follow the
    // module, so they never collide with the checker's span-keyed tables.
    let source = format!(
        "{SECRETS} fn f(a: SecU8, b: SecU8) -> SecMask64 eff [ConstTime] {{ a.ct_eq(b) }} fn main() -> u64 {{ 1 }}"
    );
    let c = checked(&source);
    assert!(c.diagnostics.is_empty(), "{:?}", c.diagnostics);
    let added = &c.world.functions["SecU8::ct_eq"];
    assert!(added.span.start > source.len());
    assert!(capability_effects::capabilities(&added.effects).contains(&Capability::ConstTime));
    // Undeclared or non-canonical secret types get no operations.
    let odd = "copy struct SecU64 { hi: u32, lo: u32 } \
               fn f(a: SecU64, b: SecU64) -> SecU64 eff [ConstTime] { a.bit_xor(b) } fn main() -> u64 { 1 }";
    assert!(!codes(odd).is_empty());
    // A user definition takes precedence over the built-in one.
    let own = format!(
        "{SECRETS} impl SecU64 {{ fn bit_xor(self, other: SecU64) -> SecU64 eff [ConstTime] {{ other }} }} \
         fn main() -> u64 {{ let a = SecU64 {{ value: 1 }}; let b = a.bit_xor(SecU64 {{ value: 2 }}); \
         let m = b.ct_eq(SecU64 {{ value: 2 }}); if m.declassify_bool() {{ 1 }} else {{ 0 }} }}"
    );
    let c = checked(&own);
    assert!(c.diagnostics.is_empty(), "{:?}", c.diagnostics);
    assert_eq!(native::jit(&c.world, "main").unwrap().to_string(), "1");
    // So does a user function named like a bitwise intrinsic.
    let shadow = "fn bit_xor(a: u64, b: u64) -> u64 { wrapping_add(a, b) } fn main() -> u64 { bit_xor(2, 3) }";
    let c = checked(shadow);
    assert!(c.diagnostics.is_empty(), "{:?}", c.diagnostics);
    assert_eq!(native::jit(&c.world, "main").unwrap().to_string(), "5");
}

#[test]
fn secret_operations_compute_the_design_semantics() {
    // ct_eq/ct_is_zero masks are all ones or zero; ct_select picks bitwise.
    let source = format!(
        "{SECRETS}
        fn probe(a: SecU32, b: SecU32, x: SecU64, y: SecU64) -> SecU64 eff [ConstTime] {{
            let m: SecMask64 = a.ct_eq(b);
            let SecMask64 {{ bits }} = m;
            let z: SecMask64 = x.ct_is_zero();
            let SecMask64 {{ bits: zb }} = z;
            let s: SecU64 = m.ct_select(x, y);
            let SecU64 {{ value }} = s;
            SecU64 {{ value: wrapping_add(wrapping_add(bit_and(bits, 1), bit_and(zb, 2)), value) }}
        }}
        fn declassify(s: SecU64) -> u64 eff [ConstTime] {{ let SecU64 {{ value }} = s; value }}
        fn main() -> u64 {{
            let same = declassify(probe(SecU32 {{ v: 7 }}, SecU32 {{ v: 7 }}, SecU64 {{ value: 0 }}, SecU64 {{ value: 100 }}));
            let differ = declassify(probe(SecU32 {{ v: 7 }}, SecU32 {{ v: 8 }}, SecU64 {{ value: 40 }}, SecU64 {{ value: 100 }}));
            wrapping_add(wrapping_mul(same, 1000), differ)
        }}"
    );
    let c = checked(&source);
    assert!(c.diagnostics.is_empty(), "{:?}", c.diagnostics);
    // same: 1 + 2 + 0 = 3; differ: 0 + 0 + 100 = 100.
    let jit = native::jit(&c.world, "main").unwrap().to_string();
    assert_eq!(jit, "3100");
    assert_eq!(evaluate(&c.world), "3100");
    // load_sec_u64le is little endian.
    let load = format!(
        "{SECRETS} fn declassify_open(s: SecU64) -> u64 eff [ConstTime] {{ let SecU64 {{ value }} = s; value }} \
         fn main() -> u64 {{ let b: (u8, u8, u8, u8, u8, u8, u8, u8) = (1, 2, 0, 0, 0, 0, 0, 128); declassify_open(load_sec_u64le(b)) }}"
    );
    let c = checked(&load);
    assert!(c.diagnostics.is_empty(), "{:?}", c.diagnostics);
    let expected = u64::from_le_bytes([1, 2, 0, 0, 0, 0, 0, 128]).to_string();
    assert_eq!(native::jit(&c.world, "main").unwrap().to_string(), expected);
}

#[test]
fn secret_operations_pass_the_constant_time_verifier() {
    let path = dir("accept").join("secret_ops.ves");
    let out_path = std::env::temp_dir().join(format!("vesper-ct-ops-{}.o", std::process::id()));
    let out = vesper(&[
        "emit",
        path.to_str().unwrap(),
        "--emit",
        "object",
        "-o",
        out_path.to_str().unwrap(),
    ]);
    let _ = std::fs::remove_file(&out_path);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert!(stderr.contains("const-time"), "{stderr}");
    // Lean extraction has no bitwise operators and rejects them by name.
    let out = vesper(&["emit", path.to_str().unwrap(), "--emit", "lean"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("bit_"));
}

#[test]
fn inference_is_limited_to_direct_calls_from_const_time_code() {
    // Plain callers do not make a parameter secret.
    let plain = "fn pick(x: u64) -> u64 eff [ConstTime] { if x == 0 { 1 } else { 2 } } \
                 fn main() -> u64 { pick(3) }";
    assert!(codes(plain).is_empty(), "{:?}", codes(plain));
    // A function used as a value gets no inferred outputs: writing a secret
    // into its public inout parameter stays an error.
    let taken = "copy struct Secret[T] { value: T } \
                 fn store(inout out: u64, s: Secret[u64]) -> Unit eff [ConstTime] { out = s.value; } \
                 fn main() -> u64 { let f = store; 0 }";
    assert!(
        codes(taken).contains(&"E0705".to_string()),
        "{:?}",
        codes(taken)
    );
    // ... while a directly called one returns the secret through the place.
    let direct = "copy struct Secret[T] { value: T } \
                  fn store(inout out: u64, s: Secret[u64]) -> Unit eff [ConstTime] { out = s.value; } \
                  fn main() -> u64 { let o: u64 = 0; store(inout o, Secret { value: 1 }); o }";
    assert!(codes(direct).is_empty(), "{:?}", codes(direct));
    // Inferred parameters still reject a secret result returned as public
    // when the secret did not come from the caller.
    let own = "copy struct Secret[T] { value: T } \
               fn mint(x: u64) -> u64 eff [ConstTime] { let s: Secret[u64] = Secret { value: x }; s.value } \
               fn main() -> u64 { mint(1) }";
    assert!(
        codes(own).contains(&"E0706".to_string()),
        "{:?}",
        codes(own)
    );
}

#[test]
fn resumed_values_are_tracked_by_taint() {
    // A public resume keeps perform sites public, even in a stateful
    // handler whose state is secret.
    let ok = "copy struct Secret[T] { value: T } linear effect Next { fn next() -> u64; } \
              fn run(s: Secret[u64]) -> Secret[u64] eff [ConstTime] { \
                let Secret { value } = s; \
                let r = handle Next state acc = value in { let n: u64 = Next::next(); let w: u64 = 2; if n == 0 { w } else { w } } \
                  with { return (v, acc) => wrapping_add(v, acc), next(acc, resume) => resume(5, wrapping_add(acc, 1)), }; \
                Secret { value: r } } \
              fn main() -> u64 { 0 }";
    assert!(codes(ok).is_empty(), "{:?}", codes(ok));
    // Before this lane the perform site's value was public by type.
    let leak = "copy struct Secret[T] { value: T } linear effect Key { fn fetch() -> u64; } \
                fn run(s: Secret[u64]) -> Secret[u64] eff [ConstTime] { \
                  let Secret { value } = s; \
                  let r = handle Key in { let k: u64 = Key::fetch(); let w: u64 = 2; if k == 0 { w } else { w } } \
                    with { return v => v, fetch(resume) => resume(value), }; \
                  Secret { value: r } } \
                fn main() -> u64 { 0 }";
    assert!(
        codes(leak).contains(&"E0702".to_string()),
        "{:?}",
        codes(leak)
    );
}

#[test]
fn policies_report_inferred_parameters_and_const_denorm() {
    let source = std::fs::read_to_string(dir("accept").join("inferred_params.ves")).unwrap();
    let p = capability_effects::capability_policies(&checked(&source)).unwrap();
    assert_eq!(p.schema, capability_effects::POLICY_SCHEMA);
    let find = |name: &str| p.const_time.iter().find(|c| c.function == name).unwrap();
    assert_eq!(find("mix").secret_params, [0, 1]);
    assert_eq!(find("mix").inferred_secret_params, [0, 1]);
    assert_eq!(find("twice").inferred_secret_params, [0]);
    assert_eq!(find("accumulate").inferred_secret_params, [1]);
    // Secret by type, not inferred.
    assert_eq!(find("digest").secret_params, [0]);
    assert!(find("digest").inferred_secret_params.is_empty());
    let source = std::fs::read_to_string(dir("accept").join("const_denorm.ves")).unwrap();
    let p = capability_effects::capability_policies(&checked(&source)).unwrap();
    let names: Vec<_> = p.const_denorm.iter().map(|c| c.function.as_str()).collect();
    assert_eq!(names, ["decay", "step"]);
    assert!(p.det_float.is_empty());
}

#[test]
fn realtime_attribute_syntax_is_not_available() {
    // `@realtime(stack_budget_bytes = .., period_us = ..)` needs §16.4
    // attribute syntax, which other agents own: it does not parse today, and
    // the policy's stack budget stays unset.
    let p =
        parser::parse("@realtime(stack_budget_bytes = 4096) fn f() -> u64 eff [Realtime] { 1 }");
    assert!(!p.diagnostics.is_empty());
}

#[test]
fn lean_model_checks_without_axioms_or_sorry() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("lean/CapabilityEffects.lean");
    let text = std::fs::read_to_string(&path).unwrap();
    for word in ["sorry", "admit", "axiom "] {
        assert!(!text.contains(word), "{word} in {}", path.display());
    }
    for theorem in [
        "theorem noninterference",
        "theorem ok_mono",
        "theorem call_ok_of_summary",
    ] {
        assert!(text.contains(theorem), "{theorem}");
    }
    let Ok(lean) = std::env::var("VESPER_LEAN") else {
        eprintln!("Lean check skipped: set VESPER_LEAN to the pinned Lean 4.24.0 binary");
        return;
    };
    let out = Command::new(lean).arg(&path).output().unwrap();
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "{all}");
    assert!(!all.contains("declaration uses 'sorry'"), "{all}");
}
