//! The swap-in glue: each swapped module's Vesper binding, wrapped as the
//! module's Rust function table and installed before `main` runs.
//!
//! Installing from a static constructor keeps the included CLI's `main`
//! untouched (the same std start-up, the same CLI thread). The table only
//! stores function pointers, so it needs nothing from std yet.
//!
//! Set `VESPER_HYBRID_STATS=1` to print, at exit, how many calls each
//! swapped module answered from Vesper (the differential harness uses it to
//! show the pilot really ran) and what each checker swap's input and output
//! cost (`stats`; `VESPER_HYBRID_STATS=time` adds the Rust pass).
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
#[path = "../port/checks.rs"]
mod checks_binding;

#[rustfmt::skip]
#[allow(clippy::all)]
#[path = "../port/stack_bound.rs"]
mod stack_bound_binding;

#[rustfmt::skip]
#[allow(clippy::all)]
#[path = "../port/codegen.rs"]
mod codegen_binding;

#[rustfmt::skip]
#[allow(clippy::all)]
#[path = "../port/wasm_backend.rs"]
mod wasm_backend_binding;

#[rustfmt::skip]
#[allow(clippy::all)]
#[path = "../port/codegen_numeric.rs"]
mod codegen_numeric_binding;

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
#[path = "../port/ir_opt.rs"]
mod ir_opt_binding;

#[rustfmt::skip]
#[allow(clippy::all)]
#[path = "../port/place_ir.rs"]
mod place_ir_binding;

#[rustfmt::skip]
#[allow(clippy::all)]
#[path = "../port/bounds_hoist.rs"]
mod bounds_hoist_binding;

#[rustfmt::skip]
#[allow(clippy::all)]
#[path = "../port/pact.rs"]
mod pact_binding;

#[path = "parse_words.rs"]
mod parse_words;

// One shared world and word answers for the checker swaps (SH5 lane 10).
#[path = "crossing.rs"]
mod crossing;

// The export glue's interface encoder (#1286), shared with the root
// package's tests/self_host_codegen_export.rs.
#[allow(dead_code)]
#[path = "../../../tests/self_host_codegen_export/words.rs"]
mod export_words;

// The planner's inputs and answer (#1286), shared with the root package's
// tests/self_host_codegen_export.rs.
#[allow(dead_code)]
#[path = "../../../tests/self_host_codegen_export/plan_words.rs"]
mod plan_words;

// The checker swaps' input encoder and result decoders (#1177), shared with
// the root package's tests/hybrid_layout.rs and tests/hybrid_ir_check.rs.
#[path = "../../../tests/common/check_words.rs"]
mod check_words;

// The trait declaration pass's encoder and decoder (#1185), shared with
// the root package's tests/self_host_check_pact.rs.
#[path = "../../../tests/common/pact_words.rs"]
mod pact_words;

// The stack-bound swap's encoder and decoder (#1182), shared with the root
// package's tests/self_host_check_loops_atomics.rs.
#[path = "../../../tests/common/stack_words.rs"]
mod stack_words;

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
// The IR optimizer swap's encoder and decoder (#1248), shared with the root
// package's tests/self_host_ir_opt.rs.
#[path = "../../../tests/common/ir_opt_words.rs"]
mod ir_opt_words;

// Place IR words both ways (#1240), shared with tests/self_host_place_ir*.rs.
#[path = "../../../tests/common/ir_words.rs"]
mod ir_words;

// The wasm backend swap's input encoder (#1289): the Place IR words above
// and the wasm section, shared with tests/self_host_wasm_backend.rs.
#[path = "../../../tests/common/wasm_words.rs"]
mod wasm_words;

