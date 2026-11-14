//! Wave-seven lane 23 (#346): source locations on emitted Cranelift
//! instructions, the native fuel bound, the capability-passing ABI, generic
//! ConstTime instances, NaN canonicalisation in the batch backend and native
//! coverage for `verify --refute` and `fuzz --differential`.
use cranelift_codegen::ir::{self, InstBuilder, types};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};
use vesper::{
    native::{self, CodegenOptions},
    native_coverage::{
        Instrumenter, NativeCoverage, NativeOutcome, SiteOp, object_with_coverage, unsigned_view,
    },
    native_verifiers::{
        ConstTimeEntry, NativeVerifier, NativeVerifierPolicy, POLICY_SCHEMA, emit_and_collect,
        policy_from_capabilities, render, verify_native_with_capabilities,
    },
};
use vesper_crucible::{CmpKind, CmpOperands, Probe};

const TARGETS: [&str; 2] = ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"];

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn checked(source: &str) -> vesper::check::World {
    let parsed = vesper::parser::parse(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let checked = vesper::check::check(&parsed.module);
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
    checked.world
}

/// 1-based `(line, column)` of byte `offset` in `source`.
fn line_col(source: &str, offset: usize) -> (usize, usize) {
    let before = &source[..offset];
    (
        before.matches('\n').count() + 1,
        before.rsplit('\n').next().unwrap().chars().count() + 1,
    )
}

/// 1-based line of the first occurrence of `needle`.
fn line_of(source: &str, needle: &str) -> usize {
    line_col(source, source.find(needle).expect(needle)).0
}

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "vesper-native-spans-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    fn write(&self, name: &str, text: &str) -> String {
        let path = self.0.join(name);
        fs::write(&path, text).unwrap();
        path.to_string_lossy().into_owned()
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

fn ves_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            ves_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "ves") {
            out.push(path);
        }
    }
}

// ---------------------------------------------------------------------------
// Identity: source locations never change object bytes.

#[test]
fn source_locations_do_not_change_object_bytes() {
    let mut sources = Vec::new();
    ves_files(&root().join("examples"), &mut sources);
    ves_files(
        &root().join("tests/fixtures/capability_verifiers"),
        &mut sources,
    );
    ves_files(
        &root().join("tests/fixtures/capability_effects"),
        &mut sources,
    );
    ves_files(&root().join("tests/fixtures/native_coverage"), &mut sources);
    sources.sort();
    let mut compared = 0;
    for path in &sources {
        let source = fs::read_to_string(path).unwrap();
        let parsed = vesper::parser::parse(&source);
        if !parsed.diagnostics.is_empty() {
            continue;
        }
        let checked = vesper::check::check(&parsed.module);
        if !checked.diagnostics.is_empty() || !checked.world.functions.contains_key("main") {
            continue;
        }
        let world = &checked.world;
        let is_example = path.starts_with(root().join("examples"));
        for target in TARGETS {
            let Ok((plain, clif)) = native::object_with_options(world, "main", Some(target), true)
            else {
                continue;
            };
            let off = CodegenOptions {
                source_locations: false,
            };
            let (bare, bare_clif) = native::object_with_codegen_options(
                world,
                "main",
                Some(target),
                true,
                None,
                None,
                off,
            )
            .unwrap();
            assert_eq!(plain, bare, "{} {target}", path.display());
            // Locations show in CLIF text only.
            assert!(
                clif.contains('@'),
                "{}: no locations in\n{clif}",
                path.display()
            );
            assert!(!bare_clif.contains("\n@"), "{}", path.display());
            if is_example {
                // No example uses the capability-passing ABI, so their code
                // is unchanged by it too.
                assert!(
                    native::capability_abi_functions(world, "main")
                        .unwrap()
                        .is_empty(),
                    "{}",
                    path.display()
                );
                assert!(
                    !plain
                        .windows(native::CAPABILITY_ABI.len())
                        .any(|w| w == native::CAPABILITY_ABI.as_bytes())
                );
            }
            compared += 1;
        }
    }
    assert!(compared >= 20, "compared {compared} objects");
}

// ---------------------------------------------------------------------------
// Spans in verifier findings and coverage maps.

