//! The CLIF clean-up on the native optimize path (#1426,
//! src/native_clif_opt.rs). Shape tests read the emitted Cranelift function
//! through the observer: `||` chains branch directly instead of building a
//! 0/1 value for a join block, `Option` temporaries and copy aggregates
//! leave no stack slot behind, and unused loads are gone. Each source also
//! runs on the evaluator and both JIT configurations, which must agree.
use cranelift_codegen::ir::{InstructionData, Opcode};
use vesper::{check, native, parser, runtime::Runtime};

fn accepted(source: &str) -> check::Checked {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    let c = check::check(&p.module);
    assert!(c.diagnostics.is_empty(), "{:#?}", c.diagnostics);
    c
}

/// Per emitted function named `function`: (stack slots with a size, blocks
/// that only `brif` on one of their parameters, loads whose result is
/// unused).
fn shape(source: &str, function: &str) -> Vec<(usize, usize, usize)> {
    let c = accepted(source);
    let mut out = vec![];
    native::object_with_observer(
        &c.world,
        "main",
        None,
        true,
        Some(&mut |f: native::EmittedFunction| {
            if f.source != function {
                return;
            }
            let func = &f.function;
            let mut used = std::collections::HashSet::new();
            for block in func.layout.blocks() {
                for inst in func.layout.block_insts(block) {
                    for v in func.dfg.inst_values(inst) {
                        used.insert(func.dfg.resolve_aliases(v));
                    }
                }
            }
            let mut flag_blocks = 0;
            let mut dead_loads = 0;
            for block in func.layout.blocks() {
                let insts: Vec<_> = func.layout.block_insts(block).collect();
                if let [only] = insts[..]
                    && let InstructionData::Brif { arg, .. } = func.dfg.insts[only]
                    && func
                        .dfg
                        .block_params(block)
                        .contains(&func.dfg.resolve_aliases(arg))
                {
                    flag_blocks += 1;
                }
                for inst in insts {
                    if func.dfg.insts[inst].opcode() == Opcode::Load
                        && !used.contains(&func.dfg.first_result(inst))
                    {
                        dead_loads += 1;
                    }
                }
            }
            let slots = func
                .sized_stack_slots
                .values()
                .filter(|s| s.size > 0)
                .count();
            out.push((slots, flag_blocks, dead_loads));
        }),
    )
    .unwrap();
    assert!(!out.is_empty(), "{function} was not emitted");
    out
}

fn agree(source: &str) -> String {
    let c = accepted(source);
    let evaluated = Runtime::new(&c.world)
        .run("main", vec![])
        .unwrap()
        .to_string();
    for optimize in [true, false] {
        let jit = native::jit_with_options(&c.world, "main", optimize)
            .unwrap()
            .to_string();
        assert_eq!(jit, evaluated, "JIT (optimize {optimize})");
    }
    evaluated
}

/// Quarry's lexer shape (SH6 lane 6): a byte class tested with `||`.
const CLASSES: &str = r#"
fn is_space(b: u8) -> bool {
    b == 32 || b == 9 || b == 10 || b == 13
}

fn count_spaces(in s: Slice[u8]) -> u64 {
    let mut n: u64 = 0;
    for i in 0..s.len() {
        match s.try_get(i) {
            Option::Some(b) => {
                if is_space(b) {
                    n = wrapping_add(n, 1);
                }
            },
            Option::None => (),
        }
    }
    n
}

fn main() -> u64 {
    let mut v = ChunkSeq::new[u8]();
    let n: u64 = 40;
    for i in 0..n {
        v.push(if i % 3 == 0 { 32 } else if i % 7 == 0 { 10 } else { 65 });
    }
    count_spaces(v.as_slice())
}
"#;

#[test]
fn or_chains_branch_directly() {
    for (_, flag_blocks, dead_loads) in shape(CLASSES, "count_spaces") {
        assert_eq!(flag_blocks, 0);
        assert_eq!(dead_loads, 0);
    }
    assert_eq!(agree(CLASSES), "18");
}

/// A copy struct chosen by `if` and an `Option` matched twice: both used
/// to live in stack slots.
const AGGREGATES: &str = r#"
copy struct Pair {
    a: u64,
    b: u64
}

fn pick(c: bool, x: u64) -> Pair {
    let p = if c {
        Pair { a: x, b: wrapping_add(x, 1) }
    } else {
        Pair { a: 7, b: x }
    };
    p
}

fn fold(n: u64) -> u64 {
    let mut total: u64 = 0;
    for i in 0..n {
        let p = pick(i % 2 == 0, i);
        let o = if p.a > 3 { Option::Some(p.b) } else { Option::None };
        total = match o {
            Option::Some(v) => wrapping_add(total, v),
            Option::None => wrapping_add(total, 1),
        };
    }
    total
}

fn main() -> u64 {
    fold(20)
}
"#;

#[test]
fn chosen_aggregates_and_options_leave_no_slot() {
    for (slots, flag_blocks, dead_loads) in shape(AGGREGATES, "fold") {
        assert_eq!((slots, flag_blocks, dead_loads), (0, 0, 0));
    }
    assert_eq!(agree(AGGREGATES), "198");
}

