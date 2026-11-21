//! The swap-in glue: each swapped module's Vesper binding, wrapped as the
//! module's Rust function table and installed before `main` runs.
//!
//! Installing from a static constructor keeps the included CLI's `main`
//! untouched (the same std start-up, the same CLI thread). The table only
//! stores function pointers, so it needs nothing from std yet.
//!
//! Set `VESPER_HYBRID_STATS=1` to print, at exit, how many calls each
//! swapped module answered from Vesper (the differential harness uses it to
//! show the pilot really ran).
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

#[rustfmt::skip]
#[allow(clippy::all)]
#[path = "../port/layout.rs"]
mod layout_binding;

#[rustfmt::skip]
#[allow(clippy::all)]
#[path = "../port/parse.rs"]
mod parse_binding;

#[rustfmt::skip]
#[allow(clippy::all)]
#[path = "../port/check_layout.rs"]
mod check_layout_binding;

#[rustfmt::skip]
#[allow(clippy::all)]
#[path = "../port/ir_check.rs"]
mod ir_check_binding;

#[rustfmt::skip]
#[allow(clippy::all)]
#[path = "../port/codegen.rs"]
mod codegen_binding;

#[rustfmt::skip]
#[allow(clippy::all)]
#[path = "../port/codegen_export.rs"]
mod codegen_export_binding;

#[rustfmt::skip]
#[allow(clippy::all)]
#[path = "../port/test_runner.rs"]
mod test_runner_binding;

#[rustfmt::skip]
#[allow(clippy::all)]
#[path = "../port/ir_inline.rs"]
mod ir_inline_binding;

#[rustfmt::skip]
#[allow(clippy::all)]
#[path = "../port/optimize.rs"]
mod optimize_binding;

#[rustfmt::skip]
#[allow(clippy::all)]
#[path = "../port/place_ir.rs"]
mod place_ir_binding;

#[rustfmt::skip]
#[allow(clippy::all)]
#[path = "../port/bounds_hoist.rs"]
mod bounds_hoist_binding;

#[path = "parse_words.rs"]
mod parse_words;

// The export glue's interface encoder (#1286), shared with the root
// package's tests/self_host_codegen_export.rs.
#[allow(dead_code)]
#[path = "../../../tests/self_host_codegen_export/words.rs"]
mod export_words;

// The checker swaps' input encoder and result decoders (#1177), shared with
// the root package's tests/hybrid_layout.rs and tests/hybrid_ir_check.rs.
#[path = "../../../tests/common/check_words.rs"]
mod check_words;

// The test-discovery swap's input encoders and result decoders (#1337),
// shared with the root package's tests/self_host_test_runner.rs.
#[path = "../../../tests/common/test_runner_words.rs"]
mod test_runner_words;

// The inliner swap's encoder and decoder (#1250), shared with the root
// package's tests/self_host_ir_inline.rs.
#[path = "../../../tests/common/inline_words.rs"]
mod inline_words;

// The AST optimizer swap's input encoder and answer readers (#1249),
// shared with the root package's tests/self_host_optimize*.rs.
#[path = "../../../tests/common/optimize_words.rs"]
mod optimize_words;

// Place IR words both ways (#1240), shared with tests/self_host_place_ir*.rs.
#[path = "../../../tests/common/ir_words.rs"]
mod ir_words;

/// Calls answered by the Vesper word-layout module.
static LAYOUT_CALLS: AtomicU64 = AtomicU64::new(0);
/// Sources parsed by the Vesper parse module.
static PARSE_CALLS: AtomicU64 = AtomicU64::new(0);
/// `Layout::of` calls answered by the Vesper check-layout module.
static CHECK_LAYOUT_CALLS: AtomicU64 = AtomicU64::new(0);
/// Programs checked by the Vesper IR-check module.
static IR_CHECK_CALLS: AtomicU64 = AtomicU64::new(0);
/// `aggregate` requests the Vesper codegen module emitted, and declined.
static CODEGEN_CALLS: AtomicU64 = AtomicU64::new(0);
static CODEGEN_DECLINED: AtomicU64 = AtomicU64::new(0);
/// Libraries whose v2 glue the Vesper export module emitted, and declined.
static EXPORT_CALLS: AtomicU64 = AtomicU64::new(0);
static EXPORT_DECLINED: AtomicU64 = AtomicU64::new(0);
/// Sources whose tests the Vesper test-runner module discovered.
static TEST_RUNNER_CALLS: AtomicU64 = AtomicU64::new(0);
/// Programs inlined by the Vesper inliner module.
static IR_INLINE_CALLS: AtomicU64 = AtomicU64::new(0);
/// Expressions the Vesper optimizer proposed for, and proposals it checked.
static OPTIMIZE_PROPOSE_CALLS: AtomicU64 = AtomicU64::new(0);
static OPTIMIZE_VERIFY_CALLS: AtomicU64 = AtomicU64::new(0);
/// Programs the IR swap point's identity hooks sent through Vesper.
static PLACE_IR_CALLS: AtomicU64 = AtomicU64::new(0);
/// Programs `modules/bounds_hoist` hoisted.
static BOUNDS_HOIST_CALLS: AtomicU64 = AtomicU64::new(0);