#[test]
fn verifier_findings_point_at_the_instruction() {
    let file = root().join("tests/fixtures/capability_verifiers/reject/realtime_fmod.ves");
    let source = fs::read_to_string(&file).unwrap();
    let world = checked(&source);
    for target in TARGETS {
        let (_, _, report) = verify_native_with_capabilities(
            &world,
            "main",
            Some(target),
            true,
            &BTreeSet::new(),
            &NativeVerifierPolicy {
                schema: POLICY_SCHEMA.into(),
                ..Default::default()
            },
        )
        .unwrap();
        let finding = report
            .findings
            .iter()
            .find(|f| f.rule == "rt-unknown-callee")
            .expect("fmod finding");
        let offset = finding.location.expect("instruction location");
        assert_eq!(&source[offset..offset + 5], "phase", "{target}");
        let (line, column) = line_col(&source, offset);
        assert_eq!(line, line_of(&source, "phase % period"));
        let rendered = render(&report, "f.ves", &source);
        assert!(
            rendered.contains(&format!(
                "f.ves:{line}:{column}: error[realtime/rt-unknown-callee]"
            )),
            "{rendered}"
        );
    }
    // The CLI prints the same location.
    let o = vesper(&["emit", file.to_str().unwrap(), "--emit", "clif"]);
    let t = text(&o);
    let line = line_of(&source, "phase % period");
    assert!(
        t.contains(&format!("realtime_fmod.ves:{line}:5: error[")),
        "{t}"
    );
}

#[test]
fn every_emitted_instruction_with_a_location_points_into_its_source() {
    let source = fs::read_to_string(root().join("examples/checked_math.ves")).unwrap();
    let world = checked(&source);
    let (_, _, functions) = emit_and_collect(&world, "main", Some(TARGETS[0]), true).unwrap();
    let mut located = 0;
    for f in &functions {
        for block in f.function.layout.blocks() {
            for inst in f.function.layout.block_insts(block) {
                if let Some(offset) = f.source_offset(inst) {
                    assert!(offset < source.len());
                    located += 1;
                }
            }
        }
    }
    assert!(located > 0);
}

const MAGIC_SOURCE: &str = "fn unlock(key: u64, len: u32) -> u32 eff [Fail[ArithOverflow]] {
    let magic: u64 = 4660 * 4660 + 77;
    if key == magic {
        len * 65536
    } else {
        len / 2
    }
}

fn below(x: i64) -> bool {
    x < 5
}

fn spin(n: u64) -> u64 eff [Fail[ArithOverflow]] {
    if n == 3 {
        spin(n) + 1
    } else {
        n
    }
}

fn main() -> i64 {
    7
}
";
const MAGIC: u64 = 21_715_677;

#[test]
fn coverage_sites_carry_source_locations() {
    let world = checked(MAGIC_SOURCE);
    let coverage = NativeCoverage::compile(MAGIC_SOURCE, &world, "unlock").unwrap();
    let key_cmp = coverage
        .sites()
        .iter()
        .filter(|s| s.function == "unlock" && s.op == SiteOp::Compare)
        .filter_map(|s| s.location)
        .map(|o| line_col(MAGIC_SOURCE, o).0)
        .collect::<BTreeSet<_>>();
    assert!(
        key_cmp.contains(&line_of(MAGIC_SOURCE, "key == magic")),
        "{key_cmp:?}"
    );
    let edges: Vec<_> = coverage
        .edge_sites()
        .iter()
        .filter(|e| e.function == "unlock")
        .collect();
    assert!(!edges.is_empty());
    assert!(edges.iter().all(|e| e.label < 8192));
    assert!(edges.iter().any(|e| e.location.is_some()));
}

// ---------------------------------------------------------------------------
// Fuel.

#[test]
fn non_terminating_inputs_stop_with_a_typed_result() {
    let world = checked(MAGIC_SOURCE);
    let mut spin = NativeCoverage::compile(MAGIC_SOURCE, &world, "spin").unwrap();
    assert!(spin.stats().fuel_checks > 0);
    // The default bounds: deep recursion stops on the stack budget.
    assert_eq!(
        spin.run(&[3]).unwrap(),
        NativeOutcome::Exhausted { stack: true }
    );
    // A small fuel budget stops it first.
    spin.set_fuel(1000);
    assert_eq!(
        spin.run(&[3]).unwrap(),
        NativeOutcome::Exhausted { stack: false }
    );
    assert_eq!(spin.buffer().fuel_left(), 0);
    // A terminating input on the same compiled code still returns its value.
    assert_eq!(spin.run(&[9]).unwrap(), NativeOutcome::Returned);
    assert_eq!(spin.last_result(), Some(9));
    assert!(spin.buffer().fuel_left() > 0 && spin.buffer().fuel_left() < 1000);
}

