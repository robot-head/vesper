//! `lean/StrataCollections.lean`: source hygiene always; with `VESPER_LEAN` set
//! (as in CI) compile it and audit every theorem's axioms. A Rust
//! transliteration of the Lean model is then run in lockstep with the real
//! `RegionSlotArena` on generated operation sequences, checking that every
//! issued key (live or stale) reads the same thing in both.
//!
//! The lockstep run covers concrete sequences only. The general claim (stale
//! keys never read a live slot) is a Lean theorem about the Lean model, not a
//! proof about the Rust arena.
use std::path::PathBuf;
use vesper_alloc::{ArenaKey, Region, RegionConfig};

const THEOREMS: [&str; 7] = [
    "step_gen_mono",
    "run_gen_mono",
    "get_stale",
    "remove_makes_stale",
    "stale_key_never_reads",
    "fresh_key_differs_from_stale",
    "live_keys_unique",
];

fn lean_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../lean")
}

fn strip_comments(source: &str) -> String {
    let mut out = String::new();
    let mut depth = 0usize;
    let mut chars = source.chars().peekable();
    while let Some(c) = chars.next() {
        match (c, chars.peek()) {
            ('/', Some('-')) => {
                chars.next();
                depth += 1;
            }
            ('-', Some('/')) if depth > 0 => {
                chars.next();
                depth -= 1;
            }
            ('-', Some('-')) if depth == 0 => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

#[test]
#[cfg_attr(miri, ignore = "reads the Lean source; Miri isolates the file system")]
fn lean_source_has_no_escape_hatches_and_audits_every_theorem() {
    let source = std::fs::read_to_string(lean_dir().join("StrataCollections.lean")).unwrap();
    let code = strip_comments(&source);
    for banned in [
        "sorry",
        "admit",
        "axiom ",
        "unsafe",
        "partial ",
        "@[implemented_by",
        "@[extern",
    ] {
        assert!(
            !code.contains(banned),
            "StrataCollections.lean contains {banned:?}"
        );
    }
    for t in THEOREMS {
        assert!(code.contains(&format!("theorem {t}")), "{t} missing");
        assert!(
            code.contains(&format!("#print axioms Vesper.StrataCollections.{t}")),
            "{t} not audited"
        );
    }
}

#[test]
#[cfg_attr(miri, ignore = "runs the Lean binary")]
fn lean_model_compiles_with_standard_axioms_only() {
    let Ok(lean) = std::env::var("VESPER_LEAN") else {
        eprintln!("VESPER_LEAN unset; Lean compilation skipped");
        return;
    };
    let out = std::process::Command::new(&lean)
        .current_dir(lean_dir())
        .arg("StrataCollections.lean")
        .output()
        .expect("run lean");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "lean failed:\n{stdout}\n{stderr}");
    for t in THEOREMS {
        let line = stdout
            .lines()
            .find(|l| l.contains(&format!("'Vesper.StrataCollections.{t}'")))
            .unwrap_or_else(|| panic!("no axiom report for {t}:\n{stdout}"));
        if let Some((_, list)) = line.split_once('[') {
            for axiom in list.trim_end_matches(']').split(',').map(str::trim) {
                assert!(
                    ["propext", "Classical.choice", "Quot.sound"].contains(&axiom),
                    "{t} depends on {axiom}"
                );
            }
        } else {
            assert!(line.contains("does not depend on any axioms"), "{line}");
        }
    }
}

/// Rust transliteration of `Arena` in the Lean model (per-slot generations).
#[derive(Default)]
struct Model {
    generation: Vec<u64>,
    val: Vec<Option<u32>>,
}

impl Model {
    fn grow(&mut self, s: usize) {
        if self.generation.len() <= s {
            self.generation.resize(s + 1, 0);
            self.val.resize(s + 1, None);
        }
    }
    fn get(&self, (slot, g): (usize, u64)) -> Option<u32> {
        (self.generation.get(slot) == Some(&g))
            .then(|| self.val[slot])
            .flatten()
    }
    fn insert(&mut self, s: usize, v: u32) -> Option<(usize, u64)> {
        self.grow(s);
        if self.val[s].is_some() {
            return None;
        }
        self.val[s] = Some(v);
        Some((s, self.generation[s]))
    }
    fn remove(&mut self, k: (usize, u64)) -> Option<u32> {
        let v = self.get(k)?;
        self.generation[k.0] += 1;
        self.val[k.0] = None;
        Some(v)
    }
}

#[test]
fn rust_arena_and_lean_model_agree_on_every_issued_key() {
    let mut region = Region::new(RegionConfig::bounded(1 << 20)).unwrap();
    let mut arena = region.arena_new::<u32>().unwrap();
    let mut model = Model::default();
    let mut keys: Vec<(ArenaKey<u32>, (usize, u64))> = Vec::new();
    let mut state = 0x5107_a7e4_u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let mut stale_checks = 0;
    for step in 0..4000u32 {
        if next() % 3 != 0 || keys.is_empty() {
            let key = region.arena_insert(&mut arena, step).ok().unwrap();
            let mk = model
                .insert(key.slot() as usize, step)
                .expect("the Rust arena reused an occupied slot");
            keys.push((key, mk));
        } else {
            let (key, mk) = keys[(next() as usize) % keys.len()];
            let rust = region.arena_remove(&mut arena, key).ok();
            assert_eq!(rust, model.remove(mk), "remove disagrees at step {step}");
        }
        for (key, mk) in &keys {
            let rust = region.arena_get(&arena, *key).ok().copied();
            let lean = model.get(*mk);
            assert_eq!(rust, lean, "key {key:?} disagrees at step {step}");
            if lean.is_none() && model.val[mk.0].is_some() {
                stale_checks += 1; // a stale key whose slot was reused
            }
        }
    }
    assert!(stale_checks > 1000, "sequence must exercise reused slots");
    region.arena_release(arena).ok().unwrap();
}
