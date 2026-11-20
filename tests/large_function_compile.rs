//! Compile time and code shape of large functions (self-hosting SH1 lane 8,
//! #1151).
//!
//! A self-hosted compiler has large generated functions: tables, big
//! `match`es, unrolled kernels. These inputs are the repros of
//! docs/parallel-results/self-host-large-functions.md:
//!
//! - a 2,000-arm `match` on an integer (`--no-opt` took 290 s to JIT in a
//!   release compiler: every block read every value of the graph, and blocks
//!   stayed unsealed until the end, so each got a parameter per value);
//! - the BLAKE3 compression written straight-line over 16 `u32` locals, as
//!   `packages/vesper-std/hash/gen/blake3_gen.py --straight-line` writes it,
//!   with and without the state stored to the output slice and reloaded
//!   after every half round (the "spill" shape, 18 s with `--no-opt`);
//! - the same function's optimized code, which rebuilt the values shared
//!   between its 16 `try_set` stores once per store (224 rotates in the
//!   source, 3,352 in the optimized function).
//!
//! Every program runs in the JIT with and without `optimize` (and the small
//! ones in the evaluator), and must return the result a Rust reference
//! computes. The time budgets are generous for debug builds on slow CI
//! hosts; the regressions they catch were minutes long.
use std::time::{Duration, Instant};
use vesper::{check, native, parser, runtime::Runtime};

const PERM: [usize; 16] = [2, 6, 3, 10, 7, 0, 4, 13, 1, 11, 12, 5, 9, 14, 15, 8];
const G: [(usize, usize, usize, usize); 8] = [
    (0, 4, 8, 12),
    (1, 5, 9, 13),
    (2, 6, 10, 14),
    (3, 7, 11, 15),
    (0, 5, 10, 15),
    (1, 6, 11, 12),
    (2, 7, 8, 13),
    (3, 4, 9, 14),
];
const IV: [u32; 4] = [0x6A09E667, 0xBB67AE85, 0x3C6EF372, 0xA54FF53A];

/// The driver both BLAKE3 shapes share: `iters` compressions, each chaining
/// value the previous output's first 8 words; returns output word 3.
fn blake3_main(iters: u64) -> String {
    format!(
        "
fn main() -> u64 {{
    let mut cv: ChunkSeq[u32] = ChunkSeq::with_capacity[u32](8);
    let mut m: ChunkSeq[u32] = ChunkSeq::with_capacity[u32](16);
    let mut out: ChunkSeq[u32] = ChunkSeq::with_capacity[u32](16);
    cv.resize(8, 1);
    m.resize(16, 7);
    out.resize(16, 0);
    let mut i: u64 = 0;
    loop {{
        if i >= {iters} {{ break; }}
        compress(in cv.as_slice(), in m.as_slice(), i, 64, 11, inout out.as_slice_mut());
        cv.try_copy_from(0, in out[0..8]);
        i = wrapping_add(i, 1);
    }}
    match out.try_get(3) {{ Option::Some(x) => wrapping_u64(x), Option::None => 0 }}
}}
"
    )
}

/// The straight-line compression; `spill` stores the state to `out` and
/// reloads it after every half round.
fn blake3_source(spill: bool, iters: u64) -> String {
    let mut s = String::from(
        "fn word(in m: Slice[u32], i: u64) -> u32 {
    match m.try_get(i) {
        Option::Some(x) => x,
        Option::None => 0,
    }
}
pub fn compress(in cv: Slice[u32], in m: Slice[u32], counter: u64, block_len: u32, flags: u32, inout out: Slice[u32]) {
",
    );
    for i in 0..8 {
        s += &format!("    let c{i} = word(in cv, {i});\n");
    }
    for i in 0..16 {
        s += &format!("    let m{i} = word(in m, {i});\n");
    }
    for i in 0..8 {
        s += &format!("    let mut s{i} = c{i};\n");
    }
    for (i, iv) in IV.iter().enumerate() {
        s += &format!("    let mut s{}: u32 = {iv};\n", 8 + i);
    }
    s += "    let mut s12 = wrapping_u32(counter);
    let mut s13 = wrapping_u32(counter >> 32);
    let mut s14 = block_len;
    let mut s15 = flags;
";
    let mut order: Vec<usize> = (0..16).collect();
    for _ in 0..7 {
        for (gi, &(a, b, c, d)) in G.iter().enumerate() {
            let (x, y) = (order[2 * gi], order[2 * gi + 1]);
            s += &format!(
                "    s{a} = wrapping_add(wrapping_add(s{a}, s{b}), m{x});
    s{d} = rotate_right(s{d} ^ s{a}, 16);
    s{c} = wrapping_add(s{c}, s{d});
    s{b} = rotate_right(s{b} ^ s{c}, 12);
    s{a} = wrapping_add(wrapping_add(s{a}, s{b}), m{y});
    s{d} = rotate_right(s{d} ^ s{a}, 8);
    s{c} = wrapping_add(s{c}, s{d});
    s{b} = rotate_right(s{b} ^ s{c}, 7);
"
            );
            if spill && gi % 4 == 3 {
                for k in 0..16 {
                    s += &format!("    out.try_set({k}, s{k});\n");
                }
                for k in 0..16 {
                    s += &format!("    s{k} = word(in out, {k});\n");
                }
            }
        }
        order = PERM.iter().map(|&p| order[p]).collect();
    }
    for i in 0..8 {
        s += &format!("    out.try_set({i}, s{i} ^ s{});\n", i + 8);
    }
    for i in 0..8 {
        s += &format!("    out.try_set({}, s{} ^ c{i});\n", i + 8, i + 8);
    }
    s += "}\n";
    s + &blake3_main(iters)
}

