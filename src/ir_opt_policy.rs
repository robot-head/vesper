//! Native-path policy around the unchanged [`crate::ir_opt::optimize`]
//! (self-hosting wave SH4 lane 5, #1279; the owner chose option A).
//!
//! `ir_opt::optimize` is Stage 1 guarded and stays as it is. It is three
//! independent per-function passes followed by token threading and
//! validation: effect equality saturation (one egglog program per block
//! that has a scalar sink read of a local), `fip_reuse_pass`,
//! `ir::thread_tokens` and `ir_check::validate`. Every one of them works on
//! one function at a time, so running it on a subset of the functions gives
//! exactly the result it gives those functions inside the whole program.
//!
//! This policy splits the work:
//!
//! - **Screen.** [`block_may_improve`] is a necessary condition for
//!   saturation to remove a read in a block. On the combined checker
//!   program 6% of the blocks pass it, yet `ir_opt::optimize` builds and runs
//!   an egglog program for most of the others.
//! - **Saturate candidates only.** A function with no candidate block skips
//!   saturation. A function with some goes through the unchanged
//!   `ir_opt::optimize` as a [`surrogate`] that holds only its candidate
//!   blocks; the replacements the pass makes there are read back and
//!   [`apply`]'d to the real function. All surrogates go through one call.
//! - **Cache (opt-in).** With `VESPER_OPT_POLICY_CACHE=on` (or
//!   [`set_cache`]), a function whose content hash was saturated earlier in
//!   this process takes the saturated body from the cache.
//! - **The rest of the pass.** Every function then gets the passes
//!   `ir_opt::optimize` runs after saturation, unchanged: `fip_reuse_pass`,
//!   `ir::thread_tokens` and `ir_check::validate`.
//!
//! The output program and the returned count are the same as
//! `ir_opt::optimize` on the whole program; `tests/optimizer_budget.rs`
//! checks that over the corpus. Only the text of a validation error can
//! differ (errors are reported per group).
//!
//! The policy is the default `opt` path of `src/ir_swap.rs`, which
//! `src/native.rs`, `src/native_export.rs` and `src/native_session.rs` call.
//! The wasm backend (`src/wasm_backend.rs`) and `vesper emit --emit ir`
//! call it too (SH5 lane 9, #1332). The verified backend and the evaluator
//! keep calling `ir_opt::optimize` directly, so `bootstrap/stage1.lock` does
//! not move. `VESPER_OPT_POLICY=off` turns the policy off (every call goes
//! straight to `ir_opt::optimize`), for measuring.
use crate::{
    ast::{Pass, PatternKind},
    check::{Place, World},
    ir::{self, FunctionIr, Op},
};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::hash::{Hash, Hasher};
use std::sync::{Mutex, OnceLock};

/// What one [`optimize`] call did, for the report and the tests.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stats {
    pub functions: usize,
    /// Functions that skip saturation because no block can lose a read.
    pub screened_out: usize,
    /// Functions whose saturation result came from the cache.
    pub cached: usize,
    /// Functions saturated (as surrogates) by `ir_opt::optimize`.
    pub saturated: usize,
    /// Saturated functions that lost at least one read.
    pub improved: usize,
    /// Blocks in all functions, and blocks that pass the screen.
    pub blocks: usize,
    pub candidate_blocks: usize,
    /// The count `ir_opt::optimize` returns for the whole program.
    pub eliminated: usize,
    /// Wall time of the phases, in microseconds: screening and hashing,
    /// saturating the surrogates, then `fip_reuse_pass`, `thread_tokens`
    /// and `validate` over every function.
    pub screen_micros: u128,
    pub saturate_micros: u128,
    pub fip_micros: u128,
    pub tokens_micros: u128,
    pub validate_micros: u128,
}

fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        !matches!(
            std::env::var("VESPER_OPT_POLICY").as_deref(),
            Ok("off" | "0" | "false")
        )
    })
}

/// Drop-in for `ir_opt::optimize` on the native paths.
pub fn optimize(world: &World, program: &mut ir::Program) -> Result<usize, String> {
    if !enabled() {
        return crate::ir_opt::optimize(world, program);
    }
    optimize_with_stats(world, program).map(|s| s.eliminated)
}

