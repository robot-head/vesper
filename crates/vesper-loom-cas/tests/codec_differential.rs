//! Differential test of the codecs: a seeded corpus of names, object ids,
//! trees and derivation inputs (valid, invalid and mutated encodings) run
//! through the public API, with every result's `Debug` text folded into
//! one FNV-1a digest. The Rust build and the Vesper port (package
//! `vesper-loom-cas-port`) must give the same digest, which is pinned below from
//! the Rust build. A different digest means the builds disagree on some
//! input; `CODEC_DIFF_DUMP=<path>` writes the full transcript to compare.

use std::fmt::Write as _;
use std::str::FromStr;

use vesper_loom_cas::{DerivationInputs, Digest, Domain, ObjectId, Tree, blob_id, validate_name};

/// The digest of the transcript, from the Rust build.
const EXPECTED: u64 = 4_758_787_979_257_237_746;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // SplitMix64.
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len() as u64) as usize]
    }
}

/// Byte pieces names are built from: ordinary letters, the policy's
/// separators, forbidden and control characters, dots and spaces, device
/// name parts, multi-byte UTF-8 (including C1 controls) and invalid bytes.
const PIECES: &[&[u8]] = &[
    b"a",
    b"b",
    b"Z",
    b"0",
    b"9",
    b".",
    b" ",
    b"/",
    b"\\",
    b":",
    b"<",
    b"?",
    b"*",
    b"\"",
    b"|",
    b"\0",
    b"\x1f",
    b"\x7f",
    b"\t",
    b"CON",
    b"con",
    b"Nul",
    b"PRN",
    b"aux",
    b"COM",
    b"lpt",
    b"1",
    b"\xc2\x85",
    b"\xc2\xa0",
    b"\xc3\xa9",
    b"\xc3\x89",
    b"\xce\xbb",
    b"\xe2\x82\xac",
    b"\xf0\x9f\x98\x80",
    b"\xff",
    b"\xc0\xaf",
    b"\xed\xa0\x80",
    b"\xe0\x80\x80",
    b"\xf4\x90\x80\x80",
    b"README",
    b"readme",
    b"x",
];

fn raw_name(r: &mut Rng) -> Vec<u8> {
    let n = r.below(6);
    let mut out = Vec::new();
    for _ in 0..n {
        out.extend_from_slice(r.pick(PIECES));
    }
    if r.below(40) == 0 {
        out = vec![b'x'; 250 + r.below(10) as usize];
    }
    out
}

fn text(r: &mut Rng) -> String {
    loop {
        if let Ok(s) = String::from_utf8(raw_name(r)) {
            return s;
        }
    }
}

const DOMAINS: [Domain; 3] = [Domain::Source, Domain::Interface, Domain::Artifact];

fn mutations(r: &mut Rng, bytes: &[u8]) -> Vec<Vec<u8>> {
    let mut out = vec![bytes.to_vec()];
    if !bytes.is_empty() {
        let mut flipped = bytes.to_vec();
        let at = r.below(bytes.len() as u64) as usize;
        flipped[at] = r.next() as u8;
        out.push(flipped);
        let cut = r.below(bytes.len() as u64) as usize;
        out.push(bytes[..cut].to_vec());
    }
    let mut longer = bytes.to_vec();
    longer.push(r.next() as u8);
    out.push(longer);
    out
}