#[test]
fn fuzzing_a_non_terminating_target_reports_invalid_runs() {
    let temp = Temp::new();
    let file = temp.write("spin.ves", MAGIC_SOURCE);
    let o = vesper(&[
        "fuzz",
        &file,
        "--target",
        "spin",
        "--native-coverage",
        "--execs",
        "300",
        "--fuel",
        "5000",
    ]);
    let t = text(&o);
    assert!(t.contains("fuel 5000 block(s)"), "{t}");
    // spin(3) never terminates; the evaluator runs out of fuel too, so it is
    // an invalid run, never a hang or a crash.
    assert!(t.contains("no failure found within budget"), "{t}");
    assert!(o.status.success(), "{t}");
}

// ---------------------------------------------------------------------------
// Comparison logging: signed operands, select and br_table.

#[test]
fn signed_comparisons_report_the_outcome_the_code_computed() {
    // Unit cases: operands zero-extended from 64 and 8 bits.
    let m1 = u64::MAX; // -1
    assert_eq!(unsigned_view(CmpKind::Lt, true, 64, m1, 5), CmpKind::Gt);
    assert_eq!(unsigned_view(CmpKind::Ge, true, 64, 5, m1), CmpKind::Le);
    assert_eq!(unsigned_view(CmpKind::Lt, true, 64, 3, 5), CmpKind::Lt);
    assert_eq!(unsigned_view(CmpKind::Lt, false, 64, m1, 5), CmpKind::Lt);
    assert_eq!(unsigned_view(CmpKind::Le, true, 8, 0xff, 1), CmpKind::Ge);
    assert_eq!(unsigned_view(CmpKind::Eq, true, 64, m1, 5), CmpKind::Eq);
    for (a, b) in [
        (-1i64, 5i64),
        (5, -1),
        (-7, -3),
        (3, 9),
        (i64::MIN, i64::MAX),
    ] {
        for (kind, holds) in [
            (CmpKind::Lt, a < b),
            (CmpKind::Le, a <= b),
            (CmpKind::Gt, a > b),
            (CmpKind::Ge, a >= b),
        ] {
            let mut probe = Probe::default();
            let view = unsigned_view(kind, true, 64, a as u64, b as u64);
            assert_eq!(
                probe.cmp(0, view, a as u64, b as u64),
                holds,
                "{a} {kind:?} {b}"
            );
        }
    }
    // End to end: `x < 5` with x = -1 is taken.
    let world = checked(MAGIC_SOURCE);
    let mut below = NativeCoverage::compile(MAGIC_SOURCE, &world, "below").unwrap();
    assert_eq!(
        below.run(&[(-1i64) as u64]).unwrap(),
        NativeOutcome::Returned
    );
    assert_eq!(below.last_result(), Some(1));
    let mut probe = Probe::default();
    below.feed(&mut probe);
    let record = probe
        .comparisons()
        .iter()
        .find(|c| {
            c.site & 0x8000_0000 == 0
                && matches!(c.operands, CmpOperands::Int { lhs, rhs: 5, .. } if lhs == u64::MAX)
        })
        .expect("x < 5 record");
    assert!(record.taken);
}

