//! ETE / ETMv4 element decoding against streams from an encoder written here
//! from the packet tables, independently of the decoder. No trace captured
//! on Arm hardware is used.

use vesper_chrono::ete::{
    ASYNC_LEN, EteContext, EteDecoder, EteElement, EteError, InstructionSet, decode_elements,
};

#[derive(Default)]
struct Enc(Vec<u8>);

impl Enc {
    fn async_(&mut self) -> &mut Self {
        self.0.extend_from_slice(&[0; 11]);
        self.0.push(0x80);
        self
    }
    fn uleb(&mut self, mut v: u64) {
        loop {
            let b = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                self.0.push(b);
                return;
            }
            self.0.push(b | 0x80);
        }
    }
    fn trace_info(&mut self, info: Option<u64>, key: Option<u64>) -> &mut Self {
        self.0.push(0x01);
        self.0
            .push(u8::from(info.is_some()) | (u8::from(key.is_some()) << 1));
        if let Some(v) = info {
            self.uleb(v);
        }
        if let Some(v) = key {
            self.uleb(v);
        }
        self
    }
    fn trace_on(&mut self) -> &mut Self {
        self.0.push(0x04);
        self
    }
    /// Timestamp updating the low `7 * n` bits (n <= 8), or all 64 (n = 9).
    fn timestamp(&mut self, value: u64, n: usize, cc: Option<u64>) -> &mut Self {
        self.0.push(if cc.is_some() { 0x03 } else { 0x02 });
        for i in 0..n {
            if i == 8 {
                self.0.push((value >> 56) as u8);
            } else {
                let more = i + 1 < n;
                self.0
                    .push(((value >> (7 * i)) & 0x7f) as u8 | if more { 0x80 } else { 0 });
            }
        }
        if let Some(cc) = cc {
            self.uleb(cc);
        }
        self
    }
    fn long64_is0(&mut self, a: u64) -> &mut Self {
        self.0.push(0x9d);
        self.addr64_is0(a);
        self
    }
    fn addr64_is0(&mut self, a: u64) {
        self.0.push(((a >> 2) & 0x7f) as u8);
        self.0.push(((a >> 9) & 0x7f) as u8);
        for i in 2..8 {
            self.0.push((a >> (8 * i)) as u8);
        }
    }
    fn long32_is1(&mut self, a: u32) -> &mut Self {
        self.0.push(0x9b);
        self.0.push(((a >> 1) & 0x7f) as u8);
        for i in 1..4 {
            self.0.push((a >> (8 * i)) as u8);
        }
        self
    }
    fn short_is0(&mut self, a: u64, two: bool) -> &mut Self {
        self.0.push(0x95);
        self.0
            .push(((a >> 2) & 0x7f) as u8 | if two { 0x80 } else { 0 });
        if two {
            self.0.push((a >> 9) as u8);
        }
        self
    }
    fn short_is1(&mut self, a: u64) -> &mut Self {
        self.0.push(0x96);
        self.0.push(((a >> 1) & 0x7f) as u8);
        self
    }
    fn exact(&mut self, i: u8) -> &mut Self {
        self.0.push(0x90 + i);
        self
    }
    fn ctx_payload(&mut self, el: u8, sf: bool, ns: bool, vmid: Option<u8>, cid: Option<u32>) {
        self.0.push(
            el | (u8::from(sf) << 4)
                | (u8::from(ns) << 5)
                | (u8::from(vmid.is_some()) << 6)
                | (u8::from(cid.is_some()) << 7),
        );
        if let Some(v) = vmid {
            self.0.push(v);
        }
        if let Some(c) = cid {
            self.0.extend_from_slice(&c.to_le_bytes());
        }
    }
    fn context(&mut self, el: u8, vmid: Option<u8>, cid: Option<u32>) -> &mut Self {
        self.0.push(0x81);
        self.ctx_payload(el, true, true, vmid, cid);
        self
    }
    fn same_context(&mut self) -> &mut Self {
        self.0.push(0x80);
        self
    }
    fn addr_ctx64_is0(&mut self, a: u64, el: u8, cid: u32) -> &mut Self {
        self.0.push(0x85);
        self.addr64_is0(a);
        self.ctx_payload(el, true, false, None, Some(cid));
        self
    }
    fn exception(&mut self, ty: u16, ret: u64) -> &mut Self {
        self.0.push(0x06);
        let wide = ty >= 32;
        self.0
            .push((((ty & 0x1f) as u8) << 1) | if wide { 0x80 } else { 0 });
        if wide {
            self.0.push((ty >> 5) as u8 & 0x1f);
        }
        self.long64_is0(ret)
    }
    fn exception_return(&mut self) -> &mut Self {
        self.0.push(0x07);
        self
    }
    fn raw(&mut self, b: &[u8]) -> &mut Self {
        self.0.extend_from_slice(b);
        self
    }
}

