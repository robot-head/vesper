//! The checker swaps (#1177) free what they allocate. Its own test binary,
//! because the live allocation counters are global to the process; for the
//! same reason it is one test.
#[rustfmt::skip]
#[allow(clippy::all, dead_code)]
#[path = "../port/check_layout.rs"]
mod layout_binding;

#[rustfmt::skip]
#[allow(clippy::all, dead_code)]
#[path = "../port/ir_check.rs"]
mod ir_check_binding;

#[path = "../../../tests/common/check_words.rs"]
mod check_words;

use std::path::PathBuf;
use vesper::ast::Type;

#[test]
fn checker_swaps_free_their_allocations() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/counter.ves");
    let source = std::fs::read_to_string(path).unwrap();
    let parsed = vesper::parser::parse(&source);
    let checked = vesper::check::check(&parsed.module);
    let w = checked.world;
    let program = vesper::ir::lower_world(&w);
    let mut types: Vec<Type> = w.adts.keys().map(|n| Type::named(n)).collect();
    // A failing layout (its message) and a tuple.
    types.push(Type::named("CheckSwapsMissing"));
    types.push(Type::Tuple(vec![Type::named("u8"), Type::named("String")]));
    let ir = check_words::ir_input(&w, &program).unwrap();
    for t in &types {
        let input = check_words::layout_input(&w, t).unwrap();
        let _ = layout_binding::layout_of_words(&input);
        let before = layout_binding::live_allocations();
        let _ = layout_binding::layout_of_words(&input);
        assert_eq!(layout_binding::live_allocations(), before, "{t}");
    }
    let _ = ir_check_binding::validate_words(&ir);
    let before = ir_check_binding::live_allocations();
    let _ = ir_check_binding::validate_words(&ir);
    assert_eq!(ir_check_binding::live_allocations(), before);
}