/// A helper that writes through an `inout` slice with a fallback, inlined
/// three times: every write used to read the view's handle, offset and
/// data pointer again in each arm (SH6 lane 11 item 1, warp-shard #1448).
const HEADERS: &str = r#"
fn wr16(inout b: Slice[u8], i: u64, v: u16) {
    if !b.try_write_u16_be(i, v) {
        b.try_set(i, wrapping_u8(v >> 8));
        b.try_set(wrapping_add(i, 1), wrapping_u8(v));
    }
}

fn put(inout out: Slice[u8], a: u16, b: u16, c: u16) -> u64 {
    if out.len() < 6 {
        return 0;
    }
    wr16(inout out, 0, a);
    wr16(inout out, 2, b);
    wr16(inout out, 4, c);
    6
}

fn main() -> u64 {
    let mut v = ChunkSeq::new[u8]();
    let n: u64 = 8;
    for i in 0..n {
        v.push(0);
    }
    let w = put(inout v.as_slice_mut(), 258, 772, 1286);
    let mut sum: u64 = w;
    for i in 0..n {
        sum = match v.try_get(i) {
            Option::Some(x) => wrapping_add(wrapping_mul(sum, 31), widen_u64(x)),
            Option::None => sum,
        };
    }
    sum
}
"#;

#[test]
fn view_header_words_are_read_once() {
    let c = accepted(HEADERS);
    let mut seen = 0;
    native::object_with_observer(
        &c.world,
        "main",
        None,
        true,
        Some(&mut |f: native::EmittedFunction| {
            if f.source != "put" {
                return;
            }
            seen += 1;
            let func = &f.function;
            let entry = func.layout.entry_block().unwrap();
            let view = func.dfg.block_params(entry)[0];
            // Loads of the view, then of the handle it holds.
            let mut header = std::collections::HashSet::new();
            header.insert(view);
            let mut words = vec![];
            for block in func.layout.blocks() {
                for inst in func.layout.block_insts(block) {
                    if let InstructionData::Load { arg, offset, .. } = func.dfg.insts[inst]
                        && header.contains(&func.dfg.resolve_aliases(arg))
                    {
                        words.push((func.dfg.resolve_aliases(arg), i32::from(offset)));
                        header.insert(func.dfg.first_result(inst));
                    }
                }
            }
            let distinct: std::collections::HashSet<_> = words.iter().collect();
            assert_eq!(words.len(), distinct.len(), "{words:?}\n{}", func.display());
        }),
    )
    .unwrap();
    assert!(seen > 0, "put was not emitted");
    assert_eq!(agree(HEADERS), "5146723582377");
}

/// A sequence local that only an early-return branch uses (kinetic's
/// `fastmath_sin` cache, #1404 gap 8): its slot used to be zeroed at
/// function entry, and its drop flag tested on the fast path as a
/// constant-false branch.
const COLD_LOCAL: &str = r#"
fn helper(inout c: Slice[u32], b: u64) -> u64 {
    let mut n = b;
    for i in 0..c.len() {
        match c.try_get(i) {
            Option::Some(w) => {
                n = wrapping_add(n, widen_u64(w));
            },
            Option::None => (),
        }
    }
    if c.len() == 0 { wrapping_mul(n, 3) } else { n }
}

fn cold(x: u64) -> u64 {
    if x > 1000 {
        let mut cache: ChunkSeq[u32] = ChunkSeq::new[u32]();
        cache.push(5);
        return helper(inout cache.as_slice_mut(), x);
    }
    wrapping_add(x, 7)
}

fn main() -> u64 {
    let mut total: u64 = 0;
    let n: u64 = 3000;
    for i in 0..n {
        total = wrapping_add(total, cold(i));
    }
    total
}
"#;

#[test]
fn a_cold_local_costs_the_entry_nothing() {
    let c = accepted(COLD_LOCAL);
    let mut seen = 0;
    native::object_with_observer(
        &c.world,
        "main",
        None,
        true,
        Some(&mut |f: native::EmittedFunction| {
            if f.source != "cold" {
                return;
            }
            seen += 1;
            let func = &f.function;
            let entry = func.layout.entry_block().unwrap();
            for inst in func.layout.block_insts(entry) {
                assert_ne!(
                    func.dfg.insts[inst].opcode(),
                    Opcode::Store,
                    "a store in the entry block\n{}",
                    func.display()
                );
            }
            for block in func.layout.blocks() {
                for inst in func.layout.block_insts(block) {
                    if let InstructionData::Brif { arg, .. } = func.dfg.insts[inst] {
                        let arg = func.dfg.resolve_aliases(arg);
                        let def = func.dfg.value_def(arg).inst();
                        assert!(
                            !def.is_some_and(|d| func.dfg.insts[d].opcode() == Opcode::Iconst),
                            "a branch on a constant\n{}",
                            func.display()
                        );
                    }
                }
            }
        }),
    )
    .unwrap();
    assert!(seen > 0, "cold was not emitted");
    assert_eq!(agree(COLD_LOCAL), "4515502");
}