fn count() {
    LAYOUT_CALLS.fetch_add(1, Relaxed);
}

/// `vesper::layout::WordLayout` from `modules/layout`.
const VESPER_WORD_LAYOUT: vesper::layout::WordLayout = vesper::layout::WordLayout {
    first_offset: |is_enum| {
        count();
        layout_binding::first_offset(is_enum)
    },
    next_offset: |offset, size| {
        count();
        let next = layout_binding::next_offset(offset, size);
        (next != layout_binding::no_fit()).then_some(next as i32)
    },
    product_size: |end| {
        count();
        layout_binding::product_size(end)
    },
    enum_size: |size, end| {
        count();
        layout_binding::enum_size(size, end)
    },
    word_offset: |index| {
        count();
        layout_binding::word_offset(index)
    },
};

/// `vesper_syntax::parser::ParseTable` from `modules/parse`: the front end
/// v2 and the AST of `@vesper/ast`, read back by `parse_words::decode`.
const VESPER_PARSE: vesper_syntax::parser::ParseTable = vesper_syntax::parser::ParseTable {
    parse: |source| {
        PARSE_CALLS.fetch_add(1, Relaxed);
        parse_words::decode(source, &parse_binding::parse_words(source))
    },
};

/// `vesper::layout::LayoutTable` from `modules/check_layout`: the layout of
/// `@vesper/check-layout`, over the ADTs the type can reach. A type whose
/// literals the decoder cannot read (over 64 bits) stays in Rust.
const VESPER_LAYOUT: vesper::layout::LayoutTable = vesper::layout::LayoutTable {
    of: |world, ty| {
        let Some(input) = check_words::layout_input(world, ty) else {
            return vesper::layout::rust_layout_of(world, ty);
        };
        CHECK_LAYOUT_CALLS.fetch_add(1, Relaxed);
        check_words::decode_layout(&check_layout_binding::layout_of_words(&input))
    },
};

/// `vesper::ir_check::IrCheckTable` from `modules/ir_check`: the IR check
/// of `@vesper/ir-check`. A program whose literals the decoder cannot read
/// stays in Rust.
const VESPER_IR_CHECK: vesper::ir_check::IrCheckTable = vesper::ir_check::IrCheckTable {
    validate: |world, program| {
        // `ir_input` refuses what `@vesper/ir-check` does not cover yet,
        // among them every world with element view matches (#1221): those
        // go to `rust_validate` (test: `element_views_stay_on_rust` in
        // tests/hybrid_ir_check.rs).
        let Some(input) = check_words::ir_input(world, program) else {
            return vesper::ir_check::rust_validate(world, program);
        };
        IR_CHECK_CALLS.fetch_add(1, Relaxed);
        check_words::decode_diagnostics(&ir_check_binding::validate_words(&input))
    },
};

/// The `aggregate` hook of `vesper::codegen_swap` from `modules/codegen`:
/// the census lowering family, emitted in Vesper through the lent builder
/// and module (`vesper_cranelift_sys::lend`).
fn vesper_aggregate(lent: &mut vesper::codegen_swap::Lent<'_, '_>) -> Result<bool, String> {
    let kind = if lent.jit {
        vesper_cranelift_sys::ModuleKind::Jit
    } else {
        vesper_cranelift_sys::ModuleKind::Object
    };
    let (answer, crossing) = vesper_cranelift_sys::lend_as(
        kind,
        lent.builder,
        &mut *lent.module,
        lent.op,
        lent.args,
        codegen_binding::aggregate,
    );
    if let Some(e) = crossing.error {
        return Err(format!("codegen (aggregate op {}): {e}", lent.op));
    }
    if crossing.unreturned {
        return Err(format!(
            "codegen (aggregate op {}) kept a lent handle",
            lent.op
        ));
    }
    if answer == 1 {
        CODEGEN_CALLS.fetch_add(1, Relaxed);
        lent.results.extend(crossing.results);
        Ok(true)
    } else {
        CODEGEN_DECLINED.fetch_add(1, Relaxed);
        Ok(false)
    }
}

