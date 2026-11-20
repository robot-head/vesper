//! Static text and byte comparison (self-hosting SH1 lane 5, #1148):
//! a `-> Str` function returns string literals as views of their static
//! bytes, a literal (or such a call) may be bound as a view
//! (`let w = ".ves".as_bytes();`), and `a.cmp(in b)` orders byte views
//! three-way like `memcmp`.
mod ergonomics_support;
use ergonomics_support::{agree, rejects};

const NAMES: [&str; 5] = ["Whitespace", "Ident", "Ident", "Number", "Other"];

#[test]
fn str_results_and_bound_literal_views_agree_on_every_backend() {
    let source = r#"
fn name(k: u8) -> Str {
    match k {
        0 => "Whitespace",
        1 | 2 => "Ident",
        3 => { "Number" },
        _ => other(),
    }
}
fn other() -> Str { if false { return "never"; } "Other" }
fn count(in s: Str) -> u64 { s.len() }
fn main() -> u64 {
    let w = ".ves".as_bytes();
    let n = name(3);
    let t = "héllo".as_str();
    let mut out = String::new();
    for k in 0..5 {
        out.push_str(name(wrapping_u8(k)));
    }
    let mut h: u64 = out.len();
    h = wrapping_add(wrapping_mul(h, 1000), w.len());
    h = wrapping_add(wrapping_mul(h, 1000), wrapping_add(n.len(), count(name(4))));
    h = wrapping_add(wrapping_mul(h, 1000), t.len());
    h = wrapping_add(wrapping_mul(h, 1000), match n.try_byte_at(0) { Option::Some(b) => widen_u64(b), Option::None => 0 });
    if n == "Number" && name(1) == name(2) && name(1) < name(0) { wrapping_add(h, 7) } else { h }
}
"#;
    let total: u64 = NAMES.iter().map(|n| n.len() as u64).sum();
    let mut h = total;
    h = h * 1000 + 4;
    h = h * 1000 + 6 + 5;
    h = h * 1000 + "héllo".len() as u64;
    h = h * 1000 + u64::from(b'N');
    assert_eq!(agree("str_results", source), h + 7);
}

#[test]
fn only_static_text_may_be_returned_as_str() {
    // A parameter view, a bound view and run-time text are not static.
    for source in [
        "fn f(in s: Str) -> Str { s } fn main() -> u64 { 0 }",
        "fn f(in s: String) -> Str { let v = s.as_str(); v } fn main() -> u64 { 0 }",
        "fn f(k: u8) -> Str { if k == 0 { \"a\" } else { let s = \"b\"; s } } fn main() -> u64 { 0 }",
        "fn f(in s: Str) -> Str { return s; } fn main() -> u64 { 0 }",
        "fn f() -> Option[Str] { Option::None } fn main() -> u64 { 0 }",
    ] {
        rejects(source, "E1530");
    }
    // A view of a mutable local stays refused.
    rejects(
        "fn main() -> u64 { let mut s = String::new(); s.push_str(\"x\"); let v = s.as_bytes(); v.len() }",
        "E1531",
    );
}

/// A deterministic byte generator (SplitMix64).
fn next(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9e3779b97f4a7c15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
    z ^ (z >> 31)
}

#[test]
fn cmp_orders_byte_views_like_memcmp_on_every_backend() {
    // Pairs of every length up to 40 (both vector steps and the scalar
    // tail), equal, prefixes, and one byte changed at every position.
    let mut state = 7u64;
    let mut pairs: Vec<(Vec<u8>, Vec<u8>)> = vec![];
    for len in 0..40usize {
        let a: Vec<u8> = (0..len)
            .map(|_| b'a' + next(&mut state) as u8 % 3)
            .collect();
        pairs.push((a.clone(), a.clone()));
        let mut longer = a.clone();
        longer.push(b'a');
        pairs.push((a.clone(), longer.clone()));
        pairs.push((longer, a.clone()));
        for at in 0..len {
            let mut b = a.clone();
            b[at] = b'a' + (b[at] - b'a' + 1 + next(&mut state) as u8 % 25) % 26;
            pairs.push((a.clone(), b));
        }
    }
    // Lengths as one byte each ('0' + len), the bytes as one literal.
    let mut lens = String::new();
    let mut bytes = String::new();
    let mut expected: u64 = 0;
    for (a, b) in &pairs {
        lens.push(char::from(b'0' + a.len() as u8));
        lens.push(char::from(b'0' + b.len() as u8));
        bytes.push_str(std::str::from_utf8(a).unwrap());
        bytes.push_str(std::str::from_utf8(b).unwrap());
        let order = match a.cmp(b) {
            std::cmp::Ordering::Less => 2u64,
            std::cmp::Ordering::Equal => 0,
            std::cmp::Ordering::Greater => 1,
        };
        expected = expected.wrapping_mul(3).wrapping_add(order);
    }
    let source = format!(
        "fn enc(x: i32) -> u64 {{ if x == -1 {{ 2 }} else if x == 0 {{ 0 }} else if x == 1 {{ 1 }} else {{ 99 }} }}
fn order(in s: Slice[u8], a: u64, m: u64, b: u64) -> u64 {{ enc(s[a..m].cmp(in s[m..b])) }}
fn len_at(in l: Slice[u8], i: u64) -> Option[u64] {{
    match l.try_get(i) {{ Option::Some(c) => Option::Some(wrapping_sub(widen_u64(c), 48)), Option::None => Option::None }}
}}
fn main() -> u64 {{
    let bytes = \"{bytes}\".as_bytes();
    let lens = \"{lens}\".as_bytes();
    let mut h: u64 = 0;
    let mut at: u64 = 0;
    let mut i: u64 = 0;
    loop {{
        match (len_at(in lens, i), len_at(in lens, wrapping_add(i, 1))) {{
            (Option::Some(la), Option::Some(lb)) => {{
                let m = wrapping_add(at, la);
                let b = wrapping_add(m, lb);
                h = wrapping_add(wrapping_mul(h, 3), order(in bytes, at, m, b));
                at = b;
                i = wrapping_add(i, 2);
            }},
            _ => {{ break; }},
        }}
    }}
    h
}}
"
    );
    assert_eq!(agree("bytes_cmp", &source), expected);
}

#[test]
fn cmp_reads_both_views_of_one_sequence_and_literal_views() {
    let source = r#"
fn enc(x: i32) -> u64 { if x < 0 { 2 } else if x == 0 { 0 } else { 1 } }
fn main() -> u64 {
    let mut s: ChunkSeq[u8] = ChunkSeq::new();
    s.push(97);
    s.push(98);
    let lit = "ab".as_bytes();
    let mut h = enc(s.cmp(in lit));
    h = wrapping_add(wrapping_mul(h, 3), enc(s.cmp(in "abc".as_bytes())));
    h = wrapping_add(wrapping_mul(h, 3), enc(lit.cmp(in "a".as_bytes())));
    h = wrapping_add(wrapping_mul(h, 3), enc(s.cmp(in s.as_slice())));
    h
}
"#;
    assert_eq!(agree("bytes_cmp_views", source), (2 * 3 + 1) * 3);
    rejects(
        "fn main() -> i32 { let mut s: ChunkSeq[u64] = ChunkSeq::new(); s.cmp(in s.as_slice()) }",
        "E1525",
    );
}