/// A cached saturation result. `Unchanged` means saturation removed
/// nothing; `Saturated` holds the function as saturation left it (before
/// `fip_reuse_pass`) and the number of reads it removed.
#[derive(Clone)]
enum Cached {
    Unchanged,
    Saturated(Box<FunctionIr>, usize),
}

/// Bounds the cache so a long-lived process (a test binary that compiles
/// the whole corpus) does not grow without limit. Past this the cache is
/// cleared and refills.
const CACHE_LIMIT: usize = 16_384;

/// Whether the cache is used. Off by default: a cache that lives for the
/// process makes the work (and the allocations) of one compilation depend on
/// what the process compiled before, which tests that count a compilation's
/// allocations (tests/dyn_trait.rs `objects_allocate_nothing`) can see.
/// `VESPER_OPT_POLICY_CACHE=on` or [`set_cache`] turns it on, for
/// long-lived processes that compile the same code again (an editor, a test
/// harness).
static CACHE_ON: OnceLock<std::sync::atomic::AtomicBool> = OnceLock::new();

fn cache_flag() -> &'static std::sync::atomic::AtomicBool {
    CACHE_ON.get_or_init(|| {
        std::sync::atomic::AtomicBool::new(matches!(
            std::env::var("VESPER_OPT_POLICY_CACHE").as_deref(),
            Ok("on" | "1" | "true")
        ))
    })
}

fn cache_enabled() -> bool {
    cache_flag().load(std::sync::atomic::Ordering::Relaxed)
}

/// Turns the in-process saturation cache on or off for this process.
pub fn set_cache(enabled: bool) {
    cache_flag().store(enabled, std::sync::atomic::Ordering::Relaxed);
}

fn cache() -> &'static Mutex<HashMap<u128, Cached>> {
    static CACHE: OnceLock<Mutex<HashMap<u128, Cached>>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// The policy itself, with what it did.
