//! Lent walk state and re-entry into Rust (self-hosting SH5 lane 1, #1324).
//!
//! SH4's family hooks get the builder and module only, so they can emit
//! leaf instructions and nothing that needs the graph walk's own state:
//! which frontend variable a local is, whether it lives in a register or a
//! home slot, a split local's words, the plans the walk made for an
//! instruction (hoisted reads, homes, drops), the value infos the Rust
//! bookkeeping keeps, or a sub-expression the hook cannot lower yet. This
//! module lends that state for one [`INSTRUCTION`](super::INSTRUCTION)
//! request at the graph walk (`Codegen::ir_graph_at`).
//!
//! # The protocol
//!
//! While a hook answers an instruction request, three calls reach the walk
//! (as Rust functions here, and as the C-ABI symbols [`vesper_walk_query`],
//! [`vesper_walk_effect`] and [`vesper_walk_reenter`] that
//! `@vesper/codegen-walk`, packages/vesper-codegen-base/walk, imports):
//!
//! - **Queries** ([`query`]) read the walk state: facts about the
//!   instruction (its kind, its place's root variable and path, its pass,
//!   its type flags, the walk's plans for it) and the walk's maps (a
//!   variable's home slot, whether it is a narrow element view or a
//!   register local, a split local's words, the frontend variable of an
//!   operand). A query outside a request, or about something the walk does
//!   not have, answers [`NONE`].
//! - **Effects** ([`effect`]) are the Rust bookkeeping of what the hook
//!   emitted: `Codegen::use_var`, `def_var`, `store` and `jump` keep value
//!   infos (closure targets, field infos) that later Rust lowering reads,
//!   and `ir_temp` names the temporaries a later read finds. The hook emits
//!   the instructions itself and records the effects; the walk applies them
//!   in order when the hook returns, before anything else is emitted. A
//!   hook that emits what `Codegen::use_var` emits records [`effect::USED`],
//!   and so on: each effect's documentation names the Rust helper it
//!   stands for.
//! - **Re-entry** ([`reentry`]): the hook asks Rust to emit a piece of the
//!   instruction and hand back its value, by calling `reenter(kind)` and
//!   then *declining*. The walk emits the piece, appends its value to the
//!   request's kept words, and asks the hook again with the same request.
//!   The kept words survive across those calls ([`effect::KEEP`] adds the
//!   hook's own, [`query::KEPT_COUNT`] and [`query::KEPT`] read them), so a
//!   hook resumes where it stopped: it emits a prefix, keeps what it needs,
//!   re-enters, and on the next call skips the prefix. Re-entry is
//!   continuation-passing because the builder is the hook's while it runs:
//!   Rust can emit only between two calls.
//!
//!   [`reentry::INSTRUCTION`] runs the whole Rust lowering of the
//!   instruction and resumes the hook with its value (the hook then
//!   answers, usually with that value); [`reentry::PLACE_VALUE`] and
//!   [`reentry::PLACE_ADDRESS`] emit the instruction's place (a `Read`'s
//!   root and path, a `Write`'s destination) as `Codegen::expr` and
//!   `Codegen::address` do.
//!
//! A hook that declines without re-entering must not have emitted anything
//! (as before). The words are the ones the rest of `codegen_swap` uses:
//! Cranelift entity numbers (values, blocks, variables), type codes and
//! immediates.
//!
//! **Nothing here runs in the default compiler**: the walk builds its facts
//! only when a hook of the instruction's family is in effect ([`wanted`]),
//! so with no hook installed the objects are byte-identical.
use super::{Family, Site, request};
use crate::check::World;
use crate::ir::{Op, Program};
use cranelift_codegen::ir;
use cranelift_frontend::FunctionBuilder;
use cranelift_module::Module;
use std::cell::{Cell, RefCell};

/// The answer to a query that has none.
pub const NONE: u64 = u64::MAX;

