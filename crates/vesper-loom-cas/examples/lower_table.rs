//! Generates `vesper/src/lower.ves`, the Vesper port's table of Rust's
//! `char::to_lowercase`, which the tree's case-collision check folds names
//! with (`str::to_lowercase`).
//!
//! Every char but U+0130 lowers to one char. The table lists the runs of
//! code points that lower to `c + delta` for one delta, every `step`-th
//! code point of the run (the others lower to themselves); U+0130 is
//! handled by the caller.
//!
//! `str::to_lowercase` lowers U+03A3 to the final sigma at the end of a
//! word, which it finds with the Unicode properties Case_Ignorable and
//! Cased. std does not expose them, so the second table is probed from
//! `str::to_lowercase` itself: a char followed by U+03A3 gives a final
//! sigma exactly when it is cased and not case-ignorable, and "A", the
//! char, U+03A3 gives one when it is case-ignorable or cased. A char that
//! is case-ignorable is always skipped, so whether it is also cased never
//! matters.
//!
//! Run `cargo run -p vesper-loom-cas --example lower_table >
//! crates/vesper-loom-cas/vesper/src/lower.ves`; the test
//! `lower_table_is_generated` (tests/lower_table.rs) fails when the committed
//! file differs from this output. The header records the generating
//! toolchain's `char::UNICODE_VERSION`; a toolchain with another Unicode
//! version (`cargo` on a newer stable than Bazel's pinned Rust) skips the
//! comparisons, since its `to_lowercase` differs on the new chars. Regenerate
//! with the toolchain Bazel pins.

/// `(first, last, delta, step)`: `c` in `first..=last` with
/// `(c - first) % step == 0` lowers to `c + delta` (wrapping).
type Run = (u32, u32, u64, u32);

fn delta(cp: u32) -> Option<u64> {
    let c = char::from_u32(cp)?;
    let mut lower = c.to_lowercase();
    let first = lower.next()?;
    if lower.next().is_some() {
        return None; // U+0130, special-cased by the lookup.
    }
    let d = (first as u32 as u64).wrapping_sub(cp as u64);
    (d != 0).then_some(d)
}

pub fn runs() -> Vec<Run> {
    let mut out: Vec<Run> = Vec::new();
    for cp in 0..=0x10_ffffu32 {
        let Some(d) = delta(cp) else { continue };
        if let Some(last) = out.last_mut() {
            let (first, end, ld, step) = *last;
            if ld == d {
                // Extend a run of the same step, or turn a single code point
                // into a run with this spacing.
                if end + step == cp || (first == end && cp - end <= 2) {
                    let step = if first == end { cp - end } else { step };
                    *last = (first, cp, d, step);
                    continue;
                }
            }
        }
        out.push((cp, cp, d, 1));
    }
    assert!(out.iter().all(|r| r.3 == 1 || r.3 == 2), "steps are 1 or 2");
    out
}

/// The case class of a char for the final-sigma rule: 1 case-ignorable,
/// 2 cased (and not case-ignorable), 0 neither.
fn class(c: char) -> u64 {
    let ends_final = |s: String| s.to_lowercase().ends_with('\u{3c2}');
    if ends_final(format!("{c}\u{3a3}")) {
        2
    } else if ends_final(format!("A{c}\u{3a3}")) {
        1
    } else {
        0
    }
}

/// Maximal runs of code points with the same class, as (first, last,
/// class), leaving out class 0. Surrogates join the run they are in.
pub fn class_runs() -> Vec<(u32, u32, u64)> {
    let mut out: Vec<(u32, u32, u64)> = Vec::new();
    let mut prev = 0;
    for cp in 0..=0x10_ffffu32 {
        let v = char::from_u32(cp).map_or(prev, class);
        prev = v;
        match out.last_mut() {
            Some((_, last, c)) if *last + 1 == cp && *c == v => *last = cp,
            _ => out.push((cp, cp, v)),
        }
    }
    out.retain(|&(_, _, v)| v != 0);
    out
}

fn array(name: &str, items: &[u64]) -> String {
    // `vesper fmt`'s layout: one line when it fits in 100 columns, else one
    // element per line.
    let items: Vec<String> = items.iter().map(u64::to_string).collect();
    let head = format!("const {name}: [u64; {}] = [", items.len());
    let line = format!("{head}{}];\n", items.join(", "));
    if line.len() <= 101 {
        line
    } else {
        format!("{head}\n    {}\n];\n", items.join(",\n    "))
    }
}