/// Calls answered by the Vesper word-layout module.
static LAYOUT_CALLS: AtomicU64 = AtomicU64::new(0);
/// Sources parsed by the Vesper parse module.
static PARSE_CALLS: AtomicU64 = AtomicU64::new(0);
/// `Layout::of` calls answered by the Vesper check-layout module.
static CHECK_LAYOUT_CALLS: AtomicU64 = AtomicU64::new(0);
/// Programs checked by the Vesper IR-check module.
static IR_CHECK_CALLS: AtomicU64 = AtomicU64::new(0);
/// Reports made by the Vesper stack-bound module.
static STACK_BOUND_CALLS: AtomicU64 = AtomicU64::new(0);
/// `aggregate` requests the Vesper codegen module emitted, and declined.
static CODEGEN_CALLS: AtomicU64 = AtomicU64::new(0);
static CODEGEN_DECLINED: AtomicU64 = AtomicU64::new(0);
/// Programs the Vesper wasm backend compiled, and left to Rust.
static WASM_CALLS: AtomicU64 = AtomicU64::new(0);
static WASM_DECLINED: AtomicU64 = AtomicU64::new(0);
/// `numeric` requests (vectors, `TiledSeq`) the Vesper module emitted, and
/// declined.
static NUMERIC_CALLS: AtomicU64 = AtomicU64::new(0);
static NUMERIC_DECLINED: AtomicU64 = AtomicU64::new(0);
/// Libraries whose v2 glue the Vesper export module emitted, and declined.
static EXPORT_CALLS: AtomicU64 = AtomicU64::new(0);
static EXPORT_DECLINED: AtomicU64 = AtomicU64::new(0);
/// Library interfaces the Vesper planner planned.
static PLAN_CALLS: AtomicU64 = AtomicU64::new(0);
/// Interface files the Vesper writer wrote.
static JSON_CALLS: AtomicU64 = AtomicU64::new(0);
/// Sources whose tests the Vesper test-runner module discovered.
static TEST_RUNNER_CALLS: AtomicU64 = AtomicU64::new(0);
/// Programs inlined by the Vesper inliner module.
static IR_INLINE_CALLS: AtomicU64 = AtomicU64::new(0);
/// Expressions the Vesper optimizer proposed for, and proposals it checked.
static OPTIMIZE_PROPOSE_CALLS: AtomicU64 = AtomicU64::new(0);
static OPTIMIZE_VERIFY_CALLS: AtomicU64 = AtomicU64::new(0);
/// Programs optimized by the Vesper IR optimizer module.
static IR_OPT_CALLS: AtomicU64 = AtomicU64::new(0);
/// Packages whose trait declarations the Vesper declaration pass checked.
static PACT_CALLS: AtomicU64 = AtomicU64::new(0);
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
        // The source is lent in place: the parse swap has no input to
        // encode, only its AST words to decode.
        let timer = stats::Timer::start();
        let words = parse_binding::parse_words(source);
        let timer = timer.lap(&stats::PARSE.vesper);
        let parsed = parse_words::decode(source, &words);
        timer.lap(&stats::PARSE.decode);
        stats::PARSE.call(
            false,
            || vesper_syntax::parser::rust_parse(source),
            |rust| *rust == parsed,
        );
        parsed
    },
};

/// One compilation's swap inputs (SH4 lane 2, #1276), kept in its
/// `vesper::swap_input` scope: the world's encoding, built once, and each
/// Vesper module's decoded copy of it with what that copy already holds.
struct Inputs {
    log: check_words::WorldLog,
    layout: Option<(check_words::Cursor, check_layout_binding::LayoutInput)>,
    ir: Option<(check_words::Cursor, ir_check_binding::IrInput)>,
    /// Every layout the Vesper module answered in this compilation, by
    /// type: `Layout::of` depends only on the world and the type, so a
    /// repeated type reuses the answer instead of crossing again.
    layouts: LayoutMemo,
    /// The one world both swaps share (SH5 lane 10, #1333): `modules/checks`
    /// holds the layout and the IR check in one Vesper library.
    shared: SharedState,
}

/// The shared input of one compilation, opened on first use.
enum SharedState {
    /// Not opened yet, or off (`VESPER_HYBRID_SHARED=0`).
    Unopened,
    Open(Box<crossing::Shared>),
    /// An ADT cannot be encoded: each swap keeps its own input, as before.
    Refused,
}

/// Whether the swaps share one world (on unless `VESPER_HYBRID_SHARED=0`,
/// the before side of the SH5 lane 10 measurement).
fn shared_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| !std::env::var_os("VESPER_HYBRID_SHARED").is_some_and(|v| v == "0"))
}

