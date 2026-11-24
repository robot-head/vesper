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

/// Float coefficients spelled `from_bits_f64(K)`, as ported math code
/// writes them (kinetic `fastmath`): they used to be built through a
/// general register at every use.
const COEFFICIENTS: &str = r#"
fn poly(x: f64) -> f64 {
    let z = x * x;
    from_bits_f64(4607182418800017408) + z * (from_bits_f64(4602678819172646912) + z * from_bits_f64(4598175219545276416))
}

fn main() -> u64 {
    let mut acc: u64 = 0;
    let n: u64 = 8;
    for i in 0..n {
        acc = wrapping_add(acc, to_bits_f64(poly(from_bits_f64(wrapping_mul(i, 4503599627370496)))) >> 40);
    }
    acc
}
"#;

#[test]
fn float_coefficients_are_constants() {
    let c = accepted(COEFFICIENTS);
    let mut seen = 0;
    native::object_with_observer(
        &c.world,
        "main",
        None,
        true,
        Some(&mut |f: native::EmittedFunction| {
            if f.source != "poly" {
                return;
            }
            seen += 1;
            let func = &f.function;
            for block in func.layout.blocks() {
                for inst in func.layout.block_insts(block) {
                    if func.dfg.insts[inst].opcode() == Opcode::Bitcast {
                        let arg = func.dfg.resolve_aliases(func.dfg.inst_args(inst)[0]);
                        let def = func.dfg.value_def(arg).inst();
                        assert!(
                            !def.is_some_and(|d| func.dfg.insts[d].opcode() == Opcode::Iconst),
                            "a float built from an integer constant\n{}",
                            func.display()
                        );
                    }
                }
            }
        }),
    )
    .unwrap();
    assert!(seen > 0, "poly was not emitted");
    assert_eq!(agree(COEFFICIENTS), "33521664");
}

#[test]
fn narrow_extend_constant_chains_fold() {
    let source = "fn clz_like(v: u64, k: u32) -> u64 {
    if v == 0 {
        return wrapping_add(64, widen_u64(k));
    }
    let mut x = v;
    let mut n: u64 = 0;
    if x >> 32 == 0 {
        n = wrapping_add(n, 32);
        x = x << 32;
    }
    if x >> 48 == 0 {
        n = wrapping_add(n, 16);
        x = x << 16;
    }
    let y: u32 = wrapping_u32(widen_u64(k));
    wrapping_add(n, widen_u64(y))
}
fn main() -> u64 {
    wrapping_add(clz_like(0, 10), clz_like(12345, 20))
}";
    let c = accepted(source);
    let (_, clif) = native::object(&c.world, "main").unwrap();
    assert!(
        !clif.contains("clz_like::cold#"),
        "early-return guard should not outline the hot body:\n{clif}"
    );
    assert!(
        clif.matches("ireduce").count() <= 2,
        "expected constant/round-trip narrow-extend chains to fold:\n{clif}"
    );
    assert_eq!(agree(source), "142");
}

#[test]
fn sign_extend_round_trips_fold() {
    let source = "fn sum_signed(x: u8) -> i64 {
    let a = wrapping_i8(x);
    let b = wrapping_i8(widen_i64(a));
    widen_i64(b)
}
fn main() -> i64 {
    wrapping_add(sum_signed(250), sum_signed(7))
}";
    let c = accepted(source);
    let (_, clif) = native::object(&c.world, "main").unwrap();
    assert!(
        clif.matches("sextend").count() <= 2,
        "expected sextend(ireduce(sextend(x))) round-trips to fold:\n{clif}"
    );
    assert_eq!(agree(source), "1");
}

#[test]
fn trivial_arith_and_bool_extends_fold() {
    let source = "fn pick(in s: Slice[u8], i: u64) -> u64 {
    if i < s.len() {
        match s.try_get(i) {
            Option::Some(b) => widen_u64(b),
            Option::None => 0,
        }
    } else {
        99
    }
}
fn main() -> u64 {
    let mut seq = ChunkSeq::new[u8]();
    seq.push(7);
    seq.push(11);
    let s = seq.as_slice();
    wrapping_add(pick(in s, 1), pick(in s, 5))
}";
    let c = accepted(source);
    let (_, clif) = native::object(&c.world, "main").unwrap();
    assert!(
        !clif.contains("imul_imm"),
        "expected unit-stride imul_imm x, 1 to fold:\n{clif}"
    );
    assert_eq!(agree(source), "110");
}