fn elements(e: &Enc) -> Vec<EteElement> {
    decode_elements(&e.0)
        .unwrap()
        .into_iter()
        .map(|e| e.element)
        .collect()
}

fn atoms(e: &[EteElement]) -> String {
    e.iter()
        .map(|e| match e {
            EteElement::Atom { executed: true } => 'E',
            EteElement::Atom { executed: false } => 'N',
            _ => '?',
        })
        .collect()
}

#[test]
fn instruction_trace_elements_decode() {
    let mut e = Enc::default();
    e.async_()
        .trace_info(Some(1), Some(0x1234))
        .trace_on()
        .context(1, Some(7), Some(0xdead_beef))
        .long64_is0(0xffff_0000_0040_1000)
        .timestamp(0x0123_4567_89ab_cdef, 9, None)
        .short_is0(0xffff_0000_0040_1234, true)
        .short_is0(0x0000_0000_0000_0040, false)
        .exact(1)
        .timestamp(0x55, 1, Some(300))
        .addr_ctx64_is0(0x7000, 0, 42)
        .exception(14, 0x8000)
        .exception(0x2a5, 0x9000)
        .exception_return()
        .same_context()
        .long32_is1(0x1234_5679)
        .short_is1(0x0000_0000_0000_00f2)
        .raw(&[0x00, 0x05, 0x00, 0x03, 0x70, 0x73]);
    let got = elements(&e);
    let ctx1 = EteContext {
        el: 1,
        sixty_four: true,
        non_secure: true,
        vmid: Some(7),
        context_id: Some(0xdead_beef),
    };
    let ctx2 = EteContext {
        el: 0,
        sixty_four: true,
        non_secure: false,
        vmid: Some(7),
        context_id: Some(42),
    };
    use EteElement::*;
    use InstructionSet::*;
    assert_eq!(
        got,
        vec![
            Sync,
            TraceInfo {
                info: 1,
                key: 0x1234,
                spec: 0,
                cyct: 0
            },
            TraceOn,
            Context(ctx1),
            Address {
                address: 0xffff_0000_0040_1000,
                isa: Is0
            },
            Timestamp {
                timestamp: 0x0123_4567_89ab_cdef,
                cycle_count: None
            },
            // Two-byte short address replaces bits [16:2].
            Address {
                address: 0xffff_0000_0040_1234,
                isa: Is0
            },
            // One-byte short address replaces bits [8:2].
            Address {
                address: 0xffff_0000_0040_1240,
                isa: Is0
            },
            // Exact match on register 1: the address before the last.
            Address {
                address: 0xffff_0000_0040_1234,
                isa: Is0
            },
            // A one-byte timestamp replaces the low 7 bits.
            Timestamp {
                timestamp: 0x0123_4567_89ab_cdd5,
                cycle_count: Some(300)
            },
            Context(ctx2),
            Address {
                address: 0x7000,
                isa: Is0
            },
            Exception {
                exception_type: 14,
                preferred_return: 0x8000,
                isa: Is0
            },
            Exception {
                exception_type: 0x2a5,
                preferred_return: 0x9000,
                isa: Is0
            },
            ExceptionReturn,
            Context(ctx2),
            // A 32-bit address keeps the upper half of the previous one.
            Address {
                address: 0x1234_5678,
                isa: Is1
            },
            Address {
                address: 0x1234_56f2,
                isa: Is1
            },
            Overflow,
            Discard,
            Event { mask: 3 },
        ]
    );
}

