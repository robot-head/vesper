//! `lean/TensorKv.lean`: source hygiene always; with `VESPER_LEAN` set (as in
//! CI) compile it with the pinned Lean and audit every theorem's axioms.
//!
//! Separately, a Rust transliteration of the Lean `step` function is run
//! side by side with the real `KvPool` on generated operation sequences, and
//! their page counts must agree after every operation. That comparison is
//! evidence on those inputs only; the general claims are the Lean theorems,
//! which are about the Lean model, not about the Rust code.
use std::path::PathBuf;

use vesper_tensor::kv::{Checkpoint, KvConfig, KvFormat, KvPool, Sequence};

const MODEL_ID: &str = "vesper.tensor.kv-model/1";
const STANDARD_AXIOMS: [&str; 3] = ["propext", "Classical.choice", "Quot.sound"];
const THEOREMS: [&str; 11] = [
    "step_conserves",
    "run_conserves",
    "step_wf",
    "run_wf",
    "rollback_guard_holds",
    "release_frees_all",
    "begin_refused_unchanged",
    "stream_completion_conserves_kv_pages",
    "gqa_kv_head_index_in_bounds",
    "gqa_flat_index_in_bounds",
    "page_index_in_bounds",
];

fn root() -> PathBuf {
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
fn lean_source_has_no_escape_hatches_and_audits_every_theorem() {
    let source = std::fs::read_to_string(root().join("TensorKv.lean")).expect("read model");
    let code = strip_comments(&source);
    for banned in [
        "sorry",
        "admit",
        "axiom ",
        "unsafe",
        "partial ",
        "@[implemented_by",
        "@[extern",
        "native_decide",
    ] {
        assert!(!code.contains(banned), "TensorKv.lean contains {banned:?}");
    }
    assert!(code.contains(&format!("def modelId : String := \"{MODEL_ID}\"")));
    for theorem in THEOREMS {
        assert!(code.contains(&format!("theorem {theorem} ")), "{theorem}");
        assert!(
            code.contains(&format!("#print axioms TensorKv.{theorem}\n")),
            "{theorem} axiom audit"
        );
    }
}

#[test]
fn lean_model_compiles_with_only_standard_axioms() {
    let Ok(lean) = std::env::var("VESPER_LEAN") else {
        eprintln!("VESPER_LEAN unset; Lean compilation skipped");
        return;
    };
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("vesper-tensor-{}-{nanos}", std::process::id()));
    std::fs::create_dir(&dir).expect("private Lean output directory");
    let compiled = std::process::Command::new(&lean)
        .current_dir(root())
        .arg("-o")
        .arg(dir.join("TensorKv.olean"))
        .arg("TensorKv.lean")
        .output()
        .expect("spawn Lean");
    let _ = std::fs::remove_dir_all(&dir);
    let report = String::from_utf8(compiled.stdout).expect("UTF-8");
    assert!(
        compiled.status.success(),
        "Lean model failed: {}\n{report}\n{}",
        compiled.status,
        String::from_utf8_lossy(&compiled.stderr)
    );
    let mut audited = 0;
    for line in report.lines() {
        if line.ends_with("does not depend on any axioms") {
            audited += 1;
            continue;
        }
        let Some((_, rest)) = line.split_once(" depends on axioms: [") else {
            assert!(!line.contains("error"), "unexpected Lean output: {line}");
            continue;
        };
        let axioms = rest
            .strip_suffix(']')
            .unwrap_or_else(|| panic!("unexpected Lean output: {line}"));
        for axiom in axioms.split(", ") {
            assert!(
                STANDARD_AXIOMS.contains(&axiom),
                "unapproved axiom {axiom} in {line}"
            );
        }
        audited += 1;
    }
    assert_eq!(audited, THEOREMS.len(), "axiom report lines:\n{report}");
}

