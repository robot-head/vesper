//! Source Strata regions (design §15): `region r { ... }` blocks, region
//! allocation, the §15.2 lifetime and alias rules, and the evaluator and
//! native JIT runs of the accepted programs.
use vesper::{check, native, parser, regions, runtime::Runtime};

fn checked(source: &str) -> check::Checked {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "parse: {:?}", p.diagnostics);
    assert_eq!(p.syntax().text().to_string(), source, "lossless parse");
    check::check(&p.module)
}

fn accepted(source: &str) -> check::World {
    let c = checked(source);
    assert!(c.diagnostics.is_empty(), "check: {:#?}", c.diagnostics);
    c.world
}

/// Runs `main` with the evaluator and the native JIT and requires the same
/// result from both.
fn both(source: &str) -> String {
    let world = accepted(source);
    let mut runtime = Runtime::new(&world);
    let evaluated = runtime.run("main", vec![]).unwrap().to_string();
    let (opened, closed, open) = runtime.region_counts();
    assert_eq!(opened, closed, "every evaluator region is closed");
    assert_eq!(open, 0);
    let jit = native::jit(&world, "main").unwrap().to_string();
    assert_eq!(evaluated, jit, "evaluator and native JIT agree");
    evaluated
}

fn rejects(source: &str, code: &str) -> Vec<vesper::ast::Diagnostic> {
    let c = checked(source);
    assert!(
        c.diagnostics.iter().any(|d| d.code == code),
        "expected {code}; got {:#?}",
        c.diagnostics
    );
    c.diagnostics
}

// ----- positive programs: evaluator and native ------------------------------

#[test]
fn region_block_allocates_and_reads() {
    assert_eq!(
        both(
            "fn main() -> i64 { region r { let a = r.box(40); let b = r.box(2); wrapping_add(r.get(a), r.get(b)) } }"
        ),
        "42"
    );
}

#[test]
fn functions_allocate_in_the_callers_region_without_lifetimes() {
    let source = "fn alloc(x: i64) -> RegionBox[r, i64] eff [r: Region] { r.box(x) }
fn read(b: RegionBox[r, i64]) -> i64 eff [r: Region] { r.get(b) }
fn main() -> i64 {
    region r(4096) {
        let a = alloc(40);
        let b: RegionBox[r, i64] = r.box(2);
        wrapping_add(read(a), r.get(b))
    }
}";
    assert_eq!(both(source), "42");
}

#[test]
fn copy_boxes_alias_and_set_writes_in_place() {
    // A copy payload gives a copy handle: both names see the write (§15.2,
    // intra-region DAG aliasing).
    assert_eq!(
        both(
            "fn main() -> i64 { region r { let a = r.box(1); let b = a; r.set(b, 41); wrapping_add(r.get(a), 1) } }"
        ),
        "42"
    );
}

#[test]
fn rewind_reclaims_and_commit_keeps() {
    let source = "fn main() -> bool {
    region r {
        let keep = r.box(1);
        let before = r.used();
        let m = r.checkpoint();
        r.box(2);
        r.box(3);
        let grown = r.used();
        r.rewind(m);
        let after = r.used();
        let c = r.checkpoint();
        let kept = r.box(4);
        r.commit(c);
        grown > before && after == before && r.get(kept) == 4 && r.get(keep) == 1
    }
}";
    assert_eq!(both(source), "true");
}

#[test]
fn nested_regions_float_and_bool_payloads() {
    let source = "fn main() -> i64 {
    region outer {
        let x = outer.box(2.5);
        let flag = outer.box(true);
        region inner(512) {
            let y = inner.box(10);
            if outer.get(flag) && outer.get(x) > 2.0 { inner.get(y) } else { 0 }
        }
    }
}";
    assert_eq!(both(source), "10");
}

#[test]
fn each_recursive_activation_gets_its_own_region() {
    let source = "fn depth(n: i64) -> i64 {
    region r(256) {
        let b = r.box(n);
        if n == 0 { r.get(b) } else { wrapping_add(depth(wrapping_sub(n, 1)), r.get(b)) }
    }
}
fn main() -> i64 { depth(10) }";
    assert_eq!(both(source), "55");
}

