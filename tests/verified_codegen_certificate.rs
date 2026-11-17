//! Range certificates for verified codegen (`verified_codegen::certificate`).
//!
//! * Every fixture's lowered program gets a certificate the production
//!   validator accepts (compilation already refuses otherwise).
//! * Forged certificates and malformed programs are rejected.
//! * The Rust mirror of `validateProgram` agrees with Lean `#eval` of
//!   `lean/CodegenFlowValidation.lean` on accepted and mutated certificates
//!   (with `VESPER_LEAN` set, as in CI).
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use vesper::verified_codegen::certificate::{
    self, Annotation, Certificate, constant_value, labels_of, validate, validate_lean, widen,
};
use vesper::verified_codegen::{
    self as vc, ArithOp, Cond, FloatOp, Kind, Label, Lir, Loc, Options, Target, TrapKind, WrapOp,
};
use vesper::{check, parser};

type Program = Vec<(String, Vec<Lir>)>;

fn lower(source: &str) -> Program {
    lower_with(source, true)
}

/// Lower `source` to the widened rows the certificate is checked on; with
/// `require`, compilation refuses a rejected certificate.
fn lower_with(source: &str, require: bool) -> Program {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    let c = check::check(&p.module);
    assert!(c.diagnostics.is_empty(), "{:?}", c.diagnostics);
    let target = Target::from_triple("x86_64-unknown-linux-gnu").unwrap();
    let options = Options {
        require_certificate: require,
        ..Options::new(target)
    };
    let artifact = vc::compile(&c.world, "main", &options).expect("compiles");
    artifact
        .lir
        .into_iter()
        .map(|(n, rows)| (n, rows.iter().map(widen).collect()))
        .collect()
}

/// Fixtures whose certificates the facts cannot establish yet: the status
/// protocol of `Fail` (milestone 7) returns a payload word that is only
/// meaningful on one side of a status test, which needs status-guarded
/// facts (the same follow-up as enum payloads).
const IMPRECISE: [&str; 2] = ["fail_implicit.ves", "fail_try.ves"];

fn fixtures() -> Vec<(String, Program)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/verified_codegen");
    let mut names: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "ves"))
        .collect();
    names.sort();
    let mut out: Vec<(String, Program)> = names
        .iter()
        .map(|p| (p.file_name().unwrap().to_string_lossy().into_owned(), p))
        .filter(|(name, _)| !IMPRECISE.contains(&name.as_str()))
        .map(|(name, p)| (name, lower(&std::fs::read_to_string(p).unwrap())))
        .collect();
    // A constant in [2^32, 2^63) at `i64` and at `u64`, and words above 2^63.
    out.push((
        "wide".into(),
        lower(
            "fn f(a: i64) -> i64 eff [Fail[ArithOverflow]] { a + 4294967296 }\n\
             fn g(a: u64) -> u64 eff [Fail[ArithOverflow]] { a + 4294967296 }\n\
             fn main() -> (i64, u64, i64, u64) eff [Fail[ArithOverflow]] {\n\
               (f(-9223372036854775807), g(18446744069414584319), -1, 18446744073709551615)\n\
             }",
        ),
    ));
    out
}

#[test]
fn every_fixture_has_an_accepted_certificate() {
    for (name, program) in fixtures() {
        let cert = certificate::generate(&program);
        validate(&program, &cert).unwrap_or_else(|r| panic!("{name}: {r}"));
        let facts: usize = cert
            .functions
            .iter()
            .flat_map(|f| &f.annotations)
            .map(Vec::len)
            .sum();
        assert!(facts > 0, "{name}: an empty certificate");
    }
}

#[test]
fn imprecise_fixtures_still_compile_and_record_the_rejection() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/verified_codegen");
    for name in IMPRECISE {
        let program = lower_with(&std::fs::read_to_string(dir.join(name)).unwrap(), false);
        let cert = certificate::generate(&program);
        assert!(
            validate(&program, &cert).is_err(),
            "{name} is certified now: move it out of IMPRECISE"
        );
    }
}

