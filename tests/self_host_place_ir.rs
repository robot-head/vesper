//! Self-hosting SH3 lane 1 (#1240): `@vesper/place-ir` against the Rust
//! Place IR, over every corpus world at every native-path pass boundary.
//!
//! - Rust round trip: `Program` -> words -> `Program` is `Debug`-equal
//!   (tests/common/ir_words.rs alone).
//! - Reader: the Vesper tables' canonical dump equals the Rust dump.
//! - Writer: the Vesper tables written back as words decode to a
//!   `Program` `Debug`-equal to the input.
#[path = "common/check_harness.rs"]
mod check_harness;
#[path = "common/check_words.rs"]
mod check_words;
#[path = "common/ir_words.rs"]
mod ir_words;

use check_harness::*;
use std::sync::OnceLock;
use vesper::check::World;
use vesper::ir::Program;

fn package() -> &'static Package {
    static P: OnceLock<Package> = OnceLock::new();
    P.get_or_init(|| Package::load("packages/vesper-place-ir"))
}

/// Every `n`-th corpus world (`PLACE_IR_STEP`, default 4): the passes of
/// every world take about half an hour in a debug build, so the default
/// runs a slice; `PLACE_IR_STEP=1` runs every world (the report's numbers).
fn step() -> usize {
    std::env::var("PLACE_IR_STEP")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|n| *n > 0)
        .unwrap_or(4)
}

/// One program at one pass boundary.
pub struct Stage {
    pub name: String,
    pub world: &'static World,
    pub program: Program,
}

/// Every corpus world's program after each native-path pass: lowering
/// (with the FFI drops), then with optimize on `ir_opt`, the inliner, drop
/// elaboration and bounds hoisting; with optimize off `fip_reuse_pass` and
/// drop elaboration. A pass that refuses a program ends its stages.
pub fn stages() -> &'static [Stage] {
    static S: OnceLock<Vec<Stage>> = OnceLock::new();
    S.get_or_init(|| {
        let mut out = vec![];
        let quiet = quiet_panics();
        for c in corpus().iter().step_by(step()) {
            let w = &c.world;
            let r = std::panic::catch_unwind(|| {
                let mut v = vec![];
                let lowered = vesper::ffi_lowering::drops::lower_world(w);
                v.push(("lower", lowered.clone()));
                let mut p = lowered.clone();
                if vesper::ir_opt::optimize(w, &mut p).is_ok() {
                    v.push(("opt", p.clone()));
                    vesper::ir_inline::inline(w, &mut p, &vesper::ir_inline::Budget::default());
                    v.push(("inline", p.clone()));
                    vesper::fip::elaborate_non_fip_drops(w, &mut p);
                    v.push(("drops", p.clone()));
                    vesper::bounds_hoist::run(&mut p);
                    v.push(("hoist", p));
                }
                let mut q = lowered;
                if vesper::ir_opt::fip_reuse_pass(w, &mut q).is_ok() {
                    vesper::fip::elaborate_non_fip_drops(w, &mut q);
                    v.push(("drops-unopt", q));
                }
                v
            });
            if let Ok(v) = r {
                for (stage, program) in v {
                    out.push(Stage {
                        name: format!("{}@{stage}", c.name),
                        world: w,
                        program,
                    });
                }
            }
        }
        drop(quiet);
        out
    })
}

fn debug(p: &Program) -> String {
    format!("{p:#?}")
}

#[test]
fn rust_round_trip_is_debug_equal() {
    let (mut bad, mut skipped, mut words) = (0, 0, 0usize);
    for s in stages() {
        let Some((w, pool)) = ir_words::program_words(&s.program) else {
            skipped += 1;
            continue;
        };
        words += w.len();
        let back = ir_words::decode_program(&w, &pool);
        if let Some(d) = first_difference(&debug(&s.program), &debug(&back)) {
            bad += 1;
            if bad <= 5 {
                eprintln!("rust round trip: {} differs at {d}", s.name);
            }
        }
    }
    eprintln!(
        "rust round trip: {bad} of {} stages differ, {skipped} not encodable, {words} words",
        stages().len()
    );
    assert!(stages().len() > 50, "only {} stages", stages().len());
    assert_eq!(bad, 0);
}