/// The `export` hook of `vesper::codegen_swap` from `modules/codegen_export`
/// (#1286): the export ABI v2 glue after the runtime prelude, emitted in
/// Vesper over the lent module. The request's words are a flags word (what
/// the module already holds, `glue.ves`'s `F_*`) and the interface in the
/// words of tests/self_host_codegen_export/words.rs.
fn vesper_export(
    m: &mut dyn cranelift_module::Module,
    _world: &vesper::check::World,
    interface: &vesper::native::export::LibraryInterface,
) -> Option<Result<(), String>> {
    use cranelift_module::FuncOrDataId;
    // The Rust glue reports a library with no runtime symbols.
    interface.runtime.as_ref()?;
    let func = |name: &str| matches!(m.get_name(name), Some(FuncOrDataId::Func(_)));
    let data = |name: &str| matches!(m.get_name(name), Some(FuncOrDataId::Data(_)));
    let flags = u64::from(func("vesper_box_free"))
        | u64::from(data("vesper_seq_live")) << 1
        | u64::from(data("vesper_box_live")) << 2;
    let bytes = export_words::binding_input(interface, &Default::default());
    let mut words = vec![flags];
    words.extend(
        bytes
            .chunks(8)
            .map(|c| c.iter().rev().fold(0u64, |w, b| w << 8 | u64::from(*b))),
    );
    let (answer, crossing) =
        vesper_cranelift_sys::lend_module(m, 0, &words, codegen_export_binding::export);
    if let Some(e) = crossing.error {
        return Some(Err(format!("codegen (export glue): {e}")));
    }
    if crossing.unreturned {
        return Some(Err("codegen (export glue) kept a lent handle".into()));
    }
    if answer == 1 {
        EXPORT_CALLS.fetch_add(1, Relaxed);
        Some(Ok(()))
    } else {
        EXPORT_DECLINED.fetch_add(1, Relaxed);
        None
    }
}
/// `vesper::test_discovery::TestDiscoveryTable` from `modules/test_runner`:
/// `@case` rows and test discovery of `@vesper/test-runner`.
const VESPER_TEST_DISCOVERY: vesper::test_discovery::TestDiscoveryTable =
    vesper::test_discovery::TestDiscoveryTable {
        case_rows: |source, module| {
            let input = test_runner_words::case_rows_input(source, module);
            test_runner_words::decode_case_rows(
                source,
                &test_runner_binding::case_rows_words(&input),
            )
        },
        discover: |source, items, cases| {
            TEST_RUNNER_CALLS.fetch_add(1, Relaxed);
            let input = test_runner_words::discover_input(source, items, cases);
            test_runner_words::decode_discover(&test_runner_binding::discover_words(&input))
        },
    };

/// `ir_swap`'s `inline` hook from `modules/ir_inline`: the inliner of
/// `@vesper/ir-inline`. The package decides and splices; the hook encodes
/// the world's answers and the program, and reads the report and the
/// inlined program back (`inline_words::decode_program`).
fn vesper_inline(
    world: &vesper::check::World,
    program: &mut vesper::ir::Program,
    budget: &vesper::ir_inline::Budget,
) -> vesper::ir_inline::Report {
    let input = inline_words::inline_input(world, program, budget);
    IR_INLINE_CALLS.fetch_add(1, Relaxed);
    let words = ir_inline_binding::inline_words(&input.bytes);
    let (report, inlined) = inline_words::decode_program(&words, &input.tables, program);
    *program = inlined;
    report
}

/// `program` read into `@vesper/place-ir` tables and written back, in
/// place: the identity Vesper pass. A program the format cannot encode
/// stays as Rust made it.
fn place_ir_round_trip(world: Option<&vesper::check::World>, program: &mut vesper::ir::Program) {
    if let Some(p) = ir_words::through_vesper(world, program, place_ir_binding::round_trip_words) {
        PLACE_IR_CALLS.fetch_add(1, Relaxed);
        *program = p;
    }
}