#[test]
fn negative_and_large_constants_get_their_reading() {
    let (_, program) = fixtures().pop().unwrap();
    let cert = certificate::generate(&program);
    let main = &cert.functions[0];
    let signed: Vec<u64> = main
        .signed_constants
        .iter()
        .map(|pc| match program[0].1[*pc] {
            Lir::Const { word, .. } => word,
            _ => unreachable!(),
        })
        .collect();
    // The i64 argument is read as a negative integer because `f` adds to it
    // at `i64`; result words nobody computes with keep the unsigned reading.
    assert!(
        signed.contains(&(-9223372036854775807i64 as u64)),
        "{signed:?}"
    );
    assert!(!signed.contains(&u64::MAX), "{signed:?}");
}

fn first_rejection(program: &Program, cert: &Certificate) -> String {
    validate(program, cert).expect_err("rejected").reason
}

/// The first row of `f` whose next annotation names a slot, and that slot.
fn some_fact(cert: &Certificate) -> (usize, usize, usize) {
    for (f, fc) in cert.functions.iter().enumerate() {
        for (pc, a) in fc.annotations.iter().enumerate() {
            if let Some(i) = a.iter().position(|(l, _)| matches!(l, Loc::Slot(_))) {
                return (f, pc, i);
            }
        }
    }
    panic!("no slot fact")
}

#[test]
fn forged_certificates_are_rejected() {
    let program = lower(
        "fn add(a: u8, b: u8) -> u8 eff [Fail[ArithOverflow]] { a + b }\n\
         fn main() -> u8 eff [Fail[ArithOverflow]] { add(200, 3) }",
    );
    let good = certificate::generate(&program);
    validate(&program, &good).unwrap();

    // Narrowing a fact the analysis proved (u8 to bool) cannot be established.
    let mut forged = good.clone();
    let (f, pc, i) = some_fact(&forged);
    forged.functions[f].annotations[pc][i].1 = Kind::Bool;
    assert!(validate(&program, &forged).is_err());

    // Dropping the callee's entry summary leaves its additions unestablished.
    let mut forged = good.clone();
    forged.functions[1].annotations[0].clear();
    assert!(first_rejection(&program, &forged).contains("is not established"));

    // A forged exit summary claims a result range nothing establishes.
    let mut forged = good.clone();
    forged.functions[1].post = vec![(Loc::In(0), Kind::Bool)];
    assert!(validate(&program, &forged).is_err());

    // The entry may not assume anything; summaries name incoming words only.
    let mut forged = good.clone();
    forged.functions[0].annotations[0] = vec![(Loc::In(0), Kind::U8)];
    assert!(first_rejection(&program, &forged).contains("precondition"));
    let mut forged = good.clone();
    forged.functions[1].annotations[0].push((Loc::Slot(9), Kind::U8));
    assert!(first_rejection(&program, &forged).contains("non-incoming"));

    // f64 has no Lean kind; annotations are one per row.
    let mut forged = good.clone();
    forged.functions[1].annotations[3].push((Loc::Slot(1), Kind::F64));
    assert!(first_rejection(&program, &forged).contains("f64"));
    let mut forged = good.clone();
    forged.functions[1].annotations.pop();
    assert!(first_rejection(&program, &forged).contains("one annotation per row"));

    // A signed reading must be on a constant row.
    let mut forged = good.clone();
    forged.functions[1].signed_constants.insert(1);
    assert!(first_rejection(&program, &forged).contains("non-constant"));
}

#[test]
fn malformed_programs_are_rejected() {
    let program = lower(
        "fn add(a: u8, b: u8) -> u8 eff [Fail[ArithOverflow]] { a + b }\n\
         fn main() -> u8 eff [Fail[ArithOverflow]] { add(200, 3) }",
    );
    let cert = certificate::generate(&program);
    let reject = |edit: &dyn Fn(&mut Program)| {
        let mut p = program.clone();
        edit(&mut p);
        let mut c = cert.clone();
        for (f, (_, rows)) in p.iter().enumerate() {
            c.functions[f].annotations.resize(rows.len(), vec![]);
        }
        validate(&p, &c)
            .expect_err("malformed program accepted")
            .reason
    };
    let add_label = match program[1].1[0] {
        Lir::Label(l) => l,
        _ => unreachable!(),
    };
    let (call_at, _) = program[0]
        .1
        .iter()
        .enumerate()
        .find(|(_, r)| matches!(r, Lir::Call { .. }))
        .unwrap();
    assert!(reject(&|p| p[0].1[call_at] = Lir::Call { callee: 9999 }).contains("non-entry label"));
    assert!(reject(&|p| p[1].1[1] = Lir::Label(9998)).contains("row 1 is not Enter"));
    assert!(reject(&|p| p[1].1.push(Lir::Enter { words: 2 })).contains("Enter outside"));
    assert!(reject(&|p| p[1].1.push(Lir::Jump(add_label))).contains("outside the body"));
    assert!(reject(&|p| p[1].1.push(Lir::Jump(9997))).contains("outside the body"));
    assert!(reject(&|p| p[1].1.push(Lir::Label(add_label))).contains("defined twice"));
    assert!(reject(&|p| p[1].1.insert(0, Lir::Return)).contains("entry label"));
}