pub fn optimize_with_stats(world: &World, program: &mut ir::Program) -> Result<Stats, String> {
    let mut stats = Stats {
        functions: program.functions.len(),
        ..Stats::default()
    };
    let mut rest = ir::Program::default();
    let mut saturated = vec![];
    let mut cached_count = 0usize;
    let clock = std::time::Instant::now();
    for (name, f) in std::mem::take(&mut program.functions) {
        let candidates = candidate_blocks(&f);
        stats.blocks += f.blocks.len();
        stats.candidate_blocks += candidates;
        if candidates == 0 {
            stats.screened_out += 1;
            rest.functions.insert(name, f);
            continue;
        }
        let caching = cache_enabled();
        let key = if caching { content_hash(&f) } else { 0 };
        let hit = if caching {
            cache().lock().unwrap().get(&key).cloned()
        } else {
            None
        };
        match hit {
            Some(Cached::Unchanged) => {
                stats.cached += 1;
                rest.functions.insert(name, f);
            }
            Some(Cached::Saturated(done, n)) => {
                stats.cached += 1;
                cached_count += n;
                rest.functions.insert(name, *done);
            }
            None => saturated.push((name, f, key)),
        }
    }
    stats.screen_micros = clock.elapsed().as_micros();
    let clock = std::time::Instant::now();
    // Saturation, on surrogates that hold only the candidate blocks (see
    // [`surrogate`]).
    let mut learned = vec![];
    if !saturated.is_empty() {
        let mut group = ir::Program::default();
        let mut saved = vec![];
        for (name, f, key) in saturated {
            stats.saturated += 1;
            // One surrogate per candidate block (SH5 lane 9): each block's
            // egglog program then holds `Ref` nodes for its own values only.
            let mut blocks = vec![];
            if !sharp_screen() {
                // Lane 5's policy, for measuring: one surrogate per function.
                let (mut s, reads) = surrogate(&f);
                s.name = block_surrogate_name(&name, 0);
                group.functions.insert(s.name.clone(), s);
                blocks.push((0, reads));
                saved.push((name, f, key, blocks));
                continue;
            }
            for (k, b) in f.blocks.iter().enumerate() {
                if !block_can_improve(&f, b) {
                    continue;
                }
                let (mut s, reads) = surrogate_of(&f, |j, _| j == k);
                s.name = block_surrogate_name(&name, k);
                group.functions.insert(s.name.clone(), s);
                blocks.push((k, reads));
            }
            saved.push((name, f, key, blocks));
        }
        // The pass works per function, so contiguous parts of the group
        // saturate independently. The first part (in name order) with an
        // error other than validation reports it, as the whole pass would.
        let mut parts = split(std::mem::take(&mut group), workers());
        let results = in_parallel(&mut parts, |p| crate::ir_opt::optimize(world, p).err());
        for e in results.into_iter().flatten() {
            // The surrogates are not meant to validate; the real functions
            // are validated below.
            if !e.starts_with(VALIDATION_ERROR) {
                return Err(e);
            }
        }
        for mut p in parts {
            group.functions.append(&mut p.functions);
        }
        for (name, mut out, key, blocks) in saved {
            let mut aliases = BTreeMap::new();
            for (k, reads) in blocks {
                let s = group
                    .functions
                    .remove(&block_surrogate_name(&name, k))
                    .expect("optimize keeps functions");
                aliases.extend(replacements(&s, &reads));
            }
            apply(&mut out, &aliases);
            let n = aliases.len();
            stats.eliminated += n;
            if n == 0 {
                learned.push((key, Cached::Unchanged));
            } else {
                stats.improved += 1;
                learned.push((key, Cached::Saturated(Box::new(out.clone()), n)));
            }
            rest.functions.insert(name, out);
        }
    }
    {
        let mut c = cache().lock().unwrap();
        if !cache_enabled() {
            learned.clear();
        }
        if c.len() + learned.len() > CACHE_LIMIT {
            c.clear();
        }
        c.extend(learned);
    }
    stats.saturate_micros = clock.elapsed().as_micros();
    // The passes `ir_opt::optimize` runs after saturation, for every
    // function.
    let clock = std::time::Instant::now();
    stats.eliminated += cached_count + crate::ir_opt::fip_reuse_pass(world, &mut rest)?;
    stats.fip_micros = clock.elapsed().as_micros();
    let clock = std::time::Instant::now();
    ir::thread_tokens(&mut rest);
    stats.tokens_micros = clock.elapsed().as_micros();
    let clock = std::time::Instant::now();
    let mut parts = split(rest, workers());
    let mut seen = BTreeSet::new();
    let errors: Vec<_> = in_parallel(&mut parts, |p| crate::ir_check::validate(world, p))
        .into_iter()
        .flatten()
        .filter(|d| seen.insert((d.code.clone(), d.span.start, d.span.end, d.message.clone())))
        .collect();
    let mut rest = ir::Program::default();
    for mut p in parts {
        rest.functions.append(&mut p.functions);
    }
    stats.validate_micros = clock.elapsed().as_micros();
    if !errors.is_empty() {
        return Err(format!("{VALIDATION_ERROR}{errors:?}"));
    }
    program.functions = rest.functions;
    Ok(stats)
}