#[test]
fn loop_invariants_hoist_to_preheader() {
    let source = "fn main() -> u64 {
    let mut acc: u64 = 0;
    let end_k: u64 = 3;
    for k in 0..end_k {
        let a = wrapping_add(k, 2);
        let b = wrapping_add(k, 5);
        for i in 0..10 {
            let scale = wrapping_mul(a, b);
            acc = wrapping_add(acc, wrapping_add(i, scale));
        }
    }
    acc
}";
    let c = accepted(source);
    let (_, clif) = native::object(&c.world, "main").unwrap();
    // `imul` is loop-invariant in the inner `0..10` loop, so it must be
    // hoisted into the preheader rather than appearing in the same block as
    // the inner loop body's `iadd`s.
    let inner_body = clif
        .split("\n\n")
        .find(|blk| blk.trim_start().starts_with("block5:"))
        .unwrap_or("");
    assert!(
        !inner_body.is_empty() && !inner_body.contains("imul"),
        "expected loop-invariant imul to hoist out of inner loop:\n{clif}"
    );
    assert_eq!(agree(source), "695");
}

#[test]
fn guarded_loop_add_overflow_is_eliminated() {
    let source = "fn copy_offset(in src: Slice[u8], inout dst: Slice[u8], at: u64) eff [Fail[ArithOverflow], Fail[RuntimeError]] {
    for i in 0..16 {
        let b = src[2 + i];
        dst.set(at + i, b);
    }
}
fn main() -> u64 {
    let mut a = ChunkSeq::new[u8]();
    let mut b = ChunkSeq::new[u8]();
    for i in 0..32 {
        a.push(wrapping_u8(i));
        b.push(0);
    }
    handle Fail[ArithOverflow] in {
        handle Fail[RuntimeError] in {
            copy_offset(in a.as_slice(), inout b.as_slice_mut(), 4);
            match b.try_get(7) {
                Option::Some(v) => widen_u64(v),
                Option::None => 999,
            }
        } with {
            return x => x,
            raise(_, _) => 998,
        }
    } with {
        return x => x,
        raise(_, _) => 997,
    }
}";
    let c = accepted(source);
    let (_, clif) = native::object(&c.world, "main").unwrap();
    assert_eq!(agree(source), "5");
    let fast_body = clif
        .split("\n\n")
        .find(|blk| blk.contains("uload8") && blk.contains("istore8"))
        .unwrap_or("");
    assert!(
        !fast_body.is_empty() && !fast_body.contains("icmp ult"),
        "expected guarded fast-loop iadd overflow check to be eliminated and body merged:\n{clif}"
    );
}

#[test]
fn guarded_loop_add_const_offset_overflow_is_eliminated() {
    let source = "fn copy_two_halves(in src: Slice[u8], inout dst: Slice[u8], at: u64) eff [Fail[ArithOverflow], Fail[RuntimeError]] {
    for i in 0..16 {
        let b = src[i];
        dst.set(at + i, b);
        dst.set(at + 16 + i, b);
    }
}
fn main() -> u64 {
    let mut a = ChunkSeq::new[u8]();
    let mut b = ChunkSeq::new[u8]();
    for i in 0..16 {
        a.push(wrapping_u8(i));
    }
    for _ in 0..40 {
        b.push(0);
    }
    handle Fail[ArithOverflow] in {
        handle Fail[RuntimeError] in {
            copy_two_halves(in a.as_slice(), inout b.as_slice_mut(), 4);
            match b.try_get(23) {
                Option::Some(v) => widen_u64(v),
                Option::None => 999,
            }
        } with {
            return x => x,
            raise(_, _) => 998,
        }
    } with {
        return x => x,
        raise(_, _) => 997,
    }
}";
    let c = accepted(source);
    let (_, clif) = native::object(&c.world, "main").unwrap();
    assert_eq!(agree(source), "3");
    let fast_body = clif
        .split("\n\n")
        .find(|blk| blk.contains("uload8") && blk.contains("istore8"))
        .unwrap_or("");
    assert!(
        !fast_body.is_empty() && !fast_body.contains("icmp ult"),
        "expected guarded fast-loop iadd+const overflow checks to be eliminated and body merged:\n{clif}"
    );
}