/// `vesper::ir_swap::IrPasses` for the hybrid: each native IR pass in Rust,
/// then the program through `modules/place_ir` and back, proving the round
/// trip inside `vesper check`/`build`. SH3 lanes 9-13 replace a hook with
/// their Vesper pass.
const VESPER_IR_PASSES: vesper::ir_swap::IrPasses = vesper::ir_swap::IrPasses {
    lower: Some(|world| {
        let mut program = vesper::ir::lower_world(world);
        place_ir_round_trip(Some(world), &mut program);
        program
    }),
    opt: Some(|world, program| {
        let n = vesper::ir_opt::optimize(world, program)?;
        place_ir_round_trip(Some(world), program);
        Ok(n)
    }),
    // SH3 lane 11 (#1250): the inliner of `@vesper/ir-inline`.
    inline: Some(|world, program, budget| {
        let report = vesper_inline(world, program, budget);
        place_ir_round_trip(Some(world), program);
        report
    }),
    // SH3 lane 12 (#1251): bounds hoisting from `modules/bounds_hoist`.
    hoist: Some(|program| {
        let Some(p) = ir_words::through_vesper(None, program, bounds_hoist_binding::hoist_words)
        else {
            return vesper::bounds_hoist::run(program);
        };
        BOUNDS_HOIST_CALLS.fetch_add(1, Relaxed);
        let n = versioned_loops(&p).saturating_sub(versioned_loops(program));
        *program = p;
        n
    }),
};

/// `optimize`'s producer from `modules/optimize`: `propose_rewrite` of
/// `@vesper/optimize` on `@vesper/egraph`. The package searches, extracts
/// and proves; the candidate is rebuilt from the returned certificate by
/// `witness::rebuild_candidate`, the builder the Rust producer uses.
fn vesper_propose(
    source: &vesper::ast::Expr,
    types: &vesper::ast::TypeMap,
    limits: vesper::optimize::witness::WitnessLimits,
) -> Result<vesper::optimize::Proposal, vesper::optimize::witness::WitnessError> {
    use vesper::optimize::{Produced, Proposal};
    let types = optimize_words::relevant_types(&[source], types);
    let mut batch = optimize_words::Batch::new(1, true);
    batch.propose(source, &types, limits);
    OPTIMIZE_PROPOSE_CALLS.fetch_add(1, Relaxed);
    let words = optimize_binding::propose_words(&batch.bytes, &batch.names);
    let text = optimize_words::unpack_text(&words);
    let head = text.lines().next().unwrap_or("");
    match head {
        "P B" => Ok(Proposal::Barrier),
        "P U" => Ok(Proposal::Unchanged),
        _ if head.starts_with("P E ") => Err(optimize_words::error_of(
            head[4..].parse().expect("an error code"),
        )),
        _ => {
            let (mut certificate, spent, (terms_cap, proofs_cap)) =
                optimize_words::parse_certificate(&text);
            // The gate charges by the vectors' capacities, as the Rust
            // producer leaves them.
            certificate
                .terms
                .reserve_exact(terms_cap.saturating_sub(certificate.terms.len()));
            certificate
                .proofs
                .reserve_exact(proofs_cap.saturating_sub(certificate.proofs.len()));
            let candidate =
                vesper::optimize::witness::rebuild_candidate(source, &types, &certificate, limits)?;
            Ok(Proposal::Candidate(Box::new(Produced {
                candidate,
                certificate,
                limits,
                spent,
            })))
        }
    }
}

/// `optimize`'s install gate from `modules/optimize`: the certificate
/// checker of `@vesper/optimize`, continuing the producer's budget.
fn vesper_verify(
    source: &vesper::ast::Expr,
    candidate: &vesper::ast::Expr,
    types: &vesper::ast::TypeMap,
    certificate: &vesper::optimize::witness::RewriteCertificate,
    limits: vesper::optimize::witness::WitnessLimits,
    spent: &mut vesper::optimize::witness::Spent,
) -> Result<(), vesper::optimize::witness::WitnessError> {
    let types = optimize_words::relevant_types(&[source, candidate], types);
    let mut batch = optimize_words::Batch::new(1, true);
    batch.verify(
        source,
        candidate,
        &types,
        certificate,
        (certificate.terms.capacity(), certificate.proofs.capacity()),
        limits,
        *spent,
        false,
    );
    OPTIMIZE_VERIFY_CALLS.fetch_add(1, Relaxed);
    let words = optimize_binding::verify_words(&batch.bytes, &batch.names);
    let text = optimize_words::unpack_text(&words);
    let (result, after) =
        optimize_words::parse_verify_line(text.lines().next().expect("an R line"));
    *spent = after;
    result
}