impl Inputs {
    /// The shared input, opened the first time; `None` when it is off or
    /// refused.
    fn shared(&mut self, world: &vesper::check::World) -> Option<&mut crossing::Shared> {
        if !shared_on() {
            return None;
        }
        if matches!(self.shared, SharedState::Unopened) {
            self.shared = match crossing::Shared::open(&mut self.log, world) {
                Some(s) => SharedState::Open(Box::new(s)),
                None => SharedState::Refused,
            };
        }
        match &mut self.shared {
            SharedState::Open(s) => Some(s),
            _ => None,
        }
    }
}

type LayoutMemo = std::collections::HashMap<
    u64,
    Vec<(vesper::ast::Type, Result<vesper::layout::Layout, String>)>,
    std::hash::BuildHasherDefault<check_words::Fx>,
>;

impl Inputs {
    fn new(world: &vesper::check::World) -> Self {
        Self {
            log: check_words::WorldLog::new(world),
            layout: None,
            ir: None,
            layouts: LayoutMemo::default(),
            shared: SharedState::Unopened,
        }
    }
}

/// `vesper::layout::LayoutTable` from `modules/check_layout`: the layout of
/// `@vesper/check-layout`. Inside a compilation's scope the world crosses
/// once (each call sends only the ADTs and types the Vesper copy lacks);
/// outside one, each call encodes the ADTs the type can reach. A type
/// whose literals the decoder cannot read (over 64 bits) stays in Rust.
const VESPER_LAYOUT: vesper::layout::LayoutTable = vesper::layout::LayoutTable {
    of: |world, ty| {
        let timer = stats::Timer::start();
        let cached = vesper::swap_input::with(
            world,
            || Inputs::new(world),
            |inputs| {
                let key = check_words::type_hash(ty);
                if let Some((_, layout)) = inputs
                    .layouts
                    .get(&key)
                    .and_then(|b| b.iter().find(|(t, _)| t == ty))
                {
                    timer.lap(&stats::LAYOUT.encode);
                    return Some((layout.clone(), true));
                }
                if inputs.shared(world).is_some() {
                    let Inputs {
                        log,
                        shared: SharedState::Open(sh),
                        ..
                    } = inputs
                    else {
                        unreachable!("opened above")
                    };
                    let part = log.layout_part(world, &mut sh.cur, ty)?;
                    let timer = timer.lap(&stats::LAYOUT.encode);
                    sh.checks.layout_of(&part);
                    let timer = timer.lap(&stats::LAYOUT.vesper);
                    stats::LAYOUT.shared.fetch_add(1, Relaxed);
                    let layout = crossing::decode_layout(sh.checks.as_ref().out(), log);
                    timer.lap(&stats::LAYOUT.decode);
                    inputs
                        .layouts
                        .entry(key)
                        .or_default()
                        .push((ty.clone(), layout.clone()));
                    return Some((layout, false));
                }
                let (cur, input) = inputs.layout.get_or_insert_with(|| {
                    (
                        check_words::Cursor::default(),
                        check_layout_binding::LayoutInput::new(),
                    )
                });
                let part = inputs.log.layout_part(world, cur, ty)?;
                let timer = timer.lap(&stats::LAYOUT.encode);
                let words = input.layout_of(&part);
                let timer = timer.lap(&stats::LAYOUT.vesper);
                let layout = check_words::decode_layout(&words);
                timer.lap(&stats::LAYOUT.decode);
                inputs
                    .layouts
                    .entry(key)
                    .or_default()
                    .push((ty.clone(), layout.clone()));
                Some((layout, false))
            },
        );
        let (layout, scoped, reused) = match cached {
            Some(Some((layout, reused))) => (layout, true, reused),
            Some(None) => return vesper::layout::rust_layout_of(world, ty),
            None => {
                let Some(input) = check_words::layout_input(world, ty) else {
                    return vesper::layout::rust_layout_of(world, ty);
                };
                let timer = timer.lap(&stats::LAYOUT.encode);
                let words = check_layout_binding::layout_of_words(&input);
                let timer = timer.lap(&stats::LAYOUT.vesper);
                let layout = check_words::decode_layout(&words);
                timer.lap(&stats::LAYOUT.decode);
                (layout, false, false)
            }
        };
        CHECK_LAYOUT_CALLS.fetch_add(1, Relaxed);
        if reused {
            stats::LAYOUT.reused.fetch_add(1, Relaxed);
        }
        stats::LAYOUT.call(
            scoped,
            || vesper::layout::rust_layout_of(world, ty),
            |rust| format!("{rust:?}") == format!("{layout:?}"),
        );
        layout
    },
};