#[test]
fn guarded_loop_pb0_join_param_overflow_is_eliminated() {
    let source = "fn copy_blocks(in src: Slice[u8], inout dst: Slice[u8], blocks: u64) eff [Fail[ArithOverflow], Fail[RuntimeError]] {
    for b in 0..blocks {
        let base = 16 * b;
        for i in 0..16 {
            let v = src[i];
            dst.set(base + i, v);
        }
    }
}
fn main() -> u64 {
    let mut a = ChunkSeq::new[u8]();
    let mut b = ChunkSeq::new[u8]();
    for i in 0..16 {
        a.push(wrapping_u8(i));
    }
    for _ in 0..32 {
        b.push(0);
    }
    handle Fail[ArithOverflow] in {
        handle Fail[RuntimeError] in {
            copy_blocks(in a.as_slice(), inout b.as_slice_mut(), 2);
            match b.try_get(19) {
                Option::Some(v) => widen_u64(v),
                Option::None => 999,
            }
        } with {
            return x => x,
            raise(_, _) => 998,
        }
    } with {
        return x => x,
        raise(_, _) => 997,
    }
}";
    let c = accepted(source);
    let (_, clif) = native::object(&c.world, "main").unwrap();
    assert_eq!(agree(source), "3");
    let fast_body = clif
        .split("\n\n")
        .find(|blk| blk.contains("uload8") && blk.contains("istore8"))
        .unwrap_or("");
    assert!(
        !fast_body.is_empty() && !fast_body.contains("icmp ult"),
        "expected guarded fast-loop pb0 join-param iadd overflow check to be eliminated:\n{clif}"
    );
}

#[test]
fn brif_preheader_hoists_reassociated_address_arithmetic() {
    let source = "fn copy_strided(in src: Slice[u8], inout dst: Slice[u32], blocks: u64) eff [Fail[ArithOverflow], Fail[RuntimeError]] {
    for b in 0..blocks {
        let base = 16 * b;
        for i in 0..16 {
            let v = widen_u32(src[2 + i]);
            dst.set(base + i, v);
        }
    }
}
fn main() -> u64 {
    let mut a = ChunkSeq::new[u8]();
    let mut b = ChunkSeq::new[u32]();
    for i in 0..20 {
        a.push(wrapping_u8(i));
    }
    for _ in 0..32 {
        b.push(0);
    }
    handle Fail[ArithOverflow] in {
        handle Fail[RuntimeError] in {
            copy_strided(in a.as_slice(), inout b.as_slice_mut(), 2);
            match b.try_get(19) {
                Option::Some(v) => widen_u64(v),
                Option::None => 999,
            }
        } with {
            return x => x,
            raise(_, _) => 998,
        }
    } with {
        return x => x,
        raise(_, _) => 997,
    }
}";
    let c = accepted(source);
    let (_, clif) = native::object(&c.world, "main").unwrap();
    assert_eq!(agree(source), "5");
    let fast_body = clif
        .split("\n\n")
        .find(|blk| blk.contains("uload8") && blk.contains("istore32"))
        .unwrap_or("");
    let imul_count = fast_body.matches("imul_imm").count();
    assert!(
        !fast_body.is_empty() && imul_count <= 1,
        "expected invariant base/scale arithmetic to hoist into brif preheader (imul_imm={imul_count}):\n{fast_body}"
    );
}

#[test]
fn narrow_binary_ops_fold_through_ireduce() {
    let source = "fn spread(v: u32) -> u32 {
    let mut x = v & 1023;
    x = (x | (x << 16)) & 50331903;
    x = (x | (x << 8)) & 50393103;
    x
}

fn main() -> u64 {
    widen_u64(spread(341))
}";
    let c = accepted(source);
    let (_, clif) = native::object(&c.world, "main").unwrap();
    assert_eq!(agree(source), "16797701");
    let fn_clif = clif
        .split("\n; ")
        .find(|s| s.starts_with("spread\n"))
        .unwrap_or(&clif);
    let ireduce_count = fn_clif.matches("ireduce.i32").count();
    assert!(
        ireduce_count == 1,
        "expected uextend->band/ishl/bor->ireduce.i32 chain to fold to native i32 ops (ireduce_count={ireduce_count}):\n{fn_clif}"
    );
}

#[test]
fn narrow_shifts_fold_through_ireduce() {
    let source = "fn compact(v: u32) -> u32 {
    let mut x = v & 153498185;
    x = (x | (x >> 2)) & 51130563;
    x = (x | (x >> 4)) & 50393103;
    x
}

fn main() -> u64 {
    widen_u64(compact(17891665))
}";
    let c = accepted(source);
    let (_, clif) = native::object(&c.world, "main").unwrap();
    let eval = vesper::runtime::Runtime::new(&c.world)
        .run("main", vec![])
        .unwrap();
    let jit = native::jit(&c.world, "main").unwrap();
    assert_eq!(eval.to_string(), jit.to_string());
    let fn_clif = clif
        .split("\n; ")
        .find(|s| s.starts_with("compact\n"))
        .unwrap_or(&clif);
    let ireduce_count = fn_clif.matches("ireduce.i32").count();
    assert!(
        ireduce_count == 1,
        "expected uextend->ushr->ireduce.i32 chain to fold to native i32 ops (ireduce_count={ireduce_count}):\n{fn_clif}"
    );
}