#[test]
fn compilation_refuses_a_program_whose_certificate_fails() {
    // The hook runs on the rows the code is selected from: forge rows whose
    // operand is out of range for the row kind and check it is refused.
    let program: Program = vec![(
        "main".into(),
        vec![
            Lir::Label(3),
            Lir::Enter { words: 2 },
            Lir::Const { dst: 0, word: 300 },
            Lir::Not { dst: 1, a: 0 },
            Lir::Return,
        ],
    )];
    let err = certificate::check(&program).expect_err("refused");
    assert!(err.to_string().contains("Not"), "{err}");
}

fn world(source: &str) -> vesper::check::World {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    let c = check::check(&p.module);
    assert!(c.diagnostics.is_empty(), "{:?}", c.diagnostics);
    c.world
}

/// Programs the facts are not yet precise enough for still compile by
/// default (the rejection is recorded) and are refused when the gate is on.
#[test]
fn imprecise_programs_compile_unless_the_gate_is_required() {
    let target = Target::from_triple("x86_64-unknown-linux-gnu").unwrap();
    for (source, construct) in [
        (
            // The payload word is a `u8` for one call and a `bool` for the
            // other; the join forgets that `Flag` holds a `bool`.
            "copy enum E { Byte(u8), Flag(bool) }
             fn f(e: E) -> bool { match e { E::Flag(b) => !b, E::Byte(_) => false } }
             fn main() -> (bool, bool) { (f(E::Byte(7)), f(E::Flag(true))) }",
            "Not",
        ),
        (
            // `spin` never returns, so its continuation has no facts.
            "fn spin() -> i64 { spin() }
             fn main() -> i64 eff [Fail[ArithOverflow]] { spin() + 1 }",
            "Arith",
        ),
    ] {
        let w = world(source);
        let artifact = vc::compile(&w, "main", &Options::new(target)).expect("compiles");
        let rejection = artifact.certificate.expect_err("imprecise");
        assert!(rejection.reason.contains(construct), "{rejection}");
        let strict = Options {
            require_certificate: true,
            ..Options::new(target)
        };
        let err = vc::compile(&w, "main", &strict).expect_err("refused");
        assert!(
            err.to_string().contains("range certificate rejected"),
            "{err}"
        );
    }
}

/// Under a trap scope a failing row jumps to the handler with its incoming
/// store, so the handler's annotation must follow from the row's facts.
#[test]
fn trap_scope_handlers_are_checked() {
    let rows = |handler: Label| {
        vec![
            Lir::Label(3),
            Lir::Enter { words: 4 },
            Lir::Scope {
                ovf: handler,
                div0: TrapKind::DivisionByZero.label(),
            },
            Lir::Const { dst: 0, word: 1 },
            Lir::Const { dst: 1, word: 255 },
            Lir::Arith {
                op: ArithOp::Add,
                kind: Kind::U8,
                dst: 2,
                a: 0,
                b: 1,
            },
            Lir::Return,
            Lir::Label(9),
            Lir::Not { dst: 3, a: 0 },
            Lir::Return,
        ]
    };
    let program: Program = vec![("main".into(), rows(9))];
    let cert = certificate::check(&program).expect("the handler reads a fact the row has");
    assert!(
        cert.functions[0].annotations[7].contains(&(Loc::Slot(0), Kind::Bool)),
        "{:?}",
        cert.functions[0].annotations[7]
    );
    // The result of the failing row is not available in the handler.
    let mut forged = cert.clone();
    forged.functions[0].annotations[7].push((Loc::Slot(2), Kind::U8));
    assert!(first_rejection(&program, &forged).contains("handler at label 9"));
    // A handler must be a label of the same function.
    let program: Program = vec![("main".into(), rows(77))];
    let cert = certificate::generate(&program);
    assert!(first_rejection(&program, &cert).contains("outside the body"));
}