#[test]
fn vesper_dump_matches_rust() {
    let dump = package().entry("place_ir_dump", true);
    let (mut bad, mut skipped, mut lines) = (0, 0, 0);
    for s in stages() {
        let Some((input, _)) = ir_words::program_input(s.world, &s.program) else {
            skipped += 1;
            continue;
        };
        let r = ir_words::dump(&s.program);
        lines += r.lines().count();
        if let Some(d) = first_difference(&r, &dump.text(&input)) {
            bad += 1;
            if bad <= 5 {
                eprintln!("dump: {} differs at {d}", s.name);
            }
        }
    }
    eprintln!(
        "dump: {bad} of {} stages differ, {skipped} not encodable, {lines} lines",
        stages().len()
    );
    assert_eq!(bad, 0);
}

#[test]
fn vesper_writer_round_trip_is_debug_equal() {
    let write = package().entry("place_ir_words", true);
    let (mut bad, mut skipped) = (0, 0);
    for s in stages() {
        let Some((input, pool)) = ir_words::program_input(s.world, &s.program) else {
            skipped += 1;
            continue;
        };
        let back = ir_words::decode_program(&write.words(&input), &pool);
        if let Some(d) = first_difference(&debug(&s.program), &debug(&back)) {
            bad += 1;
            if bad <= 5 {
                eprintln!("writer: {} differs at {d}", s.name);
            }
        }
    }
    eprintln!(
        "writer: {bad} of {} stages differ, {skipped} not encodable",
        stages().len()
    );
    assert_eq!(bad, 0);
}

#[test]
fn vesper_json_matches_rust() {
    let json = package().entry("place_ir_json", true);
    let (mut bad, mut skipped, mut bytes) = (0, 0, 0usize);
    for s in stages() {
        let Some(input) = ir_words::program_json_input(&s.program) else {
            skipped += 1;
            continue;
        };
        let expected = serde_json::to_string_pretty(&s.program).expect("serialize Place IR");
        bytes += expected.len();
        let actual = json.text(&input);
        if let Some(d) = first_difference(&expected, &actual) {
            bad += 1;
            if bad <= 5 {
                eprintln!("json: {} differs at {d}", s.name);
            }
        }
    }
    eprintln!(
        "json: {bad} of {} stages differ, {skipped} not encodable, {bytes} bytes",
        stages().len()
    );
    assert_eq!(bad, 0);
}