#[test]
fn pure_shift_diamond_if_converts_to_select() {
    let source = "
fn safe_shl(x: u64, s: u32) -> u64 {
    if s < 64 {
        wrapping_shl_u64(x, s)
    } else {
        0
    }
}

fn main() -> u64 {
    wrapping_add(safe_shl(3, 5), safe_shl(3, 70))
}";
    let c = accepted(source);
    let (_, clif) = native::object(&c.world, "main").unwrap();
    let eval = vesper::runtime::Runtime::new(&c.world)
        .run("main", vec![])
        .unwrap();
    let jit = native::jit(&c.world, "main").unwrap();
    assert_eq!(eval.to_string(), jit.to_string());
    let fn_clif = clif
        .split("\n; ")
        .find(|s| s.starts_with("safe_shl\n"))
        .unwrap_or(&clif);
    assert!(
        fn_clif.contains("select ") && !fn_clif.contains("brif "),
        "expected pure shift diamond to if-convert to branchless select:\n{fn_clif}"
    );
}

#[test]
fn seq_header_loads_reuse_across_try_set() {
    let source = "
struct Bag {
    s: ChunkSeq[u64],
}

fn fill_and_sum(inout b: Bag, n: u64) -> u64 {
    let mut i: u64 = 0;
    let mut acc: u64 = 0;
    loop {
        if i >= n {
            break;
        }
        let _ = b.s.try_set(i, wrapping_mul(i, 3));
        let v = match b.s.try_get(i) {
            Option::Some(x) => x,
            Option::None => 0,
        };
        acc = wrapping_add(acc, v);
        i = wrapping_add(i, 1);
    }
    acc
}

fn make_bag() -> Bag {
    let mut s: ChunkSeq[u64] = ChunkSeq::new();
    s.push(0);
    s.push(0);
    s.push(0);
    s.push(0);
    s.push(0);
    s.push(0);
    Bag { s: s }
}

fn main() -> u64 {
    let mut b = make_bag();
    let a = fill_and_sum(inout b, 6);
    let c = fill_and_sum(inout b, 4);
    wrapping_add(a, c)
}";
    let c = accepted(source);
    let (_, clif) = native::object(&c.world, "main").unwrap();
    let eval = vesper::runtime::Runtime::new(&c.world)
        .run("main", vec![])
        .unwrap();
    let jit = native::jit(&c.world, "main").unwrap();
    assert_eq!(eval.to_string(), jit.to_string());
    let fn_clif = clif
        .split("\n; ")
        .find(|s| s.starts_with("main\n"))
        .unwrap_or(&clif);
    let try_set_blocks: Vec<&str> = fn_clif
        .split("\n\n")
        .filter(|blk| {
            blk.contains("stack_addr.i64") && blk.contains("select ") && blk.contains("store ")
        })
        .collect();
    assert!(
        !try_set_blocks.is_empty()
            && try_set_blocks.iter().all(|blk| !blk
                .split("store ")
                .nth(1)
                .unwrap_or("")
                .contains("load.i64")),
        "expected SeqHeader/Param loads to hoist and reuse across try_set:\n{fn_clif}"
    );
}

#[test]
fn constant_param_jumps_thread_and_branch_conditions_propagate() {
    let source = "
fn classify(x: u64, inout s: ChunkSeq[u64]) -> u64 {
    let tag = if x > 10 {
        s.push(x);
        1
    } else {
        0
    };
    if tag == 1 {
        s.push(42);
        42
    } else {
        7
    }
}

fn main() -> u64 {
    let mut s1: ChunkSeq[u64] = ChunkSeq::new();
    let mut s2: ChunkSeq[u64] = ChunkSeq::new();
    wrapping_add(classify(20, inout s1), classify(3, inout s2))
}";
    let c = accepted(source);
    let (_, clif) = native::object(&c.world, "main").unwrap();
    let eval = vesper::runtime::Runtime::new(&c.world)
        .run("main", vec![])
        .unwrap();
    let jit = native::jit(&c.world, "main").unwrap();
    assert_eq!(eval.to_string(), jit.to_string());
    let fn_clif = clif
        .split("\n; ")
        .find(|s| s.starts_with("classify\n"))
        .unwrap_or(&clif);
    assert!(
        !fn_clif.contains("icmp_imm eq") && !fn_clif.contains("icmp eq"),
        "expected `tag == 1` branch to thread away:\n{fn_clif}"
    );
}