/// Threads for saturation and validation: the machine's parallelism, at
/// most 8. `VESPER_OPT_POLICY_THREADS=n` overrides it (1 runs everything on
/// the calling thread).
fn workers() -> usize {
    static N: OnceLock<usize> = OnceLock::new();
    *N.get_or_init(|| {
        std::env::var("VESPER_OPT_POLICY_THREADS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(|| std::thread::available_parallelism().map_or(1, |n| n.get().min(8)))
            .max(1)
    })
}

/// Splits a program into at most `n` parts of contiguous names with about
/// the same number of instructions each.
fn split(program: ir::Program, n: usize) -> Vec<ir::Program> {
    let weight = |f: &FunctionIr| {
        1 + f
            .blocks
            .iter()
            .map(|b| 1 + b.instructions.len())
            .sum::<usize>()
    };
    let total: usize = program.functions.values().map(weight).sum();
    let share = total.div_ceil(n.max(1)).max(1);
    let mut parts = vec![ir::Program::default()];
    let mut filled = 0;
    for (name, f) in program.functions {
        let w = weight(&f);
        if filled >= share && parts.len() < n {
            parts.push(ir::Program::default());
            filled = 0;
        }
        filled += w;
        parts
            .last_mut()
            .expect("one part")
            .functions
            .insert(name, f);
    }
    parts
}

/// Validation and saturation recurse over types and bodies; workers get the
/// CLI thread's stack.
const WORKER_STACK_BYTES: usize = 64 << 20;

/// Runs `job` on every part, on scoped threads when there is more than one,
/// and returns the results in part order.
fn in_parallel<T: Send>(
    parts: &mut [ir::Program],
    job: impl Fn(&mut ir::Program) -> T + Sync,
) -> Vec<T> {
    if parts.len() <= 1 {
        return parts.iter_mut().map(&job).collect();
    }
    std::thread::scope(|scope| {
        let job = &job;
        let handles: Vec<_> = parts
            .iter_mut()
            .map(|p| {
                std::thread::Builder::new()
                    .name("vesper-opt-policy".to_owned())
                    .stack_size(WORKER_STACK_BYTES)
                    .spawn_scoped(scope, move || job(p))
                    .expect("spawn an optimizer policy worker")
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap_or_else(|e| std::panic::resume_unwind(e)))
            .collect()
    })
}

/// The prefix of `ir_opt::optimize`'s validation error, which the policy
/// reproduces for the real functions.
const VALIDATION_ERROR: &str = "optimized IR failed ownership validation: ";

/// A name no world function has, so `fip_reuse_pass` skips the surrogate
/// (the policy runs it on the real function afterwards).
fn surrogate_name(name: &str) -> String {
    format!("{name}\u{0}ir_opt_policy")
}

/// The surrogate of block `k` of function `name`. Block numbers are padded
/// so the surrogates sort (and report errors) in the pass's order.
fn block_surrogate_name(name: &str, k: usize) -> String {
    format!("{}\u{0}{k:010}", surrogate_name(name))
}

/// A copy of `f` holding only its candidate blocks, from which saturation's
/// replacements can be read back, and for each kept block the original
/// block index and the sink reads whose replacement it reports.
///
/// `ir_opt::optimize` builds one egglog program per block, from that
/// block's instructions and the function's values and locals alone, and a
/// read is only ever replaced by a value of its own block. So a block's
/// replacements are the same whether the other blocks are there or not, and
/// blocks that fail [`block_may_improve`] have none. Each kept block ends in
/// a jump to itself whose arguments are its sink reads of locals: the pass
/// rewrites terminator operands with its replacements, so after the pass
/// each argument names the value that read was replaced by (or itself).
/// The surrogate is renamed so `fip_reuse_pass` leaves it alone, and it is
/// not expected to validate.
fn surrogate(f: &FunctionIr) -> (FunctionIr, Vec<Vec<ir::ValueId>>) {
    surrogate_of(f, |_, b| block_can_improve(f, b))
}

/// [`surrogate`] over the blocks `keep` selects (by index and block).
fn surrogate_of(
    f: &FunctionIr,
    keep: impl Fn(usize, &ir::Block) -> bool,
) -> (FunctionIr, Vec<Vec<ir::ValueId>>) {
    let mut s = FunctionIr {
        name: surrogate_name(&f.name),
        params: f.params.clone(),
        result: f.result.clone(),
        effects: f.effects.clone(),
        bounds: f.bounds.clone(),
        external_captures: f.external_captures.clone(),
        blocks: vec![],
        values: f.values.clone(),
        locals: f.locals.clone(),
        fip_call_slots: BTreeMap::new(),
        fip_join_slots: BTreeMap::new(),
        box_drops_before_inst: BTreeMap::new(),
        box_drops_after_inst: BTreeMap::new(),
        box_drops_before_term: BTreeMap::new(),
    };
    let mut reads = vec![];
    for (k, b) in f.blocks.iter().enumerate() {
        if !keep(k, b) {
            continue;
        }
        let watched: Vec<ir::ValueId> = b
            .instructions
            .iter()
            .filter(|i| {
                matches!(&i.op, Op::Read { pass: Pass::Sink, root, .. } if f.locals.contains_key(root))
            })
            .map(|i| i.dest)
            .collect();
        let mut kept = b.clone();
        kept.terminator = ir::Terminator::Jump {
            block: s.blocks.len(),
            args: watched.clone(),
        };
        s.blocks.push(kept);
        reads.push(watched);
    }
    // Only the kept blocks' values. The pass gives every function value an
    // opaque `Ref` node in each block's program; a value the block never
    // names is a node nothing points to, so dropping it changes no equality.
    let mut used = BTreeSet::new();
    for b in &s.blocks {
        used.extend(b.params.iter().copied());
        for i in &b.instructions {
            used.insert(i.dest);
            used.extend(crate::ir_inline::operands(&i.op));
        }
    }
    s.values.retain(|v, _| used.contains(v));
    (s, reads)
}

/// Reads saturation's replacements off the saturated surrogate.
fn replacements(s: &FunctionIr, reads: &[Vec<ir::ValueId>]) -> BTreeMap<ir::ValueId, ir::ValueId> {
    let mut out = BTreeMap::new();
    for (b, watched) in s.blocks.iter().zip(reads) {
        let ir::Terminator::Jump { args, .. } = &b.terminator else {
            unreachable!("the pass keeps terminator kinds");
        };
        for (d, r) in watched.iter().zip(args) {
            if d != r {
                out.insert(*d, *r);
            }
        }
    }
    out
}

/// Applies replacements to the real function the way `ir_opt::optimize`
/// does after saturation: replaced reads are removed, every operand and
/// terminator operand is rewritten. (The pass's own `operands` helper is
/// private; this mirrors it, and `tests/optimizer_budget.rs` checks the
/// result equals the pass's over the corpus.)
fn apply(f: &mut FunctionIr, aliases: &BTreeMap<ir::ValueId, ir::ValueId>) {
    let resolve = |v: ir::ValueId| aliases.get(&v).copied().unwrap_or(v);
    for b in &mut f.blocks {
        b.instructions.retain(|i| !aliases.contains_key(&i.dest));
        for i in &mut b.instructions {
            operands(&mut i.op, resolve);
        }
        match &mut b.terminator {
            ir::Terminator::Return(v) => *v = resolve(*v),
            ir::Terminator::Jump { args, .. } => {
                for v in args {
                    *v = resolve(*v);
                }
            }
            ir::Terminator::Branch { condition, .. } => *condition = resolve(*condition),
            ir::Terminator::Match { value, .. } => *value = resolve(*value),
            _ => {}
        }
    }
}

/// The operands `ir_opt::optimize` rewrites (a mirror of its private
/// `operands`).
fn operands(op: &mut Op, resolve: impl Fn(ir::ValueId) -> ir::ValueId) {
    match op {
        Op::Bind { value, .. }
        | Op::Write { value, .. }
        | Op::Try(value)
        | Op::Unary(_, value)
        | Op::Exit { value, .. }
        | Op::Project(value, _) => *value = resolve(*value),
        Op::Binary(_, a, b) | Op::Index(a, b) => {
            *a = resolve(*a);
            *b = resolve(*b);
        }
        Op::Tuple(vs) | Op::Vector { args: vs, .. } => {
            for v in vs {
                *v = resolve(*v);
            }
        }
        Op::Data(_, fields) => {
            for (_, v) in fields {
                *v = resolve(*v);
            }
        }
        Op::Handle {
            state_value: Some(value),
            ..
        } => *value = resolve(*value),
        Op::Call { args, .. }
        | Op::Tiled { args, .. }
        | Op::Seq { args, .. }
        | Op::Fip { args, .. } => {
            for a in args {
                a.value = resolve(a.value);
            }
        }
        Op::Indirect { callee, args } => {
            *callee = resolve(*callee);
            for a in args {
                a.value = resolve(a.value);
            }
        }
        _ => {}
    }
}

/// A 128-bit hash of the whole function (its name included, since
/// `fip_reuse_pass` and validation look functions up by name).
fn content_hash(f: &FunctionIr) -> u128 {
    struct Both(std::collections::hash_map::DefaultHasher, u64);
    impl std::fmt::Write for Both {
        fn write_str(&mut self, s: &str) -> std::fmt::Result {
            s.hash(&mut self.0);
            // FNV-1a as a second, independent 64 bits.
            for b in s.bytes() {
                self.1 = (self.1 ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
            }
            Ok(())
        }
    }
    let mut h = Both(Default::default(), 0xcbf2_9ce4_8422_2325);
    std::fmt::write(&mut h, format_args!("{f:?}")).expect("Debug writes");
    (u128::from(h.0.finish()) << 64) | u128::from(h.1)
}

/// How many blocks of `f` pass [`block_can_improve`] (the candidates).
pub fn candidate_blocks(f: &FunctionIr) -> usize {
    f.blocks.iter().filter(|b| block_can_improve(f, b)).count()
}

/// The place an instruction gives `ir_opt::optimize`'s place table, the
/// same mapping it uses.
fn place_of(f: &FunctionIr, op: &Op) -> Option<Place> {
    match op {
        Op::Read { root, path, .. } if f.locals.contains_key(root) => Some(Place {
            root: root.clone(),
            path: path.clone(),
        }),
        Op::Write { place, .. } => {
            let mut parts = place.split('.');
            Some(Place {
                root: parts.next().unwrap_or_default().to_owned(),
                path: parts.map(str::to_owned).collect(),
            })
        }
        Op::Bind { pattern, .. } => match &pattern.kind {
            PatternKind::Bind(n) => Some(Place {
                root: n.clone(),
                path: vec![],
            }),
            _ => None,
        },
        _ => None,
    }
}

/// A necessary condition for saturation to remove a read in this block.
///
/// Saturation can only replace a sink read `d` of place `p` with an earlier
/// value when the term `Read(s, p)` reaches that value: through a write of
/// `p` earlier in the block (`Read (Write s p v) p = v`, possibly across
/// writes to disjoint places), or through an earlier sink read of `p` from a
/// state it shares. State terms within a block are a chain from one `Start`,
/// so no two are equal, and every other equality the rules make involves only
/// instructions after `d`. So a block where no sink read has an earlier
/// instruction on the same place can lose nothing. A sink read without state
/// evidence counts as a candidate, so `ir_opt::optimize` still reports it.
pub fn block_may_improve(f: &FunctionIr, instructions: &[ir::Instruction]) -> bool {
    let mut seen = BTreeSet::<Place>::new();
    for i in instructions {
        let Some(p) = place_of(f, &i.op) else {
            continue;
        };
        if let Op::Read {
            pass: Pass::Sink, ..
        } = &i.op
            && (i.token.is_none() || seen.contains(&p))
        {
            return true;
        }
        seen.insert(p);
    }
    false
}

/// Per-function profile of `ir_opt::optimize` for the report: the time one
/// call takes on that function alone, the count it returns, and whether the
/// screen passes it. Runs the pass on clones; `program` is not changed.
#[derive(Debug, Clone)]
pub struct FunctionProfile {
    pub name: String,
    pub blocks: usize,
    pub values: usize,
    pub candidate_blocks: usize,
    pub micros: u128,
    pub eliminated: usize,
    pub instructions_removed: usize,
}

pub fn profile(world: &World, program: &ir::Program) -> Result<Vec<FunctionProfile>, String> {
    let mut rows = vec![];
    for (name, f) in &program.functions {
        let mut one = ir::Program {
            functions: BTreeMap::from([(name.clone(), f.clone())]),
        };
        let before: usize = f.blocks.iter().map(|b| b.instructions.len()).sum();
        let t = std::time::Instant::now();
        let eliminated = crate::ir_opt::optimize(world, &mut one)?;
        let micros = t.elapsed().as_micros();
        let after: usize = one.functions[name]
            .blocks
            .iter()
            .map(|b| b.instructions.len())
            .sum();
        rows.push(FunctionProfile {
            name: name.clone(),
            blocks: f.blocks.len(),
            values: f.values.len(),
            candidate_blocks: candidate_blocks(f),
            micros,
            eliminated,
            instructions_removed: before - after,
        });
    }
    Ok(rows)
}

/// The program the native paths hand to the optimizer: `drops::lower_world`
/// then bit-builtin lowering, as in `src/native.rs`. For the tests and the
/// profile.
pub fn native_input(world: &World) -> ir::Program {
    let mut program = crate::ffi_lowering::drops::lower_world(world);
    crate::integer_widths::lower_bit_builtin_calls(world, &mut program);
    program
}

/// One candidate block, saturated alone by `ir_opt::optimize` as a
/// one-block surrogate, for the report.
#[derive(Debug, Clone)]
pub struct BlockProfile {
    pub function: String,
    pub block: usize,
    pub instructions: usize,
    pub places: usize,
    pub values: usize,
    pub sink_reads: usize,
    pub removed: usize,
    pub micros: u128,
    /// The block passes [`block_may_improve`] but not [`block_can_improve`].
    pub screened: bool,
}

/// Times saturation of every candidate block on its own (one block, only
/// the values it names). `program` is not changed.
pub fn profile_blocks(world: &World, program: &ir::Program) -> Result<Vec<BlockProfile>, String> {
    let mut rows = vec![];
    for (name, f) in program.functions.iter() {
        for (k, b) in f.blocks.iter().enumerate() {
            if !block_may_improve(f, &b.instructions) {
                continue;
            }
            let screened = !block_can_improve(f, b);
            let (s, reads) = surrogate_of(f, |j, _| j == k);
            let values = s.values.len();
            let mut one = ir::Program {
                functions: BTreeMap::from([(s.name.clone(), s)]),
            };
            let t = std::time::Instant::now();
            if let Err(e) = crate::ir_opt::optimize(world, &mut one)
                && !e.starts_with(VALIDATION_ERROR)
            {
                return Err(e);
            }
            let micros = t.elapsed().as_micros();
            let s = one.functions.into_values().next().expect("one function");
            let places: BTreeSet<Place> = b
                .instructions
                .iter()
                .filter_map(|i| place_of(f, &i.op))
                .collect();
            rows.push(BlockProfile {
                function: name.clone(),
                block: k,
                instructions: b.instructions.len(),
                places: places.len(),
                values,
                sink_reads: reads[0].len(),
                removed: replacements(&s, &reads).len(),
                micros,
                screened,
            });
        }
    }
    Ok(rows)
}

/// `ir_opt`'s private `scalar` (mirrored): the values its saturation tracks.
fn scalar(t: &crate::ast::Type) -> bool {
    (crate::numeric::is_scalar_name(t.name()) && t.name() != "Never")
        || *t == crate::ast::Type::unit()
}

/// `VESPER_OPT_POLICY_SCREEN=basic` keeps lane 5's policy (the screen
/// [`block_may_improve`] alone, one surrogate per function), for measuring.
fn sharp_screen() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        !matches!(
            std::env::var("VESPER_OPT_POLICY_SCREEN").as_deref(),
            Ok("basic")
        )
    })
}