#[test]
fn vesper_json_covers_edge_cases() {
    use std::collections::{BTreeMap, BTreeSet};
    use vesper::ast::{
        Effect, Effects, Expr, ExprKind, Literal, Mode, Param, Pass, Pattern, PatternKind, Type,
    };
    use vesper::check::Place;
    use vesper::fip::{CallSlot, FipDrop, FipJoinSlot, FipOp};
    use vesper::ir::{Argument, Block, FunctionIr, Instruction, Op, Terminator};
    use vesper::seq::{Access, IndexForm, SeqOp, Wide};
    use vesper::strings::StrOp;
    use vesper::tiled::TiledOp;
    use vesper::vector::{BinOp, Elem, Intrinsic, Profile, Shape, SliceIo, VectorOp};

    let json = package().entry("place_ir_json", true);

    // Empty program `{ "functions": {} }`.
    let empty = Program::default();
    let empty_in = ir_words::program_json_input(&empty).expect("empty program encodes");
    assert_eq!(
        json.text(&empty_in),
        serde_json::to_string_pretty(&empty).unwrap()
    );

    let shape = Shape::new(Elem::U8, 16).unwrap();
    let wide = Wide {
        bytes: 4,
        big: true,
        write: false,
        total: true,
    };
    let fn_ty = Type::Function {
        params: vec![(Pass::In, Type::named("i64")), (Pass::Sink, Type::Infer)],
        result: Box::new(Type::IntLiteral(-42)),
        effects: Effects {
            instances: vec![Effect {
                label: Some("lbl".into()),
                name: "Fail".into(),
                args: vec![Type::named("RuntimeError")],
            }],
            tail: Some("r".into()),
        },
        mode: Mode::Affine,
    };

    let mk_inst =
        |dest: u32, op: Op, ty: Type, token: Option<(u32, u32)>, fallible: bool| Instruction {
            dest,
            op,
            ty,
            span: 3..19,
            token,
            fallible,
        };

    let escaped = "quote\"slash\\bs\x08ff\x0cnl\ncr\rtab\tnul\x00ctrl\x1bdel\x7futf8-λ";
    let instructions = vec![
        mk_inst(
            0,
            Op::Constant(Literal::Int(i128::MIN)),
            Type::named("i128"),
            None,
            false,
        ),
        mk_inst(
            1,
            Op::Constant(Literal::Int(i128::MAX)),
            Type::named("i128"),
            None,
            false,
        ),
        mk_inst(
            2,
            Op::Constant(Literal::Int(-1)),
            Type::named("i64"),
            None,
            false,
        ),
        mk_inst(
            3,
            Op::Constant(Literal::WideInt(u128::MAX)),
            Type::named("u128"),
            None,
            false,
        ),
        mk_inst(
            4,
            Op::Constant(Literal::Float(-0.0)),
            Type::named("f64"),
            None,
            false,
        ),
        mk_inst(
            5,
            Op::Constant(Literal::Float(f64::NAN)),
            Type::named("f64"),
            None,
            false,
        ),
        mk_inst(
            6,
            Op::Constant(Literal::Float(1.25e-12)),
            Type::named("f64"),
            None,
            false,
        ),
        mk_inst(
            7,
            Op::Constant(Literal::String(escaped.into())),
            Type::named("String"),
            None,
            false,
        ),
        mk_inst(8, Op::Unit, Type::unit(), Some((0, 1)), true),
        mk_inst(
            9,
            Op::Bind {
                pattern: Pattern {
                    kind: PatternKind::Constructor {
                        name: "Pair".into(),
                        fields: vec![
                            (
                                "a".into(),
                                Pattern {
                                    kind: PatternKind::Literal(Literal::Float(3.5)),
                                    span: 1..2,
                                    integer_kind: None,
                                },
                            ),
                            (
                                "b".into(),
                                Pattern {
                                    kind: PatternKind::Tuple(vec![Pattern {
                                        kind: PatternKind::Wild,
                                        span: 2..3,
                                        integer_kind: None,
                                    }]),
                                    span: 2..4,
                                    integer_kind: None,
                                },
                            ),
                        ],
                        named: true,
                    },
                    span: 1..5,
                    integer_kind: None,
                },
                value: 0,
            },
            fn_ty.clone(),
            None,
            false,
        ),
        mk_inst(
            10,
            Op::Closure {
                expr: Expr {
                    kind: ExprKind::Literal(Literal::String("inner\nline".into())),
                    span: 4..9,
                },
                aliases: BTreeMap::from([("b".into(), "x".into()), ("a".into(), "y".into())]),
                moves: vec![Place {
                    root: "m".into(),
                    path: vec!["f".into(), "0".into()],
                }],
            },
            fn_ty.clone(),
            None,
            false,
        ),
        mk_inst(
            11,
            Op::Seq {
                op: SeqOp::Hoisted(Access::Wide(wide)),
                args: vec![Argument {
                    pass: Pass::Inout,
                    value: 0,
                    place: Some(Place {
                        root: "buf".into(),
                        path: vec![],
                    }),
                }],
            },
            Type::unit(),
            None,
            false,
        ),
        mk_inst(
            12,
            Op::Seq {
                op: SeqOp::BoundsGuard {
                    inclusive: true,
                    form: IndexForm {
                        negated: true,
                        plus: 2,
                        minus: 1,
                        scaled: true,
                        outer_plus: 3,
                        outer_minus: 4,
                        width: 8,
                    },
                },
                args: vec![],
            },
            Type::named("bool"),
            None,
            false,
        ),
        mk_inst(
            13,
            Op::Seq {
                op: SeqOp::AtomicLoad { width: Some(8) },
                args: vec![],
            },
            Type::named("u64"),
            None,
            false,
        ),
        mk_inst(
            14,
            Op::Seq {
                op: SeqOp::AtomicStore { width: None },
                args: vec![],
            },
            Type::named("bool"),
            None,
            false,
        ),
        mk_inst(
            15,
            Op::Seq {
                op: SeqOp::Str(StrOp::PushChar),
                args: vec![],
            },
            Type::named("bool"),
            None,
            false,
        ),
        mk_inst(
            16,
            Op::Seq {
                op: SeqOp::Vector(SliceIo {
                    shape,
                    store: true,
                    masked: false,
                }),
                args: vec![],
            },
            Type::unit(),
            None,
            false,
        ),
        mk_inst(
            17,
            Op::Vector {
                op: VectorOp::Lanes {
                    intrinsic: Intrinsic::Binary(BinOp::Add),
                    shape,
                    constants: vec![0, 7, 15],
                },
                args: vec![0, 1],
            },
            Type::unit(),
            None,
            false,
        ),
        mk_inst(
            18,
            Op::Vector {
                op: VectorOp::IsaSupported(Profile::Avx2),
                args: vec![],
            },
            Type::named("bool"),
            None,
            false,
        ),
        mk_inst(
            19,
            Op::Tiled {
                op: TiledOp::LoadTileVec {
                    adt: "Row\"λ\x07".into(),
                    field: "col'1".into(),
                    shape,
                },
                args: vec![],
                capability: Some("reg".into()),
            },
            Type::unit(),
            None,
            false,
        ),
        mk_inst(
            20,
            Op::Fip {
                op: FipOp::BoxReuse {
                    bind_dest: 9,
                    slot_index: 2,
                    size: 24,
                },
                args: vec![],
            },
            Type::unit(),
            None,
            false,
        ),
        mk_inst(21, Op::Loop, Type::unit(), None, false),
        mk_inst(
            22,
            Op::Exit {
                value: 2,
                owner: "f".into(),
            },
            Type::unit(),
            None,
            false,
        ),
    ];

    let func = FunctionIr {
        name: "f".into(),
        params: vec![Param {
            name: "p".into(),
            pass: Pass::Inout,
            projections: vec![vec!["x".into(), "y".into()]],
            ty: fn_ty,
            span: 0..10,
        }],
        result: Type::unit(),
        effects: Effects::default(),
        bounds: BTreeMap::from([("T".into(), Mode::Linear), ("A".into(), Mode::Copy)]),
        external_captures: BTreeSet::from(["cap_b".into(), "cap_a".into()]),
        blocks: vec![Block {
            entry_token: 0,
            incoming_tokens: vec![(0, 1)],
            outgoing_token: 2,
            params: vec![0],
            instructions,
            terminator: Terminator::Unreachable,
        }],
        values: BTreeMap::from([(10, Type::unit()), (2, Type::named("i64"))]),
        locals: BTreeMap::from([
            ("z".into(), Type::unit()),
            ("a".into(), Type::IntLiteral(7)),
        ]),
        fip_call_slots: BTreeMap::from([
            (1, CallSlot::CallerSlot),
            (
                2,
                CallSlot::ReuseSlot {
                    bind_dest: 9,
                    slot_index: 0,
                    size: 16,
                },
            ),
        ]),
        fip_join_slots: BTreeMap::from([(
            0,
            vec![FipJoinSlot {
                dest_slot: (3, 1),
                incoming: vec![(0, 1, 0), (0, 2, 1)],
            }],
        )]),
        box_drops_before_inst: BTreeMap::from([(
            0,
            vec![(0, FipDrop::Value(1, Type::named("Box")))],
        )]),
        box_drops_after_inst: BTreeMap::from([(
            0,
            vec![(
                1,
                FipDrop::Place(
                    Place {
                        root: "loc".into(),
                        path: vec!["0".into()],
                    },
                    Type::named("Box"),
                ),
            )],
        )]),
        box_drops_before_term: BTreeMap::from([(
            0,
            vec![FipDrop::OtherFields {
                value: 4,
                ty: Type::named("Box"),
                keep: "0".into(),
            }],
        )]),
    };

    let mut prog = Program::default();
    prog.functions.insert("f".into(), func);
    let input = ir_words::program_json_input(&prog).expect("synthetic program encodes");
    let expected = serde_json::to_string_pretty(&prog).unwrap();
    let actual = json.text(&input);
    if let Some(d) = first_difference(&expected, &actual) {
        panic!("synthetic json differs at {d}");
    }
}