const VESPER_OPTIMIZE: vesper::optimize::OptimizeTable = vesper::optimize::OptimizeTable {
    verify: vesper_verify,
    propose: Some(vesper_propose),
};

/// The loops of `program` behind a bounds guard: branches on a
/// `SeqOp::BoundsGuard` result of their own block, one per versioned loop
/// (the count `bounds_hoist::run` returns, which no caller reads).
fn versioned_loops(program: &vesper::ir::Program) -> usize {
    use vesper::ir::{Op, Terminator};
    use vesper::seq::SeqOp;
    program
        .functions
        .values()
        .flat_map(|f| &f.blocks)
        .filter(|b| {
            let Terminator::Branch { condition, .. } = b.terminator else {
                return false;
            };
            b.instructions.iter().any(|i| {
                i.dest == condition
                    && matches!(
                        i.op,
                        Op::Seq {
                            op: SeqOp::BoundsGuard { .. },
                            ..
                        }
                    )
            })
        })
        .count()
}

extern "C" fn install() {
    // Nothing runs before this constructor, so the slots are empty.
    if vesper::layout::install_word_layout(VESPER_WORD_LAYOUT).is_err() {
        std::process::abort();
    }
    if vesper_syntax::parser::install_parse(VESPER_PARSE).is_err() {
        std::process::abort();
    }
    if vesper::layout::install_layout(VESPER_LAYOUT).is_err() {
        std::process::abort();
    }
    if vesper::ir_check::install_ir_check(VESPER_IR_CHECK).is_err() {
        std::process::abort();
    }
    let hooks = vesper::codegen_swap::CodegenHooks {
        aggregate: Some(vesper_aggregate),
        export: Some(vesper_export),
        ..Default::default()
    };
    if vesper::codegen_swap::install(hooks).is_err() {
        std::process::abort();
    }
    if vesper::test_discovery::install_test_discovery(VESPER_TEST_DISCOVERY).is_err() {
        std::process::abort();
    }
    if vesper::ir_swap::install(VESPER_IR_PASSES).is_err() {
        std::process::abort();
    }
    // `VESPER_HYBRID_RUST_PRODUCER=1` keeps the Rust producer behind the
    // Vesper gate: the differential's check of the optimizer's accepted
    // equal-cost ties (tests/differential.rs).
    let table = if std::env::var_os("VESPER_HYBRID_RUST_PRODUCER").is_some_and(|v| v == "1") {
        vesper::optimize::OptimizeTable {
            propose: None,
            ..VESPER_OPTIMIZE
        }
    } else {
        VESPER_OPTIMIZE
    };
    if vesper::optimize::install_optimize(table).is_err() {
        std::process::abort();
    }
    if std::env::var_os("VESPER_HYBRID_STATS").is_some_and(|v| v == "1") {
        unsafe extern "C" {
            fn atexit(f: extern "C" fn()) -> std::ffi::c_int;
        }
        // SAFETY: `report` is a plain `extern "C" fn` that only formats
        // and writes to stderr.
        unsafe { atexit(report) };
    }
}