/// Queries: `query(q, a, b)`. Facts are about the instruction being
/// emitted; map queries take a variable (or an index) in `a`.
pub mod query {
    /// The instruction's kind ([`super::kind`]).
    pub const KIND: u32 = 1;
    /// The frontend variable of the instruction's place root: a `Read`'s
    /// root, a `Write`'s destination root, a `Bind` of one name. `NONE`
    /// when the root is not a local of the walk (a function, a pattern).
    pub const PLACE_VAR: u32 = 2;
    /// How many fields the place's path has (0 for a `Bind` of a name).
    pub const PATH_LEN: u32 = 3;
    /// A `Read`'s pass: 0 `in`, 1 `inout`, 2 `sink`.
    pub const PASS: u32 = 4;
    /// 1 when a `Write` is a plain `=` (else a compound assignment).
    pub const ASSIGN: u32 = 5;
    /// The instruction's type flags ([`super::flags`]).
    pub const FLAGS: u32 = 6;
    /// The walk's plans for the instruction ([`super::plan`]).
    pub const PLAN: u32 = 7;
    /// The frontend variable that holds operand `a` (the walk's register
    /// of that IR value).
    pub const OPERAND_VAR: u32 = 8;
    /// 1 when the walk emits the optimize path's lowerings
    /// (`Codegen::operands_only`: register locals, split locals, homes).
    pub const OPTIMIZE: u32 = 9;
    /// The variable holding the address of variable `a`'s home slot, when
    /// `a` lives in memory (`Codegen::pointers`).
    pub const HOME: u32 = 10;
    /// 1 when variable `a` is bound to a narrow element view
    /// (`Codegen::narrow_homes`): every read loads the element, every
    /// write stores it, at its width.
    pub const NARROW: u32 = 11;
    /// 1 when variable `a` is a scalar local kept in a register on the
    /// optimize path (`Codegen::register_locals`).
    pub const REGISTER: u32 = 12;
    /// How many words split local `a` has (0 when it is not split).
    pub const SPLIT_WORDS: u32 = 13;
    /// The variable of word `b` of split local `a`.
    pub const SPLIT_WORD: u32 = 14;
    /// How many words the request has kept (its own and re-entries').
    pub const KEPT_COUNT: u32 = 20;
    /// Kept word `a`.
    pub const KEPT: u32 = 21;
}

/// [`query::KIND`]'s answers.
pub mod kind {
    pub const OTHER: u64 = 0;
    pub const READ: u64 = 1;
    pub const BIND: u64 = 2;
    pub const WRITE: u64 = 3;
}

/// [`query::FLAGS`]'s bits.
pub mod flags {
    /// The instruction's own type (a `Read`'s result, a `Write`'s or
    /// `Bind`'s value) has an aggregate layout.
    pub const RESULT_AGGREGATE: u64 = 1;
    /// The instruction's own type is a function type.
    pub const RESULT_FUNCTION: u64 = 2;
    /// The place's type, as `Codegen::ty` reads the place expression, has
    /// an aggregate layout.
    pub const PLACE_AGGREGATE: u64 = 4;
    /// The place's type is a function type.
    pub const PLACE_FUNCTION: u64 = 8;
    /// The place's type is a narrow or 128-bit integer kind: a read of it
    /// as an expression, and a store to it, normalize the value
    /// (`Codegen::coerce`).
    pub const PLACE_COERCES: u64 = 16;
    /// The place root names a constructor (`Codegen::expr` builds it).
    pub const PLACE_CONSTRUCTOR: u64 = 32;
    /// Reading the instruction's value back as an expression normalizes
    /// it: a `Write`'s value type, as `Codegen::ir_temp` names its
    /// temporary, is a narrow or 128-bit integer kind (`Codegen::expr`
    /// ends in `coerce`).
    pub const RESULT_COERCES: u64 = 64;
    /// The type the checker gave the instruction's span is a narrow or
    /// 128-bit integer kind: `Codegen::expr` of the `Write`'s assignment
    /// normalizes its zero.
    pub const STATEMENT_COERCES: u64 = 128;
}

/// [`query::PLAN`]'s bits: what the walk planned for the instruction.
pub mod plan {
    /// A `Read` whose sequence header was read before its loop.
    pub const HOISTED: u64 = 1;
    /// The place root is a split local (`aggregates::Split`).
    pub const SPLIT: u64 = 2;
    /// A `Read` of a pass-through local that yields its `let`'s storage.
    pub const HOMED: u64 = 4;
    /// A `Read` copied straight into a split local.
    pub const SPLIT_SOURCE: u64 = 8;
    /// An array read indexed in place, or an array element write.
    pub const ARRAY: u64 = 16;
    /// A `Bind` whose value the read already bound.
    pub const BOUND_EARLY: u64 = 32;
    /// A `Write` whose value was built in the destination.
    pub const PLACED: u64 = 64;
    /// The place root (a `Write`'s whole place) has a sequence or string
    /// drop slot.
    pub const DROP_SLOT: u64 = 128;
    /// A `Write` that frees what it replaces.
    pub const REPLACES: u64 = 256;
    /// A `Bind` of a fused `try_get`.
    pub const FUSED: u64 = 512;
    /// The world has element views (a `Bind` may have to refuse one).
    pub const ELEMENT_VIEWS: u64 = 1024;
}