/// The cost of a swap, per corpus world at the `lower` boundary: encoding
/// (Rust), the Vesper read and write, and decoding (Rust), next to the
/// Rust lowering it would replace. IR lanes subtract it from their pass.
#[test]
#[ignore = "timing; CHECK_HARNESS_SPEED=1"]
fn place_ir_cost() {
    if !speed() {
        return;
    }
    let write = package().entry("place_ir_words", true);
    let lowered: Vec<&Stage> = stages()
        .iter()
        .filter(|s| s.name.ends_with("@lower"))
        .collect();
    let inputs: Vec<(Vec<u8>, ir_words::Pool)> = lowered
        .iter()
        .filter_map(|s| ir_words::program_input(s.world, &s.program))
        .collect();
    let outputs: Vec<Vec<u64>> = inputs.iter().map(|(i, _)| write.words(i)).collect();
    let best = |f: &mut dyn FnMut()| (0..5).map(|_| time(&mut *f)).min().unwrap();
    let lower = best(&mut || {
        for s in &lowered {
            std::hint::black_box(vesper::ir::lower_world(s.world));
        }
    });
    let encode = best(&mut || {
        for s in &lowered {
            std::hint::black_box(ir_words::program_input(s.world, &s.program));
        }
    });
    let vesper = best(&mut || {
        for (i, _) in &inputs {
            std::hint::black_box(write.words(i));
        }
    });
    let decode = best(&mut || {
        for (o, (_, pool)) in outputs.iter().zip(&inputs) {
            std::hint::black_box(ir_words::decode_program(o, pool));
        }
    });
    let bytes: usize = inputs.iter().map(|(i, _)| i.len()).sum();
    eprintln!(
        "place-ir cost over {} worlds ({} input MB): lower_world {lower:?}, encode {encode:?}, \
         Vesper read+write {vesper:?}, decode {decode:?}",
        inputs.len(),
        bytes / 1_000_000
    );
}