#[test]
fn a_malformed_callee_certificate_is_a_rejection_not_a_panic() {
    let program = lower(
        "fn add(a: u8, b: u8) -> u8 eff [Fail[ArithOverflow]] { a + b }\n\
         fn main() -> u8 eff [Fail[ArithOverflow]] { add(200, 3) }",
    );
    let mut cert = certificate::generate(&program);
    cert.functions[1].annotations.clear();
    assert!(first_rejection(&program, &cert).contains("one annotation per row"));
}

/// Facts of temporaries nobody reads again are pruned: the certificate of a
/// long straight-line function stays linear in its length.
#[test]
fn dead_facts_are_pruned() {
    let mut rows = vec![Lir::Label(3), Lir::Enter { words: 4000 }];
    for dst in 0..3998 {
        rows.push(Lir::Const { dst, word: 1 });
    }
    rows.push(Lir::Return);
    let program: Program = vec![("main".into(), rows)];
    let cert = certificate::check(&program).expect("accepted");
    let facts: usize = cert.functions[0].annotations.iter().map(Vec::len).sum();
    assert_eq!(facts, 0);
}

// ---------------------------------------------------------------------------
// Differential test of the Rust mirror against Lean `#eval`.

struct Case {
    rows: Vec<Lir>,
    labels: HashMap<Label, usize>,
    annotations: Vec<Annotation>,
    signed: BTreeSet<usize>,
}

/// Rows the Lean flow validator models: `Enter` becomes a fresh label and a
/// `Call` an abort (the comparison is between two checkers, not a claim
/// about these programs).
fn lean_fragment(rows: &[Lir]) -> Vec<Lir> {
    rows.iter()
        .map(|r| match r {
            Lir::Enter { .. } => Lir::Label(1_000_000),
            Lir::Call { .. } => Lir::Trap(TrapKind::Unreachable),
            r => r.clone(),
        })
        .collect()
}

/// A deterministic generator (no new dependency).
struct Lcg(u64);
impl Lcg {
    fn next(&mut self, n: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((self.0 >> 33) % n as u64) as usize
    }
}

const KINDS: [Kind; 5] = [Kind::Bool, Kind::U8, Kind::U32, Kind::U64, Kind::I64];

fn mutate(case: &Case, rng: &mut Lcg) -> Case {
    let mut annotations = case.annotations.clone();
    let mut signed = case.signed.clone();
    for _ in 0..1 + rng.next(3) {
        let pc = rng.next(annotations.len().max(1));
        let Some(a) = annotations.get_mut(pc) else {
            continue;
        };
        match rng.next(4) {
            0 if !a.is_empty() => {
                let i = rng.next(a.len());
                a.remove(i);
            }
            1 if !a.is_empty() => {
                let i = rng.next(a.len());
                a[i].1 = KINDS[rng.next(5)];
            }
            2 => {
                let n = rng.next(48) as u32;
                let l = [Loc::Slot(n), Loc::Out(n % 8), Loc::In(n % 8)][rng.next(3)];
                a.insert(rng.next(a.len() + 1), (l, KINDS[rng.next(5)]));
            }
            _ => {
                let consts: Vec<usize> = case
                    .rows
                    .iter()
                    .enumerate()
                    .filter(|(_, r)| matches!(r, Lir::Const { .. }))
                    .map(|(pc, _)| pc)
                    .collect();
                if !consts.is_empty() {
                    let pc = consts[rng.next(consts.len())];
                    if !signed.remove(&pc) {
                        signed.insert(pc);
                    }
                }
            }
        }
    }
    Case {
        rows: case.rows.clone(),
        labels: case.labels.clone(),
        annotations,
        signed,
    }
}

fn kind(k: Kind) -> &'static str {
    match k {
        Kind::U8 => ".u8",
        Kind::U32 => ".u32",
        Kind::U64 => ".u64",
        Kind::I64 => ".i64",
        Kind::Bool => ".bool",
        Kind::F64 => unreachable!("no Lean kind"),
    }
}