/// The BLAKE3 compression (reference implementation) on the driver's inputs.
fn blake3_reference(iters: u64) -> u64 {
    let mut cv = [1u32; 8];
    let m = [7u32; 16];
    let mut out = [0u32; 16];
    for counter in 0..iters {
        let mut s = [0u32; 16];
        s[..8].copy_from_slice(&cv);
        s[8..12].copy_from_slice(&IV);
        s[12] = counter as u32;
        s[13] = (counter >> 32) as u32;
        s[14] = 64;
        s[15] = 11;
        let mut order: Vec<usize> = (0..16).collect();
        for _ in 0..7 {
            for (gi, &(a, b, c, d)) in G.iter().enumerate() {
                let (x, y) = (m[order[2 * gi]], m[order[2 * gi + 1]]);
                s[a] = s[a].wrapping_add(s[b]).wrapping_add(x);
                s[d] = (s[d] ^ s[a]).rotate_right(16);
                s[c] = s[c].wrapping_add(s[d]);
                s[b] = (s[b] ^ s[c]).rotate_right(12);
                s[a] = s[a].wrapping_add(s[b]).wrapping_add(y);
                s[d] = (s[d] ^ s[a]).rotate_right(8);
                s[c] = s[c].wrapping_add(s[d]);
                s[b] = (s[b] ^ s[c]).rotate_right(7);
            }
            order = PERM.iter().map(|&p| order[p]).collect();
        }
        for i in 0..8 {
            out[i] = s[i] ^ s[i + 8];
            out[i + 8] = s[i + 8] ^ cv[i];
        }
        cv.copy_from_slice(&out[..8]);
    }
    u64::from(out[3])
}

fn arm_value(i: u64) -> u64 {
    i * 2654435761 % 1000003
}

/// `pick`, an `arms`-arm `match` on a `u64`, summed over `iters` calls.
fn match_source(arms: u64, iters: u64) -> String {
    let mut s = String::from("fn pick(x: u64) -> u64 {\n    match x {\n");
    for i in 0..arms {
        s += &format!("        {i} => {},\n", arm_value(i));
    }
    s += "        _ => 0,\n    }\n}\n";
    s += &format!(
        "
fn main() -> u64 {{
    let mut i: u64 = 0;
    let mut acc: u64 = 0;
    loop {{
        if i >= {iters} {{ break; }}
        acc = wrapping_add(acc, pick(i % {}));
        i = wrapping_add(i, 1);
    }}
    acc
}}
",
        arms + 1
    );
    s
}

fn match_reference(arms: u64, iters: u64) -> u64 {
    (0..iters)
        .map(|i| {
            let x = i % (arms + 1);
            if x < arms { arm_value(x) } else { 0 }
        })
        .fold(0u64, u64::wrapping_add)
}

fn accepted(source: &str) -> check::Checked {
    let limits = parser::ParseLimits {
        max_tokens: vesper::lexer::HARD_TOKENS,
        ..Default::default()
    };
    let p = parser::parse_with_limits(source, limits).unwrap();
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    let c = check::check(&p.module);
    assert!(c.diagnostics.is_empty(), "{:#?}", c.diagnostics);
    c
}