/// `vesper::ir_check::IrCheckTable` from `modules/ir_check`: the IR check
/// of `@vesper/ir-check`, over the world without its expression types and
/// span tables (which it does not read). Inside a compilation's scope the
/// world crosses once. A program whose literals the decoder cannot read
/// stays in Rust.
const VESPER_IR_CHECK: vesper::ir_check::IrCheckTable = vesper::ir_check::IrCheckTable {
    validate: |world, program| {
        let timer = stats::Timer::start();
        let cached = vesper::swap_input::with(
            world,
            || Inputs::new(world),
            |inputs| {
                if inputs.shared(world).is_some() {
                    let Inputs {
                        log,
                        shared: SharedState::Open(sh),
                        ..
                    } = inputs
                    else {
                        unreachable!("opened above")
                    };
                    let part = log.ir_part(world, &mut sh.cur, program)?;
                    let timer = timer.lap(&stats::IR_CHECK.encode);
                    let words = sh.checks.validate(&part);
                    stats::IR_CHECK.shared.fetch_add(1, Relaxed);
                    return Some((timer.lap(&stats::IR_CHECK.vesper), words));
                }
                let (cur, input) = inputs.ir.get_or_insert_with(|| {
                    (
                        check_words::Cursor::default(),
                        ir_check_binding::IrInput::new(),
                    )
                });
                let part = inputs.log.ir_part(world, cur, program)?;
                let timer = timer.lap(&stats::IR_CHECK.encode);
                let words = input.validate(&part);
                Some((timer.lap(&stats::IR_CHECK.vesper), words))
            },
        );
        let (timer, words, scoped) = match cached {
            Some(Some((timer, words))) => (timer, words, true),
            Some(None) => return vesper::ir_check::rust_validate(world, program),
            None => {
                // `ir_input` (and `ir_part` above) refuse what
                // `@vesper/ir-check` does not cover yet, among them every
                // world with element view matches (#1221): those go to
                // `rust_validate` (test: `element_views_stay_on_rust` in
                // tests/hybrid_ir_check.rs).
                let Some(input) = check_words::ir_input(world, program) else {
                    return vesper::ir_check::rust_validate(world, program);
                };
                let timer = timer.lap(&stats::IR_CHECK.encode);
                let words = ir_check_binding::validate_words(&input);
                (timer.lap(&stats::IR_CHECK.vesper), words, false)
            }
        };
        IR_CHECK_CALLS.fetch_add(1, Relaxed);
        let diagnostics = check_words::decode_diagnostics(&words);
        timer.lap(&stats::IR_CHECK.decode);
        stats::IR_CHECK.call(
            scoped,
            || vesper::ir_check::rust_validate(world, program),
            |rust| *rust == diagnostics,
        );
        diagnostics
    },
};

/// `vesper::stack_bound::StackBoundTable` from `modules/stack_bound`: the
/// stack-bound report of `@vesper/check-loops-atomics` (`vesper ebpf check`
/// and `--verify-stack`).
const VESPER_STACK_BOUND: vesper::stack_bound::StackBoundTable =
    vesper::stack_bound::StackBoundTable {
        report: |graph, policy| {
            STACK_BOUND_CALLS.fetch_add(1, Relaxed);
            let input = stack_words::stack_input(graph, policy);
            stack_words::decode_stack_report(
                graph.origin,
                &stack_bound_binding::report_words(&input),
            )
        },
    };