/// Transliteration of the Lean model's `step` (one sequence per id).
#[derive(Clone, Debug, Default)]
struct ModelPool {
    free: usize,
    seqs: Vec<(usize, usize, usize)>, // (id, tokens, pages)
}
fn pages_for(t: usize) -> usize {
    t.div_ceil(16)
}
impl ModelPool {
    fn active(&self) -> usize {
        self.seqs.iter().map(|s| s.2).sum()
    }
    fn find(&self, id: usize) -> Option<usize> {
        self.seqs.iter().position(|s| s.0 == id)
    }
    fn begin(&mut self, id: usize) -> bool {
        if self.find(id).is_some() {
            return false;
        }
        self.seqs.insert(0, (id, 0, 0));
        true
    }
    fn append(&mut self, id: usize) -> bool {
        let Some(i) = self.find(id) else { return false };
        let (_, t, k) = self.seqs[i];
        if t % 16 == 0 {
            if self.free == 0 {
                return false;
            }
            self.free -= 1;
            self.seqs[i] = (id, t + 1, k + 1);
        } else {
            self.seqs[i] = (id, t + 1, k);
        }
        true
    }
    fn rollback(&mut self, id: usize, keep: usize) -> bool {
        let Some(i) = self.find(id) else { return false };
        let (_, t, k) = self.seqs[i];
        if !(keep <= t && pages_for(keep) <= k) {
            return false;
        }
        self.free += k - pages_for(keep);
        self.seqs[i] = (id, keep, pages_for(keep));
        true
    }
    fn release(&mut self, id: usize) {
        let held: usize = self.seqs.iter().filter(|s| s.0 == id).map(|s| s.2).sum();
        self.free += held;
        self.seqs.retain(|s| s.0 != id);
    }
}

#[test]
fn rust_pool_page_counts_follow_the_lean_step_function() {
    let pages = 12;
    let config = KvConfig {
        heads: 1,
        head_dim: 2,
        pages,
        max_sequences: 4,
        max_tokens_per_sequence: 96,
        max_bytes: 1 << 20,
    };
    let mut state = 0x7e45_0aa1u64;
    let mut next = |n: u64| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state % n) as usize
    };
    for _round in 0..200 {
        let mut pool = KvPool::with_format(config, KvFormat::F32).unwrap();
        let mut model = ModelPool {
            free: pages,
            ..ModelPool::default()
        };
        type Live = Option<(Sequence, Option<(Checkpoint, usize)>)>;
        let mut live: Vec<Live> = (0..4).map(|_| None).collect();
        for _ in 0..300 {
            let id = next(4);
            match next(10) {
                0 => {
                    let ok = live[id].is_none()
                        && pool.begin().map(|s| live[id] = Some((s, None))).is_ok();
                    assert_eq!(ok, model.begin(id));
                }
                1..=6 => {
                    let ok = match &mut live[id] {
                        Some((s, _)) => {
                            let n = pool.len(s).unwrap();
                            pool.append_f32(s, n, &[1.0, 2.0], &[3.0, 4.0]).is_ok()
                        }
                        None => false,
                    };
                    assert_eq!(ok, model.append(id));
                }
                7 => {
                    if let Some((s, mark)) = &mut live[id] {
                        let len = pool.len(s).unwrap();
                        *mark = pool.checkpoint(s).ok().map(|c| (c, len));
                    }
                }
                8 => {
                    let ok = match &mut live[id] {
                        Some((s, mark @ Some(_))) => {
                            let (m, keep) = mark.take().unwrap();
                            pool.rollback(s, &m).unwrap();
                            Some(keep)
                        }
                        _ => None,
                    };
                    if let Some(keep) = ok {
                        assert!(model.rollback(id, keep));
                    }
                }
                _ => {
                    if let Some((s, _)) = live[id].take() {
                        pool.release(s).unwrap();
                    }
                    model.release(id);
                }
            }
            let stats = pool.stats();
            assert_eq!(stats.free_pages, model.free);
            assert_eq!(stats.allocated_pages, model.active());
            assert_eq!(model.free + model.active(), pages, "conservation");
            for (id, t, k) in &model.seqs {
                assert_eq!(*k, pages_for(*t));
                let (s, _) = live[*id].as_ref().unwrap();
                assert_eq!(pool.len(s).unwrap(), *t);
                assert_eq!(pool.page_count(s).unwrap(), *k);
            }
        }
        for slot in live.iter_mut() {
            if let Some((s, _)) = slot.take() {
                pool.release(s).unwrap();
            }
        }
        assert_eq!(pool.stats().free_pages, pages);
    }
}