const LOOKUP: &str = r#"
// The last run whose first code point is at most `c`, or the run count.
fn lower_run(c: u64) -> u64 {
    let mut lo: u64 = 0;
    let mut hi: u64 = {N};
    loop {
        if lo >= hi {
            break;
        }
        let mid = wrapping_add(lo, wrapping_sub(hi, lo) >> 1);
        let first = if mid < {N} {
            LOWER_LO[mid]
        } else {
            0
        };
        if first <= c {
            lo = wrapping_add(mid, 1);
        } else {
            hi = mid;
        }
    }
    if lo == 0 {
        {N}
    } else {
        wrapping_sub(lo, 1)
    }
}

// The char `c` lowers to, for every char but U+0130 (`char::to_lowercase`).
pub fn lower_char(c: u64) -> u64 {
    let i = lower_run(c);
    if i < {N} {
        if c <= LOWER_HI[i] {
            // Steps are 1 or 2.
            if LOWER_STEP[i] == 1 || (wrapping_sub(c, LOWER_LO[i]) & 1) == 0 {
                return wrapping_add(c, LOWER_DELTA[i]);
            }
        }
    }
    c
}

// The case class of the char `c` for the final-sigma rule: 1
// case-ignorable, 2 cased (and not case-ignorable), 0 neither.
pub fn case_class(c: u64) -> u64 {
    let mut lo: u64 = 0;
    let mut hi: u64 = {C};
    loop {
        if lo >= hi {
            break;
        }
        let mid = wrapping_add(lo, wrapping_sub(hi, lo) >> 1);
        let first = if mid < {C} {
            CASE_LO[mid]
        } else {
            0
        };
        if first <= c {
            lo = wrapping_add(mid, 1);
        } else {
            hi = mid;
        }
    }
    if lo == 0 {
        return 0;
    }
    let i = wrapping_sub(lo, 1);
    if i < {C} {
        if c <= CASE_HI[i] {
            return CASE_CLASS[i];
        }
    }
    0
}
"#;

const UNICODE_LINE: &str = "// Unicode version: ";

fn version_text((a, b, c): (u8, u8, u8)) -> String {
    format!("{a}.{b}.{c}")
}

/// Whether this toolchain's `char::UNICODE_VERSION` is the one the committed
/// table `src` was generated with, so that its `to_lowercase` must agree.
#[allow(dead_code)]
pub fn same_unicode(src: &str) -> bool {
    let want = format!("{UNICODE_LINE}{}", version_text(char::UNICODE_VERSION));
    let same = src.lines().any(|l| l == want);
    if !same {
        eprintln!("skipped: lower.ves is for another Unicode version than this toolchain's");
    }
    same
}

pub fn generate() -> String {
    let runs = runs();
    let mut s = String::from(
        "// @generated by crates/vesper-loom-cas/examples/lower_table.rs; do not edit.\n\
         // Rust's `char::to_lowercase` as runs of code points that lower to `c + delta`\n\
         // (wrapping), every `step`-th code point of a run; U+0130 lowers to two chars\n\
         // and is handled by the caller. Then the case classes of the final-sigma rule\n\
         // (1 case-ignorable, 2 cased), probed from `str::to_lowercase`.\n",
    );
    s += &format!("{UNICODE_LINE}{}\n\n", version_text(char::UNICODE_VERSION));
    let col = |f: &dyn Fn(&Run) -> u64| runs.iter().map(f).collect::<Vec<u64>>();
    s += &array("LOWER_LO", &col(&|r| r.0 as u64));
    s += &array("LOWER_HI", &col(&|r| r.1 as u64));
    s += &array("LOWER_DELTA", &col(&|r| r.2));
    s += &array("LOWER_STEP", &col(&|r| r.3 as u64));
    let classes = class_runs();
    s += "\n";
    let ccol = |f: &dyn Fn(&(u32, u32, u64)) -> u64| classes.iter().map(f).collect::<Vec<u64>>();
    s += &array("CASE_LO", &ccol(&|r| r.0 as u64));
    s += &array("CASE_HI", &ccol(&|r| r.1 as u64));
    s += &array("CASE_CLASS", &ccol(&|r| r.2));
    s += &LOOKUP
        .replace("{N}", &runs.len().to_string())
        .replace("{C}", &classes.len().to_string());
    s
}

#[allow(dead_code)]
fn main() {
    print!("{}", generate());
}