/// One phase of a swap over the lowered stages, for callgrind:
/// `PLACE_IR_PHASE` is `lower` (Rust `ir::lower_world`), `encode`,
/// `vesper` (read + write), `decode` or `none` (setup only). Collect inside
/// the phase only:
///
/// ```text
/// PLACE_IR_PHASE=vesper PLACE_IR_STEP=10 valgrind --tool=callgrind \
///   --toggle-collect='*place_ir_measured*' <test binary> --ignored \
///   --exact place_ir_callgrind --nocapture
/// ```
///
/// Add `--instr-atstart=no` and `PLACE_IR_GO=<file>` to skip simulating
/// the setup (see below); docs/parallel-results/self-host-place-ir-data.md
/// has the driver loop.
#[test]
#[ignore = "callgrind; PLACE_IR_PHASE"]
fn place_ir_callgrind() {
    let Ok(phase) = std::env::var("PLACE_IR_PHASE") else {
        return;
    };
    let write = package().entry("place_ir_words", true);
    // Lowering only (not `stages()`, whose later passes would cost hours
    // under callgrind).
    let quiet = quiet_panics();
    let owned: Vec<Stage> = corpus()
        .iter()
        .step_by(step())
        .filter_map(|c| {
            let w = &c.world;
            std::panic::catch_unwind(|| vesper::ffi_lowering::drops::lower_world(w))
                .ok()
                .map(|program| Stage {
                    name: c.name.clone(),
                    world: w,
                    program,
                })
        })
        .collect();
    drop(quiet);
    let lowered: Vec<&Stage> = owned.iter().collect();
    let inputs: Vec<(Vec<u8>, ir_words::Pool)> = lowered
        .iter()
        .filter_map(|s| ir_words::program_input(s.world, &s.program))
        .collect();
    let outputs: Vec<Vec<u64>> = inputs.iter().map(|(i, _)| write.words(i)).collect();
    let words: usize = outputs.iter().map(Vec::len).sum();
    // With `PLACE_IR_GO=<file>`, wait for the file: a driver run with
    // `--instr-atstart=no` turns instrumentation on (`callgrind_control -i
    // on`) after the setup, which takes minutes natively, then creates it.
    if let Ok(go) = std::env::var("PLACE_IR_GO") {
        eprintln!("place-ir callgrind: ready");
        while !std::path::Path::new(&go).exists() {
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    }
    match phase.as_str() {
        "lower" => place_ir_measured_lower(&lowered),
        "encode" => place_ir_measured_encode(&lowered),
        "vesper" => place_ir_measured_vesper(&write, &inputs),
        "decode" => place_ir_measured_decode(&outputs, &inputs),
        "none" => {}
        other => panic!("unknown PLACE_IR_PHASE {other}"),
    }
    eprintln!(
        "place-ir callgrind: {phase} over {} worlds ({words} output words)",
        inputs.len()
    );
}

#[inline(never)]
fn place_ir_measured_lower(lowered: &[&Stage]) {
    for s in lowered {
        std::hint::black_box(vesper::ir::lower_world(s.world));
    }
}

#[inline(never)]
fn place_ir_measured_encode(lowered: &[&Stage]) {
    for s in lowered {
        std::hint::black_box(ir_words::program_input(s.world, &s.program));
    }
}

#[inline(never)]
fn place_ir_measured_vesper(write: &Entry, inputs: &[(Vec<u8>, ir_words::Pool)]) {
    for (i, _) in inputs {
        std::hint::black_box(write.words(i));
    }
}

#[inline(never)]
fn place_ir_measured_decode(outputs: &[Vec<u64>], inputs: &[(Vec<u8>, ir_words::Pool)]) {
    for (o, (_, pool)) in outputs.iter().zip(inputs) {
        std::hint::black_box(ir_words::decode_program(o, pool));
    }
}

/// The pass harness with the identity pass on both sides agrees at every
/// boundary, and catches a real difference: `ir_opt` on the Rust side
/// against the Vesper identity must differ on some world.
/// Every `HARNESS_STEP`-th stage goes through the pass harness.
const HARNESS_STEP: usize = 16;

#[test]
fn pass_harness_agrees_on_identity_and_catches_a_pass() {
    let write = package().entry("place_ir_words", true);
    let (mut bad, mut caught, mut skipped) = (0, 0, 0);
    // The IR check runs twice per case and takes seconds on the larger
    // programs in a debug build, so every sixteenth stage (the other
    // tests already cover every stage; this one checks the harness).
    for s in stages().iter().step_by(HARNESS_STEP) {
        let same = ir_words::compare_pass(s.world, &s.program, |_| {}, |i| write.words(i));
        skipped += same.skipped as usize;
        if !same.agrees() {
            bad += 1;
            if bad <= 5 {
                eprintln!("pass harness: {} identity differs: {same:?}", s.name);
            }
        }
    }
    // `ir_opt` against the identity, on lowered programs until it has
    // changed three of them.
    for s in stages().iter().filter(|s| s.name.ends_with("@lower")) {
        if caught == 3 {
            break;
        }
        let opt = ir_words::compare_pass(
            s.world,
            &s.program,
            |p| {
                let _ = vesper::ir_opt::optimize(s.world, p);
            },
            |i| write.words(i),
        );
        caught += (!opt.agrees()) as usize;
    }
    eprintln!(
        "pass harness: {bad} of {} identity cases differ, {skipped} skipped; ir_opt vs identity differs on {caught} worlds",
        stages().len().div_ceil(HARNESS_STEP)
    );
    assert_eq!(bad, 0);
    assert!(caught > 0, "the harness never saw ir_opt change a program");
}