fn arith(op: ArithOp) -> &'static str {
    match op {
        ArithOp::Add => ".add",
        ArithOp::Sub => ".sub",
        ArithOp::Mul => ".mul",
        ArithOp::Div => ".div",
        ArithOp::Rem => ".rem",
    }
}

fn wrap(op: WrapOp) -> &'static str {
    match op {
        WrapOp::Add => ".add",
        WrapOp::Sub => ".sub",
        WrapOp::Mul => ".mul",
    }
}

fn float(op: FloatOp) -> &'static str {
    match op {
        FloatOp::Add => ".add",
        FloatOp::Sub => ".sub",
        FloatOp::Mul => ".mul",
        FloatOp::Div => ".div",
    }
}

fn cond(c: Cond) -> &'static str {
    match c {
        Cond::Eq => ".eq",
        Cond::Ne => ".ne",
        Cond::Lt => ".lt",
        Cond::Le => ".le",
        Cond::Gt => ".gt",
        Cond::Ge => ".ge",
    }
}

fn loc(l: Loc) -> String {
    match l {
        Loc::Slot(n) => format!(".slot {n}"),
        Loc::Out(n) => format!(".out {n}"),
        Loc::In(n) => format!(".inn {n}"),
    }
}

/// The Lean term of a row; `None` when it has no Lean counterpart.
fn lean_row(pc: usize, row: &Lir, signed: &BTreeSet<usize>) -> Option<String> {
    Some(match *row {
        Lir::Const { dst, word } => {
            format!(
                "Lir.const {dst} ({})",
                constant_value(word, signed.contains(&pc))
            )
        }
        Lir::Move { dst, src } => format!("Lir.move {dst} {src}"),
        Lir::Arith {
            op,
            kind: k,
            dst,
            a,
            b,
        } if k != Kind::F64 => format!("Lir.arith {} {} {dst} {a} {b}", arith(op), kind(k)),
        Lir::Wrapping {
            op,
            kind: k,
            dst,
            a,
            b,
        } if k != Kind::F64 => format!("Lir.wrapping {} {} {dst} {a} {b}", wrap(op), kind(k)),
        Lir::Neg { kind: k, dst, a } if k != Kind::F64 => format!("Lir.neg {} {dst} {a}", kind(k)),
        Lir::Not { dst, a } => format!("Lir.not {dst} {a}"),
        Lir::Compare {
            cond: c,
            kind: k,
            dst,
            a,
            b,
        } if k != Kind::F64 => format!("Lir.compare {} {} {dst} {a} {b}", cond(c), kind(k)),
        Lir::FArith { op, dst, a, b } => format!("Lir.farith {} {dst} {a} {b}", float(op)),
        Lir::FNeg { dst, a } => format!("Lir.fneg {dst} {a}"),
        Lir::FCompare { cond: c, dst, a, b } => format!("Lir.fcompare {} {dst} {a} {b}", cond(c)),
        Lir::StoreOut { index, src } => format!("Lir.storeOut {index} {src}"),
        Lir::LoadOut { dst, index } => format!("Lir.loadOut {dst} {index}"),
        Lir::LoadIn { dst, index } => format!("Lir.loadIn {dst} {index}"),
        Lir::StoreIn { index, src } => format!("Lir.storeIn {index} {src}"),
        Lir::BranchZero { cond, target } => format!("Lir.branchZero {cond} {target}"),
        Lir::Jump(l) => format!("Lir.jump {l}"),
        Lir::Label(l) => format!("Lir.label {l}"),
        Lir::Call { callee } => format!("Lir.call {callee}"),
        Lir::Enter { words } => format!("Lir.enter {words}"),
        Lir::Return => "Lir.ret".into(),
        Lir::Trap(TrapKind::Overflow) => "Lir.trap .overflow".into(),
        Lir::Trap(TrapKind::DivisionByZero) => "Lir.trap .divZero".into(),
        Lir::Trap(TrapKind::Unreachable) => "Lir.trap .unreachable".into(),
        Lir::Scope { ovf, div0 } => format!("Lir.scope {ovf} {div0}"),
        _ => return None,
    })
}

