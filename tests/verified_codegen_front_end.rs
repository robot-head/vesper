//! Verified codegen over the whole self-hosted front end (design §42.1,
//! §42.2): every function of `packages/vesper-compiler` is planned and
//! lowered by the verified backend, and every refusal (not only the first)
//! is listed. `VESPER_BLESS=1` rewrites the checked-in inventory.
#[path = "fixtures/self_host/driver.rs"]
mod support;
use std::collections::BTreeSet;
use support::*;
use vesper::{check, lexer, parser, verified_codegen as verified};

/// The checker's sources (`tests/self_host_check.rs`), in order.
const CHECKER: &[&str] = &[
    "lexer",
    "cursor",
    "types_items",
    "expressions",
    "driver",
    "ast_core",
    "resolve_core",
    "check_modes",
    "check_adt",
    "check_expr",
    "check_blocks",
    "check_driver",
];

fn checked(text: &str) -> check::World {
    let limits = parser::ParseLimits {
        max_source_bytes: lexer::HARD_SOURCE_BYTES,
        max_tokens: lexer::HARD_TOKENS,
        ..Default::default()
    };
    let parsed = parser::parse_with_limits(text, limits).expect("program limits");
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let checked = check::check(&parsed.module);
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
    checked.world
}

/// The functions whose definition starts in `text[..boundary]`.
fn defined_in(world: &check::World, boundary: usize) -> BTreeSet<String> {
    world
        .functions
        .iter()
        .filter(|(_, f)| f.body.is_some() && f.span.start < boundary)
        .map(|(n, _)| n.clone())
        .collect()
}

/// One compilation unit of the front end: its name, checked world and the
/// functions it defines (the generated input provider included).
struct Unit {
    name: &'static str,
    world: check::World,
    roots: BTreeSet<String>,
}

fn units() -> Vec<Unit> {
    let read = |f: &str| {
        std::fs::read_to_string(root().join(format!("packages/vesper-compiler/src/{f}.ves")))
            .unwrap()
    };
    let input = source_module(b"fn f() -> i64 { 1 }");
    let mut out = vec![];
    // Stage 1 (bootstrap/stage1.rs): the five Loom.toml files.
    let stage1 = FRONT_END
        .iter()
        .map(|p| std::fs::read_to_string(root().join(p)).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    // The structured parser (`parse_*`, `syntax_*`).
    let parser = read_front_end();
    let checker = CHECKER
        .iter()
        .map(|f| read(f))
        .collect::<Vec<_>>()
        .join("\n");
    for (name, front) in [("stage1", stage1), ("parser", parser), ("checker", checker)] {
        let text = format!("{front}{input}");
        let world = checked(&text);
        let roots = defined_in(&world, text.len());
        out.push(Unit { name, world, roots });
    }
    out
}

fn inventory() -> String {
    let mut out = String::new();
    for unit in units() {
        let cover = verified::coverage(&unit.world, Some(&unit.roots), true).unwrap();
        out.push_str(&format!("== {} ==\n{}\n", unit.name, cover.listing()));
    }
    out
}

const INVENTORY: &str = "docs/parallel-results/verified-codegen-front-end-inventory.txt";

#[test]
fn every_front_end_refusal_is_listed() {
    let listing = on_big_stack(inventory);
    let path = root().join(INVENTORY);
    if std::env::var_os("VESPER_BLESS").is_some() {
        std::fs::write(&path, &listing).unwrap();
    }
    let recorded = std::fs::read_to_string(&path).unwrap_or_default();
    assert_eq!(
        recorded, listing,
        "the verified-codegen front-end inventory changed; rerun with VESPER_BLESS=1"
    );
}
