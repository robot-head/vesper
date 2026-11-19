//! Differential check of the ported front end (port wave P3b lane 6,
//! #1068): seeded random choreography sources, valid and malformed, are
//! parsed, projected, checked for duality and explored, and the `Debug`
//! text of every result is folded into one FNV-1a digest. The Vesper library
//! must produce the digest pinned here, which the replaced Rust
//! implementation computed (before #1068 deleted it).
use vesper_chorus::check::{ExploreLimits, explore, explore_locals};
use vesper_chorus::{
    Choreography, GBranch, GlobalChor, RoleId, are_dual, parse_choreography, project_all,
};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
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

const ROLES: [&str; 5] = ["A", "B", "C", "D", "rec"];
const LABELS: [&str; 6] = ["Go", "Stop", "Data", "Ack", "Go", "Z9"];
const TYPES: [&str; 6] = ["", "u64", "String", "Vec[u8]", "a::B[u64, C]", "u64"];

fn role(r: &mut Rng, n: usize) -> &'static str {
    // Mostly declared roles, sometimes an undeclared one.
    if r.below(400) == 0 {
        "Q"
    } else {
        ROLES[r.below(n as u64) as usize]
    }
}

fn pair(r: &mut Rng, n: usize) -> (&'static str, &'static str) {
    let a = role(r, n);
    if r.below(50) == 0 {
        return (a, a);
    }
    loop {
        let b = role(r, n);
        if b != a {
            return (a, b);
        }
    }
}

fn block(r: &mut Rng, out: &mut String, n: usize, depth: u32, vars: &mut Vec<String>) {
    let stmts = r.below(4);
    for _ in 0..stmts {
        let k = r.below(if depth > 3 { 4 } else { 8 });
        match k {
            0 | 1 => {
                let (a, b) = pair(r, n);
                let l = LABELS[r.below(6) as usize];
                let t = TYPES[r.below(6) as usize];
                out.push_str(&format!("{a} -> {b}: {l}({t});\n"));
            }
            2 => {
                let a = role(r, n);
                out.push_str(&format!("{a}: act{};\n", r.below(3)));
            }
            3 => {
                if let Some(v) = vars.last()
                    && r.below(2) == 0
                {
                    out.push_str(&format!("continue {v};\n"));
                    return;
                }
                if r.below(4) == 0 {
                    out.push_str("end;\n");
                    return;
                }
            }
            4..=6 => {
                let (a, b) = pair(r, n);
                out.push_str(&format!("{a} -> {b} {{\n"));
                let first = r.below(6);
                // A repeated label now and then.
                let step = if r.below(20) == 0 { 0 } else { 1 };
                for i in 0..(1 + r.below(3)) {
                    let l =
                        ["Go", "Stop", "Data", "Ack", "Z9", "Q"][((first + i * step) % 6) as usize];
                    let t = TYPES[r.below(6) as usize];
                    out.push_str(&format!("{l}({t}) {{\n"));
                    block(r, out, n, depth + 1, vars);
                    out.push_str("}\n");
                }
                out.push_str("}\n");
            }
            _ => {
                let v = ["X", "Y", "X"][r.below(3) as usize].to_owned();
                out.push_str(&format!("rec {v} {{\n"));
                vars.push(v);
                block(r, out, n, depth + 1, vars);
                vars.pop();
                out.push_str("}\n");
            }
        }
    }
}

fn source(r: &mut Rng) -> String {
    let n = 2 + r.below(3) as usize;
    let mut s = format!("roles {};\n", ROLES[..n].join(", "));
    // Every role usually takes part.
    if r.below(10) != 0 {
        for i in 1..n {
            s.push_str(&format!("{} -> {}: Hello();\n", ROLES[i - 1], ROLES[i]));
        }
    }
    block(r, &mut s, n, 0, &mut Vec::new());
    // Occasional lexical damage.
    match r.below(30) {
        0 => s.push('#'),
        1 => s.insert(0, 'é'),
        2 => s.push_str("// trailing comment"),
        3 => s.push_str("A -> B: X("),
        4 => s.push('/'),
        _ => {}
    }
    s
}

fn fnv(h: &mut u64, s: &str) {
    for b in s.bytes() {
        *h ^= u64::from(b);
        *h = h.wrapping_mul(0x100_0000_01b3);
    }
}