/// `fn(i64) -> i64` with a `select` and a three-way `br_table`.
fn select_and_table() -> ir::Function {
    use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
    let mut sig = ir::Signature::new(cranelift_codegen::isa::CallConv::SystemV);
    sig.params.push(ir::AbiParam::new(types::I64));
    sig.returns.push(ir::AbiParam::new(types::I64));
    let mut func = ir::Function::with_name_signature(ir::UserFuncName::user(0, 0), sig);
    let mut ctx = FunctionBuilderContext::new();
    {
        let mut b = FunctionBuilder::new(&mut func, &mut ctx);
        let entry = b.create_block();
        let cases: Vec<ir::Block> = (0..3).map(|_| b.create_block()).collect();
        let default = b.create_block();
        b.append_block_params_for_function_params(entry);
        b.switch_to_block(entry);
        let x = b.block_params(entry)[0];
        let ten = b.ins().iconst(types::I64, 10);
        let twenty = b.ins().iconst(types::I64, 20);
        let picked = b.ins().select(x, ten, twenty);
        let index = b.ins().band_imm(picked, 3);
        let index32 = b.ins().ireduce(types::I32, index);
        let pool = &mut b.func.dfg.value_lists;
        let calls: Vec<ir::BlockCall> = cases
            .iter()
            .map(|&c| ir::BlockCall::new(c, std::iter::empty(), pool))
            .collect();
        let default_call = ir::BlockCall::new(default, std::iter::empty(), pool);
        let table = b.create_jump_table(ir::JumpTableData::new(default_call, &calls));
        b.ins().br_table(index32, table);
        for (k, block) in cases.into_iter().chain([default]).enumerate() {
            b.switch_to_block(block);
            let v = b.ins().iconst(types::I64, k as i64);
            b.ins().return_(&[v]);
        }
        b.seal_all_blocks();
        b.finalize();
    }
    func
}

#[test]
fn select_and_br_table_operands_are_logged() {
    let mut func = select_and_table();
    let mut instrumenter = Instrumenter::new(0x1000_0000);
    instrumenter.rewrite("table", &mut func).unwrap();
    let stats = instrumenter.stats();
    assert_eq!(stats.selects, 1, "{stats:?}");
    assert_eq!(stats.branch_tables, 1, "{stats:?}");
    assert!(stats.fuel_checks >= 5, "{stats:?}");
    let ops: Vec<SiteOp> = instrumenter.sites().iter().map(|s| s.op).collect();
    assert!(ops.contains(&SiteOp::Select));
    assert!(ops.contains(&SiteOp::BranchTable { cases: 3 }));
    // The rewritten function is valid Cranelift IR and compiles for both ISAs.
    let flags = cranelift_codegen::settings::Flags::new(cranelift_codegen::settings::builder());
    cranelift_codegen::verify_function(&func, &flags).unwrap();
    for target in TARGETS {
        let isa = native::object_isa(Some(target)).unwrap();
        let mut ctx = cranelift_codegen::Context::for_function(func.clone());
        ctx.compile(&*isa, &mut Default::default()).unwrap();
    }
}

// ---------------------------------------------------------------------------
// The capability-passing ABI.

const MIXED: &str = "fn step(x: u64) -> u64 eff [Realtime, Fail[ArithOverflow]] {
    x * 3 + 1
}

fn twice(x: u64) -> u64 eff [Realtime, Fail[ArithOverflow]] {
    step(step(x))
}

fn run(x: u64) -> u64 {
    handle Fail[ArithOverflow] in {
        twice(x)
    } with {
        return value => value,
        raise(error, resume) => 0,
    }
}

fn main() -> u64 {
    if run(6148914691236517205) == 0 {
        run(5)
    } else {
        1
    }
}
";