/// What an instruction that changes the state token is in a block's egglog
/// program: a `Write` of a place (by index) or an opaque `Barrier`.
#[derive(Clone, Copy)]
enum Step {
    Write(usize),
    Barrier,
}

/// A sharper necessary condition than [`block_may_improve`] (SH5 lane 9,
/// #1332): some scalar sink read's `Read(s, p)` term can reach an earlier
/// value under the pass's two rules.
///
/// In a block's program, a read `d` of place `p` at state `s` is unioned
/// with `Read(s, p)`. The rules rewrite `Read(Write(s', p, v), p)` to `v`
/// and `Read(Write(s', q, v), p)` to `Read(s', p)` when `p` and `q` are
/// disjoint; nothing rewrites a `Read` of a `Barrier`, of a `Start`, or of a
/// write to an overlapping other place. So the terms in `d`'s class that the
/// rules can produce are the `Read(t, p)` for the states `t` on the walk
/// back from `s` across disjoint writes, plus the value of a write of `p`
/// where the walk stops. `d` can only become equal to an earlier value if
/// that walk ends at a write of `p`, or meets a state from which an earlier
/// scalar sink read of `p` reaches the same term. This screen follows those
/// walks exactly (ignoring the round bound and the type filter, so it only
/// ever keeps more blocks than the pass improves). A sink read without
/// state evidence, or a walk through a state the block does not define,
/// keeps the block so the pass reports what it reports.
pub fn block_can_improve(f: &FunctionIr, block: &ir::Block) -> bool {
    if !block_may_improve(f, &block.instructions) {
        return false;
    }
    if !sharp_screen() {
        return true;
    }
    // The pass's place table and place graph for this block.
    let mut places = BTreeMap::<Place, usize>::new();
    for i in &block.instructions {
        if let Some(p) = place_of(f, &i.op) {
            let next = places.len();
            places.entry(p).or_insert(next);
        }
    }
    let mut graph = ir::PlaceGraph::default();
    for p in places.keys() {
        graph.insert(p);
    }
    let by_index: Vec<&Place> = {
        let mut v = vec![None; places.len()];
        for (p, k) in &places {
            v[*k] = Some(p);
        }
        v.into_iter()
            .map(|p| p.expect("dense place indices"))
            .collect()
    };
    let mut disjoint = HashMap::<(usize, usize), bool>::new();
    let mut is_disjoint = |a: usize, b: usize| {
        *disjoint
            .entry((a, b))
            .or_insert_with(|| !graph.overlaps(by_index[a], by_index[b]))
    };
    // State token -> (the state it was made from, what made it).
    let mut steps = HashMap::<u32, (u32, Step)>::new();
    let mut consumed = BTreeSet::<u32>::new();
    // (state, place) for every `Read(state, place)` term an earlier scalar
    // sink read's class holds.
    let mut reached = BTreeSet::<(u32, usize)>::new();
    for i in &block.instructions {
        if let Op::Read {
            root,
            path,
            pass: Pass::Sink,
            ..
        } = &i.op
            && scalar(&i.ty)
            && let Some(&p) = places.get(&Place {
                root: root.clone(),
                path: path.clone(),
            })
        {
            let Some((input, _)) = i.token else {
                return true;
            };
            let mut s = input;
            let mut walk = vec![];
            loop {
                if reached.contains(&(s, p)) {
                    return true;
                }
                walk.push(s);
                match steps.get(&s) {
                    Some((_, Step::Write(q))) if *q == p => return true,
                    Some((from, Step::Write(q))) if is_disjoint(*q, p) => s = *from,
                    Some(_) => break,
                    None if s == block.entry_token => break,
                    None => return true,
                }
                if walk.len() > block.instructions.len() + 1 {
                    // A cycle of states; let the pass decide.
                    return true;
                }
            }
            reached.extend(walk.into_iter().map(|s| (s, p)));
        }
        if let Some((input, output)) = i.token
            && input != output
        {
            let step = match &i.op {
                Op::Write { op, value, .. }
                    if op == "=" && f.values.get(value).is_some_and(scalar) =>
                {
                    place_of(f, &i.op)
                        .and_then(|p| places.get(&p))
                        .map(|k| Step::Write(*k))
                }
                Op::Bind { pattern, value, .. }
                    if matches!(pattern.kind, PatternKind::Bind(_))
                        && f.values.get(value).is_some_and(scalar) =>
                {
                    place_of(f, &i.op)
                        .and_then(|p| places.get(&p))
                        .map(|k| Step::Write(*k))
                }
                _ => None,
            };
            if !consumed.insert(input) {
                // Two steps from one state: writes of equal values from it
                // would be congruent. Not a chain; let the pass decide.
                return true;
            }
            steps.insert(output, (input, step.unwrap_or(Step::Barrier)));
        }
    }
    false
}
