//! Shared deterministic generators for the SIMD JSON differential tests.
#![allow(dead_code)]

pub struct Rng(pub u64);
impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn ws(rng: &mut Rng, out: &mut Vec<u8>) {
    for _ in 0..rng.below(3) {
        out.push(b" \t\n\r"[rng.below(4)]);
    }
}

fn string(rng: &mut Rng, out: &mut Vec<u8>) {
    out.push(b'"');
    for _ in 0..rng.below(12) {
        match rng.below(14) {
            0 => out.extend_from_slice(br"\\"),
            1 => out.extend_from_slice(br#"\""#),
            2 => out.extend_from_slice(br"\n\t\/\b\f\r"),
            3 => out.extend_from_slice(br"\u00e9"),
            4 => out.extend_from_slice(br"\ud83d\ude00"),
            5 => out.extend_from_slice("é€😀".as_bytes()),
            6 => out.extend(std::iter::repeat_n(b'x', rng.below(80))),
            _ => out.push(b"abc{}[]:, 019-"[rng.below(14)]),
        }
    }
    out.push(b'"');
}

fn value(rng: &mut Rng, out: &mut Vec<u8>, depth: usize) {
    let pick = if depth > 5 {
        rng.below(6)
    } else {
        rng.below(9)
    };
    match pick {
        0 => out.extend_from_slice(b"null"),
        1 => out.extend_from_slice(if rng.below(2) == 0 { b"true" } else { b"false" }),
        2 => out.extend_from_slice(rng.next().to_string().as_bytes()),
        3 => out.extend_from_slice(format!("{}", -(rng.below(1 << 40) as i64)).as_bytes()),
        4 => {
            let f = f64::from_bits(rng.next());
            if f.is_finite() {
                out.extend_from_slice(format!("{f:e}").as_bytes());
            } else {
                out.extend_from_slice(b"-0.5E+3");
            }
        }
        5 => string(rng, out),
        6 | 7 => {
            out.push(b'[');
            let n = rng.below(5);
            for i in 0..n {
                if i > 0 {
                    out.push(b',');
                }
                ws(rng, out);
                value(rng, out, depth + 1);
                ws(rng, out);
            }
            out.push(b']');
        }
        _ => {
            out.push(b'{');
            let n = rng.below(5);
            for i in 0..n {
                if i > 0 {
                    out.push(b',');
                }
                ws(rng, out);
                // Distinct keys by construction, with occasional escapes.
                if rng.below(4) == 0 {
                    out.extend_from_slice(format!("\"\\u006b{i}\"").as_bytes());
                } else {
                    out.extend_from_slice(format!("\"key{i}_{}\"", rng.below(100)).as_bytes());
                }
                ws(rng, out);
                out.push(b':');
                ws(rng, out);
                value(rng, out, depth + 1);
            }
            out.push(b'}');
        }
    }
}

pub fn document(rng: &mut Rng) -> Vec<u8> {
    let mut out = Vec::new();
    ws(rng, &mut out);
    value(rng, &mut out, 0);
    ws(rng, &mut out);
    out
}

pub fn mutate(rng: &mut Rng, doc: &mut Vec<u8>) {
    for _ in 0..1 + rng.below(3) {
        let at = rng.below(doc.len() + 1);
        match rng.below(5) {
            0 if at < doc.len() => {
                doc.remove(at);
            }
            1 if at < doc.len() => doc[at] = rng.next() as u8,
            2 => doc.insert(at, b"\"\\,:[]{}x\x01\xc3 0-e."[rng.below(16)]),
            3 if at < doc.len() => doc[at] = b"\"\\,:[]{}"[rng.below(8)],
            _ => doc.truncate(at),
        }
    }
}
