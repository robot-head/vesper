//! The Vesper port of `format_recovery_source` against the frozen Rust seed
//! (#1996, fmt part 3c-3): every result, output, edits, counts and digest,
//! or error, must be the same. The corpus is every `.ves` file in the
//! repository, damaged copies of them from a fixed seed, and sweeps of the
//! limits across the point where they start to refuse.
#![cfg(vesper_port)]
use std::path::{Path, PathBuf};
use vesper_fmt::{Limits, port_format_recovery_source};

fn root() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    if manifest.join("../../Cargo.toml").is_file() {
        manifest.join("../..")
    } else if Path::new("../../packages").is_dir() {
        PathBuf::from("../..")
    } else {
        PathBuf::from(".")
    }
}

fn repository_sources() -> Vec<String> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            if path.is_dir() {
                if !matches!(
                    name.to_str(),
                    Some("target" | ".git" | "node_modules" | "bazel-out")
                ) && !name.to_string_lossy().starts_with("bazel-")
                {
                    walk(&path, out);
                }
            } else if path.extension().is_some_and(|e| e == "ves") {
                out.push(path);
            }
        }
    }
    let mut paths = vec![];
    walk(&root(), &mut paths);
    paths.sort();
    paths
        .iter()
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .collect()
}

fn seed_limits(l: Limits) -> fmt_seed::Limits {
    fmt_seed::Limits {
        max_input_bytes: l.max_input_bytes,
        max_output_bytes: l.max_output_bytes,
        max_tokens: l.max_tokens,
        max_doc_nodes: l.max_doc_nodes,
        max_depth: l.max_depth,
        max_work: l.max_work,
    }
}

fn seed(source: &str, limits: Limits) -> String {
    match fmt_seed::recovery::format_recovery_source(source, seed_limits(limits)) {
        Ok(r) => format!(
            "Ok(changed={} formatted={} hash={:?} edits={:?}) {:?}",
            r.changed(),
            r.formatted_declarations(),
            r.source_hash(),
            r.edits()
                .iter()
                .map(|e| (e.span(), e.replacement().to_owned()))
                .collect::<Vec<_>>(),
            r.output()
        ),
        Err(e) => format!("Err({e:?})"),
    }
}

fn port(source: &str, limits: Limits) -> String {
    match port_format_recovery_source(source, limits) {
        None => "None".to_owned(),
        Some(Ok(r)) => format!(
            "Ok(changed={} formatted={} hash={:?} edits={:?}) {:?}",
            r.changed(),
            r.formatted_declarations(),
            r.source_hash(),
            r.edits()
                .iter()
                .map(|e| (e.span(), e.replacement().to_owned()))
                .collect::<Vec<_>>(),
            r.output()
        ),
        Some(Err(e)) => format!("Err({e:?})"),
    }
}

fn compare(source: &str, limits: Limits) -> Option<String> {
    let (s, p) = (seed(source, limits), port(source, limits));
    if s == p {
        return None;
    }
    let cut = |s: &str| s.chars().take(400).collect::<String>();
    Some(format!("seed {}\nport {}", cut(&s), cut(&p)))
}

