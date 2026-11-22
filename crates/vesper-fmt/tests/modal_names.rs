//! `copy` and `affine` used as names space like names (self-hosting SH5
//! lane 4, #1327), while the modes keep their spacing: `fn @copy (u64)`,
//! `T: mod copy`, `p: P @@ copy` and `copy struct` are unchanged.
use vesper_fmt::{Limits, format_source};

fn formats(source: &str) -> String {
    let out = format_source(source, Limits::default())
        .unwrap_or_else(|e| panic!("{e}: {source}"))
        .output;
    let again = format_source(&out, Limits::default()).unwrap();
    assert!(!again.changed, "not idempotent: {source}");
    out
}

#[test]
fn modal_words_as_names() {
    let out = formats(
        "fn copy (x: u64) -> u64 { x } fn affine () {} fn f(v: S) -> u64 { let copy = copy (1); affine (); let a = copy - 1; let b = v.copy[0]; copy [0] + v.affine () }",
    );
    assert!(out.contains("fn copy(x: u64) -> u64"), "{out}");
    assert!(out.contains("fn affine() {}"), "{out}");
    assert!(out.contains("let copy = copy(1);"), "{out}");
    assert!(out.contains("affine();"), "{out}");
    assert!(out.contains("let a = copy - 1;"), "{out}");
    assert!(out.contains("let b = v.copy[0];"), "{out}");
    assert!(out.contains("copy[0] + v.affine()"), "{out}");
}

#[test]
fn modes_keep_their_spacing() {
    let out = formats(
        "copy struct P { x: u64 }\nstruct H { p: P @@copy }\nfn id[T: mod copy + Ord](f: fn @copy (u64) -> u64) {}\n",
    );
    assert!(out.contains("copy struct P"), "{out}");
    assert!(out.contains("p: P @@copy"), "{out}");
    assert!(out.contains("T: mod copy + Ord"), "{out}");
    assert!(out.contains("fn @copy (u64) -> u64"), "{out}");
}
