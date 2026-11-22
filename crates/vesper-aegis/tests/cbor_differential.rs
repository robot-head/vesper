//! The port's CBOR decoder (vesper/src/cbor.ves) against ciborium, the
//! decoder it replaced and which coset still links (SH6 lane 8, #1403):
//! the same accept or reject, the same value and the same length used, on
//! edge cases, the CCA and CoRIM vendor samples and the byte strings
//! inside them with 2,000 mutations each, and 200,000 random inputs.

use coset::cbor::value::Value;
use vesper_aegis::__port_cbor_value as cbor_value;

fn ciborium(input: &[u8]) -> Option<(Value, usize)> {
    let mut rest = input;
    let v: Value = coset::cbor::de::from_reader(&mut rest).ok()?;
    Some((v, input.len() - rest.len()))
}

fn encode(v: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    coset::cbor::ser::into_writer(v, &mut out).unwrap();
    out
}

fn same(input: &[u8]) {
    let want = ciborium(input).map(|(v, n)| (encode(&v), n));
    let got = cbor_value(input).map(|(v, n)| (encode(&v), n));
    assert_eq!(got, want, "input {}", hex(input));
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Every byte string inside `v`, which may itself be CBOR.
fn byte_strings(v: &Value, out: &mut Vec<Vec<u8>>) {
    match v {
        Value::Bytes(b) => out.push(b.clone()),
        Value::Array(a) => a.iter().for_each(|x| byte_strings(x, out)),
        Value::Map(m) => m.iter().for_each(|(k, x)| {
            byte_strings(k, out);
            byte_strings(x, out);
        }),
        Value::Tag(_, x) => byte_strings(x, out),
        _ => {}
    }
}

fn samples() -> Vec<Vec<u8>> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/vendor");
    let mut files = Vec::new();
    for dir in std::fs::read_dir(&root).unwrap() {
        let dir = dir.unwrap().path();
        if !dir
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("cca-")
        {
            continue;
        }
        for f in std::fs::read_dir(&dir).unwrap() {
            let f = f.unwrap().path();
            if f.extension().is_some_and(|e| e == "cbor") {
                files.push(std::fs::read(&f).unwrap());
            }
        }
    }
    files.sort();
    let mut out = files.clone();
    for f in &files {
        if let Some((v, _)) = ciborium(f) {
            let mut inner = Vec::new();
            byte_strings(&v, &mut inner);
            out.extend(inner.into_iter().filter(|b| ciborium(b).is_some()));
        }
    }
    out
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
        (self.next() % n as u64) as usize
    }
}

#[test]
fn edge_cases_match_ciborium() {
    let depth = |n: usize, open: u8| {
        let mut b = vec![open; n];
        b.push(0);
        b
    };
    let cases: Vec<Vec<u8>> = vec![
        vec![],
        vec![0x00],
        vec![0x17],
        vec![0x18, 0x05],
        vec![0x1b, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff],
        vec![0x3b, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff],
        vec![0x1c],
        vec![0x1f],
        vec![0x3f],
        vec![0x5f, 0x41, 0x01, 0x5f, 0x41, 0x02, 0xff, 0xff],
        vec![0x5f, 0x5f, 0xff, 0xff],
        vec![0x5f, 0x61, 0x61, 0xff],
        vec![0x7f, 0x62, 0xc3, 0xa9, 0xff],
        vec![0x7f, 0x61, 0xc3, 0x61, 0xa9, 0xff],
        vec![0x62, 0xc3, 0xa9],
        vec![0x62, 0xed, 0xa0],
        vec![0x9f, 0x01, 0x02, 0xff],
        vec![0xbf, 0x01, 0x02, 0xff],
        vec![0xbf, 0x01, 0xff],
        vec![0xa1, 0x01],
        vec![0x9b, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00],
        vec![0xbb, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
        vec![0xc2, 0x40],
        vec![0xc2, 0x41, 0x00],
        vec![0xc2, 0x49, 0x01, 0, 0, 0, 0, 0, 0, 0, 0],
        vec![0xc3, 0x49, 0x01, 0, 0, 0, 0, 0, 0, 0, 0],
        vec![
            0xc3, 0x50, 0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ],
        vec![
            0xc3, 0x50, 0x7f, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ],
        vec![
            0xc2, 0x51, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1,
        ],
        vec![0xc2, 0x5f, 0x41, 0x01, 0xff],
        vec![0xc2, 0x61, 0x61],
        vec![0xc2],
        vec![0xdf],
        vec![0xc6, 0xc2, 0x41, 0x05],
        vec![0xf4],
        vec![0xf5],
        vec![0xf6],
        vec![0xf7],
        vec![0xf8, 0x14],
        vec![0xf8, 0x16],
        vec![0xf8, 0x20],
        vec![0xf0],
        vec![0xff],
        vec![0xf9, 0x7c, 0x01],
        vec![0xf9, 0x00, 0x01],
        vec![0xf9, 0x83, 0xff],
        vec![0xfa, 0x7f, 0x80, 0x00, 0x01],
        vec![0xfa, 0x00, 0x00, 0x00, 0x01],
        vec![0xfa, 0x80, 0x40, 0x00, 0x00],
        vec![0xfb, 0x7f, 0xf0, 0, 0, 0, 0, 0, 1],
        vec![0x01, 0x02],
        depth(256, 0x81),
        depth(257, 0x81),
        depth(256, 0xc6),
        depth(257, 0xc6),
        depth(300, 0x9f),
    ];
    for c in &cases {
        same(c);
    }
}

#[test]
fn vendor_tokens_and_mutations_match_ciborium() {
    let samples = samples();
    assert!(samples.len() > 30, "{} samples", samples.len());
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    for s in &samples {
        same(s);
        for _ in 0..2_000 {
            let mut m = s.clone();
            for _ in 0..=rng.below(3) {
                match rng.below(4) {
                    0 if !m.is_empty() => {
                        let i = rng.below(m.len());
                        m[i] ^= 1 << rng.below(8);
                    }
                    1 if !m.is_empty() => {
                        let i = rng.below(m.len());
                        m[i] = rng.next() as u8;
                    }
                    2 => m.truncate(rng.below(m.len() + 1)),
                    _ => {
                        let i = rng.below(m.len() + 1);
                        m.insert(i, rng.next() as u8);
                    }
                }
            }
            same(&m);
        }
    }
}

#[test]
fn random_items_match_ciborium() {
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    for _ in 0..200_000 {
        let n = rng.below(24);
        let b: Vec<u8> = (0..n).map(|_| rng.next() as u8).collect();
        same(&b);
    }
}