#[test]
fn realtime_fail_functions_are_compiled_once_and_checked_standalone() {
    let world = checked(MIXED);
    let abi = native::capability_abi_functions(&world, "main").unwrap();
    assert_eq!(abi, ["step", "twice"]);
    // Evaluator and JIT agree, including the raised overflow.
    let evaluated = vesper::runtime::Runtime::new(&world)
        .run("main", vec![])
        .unwrap();
    assert_eq!(
        native::jit(&world, "main").unwrap().to_string(),
        evaluated.to_string()
    );
    assert_eq!(evaluated.to_string(), "49");
    for target in TARGETS {
        let (bytes, _, functions) = emit_and_collect(&world, "main", Some(target), true).unwrap();
        let keys: Vec<&str> = functions.iter().map(|f| f.key.as_str()).collect();
        assert!(
            keys.contains(&"step") && keys.contains(&"twice"),
            "{keys:?}"
        );
        // The object names the ABI version and the functions using it.
        let marker = format!("{}\n", native::CAPABILITY_ABI);
        let at = bytes
            .windows(marker.len())
            .position(|w| w == marker.as_bytes())
            .expect("ABI metadata");
        let tail = String::from_utf8_lossy(&bytes[at..at + 200.min(bytes.len() - at)]);
        assert!(tail.contains("vesper_function_"), "{tail}");
        // Each realtime function is its own root and passes.
        let (_, _, report) = verify_native_with_capabilities(
            &world,
            "main",
            Some(target),
            true,
            &BTreeSet::new(),
            &NativeVerifierPolicy {
                schema: POLICY_SCHEMA.into(),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(report.verifiers.contains(&NativeVerifier::Realtime));
        assert!(report.findings.is_empty(), "{:?}", report.findings);
        assert!(
            report.notes.iter().all(|n| !n.contains("specialized")),
            "{:?}",
            report.notes
        );
    }
    // The executable links and runs on the host (Linux only: it is linked
    // with the system C compiler).
    if cfg!(target_os = "linux") {
        let temp = Temp::new();
        let file = temp.write("mixed.ves", MIXED);
        let exe = temp.0.join("mixed").to_string_lossy().into_owned();
        let o = vesper(&["build", &file, "-o", &exe]);
        assert!(o.status.success(), "{}", text(&o));
        let status = Command::new(&exe).status().unwrap();
        assert_eq!(status.code(), Some(49));
    }
}

#[test]
fn realtime_fail_rejections_are_found_in_the_standalone_function() {
    let source = "fn wrap(p: f64, q: f64) -> f64 eff [Realtime, Fail[ArithOverflow]] {
    p % q
}

fn main() -> u64 {
    handle Fail[ArithOverflow] in {
        let w = wrap(7.5, 2.0);
        if w > 1.0 {
            1
        } else {
            0
        }
    } with {
        return value => value,
        raise(error, resume) => 0,
    }
}
";
    let world = checked(source);
    for target in TARGETS {
        let (_, _, report) = verify_native_with_capabilities(
            &world,
            "main",
            Some(target),
            true,
            &BTreeSet::new(),
            &NativeVerifierPolicy {
                schema: POLICY_SCHEMA.into(),
                ..Default::default()
            },
        )
        .unwrap();
        let finding = report
            .findings
            .iter()
            .find(|f| f.rule == "rt-unknown-callee")
            .unwrap_or_else(|| panic!("{:?}", report.findings));
        assert_eq!(finding.function, "wrap");
        let offset = finding.location.unwrap();
        assert_eq!(line_col(source, offset).0, 2);
    }
}

#[test]
fn capability_functions_taking_function_values_get_their_own_instance() {
    let file = root().join("tests/fixtures/capability_effects/accept/closures.ves");
    let source = fs::read_to_string(&file).unwrap();
    let world = checked(&source);
    let evaluated = vesper::runtime::Runtime::new(&world)
        .run("main", vec![])
        .unwrap();
    assert_eq!(
        native::jit(&world, "main").unwrap().to_string(),
        evaluated.to_string()
    );
    for target in TARGETS {
        let (_, _, functions) = emit_and_collect(&world, "main", Some(target), true).unwrap();
        let apply = functions
            .iter()
            .find(|f| f.key == "apply$fn[f=triple]")
            .unwrap_or_else(|| {
                panic!("{:?}", functions.iter().map(|f| &f.key).collect::<Vec<_>>())
            });
        assert_eq!(apply.source, "apply");
        // The bound parameter is not passed; the call to `triple` is direct.
        assert_eq!(apply.function.signature.params.len(), 1);
        let triple = functions.iter().find(|f| f.key == "triple").unwrap();
        assert!(apply.callees.iter().any(|(id, _)| *id == triple.func_id));
        let derived = policy_from_capabilities(
            &world,
            &functions,
            &NativeVerifierPolicy {
                schema: POLICY_SCHEMA.into(),
                ..Default::default()
            },
        )
        .unwrap();
        let det = derived.policy.det_float.functions.unwrap();
        assert!(det.contains(&"apply$fn[f=triple]".to_string()), "{det:?}");
    }
}

// ---------------------------------------------------------------------------
// Generic ConstTime functions are checked per instance.

const GENERIC_CT: &str = "copy struct Secret[T] {
    value: T,
}

copy struct Boxed[T] {
    value: T,
}

fn ct_id[T](sink s: Secret[T]) -> Secret[T] eff [ConstTime] {
    s
}

fn pick[T](c: bool, sink s: Boxed[T]) -> Boxed[T] {
    if c {
        s
    } else {
        s
    }
}

