use vesper::{check, ir, ir_check, ir_opt, native, parser, runtime::Runtime};
fn checked(source: &str) -> check::World {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    let c = check::check(&p.module);
    assert!(c.diagnostics.is_empty(), "{:?}", c.diagnostics);
    c.world
}
fn agrees(source: &str, expected: &str) {
    let w = checked(source);
    let mut plain = Runtime::new(&w);
    plain.optimize = false;
    assert_eq!(plain.run("main", vec![]).unwrap().to_string(), expected);
    assert_eq!(
        Runtime::new(&w).run("main", vec![]).unwrap().to_string(),
        expected
    );
    assert_eq!(native::jit(&w, "main").unwrap().to_string(), expected);
    assert_eq!(
        native::jit_with_options(&w, "main", false)
            .unwrap()
            .to_string(),
        expected
    );
}
#[test]
fn forwards_scalar_binding_and_disjoint_write() {
    let source = "copy struct Pair { x: i64, y: i64 } fn main() -> i64 { let p = Pair { x: 42, y: 0 }; let before = p.x; p.y = 10; p.x + before - 42 }";
    let w = checked(source);
    let mut program = ir::lower_world(&w);
    assert!(ir_opt::optimize(&w, &mut program).unwrap() >= 2);
    agrees(source, "42");
}
#[test]
fn mutable_call_is_a_state_barrier() {
    agrees(
        "fn inc(inout x: i64) { x += 1; } fn main() -> i64 { let x = 1; let before = x; inc(inout x); before + x }",
        "3",
    );
}
#[test]
fn overlapping_record_write_invalidates_read() {
    agrees(
        "copy struct Pair { x: i64, y: i64 } fn main() -> i64 { let p = Pair { x: 40, y: 0 }; let before = p.x; p = Pair { x: 2, y: 0 }; p.x + before }",
        "42",
    );
}
#[test]
fn branch_join_uses_path_specific_state() {
    agrees(
        "fn choose(b: bool) -> i64 { let x = 0; if b { x = 1; } else { x = 2; }; x } fn main() -> i64 { choose(true) + choose(false) }",
        "3",
    );
}
#[test]
fn evidence_checker_rejects_stale_tokens_and_wrong_phi() {
    let w = checked("fn main() -> i64 { let x = 0; if true { x = 1; } else { x = 2; }; x }");
    let mut program = ir::lower_world(&w);
    assert!(ir_check::evidence(&program).is_empty());
    let f = program.functions.get_mut("main").unwrap();
    let i = f
        .blocks
        .iter_mut()
        .flat_map(|b| &mut b.instructions)
        .find(|i| i.token.is_some())
        .unwrap();
    i.token.as_mut().unwrap().0 = u32::MAX;
    assert!(
        ir_check::evidence(&program)
            .iter()
            .any(|d| d.code == "E0411")
    );
    ir::thread_tokens(&mut program);
    let f = program.functions.get_mut("main").unwrap();
    f.blocks
        .iter_mut()
        .find(|b| !b.incoming_tokens.is_empty())
        .unwrap()
        .incoming_tokens
        .clear();
    assert!(
        ir_check::evidence(&program)
            .iter()
            .any(|d| d.code == "E0411")
    );
}