#[test]
fn a_pure_function_may_use_a_local_scratch_region() {
    // [T-RegionScope] discharges r: `scratch` has an empty effect row.
    let source = "fn scratch(x: i64) -> i64 eff [] { region r(64) { let b = r.box(x); r.set(b, wrapping_add(r.get(b), 1)); r.get(b) } }
fn main() -> i64 eff [] { scratch(41) }";
    let world = accepted(source);
    assert!(world.functions["scratch"].effects.instances.is_empty());
    assert_eq!(both(source), "42");
}

#[test]
fn an_abort_leaves_the_region_and_the_handler_sees_it() {
    let source = "fn risky(b: bool) -> i64 eff [Fail[i64]] { if b { Fail::raise(7) } else { 1 } }
fn main() -> i64 {
    handle Fail[i64] in {
        region r { let a = r.box(1); wrapping_add(risky(true), r.get(a)) }
    } with { return x => x, raise(e, k) => e, }
}";
    let world = accepted(source);
    assert_eq!(
        Runtime::new(&world)
            .run("main", vec![])
            .unwrap()
            .to_string(),
        "7"
    );
    let (opened_before, _, _) = regions::native_runtime::counts();
    assert_eq!(native::jit(&world, "main").unwrap().to_string(), "7");
    let (opened_after, _, open) = regions::native_runtime::counts();
    assert_eq!(opened_after, opened_before + 1);
    assert_eq!(open, 0, "the JIT driver closed the aborted region");
}

#[test]
fn linear_payloads_are_moved_out_with_take() {
    // Aggregate payloads run natively too (wave eight): a linear struct is
    // stored as its native layout and moved out word by word.
    let source = "linear struct Token { v: i64 }
fn main() -> i64 { region r { let t = r.box(Token { v: 5 }); let Token { v } = r.take(t); v } }";
    assert_eq!(both(source), "5");
}

#[test]
fn region_bounds_are_enforced_at_run_time() {
    let source = "fn main() -> i64 { region r(16) { let a = r.box(1); let b = r.box(2); let c = r.box(3); wrapping_add(r.get(a), r.get(c)) } }";
    let world = accepted(source);
    let e = Runtime::new(&world).run("main", vec![]).unwrap_err();
    assert!(e.contains("region bound exhausted"), "{e}");
    let n = native::jit(&world, "main").unwrap_err();
    assert!(
        n.contains("native region fault: region bound exhausted"),
        "{n}"
    );
}

#[test]
fn native_object_output_carries_the_region_runtime() {
    // Object output defines the region runtime itself (wave eight), so it
    // needs only the C library at link time.
    let world = accepted("fn main() -> i64 { region r { let a = r.box(1); r.get(a) } }");
    let (bytes, _) = native::object(&world, "main").unwrap();
    let names = String::from_utf8_lossy(&bytes);
    assert!(names.contains("vesper_region_open"), "runtime symbols");
    assert!(names.contains("malloc"), "C library import");
}

#[test]
fn region_syntax_round_trips_and_is_contextual() {
    // `region` stays an ordinary identifier outside `region name {`.
    assert_eq!(
        both(
            "fn region(x: i64) -> i64 { x } fn two() -> i64 { let region = 2; region } fn main() -> i64 { wrapping_add(region(40), two()) }"
        ),
        "42"
    );
    let p = parser::parse("fn main() -> i64 { region r(0) { 1 } }");
    assert!(!p.diagnostics.is_empty(), "zero capacity is rejected");
    let p = parser::parse("fn main() -> i64 { region r(2000000000) { 1 } }");
    assert!(
        !p.diagnostics.is_empty(),
        "capacity above 1 GiB is rejected"
    );
}

#[test]
fn modules_without_regions_keep_their_world() {
    let world = accepted("fn main() -> i64 { 1 }");
    assert!(!world.effects.contains_key(regions::REGION_EFFECT));
}

// ----- [T-RegionScope]: nothing escapes, nothing is used after the end ------

#[test]
fn a_box_cannot_escape_its_block() {
    let d = rejects(
        "fn main() -> i64 { let b = region r { r.box(1) }; 0 }",
        "E0791",
    );
    assert!(d.iter().any(|d| d.message.contains("escapes its block")));
}