/// `vesper::pact_frontend::DeclarationPassTable` from `modules/pact`: the
/// declaration pass of `@vesper/check-pact` (collecting and validating the
/// trait, conformance, `use model` and bound declarations of a package
/// without dependencies). A module whose literals the decoder cannot read
/// stays in Rust; a clean answer leaves the Pact session to Rust.
const VESPER_DECLARATION_PASS: vesper::pact_frontend::DeclarationPassTable =
    vesper::pact_frontend::DeclarationPassTable {
        diagnostics: |module, base_world, identity| {
            let timer = stats::Timer::start();
            let input = pact_words::declaration_input(module, base_world, &identity.to_string())?;
            let timer = timer.lap(&stats::PACT.encode);
            let words = pact_binding::pass_words(&input);
            let timer = timer.lap(&stats::PACT.vesper);
            let diagnostics = pact_words::decode_declarations(&words);
            timer.lap(&stats::PACT.decode);
            PACT_CALLS.fetch_add(1, Relaxed);
            stats::PACT.call(
                false,
                || {
                    vesper::pact_frontend::rust_declaration_diagnostics(
                        module, base_world, identity,
                    )
                },
                |rust| rust.as_ref() == Some(&diagnostics),
            );
            Some(diagnostics)
        },
    };

/// The swaps' input and output costs (SH4 lane 2, #1276), reported at exit
/// with `VESPER_HYBRID_STATS=1`. `VESPER_HYBRID_STATS=time` also runs the
/// Rust pass on every swapped call, times it and counts the calls whose
/// answers differ, so one hybrid run gives each swap's encode, Vesper and
/// decode time beside the Rust pass it replaces.
mod stats {
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
    use std::time::Instant;

    /// 0: off, 1: costs, 2: costs and the Rust pass.
    pub fn level() -> u8 {
        static LEVEL: OnceLock<u8> = OnceLock::new();
        *LEVEL.get_or_init(|| match std::env::var("VESPER_HYBRID_STATS").as_deref() {
            Ok("1") => 1,
            Ok("time") => 2,
            _ => 0,
        })
    }

    pub struct Cost {
        pub name: &'static str,
        pub calls: AtomicU64,
        pub scoped: AtomicU64,
        pub reused: AtomicU64,
        /// Calls that crossed into the shared world (SH5 lane 10).
        pub shared: AtomicU64,
        pub encode: AtomicU64,
        pub vesper: AtomicU64,
        pub decode: AtomicU64,
        pub rust: AtomicU64,
        pub differ: AtomicU64,
    }

    impl Cost {
        const fn new(name: &'static str) -> Self {
            Self {
                name,
                calls: AtomicU64::new(0),
                scoped: AtomicU64::new(0),
                reused: AtomicU64::new(0),
                shared: AtomicU64::new(0),
                encode: AtomicU64::new(0),
                vesper: AtomicU64::new(0),
                decode: AtomicU64::new(0),
                rust: AtomicU64::new(0),
                differ: AtomicU64::new(0),
            }
        }

        /// Counts one swapped call; at level 2 also times the Rust pass
        /// (`rust`) and counts the call if `same` says its answer differs
        /// from the swap's.
        pub fn call<T>(
            &self,
            scoped: bool,
            rust: impl FnOnce() -> T,
            same: impl FnOnce(&T) -> bool,
        ) {
            if level() == 0 {
                return;
            }
            self.calls.fetch_add(1, Relaxed);
            self.scoped.fetch_add(scoped as u64, Relaxed);
            if level() == 2 {
                let t = Instant::now();
                let answer = std::hint::black_box(rust());
                self.rust.fetch_add(t.elapsed().as_nanos() as u64, Relaxed);
                self.differ.fetch_add(!same(&answer) as u64, Relaxed);
            }
        }

        pub fn report(&self) {
            let ms = |a: &AtomicU64| a.load(Relaxed) as f64 / 1e6;
            eprintln!(
                "vesper-hybrid: cost {}: {} calls ({} in a compilation scope), encode {:.3} ms, \
                 vesper {:.3} ms, decode {:.3} ms, rust {:.3} ms, {} differ, {} reused, \
                 {} shared",
                self.name,
                self.calls.load(Relaxed),
                self.scoped.load(Relaxed),
                ms(&self.encode),
                ms(&self.vesper),
                ms(&self.decode),
                ms(&self.rust),
                self.differ.load(Relaxed),
                self.reused.load(Relaxed),
                self.shared.load(Relaxed),
            );
        }
    }