/// With `CHORUS_DIFF_DUMP=<file>`, the running digest after every seed is
/// appended to the file, so two builds' runs can be compared seed by seed.
fn dump(seed: u64, h: u64) {
    if let Ok(path) = std::env::var("CHORUS_DIFF_DUMP") {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        writeln!(f, "{seed} {h:x}").unwrap();
    }
}

fn run(seeds: u64) -> (u64, [usize; 3]) {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    let mut counts = [0usize; 3];
    let limits = ExploreLimits {
        max_states: 2_000,
        max_queue: 2,
    };
    for seed in 0..seeds {
        let mut r = Rng(seed);
        let src = source(&mut r);
        let c = match parse_choreography(&src) {
            Ok(c) => c,
            Err(e) => {
                counts[0] += 1;
                fnv(&mut h, &format!("{e:?} {e}"));
                dump(seed, h);
                continue;
            }
        };
        fnv(&mut h, &format!("{c:?}"));
        match project_all(&c) {
            Ok(all) => {
                counts[1] += 1;
                for l in &all {
                    fnv(&mut h, &l.render(&c));
                }
                if all.len() >= 2 {
                    fnv(
                        &mut h,
                        &format!("{}", are_dual(RoleId(0), &all[0], RoleId(1), &all[1])),
                    );
                }
                // Hand-written endpoints: swap two roles' local types.
                let mut swapped = all.clone();
                swapped.swap(0, 1);
                fnv(
                    &mut h,
                    &format!("{:?}", explore_locals(&c, swapped, limits)),
                );
            }
            Err(e) => {
                counts[2] += 1;
                fnv(&mut h, &format!("{e:?} {e}"));
            }
        }
        fnv(&mut h, &format!("{:?}", explore(&c, limits)));
        dump(seed, h);
    }
    (h, counts)
}

/// `CHORUS_DIFF_SHOW=<seed>`: print that seed's source and results.
#[test]
fn show_one_seed() {
    let Ok(seed) = std::env::var("CHORUS_DIFF_SHOW") else {
        return;
    };
    let mut r = Rng(seed.parse().unwrap());
    let src = source(&mut r);
    println!("{src}");
    let limits = ExploreLimits {
        max_states: 2_000,
        max_queue: 2,
    };
    match parse_choreography(&src) {
        Err(e) => println!("{e:?} {e}"),
        Ok(c) => {
            println!("{c:?}");
            let all = project_all(&c);
            println!("{all:?}");
            if let Ok(all) = all {
                for l in &all {
                    println!("{}", l.render(&c));
                }
                let mut swapped = all.clone();
                swapped.swap(0, 1);
                println!("{:?}", explore_locals(&c, swapped, limits));
            }
            println!("{:?}", explore(&c, limits));
        }
    }
}

#[test]
fn random_choreographies_match_the_rust_build() {
    let (digest, counts) = run(3_000);
    // Every outcome class is exercised.
    assert!(counts.iter().all(|&n| n > 100), "{counts:?}");
    assert_eq!(digest, PINNED, "digest {digest:#x}, counts {counts:?}");
}

/// Computed by the Rust implementation (`cargo test -p vesper-chorus --test
/// differential` at a2adca20, the last commit that has it).
const PINNED: u64 = 0x8e90_f36e_6924_f221;

#[test]
fn hand_built_trees_are_checked_like_parsed_ones() {
    // `Choreography::new` runs the same checks as the parser.
    let body = GlobalChor::Comm {
        from: RoleId(0),
        to: RoleId(1),
        branches: vec![
            GBranch {
                label: "L".into(),
                payload: String::new(),
                next: GlobalChor::End,
            },
            GBranch {
                label: "L".into(),
                payload: "u64".into(),
                next: GlobalChor::End,
            },
        ],
    };
    let e = Choreography::new(vec!["A".into(), "B".into()], body).unwrap_err();
    assert_eq!(e.to_string(), "choice `A -> B` repeats label `L`");
    let deep = (0..300).fold(GlobalChor::End, |next, _| GlobalChor::Local {
        role: RoleId(0),
        action: "a".into(),
        next: Box::new(next),
    });
    let e = Choreography::new(vec!["A".into()], deep).unwrap_err();
    assert_eq!(
        e.to_string(),
        "choreography nesting 257 exceeds the limit 256"
    );
    let bad = Choreography::new(vec!["ok".into(), "1bad".into()], GlobalChor::End).unwrap_err();
    assert_eq!(bad.to_string(), "`1bad` is not a valid identifier");
}