#[test]
fn escape_is_rejected_inside_tuples_watermarks_and_closures() {
    rejects(
        "fn main() -> i64 { let p = region r { (1, r.box(2)) }; 0 }",
        "E0791",
    );
    rejects(
        "fn main() -> i64 { region r { let m = r.checkpoint(); m }; 0 }",
        "E0791",
    );
    // A closure that captures the capability r escapes through its row.
    rejects(
        "fn main() -> i64 { let f = region r { || r.get(r.box(1)) }; 0 }",
        "E0791",
    );
}

#[test]
fn the_capability_is_not_nameable_after_its_block() {
    rejects(
        "fn main() -> i64 { let x = region r { 1 }; region q { r.box(1); 0 } }",
        "E0201",
    );
}

#[test]
fn signatures_cannot_name_a_region_they_do_not_receive() {
    rejects(
        "fn leak() -> RegionBox[r, i64] { 0 } fn main() -> i64 { 0 }",
        "E0797",
    );
    rejects(
        "struct Holder { b: RegionBox[r, i64] } fn main() -> i64 { 0 }",
        "E0797",
    );
    rejects(
        "fn main() -> (i64, Watermark[r]) { region r { (1, r.checkpoint()) } }",
        "E0797",
    );
}

#[test]
fn allocation_requires_the_capability_in_the_row() {
    rejects(
        "fn f() -> i64 eff [r: Region] { r.get(r.box(1)) } fn main() -> i64 { f() }",
        "E0401",
    );
}

// ----- alias rules -----------------------------------------------------------

#[test]
fn boxes_belong_to_their_region() {
    let d = rejects(
        "fn main() -> i64 { region a { region b { let x = a.box(1); b.get(x) } } }",
        "E0792",
    );
    assert!(d.iter().any(|d| d.message.contains("belongs to")));
    rejects(
        "fn main() -> i64 { region a { region b { let m = a.checkpoint(); b.rewind(m); 0 } } }",
        "E0792",
    );
}

#[test]
fn region_parameters_are_instantiated_per_call() {
    rejects(
        "fn read(b: RegionBox[r, i64]) -> i64 eff [r: Region] { r.get(b) }
fn main() -> i64 { region a { region r { let x = a.box(1); read(x) } } }",
        "E0792",
    );
}

#[test]
fn get_and_set_need_copy_payloads_and_take_needs_unique_ones() {
    rejects(
        "linear struct T { v: i64 } fn main() -> i64 { region r { let b = r.box(T { v: 1 }); let T { v } = r.get(b); v } }",
        "E0793",
    );
    rejects(
        "affine struct A { v: i64 } fn main() -> i64 { region r { let b = r.box(A { v: 1 }); r.set(b, A { v: 2 }); 0 } }",
        "E0793",
    );
    rejects(
        "fn main() -> i64 { region r { let b = r.box(1); r.take(b) } }",
        "E0794",
    );
}

#[test]
fn affine_boxes_are_unique() {
    rejects(
        "affine struct A { v: i64 } fn main() -> i64 { region r { let b = r.box(A { v: 1 }); let c = b; let A { v } = r.take(b); v } }",
        "E0303",
    );
}

#[test]
fn a_linear_payload_must_be_taken_before_the_region_ends() {
    rejects(
        "linear struct T { v: i64 } fn main() -> i64 { region r { let t = r.box(T { v: 5 }); 1 } }",
        "E0302",
    );
}

#[test]
fn rewind_is_rejected_while_a_linear_payload_is_live() {
    rejects(
        "linear struct T { v: i64 } fn main() -> i64 { region r { let b = r.box(T { v: 1 }); let m = r.checkpoint(); r.rewind(m); let T { v } = r.take(b); v } }",
        "E0795",
    );
}

#[test]
fn watermarks_are_linear() {
    rejects(
        "fn main() -> i64 { region r { let m = r.checkpoint(); 1 } }",
        "E0302",
    );
    rejects(
        "fn main() -> i64 { region r { let m = r.checkpoint(); r.rewind(m); r.rewind(m); 1 } }",
        "E0303",
    );
}

#[test]
fn a_box_is_dead_after_a_rewind_past_its_allocation() {
    let d = rejects(
        "fn main() -> i64 { region r { let m = r.checkpoint(); let t = r.box(7); r.rewind(m); r.get(t) } }",
        "E0796",
    );
    assert!(d.iter().any(|d| !d.related.is_empty()));
    // A box allocated before the checkpoint survives the rewind.
    assert_eq!(
        both(
            "fn main() -> i64 { region r { let k = r.box(7); let m = r.checkpoint(); let t = r.box(1); r.rewind(m); r.get(k) } }"
        ),
        "7"
    );
}

