//! Source capability effects `ConstTime`, `Realtime` and `DetFloat`
//! (design §23.2, §32.2, §36.2): fixtures through `vesper check`, typed
//! diagnostics with spans, and the verifier policy inputs.
use std::path::{Path, PathBuf};
use std::process::Command;
use vesper::capability_effects::{self, Capability, PolicyError};
use vesper::{check, parser};

fn fixtures(kind: &str) -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/capability_effects")
        .join(kind);
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

fn vesper(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_vesper"))
        .args(args)
        .output()
        .unwrap()
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
fn accepted_fixtures_check_and_run_on_both_backends() {
    let expected = [
        ("closures.ves", "6.5"),
        ("consttime.ves", "true"),
        ("detfloat.ves", "0.75"),
        ("realtime.ves", "42"),
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
            assert_eq!(
                String::from_utf8_lossy(&out.stdout).trim(),
                value,
                "{backend} {p}"
            );
        }
    }
}

#[test]
fn rejected_fixtures_report_typed_diagnostics_with_spans() {
    let mut codes = std::collections::BTreeSet::new();
    for path in fixtures("reject") {
        let source = std::fs::read_to_string(&path).unwrap();
        let code = expected_code(&source);
        let c = checked(&source);
        let d = c
            .diagnostics
            .iter()
            .find(|d| d.code == code)
            .unwrap_or_else(|| {
                panic!(
                    "{}: expected {code}, got {:?}",
                    path.display(),
                    c.diagnostics
                )
            });
        assert!(
            d.span.start < d.span.end && d.span.end <= source.len(),
            "{}: span {:?}",
            path.display(),
            d.span
        );
        // The span points into the code, not the header comment.
        assert!(
            d.span.start > source.find('\n').unwrap(),
            "{}: {:?}",
            path.display(),
            d.span
        );
        let out = vesper(&["check", path.to_str().unwrap()]);
        assert!(!out.status.success(), "{}", path.display());
        assert!(
            String::from_utf8_lossy(&out.stderr).contains(&format!("[{code}]")),
            "{}: {}",
            path.display(),
            String::from_utf8_lossy(&out.stderr)
        );
        codes.insert(code);
    }
    // Every rule has at least one negative fixture.
    for code in [
        "E0100", "E0700", "E0701", "E0702", "E0703", "E0704", "E0705", "E0706", "E0707", "E0708",
        "E0720", "E0721", "E0722", "E0723", "E0730", "E0731",
    ] {
        assert!(codes.contains(code), "no fixture for {code}");
    }
}

#[test]
fn diagnostics_point_at_the_offending_expression() {
    let source = "fn plain(x: f64) -> f64 { x } fn rt(x: f64) -> f64 eff [Realtime] { plain(x) }";
    let c = checked(source);
    let d = c
        .diagnostics
        .iter()
        .find(|d| d.code == "E0700")
        .expect("E0700");
    assert_eq!(&source[d.span.clone()], "plain(x)");
    assert!(
        d.message.contains("'plain'") && d.message.contains("Realtime"),
        "{}",
        d.message
    );
    assert!(d.help.is_some());

    let source = "copy struct Secret[T] { value: T } fn f(s: Secret[u64]) -> Secret[u64] eff [ConstTime] { let Secret { value } = s; if value == 0 { s } else { s } }";
    let c = checked(source);
    let d = c
        .diagnostics
        .iter()
        .find(|d| d.code == "E0702")
        .expect("E0702");
    assert_eq!(&source[d.span.clone()], "value == 0");
}

#[test]
fn capability_effects_are_not_requested_from_callers() {
    // Plain code enters a capability by calling a function carrying it; no
    // handler or row entry is needed, and the call discipline only binds
    // callees.
    for cap in ["ConstTime", "Realtime", "DetFloat"] {
        let source =
            format!("fn inner() -> u64 eff [{cap}] {{ 1 }} fn main() -> u64 {{ inner() }}");
        let c = checked(&source);
        assert!(c.diagnostics.is_empty(), "{cap}: {:?}", c.diagnostics);
    }
    // A callee may carry more capabilities than its caller.
    let c = checked(
        "fn both() -> u64 eff [Realtime, DetFloat] { 1 } fn rt() -> u64 eff [Realtime] { both() }",
    );
    assert!(c.diagnostics.is_empty(), "{:?}", c.diagnostics);
    // But not fewer.
    let c = checked(
        "fn rt() -> u64 eff [Realtime] { 1 } fn both() -> u64 eff [Realtime, DetFloat] { rt() }",
    );
    let d = c
        .diagnostics
        .iter()
        .find(|d| d.code == "E0700")
        .expect("E0700");
    assert!(
        d.message.contains("DetFloat") && !d.message.ends_with("Realtime, DetFloat"),
        "{}",
        d.message
    );
}

#[test]
fn capability_rows_take_no_type_arguments() {
    let c = checked("fn f() -> u64 eff [Realtime[u64]] { 1 }");
    assert!(
        c.diagnostics
            .iter()
            .any(|d| d.code == "E0400" && d.message.contains("no type arguments")),
        "{:?}",
        c.diagnostics
    );
}

