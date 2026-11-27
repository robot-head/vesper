//! Differential and ABBA flip-gate tests for `@vesper/beacon-arrow::cbor`
//! (Rust-exit SH6 lane 18, #1413 / #2311).
use std::hint::black_box;
use std::time::Instant;
use vesper_beacon_arrow::otap_cbor_scan;

const SCAN_OK: u64 = 0;
const SCAN_BAD_VALUE: u64 = 1;
const SCAN_TOO_LARGE: u64 = 2;

const TEST_NODE_WEIGHT: u64 = 32;
const TEST_MAX_DEPTH: u64 = 32;

const ABBA_WARMUP_EPOCHS: usize = 2;
const ABBA_EPOCHS: usize = 30;
const ABBA_OPS_PER_WINDOW: usize = 512;

mod reference {
    use super::{SCAN_BAD_VALUE, SCAN_OK, SCAN_TOO_LARGE};

    struct Scan<'a> {
        bytes: &'a [u8],
        left: usize,
        node_weight: usize,
        max_depth: usize,
    }

    impl Scan<'_> {
        fn byte(&mut self) -> Result<u8, u64> {
            let (&b, rest) = self.bytes.split_first().ok_or(SCAN_BAD_VALUE)?;
            self.bytes = rest;
            Ok(b)
        }
        fn skip(&mut self, n: u64) -> Result<usize, u64> {
            let n = usize::try_from(n).map_err(|_| SCAN_BAD_VALUE)?;
            if n > self.bytes.len() {
                return Err(SCAN_BAD_VALUE);
            }
            self.bytes = &self.bytes[n..];
            Ok(n)
        }
        fn arg(&mut self, info: u8) -> Result<Option<u64>, u64> {
            let width = match info {
                0..=23 => return Ok(Some(u64::from(info))),
                24 => 1,
                25 => 2,
                26 => 4,
                27 => 8,
                31 => return Ok(None),
                _ => return Err(SCAN_BAD_VALUE),
            };
            let mut v = 0u64;
            for _ in 0..width {
                v = (v << 8) | u64::from(self.byte()?);
            }
            Ok(Some(v))
        }
        fn charge(&mut self, bytes: usize) -> Result<(), u64> {
            self.left = self
                .left
                .checked_sub(bytes.saturating_add(self.node_weight))
                .ok_or(SCAN_TOO_LARGE)?;
            Ok(())
        }
        fn item(&mut self, depth: usize) -> Result<bool, u64> {
            if depth > self.max_depth {
                return Err(SCAN_BAD_VALUE);
            }
            let b = self.byte()?;
            let (major, info) = (b >> 5, b & 0x1f);
            match major {
                0 | 1 => {
                    self.arg(info)?.ok_or(SCAN_BAD_VALUE)?;
                    self.charge(0)?;
                }
                2 | 3 => match self.arg(info)? {
                    Some(n) => {
                        let n = self.skip(n)?;
                        self.charge(n)?;
                    }
                    None => {
                        let mut total = 0usize;
                        loop {
                            let c = self.byte()?;
                            if c == 0xff {
                                break;
                            }
                            if c >> 5 != major {
                                return Err(SCAN_BAD_VALUE);
                            }
                            let n = self.arg(c & 0x1f)?.ok_or(SCAN_BAD_VALUE)?;
                            total = total.saturating_add(self.skip(n)?);
                        }
                        self.charge(total)?;
                    }
                },
                4 | 5 => {
                    self.charge(0)?;
                    let per = if major == 5 { 2 } else { 1 };
                    match self.arg(info)? {
                        Some(n) => {
                            let items = n.checked_mul(per).ok_or(SCAN_BAD_VALUE)?;
                            for _ in 0..items {
                                if !self.item(depth + 1)? {
                                    return Err(SCAN_BAD_VALUE);
                                }
                            }
                        }
                        None => {
                            while self.item(depth + 1)? {
                                if per == 2 && !self.item(depth + 1)? {
                                    return Err(SCAN_BAD_VALUE);
                                }
                            }
                        }
                    }
                }
                7 => match info {
                    31 => return Ok(false),
                    20..=22 => self.charge(0)?,
                    25..=27 => {
                        self.skip(1 << (info - 24))?;
                        self.charge(0)?;
                    }
                    _ => return Err(SCAN_BAD_VALUE),
                },
                _ => return Err(SCAN_BAD_VALUE),
            }
            Ok(true)
        }
    }

    pub fn scan(bytes: &[u8], budget: u64, node_weight: u64, max_depth: u64) -> u64 {
        let mut s = Scan {
            bytes,
            left: budget as usize,
            node_weight: node_weight as usize,
            max_depth: max_depth as usize,
        };
        match s.item(1) {
            Ok(true) if s.bytes.is_empty() => SCAN_OK,
            Ok(_) => SCAN_BAD_VALUE,
            Err(code) => code,
        }
    }
}