fn names_and_ids(r: &mut Rng, log: &mut String) {
    for _ in 0..3000 {
        let raw = raw_name(r);
        match std::str::from_utf8(&raw) {
            Ok(s) => writeln!(log, "name {s:?} {:?}", validate_name(s)).unwrap(),
            Err(_) => writeln!(log, "name {raw:?} not utf8").unwrap(),
        }
    }
    for _ in 0..500 {
        let d = *r.pick(&DOMAINS);
        let bytes = raw_name(r);
        let id = blob_id(d, &bytes);
        let shown = id.to_string();
        writeln!(
            log,
            "blob {d:?} {bytes:?} {shown} {:?}",
            ObjectId::from_str(&shown)
        )
        .unwrap();
        let tree = shown.replace(":blob:", ":tree:");
        writeln!(log, "tree id {:?}", ObjectId::from_str(&tree)).unwrap();
        for m in mutations(r, shown.as_bytes()) {
            if let Ok(s) = String::from_utf8(m) {
                writeln!(log, "id {s:?} {:?}", ObjectId::from_str(&s)).unwrap();
                let hex = s.rsplit(':').next().unwrap_or("");
                writeln!(log, "hex {:?}", Digest::from_hex(hex)).unwrap();
            }
        }
    }
    for s in [
        "",
        "b3v1",
        "b3v1:source:blob:",
        "b3v2:source:blob:00",
        "b3v1:Source:blob:00",
    ] {
        writeln!(log, "id {s:?} {:?}", ObjectId::from_str(s)).unwrap();
    }
}

fn trees(r: &mut Rng, log: &mut String) {
    for _ in 0..600 {
        let d = *r.pick(&DOMAINS);
        let mut tree = Tree::new(d);
        for _ in 0..r.below(6) {
            let name = text(r);
            let result = if r.below(3) == 0 {
                let mut child = Tree::new(d);
                let _ = child.insert_file("f", blob_id(d, b"f"), 1, false);
                let child_id = if r.below(10) == 0 {
                    blob_id(d, b"x")
                } else {
                    child.id()
                };
                tree.insert_dir(&name, child_id)
            } else {
                let other = if r.below(10) == 0 {
                    *r.pick(&DOMAINS)
                } else {
                    d
                };
                tree.insert_file(
                    &name,
                    blob_id(other, name.as_bytes()),
                    r.below(1 << 20),
                    r.below(2) == 0,
                )
            };
            writeln!(log, "insert {name:?} {result:?}").unwrap();
        }
        let bytes = tree.encode();
        writeln!(log, "tree {bytes:?} {}", tree.id()).unwrap();
        for m in mutations(r, &bytes) {
            writeln!(log, "decode {:?}", Tree::decode(d, &m)).unwrap();
        }
        // Splice an arbitrary name into a one-entry encoding.
        let raw = raw_name(r);
        let mut one = b"VLTR\x01\0\0\0\x01\0\0\0\0\0\0\0".to_vec();
        one.extend_from_slice(&(raw.len() as u16).to_le_bytes());
        one.extend_from_slice(&raw);
        one.push(1 + r.below(4) as u8);
        one.extend_from_slice(&[7; 32]);
        one.extend_from_slice(&r.below(3).to_le_bytes());
        writeln!(log, "decode1 {:?}", Tree::decode(d, &one)).unwrap();
        // Two entries in either order, often equal up to case.
        let a = text(r);
        let b = if r.below(2) == 0 {
            a.to_uppercase()
        } else {
            text(r)
        };
        let (a, b) = if r.below(4) != 0 && b < a {
            (b, a)
        } else {
            (a, b)
        };
        let mut two = b"VLTR\x01\0\0\0\x02\0\0\0\0\0\0\0".to_vec();
        for n in [&a, &b] {
            two.extend_from_slice(&(n.len() as u16).to_le_bytes());
            two.extend_from_slice(n.as_bytes());
            two.push(1);
            two.extend_from_slice(&[9; 32]);
            two.extend_from_slice(&0u64.to_le_bytes());
        }
        writeln!(log, "decode2 {:?}", Tree::decode(d, &two)).unwrap();
    }
}

fn label(r: &mut Rng) -> String {
    match r.below(30) {
        0 => String::new(),
        1 => "y".repeat(4096 + r.below(2) as usize),
        2 => format!("{}\u{85}", "é".repeat(70)),
        3..=9 => text(r),
        _ => {
            let n = 1 + r.below(8);
            (0..n)
                .map(|_| *r.pick(&['a', 'b', 'z', '-', '.', '0', 'é', 'λ']))
                .collect()
        }
    }
}

