//! Lane 24 (#266): Stage-5 backend verifiers run from source capability
//! effects. `eff [ConstTime]`, `eff [Realtime]` and `eff [DetFloat]` imply the
//! Vault constant-time, Resonance realtime and Kinetic DetFloat verifiers and
//! their policies, which `vesper build` and `vesper emit --emit object|clif`
//! run on the emitted Cranelift functions without any flag. Every case runs
//! for x86_64 and AArch64 objects.
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};
use vesper::{
    check, native,
    native_verifiers::{
        self, NativeVerifier, NativeVerifierPolicy, POLICY_SCHEMA, capability_verifiers,
        emit_and_collect, policy_from_capabilities, verify_native_with_capabilities,
    },
    parser,
};

const TARGETS: [&str; 2] = ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"];

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn fixtures(kind: &str) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = fs::read_dir(
        root()
            .join("tests/fixtures/capability_verifiers")
            .join(kind),
    )
    .unwrap()
    .map(|e| e.unwrap().path())
    .filter(|p| p.extension().is_some_and(|e| e == "ves"))
    .collect();
    out.sort();
    assert!(!out.is_empty(), "no {kind} fixtures");
    out
}

/// Lane 15's accepted capability programs, which must also pass the backend.
fn lane15_accepted() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> =
        fs::read_dir(root().join("tests/fixtures/capability_effects/accept"))
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|e| e == "ves"))
            .collect();
    out.sort();
    out
}

fn checked(path: &Path) -> check::Checked {
    let source = fs::read_to_string(path).unwrap();
    let parsed = parser::parse(&source);
    assert!(
        parsed.diagnostics.is_empty(),
        "{}: {:?}",
        path.display(),
        parsed.diagnostics
    );
    let checked = check::check(&parsed.module);
    assert!(
        checked.diagnostics.is_empty(),
        "{} must be source-accepted: {:?}",
        path.display(),
        checked.diagnostics
    );
    checked
}

