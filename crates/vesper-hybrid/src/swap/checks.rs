// The checker swaps (#1177, SH5 lane 10 #1333): the whole layout of
// `@vesper/check-layout` (`modules/check_layout`) and the IR check of
// `@vesper/ir-check` (`modules/ir_check`), over one shared world
// (`modules/checks`, `crossing`) when it can be encoded.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

#[rustfmt::skip]
#[allow(clippy::all)]
#[path = "../../port/check_layout.rs"]
mod check_layout_binding;

#[rustfmt::skip]
#[allow(clippy::all)]
#[path = "../../port/ir_check.rs"]
mod ir_check_binding;

#[rustfmt::skip]
#[allow(clippy::all)]
#[path = "../../port/checks.rs"]
mod checks_binding;

// One shared world and word answers for the checker swaps (SH5 lane 10).
#[path = "../crossing.rs"]
mod crossing;

use super::{Registry, Swap, check_words, must, on, open, stats};

/// `Layout::of` calls answered by the Vesper check-layout module.
static CHECK_LAYOUT_CALLS: AtomicU64 = AtomicU64::new(0);
/// Programs checked by the Vesper IR-check module.
static IR_CHECK_CALLS: AtomicU64 = AtomicU64::new(0);
/// The checker swaps' costs.
static LAYOUT_COST: stats::Cost = stats::Cost::new("check-layout");
static IR_CHECK_COST: stats::Cost = stats::Cost::new("ir-check");

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
                    timer.lap(&LAYOUT_COST.encode);
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
                    let timer = timer.lap(&LAYOUT_COST.encode);
                    sh.checks.layout_of(&part);
                    let timer = timer.lap(&LAYOUT_COST.vesper);
                    LAYOUT_COST.shared.fetch_add(1, Relaxed);
                    let layout = crossing::decode_layout(sh.checks.as_ref().out(), log);
                    timer.lap(&LAYOUT_COST.decode);
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
                let timer = timer.lap(&LAYOUT_COST.encode);
                let words = input.layout_of(&part);
                let timer = timer.lap(&LAYOUT_COST.vesper);
                let layout = check_words::decode_layout(&words);
                timer.lap(&LAYOUT_COST.decode);
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
                let timer = timer.lap(&LAYOUT_COST.encode);
                let words = check_layout_binding::layout_of_words(&input);
                let timer = timer.lap(&LAYOUT_COST.vesper);
                let layout = check_words::decode_layout(&words);
                timer.lap(&LAYOUT_COST.decode);
                (layout, false, false)
            }
        };
        CHECK_LAYOUT_CALLS.fetch_add(1, Relaxed);
        if reused {
            LAYOUT_COST.reused.fetch_add(1, Relaxed);
        }
        LAYOUT_COST.call(
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
                    let timer = timer.lap(&IR_CHECK_COST.encode);
                    let words = sh.checks.validate(&part);
                    IR_CHECK_COST.shared.fetch_add(1, Relaxed);
                    return Some((timer.lap(&IR_CHECK_COST.vesper), words));
                }
                let (cur, input) = inputs.ir.get_or_insert_with(|| {
                    (
                        check_words::Cursor::default(),
                        ir_check_binding::IrInput::new(),
                    )
                });
                let part = inputs.log.ir_part(world, cur, program)?;
                let timer = timer.lap(&IR_CHECK_COST.encode);
                let words = input.validate(&part);
                Some((timer.lap(&IR_CHECK_COST.vesper), words))
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
                let timer = timer.lap(&IR_CHECK_COST.encode);
                let words = ir_check_binding::validate_words(&input);
                (timer.lap(&IR_CHECK_COST.vesper), words, false)
            }
        };
        IR_CHECK_CALLS.fetch_add(1, Relaxed);
        let diagnostics = check_words::decode_diagnostics(&words);
        timer.lap(&IR_CHECK_COST.decode);
        IR_CHECK_COST.call(
            scoped,
            || vesper::ir_check::rust_validate(world, program),
            |rust| *rust == diagnostics,
        );
        diagnostics
    },
};

/// The flip-gate rows of this file's swaps (`swaps::on`).
pub(super) const SWAPS: &[Swap] = &[
    open("layout", "0 diffs; no flip-gate configuration"),
    open(
        "ir_check",
        "flip gate FAIL 1.71x with encoding (scripts/ci/flip-gate/ir-check.json, #2289)",
    ),
];

pub(super) fn register(_r: &mut Registry) {
    if on("layout") {
        must(vesper::layout::install_layout(VESPER_LAYOUT));
    }
    if on("ir_check") {
        must(vesper::ir_check::install_ir_check(VESPER_IR_CHECK));
    }
}

pub(super) fn report() {
    eprintln!(
        "vesper-hybrid: check-layout answered {} calls from Vesper",
        CHECK_LAYOUT_CALLS.load(Relaxed),
    );
    eprintln!(
        "vesper-hybrid: ir-check answered {} calls from Vesper",
        IR_CHECK_CALLS.load(Relaxed),
    );
    LAYOUT_COST.report();
    IR_CHECK_COST.report();
}
