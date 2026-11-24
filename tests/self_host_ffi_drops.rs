//! Self-hosting SH4 lane 13 (#1287 / #1714): `@vesper/ffi-drops`
//! (`packages/vesper-ffi-lowering/drops`) against `src/ffi_lowering/drops.rs`
//! (`insert_drops`, `check_rows`, `DropCallees`).
#![allow(dead_code)]

#[path = "common/check_harness.rs"]
mod check_harness;
#[path = "common/check_words.rs"]
mod check_words;
#[path = "common/ir_words.rs"]
mod ir_words;

use check_harness::*;
use check_words::Enc;
use ir_words::Tables;
use std::collections::BTreeSet;
use std::sync::OnceLock;
use vesper::check::World;
use vesper::ffi_lowering::drops::{DROP_PREFIX, DropCallees, insert_drops};
use vesper::ffi_lowering::{self, Handler, Imports, plan_probe};
use vesper::fip::{FipDrop, HandleDrops, handle_drop_plan};
use vesper::ir::{Program, Terminator, lower_world};

fn drops_entry() -> &'static vesper::native::WordFunction {
    static F: OnceLock<vesper::native::WordFunction> = OnceLock::new();
    F.get_or_init(|| {
        Package::load("packages/vesper-ffi-lowering/drops")
            .entry("ffi_drops_entry", false)
            .0
    })
}

const VSYS_RES: &str = r#"sys library Res (
    target = "x86_64-unknown-linux-gnu", abi = "sysv64",
    link = "static", name = "vesper_res", artifact = "blake3:0000000000000000000000000000000000000000000000000000000000000000"
) {
    affine extern type Probe drop res_release;
    fn res_open(n: u64) -> ForeignVal[Probe];
    fn res_maybe(n: u64) -> ForeignVal[Option[Probe]];
    fn res_read(in p: Probe) -> ForeignVal[u64];
    fn res_close(sink p: Probe) -> ForeignVal[u64];
    fn res_release(sink p: Probe) -> ForeignVal[Unit];
}
"#;

fn make_imports() -> Imports {
    let mut imports = Imports::new();
    imports.add(VSYS_RES, Handler::Direct).expect("insert Res");
    imports
}

fn encode_drop_item(w: &mut Vec<u64>, enc: &mut Enc, d: &FipDrop) {
    match d {
        FipDrop::Value(v, ty) => {
            w.push(0);
            w.push(*v as u64);
            let ti = enc.ty(ty);
            w.push(ti);
        }
        FipDrop::Place(place, ty) => {
            w.push(1);
            let ri = enc.name(&place.root);
            w.push(ri);
            w.push(place.path.len() as u64);
            for seg in &place.path {
                let si = enc.name(seg);
                w.push(si);
            }
            let ti = enc.ty(ty);
            w.push(ti);
        }
        FipDrop::OtherFields { ty, keep, .. } => {
            w.push(2);
            let ti = enc.ty(ty);
            w.push(ti);
            let ki = enc.name(keep);
            w.push(ki);
        }
    }
}

fn prepare_input(
    world: &World,
    program: &Program,
    mode: u64,
    queries: &[String],
) -> (Vec<u8>, ir_words::Pool) {
    let mut t = Tables::of(program);
    t.label_names();
    let section = t.section();
    let world_words = t.enc.world(world);
    let mut body: Vec<u64> = vec![];
    body.push(world.functions.len() as u64);
    for (name, f) in &world.functions {
        let ni = t.enc.name(name);
        body.push(ni);
        body.push(u64::from(f.fip));
        body.push(f.span.start as u64);
        body.push(f.span.end as u64);
    }
    body.push(mode);
    if mode == 1 {
        body.push(queries.len() as u64);
        for q in queries {
            let qi = t.enc.name(q);
            body.push(qi);
        }
    } else {
        let mut handles = BTreeSet::new();
        for name in world.functions.keys() {
            if let Some(h) = name.strip_prefix(DROP_PREFIX)
                && !h.contains('$')
            {
                handles.insert(h.to_owned());
            }
        }
        type PlanEntry = (usize, Option<(Vec<(usize, usize)>, HandleDrops)>);
        let mut plans: Vec<PlanEntry> = vec![];
        for (fi, (_, f)) in program.functions.iter().enumerate() {
            if f.name.starts_with(DROP_PREFIX)
                || world.functions.get(&f.name).is_some_and(|src| src.fip)
            {
                continue;
            }
            let mut fc = f.clone();
            let orig_blocks = fc.blocks.len();
            let plan = handle_drop_plan(world, &mut fc, &handles).map(|p| {
                let mut splits = vec![];
                for blk in &fc.blocks[orig_blocks..] {
                    let from = blk.incoming_tokens[0].0;
                    let to = match blk.terminator {
                        Terminator::Jump { block, .. } => block,
                        _ => unreachable!(),
                    };
                    splits.push((from, to));
                }
                (splits, p)
            });
            plans.push((fi, plan));
        }
        body.push(plans.len() as u64);
        for (fi, plan) in plans {
            body.push(fi as u64);
            match plan {
                None => body.push(0),
                Some((splits, p)) => {
                    body.push(1);
                    body.push(splits.len() as u64);
                    for (from, to) in splits {
                        body.push(from as u64);
                        body.push(to as u64);
                    }
                    let bi_count: usize = p.before_inst.values().map(Vec::len).sum();
                    body.push(bi_count as u64);
                    for (blk, items) in &p.before_inst {
                        for (at, d) in items {
                            body.push(*blk as u64);
                            body.push(*at as u64);
                            encode_drop_item(&mut body, &mut t.enc, d);
                        }
                    }
                    let ai_count: usize = p.after_inst.values().map(Vec::len).sum();
                    body.push(ai_count as u64);
                    for (blk, items) in &p.after_inst {
                        for (at, d) in items {
                            body.push(*blk as u64);
                            body.push(*at as u64);
                            encode_drop_item(&mut body, &mut t.enc, d);
                        }
                    }
                    let bt_count: usize = p.before_term.values().map(Vec::len).sum();
                    body.push(bt_count as u64);
                    for (blk, items) in &p.before_term {
                        for d in items {
                            body.push(*blk as u64);
                            encode_drop_item(&mut body, &mut t.enc, d);
                        }
                    }
                }
            }
        }
    }
    let mut input = t.enc.finish(&world_words, &section);
    for w in body {
        input.extend_from_slice(&w.to_le_bytes());
    }
    (input, t.pool)
}