#[test]
fn a_module_without_region_syntax_keeps_its_own_region_effect() {
    let world = accepted(
        "effect Region { fn touch() -> i64; } fn f() -> i64 eff [Region] { Region::touch() } fn main() -> i64 { handle Region in { f() } with { return x => x, touch(k) => k(3), } }",
    );
    assert!(!regions::active(&world));
    assert_eq!(
        Runtime::new(&world)
            .run("main", vec![])
            .unwrap()
            .to_string(),
        "3"
    );
}

#[test]
fn the_built_in_region_effect_is_reserved() {
    rejects(
        "effect Region { fn x(); } fn main() -> i64 { region r { 0 } }",
        "E0797",
    );
    rejects(
        "fn main() -> i64 { handle r: Region in { 1 } with { return x => x, } }",
        "E0797",
    );
    rejects(
        "fn main() -> i64 { region r { Region::box(1); 0 } }",
        "E0790",
    );
    rejects("fn main() -> i64 { region r { r.frob(1); 0 } }", "E0790");
    rejects("fn main() -> i64 { region r { r.box(1, 2); 0 } }", "E0790");
}

// ----- review follow-ups ------------------------------------------------------

#[test]
fn an_abort_out_of_an_inner_activation_does_not_misdirect_the_outer_one() {
    // The inner activation of the same block aborts; the outer one must still
    // read its own box afterwards.
    let source = "fn f(n: i64) -> i64 eff [Fail[i64]] {
    region r {
        let a = r.box(n);
        let inner = if n == 0 { Fail::raise(9) } else { handle Fail[i64] in { f(wrapping_sub(n, 1)) } with { return x => x, raise(e, k) => e, } };
        wrapping_add(inner, r.get(a))
    }
}
fn main() -> i64 { handle Fail[i64] in { f(1) } with { return x => x, raise(e, k) => e, } }";
    assert_eq!(both_allowing_aborts(source), "10");
}

/// Like [`both`], for programs whose aborts leave regions to the drivers.
fn both_allowing_aborts(source: &str) -> String {
    let world = accepted(source);
    let evaluated = Runtime::new(&world)
        .run("main", vec![])
        .unwrap()
        .to_string();
    let jit = native::jit(&world, "main").unwrap().to_string();
    assert_eq!(evaluated, jit, "evaluator and native JIT agree");
    let (_, _, open) = regions::native_runtime::counts();
    assert_eq!(open, 0);
    evaluated
}

#[test]
fn an_alias_of_a_box_from_before_the_checkpoint_survives_rewind() {
    assert_eq!(
        both(
            "fn main() -> i64 { region r { let a = r.box(7); let m = r.checkpoint(); let b = a; r.rewind(m); r.get(b) } }"
        ),
        "7"
    );
}

#[test]
fn an_alias_of_a_box_from_after_the_checkpoint_dies_with_it() {
    rejects(
        "fn main() -> i64 { region r { let m = r.checkpoint(); let a = r.box(7); let b = a; r.rewind(m); r.get(b) } }",
        "E0796",
    );
}

#[test]
fn closure_annotations_name_source_regions() {
    assert_eq!(
        both(
            "fn main() -> i64 { region r { let a = r.box(5); let read = |b: RegionBox[r, i64]| r.get(b); read(a) } }"
        ),
        "5"
    );
}

#[test]
fn used_bytes_agree_across_backends() {
    assert_eq!(
        both(
            "fn main() -> u64 { region r { let a = r.box(1); let n = r.used(); let _ = r.get(a); n } }"
        ),
        "8"
    );
}

#[test]
fn a_failing_callee_region_does_not_capture_the_callers_handles() {
    let source =
        "fn risky() -> i64 eff [Fail[i64]] { region r { let _ = r.box(99); Fail::raise(7) } }
fn main() -> i64 {
    region r {
        let a = r.box(42);
        let e = handle Fail[i64] in { risky() } with { return x => x, raise(e, k) => e, };
        wrapping_add(e, r.get(a))
    }
}";
    assert_eq!(both_allowing_aborts(source), "49");
}
