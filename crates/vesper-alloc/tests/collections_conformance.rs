//! Conformance traces for `CacheBTree[K, V]` and `ChunkSeq[T]` (§13.5).
//!
//! Both collections will be implemented in Vesper source; the files under
//! `tests/fixtures/strata/` are the executable specification that port must
//! pass. Each trace is a list of operations with the result the standard
//! library oracle (`BTreeMap`, `Vec`) produces. This test:
//!
//! 1. regenerates every trace from its seed with the oracle and requires the
//!    checked-in file to match byte for byte (set `VESPER_BLESS=1` to rewrite
//!    the files instead), so the fixtures cannot drift from the oracle;
//! 2. replays every `CacheBTree` trace against the Rust reference
//!    [`vesper_alloc::CacheBTree`], checking its structural invariants after
//!    every mutation.
//!
//! `ChunkSeq` has no Rust implementation: its traces are checked against the
//! oracle only and wait for the Vesper port. See `tests/fixtures/strata/README.md`
//! for the format and the semantics each operation pins down.
use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::path::PathBuf;
use vesper_alloc::CacheBTree;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/strata")
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        // splitmix64
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn opt(v: Option<u64>) -> String {
    v.map_or("none".to_string(), |v| v.to_string())
}

// ----- CacheBTree -----------------------------------------------------------

/// Trace shapes: (file stem, seed, ops, key space, key pattern).
#[derive(Clone, Copy)]
enum Keys {
    Random,
    Ascending,
    Descending,
}

const MAP_TRACES: &[(&str, u64, usize, u64, Keys)] = &[
    ("cache_btree_small_churn", 1, 600, 40, Keys::Random),
    ("cache_btree_dense_churn", 2, 6000, 700, Keys::Random),
    ("cache_btree_wide", 3, 8000, 1 << 40, Keys::Random),
    ("cache_btree_ascending", 4, 3000, 0, Keys::Ascending),
    ("cache_btree_descending", 5, 3000, 0, Keys::Descending),
];

fn gen_map_trace(seed: u64, ops: usize, space: u64, keys: Keys) -> String {
    let mut rng = Rng(seed);
    let mut oracle = BTreeMap::<u64, u64>::new();
    let mut out = String::new();
    writeln!(
        out,
        "# CacheBTree conformance trace (oracle: BTreeMap<u64, u64>)"
    )
    .unwrap();
    writeln!(
        out,
        "# seed {seed}, {ops} ops; regenerate with VESPER_BLESS=1"
    )
    .unwrap();
    let mut seq = 0u64;
    for _ in 0..ops {
        let key = |rng: &mut Rng, seq: &mut u64| match keys {
            Keys::Random => rng.below(space),
            Keys::Ascending => {
                *seq += 1;
                *seq * 3
            }
            Keys::Descending => {
                *seq += 1;
                1_000_000 - *seq * 3
            }
        };
        let roll = rng.below(100);
        // Ascending/descending traces insert in order, then remove most keys
        // in the same order in their second half.
        let removing = !matches!(keys, Keys::Random) && seq as usize >= ops / 2;
        if removing && roll < 70 {
            let k = *oracle
                .keys()
                .nth(rng.below(oracle.len().max(1) as u64) as usize)
                .unwrap_or(&0);
            writeln!(out, "remove {k} => {}", opt(oracle.remove(&k))).unwrap();
        } else if roll < 45 || (!matches!(keys, Keys::Random) && !removing) {
            let k = key(&mut rng, &mut seq);
            let v = rng.below(1 << 32);
            writeln!(out, "insert {k} {v} => {}", opt(oracle.insert(k, v))).unwrap();
        } else if roll < 75 {
            let k = if matches!(keys, Keys::Random) {
                rng.below(space)
            } else {
                rng.below(seq * 3 + 1)
            };
            writeln!(out, "remove {k} => {}", opt(oracle.remove(&k))).unwrap();
        } else if roll < 90 {
            let k = rng.below(space.max(seq * 3 + 1));
            writeln!(out, "get {k} => {}", opt(oracle.get(&k).copied())).unwrap();
        } else if roll < 97 {
            let a = rng.below(space.max(seq * 3 + 1));
            let b = a + rng.below(space.max(seq * 3 + 1) / 8 + 2);
            let hits: Vec<String> = oracle
                .range(a..b)
                .take(8)
                .map(|(k, v)| format!("{k}={v}"))
                .collect();
            writeln!(out, "range {a} {b} 8 => {}", hits.join(",")).unwrap();
        } else {
            let first = oracle.first_key_value().map(|(k, v)| format!("{k}={v}"));
            let last = oracle.last_key_value().map(|(k, v)| format!("{k}={v}"));
            writeln!(
                out,
                "bounds => {} {} {}",
                oracle.len(),
                first.unwrap_or("none".into()),
                last.unwrap_or("none".into())
            )
            .unwrap();
        }
    }
    let all: Vec<String> = oracle.iter().map(|(k, v)| format!("{k}={v}")).collect();
    writeln!(out, "len => {}", oracle.len()).unwrap();
    writeln!(out, "hash => {:016x}", fnv(&all.join(","))).unwrap();
    out
}