fn call_drops(input: &[u8]) -> Vec<u64> {
    let f = drops_entry();
    let arg = [input.len() as u64, 0, input.as_ptr() as u64];
    let ptr = unsafe { f.call(&[arg.as_ptr() as u64]) }.expect("ffi_drops_entry");
    take_words(ptr)
}

fn check_case(label: &str, source: &str) {
    let imports = make_imports();
    let fc =
        ffi_lowering::check_source(source, &imports).unwrap_or_else(|e| panic!("{label}: {e}"));
    let world = fc.world();
    let mut rust_prog = lower_world(world);
    let base_prog = rust_prog.clone();
    insert_drops(world, &mut rust_prog);

    let (input, pool) = prepare_input(world, &base_prog, 0, &[]);
    let words = call_drops(&input);
    assert_eq!(
        words[0],
        0,
        "{label}: expected status 0, got {}",
        unpack_text(&words)
    );
    let vesper_prog = ir_words::decode_program(&words[1..], &pool);
    let r_dump = ir_words::dump(&rust_prog);
    let v_dump = ir_words::dump(&vesper_prog);
    assert_eq!(r_dump, v_dump, "{label}: program mismatch");

    let queries: Vec<String> = world.functions.keys().cloned().collect();
    let (q_in, _) = prepare_input(world, &rust_prog, 1, &queries);
    let q_words = call_drops(&q_in);
    let v_callees = unpack_text(&q_words);
    let dc = DropCallees::new(&rust_prog);
    let mut r_callees = String::new();
    for q in &queries {
        let cs = dc.of(world, q);
        r_callees.push_str(q);
        r_callees.push_str(": ");
        r_callees.push_str(&cs.join(" "));
        r_callees.push('\n');
    }
    assert_eq!(r_callees, v_callees, "{label}: DropCallees mismatch");
}

#[test]
fn drops_insert_and_callees_match_rust() {
    let ok_source = r#"
affine struct Holder {
    p: Probe,
    n: u64,
}

fn open(n: u64) -> Probe eff [Ffi[Res]] {
    match Res::res_open(n) {
        Ok(p) => p,
        Err(_) => loop {},
    }
}

pub fn scope() -> u64 eff [Ffi[Res]] {
    let p = open(1);
    Res::res_read(in p)
}

pub fn wildcard() -> u64 eff [Ffi[Res]] {
    let _ = open(2);
    1
}

pub fn opt_drop(flag: bool) -> u64 eff [Ffi[Res]] {
    let o = if flag {
        Option::Some(open(3))
    } else {
        Option::None
    };
    0
}

pub fn branch_split(flag: bool) -> u64 eff [Ffi[Res]] {
    let p = open(4);
    if flag {
        Res::res_close(p)
    } else {
        10
    }
}

pub fn loop_drop() -> u64 eff [Ffi[Res]] {
    let mut sum: u64 = 0;
    for i in 0..3 {
        let p = open(i);
        sum = wrapping_add(sum, Res::res_read(in p));
    }
    sum
}
"#;
    check_case("affine_drops_ok", ok_source);
}