fn derivations(r: &mut Rng, log: &mut String) {
    for _ in 0..600 {
        let d = *r.pick(&DOMAINS);
        let mut v = DerivationInputs::new(d, label(r), label(r));
        if r.below(2) == 0 {
            v = v.with_target(label(r));
        }
        for _ in 0..r.below(4) {
            v = v.with_option(label(r), label(r));
        }
        for _ in 0..(r.below(4) + u64::from(r.below(8) != 0)) {
            let name = label(r);
            v = v.with_input(name.clone(), blob_id(*r.pick(&DOMAINS), name.as_bytes()));
        }
        let encoded = v.encode();
        writeln!(
            log,
            "drv {v:?} {encoded:?} {:?}",
            v.key().map(|k| k.to_string())
        )
        .unwrap();
        // A valid encoding with the output domain set to source, to reach
        // the decoder's validation.
        let bytes = match encoded {
            Ok(b) => b,
            Err(_) => continue,
        };
        let mut source = bytes.clone();
        source[8] = 1;
        writeln!(log, "decode-src {:?}", DerivationInputs::decode(&source)).unwrap();
        for m in mutations(r, &bytes) {
            writeln!(log, "decode {:?}", DerivationInputs::decode(&m)).unwrap();
        }
        // The schema, the domain tag and the last input id's tags.
        for (at, value) in [(4, r.below(3) as u8), (8, r.below(5) as u8)] {
            let mut m = bytes.clone();
            m[at] = value;
            writeln!(
                log,
                "decode-at {at} {value} {:?}",
                DerivationInputs::decode(&m)
            )
            .unwrap();
        }
        let id_at = bytes.len() - 34 + r.below(2) as usize;
        let mut m = bytes.clone();
        m[id_at] = r.below(5) as u8;
        writeln!(log, "decode-id {:?}", DerivationInputs::decode(&m)).unwrap();
    }
    // Unsorted maps and non-UTF-8 or oversized strings, written by hand.
    let id = blob_id(Domain::Source, b"s");
    for (k1, k2) in [("b", "a"), ("a", "a"), ("a", "b")] {
        let mut v = DerivationInputs::new(Domain::Artifact, "c", "1")
            .with_input("x", id)
            .encode()
            .unwrap();
        // Replace the empty option map with two entries in this order.
        let opts = 4 + 4 + 1 + 5 + 5 + 1;
        let mut tail = v.split_off(opts + 4);
        v.truncate(opts);
        v.extend_from_slice(&2u32.to_le_bytes());
        for k in [k1, k2] {
            for s in [k, "v"] {
                v.extend_from_slice(&(s.len() as u32).to_le_bytes());
                v.extend_from_slice(s.as_bytes());
            }
        }
        v.append(&mut tail);
        writeln!(log, "unsorted {k1} {k2} {:?}", DerivationInputs::decode(&v)).unwrap();
    }
    for bad in [&b"\xff\xfe"[..], &b"ok"[..]] {
        let mut v = b"VLDI\x01\0\0\0\x03".to_vec();
        v.extend_from_slice(&(bad.len() as u32).to_le_bytes());
        v.extend_from_slice(bad);
        v.extend_from_slice(&5000u32.to_le_bytes());
        writeln!(log, "strings {:?}", DerivationInputs::decode(&v)).unwrap();
    }
}

fn fnv(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

#[test]
fn codecs_agree_with_the_rust_build() {
    let mut r = Rng(0x5eed_1411);
    let mut log = String::new();
    names_and_ids(&mut r, &mut log);
    trees(&mut r, &mut log);
    derivations(&mut r, &mut log);
    if let Ok(path) = std::env::var("CODEC_DIFF_DUMP") {
        std::fs::write(path, &log).unwrap();
    }
    assert_eq!(
        fnv(log.as_bytes()),
        EXPECTED,
        "transcript of {} bytes",
        log.len()
    );
}