fn lean_case(i: usize, case: &Case) -> Option<String> {
    let rows: Option<Vec<String>> = case
        .rows
        .iter()
        .enumerate()
        .map(|(pc, r)| lean_row(pc, r, &case.signed))
        .collect();
    let mut labels: Vec<_> = case.labels.iter().collect();
    labels.sort();
    let arms: String = labels
        .iter()
        .map(|(l, pc)| format!("  | {l} => some {pc}\n"))
        .collect();
    let annotations: Vec<String> = case
        .annotations
        .iter()
        .map(|a| {
            let facts: Vec<String> = a
                .iter()
                .map(|(l, k)| format!("({}, {})", loc(*l), kind(*k)))
                .collect();
            format!("[{}]", facts.join(", "))
        })
        .collect();
    Some(format!(
        "def rows{i} : List Lir := [{}]\n\
         def labels{i} : Labels := fun label => match label with\n{arms}  | _ => none\n\
         def cert{i} : Certificate := [{}]\n",
        rows?.join(", "),
        annotations.join(", ")
    ))
}

fn cases() -> Vec<Case> {
    let mut rng = Lcg(0x5eed_c0de);
    let mut out = vec![];
    for (_, program) in fixtures() {
        let cert = certificate::generate(&program);
        for ((_, rows), fc) in program.iter().zip(&cert.functions) {
            let rows = lean_fragment(rows);
            let base = Case {
                labels: labels_of(&rows),
                rows,
                annotations: fc.annotations.clone(),
                signed: fc.signed_constants.clone(),
            };
            for _ in 0..4 {
                out.push(mutate(&base, &mut rng));
            }
            out.push(base);
        }
    }
    out
}

#[test]
fn rust_mirror_accepts_and_rejects_a_mix() {
    let verdicts: Vec<bool> = cases()
        .iter()
        .map(|c| validate_lean(&c.rows, &c.labels, &c.annotations, &c.signed))
        .collect();
    let accepted = verdicts.iter().filter(|v| **v).count();
    assert!(accepted >= 10, "{accepted} accepted");
    assert!(verdicts.len() - accepted >= 10, "too few rejections");
}

#[test]
fn rust_mirror_agrees_with_lean_eval() {
    let Ok(lean) = std::env::var("VESPER_LEAN") else {
        eprintln!(
            "VESPER_LEAN unset; Lean differential check of the certificate validator skipped"
        );
        return;
    };
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "vesper-codegen-certificate-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir(&dir).unwrap();
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(dir.clone());
    for module in [
        "VerifiedCodegen",
        "CodegenComposition",
        "CodegenControlFlow",
        "CodegenValidation",
        "CodegenFlowValidation",
    ] {
        let file = dir.join(format!("{module}.lean"));
        std::fs::copy(root.join(format!("lean/{module}.lean")), &file).unwrap();
        let out = std::process::Command::new(&lean)
            .current_dir(&dir)
            .env("LEAN_PATH", &dir)
            .arg("-o")
            .arg(dir.join(format!("{module}.olean")))
            .arg(&file)
            .output()
            .expect("spawn Lean");
        assert!(
            out.status.success(),
            "{module}: {}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let cases = cases();
    let mut source = String::from(
        "import CodegenFlowValidation\nopen Vesper.VerifiedCodegen Vesper.CodegenValidation \
         Vesper.CodegenControlFlow Vesper.CodegenFlowValidation\n\n",
    );
    let mut expected = vec![];
    for (i, case) in cases.iter().enumerate() {
        let Some(defs) = lean_case(i, case) else {
            continue;
        };
        source.push_str(&defs);
        source.push_str(&format!(
            "#eval IO.println s!\"case {i} {{validateProgram rows{i} labels{i} cert{i}}}\"\n\n"
        ));
        let verdict = validate_lean(&case.rows, &case.labels, &case.annotations, &case.signed);
        expected.push(format!("case {i} {verdict}"));
    }
    assert!(expected.len() >= 50, "{} Lean cases", expected.len());
    let file = dir.join("CertificateDifferential.lean");
    std::fs::write(&file, &source).unwrap();
    let out = std::process::Command::new(&lean)
        .current_dir(&dir)
        .env("LEAN_PATH", &dir)
        .arg(&file)
        .output()
        .expect("spawn Lean");
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(
        out.status.success(),
        "Lean failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let got: Vec<&str> = stdout.lines().filter(|l| l.starts_with("case ")).collect();
    assert_eq!(got, expected, "the Rust mirror disagrees with Lean");
}