/// Effects: `effect(e, a, b, c)`, applied in order when the hook returns.
pub mod effect {
    /// `Codegen::use_var`'s bookkeeping: variable `a` was read as value
    /// `b` (the value takes the variable's infos).
    pub const USED: u32 = 1;
    /// `Codegen::def_var`'s bookkeeping: variable `a` was defined as value
    /// `b` (the variable takes the value's infos).
    pub const DEFINED: u32 = 2;
    /// `Codegen::store`'s (and `def_var`'s home store's) bookkeeping:
    /// value `b` was stored at pointer `a`.
    pub const STORED: u32 = 3;
    /// `Codegen::jump`'s and `branch`'s bookkeeping: value `c` flows into
    /// parameter `b` of block `a`.
    pub const EDGE: u32 = 4;
    /// `Codegen::ir_temp`'s naming: variable `b` holds operand `a`, as the
    /// temporary `$ir_value<id>` a later read of it finds.
    pub const TEMP: u32 = 5;
    /// Keep word `a` for the next call of this request (applied at once).
    pub const KEEP: u32 = 6;
}

/// Re-entry kinds: `reenter(kind)`, then decline.
pub mod reentry {
    /// Rust emits the whole instruction and resumes the hook with its
    /// value, which the hook then answers (it may emit more first).
    pub const INSTRUCTION: u32 = 1;
    /// Rust emits the value of the instruction's place (`Codegen::expr` of
    /// the place expression).
    pub const PLACE_VALUE: u32 = 2;
    /// Rust emits the address of the instruction's place
    /// (`Codegen::address`).
    pub const PLACE_ADDRESS: u32 = 3;
}

/// The facts of one instruction ([`query`]), built by the walk.
#[derive(Debug, Clone, Copy)]
pub struct Facts {
    pub kind: u64,
    pub place_var: u64,
    pub path_len: u64,
    pub pass: u64,
    pub assign: u64,
    pub flags: u64,
    pub plan: u64,
    pub optimize: u64,
}

impl Default for Facts {
    fn default() -> Self {
        Facts {
            kind: kind::OTHER,
            place_var: NONE,
            path_len: NONE,
            pass: NONE,
            assign: NONE,
            flags: 0,
            plan: 0,
            optimize: 0,
        }
    }
}

/// The walk's maps, read for one request.
pub trait Maps {
    /// The variable holding the address of `var`'s home slot.
    fn home(&self, var: u32) -> Option<u32>;
    fn narrow(&self, var: u32) -> bool;
    fn register(&self, var: u32) -> bool;
    /// The variables of split local `var`'s words, in layout order.
    fn split_words(&self, var: u32) -> Vec<u32>;
    /// The variable holding operand `index`.
    fn operand_var(&self, index: usize) -> Option<u32>;
}

/// Bookkeeping a hook recorded ([`effect`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    Used {
        var: u32,
        value: ir::Value,
    },
    Defined {
        var: u32,
        value: ir::Value,
    },
    Stored {
        pointer: ir::Value,
        value: ir::Value,
    },
    Edge {
        block: ir::Block,
        param: usize,
        value: ir::Value,
    },
    Temp {
        operand: usize,
        var: u32,
    },
}

/// What one call of the hook answered.
#[derive(Debug)]
pub enum Answer {
    /// No hook, or it declined: the Rust code emits the instruction.
    Rust,
    /// Handled with this value.
    Value(ir::Value),
    /// Declined after asking for this re-entry ([`reentry`]).
    Reenter(u32),
}

/// What the walk lends a hook for one request: the instruction's facts,
/// the maps, and the words kept across re-entries.
struct Current<'a> {
    facts: &'a Facts,
    maps: &'a dyn Maps,
    kept: RefCell<&'a mut Vec<u64>>,
    effects: RefCell<Vec<Effect>>,
    reenter: Cell<Option<u32>>,
    error: RefCell<Option<String>>,
}