#[test]
fn every_atom_format() {
    let mut e = Enc::default();
    e.async_().raw(&[
        0xf7, // f1: E
        0xf6, // f1: N
        0xd9, // f2: bits 01 -> E N
        0xfd, // f3: bits 101 -> E N E
        0xdc, // f4: N E E E
        0xdd, // f4: N N N N
        0xde, // f4: N E N E
        0xdf, // f4: E N E N
        0xf5, // f5: E E E E N
        0xd5, // f5: N N N N N
        0xd6, // f5: N E N E N
        0xd7, // f5: E N E N E
        0xc0, // f6: count 0 -> 3 E then E
        0xe2, // f6: count 2 -> 5 E then N
    ]);
    let got = elements(&e);
    assert_eq!(got[0], EteElement::Sync);
    assert_eq!(
        atoms(&got[1..]),
        [
            "E", "N", "EN", "ENE", "NEEE", "NNNN", "NENE", "ENEN", "EEEEN", "NNNNN", "NENEN",
            "ENENE", "EEEE", "EEEEEN"
        ]
        .concat()
    );
}

#[test]
fn malformed_streams_are_typed_errors() {
    let with = |tail: &[u8]| {
        let mut e = Enc::default();
        e.async_().raw(tail);
        decode_elements(&e.0).unwrap_err()
    };
    let at = ASYNC_LEN;
    assert_eq!(
        decode_elements(&[0x04]).unwrap_err(),
        EteError::NotSynchronized { offset: 0 }
    );
    assert!(matches!(
        decode_elements(&[0; 11]).unwrap_err(),
        EteError::NotSynchronized { .. }
    ));
    assert_eq!(EteDecoder::sync(&[1, 2, 3]).unwrap_err(), EteError::NoSync);
    assert!(matches!(with(&[0x9d, 1, 2, 3]), EteError::Truncated { .. }));
    assert!(matches!(with(&[0x95]), EteError::Truncated { .. }));
    assert!(matches!(with(&[0x95, 0x80]), EteError::Truncated { .. }));
    assert!(matches!(with(&[0x81, 0x80, 1]), EteError::Truncated { .. }));
    assert_eq!(
        with(&[0x00, 0x07]),
        EteError::MalformedExtension { offset: at }
    );
    assert_eq!(
        with(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x81]),
        EteError::MalformedExtension { offset: at }
    );
    assert_eq!(
        with(&[0x2d]),
        EteError::Unsupported {
            offset: at,
            header: 0x2d
        }
    );
    assert_eq!(
        with(&[0x9f]),
        EteError::Reserved {
            offset: at,
            header: 0x9f
        }
    );
    assert_eq!(
        with(&[0x06, 0x02, 0x04]),
        EteError::ExceptionWithoutAddress { offset: at }
    );
    assert_eq!(
        with(&[0x06, 0x02]),
        EteError::ExceptionWithoutAddress { offset: at }
    );
    assert_eq!(
        with(&[0x06, 0x82, 0x80]),
        EteError::FieldTooLong { offset: at }
    );
    assert_eq!(with(&[0x92]), EteError::EmptyAddressHistory { offset: at });
    assert_eq!(
        with(&[0x01, 0x01, 0x80, 0x80, 0x80, 0x80, 0x80, 0x00]),
        EteError::FieldTooLong { offset: at }
    );
    // Elements decoded before an error are still delivered, then the error.
    let mut e = Enc::default();
    e.async_().trace_on().raw(&[0x9f]);
    let all: Vec<_> = EteDecoder::new(&e.0).collect();
    assert!(all[0].is_ok() && all[1].is_ok() && all[2].is_err() && all.len() == 3);
    // Sync skips leading junk.
    let mut e = Enc::default();
    e.raw(&[0x13, 0x37]).async_().trace_on();
    let got: Vec<_> = EteDecoder::sync(&e.0)
        .unwrap()
        .map(|x| x.unwrap().element)
        .collect();
    assert_eq!(got, vec![EteElement::Sync, EteElement::TraceOn]);
}

#[test]
fn arbitrary_bytes_never_panic() {
    let mut state = 0x0123_4567_89ab_cdefu64;
    for len in 0..500usize {
        let mut e = Enc::default();
        e.async_();
        for _ in 0..len % 48 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            e.0.push(state as u8);
        }
        let _ = decode_elements(&e.0);
    }
}