fn sample_nested_cbor() -> Vec<u8> {
    // Definite map of 8 entries: text keys -> arrays of ints, floats, strings, bytes, bools, nulls.
    let mut out = Vec::new();
    out.push(0xa8); // map(8)
    for i in 0u8..8 {
        out.push(0x62); // text(2)
        out.push(b'k');
        out.push(b'0' + i);
        out.push(0x84); // array(4)
        out.push(0x18); // uint8
        out.push(i.wrapping_mul(17));
        out.push(0xfb); // float64
        out.extend_from_slice(&(f64::from(i) * 1.25).to_be_bytes());
        out.push(0x64); // text(4)
        out.extend_from_slice(b"val!");
        out.push(if i % 2 == 0 { 0xf5 } else { 0xf6 }); // true / null
    }
    out
}

#[test]
fn cbor_scan_matches_rust_reference() {
    let nested = sample_nested_cbor();
    let deep_ok = {
        let mut v = vec![0x81; TEST_MAX_DEPTH as usize - 1];
        v.push(0x00);
        v
    };
    let deep_bad = {
        let mut v = vec![0x81; TEST_MAX_DEPTH as usize];
        v.push(0x00);
        v
    };
    let cases: &[(&[u8], u64)] = &[
        (&[], 1024),
        (&[0x00], 1024),
        (&[0x00], 16), // too large for node_weight=32
        (&[0x20], 1024),
        (&[0x1b, 0, 0, 0, 0, 0, 0, 0, 1], 1024),
        (&[0x1b, 0, 0, 0], 1024), // truncated uint64
        (&[0x43, 1, 2, 3], 1024),
        (&[0x43, 1, 2], 1024), // truncated bytes
        (&[0x63, b'a', b'b', b'c'], 1024),
        (&[0x7f, 0x62, b'h', b'i', 0x61, b'!', 0xff], 1024), // indefinite text
        (&[0x7f, 0x41, 0x00, 0xff], 1024),                   // mismatched chunk major
        (&[0x9f, 0x01, 0x02, 0xff], 1024),                   // indefinite array
        (&[0xbf, 0x61, b'a', 0x01, 0xff], 1024),             // indefinite map
        (&[0xbf, 0x61, b'a', 0xff], 1024),                   // odd indefinite map
        (&[0xf4], 1024),                                     // false
        (&[0xf5], 1024),                                     // true
        (&[0xf6], 1024),                                     // null
        (&[0xf7], 1024),                                     // undefined (rejected)
        (&[0xf9, 0x3c, 0x00], 1024),                         // float16
        (&[0xfa, 0x3f, 0x80, 0x00, 0x00], 1024),             // float32
        (&[0xfb, 0, 0, 0, 0, 0, 0, 0, 0], 1024),             // float64
        (&[0xff], 1024),                                     // stray break
        (&[0x00, 0x00], 1024),                               // trailing byte
        (&[0xc0, 0x61, b'x'], 1024),                         // tag (rejected)
        (&nested, 4096),
        (&nested, 128), // budget exceeded mid-map
        (&deep_ok, 4096),
        (&deep_bad, 4096),
    ];

    for &(bytes, budget) in cases {
        let got = otap_cbor_scan(bytes, budget, TEST_NODE_WEIGHT, TEST_MAX_DEPTH);
        let want = reference::scan(bytes, budget, TEST_NODE_WEIGHT, TEST_MAX_DEPTH);
        assert_eq!(got, want, "bytes={bytes:02x?}, budget={budget}");
    }
    println!("flip-gate differential differences=0 cases={}", cases.len());
}

#[test]
#[ignore = "benchmark; run in release with --ignored --nocapture"]
fn abba_cbor_scan() {
    let payload = sample_nested_cbor();
    let budget = 8192u64;

    let run_rust = || {
        for _ in 0..ABBA_OPS_PER_WINDOW {
            let r = reference::scan(
                black_box(&payload),
                black_box(budget),
                TEST_NODE_WEIGHT,
                TEST_MAX_DEPTH,
            );
            assert_eq!(black_box(r), SCAN_OK);
        }
    };
    let run_vesper = || {
        for _ in 0..ABBA_OPS_PER_WINDOW {
            let r = otap_cbor_scan(
                black_box(&payload),
                black_box(budget),
                TEST_NODE_WEIGHT,
                TEST_MAX_DEPTH,
            );
            assert_eq!(black_box(r), SCAN_OK);
        }
    };

    for _ in 0..ABBA_WARMUP_EPOCHS {
        run_rust();
        run_vesper();
        run_vesper();
        run_rust();
    }

    let mut pairs = Vec::with_capacity(ABBA_EPOCHS);
    for _ in 0..ABBA_EPOCHS {
        let t0 = Instant::now();
        run_rust();
        let a1 = t0.elapsed().as_nanos().max(1);
        let t1 = Instant::now();
        run_vesper();
        let b1 = t1.elapsed().as_nanos().max(1);
        let t2 = Instant::now();
        run_vesper();
        let b2 = t2.elapsed().as_nanos().max(1);
        let t3 = Instant::now();
        run_rust();
        let a2 = t3.elapsed().as_nanos().max(1);
        pairs.push(format!("{}:{}", a1 + a2, b1 + b2));
    }
    println!("flip-gate pairs otap_cbor_scan {}", pairs.join(" "));
}