#[test]
fn mutual_recursion_is_rejected_once_per_call_site() {
    let source = "fn a(n: u64) -> u64 eff [Realtime] { if n == 0 { 0 } else { b(wrapping_sub(n, 1)) } } \
                  fn b(n: u64) -> u64 eff [Realtime] { a(n) } fn main() -> u64 { a(2) }";
    let c = checked(source);
    let spans: Vec<_> = c
        .diagnostics
        .iter()
        .filter(|d| d.code == "E0722")
        .map(|d| &source[d.span.clone()])
        .collect();
    assert_eq!(spans, ["b(wrapping_sub(n, 1))", "a(n)"]);
    // Recursion outside Realtime is unaffected.
    let c = checked(
        "fn a(n: u64) -> u64 { if n == 0 { 0 } else { a(wrapping_sub(n, 1)) } } fn main() -> u64 { a(2) }",
    );
    assert!(c.diagnostics.is_empty(), "{:?}", c.diagnostics);
}

#[test]
fn single_arm_destructuring_match_on_a_secret_is_not_a_branch() {
    let ok = "copy struct Secret[T] { value: T } fn f(s: Secret[u64]) -> Secret[u64] eff [ConstTime] { match s { Secret { value } => Secret { value: wrapping_add(value, 1) } } }";
    let c = checked(ok);
    assert!(c.diagnostics.is_empty(), "{:?}", c.diagnostics);
    let short = "copy struct Secret[T] { value: T } fn f(s: Secret[bool], p: bool) -> Secret[bool] eff [ConstTime] { let Secret { value } = s; Secret { value: p && value } }";
    // A public left operand may short-circuit; the secret is only evaluated.
    let c = checked(short);
    assert!(c.diagnostics.is_empty(), "{:?}", c.diagnostics);
}

#[test]
fn existing_examples_are_unaffected() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples");
    let mut n = 0;
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|x| x == "ves") {
            let source = std::fs::read_to_string(&path).unwrap();
            let c = checked(&source);
            assert!(
                c.diagnostics.is_empty(),
                "{}: {:?}",
                path.display(),
                c.diagnostics
            );
            assert!(
                capability_effects::capability_policies(&c)
                    .unwrap()
                    .const_time
                    .is_empty()
            );
            n += 1;
        }
    }
    assert!(n >= 5);
}

#[test]
fn policies_carry_roots_secret_parameters_and_allowlists() {
    let dir =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/capability_effects/accept");
    let ct = checked(&std::fs::read_to_string(dir.join("consttime.ves")).unwrap());
    let p = capability_effects::capability_policies(&ct).unwrap();
    assert_eq!(p.schema, "vesper.capability-policies/1");
    let names: Vec<_> = p.const_time.iter().map(|f| f.function.as_str()).collect();
    assert_eq!(
        names,
        ["ct_diff", "ct_is_zero", "declassify_bool", "verify_tag"]
    );
    let verify = &p.const_time[3];
    assert_eq!(verify.secret_params, [0, 1]);
    assert!(!verify.secret_result && !verify.declassify_gate);
    assert_eq!(verify.constant_time_callees, ["ct_diff", "ct_is_zero"]);
    assert_eq!(verify.declassify_callees, ["declassify_bool"]);
    assert!(p.const_time[2].declassify_gate);
    assert!(p.const_time[0].secret_result);

    let rt = checked(&std::fs::read_to_string(dir.join("realtime.ves")).unwrap());
    let p = capability_effects::capability_policies(&rt).unwrap();
    let roots: Vec<_> = p
        .realtime
        .functions
        .iter()
        .filter(|f| f.root)
        .map(|f| f.function.as_str())
        .collect();
    assert_eq!(roots, ["step"]);
    assert_eq!(
        p.realtime.calls,
        [
            ("step".to_string(), "torque".to_string()),
            ("torque".into(), "clamp".into())
        ]
    );
    assert!(p.realtime.reject_unbounded_loops);
    assert!(p.realtime.forbidden_effects.iter().any(|e| e == "Alloc"));
    assert!(
        p.realtime
            .functions
            .iter()
            .all(|f| f.stack_budget_bytes.is_none())
    );

    let df = checked(&std::fs::read_to_string(dir.join("detfloat.ves")).unwrap());
    let p = capability_effects::capability_policies(&df).unwrap();
    let integrate = p
        .det_float
        .iter()
        .find(|f| f.function == "integrate")
        .unwrap();
    assert_eq!(integrate.allowed_float_calls, ["half"]);
    assert!(!integrate.allow_fma);

    // Plain data, serializable for the backend hook.
    let json = serde_json::to_string(&p).unwrap();
    assert!(json.contains("\"det_float\""));
}

#[test]
fn policies_refuse_unchecked_programs() {
    let c = checked("fn plain() -> u64 { 1 } fn rt() -> u64 eff [Realtime] { plain() }");
    assert_eq!(
        capability_effects::capability_policies(&c),
        Err(PolicyError::Unchecked {
            diagnostics: c.diagnostics.len()
        })
    );
}

#[test]
fn capability_names_are_stable() {
    for c in Capability::ALL {
        assert_eq!(Capability::from_name(c.name()), Some(c));
        assert!(capability_effects::is_capability_effect(c.name()));
    }
    assert!(!capability_effects::is_capability_effect("Fail"));
}

#[test]
fn fixtures_are_canonically_formatted() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/capability_effects");
    let out = vesper(&["fmt", "--check", dir.to_str().unwrap()]);
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