thread_local! {
    /// The request being answered on this thread (null outside one).
    static CURRENT: Cell<*const ()> = const { Cell::new(std::ptr::null()) };
}

fn with_current<R>(f: impl FnOnce(&Current<'_>) -> R) -> Option<R> {
    let p = CURRENT.with(Cell::get);
    if p.is_null() {
        return None;
    }
    // SAFETY: `CURRENT` is set only by `instruction`, to a `Current` in its
    // frame, for the duration of the hook call, and cleared before that
    // frame ends (also on unwind).
    Some(f(unsafe { &*(p as *const Current<'_>) }))
}

/// Whether a hook would be asked to emit `op` (only then does the walk
/// build the facts).
pub fn wanted(world: &World, op: &Op) -> bool {
    super::ACTIVE.load(std::sync::atomic::Ordering::Acquire)
        && super::hook(Family::of(world, op)).is_some()
}

/// One call of the instruction request with the walk lent: `None`-like
/// [`Answer::Rust`] when no hook is in effect or it declined without
/// re-entering. The effects come back for the walk to apply.
#[allow(clippy::too_many_arguments)]
pub fn instruction(
    builder: &mut FunctionBuilder<'_>,
    module: &mut dyn Module,
    world: &World,
    program: &Program,
    (function, block, index, inst): (&str, usize, usize, &crate::ir::Instruction),
    values: &std::collections::BTreeMap<crate::ir::ValueId, ir::Value>,
    ty: &crate::ast::Type,
    facts: &Facts,
    maps: &dyn Maps,
    kept: &mut Vec<u64>,
) -> Result<(Answer, Vec<Effect>), String> {
    if !super::ACTIVE.load(std::sync::atomic::Ordering::Acquire) {
        return Ok((Answer::Rust, vec![]));
    }
    super::take_error()?;
    let family = Family::of(world, &inst.op);
    if super::hook(family).is_none() {
        return Ok((Answer::Rust, vec![]));
    }
    let args: Vec<u64> = crate::ir_inline::operands(&inst.op)
        .iter()
        .map(|id| values.get(id).map_or(super::NO_VALUE, |v| super::word(*v)))
        .collect();
    let site = Site {
        function,
        block: block as u32,
        inst: index as u32,
    };
    let current = Current {
        facts,
        maps,
        kept: RefCell::new(kept),
        effects: RefCell::new(vec![]),
        reenter: Cell::new(None),
        error: RefCell::new(None),
    };
    struct Clear(*const ());
    impl Drop for Clear {
        fn drop(&mut self) {
            CURRENT.with(|c| c.set(self.0));
        }
    }
    let answer = {
        let _clear = Clear(CURRENT.with(|c| c.replace(&raw const current as *const ())));
        request(
            family,
            builder,
            module,
            world,
            program,
            site,
            super::INSTRUCTION,
            &args,
            Some(ty),
        )?
    };
    if let Some(e) = current.error.take() {
        return Err(e);
    }
    let effects = current.effects.take();
    let answer = match (answer, current.reenter.get()) {
        (None, None) => Answer::Rust,
        (None, Some(kind)) => Answer::Reenter(kind),
        (Some(results), None) => Answer::Value(super::value_of(&results)?),
        (Some(_), Some(kind)) => {
            return Err(format!(
                "codegen hook asked for re-entry {kind} and answered the request too"
            ));
        }
    };
    Ok((answer, effects))
}

/// A query ([`query`]) of the request being answered.
pub fn query(q: u32, a: u64, b: u64) -> u64 {
    with_current(|c| {
        let var = u32::try_from(a).ok();
        let f = c.facts;
        let found = match q {
            query::KIND => Some(f.kind),
            query::PLACE_VAR => Some(f.place_var),
            query::PATH_LEN => Some(f.path_len),
            query::PASS => Some(f.pass),
            query::ASSIGN => Some(f.assign),
            query::FLAGS => Some(f.flags),
            query::PLAN => Some(f.plan),
            query::OPTIMIZE => Some(f.optimize),
            query::OPERAND_VAR => usize::try_from(a)
                .ok()
                .and_then(|i| c.maps.operand_var(i))
                .map(u64::from),
            query::HOME => var.and_then(|v| c.maps.home(v)).map(u64::from),
            query::NARROW => var.map(|v| u64::from(c.maps.narrow(v))),
            query::REGISTER => var.map(|v| u64::from(c.maps.register(v))),
            query::SPLIT_WORDS => var.map(|v| c.maps.split_words(v).len() as u64),
            query::SPLIT_WORD => var.and_then(|v| {
                let words = c.maps.split_words(v);
                words.get(usize::try_from(b).ok()?).map(|w| u64::from(*w))
            }),
            query::KEPT_COUNT => Some(c.kept.borrow().len() as u64),
            query::KEPT => usize::try_from(a)
                .ok()
                .and_then(|i| c.kept.borrow().get(i).copied()),
            _ => None,
        };
        found.unwrap_or(NONE)
    })
    .unwrap_or(NONE)
}

/// Records effect `e` ([`effect`]) of the request being answered. A
/// malformed effect fails the compile when the hook returns.
pub fn record(e: u32, a: u64, b: u64, c: u64) {
    with_current(|cur| {
        let value = |w: u64| u32::try_from(w).ok().map(ir::Value::from_u32);
        let var = |w: u64| u32::try_from(w).ok();
        let index = |w: u64| usize::try_from(w).ok();
        let effect =
            match e {
                effect::USED => var(a)
                    .zip(value(b))
                    .map(|(var, value)| Effect::Used { var, value }),
                effect::DEFINED => var(a)
                    .zip(value(b))
                    .map(|(var, value)| Effect::Defined { var, value }),
                effect::STORED => value(a)
                    .zip(value(b))
                    .map(|(pointer, value)| Effect::Stored { pointer, value }),
                effect::EDGE => u32::try_from(a).ok().zip(index(b)).zip(value(c)).map(
                    |((block, param), value)| Effect::Edge {
                        block: ir::Block::from_u32(block),
                        param,
                        value,
                    },
                ),
                effect::TEMP => index(a)
                    .zip(var(b))
                    .map(|(operand, var)| Effect::Temp { operand, var }),
                effect::KEEP => {
                    cur.kept.borrow_mut().push(a);
                    return;
                }
                _ => None,
            };
        match effect {
            Some(effect) => cur.effects.borrow_mut().push(effect),
            None => {
                cur.error.borrow_mut().get_or_insert_with(|| {
                    format!("codegen hook recorded a malformed walk effect {e} ({a}, {b}, {c})")
                });
            }
        }
    });
}

/// Asks for re-entry `kind` ([`reentry`]); the hook then declines.
pub fn reenter(kind: u32) {
    with_current(|c| {
        if !(reentry::INSTRUCTION..=reentry::PLACE_ADDRESS).contains(&kind) {
            c.error
                .borrow_mut()
                .get_or_insert_with(|| format!("codegen hook asked for unknown re-entry {kind}"));
        } else if c.reenter.replace(Some(kind)).is_some() {
            c.error
                .borrow_mut()
                .get_or_insert_with(|| "codegen hook asked for two re-entries in one call".into());
        }
    });
}

/// [`query`] as a C-ABI symbol (`@vesper/codegen-walk`'s `.vsys` import).
///
/// # Safety
///
/// Callable from any thread at any time: outside a request it answers
/// [`NONE`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vesper_walk_query(q: u32, a: u64, b: u64) -> u64 {
    query(q, a, b)
}

/// [`record`] as a C-ABI symbol.
///
/// # Safety
///
/// Callable from any thread at any time: outside a request it does
/// nothing.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vesper_walk_effect(e: u32, a: u64, b: u64, c: u64) {
    record(e, a, b, c)
}

/// [`reenter`] as a C-ABI symbol.
///
/// # Safety
///
/// Callable from any thread at any time: outside a request it does
/// nothing.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vesper_walk_reenter(kind: u32) {
    reenter(kind)
}

/// The walk's C-ABI symbols, for a JIT host that binds
/// `@vesper/codegen-walk`'s imports (with the shim's own).
pub fn symbols() -> Vec<(&'static str, *const u8)> {
    vec![
        ("vesper_walk_query", vesper_walk_query as *const u8),
        ("vesper_walk_effect", vesper_walk_effect as *const u8),
        ("vesper_walk_reenter", vesper_walk_reenter as *const u8),
    ]
}