fn vesper(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vesper"))
        .args(args)
        .output()
        .unwrap()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "vesper-capability-verifiers-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
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

/// The `// expect: <verifier>/<rule>` line of a rejected fixture.
fn expected_rule(path: &Path) -> String {
    let source = fs::read_to_string(path).unwrap();
    source
        .lines()
        .find_map(|l| l.strip_prefix("// expect: "))
        .unwrap_or_else(|| panic!("{} has no expect line", path.display()))
        .trim()
        .to_string()
}

/// 1-based line of the first `fn <name>` in `source`.
fn fn_line(source: &str, name: &str) -> usize {
    source
        .lines()
        .position(|l| {
            l.starts_with(&format!("fn {name}(")) || l.starts_with(&format!("fn {name}["))
        })
        .unwrap()
        + 1
}

#[test]
fn source_accepted_capability_programs_pass_the_backend_on_both_isas() {
    let temp = Temp::new();
    let mut files = fixtures("accept");
    files.extend(lane15_accepted());
    for file in &files {
        let world = checked(file).world;
        let implied = capability_verifiers(&world);
        assert!(
            !implied.is_empty(),
            "{} has no capability effect",
            file.display()
        );
        for target in TARGETS {
            let out = temp.path("out.o");
            let o = vesper(&[
                "emit",
                file.to_str().unwrap(),
                "--emit",
                "object",
                "--target",
                target,
                "-o",
                out.to_str().unwrap(),
            ]);
            let err = stderr(&o);
            assert!(o.status.success(), "{} on {target}:\n{err}", file.display());
            let names: Vec<_> = implied.iter().map(|v| v.name()).collect();
            assert!(
                err.contains(&format!("emitted function(s): {}", names.join(", "))),
                "{} on {target} did not run {names:?}:\n{err}",
                file.display()
            );
            assert!(!err.contains("error["), "{err}");
            assert!(fs::metadata(&out).unwrap().len() > 0);
        }
    }
}

#[test]
fn source_accepted_programs_rejected_by_the_backend_report_source_locations() {
    let temp = Temp::new();
    for file in fixtures("reject") {
        let source = fs::read_to_string(&file).unwrap();
        checked(&file);
        let rule = expected_rule(&file);
        for target in TARGETS {
            let out = temp.path("out.o");
            let o = vesper(&[
                "emit",
                file.to_str().unwrap(),
                "--emit",
                "object",
                "--target",
                target,
                "-o",
                out.to_str().unwrap(),
            ]);
            let err = stderr(&o);
            assert!(
                !o.status.success(),
                "{} accepted on {target}:\n{err}",
                file.display()
            );
            assert_eq!(o.status.code(), Some(1), "{err}");
            assert!(err.contains("native verifier finding(s)"), "{err}");
            let line = err
                .lines()
                .find(|l| l.contains(&format!("error[{rule}]")))
                .unwrap_or_else(|| panic!("{} on {target}: no {rule}:\n{err}", file.display()));
            // `file:line:col: error[...]: <function>...`: the location is the
            // source function's item, after its leading comments.
            let function = line
                .split("]: ")
                .nth(1)
                .and_then(|r| r.split([':', ' ']).next())
                .unwrap();
            let location = format!("{}:{}:1: ", file.display(), fn_line(&source, function));
            assert!(
                line.starts_with(&location),
                "{line} does not start with {location}"
            );
        }
    }
}

#[test]
fn realtime_rejection_is_lifted_by_an_attested_symbol_table() {
    let temp = Temp::new();
    let file = root().join("tests/fixtures/capability_verifiers/reject/realtime_fmod.ves");
    let policy = temp.path("policy.json");
    fs::write(
        &policy,
        format!(
            r#"{{"schema":"{POLICY_SCHEMA}","realtime":{{"symbols":{{"fmod":{{"effects":["Realtime"],"frame_bytes":64}}}}}}}}"#
        ),
    )
    .unwrap();
    for target in TARGETS {
        let out = temp.path("out.o");
        let o = vesper(&[
            "emit",
            file.to_str().unwrap(),
            "--emit",
            "object",
            "--target",
            target,
            "-o",
            out.to_str().unwrap(),
            "--verify-realtime",
            "--verify-policy",
            policy.to_str().unwrap(),
        ]);
        let err = stderr(&o);
        assert!(o.status.success(), "{target}: {err}");
        assert!(err.contains("emitted function(s): realtime"), "{err}");
    }
}

#[test]
fn programs_without_capability_effects_run_no_verifier() {
    let temp = Temp::new();
    let file = root().join("tests/fixtures/integration_b/verifiers.ves");
    let world = checked(&file).world;
    assert!(capability_verifiers(&world).is_empty());
    let out = temp.path("out.o");
    let o = vesper(&[
        "emit",
        file.to_str().unwrap(),
        "--emit",
        "object",
        "--entry",
        "ct_main",
        "-o",
        out.to_str().unwrap(),
    ]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert_eq!(stderr(&o), "");
    // The library entry point is then exactly object emission.
    let (bytes, _, report) = verify_native_with_capabilities(
        &world,
        "ct_main",
        Some(TARGETS[0]),
        true,
        &BTreeSet::new(),
        &NativeVerifierPolicy {
            schema: POLICY_SCHEMA.into(),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(report.verifiers.is_empty());
    assert_eq!(
        bytes,
        native::object_with_options(&world, "ct_main", Some(TARGETS[0]), true)
            .unwrap()
            .0
    );
}

#[test]
fn derived_policies_name_the_source_facts() {
    let file = root().join("tests/fixtures/capability_effects/accept/consttime.ves");
    let world = checked(&file).world;
    assert_eq!(
        capability_verifiers(&world),
        BTreeSet::from([NativeVerifier::ConstTime])
    );
    let (_, _, functions) = emit_and_collect(&world, "main", Some(TARGETS[1]), true).unwrap();
    // Capability-only functions are compiled once instead of being
    // specialized into main, so each has its own Cranelift function.
    let keys: BTreeSet<_> = functions.iter().map(|f| f.key.as_str()).collect();
    for f in [
        "ct_diff",
        "ct_is_zero",
        "declassify_bool",
        "verify_tag",
        "main",
    ] {
        assert!(keys.contains(f), "{f} not emitted: {keys:?}");
    }
    let derived = policy_from_capabilities(
        &world,
        &functions,
        &NativeVerifierPolicy {
            schema: POLICY_SCHEMA.into(),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(derived.findings.is_empty());
    let ct = &derived.policy.const_time;
    assert_eq!(
        ct.keys().map(String::as_str).collect::<Vec<_>>(),
        ["ct_diff", "ct_is_zero", "declassify_bool", "verify_tag"]
    );
    assert_eq!(ct["ct_diff"].secret, ["a", "b"]);
    assert_eq!(ct["verify_tag"].secret, ["tag", "expected"]);
    assert_eq!(ct["verify_tag"].declassify, ["declassify_bool"]);
    assert_eq!(
        ct["verify_tag"].constant_time_callees,
        ["ct_diff", "ct_is_zero"]
    );
    // A policy file entry wins over the derived one.
    let mut base = NativeVerifierPolicy {
        schema: POLICY_SCHEMA.into(),
        ..Default::default()
    };
    base.const_time.insert("ct_diff".into(), Default::default());
    let layered = policy_from_capabilities(&world, &functions, &base).unwrap();
    assert!(layered.policy.const_time["ct_diff"].secret.is_empty());
    assert_eq!(
        layered.policy.const_time["verify_tag"].secret,
        ["tag", "expected"]
    );

    let file = root().join("tests/fixtures/capability_effects/accept/realtime.ves");
    let world = checked(&file).world;
    let (_, _, functions) = emit_and_collect(&world, "main", Some(TARGETS[0]), true).unwrap();
    let derived = policy_from_capabilities(
        &world,
        &functions,
        &NativeVerifierPolicy {
            schema: POLICY_SCHEMA.into(),
            ..Default::default()
        },
    )
    .unwrap();
    let rt = &derived.policy.realtime;
    assert_eq!(
        rt.roots.keys().map(String::as_str).collect::<Vec<_>>(),
        ["clamp", "step", "torque"]
    );
    assert!(rt.reject_unbounded_loops);
    assert_eq!(rt.effects["step"], ["Realtime"]);

    let file = root().join("tests/fixtures/capability_effects/accept/closures.ves");
    let world = checked(&file).world;
    let (_, _, functions) = emit_and_collect(&world, "main", Some(TARGETS[0]), true).unwrap();
    let derived = policy_from_capabilities(
        &world,
        &functions,
        &NativeVerifierPolicy {
            schema: POLICY_SCHEMA.into(),
            ..Default::default()
        },
    )
    .unwrap();
    // `apply` takes a function value, so it is specialized into its callers;
    // that is reported, not silently skipped.
    assert!(
        derived
            .notes
            .iter()
            .any(|n| n.contains("'apply' is specialized")),
        "{:?}",
        derived.notes
    );
    let det = derived.policy.det_float.functions.unwrap();
    assert!(det.contains(&"triple".to_string()) && det.contains(&"twice_then_triple".to_string()));
    assert!(!derived.policy.det_float.allow_fma);
}

#[test]
fn detfloat_programs_are_compiled_with_nan_canonicalization() {
    let det =
        checked(&root().join("tests/fixtures/capability_verifiers/accept/detfloat_canonical.ves"))
            .world;
    let plain = checked(&root().join("tests/fixtures/integration_b/verifiers.ves")).world;
    assert!(native::needs_nan_canonicalization(&det));
    assert!(!native::needs_nan_canonicalization(&plain));
    for target in TARGETS {
        assert!(
            native::object_isa_for(&det, Some(target))
                .unwrap()
                .flags()
                .enable_nan_canonicalization()
        );
        assert!(
            !native::object_isa_for(&plain, Some(target))
                .unwrap()
                .flags()
                .enable_nan_canonicalization()
        );
    }
    // Kinetic would reject the escaping float arithmetic without it: audit
    // the same emitted IR under a policy that says NaNs are not canonicalised.
    let (_, _, functions) = emit_and_collect(&det, "main", Some(TARGETS[0]), true).unwrap();
    let mix = functions.iter().find(|f| f.key == "mix").unwrap();
    let findings =
        vesper_kinetic::audit(&mix.function, &vesper_kinetic::DetFloatPolicy::default()).unwrap();
    assert!(
        findings.iter().any(|f| f.rule == "detfloat.nan-escape"),
        "{findings:?}"
    );
    let report = native_verifiers::verify_emitted(
        &det,
        &functions,
        Some(TARGETS[0]),
        &BTreeSet::from([NativeVerifier::DetFloat]),
        &policy_from_capabilities(
            &det,
            &functions,
            &NativeVerifierPolicy {
                schema: POLICY_SCHEMA.into(),
                ..Default::default()
            },
        )
        .unwrap()
        .policy,
    )
    .unwrap();
    assert!(report.accepted(), "{:?}", report.findings);
}

/// Capability functions now compile once; their results under the native
/// backend still match the evaluator.
#[test]
fn native_results_match_the_evaluator() {
    let mut files = fixtures("accept");
    files.extend(lane15_accepted());
    files.extend(fixtures("reject"));
    for file in files {
        let f = file.to_str().unwrap();
        let run = vesper(&["run", f]);
        let jit = vesper(&["jit", f]);
        assert!(run.status.success(), "{f}: {}", stderr(&run));
        assert!(jit.status.success(), "{f}: {}", stderr(&jit));
        assert_eq!(run.stdout, jit.stdout, "{f}");
    }
}

#[cfg(target_os = "linux")]
#[test]
fn vesper_build_runs_the_capability_verifiers() {
    let temp = Temp::new();
    let ok = root().join("tests/fixtures/capability_verifiers/accept/consttime_select.ves");
    let exe = temp.path("ct");
    let o = vesper(&["build", ok.to_str().unwrap(), "-o", exe.to_str().unwrap()]);
    let err = stderr(&o);
    if err.contains("linker") && !o.status.success() {
        eprintln!("skipping: no linker: {err}");
        return;
    }
    assert!(o.status.success(), "{err}");
    assert!(err.contains("emitted function(s): const-time"), "{err}");
    let status = Command::new(&exe).status().unwrap();
    assert_eq!(status.code(), Some(0));

    let bad = root().join("tests/fixtures/capability_verifiers/reject/realtime_fmod.ves");
    let exe = temp.path("rt");
    let o = vesper(&["build", bad.to_str().unwrap(), "-o", exe.to_str().unwrap()]);
    assert!(!o.status.success());
    assert!(
        stderr(&o).contains("error[realtime/rt-unknown-callee]"),
        "{}",
        stderr(&o)
    );
    assert!(!exe.exists(), "a rejected program must not be linked");
}