#[test]
fn drops_check_rows_and_refusals_match_rust() {
    let imports = make_imports();
    // 1. Missing Ffi[Res] -> E0401
    let missing_eff = r#"
fn open_it(n: u64) -> Probe eff [Ffi[Res]] {
    match Res::res_open(n) {
        Ok(p) => p,
        Err(_) => loop {},
    }
}
pub fn bad(sink p: Probe) -> u64 {
    1
}
"#;
    let err = ffi_lowering::check_source(missing_eff, &imports).unwrap_err();
    assert!(!err.diagnostics.is_empty(), "expected E0401");
    let expected_msg = &err.diagnostics[0].message;

    let parsed = vesper::parser::parse(missing_eff);
    let r#gen = plan_probe::generated(&imports).expect("gen");
    let mut m1 = parsed.module.clone();
    plan_probe::append_drops(&mut m1, &imports);
    let _ = plan_probe::rewrite(&mut m1, &imports, &r#gen, None);
    let mut m_decls = m1.clone();
    m_decls.items.extend(r#gen.declarations(None));
    let arities = {
        let chk = vesper::check::check(&m_decls);
        assert!(chk.diagnostics.is_empty(), "{:?}", chk.diagnostics);
        plan_probe::arities(&parsed.module, &imports, &r#gen).unwrap_or_default()
    };
    let mut m2 = parsed.module;
    plan_probe::append_drops(&mut m2, &imports);
    let r2 = plan_probe::rewrite(&mut m2, &imports, &r#gen, Some(&arities));
    m2.items.extend(r#gen.declarations(Some(&r2.instances)));
    let chk = vesper::check::check(&m2);
    assert!(chk.diagnostics.is_empty(), "{:?}", chk.diagnostics);

    let rust_err = plan_probe::check_drop_rows(&chk.world).unwrap_err();
    assert_eq!(rust_err.diagnostics[0].message, *expected_msg);

    let prog = lower_world(&chk.world);
    let (input, _) = prepare_input(&chk.world, &prog, 0, &[]);
    let words = call_drops(&input);
    let vesper_out = unpack_text(&words);
    assert!(
        vesper_out.contains("E0401") && vesper_out.contains(expected_msg),
        "expected E0401 with {expected_msg:?}, got {vesper_out:?}"
    );

    // 2. Refusal cases: wildcard in pattern, closure move, OtherFields, non-handle struct.
    for (label, src) in [
        (
            "wild_in_tuple",
            r#"
fn open_it(n: u64) -> Probe eff [Ffi[Res]] {
    match Res::res_open(n) { Ok(p) => p, Err(_) => loop {} }
}
pub fn bad() -> u64 eff [Ffi[Res]] {
    let (_, n): (Probe, u64) = (open_it(1), 2);
    n
}
"#,
        ),
        (
            "closure_move",
            r#"
fn open_it(n: u64) -> Probe eff [Ffi[Res]] {
    match Res::res_open(n) { Ok(p) => p, Err(_) => loop {} }
}
pub fn bad() -> u64 eff [Ffi[Res]] {
    let p = open_it(1);
    let _f = || -> u64 eff [Ffi[Res]] { Res::res_read(in p) };
    0
}
"#,
        ),
        (
            "other_fields",
            r#"
affine struct Holder { p: Probe, n: u64 }
fn open_it(n: u64) -> Probe eff [Ffi[Res]] {
    match Res::res_open(n) { Ok(p) => p, Err(_) => loop {} }
}
fn make_holder() -> Holder eff [Ffi[Res]] {
    Holder { p: open_it(1), n: 2 }
}
pub fn bad() -> u64 eff [Ffi[Res]] {
    make_holder().n
}
"#,
        ),
    ] {
        let e = ffi_lowering::check_source(src, &imports).unwrap_err();
        let parsed = vesper::parser::parse(src);
        let r#gen = plan_probe::generated(&imports).expect("gen");
        let mut m1 = parsed.module.clone();
        plan_probe::append_drops(&mut m1, &imports);
        let _ = plan_probe::rewrite(&mut m1, &imports, &r#gen, None);
        let mut m_decls = m1.clone();
        m_decls.items.extend(r#gen.declarations(None));
        let arities = {
            let chk = vesper::check::check(&m_decls);
            assert!(chk.diagnostics.is_empty(), "{label}: {:?}", chk.diagnostics);
            plan_probe::arities(&parsed.module, &imports, &r#gen).unwrap_or_default()
        };
        let mut m2 = parsed.module;
        plan_probe::append_drops(&mut m2, &imports);
        let r2 = plan_probe::rewrite(&mut m2, &imports, &r#gen, Some(&arities));
        m2.items.extend(r#gen.declarations(Some(&r2.instances)));
        let chk = vesper::check::check(&m2);
        assert!(chk.diagnostics.is_empty(), "{label}: {:?}", chk.diagnostics);
        let prog = lower_world(&chk.world);
        let (input, _) = prepare_input(&chk.world, &prog, 0, &[]);
        let words = call_drops(&input);
        let vesper_out = unpack_text(&words);
        assert_eq!(
            vesper_out,
            format!("ERR {}", e.message),
            "{label}: refusal message mismatch"
        );
    }
}
