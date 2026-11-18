//! Differential test of the version subset against the `semver` crate
//! (port wave P1 lane 15, #912): `check_release` must parse, reject, display
//! and order versions exactly as `semver::Version` does. It runs against both
//! builds; on the port-backed package it checks the Vesper reimplementation
//! in `vesper/src/version.ves`.
use semver::Version;
use std::cmp::Ordering;
use vesper_semver::{Interface, ReleaseIssueCode, ReleaseVersionSide, check_release};

/// splitmix64, so the corpus is the same on every platform.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[self.below(items.len())]
    }
}

const NUMBERS: &[&str] = &[
    "0",
    "1",
    "2",
    "3",
    "9",
    "10",
    "11",
    "99",
    "100",
    "18446744073709551615",
];
const IDENTIFIERS: &[&str] = &[
    "alpha", "beta", "rc", "0", "1", "2", "10", "11", "a1", "1a", "x-y", "-", "A", "Z", "aa",
    "alpha1", "999", "00a",
];

fn identifier_list(rng: &mut Rng) -> String {
    let n = 1 + rng.below(3);
    (0..n)
        .map(|_| rng.pick(IDENTIFIERS))
        .collect::<Vec<_>>()
        .join(".")
}

/// A well-formed version, with a small value range so that orderings tie.
fn valid(rng: &mut Rng) -> String {
    let mut v = format!(
        "{}.{}.{}",
        rng.pick(&NUMBERS[..4]),
        rng.pick(&NUMBERS[..4]),
        rng.pick(&NUMBERS[..4])
    );
    if rng.below(2) == 0 {
        v.push('-');
        v.push_str(&identifier_list(rng));
    }
    if rng.below(4) == 0 {
        v.push('+');
        v.push_str(&identifier_list(rng));
    }
    v
}

const NOISE: &[&str] = &[
    ".",
    "-",
    "+",
    "0",
    "00",
    "01",
    "v",
    " ",
    "\t",
    "\n",
    "\r",
    "\0",
    "'",
    "\\",
    "\"",
    "*",
    "x",
    "é",
    "λ",
    "😀",
    "\u{85}",
    "\u{7f}",
    "\u{1b}",
    "99999999999999999999",
    "",
    "..",
    "-.",
    "+.",
];

/// A version with a random local edit, usually malformed.
fn mutated(rng: &mut Rng) -> String {
    let base = if rng.below(3) == 0 {
        format!(
            "{}.{}.{}",
            rng.pick(NUMBERS),
            rng.pick(NUMBERS),
            rng.pick(NUMBERS)
        )
    } else {
        valid(rng)
    };
    let chars: Vec<char> = base.chars().collect();
    let at = rng.below(chars.len() + 1);
    let noise = rng.pick(NOISE);
    let mut out: String = chars[..at].iter().collect();
    match rng.below(3) {
        0 => {
            out.push_str(noise);
            out.extend(&chars[at..]);
        }
        1 => {
            out.push_str(noise);
            out.extend(chars.iter().skip(at + 1));
        }
        _ => out.extend(chars.iter().skip(at + 1)),
    }
    out
}

const FIXED: &[&str] = &[
    "",
    "1",
    "1.",
    "1.0",
    "1.0.",
    "v1.0.0",
    "1.00.0",
    "01.0.0",
    "1.0.0-",
    "1.0.0+",
    "1.0.0-+",
    "1.0.0-a..b",
    "1.0.0-01",
    "1.0.0-00a",
    "1.0.0+01",
    "1.0.0-a+b+c",
    "18446744073709551615.0.0",
    "18446744073709551616.0.0",
    "1.18446744073709551616.0",
    "1.0.99999999999999999999",
    "1.0.0 ",
    " 1.0.0",
    "1.0.0-\u{85}",
    "1.0.0\u{7f}",
    "1.0.0-é",
];

fn corpus() -> Vec<String> {
    let mut rng = Rng(0x5eed_5e3e_4912);
    let mut out: Vec<String> = FIXED.iter().map(|s| s.to_string()).collect();
    for _ in 0..300 {
        out.push(valid(&mut rng));
    }
    for _ in 0..200 {
        out.push(mutated(&mut rng));
    }
    out
}

fn increases(old: &str, new: &str) -> bool {
    let empty = Interface::default();
    let report = check_release(&empty, &empty, old, new).unwrap();
    !report
        .issues
        .iter()
        .any(|i| i.code == ReleaseIssueCode::NonIncreasingVersion)
}

#[test]
fn parse_errors_and_display_match_the_semver_crate() {
    let empty = Interface::default();
    let mut errors = 0;
    for text in corpus() {
        let expected = Version::parse(&text);
        let got = check_release(&empty, &empty, &text, "9.9.9");
        match (&expected, &got) {
            (Ok(v), Ok(report)) => assert_eq!(report.old_version, v.to_string(), "{text:?}"),
            (Err(e), Err(error)) => {
                errors += 1;
                assert_eq!(error.side, ReleaseVersionSide::Old);
                assert_eq!(error.version, text);
                assert_eq!(error.message, e.to_string(), "{text:?}");
            }
            _ => panic!("{text:?}: semver {expected:?}, check_release {got:?}"),
        }
        if expected.is_ok() {
            let error = check_release(&empty, &empty, "0.0.0", &format!("{text}?")).unwrap_err();
            assert_eq!(error.side, ReleaseVersionSide::New);
            assert_eq!(
                error.message,
                Version::parse(&format!("{text}?")).unwrap_err().to_string()
            );
        }
    }
    assert!(errors > 100, "only {errors} malformed inputs");
}

#[test]
fn precedence_matches_the_semver_crate_on_generated_pairs() {
    let versions: Vec<(String, Version)> = corpus()
        .into_iter()
        .filter_map(|t| Version::parse(&t).ok().map(|v| (t, v)))
        .collect();
    let mut rng = Rng(912);
    let mut pairs = 0;
    let mut by_order = [0usize; 3];
    while pairs < 10_000 {
        let (a, va) = &versions[rng.below(versions.len())];
        let (b, vb) = if rng.below(4) == 0 {
            // The same precedence, with other build metadata.
            let mut same = va.clone();
            same.build = semver::BuildMetadata::new(rng.pick(&["", "b", "1.x"])).unwrap();
            (same.to_string(), same)
        } else {
            versions[rng.below(versions.len())].clone()
        };
        let (b, vb) = (&b, &vb);
        let expected = va.cmp_precedence(vb);
        let got = match (increases(b, a), increases(a, b)) {
            (true, false) => Ordering::Greater,
            (false, true) => Ordering::Less,
            (false, false) => Ordering::Equal,
            (true, true) => panic!("{a} and {b} each exceed the other"),
        };
        assert_eq!(got, expected, "{a} vs {b}");
        by_order[(expected as i8 + 1) as usize] += 1;
        pairs += 1;
    }
    // The corpus exercises every outcome, not just one.
    assert!(by_order.iter().all(|&n| n > 500), "{by_order:?}");
}
