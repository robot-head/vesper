//! Zero-allocation evidence for the Vesper build's own heap.
//!
//! `tests/alloc.rs` counts the Rust allocator, which sees the wrappers'
//! allocations but not Vesper's: native Vesper code allocates through the C
//! library (`vesper_seq_new`, `vesper_seq_grow`, `vesper_box_alloc`). In
//! native Vesper code a heap allocation happens only at `ChunkSeq::new`,
//! `ChunkSeq::with_capacity`, `push`, `extend`, `Box::new` and the string
//! builders (docs/maintenance/porting-a-crate.md, "The ABI"; `fip_allocation`
//! in src/seq.rs), and `try_get`/`try_set`/`as_slice` never allocate.
//!
//! This test reads the port (`vesper/src/*.ves`) and proves, over a conservative call
//! graph (functions of the same name are merged, every identifier followed
//! by `(` is a call), that no allocating operation is reachable from the
//! steady-state entry points that `tests/alloc.rs` drives. It also checks
//! that the audit can fail: the constructors and `drain` must be flagged.
use std::collections::{BTreeMap, BTreeSet};

const ALLOCATING: &[&str] = &[
    "ChunkSeq::new",
    "ChunkSeq::with_capacity",
    ".push(",
    ".extend(",
    "Box::new",
    "String::",
    "push_str",
    "push_char",
    "reserve",
];

/// The exported functions `tests/alloc.rs` calls after warm-up.
const STEADY_STATE: &[&str] = &[
    "wheel_core_insert",
    "wheel_core_cancel",
    "wheel_core_poll",
    "wheel_core_advance",
    "wheel_core_deadline",
    "wheel_core_next_wake_hint_ns",
    "wheel_core_len",
    "lease_core_park",
    "lease_core_on_ack",
    "lease_core_rearm",
    "lease_core_poll_pto",
    "lease_core_contains",
    "edt_core_submit",
    "edt_core_poll_release",
    "clock_advance_to",
    "clock_advance_by",
];

/// Exported functions that allocate by design (they return sequences or
/// build the preallocated storage).
const ALLOCATES_BY_DESIGN: &[&str] = &[
    "wheel_core_new",
    "wheel_core_drain",
    "lease_core_new",
    "lease_core_drain",
    "lease_core_discard_space",
    "edt_core_new",
    "edt_core_drain",
];

fn strip_comments(source: &str) -> String {
    source
        .lines()
        .map(|line| match line.find("//") {
            Some(at) => &line[..at],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every function body, by name (bodies of same-named functions joined).
fn bodies(source: &str) -> BTreeMap<String, String> {
    let bytes = source.as_bytes();
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    let mut at = 0;
    while let Some(found) = source[at..].find("fn ") {
        let start = at + found;
        at = start + 3;
        let boundary =
            start == 0 || !(bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_');
        if !boundary {
            continue;
        }
        let name: String = source[at..]
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        if name.is_empty() {
            continue;
        }
        let Some(open) = source[at..].find('{').map(|o| at + o) else {
            break;
        };
        let mut depth = 0usize;
        let mut end = open;
        for (i, b) in bytes.iter().enumerate().skip(open) {
            match b {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = i;
                        break;
                    }
                }
                _ => {}
            }
        }
        out.entry(name).or_default().push_str(&source[open..=end]);
        at = open + 1;
    }
    out
}

/// Identifiers followed by `(` in `body`.
fn calls(body: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let chars: Vec<char> = body.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_ascii_alphabetic() || chars[i] == '_' {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            if chars.get(i) == Some(&'(') {
                out.insert(chars[start..i].iter().collect());
            }
        } else {
            i += 1;
        }
    }
    out
}

/// The allocating operations reachable from `root`, with the function that
/// performs each.
fn reachable_allocations(fns: &BTreeMap<String, String>, root: &str) -> Vec<(String, String)> {
    let mut seen = BTreeSet::new();
    let mut stack = vec![root.to_string()];
    let mut found = vec![];
    while let Some(name) = stack.pop() {
        if !seen.insert(name.clone()) {
            continue;
        }
        let Some(body) = fns.get(&name) else {
            continue;
        };
        for op in ALLOCATING {
            if body.contains(op) {
                found.push((name.clone(), op.to_string()));
            }
        }
        stack.extend(calls(body).into_iter().filter(|c| fns.contains_key(c)));
    }
    found
}

fn source() -> BTreeMap<String, String> {
    let parts = [
        include_str!("../vesper/src/types.ves"),
        include_str!("../vesper/src/util.ves"),
        include_str!("../vesper/src/wheel.ves"),
        include_str!("../vesper/src/wheel_lists.ves"),
        include_str!("../vesper/src/wheel_api.ves"),
        include_str!("../vesper/src/leases.ves"),
        include_str!("../vesper/src/edt.ves"),
        include_str!("../vesper/src/clock.ves"),
    ];
    bodies(&strip_comments(&parts.join("\n")))
}

#[test]
fn steady_state_vesper_paths_never_allocate() {
    let fns = source();
    for root in STEADY_STATE {
        assert!(fns.contains_key(*root), "{root} is not in the port");
        let found = reachable_allocations(&fns, root);
        assert!(
            found.is_empty(),
            "{root} can reach an allocation: {found:?}"
        );
    }
}

#[test]
fn the_audit_flags_the_allocating_functions() {
    let fns = source();
    for root in ALLOCATES_BY_DESIGN {
        assert!(
            !reachable_allocations(&fns, root).is_empty(),
            "the audit missed the allocation in {root}"
        );
    }
}