fn declassify_u64(s: Secret[u64]) -> u64 eff [ConstTime] {
    let Secret {
        value
    }
    = ct_id(s);
    value
}

fn main() -> u64 {
    let s: Secret[u64] = Secret {
        value: 4
    };
    let t: Boxed[u64] = Boxed {
        value: 5
    };
    let Boxed {
        value
    }
    = pick(true, t);
    if value == 5 {
        declassify_u64(s)
    } else {
        0
    }
}
";

#[test]
fn generic_consttime_functions_are_checked_with_instance_layouts() {
    let world = checked(GENERIC_CT);
    for target in TARGETS {
        let (_, _, functions) = emit_and_collect(&world, "main", Some(target), true).unwrap();
        let instance = functions
            .iter()
            .find(|f| f.source == "ct_id")
            .expect("ct_id instance");
        assert!(!instance.vars.is_empty());
        let concrete = instance.instance(&world).unwrap();
        assert_eq!(concrete.params[0].ty.to_string(), "Secret[u64]");
        let derived = policy_from_capabilities(
            &world,
            &functions,
            &NativeVerifierPolicy {
                schema: POLICY_SCHEMA.into(),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(derived.findings.is_empty(), "{:?}", derived.findings);
        assert_eq!(derived.policy.const_time[&instance.key].secret, ["s"]);
        let (_, _, report) = verify_native_with_capabilities(
            &world,
            "main",
            Some(target),
            true,
            &BTreeSet::new(),
            &NativeVerifierPolicy {
                schema: POLICY_SCHEMA.into(),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(report.findings.is_empty(), "{:?}", report.findings);

        // A generic instance named by a policy file is checked too: `pick`
        // branches on `c`, which the file declares secret.
        let pick = functions.iter().find(|f| f.source == "pick").unwrap();
        let mut policy = NativeVerifierPolicy {
            schema: POLICY_SCHEMA.into(),
            ..Default::default()
        };
        policy.const_time.insert(
            pick.key.clone(),
            ConstTimeEntry {
                secret: vec!["c".into()],
                ..Default::default()
            },
        );
        let (_, _, report) = verify_native_with_capabilities(
            &world,
            "main",
            Some(target),
            false,
            &BTreeSet::from([NativeVerifier::ConstTime]),
            &policy,
        )
        .unwrap();
        let leak = report
            .findings
            .iter()
            .find(|f| f.function == pick.key)
            .unwrap_or_else(|| panic!("{:?}", report.findings));
        assert_eq!(leak.verifier, NativeVerifier::ConstTime);
    }
}

// ---------------------------------------------------------------------------
// NaN canonicalisation in the batch backend.

#[test]
fn detfloat_batches_canonicalize_nans() {
    let source = "fn nan(x: f64) -> f64 eff [DetFloat] {
    x / x
}

fn main() -> f64 {
    nan(0.0)
}
";
    let world = checked(source);
    let mut batch = native::batch::NativeBatch::compile(
        &world,
        &["main"],
        native::batch::BatchLimits::default(),
    )
    .unwrap();
    let value = batch.run("main").unwrap();
    let vesper::runtime::Value::Float(f) = value else {
        panic!("{value:?}")
    };
    // Cranelift's canonical NaN; x86-64 would produce 0xfff8... otherwise.
    assert_eq!(f.to_bits(), 0x7ff8_0000_0000_0000);
}

// ---------------------------------------------------------------------------
// Native coverage for verify --refute and fuzz --differential.

const CAMPAIGNS: &str = "fn safe(key: u64) -> bool {
    let magic: u64 = 4660 * 4660 + 77;
    key != magic
}

fn halve(key: u64) -> u64 {
    key / 2
}

fn halve_fast(key: u64) -> u64 {
    let magic: u64 = 4660 * 4660 + 77;
    if key == magic {
        0
    } else {
        key / 2
    }
}

fn main() -> u64 {
    halve(4)
}
";

#[test]
fn refute_with_native_coverage_finds_a_computed_magic_value() {
    let temp = Temp::new();
    let file = temp.write("campaigns.ves", CAMPAIGNS);
    let run = |native: bool| {
        let mut args = vec![
            "verify",
            "--refute",
            file.as_str(),
            "--contract",
            "safe",
            "--execs",
            "2000",
        ];
        if native {
            args.push("--native-coverage");
        }
        vesper(&args)
    };
    let o = run(true);
    let t = text(&o);
    assert!(t.contains("native coverage:"), "{t}");
    assert!(
        t.contains(&format!("counterexample safe({MAGIC}) is false")),
        "{t}"
    );
    assert!(!o.status.success());
    // The evaluator-guided search does not reach it.
    let o = run(false);
    let t = text(&o);
    assert!(t.contains("no counterexample found"), "{t}");
}

#[test]
fn differential_with_native_coverage_runs_both_sides_natively() {
    let temp = Temp::new();
    let file = temp.write("campaigns.ves", CAMPAIGNS);
    let run = |native: bool| {
        let mut args = vec![
            "fuzz",
            file.as_str(),
            "--target",
            "halve",
            "--differential",
            "halve_fast",
            "--execs",
            "2000",
        ];
        if native {
            args.push("--native-coverage");
        }
        vesper(&args)
    };
    let o = run(true);
    let t = text(&o);
    assert!(t.contains("native coverage (shadow halve_fast):"), "{t}");
    assert!(t.contains("counterexample"), "{t}");
    assert!(t.contains(&format!("halve({MAGIC})")), "{t}");
    assert!(t.contains("both native"), "{t}");
    assert!(!o.status.success());
    let o = run(false);
    let t = text(&o);
    assert!(t.contains("no failure found within budget"), "{t}");
}

#[test]
fn native_coverage_cross_checks_results_with_the_evaluator() {
    let world = checked(CAMPAIGNS);
    let mut safe = NativeCoverage::compile(CAMPAIGNS, &world, "safe").unwrap();
    assert_eq!(safe.run(&[MAGIC]).unwrap(), NativeOutcome::Returned);
    assert_eq!(safe.last_result(), Some(0));
    assert_eq!(safe.run(&[1]).unwrap(), NativeOutcome::Returned);
    assert_eq!(safe.last_result(), Some(1));
    let _ = object_with_coverage(&world, "main", Some(TARGETS[1]), true, 0x1000_0000).unwrap();
}

#[test]
fn native_differential_compares_results_native_code_does_not_report() {
    // f64 results have no native result word; the evaluator's outcomes
    // (cross-checked against native code) still decide the comparison.
    let source = "fn fa(x: u64) -> f64 {
    1.0
}

fn fb(x: u64) -> f64 {
    2.0
}

fn main() -> u64 {
    0
}
";
    let temp = Temp::new();
    let file = temp.write("floats.ves", source);
    let o = vesper(&[
        "fuzz",
        &file,
        "--target",
        "fa",
        "--differential",
        "fb",
        "--native-coverage",
        "--execs",
        "50",
    ]);
    let t = text(&o);
    assert!(t.contains("failure [divergence]"), "{t}");
    assert!(!o.status.success(), "{t}");
}

#[test]
fn bound_consttime_functions_are_trusted_callees_of_their_instance() {
    let source = "copy struct Secret[T] {
    value: T,
}

fn keep(s: Secret[u64]) -> Secret[u64] eff [ConstTime] {
    s
}

fn apply(sink f: fn(Secret[u64]) -> Secret[u64] eff [ConstTime], sink s: Secret[u64]) -> Secret[u64] eff [ConstTime] {
    f(s)
}

fn declassify_u64(s: Secret[u64]) -> u64 eff [ConstTime] {
    let Secret {
        value
    }
    = apply(keep, s);
    value
}

fn main() -> u64 {
    let s: Secret[u64] = Secret {
        value: 4
    };
    declassify_u64(s)
}
";
    let world = checked(source);
    for target in TARGETS {
        let (_, _, functions) = emit_and_collect(&world, "main", Some(target), true).unwrap();
        let key = "apply$fn[f=keep]";
        assert!(functions.iter().any(|f| f.key == key));
        let derived = policy_from_capabilities(
            &world,
            &functions,
            &NativeVerifierPolicy {
                schema: POLICY_SCHEMA.into(),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            derived.policy.const_time[key].constant_time_callees,
            ["keep"]
        );
        let (_, _, report) = verify_native_with_capabilities(
            &world,
            "main",
            Some(target),
            true,
            &BTreeSet::new(),
            &NativeVerifierPolicy {
                schema: POLICY_SCHEMA.into(),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(report.findings.is_empty(), "{:?}", report.findings);
    }
}