/// JIT-compiles and runs `main`, failing past `budget`.
/// The result is printed as `vesper jit` prints it.
fn jit(c: &check::Checked, optimize: bool, budget: Duration, what: &str) -> String {
    let start = Instant::now();
    let value = native::jit_with_options(&c.world, "main", optimize)
        .unwrap()
        .to_string();
    let took = start.elapsed();
    assert!(
        took < budget,
        "{what} (optimize {optimize}) took {took:?} to compile and run; budget {budget:?}"
    );
    value
}

#[test]
fn unoptimized_two_thousand_arm_match_compiles_quickly() {
    let c = accepted(&match_source(2000, 10_000));
    let expected = match_reference(2000, 10_000).to_string();
    assert_eq!(
        jit(&c, false, Duration::from_secs(60), "2,000-arm match"),
        expected
    );
}

#[test]
fn two_thousand_arm_match_agrees_optimized() {
    let c = accepted(&match_source(2000, 10_000));
    let expected = match_reference(2000, 10_000).to_string();
    assert_eq!(
        jit(&c, true, Duration::from_secs(300), "2,000-arm match"),
        expected
    );
}

#[test]
fn unoptimized_straight_line_blake3_compiles_quickly() {
    for spill in [false, true] {
        let c = accepted(&blake3_source(spill, 100));
        let expected = blake3_reference(100).to_string();
        assert_eq!(
            jit(
                &c,
                false,
                Duration::from_secs(60),
                &format!("straight-line BLAKE3 (spill {spill})")
            ),
            expected
        );
    }
}

#[test]
fn optimized_straight_line_blake3_agrees() {
    // The optimized spill shape spends most of its time in the guarded
    // Place IR optimizer (`ir_opt::optimize`); see the report.
    for spill in [false, true] {
        let c = accepted(&blake3_source(spill, 100));
        let expected = blake3_reference(100).to_string();
        assert_eq!(
            jit(
                &c,
                true,
                Duration::from_secs(600),
                &format!("straight-line BLAKE3 (spill {spill})")
            ),
            expected
        );
    }
}

#[test]
fn small_programs_agree_with_the_evaluator() {
    for source in [
        blake3_source(false, 2),
        blake3_source(true, 2),
        match_source(300, 700),
    ] {
        let c = accepted(&source);
        let mut runtime = Runtime::new(&c.world);
        runtime.optimize = false;
        let evaluated = runtime.run("main", vec![]).unwrap().to_string();
        for optimize in [false, true] {
            assert_eq!(
                native::jit_with_options(&c.world, "main", optimize)
                    .unwrap()
                    .to_string(),
                evaluated,
                "optimize {optimize}"
            );
        }
    }
}

/// Rotates in `compress` after Cranelift's optimizer (`opt_level=speed`)
/// for the host ISA.
fn optimized_rotates(source: &str) -> usize {
    use cranelift_codegen::ir::{InstructionData, Opcode};
    let c = accepted(source);
    let isa = native::object_isa_optimized(&c.world, None, true).unwrap();
    let mut rotates = None;
    native::object_with_observer(
        &c.world,
        "main",
        None,
        true,
        Some(&mut |f: native::EmittedFunction| {
            if f.source != "compress" {
                return;
            }
            let mut ctx = cranelift_codegen::Context::for_function(f.function);
            ctx.optimize(&*isa, &mut Default::default()).unwrap();
            let func = &ctx.func;
            let mut n = 0;
            for block in func.layout.blocks() {
                for inst in func.layout.block_insts(block) {
                    if matches!(
                        func.dfg.insts[inst],
                        InstructionData::Binary {
                            opcode: Opcode::Rotr | Opcode::Rotl,
                            ..
                        } | InstructionData::BinaryImm64 {
                            opcode: Opcode::RotrImm | Opcode::RotlImm,
                            ..
                        }
                    ) {
                        n += 1;
                    }
                }
            }
            rotates = Some(n);
        }),
    )
    .unwrap();
    rotates.expect("compress was not emitted")
}

#[test]
fn optimized_straight_line_blake3_builds_each_rotate_once() {
    // 7 rounds of 8 quarter rounds of 4 rotates. Before #1151 each of the
    // 16 stores rebuilt the values it shared with the others: 3,352.
    let source = blake3_source(false, 1);
    let rotates = optimized_rotates(&source);
    assert!(rotates <= 224, "{rotates} rotates for 224 in the source");
}