extern "C" fn report() {
    eprintln!(
        "vesper-hybrid: layout answered {} calls from Vesper",
        LAYOUT_CALLS.load(Relaxed)
    );
    eprintln!(
        "vesper-hybrid: parse answered {} calls from Vesper",
        PARSE_CALLS.load(Relaxed)
    );
    eprintln!(
        "vesper-hybrid: check-layout answered {} calls from Vesper",
        CHECK_LAYOUT_CALLS.load(Relaxed)
    );
    eprintln!(
        "vesper-hybrid: ir-check answered {} calls from Vesper",
        IR_CHECK_CALLS.load(Relaxed)
    );
    eprintln!(
        "vesper-hybrid: codegen emitted {} aggregate requests from Vesper ({} declined)",
        CODEGEN_CALLS.load(Relaxed),
        CODEGEN_DECLINED.load(Relaxed)
    );
    eprintln!(
        "vesper-hybrid: codegen emitted the export glue of {} libraries from Vesper ({} declined)",
        EXPORT_CALLS.load(Relaxed),
        EXPORT_DECLINED.load(Relaxed)
    );
    eprintln!(
        "vesper-hybrid: test-runner answered {} calls from Vesper",
        TEST_RUNNER_CALLS.load(Relaxed)
    );
    eprintln!(
        "vesper-hybrid: ir-inline answered {} calls from Vesper",
        IR_INLINE_CALLS.load(Relaxed)
    );
    eprintln!(
        "vesper-hybrid: optimize proposed for {} expressions and checked {} proposals from Vesper",
        OPTIMIZE_PROPOSE_CALLS.load(Relaxed),
        OPTIMIZE_VERIFY_CALLS.load(Relaxed)
    );
    eprintln!(
        "vesper-hybrid: place-ir round-tripped {} programs through Vesper",
        PLACE_IR_CALLS.load(Relaxed)
    );
    eprintln!(
        "vesper-hybrid: bounds-hoist hoisted {} programs in Vesper",
        BOUNDS_HOIST_CALLS.load(Relaxed)
    );
}

// Run `install` before `main` (the platform's static-constructor section).
#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd",
    target_vendor = "apple",
    windows
)))]
compile_error!(
    "vesper-hybrid installs its modules from a static constructor; add this target's section"
);

#[used]
#[cfg_attr(
    any(target_os = "linux", target_os = "android", target_os = "freebsd"),
    unsafe(link_section = ".init_array")
)]
#[cfg_attr(
    target_vendor = "apple",
    unsafe(link_section = "__DATA,__mod_init_func")
)]
#[cfg_attr(windows, unsafe(link_section = ".CRT$XCU"))]
static INSTALL: extern "C" fn() = install;

#[cfg(test)]
mod tests {
    use vesper::ast::{Arg, Expr, ExprKind, Literal, Pass, Type, TypeMap};
    use vesper::optimize::witness::WitnessLimits;

    use super::{optimize_binding, optimize_words};

    fn expr(kind: ExprKind, at: usize, len: usize) -> Expr {
        Expr {
            kind,
            span: at..at + len,
        }
    }

    fn typed(e: &Expr, t: &str, types: &mut TypeMap) {
        types.insert((e.span.start, e.span.end), Type::Named(t.into(), vec![]));
    }

    /// A split batch of several cases reads each case's names after the
    /// previous case's, so it answers as the cases do one by one.
    #[test]
    fn split_batches_keep_the_names_cursor() {
        let mut types = TypeMap::new();
        // x + 0
        let x = expr(ExprKind::Name("x".into()), 0, 1);
        let zero = expr(ExprKind::Literal(Literal::Int(0)), 4, 1);
        typed(&x, "u32", &mut types);
        typed(&zero, "u32", &mut types);
        let first = expr(
            ExprKind::Binary("+".into(), Box::new(x), Box::new(zero)),
            0,
            5,
        );
        typed(&first, "u32", &mut types);
        // wrapping_add(y, 0)
        let callee = expr(ExprKind::Name("wrapping_add".into()), 10, 12);
        let y = expr(ExprKind::Name("y".into()), 23, 1);
        let zero = expr(ExprKind::Literal(Literal::Int(0)), 26, 1);
        typed(&y, "u32", &mut types);
        typed(&zero, "u32", &mut types);
        let arg = |expr| Arg {
            pass: Pass::In,
            explicit: false,
            expr,
        };
        let second = expr(
            ExprKind::Call {
                callee: Box::new(callee),
                types: vec![],
                args: vec![arg(y), arg(zero)],
            },
            10,
            18,
        );
        typed(&second, "u32", &mut types);
        let limits = WitnessLimits::default();
        let propose = |cases: &[&Expr]| {
            let mut batch = optimize_words::Batch::new(cases.len(), true);
            for e in cases {
                batch.propose(e, &optimize_words::relevant_types(&[e], &types), limits);
            }
            optimize_words::unpack_text(&optimize_binding::propose_words(
                &batch.bytes,
                &batch.names,
            ))
        };
        let one_by_one = propose(&[&first]) + &propose(&[&second]);
        assert_eq!(propose(&[&first, &second]), one_by_one);
        assert!(!one_by_one.contains("P E "), "{one_by_one}");
    }
}
