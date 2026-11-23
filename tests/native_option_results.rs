//! Regression tests for G10 (#1498): two-word Option/Result register returns,
//! `seq_option` direct SSA delivery, and multi-hop `split_join_slots`.
use vesper::{check, native, parser, runtime};

fn check_world(source: &str) -> vesper::check::World {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    let c = check::check(&p.module);
    assert!(c.diagnostics.is_empty(), "{:#?}", c.diagnostics);
    c.world
}

fn clif_function<'a>(clif: &'a str, name: &str) -> &'a str {
    let marker = format!("; {name}\n");
    let start = clif
        .find(&marker)
        .unwrap_or_else(|| panic!("missing function '{name}' in CLIF:\n{clif}"));
    let rest = &clif[start + marker.len()..];
    match rest.find("\n; ") {
        Some(end) => &rest[..end],
        None => rest,
    }
}

#[test]
fn small_option_and_result_calls_return_in_registers_without_stack_slots() {
    let source = "copy enum BigErr { Small(u64), Pair(u64, u64) }

fn opt_u64_big(x: u64) -> Option[u64] {
    let mut acc: u64 = x;
    for i in 0..6 {
        acc = wrapping_add(acc, i);
    }
    if (acc & 1) == 0 {
        Option::Some(wrapping_add(acc, 10))
    } else {
        Option::None
    }
}

fn read_u32_big(in b: ChunkSeq[u8], off: u64) -> Option[u32] {
    let mut sum: u32 = 0;
    for i in 0..6 {
        let idx = wrapping_add(off, i);
        match b.try_read_u32_le(idx) {
            Option::Some(v) => { sum = wrapping_add(sum, v); }
            Option::None => {}
        }
    }
    if sum > 0 {
        Option::Some(sum)
    } else {
        Option::None
    }
}

fn res_three_big(x: u64) -> Result[u64, BigErr] {
    let mut acc: u64 = x;
    for i in 0..4 {
        acc = wrapping_add(acc, wrapping_mul(i, 5));
    }
    if acc == 0 {
        Result::Err(BigErr::Small(1))
    } else if acc < 10 {
        Result::Err(BigErr::Pair(acc, 7))
    } else {
        Result::Ok(wrapping_add(acc, 3))
    }
}

fn try_get_loop(in d: ChunkSeq[u64], n: u64) -> Option[u64] {
    let mut sum: u64 = 0;
    for i in 0..n {
        match d.try_get(i) {
            Option::Some(v) => { sum = wrapping_add(sum, v); }
            Option::None => { return Option::None; }
        }
    }
    Option::Some(sum)
}

fn main() -> u64 {
    let mut b: ChunkSeq[u8] = ChunkSeq::new[u8]();
    for i in 0..32 { b.push(1); }
    let mut d: ChunkSeq[u64] = ChunkSeq::new[u64]();
    for i in 1..9 { d.push(i); }
    let mut out: u64 = 0;
    for k in 0..10 {
        let a = match opt_u64_big(k) { Option::Some(v) => v, Option::None => 7 };
        let c = match read_u32_big(b, k) { Option::Some(v) => widen_u64(v), Option::None => 11 };
        let e = match res_three_big(k) {
            Result::Ok(v) => v,
            Result::Err(err) => match err {
                BigErr::Small(z) => z,
                BigErr::Pair(p, q) => wrapping_add(p, q),
            },
        };
        let f = match try_get_loop(d, k) { Option::Some(v) => v, Option::None => 13 };
        out = wrapping_add(out, wrapping_add(wrapping_add(a, c), wrapping_add(e, f)));
    }
    out
}";
    let world = check_world(source);
    let eval = runtime::Runtime::new(&world).run("main", vec![]).unwrap();
    let jit_opt = native::jit_with_options(&world, "main", true).unwrap();
    let jit_noopt = native::jit_with_options(&world, "main", false).unwrap();
    assert_eq!(eval.to_string(), jit_opt.to_string());
    assert_eq!(eval.to_string(), jit_noopt.to_string());

    for target in ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"] {
        let (_, clif) = native::object_with_options(&world, "main", Some(target), true).unwrap();
        for two_word_fn in ["opt_u64_big", "read_u32_big", "try_get_loop"] {
            let body = clif_function(&clif, two_word_fn);
            let header = body.lines().next().unwrap();
            assert!(
                header.contains("-> i64, i64"),
                "{target}: expected 2-register return for {two_word_fn}:\n{header}"
            );
            let has_nonzero_slot = body.lines().any(|line| {
                let t = line.trim();
                t.contains("explicit_slot ") && !t.contains("explicit_slot 0,")
            });
            assert!(
                !has_nonzero_slot
                    && !body.contains("stack_addr")
                    && !body.contains("stack_load")
                    && !body.contains("stack_store"),
                "{target}: unexpected stack slot traffic in {two_word_fn}:\n{body}"
            );
        }
        let three_body = clif_function(&clif, "res_three_big");
        let three_nonzero_slot = three_body.lines().any(|line| {
            let t = line.trim();
            t.contains("explicit_slot ") && !t.contains("explicit_slot 0,")
        });
        assert!(
            !three_nonzero_slot && !three_body.contains("stack_addr"),
            "{target}: expected split_join_slots to eliminate local stack slot in res_three_big:\n{three_body}"
        );
    }
}
