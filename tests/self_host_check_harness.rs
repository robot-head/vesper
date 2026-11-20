//! Self-hosting SH2 lane 4 (#1176): tests of the shared differential
//! harness itself (tests/common/check_harness.rs): the corpus loads, the
//! decoder of `@vesper/check-data` rebuilds every table the encoder wrote,
//! a changed input changes the answer, and the IR mutants differ from the
//! program they come from.
#[path = "common/check_harness.rs"]
mod check_harness;

use check_harness::*;
use std::collections::BTreeSet;

#[test]
fn corpus_loads_checked_worlds() {
    let corpus = corpus();
    eprintln!("corpus: {} worlds", corpus.len());
    assert!(corpus.len() >= 100, "only {} corpus worlds", corpus.len());
    assert!(
        corpus.iter().any(|c| c.name.ends_with('/')),
        "no Loom package in the corpus"
    );
    let names: BTreeSet<&str> = corpus.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names.len(), corpus.len(), "duplicate corpus names");
}

/// The decoder's table sizes equal what the encoder wrote: names (plus the
/// names the decoder interns itself), hash-consed types, ADTs, variants,
/// fields, effects and the three span tables.
#[test]
fn decoder_rebuilds_every_table() {
    let f = check_data().entry("check_data_sizes", true);
    for c in corpus() {
        let mut enc = Enc::default();
        let world = enc.world(&c.world);
        let input = enc.finish(&world, &[]);
        let sizes = f.words(&input);
        let w = &c.world;
        let variants: usize = w.adts.values().map(|a| a.variants.len()).sum();
        let fields: usize = w
            .adts
            .values()
            .flat_map(|a| &a.variants)
            .map(|v| v.fields.len())
            .sum();
        assert!(sizes[0] >= enc.name_list.len() as u64, "{}: names", c.name);
        // Distinct encoded types are distinct arena types; the arena adds
        // `()`, `_` and whatever a row type needs, never fewer.
        assert!(
            sizes[1] >= enc.type_count,
            "{}: types {} < {}",
            c.name,
            sizes[1],
            enc.type_count
        );
        assert_eq!(
            &sizes[3..],
            &[
                w.adts.len() as u64,
                variants as u64,
                fields as u64,
                w.effects.len() as u64,
                w.types.len() as u64,
                w.partial_ops.len() as u64,
                w.overflow_ops.len() as u64,
            ],
            "{}",
            c.name
        );
    }
}

/// One changed word of the input changes the text an entry returns, so a
/// differential over these texts can fail.
#[test]
fn answers_depend_on_the_input() {
    let f = check_data().entry("check_data_types", true);
    let c = corpus().iter().find(|c| !c.world.adts.is_empty()).unwrap();
    let a = c.world.adts.values().next().unwrap();
    let mut e = Enc::default();
    let world = e.world(&c.world);
    let ty = e.ty(&vesper::ast::Type::named(&a.name));
    let input = e.finish(&world, &[1, ty, 0]);
    let text = f.text(&input);
    assert!(text.starts_with(&format!("D {}\n", a.name)), "{text}");
    // Point the type's name word at another name.
    let mut changed = Enc::default();
    let world2 = changed.world(&c.world);
    let ty2 = changed.ty(&vesper::ast::Type::named("CheckDataRenamed"));
    let text2 = f.text(&changed.finish(&world2, &[1, ty2, 0]));
    assert!(text2.starts_with("D CheckDataRenamed\n"), "{text2}");
    assert_ne!(text, text2);
}

#[test]
fn ir_mutants_change_programs() {
    let c = corpus()
        .iter()
        .find(|c| c.world.functions.len() > 3)
        .unwrap();
    let program = vesper::ir::lower_world(&c.world);
    let base = format!("{program:?}");
    let changed = (1..=IR_MUTANTS)
        .filter(|m| format!("{:?}", ir_mutant(&program, *m)) != base)
        .count();
    assert!(
        changed >= 3,
        "only {changed} of {IR_MUTANTS} mutants changed {}",
        c.name
    );
}

#[test]
fn digests_match_the_vesper_fold() {
    // FNV-1a test vectors: fold_bytes of "" and "a" (then the 255 marker).
    assert_eq!(fold(SEED, 0x61), 0xaf63dc4c8601ec8c);
    assert_ne!(fold_bytes(SEED, b""), fold_bytes(SEED, b"a"));
}