    pub static PARSE: Cost = Cost::new("parse");
    pub static LAYOUT: Cost = Cost::new("check-layout");
    pub static IR_CHECK: Cost = Cost::new("ir-check");
    pub static PACT: Cost = Cost::new("pact");

    /// A stopwatch that only reads the clock when stats are on.
    #[derive(Clone, Copy)]
    pub struct Timer(Option<Instant>);

    impl Timer {
        pub fn start() -> Self {
            Self((level() > 0).then(Instant::now))
        }

        /// Adds the time since the last lap to `into`.
        pub fn lap(self, into: &AtomicU64) -> Self {
            match self.0 {
                Some(t) => {
                    let now = Instant::now();
                    into.fetch_add((now - t).as_nanos() as u64, Relaxed);
                    Self(Some(now))
                }
                None => self,
            }
        }
    }
}

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

/// The `wasm` hook of `vesper::codegen_swap` from `modules/wasm_backend`:
/// `@vesper/wasm-backend` writes the core module and the WIT, and
/// wit-component wraps them here. Errors (so the messages stay Rust's) and
/// programs with handlers or host capabilities (not ported yet) decline to
/// the Rust backend.
fn vesper_wasm(
    world: &vesper::check::World,
    entry: &str,
    options: &vesper::wasm_backend::WasmOptions,
) -> Option<Result<vesper::wasm_backend::WasmArtifact, vesper::wasm_backend::WasmError>> {
    match wasm_words::compile_with(world, entry, options, wasm_backend_binding::compile_words) {
        Some(artifact) => {
            WASM_CALLS.fetch_add(1, Relaxed);
            Some(Ok(artifact))
        }
        None => {
            WASM_DECLINED.fetch_add(1, Relaxed);
            None
        }
    }
}

/// The `numeric` hook of `vesper::codegen_swap` from
/// `modules/codegen_numeric` (#1285): vector and `TiledSeq` operations
/// emitted in Vesper. The graph walk's `INSTRUCTION` requests are declined
/// here, without crossing: the Rust family code then sends the typed
/// requests of `codegen_swap::numeric`, which the Vesper hook answers.
fn vesper_numeric(lent: &mut vesper::codegen_swap::Lent<'_, '_>) -> Result<bool, String> {
    if lent.op == vesper::codegen_swap::INSTRUCTION {
        return Ok(false);
    }
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
        codegen_numeric_binding::numeric,
    );
    if let Some(e) = crossing.error {
        return Err(format!("codegen (numeric op {}): {e}", lent.op));
    }
    if crossing.unreturned {
        return Err(format!(
            "codegen (numeric op {}) kept a lent handle",
            lent.op
        ));
    }
    if answer == 1 {
        NUMERIC_CALLS.fetch_add(1, Relaxed);
        lent.results.extend(crossing.results);
        Ok(true)
    } else {
        NUMERIC_DECLINED.fetch_add(1, Relaxed);
        Ok(false)
    }
}

/// The `plan` hook of `vesper::codegen_swap` from `modules/codegen_export`
/// (#1286): `plan_with` in Vesper. The checker's answers about each type
/// travel with the request (plan_words.rs `plan_input`).
fn vesper_plan(
    world: &vesper::check::World,
    public: &std::collections::BTreeSet<String>,
    library: &str,
    directives: &vesper::native::export::Directives,
) -> Option<
    Result<vesper::native::export::LibraryInterface, Vec<vesper::native::export::ExportError>>,
> {
    let input = plan_words::plan_input(world, public, library, directives);
    PLAN_CALLS.fetch_add(1, Relaxed);
    Some(plan_words::decode_plan(&codegen_export_binding::plan(
        &input,
    )))
}