fn report(failures: &[String]) {
    assert!(
        failures.is_empty(),
        "{} differ:\n{}",
        failures.len(),
        failures
            .iter()
            .take(5)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn repository_sources_give_the_same_proposal() {
    let mut failures = vec![];
    let mut formatted = 0;
    for (i, source) in repository_sources().iter().enumerate() {
        if let Some(d) = compare(source, Limits::default()) {
            failures.push(format!("file {i}: {d}"));
        }
        if fmt_seed::recovery::format_recovery_source(source, fmt_seed::Limits::default()).is_ok() {
            formatted += 1;
        }
    }
    report(&failures);
    eprintln!("{formatted} accepted");
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

const FRAGMENTS: &[&str] = &[
    "{",
    "}",
    "(",
    ")",
    "[",
    "]",
    ",",
    ";",
    "\n",
    "\n\n",
    " ",
    "// c\n",
    "/* b */",
    "fn",
    "let",
    "x",
    "1",
    "\"s\"",
    "->",
    "=>",
    "::",
    ".",
    "+",
    "!",
    "@",
    "$",
    "if",
    "else",
    "match",
    "quote",
    "m!",
    "é",
    "\t",
    "fn g( ) {",
    "pub fn h()->u64{1}",
];

fn snap(source: &str, mut at: usize) -> usize {
    at = at.min(source.len());
    while !source.is_char_boundary(at) {
        at -= 1;
    }
    at
}

fn mutate(rng: &mut Rng, source: &str) -> String {
    let mut s = source.to_owned();
    for _ in 0..1 + rng.below(4) {
        let a = snap(&s, rng.below(s.len() + 1));
        let b = snap(&s, a + rng.below(24));
        match rng.below(5) {
            0 => s.replace_range(a..b, ""),
            1 => {
                let piece = s[a..b].to_owned();
                s.insert_str(b, &piece);
            }
            // Scramble the whitespace of a stretch, so complete functions
            // have something to format.
            2 => {
                let piece: String = s[a..b]
                    .chars()
                    .map(|c| if c == ' ' { '\n' } else { c })
                    .collect();
                s.replace_range(a..b, &piece);
            }
            3 => s.truncate(b.max(s.len() / 2)),
            _ => s.insert_str(a, FRAGMENTS[rng.below(FRAGMENTS.len())]),
        }
    }
    s
}

/// Damaged repository sources from a fixed seed (`VESPER_FMT_FUZZ` sets
/// the count).
#[test]
fn damaged_sources_give_the_same_proposal() {
    let sources: Vec<String> = repository_sources()
        .into_iter()
        .filter(|s| s.len() < 24 * 1024)
        .collect();
    let count = std::env::var("VESPER_FMT_FUZZ")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1500);
    let mut rng = Rng(0x5248_3363_3366_6d74);
    let mut failures = vec![];
    let mut edited = 0;
    for i in 0..count {
        let pick = rng.below(sources.len());
        let source = mutate(&mut rng, &sources[pick]);
        if let Some(d) = compare(&source, Limits::default()) {
            failures.push(format!("case {i}: {d}\n{source:?}"));
        }
        if fmt_seed::recovery::format_recovery_source(&source, fmt_seed::Limits::default())
            .is_ok_and(|r| !r.edits().is_empty())
        {
            edited += 1;
        }
    }
    report(&failures);
    eprintln!("{count} cases, {edited} with edits");
    assert!(
        edited > count / 20,
        "too few cases exercise edits: {edited}"
    );
}

/// Each limit swept across the point where it starts to refuse.
#[test]
fn limit_sweeps_fail_at_the_same_point() {
    let sources = [
        "fn main( )->u64{let x=1;x}\nfn broken( {\nfn after()->u64{ 2 }\n".to_owned(),
        "struct S{a:u64}\nfn f(x:u64)->u64{x+1}\nenum E{A,B}\nfn g(){ let y = ( ; }\n".to_owned(),
        format!(
            "{}fn tail( {{\n",
            (0..20)
                .map(|i| format!("fn f{i}( a:u64 )->u64{{ a*{i} }}\n"))
                .collect::<Vec<_>>()
                .join("")
        ),
    ];
    let base = Limits::default();
    let mut failures = vec![];
    for source in &sources {
        let mut cases = vec![];
        let ok =
            |l: Limits| fmt_seed::recovery::format_recovery_source(source, seed_limits(l)).is_ok();
        let mut low = 0usize;
        let mut high = 1usize << 31;
        while low + 1 < high {
            let mid = low + (high - low) / 2;
            if ok(Limits {
                max_work: mid,
                ..base
            }) {
                high = mid;
            } else {
                low = mid;
            }
        }
        for w in high.saturating_sub(4)..high + 4 {
            cases.push(Limits {
                max_work: w,
                ..base
            });
        }
        for k in 1..24 {
            cases.push(Limits {
                max_work: high / 24 * k,
                ..base
            });
        }
        for n in 0..source.len() + 2 {
            cases.push(Limits {
                max_input_bytes: n,
                ..base
            });
            cases.push(Limits {
                max_output_bytes: n,
                ..base
            });
        }
        for n in 0..source.len() + 2 {
            cases.push(Limits {
                max_tokens: n,
                ..base
            });
        }
        for n in 0..40 {
            cases.push(Limits {
                max_depth: n,
                ..base
            });
            cases.push(Limits {
                max_doc_nodes: n,
                ..base
            });
        }
        for l in cases {
            if let Some(d) = compare(source, l) {
                failures.push(format!("{l:?}: {d}"));
            }
        }
    }
    report(&failures);
}