fn fnv(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

fn replay_map(name: &str, text: &str) {
    let mut t = CacheBTree::<u64, u64>::new();
    for (lineno, line) in text.lines().enumerate() {
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        let (op, expect) = line.split_once(" => ").expect("op => result");
        let w: Vec<&str> = op.split(' ').collect();
        let n = |i: usize| w[i].parse::<u64>().unwrap();
        let got = match w[0] {
            "insert" => {
                let r = opt(t.insert(n(1), n(2)).expect("insert"));
                t.check_invariants().unwrap();
                r
            }
            "remove" => {
                let r = opt(t.remove(&n(1)));
                t.check_invariants().unwrap();
                r
            }
            "get" => opt(t.get(&n(1)).copied()),
            "range" => t
                .range(n(1)..n(2))
                .take(n(3) as usize)
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(","),
            "bounds" => format!(
                "{} {} {}",
                t.len(),
                t.first().map_or("none".into(), |(k, v)| format!("{k}={v}")),
                t.last().map_or("none".into(), |(k, v)| format!("{k}={v}"))
            ),
            "len" => t.len().to_string(),
            "hash" => {
                let all: Vec<String> = t.iter().map(|(k, v)| format!("{k}={v}")).collect();
                format!("{:016x}", fnv(&all.join(",")))
            }
            other => panic!("{name}:{}: unknown op {other}", lineno + 1),
        };
        assert_eq!(got, expect, "{name}:{}: {op}", lineno + 1);
    }
}

// ----- ChunkSeq -------------------------------------------------------------

const SEQ_TRACES: &[(&str, u64, usize)] =
    &[("chunk_seq_small", 11, 800), ("chunk_seq_deep", 12, 12000)];

/// `ChunkSeq` semantics pinned by the trace: a transient sequence mutated in
/// place, `freeze NAME` taking an O(1) persistent snapshot, `thaw NAME`
/// replacing the transient with a mutable copy of a snapshot, and `snap NAME`
/// reading a snapshot back (later edits never show through).
fn gen_seq_trace(seed: u64, ops: usize) -> String {
    let mut rng = Rng(seed);
    let mut cur: Vec<u64> = Vec::new();
    let mut snaps: HashMap<String, Vec<u64>> = HashMap::new();
    let mut out = String::new();
    writeln!(
        out,
        "# ChunkSeq conformance trace (oracle: Vec<u64> + snapshots)"
    )
    .unwrap();
    writeln!(
        out,
        "# seed {seed}, {ops} ops; regenerate with VESPER_BLESS=1"
    )
    .unwrap();
    let summary = |v: &[u64]| format!("{} {:016x}", v.len(), fnv(&format!("{v:?}")));
    for _ in 0..ops {
        let roll = rng.below(100);
        let len = cur.len() as u64;
        if roll < 45 {
            // Bursts cross 32-element chunk and 1024-element level boundaries.
            let burst = if rng.below(10) == 0 { 40 } else { 1 };
            for _ in 0..burst {
                let v = rng.below(1 << 32);
                cur.push(v);
                writeln!(out, "push {v} => {}", cur.len()).unwrap();
            }
        } else if roll < 60 {
            writeln!(out, "pop => {}", opt(cur.pop())).unwrap();
        } else if roll < 75 {
            let i = rng.below(len + 2);
            let v = rng.below(1 << 32);
            let r = match cur.get_mut(i as usize) {
                Some(slot) => {
                    *slot = v;
                    "ok"
                }
                None => "oob",
            };
            writeln!(out, "set {i} {v} => {r}").unwrap();
        } else if roll < 88 {
            let i = rng.below(len + 2);
            writeln!(out, "get {i} => {}", opt(cur.get(i as usize).copied())).unwrap();
        } else if roll < 93 {
            let name = format!("s{}", rng.below(4));
            snaps.insert(name.clone(), cur.clone());
            writeln!(out, "freeze {name} => {}", cur.len()).unwrap();
        } else if roll < 96 {
            let name = format!("s{}", rng.below(4));
            match snaps.get(&name) {
                Some(s) => {
                    cur = s.clone();
                    writeln!(out, "thaw {name} => {}", cur.len()).unwrap();
                }
                None => writeln!(out, "thaw {name} => missing").unwrap(),
            }
        } else {
            let name = format!("s{}", rng.below(4));
            let r = snaps.get(&name).map_or("missing".into(), |s| summary(s));
            writeln!(out, "snap {name} => {r}").unwrap();
        }
    }
    writeln!(out, "all => {}", summary(&cur)).unwrap();
    out
}

// ----- driver ---------------------------------------------------------------

fn check_or_bless(stem: &str, generated: &str) -> String {
    let path = fixture_dir().join(format!("{stem}.trace"));
    if std::env::var_os("VESPER_BLESS").is_some() {
        std::fs::create_dir_all(fixture_dir()).unwrap();
        std::fs::write(&path, generated).unwrap();
        return generated.to_string();
    }
    let on_disk = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{}: {e} (run with VESPER_BLESS=1)", path.display()));
    // Tolerate CRLF checkouts on Windows.
    let on_disk = on_disk.replace("\r\n", "\n");
    assert!(
        on_disk == generated,
        "{} drifted from the oracle; rerun with VESPER_BLESS=1 and review",
        path.display()
    );
    on_disk
}

#[test]
fn cache_btree_traces_match_the_oracle_and_the_reference() {
    for &(stem, seed, ops, space, keys) in MAP_TRACES {
        let text = check_or_bless(stem, &gen_map_trace(seed, ops, space, keys));
        replay_map(stem, &text);
    }
}

#[test]
fn chunk_seq_traces_match_the_oracle() {
    for &(stem, seed, ops) in SEQ_TRACES {
        check_or_bless(stem, &gen_seq_trace(seed, ops));
    }
}

#[test]
fn chunk_seq_traces_reach_chunk_and_level_boundaries() {
    // The traces only specify something if they exercise multi-level shapes:
    // more than 32 elements (a second chunk) and, for the deep one, more than
    // 32 * 32 (a second radix level).
    let deep = gen_seq_trace(12, 12000);
    let max_len = deep
        .lines()
        .filter_map(|l| l.strip_prefix("push "))
        .filter_map(|l| l.split(" => ").nth(1)?.parse::<usize>().ok())
        .max()
        .unwrap();
    assert!(max_len > 32 * 32, "deep trace peaks at {max_len}");
    for op in ["freeze", "thaw", "snap", "set", "pop"] {
        assert!(deep.lines().any(|l| l.starts_with(op)), "no {op} in trace");
    }
}