/// The `interface_json` hook of `vesper::codegen_swap` from
/// `modules/codegen_export` (#1286): the interface file in Vesper, from the
/// interface in the binding generator's words.
fn vesper_interface_json(interface: &vesper::native::export::LibraryInterface) -> Option<String> {
    let input = export_words::binding_input(interface, &Default::default());
    let text = String::from_utf8(codegen_export_binding::interface_file(&input)).ok()?;
    JSON_CALLS.fetch_add(1, Relaxed);
    Some(text)
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

/// `ir_swap`'s `opt` hook from `modules/ir_opt`: `ir_opt::optimize` of
/// `@vesper/ir-opt` (memory forwarding and `@fip` reuse), then Rust's
/// token threading and IR check on the decoded program
/// (`ir_opt_words::finish`). A program the Place IR words cannot encode,
/// or a block egglog would refuse (the message is egglog's own), stays in
/// Rust.
fn vesper_optimize(
    world: &vesper::check::World,
    program: &mut vesper::ir::Program,
) -> Result<usize, String> {
    let ours = ir_opt_words::optimize_with(world, program, ir_opt_binding::opt_words);
    match ours {
        Some(result) => {
            IR_OPT_CALLS.fetch_add(1, Relaxed);
            result
        }
        None => vesper::ir_opt::optimize(world, program),
    }
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
    // SH3 lane 9 (#1248): the IR optimizer of `@vesper/ir-opt`.
    opt: Some(|world, program| {
        let n = vesper_optimize(world, program)?;
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
    if vesper::stack_bound::install_stack_bound(VESPER_STACK_BOUND).is_err() {
        std::process::abort();
    }
    // Swaps keep one input per compilation (SH4 lane 2, #1276).
    // `VESPER_HYBRID_SCOPES=0` encodes per call instead (the before side of
    // the report's measurement).
    if !std::env::var_os("VESPER_HYBRID_SCOPES").is_some_and(|v| v == "0") {
        vesper::swap_input::enable();
    }
    let hooks = vesper::codegen_swap::CodegenHooks {
        aggregate: Some(vesper_aggregate),
        wasm: Some(vesper_wasm),
        numeric: Some(vesper_numeric),
        export: Some(vesper_export),
        plan: Some(vesper_plan),
        interface_json: Some(vesper_interface_json),
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
    if vesper::pact_frontend::install_declaration_pass(VESPER_DECLARATION_PASS).is_err() {
        std::process::abort();
    }
    if std::env::var_os("VESPER_HYBRID_STATS").is_some_and(|v| v == "1" || v == "time") {
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
    stats::PARSE.report();
    stats::LAYOUT.report();
    stats::IR_CHECK.report();
    eprintln!(
        "vesper-hybrid: pact answered {} calls from Vesper",
        PACT_CALLS.load(Relaxed)
    );
    stats::PACT.report();
    eprintln!(
        "vesper-hybrid: stack-bound answered {} calls from Vesper",
        STACK_BOUND_CALLS.load(Relaxed)
    );
    eprintln!(
        "vesper-hybrid: codegen emitted {} aggregate requests from Vesper ({} declined)",
        CODEGEN_CALLS.load(Relaxed),
        CODEGEN_DECLINED.load(Relaxed)
    );
    eprintln!(
        "vesper-hybrid: wasm backend compiled {} programs from Vesper ({} declined)",
        WASM_CALLS.load(Relaxed),
        WASM_DECLINED.load(Relaxed)
    );
    eprintln!(
        "vesper-hybrid: codegen emitted {} numeric requests from Vesper ({} declined)",
        NUMERIC_CALLS.load(Relaxed),
        NUMERIC_DECLINED.load(Relaxed)
    );
    eprintln!(
        "vesper-hybrid: codegen emitted the export glue of {} libraries from Vesper ({} declined)",
        EXPORT_CALLS.load(Relaxed),
        EXPORT_DECLINED.load(Relaxed)
    );
    eprintln!(
        "vesper-hybrid: codegen planned {} library interfaces from Vesper",
        PLAN_CALLS.load(Relaxed)
    );
    eprintln!(
        "vesper-hybrid: codegen wrote {} interface files from Vesper",
        JSON_CALLS.load(Relaxed)
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
        "vesper-hybrid: ir-opt answered {} calls from Vesper",
        IR_OPT_CALLS.load(Relaxed)
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
