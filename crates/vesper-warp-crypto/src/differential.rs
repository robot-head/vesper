//! Differential tests of the Vesper port against the Rust code it replaced
//! (`crate::reference`, the last Rust commit's modules). Each test runs
//! both on the same seeded cases, including every error path, and compares
//! results, errors, buffers and state.

use crate::reference as r;
use crate::{
    Cipher, InitialSecrets, OneRttKeys, Opener, Sealer, Side, TrafficSecret, retry_integrity_tag,
    verify_retry_integrity,
};

const CIPHERS: [Cipher; 3] = [
    Cipher::Aes128Gcm,
    Cipher::Aes256Gcm,
    Cipher::ChaCha20Poly1305,
];

/// Counts the outcomes a test reached, so it can assert that every error
/// path occurred.
#[derive(Default)]
struct Tally(std::collections::BTreeMap<String, u64>);

impl Tally {
    fn note<T, E: std::fmt::Debug>(&mut self, what: &str, r: &Result<T, E>) {
        let key = match r {
            Ok(_) => format!("{what} ok"),
            Err(e) => {
                let e = format!("{e:?}");
                let name = e.split(['(', ' ', '{']).next().unwrap_or("").to_string();
                format!("{what} {name}")
            }
        };
        *self.0.entry(key).or_default() += 1;
    }
    fn assert_reached(&self, keys: &[&str]) {
        for k in keys {
            assert!(self.0.contains_key(*k), "never reached {k}: {:?}", self.0);
        }
    }
}

/// xorshift64*: deterministic cases without a dependency.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.next() as u8).collect()
    }
    fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }
}

fn secrets(c: Cipher, bytes: &[u8]) -> (TrafficSecret, r::schedule::TrafficSecret) {
    (
        TrafficSecret::new(c, bytes).unwrap(),
        r::schedule::TrafficSecret::new(c, bytes).unwrap(),
    )
}

/// The public fields of both `Unprotected`s.
fn fields_v(h: &crate::Unprotected) -> (usize, usize, u64, u8) {
    (h.pn_offset, h.pn_len, h.truncated_pn, h.first_byte)
}
fn fields_r(h: &r::protect::Unprotected) -> (usize, usize, u64, u8) {
    (h.pn_offset, h.pn_len, h.truncated_pn, h.first_byte)
}

#[test]
fn cipher_tables_match_the_rust_constants() {
    let want = [
        (16, 32, 1 << 23, 1 << 52),
        (32, 48, 1 << 23, 1 << 52),
        (32, 32, crate::PACKET_NUMBER_LIMIT, 1 << 36),
    ];
    for (c, w) in CIPHERS.into_iter().zip(want) {
        let got = (
            c.key_len(),
            c.secret_len(),
            c.confidentiality_limit(),
            c.integrity_limit(),
        );
        assert_eq!(got, w, "{c:?}");
    }
}

#[test]
fn key_schedule_matches_the_rust_schedule() {
    let mut rng = Rng(0x5eed_0001);
    for case in 0..600 {
        let c = CIPHERS[case % 3];
        // Mostly valid secrets; some of the wrong length.
        let len = if rng.chance(10) {
            rng.below(64) as usize
        } else {
            c.secret_len()
        };
        let bytes = rng.bytes(len);
        let v = TrafficSecret::new(c, &bytes);
        let rr = r::schedule::TrafficSecret::new(c, &bytes);
        match (v, rr) {
            (Ok(v), Ok(rr)) => {
                let (mv, mr) = (v.key_material(), rr.key_material());
                assert_eq!(mv.key(), mr.key(), "case {case}");
                assert_eq!(mv.iv(), mr.iv(), "case {case}");
                assert_eq!(mv.hp(), mr.hp(), "case {case}");
                let (nv, nr) = (v.next_phase(), rr.next_phase());
                assert_eq!(nv.as_bytes(), nr.as_bytes(), "case {case}");
                assert_eq!(nv.next_phase().as_bytes(), nr.next_phase().as_bytes());
            }
            (Err(a), Err(b)) => assert_eq!(a, b, "case {case}"),
            _ => panic!("case {case}: one side failed"),
        }
    }
    for len in 0..=24 {
        let dcid = rng.bytes(len);
        let v = InitialSecrets::new_v1(&dcid);
        let rr = r::schedule::InitialSecrets::new_v1(&dcid);
        match (v, rr) {
            (Ok(v), Ok(rr)) => {
                assert_eq!(v.client.as_bytes(), rr.client.as_bytes());
                assert_eq!(v.server.as_bytes(), rr.server.as_bytes());
            }
            (Err(a), Err(b)) => assert_eq!(a, b),
            _ => panic!("dcid length {len}: one side failed"),
        }
        assert_eq!(
            InitialSecrets::initial_secret_v1(&dcid),
            r::schedule::InitialSecrets::initial_secret_v1(&dcid)
        );
    }
}

#[test]
fn packet_and_header_protection_match_the_rust_code() {
    let mut rng = Rng(0x5eed_0002);
    for case in 0..3_000 {
        let c = CIPHERS[case % 3];
        let (v, rr) = secrets(c, &rng.bytes(c.secret_len()));
        let (kv, kr) = (v.keys(), rr.keys());
        let pn = match rng.below(4) {
            0 => rng.below(256),
            1 => crate::PACKET_NUMBER_LIMIT - 1 - rng.below(2),
            2 => crate::PACKET_NUMBER_LIMIT + rng.below(3),
            _ => rng.next() >> 2,
        };
        assert_eq!(kv.packet.nonce(pn), kr.packet.nonce(pn));
        let len = rng.below(80) as usize;
        let mut bv = rng.bytes(len);
        let mut br = bv.clone();
        let header_len = rng.below(30) as usize;
        let payload_len = if rng.chance(5) {
            usize::MAX - rng.below(4) as usize
        } else {
            rng.below(60) as usize
        };
        assert_eq!(
            kv.packet.seal(pn, &mut bv, header_len, payload_len),
            kr.packet.seal(pn, &mut br, header_len, payload_len),
            "seal case {case}"
        );
        assert_eq!(bv, br, "seal case {case}");
        if rng.chance(30) && !bv.is_empty() {
            let i = rng.below(bv.len() as u64) as usize;
            bv[i] ^= 1 << rng.below(8);
            br[i] = bv[i];
        }
        let open_len = rng.below(30) as usize;
        assert_eq!(
            kv.packet.open(pn, &mut bv, open_len),
            kr.packet.open(pn, &mut br, open_len),
            "open case {case}"
        );
        assert_eq!(bv, br, "open case {case}");

        // Header protection on arbitrary bytes and offsets.
        let pn_offset = if rng.chance(5) {
            usize::MAX - rng.below(30) as usize
        } else {
            rng.below(40) as usize
        };
        let mut sample = [0u8; 16];
        sample.copy_from_slice(&rng.bytes(16));
        assert_eq!(kv.header.mask(&sample), kr.header.mask(&sample));
        assert_eq!(
            kv.header.apply(&mut bv, pn_offset),
            kr.header.apply(&mut br, pn_offset),
            "apply case {case}"
        );
        assert_eq!(bv, br, "apply case {case}");
        let before = bv.clone();
        match (
            kv.header.remove(&mut bv, pn_offset),
            kr.header.remove(&mut br, pn_offset),
        ) {
            (Ok(hv), Ok(hr)) => {
                assert_eq!(fields_v(&hv), fields_r(&hr), "remove case {case}");
                assert_eq!(bv, br, "remove case {case}");
                let full = rng.next() >> 2;
                assert_eq!(hv.encodes(full), hr.encodes(full));
                assert_eq!(hv.encodes(hv.truncated_pn), hr.encodes(hr.truncated_pn));
                assert_eq!(hv.key_phase(), hr.key_phase());
                hv.reapply(&mut bv);
                hr.reapply(&mut br);
                assert_eq!(bv, before, "reapply case {case}");
                assert_eq!(br, before, "reapply case {case}");
            }
            (Err(a), Err(b)) => {
                assert_eq!(a, b, "remove case {case}");
                assert_eq!(bv, br, "remove case {case}");
            }
            _ => panic!("remove case {case}: one side failed"),
        }
        let pl = 1 + rng.below(4) as usize;
        let at = rng.below(40) as usize;
        assert_eq!(
            crate::protect::truncated_matches(&bv, at, pl, pn),
            r::protect::truncated_matches(&bv, at, pl, pn)
        );
    }
}

/// Writes a header with a `pn_len`-byte packet number at `pn_offset`.
fn header(buf: &mut [u8], first: u8, pn_offset: usize, pn_len: usize, pn: u64) {
    if buf.is_empty() {
        return;
    }
    buf[0] = (first & !3) | (pn_len as u8 - 1);
    let be = pn.to_be_bytes();
    for i in 0..pn_len {
        if let Some(b) = buf.get_mut(pn_offset + i) {
            *b = be[8 - pn_len + i];
        }
    }
}

#[test]
fn sealers_and_openers_match_the_rust_ticket_rules() {
    let mut rng = Rng(0x5eed_0003);
    let mut tally = Tally::default();
    for case in 0..120 {
        let dcid = rng.bytes(8);
        let (sv, ov) = {
            let s = InitialSecrets::new_v1(&dcid).unwrap();
            (s.keys_for(Side::Client).0, s.keys_for(Side::Server).1)
        };
        let (sr, or) = {
            let s = r::schedule::InitialSecrets::new_v1(&dcid).unwrap();
            (s.keys_for(Side::Client).0, s.keys_for(Side::Server).1)
        };
        let limit = 5 + rng.below(40);
        let mut sealer_v = Sealer::new(sv).with_confidentiality_limit(limit);
        let mut sealer_r = r::ticket::Sealer::new(sr).with_confidentiality_limit(limit);
        let ilimit = 1 + rng.below(6);
        let mut opener_v = Opener::new(ov).with_integrity_limit(ilimit);
        let mut opener_r = r::ticket::Opener::new(or).with_integrity_limit(ilimit);
        for step in 0..120 {
            let at = format!("case {case} step {step}");
            assert_eq!(
                sealer_v.remaining_under_key(),
                sealer_r.remaining_under_key()
            );
            let next = sealer_v.next_packet_number();
            assert_eq!(next, sealer_r.next_packet_number(), "{at}");
            let pn = match rng.below(6) {
                0 => next.saturating_sub(1),
                1 => next.saturating_sub(1 + rng.below(5)),
                2 => next + rng.below(3),
                3 => crate::PACKET_NUMBER_LIMIT + rng.below(2),
                _ => next,
            };
            let (tv, tr) = if rng.chance(50) {
                (sealer_v.next_ticket(), sealer_r.next_ticket())
            } else {
                (sealer_v.ticket(pn), sealer_r.ticket(pn))
            };
            tally.note("ticket", &tv);
            let (tv, tr) = match (tv, tr) {
                (Ok(tv), Ok(tr)) => {
                    assert_eq!(tv.packet_number(), tr.packet_number(), "{at}");
                    (tv, tr)
                }
                (Err(a), Err(b)) => {
                    assert_eq!(a, b, "{at}");
                    continue;
                }
                _ => panic!("{at}: one ticket failed"),
            };
            if rng.chance(15) {
                assert!(sealer_v.abandon(tv).is_ok() && sealer_r.abandon(tr).is_ok());
                continue;
            }
            let pn_offset = if rng.chance(5) {
                0
            } else {
                1 + rng.below(20) as usize
            };
            let pn_len = 1 + rng.below(4) as usize;
            let payload = rng.below(40) as usize;
            let spare = if rng.chance(10) { rng.below(16) } else { 16 } as usize;
            let mut bv = rng.bytes(pn_offset + pn_len + payload + spare);
            let wire_pn = if rng.chance(5) {
                tv.packet_number() + 1
            } else {
                tv.packet_number()
            };
            header(&mut bv, 0xc0, pn_offset, pn_len, wire_pn);
            let mut br = bv.clone();
            let (lv, lr) = (
                sealer_v.seal(tv, &mut bv, pn_offset, payload),
                sealer_r.seal(tr, &mut br, pn_offset, payload),
            );
            assert_eq!(bv, br, "{at}");
            tally.note("seal", &lv.as_ref().map_err(|e| e.error));
            let len = match (lv, lr) {
                (Ok(a), Ok(b)) => {
                    assert_eq!(a, b, "{at}");
                    a
                }
                (Err(a), Err(b)) => {
                    assert_eq!(a.error, b.error, "{at}");
                    let _ = sealer_v.abandon(a.ticket);
                    let _ = sealer_r.abandon(b.ticket);
                    continue;
                }
                _ => panic!("{at}: one seal failed"),
            };
            let (a, b) = (sealer_v.audit(), sealer_r.audit());
            assert_eq!(
                (a.issued, a.sealed, a.abandoned, a.outstanding),
                (b.issued, b.sealed, b.abandoned, b.outstanding),
                "{at}"
            );
            // Receive it, sometimes tampered or with a wrong packet number.
            bv.truncate(len);
            if rng.chance(20) {
                let i = rng.below(len as u64) as usize;
                bv[i] ^= 1 << rng.below(8);
            }
            br = bv.clone();
            let sent = wire_pn;
            let skew = u64::from(rng.chance(10));
            let decline = rng.chance(5);
            let guess =
                |h_pn: u64, h_len: usize| (!decline).then(|| reconstruct(sent, h_pn, h_len) + skew);
            let ov = opener_v.open_packet(&mut bv, pn_offset, |h| guess(h.truncated_pn, h.pn_len));
            let ore = opener_r.open_packet(&mut br, pn_offset, |h| guess(h.truncated_pn, h.pn_len));
            tally.note("open", &ov);
            match (ov, ore) {
                (Ok(a), Ok(b)) => {
                    assert_eq!(
                        (a.packet_number, a.header_len, a.payload_len, a.first_byte),
                        (b.packet_number, b.header_len, b.payload_len, b.first_byte),
                        "{at}"
                    );
                }
                (Err(a), Err(b)) => assert_eq!(a, b, "{at}"),
                (a, b) => panic!("{at}: {a:?} vs {b:?}"),
            }
            assert_eq!(bv, br, "{at}");
            assert_eq!(
                opener_v.authentication_failures(),
                opener_r.authentication_failures(),
                "{at}"
            );
            assert_eq!(opener_v.largest_opened(), opener_r.largest_opened(), "{at}");
        }
    }
    tally.assert_reached(&[
        "ticket ok",
        "ticket PacketNumberReused",
        "ticket PacketNumberRegression",
        "ticket PacketNumberOutOfRange",
        "ticket ConfidentialityLimitReached",
        "seal ok",
        "seal BufferTooShort",
        "seal InvalidPacketNumberOffset",
        "seal PacketNumberMismatch",
        "open ok",
        "open AuthenticationFailed",
        "open PacketNumberReconstruction",
        "open PacketNumberMismatch",
        "open IntegrityLimitReached",
    ]);
}

/// The reconstruction both sides use: the sent packet number's high bits
/// with the received truncated value.
fn reconstruct(sent: u64, truncated: u64, pn_len: usize) -> u64 {
    let bits = 8 * pn_len as u32;
    (sent & !((1u64 << bits) - 1)) | truncated
}

/// One endpoint pair per implementation, with the same secrets.
struct Pair {
    cv: OneRttKeys,
    sv: OneRttKeys,
    cr: r::update::OneRttKeys,
    sr: r::update::OneRttKeys,
}

impl Pair {
    fn new(c: Cipher, rng: &mut Rng) -> Self {
        let a = rng.bytes(c.secret_len());
        let b = rng.bytes(c.secret_len());
        let window = 1 + rng.below(50);
        let climit = 3 + rng.below(30);
        let ilimit = 2 + rng.below(8);
        let mk = |x: &[u8], y: &[u8]| {
            let (xv, xr) = secrets(c, x);
            let (yv, yr) = secrets(c, y);
            (
                OneRttKeys::new(xv, yv, window)
                    .unwrap()
                    .with_confidentiality_limit(climit)
                    .with_integrity_limit(ilimit),
                r::update::OneRttKeys::new(xr, yr, window)
                    .unwrap()
                    .with_confidentiality_limit(climit)
                    .with_integrity_limit(ilimit),
            )
        };
        let (cv, cr) = mk(&a, &b);
        let (sv, sr) = mk(&b, &a);
        Self { cv, sv, cr, sr }
    }
}

fn same_keys(v: &OneRttKeys, r: &r::update::OneRttKeys, at: &str) {
    assert_eq!(v.send_key_phase(), r.send_key_phase(), "{at}");
    assert_eq!(v.send_generation(), r.send_generation(), "{at}");
    assert_eq!(v.recv_generation(), r.recv_generation(), "{at}");
    assert_eq!(v.has_previous_recv_key(), r.has_previous_recv_key(), "{at}");
    assert_eq!(
        v.authentication_failures(),
        r.authentication_failures(),
        "{at}"
    );
    assert_eq!(v.key_update_permitted(), r.key_update_permitted(), "{at}");
    let (a, b) = (v.audit(), r.audit());
    assert_eq!(
        (a.issued, a.sealed, a.abandoned, a.outstanding),
        (b.issued, b.sealed, b.abandoned, b.outstanding),
        "{at}"
    );
}

#[test]
fn one_rtt_key_updates_match_the_rust_state_machine() {
    let mut rng = Rng(0x5eed_0004);
    let mut tally = Tally::default();
    for case in 0..90 {
        let c = CIPHERS[case % 3];
        let mut p = Pair::new(c, &mut rng);
        let mut now = 0u64;
        // Packets sealed earlier, kept for reordering: (bytes, pn).
        let mut held: Vec<(Vec<u8>, u64)> = Vec::new();
        for step in 0..160 {
            let at = format!("case {case} step {step}");
            now += rng.below(8);
            match rng.below(10) {
                // An acknowledgement, sometimes of an old packet.
                0 => {
                    let pn = p.cv.audit().issued.saturating_sub(rng.below(4));
                    p.cv.on_packet_acked(pn);
                    p.cr.on_packet_acked(pn);
                }
                // A key update attempt by the client.
                1 => {
                    let a = p.cv.initiate_key_update();
                    tally.note("update", &a);
                    assert_eq!(a, p.cr.initiate_key_update(), "{at}");
                }
                // A held (reordered) packet arrives.
                2 if !held.is_empty() => {
                    let (bytes, pn) = held.swap_remove(rng.below(held.len() as u64) as usize);
                    deliver(&mut p, bytes, pn, now, &mut rng, &at, &mut tally);
                }
                3 => {
                    p.sv.discard_expired(now);
                    p.sr.discard_expired(now);
                }
                _ => {
                    // The client seals one packet; the server receives it
                    // now or later.
                    let pn = p.cv.audit().issued + rng.below(2);
                    let (tv, tr) = (p.cv.ticket(pn), p.cr.ticket(pn));
                    tally.note("ticket", &tv);
                    let (tv, tr) = match (tv, tr) {
                        (Ok(a), Ok(b)) => (a, b),
                        (Err(a), Err(b)) => {
                            assert_eq!(a, b, "{at}");
                            continue;
                        }
                        _ => panic!("{at}: one ticket failed"),
                    };
                    let phase = if rng.chance(5) {
                        !p.cv.send_key_phase()
                    } else {
                        p.cv.send_key_phase()
                    };
                    let payload = 20 + rng.below(30) as usize;
                    let mut bv = rng.bytes(9 + 2 + payload + 16);
                    header(
                        &mut bv,
                        0x40 | (u8::from(phase) << 2) | if rng.chance(3) { 0x80 } else { 0 },
                        9,
                        2,
                        pn,
                    );
                    let mut br = bv.clone();
                    let (a, b) = (
                        p.cv.seal(tv, &mut bv, 9, payload),
                        p.cr.seal(tr, &mut br, 9, payload),
                    );
                    assert_eq!(bv, br, "{at}");
                    tally.note("seal", &a.as_ref().map_err(|e| e.error));
                    match (a, b) {
                        (Ok(a), Ok(b)) => assert_eq!(a, b, "{at}"),
                        (Err(a), Err(b)) => {
                            assert_eq!(a.error, b.error, "{at}");
                            assert!(p.cv.abandon(a.ticket).is_ok());
                            assert!(p.cr.abandon(b.ticket).is_ok());
                            continue;
                        }
                        _ => panic!("{at}: one seal failed"),
                    }
                    if rng.chance(25) {
                        held.push((bv, pn));
                    } else {
                        deliver(&mut p, bv, pn, now, &mut rng, &at, &mut tally);
                    }
                }
            }
            same_keys(&p.cv, &p.cr, &at);
            same_keys(&p.sv, &p.sr, &at);
            // The server sometimes answers so that the client's updates
            // become permitted and peer-initiated updates are exercised.
            if rng.chance(20) {
                let ok = p.sv.initiate_key_update();
                assert_eq!(ok, p.sr.initiate_key_update(), "{at}");
                same_keys(&p.sv, &p.sr, &at);
            }
        }
    }
    tally.assert_reached(&[
        "update ok",
        "update KeyUpdateNotPermitted",
        "ticket ConfidentialityLimitReached",
        "seal ok",
        "seal KeyPhaseMismatch",
        "seal InvalidHeaderForm",
        "open ok",
        "open promoted",
        "open previous",
        "open AuthenticationFailed",
        "open KeyRetired",
        "open PacketNumberReconstruction",
        "open IntegrityLimitReached",
    ]);
}

/// The server opens `bytes`, sometimes tampered, on both implementations.
fn deliver(
    p: &mut Pair,
    mut bv: Vec<u8>,
    pn: u64,
    now: u64,
    rng: &mut Rng,
    at: &str,
    tally: &mut Tally,
) {
    if rng.chance(10) {
        let i = rng.below(bv.len() as u64) as usize;
        bv[i] ^= 1 << rng.below(8);
    }
    let decline = rng.chance(3);
    let mut br = bv.clone();
    let a = p.sv.open_packet(&mut bv, 9, now, |h| {
        (!decline).then(|| reconstruct(pn, h.truncated_pn, h.pn_len))
    });
    let b = p.sr.open_packet(&mut br, 9, now, |h| {
        (!decline).then(|| reconstruct(pn, h.truncated_pn, h.pn_len))
    });
    tally.note("open", &a);
    if let Ok(o) = &a {
        if o.promoted {
            *tally.0.entry("open promoted".into()).or_default() += 1;
        }
        if o.generation < p.sv.recv_generation() {
            *tally.0.entry("open previous".into()).or_default() += 1;
        }
    }
    match (a, b) {
        (Ok(a), Ok(b)) => assert_eq!(
            (
                a.packet_number,
                a.header_len,
                a.payload_len,
                a.first_byte,
                a.generation,
                a.promoted
            ),
            (
                b.packet_number,
                b.header_len,
                b.payload_len,
                b.first_byte,
                b.generation,
                b.promoted
            ),
            "{at}"
        ),
        (Err(a), Err(b)) => assert_eq!(a, b, "{at}"),
        (a, b) => panic!("{at}: {a:?} vs {b:?}"),
    }
    assert_eq!(bv, br, "{at}");
}

#[test]
fn retry_integrity_matches_the_rust_code() {
    let mut rng = Rng(0x5eed_0005);
    for case in 0..400 {
        let n = rng.below(24) as usize;
        let odcid = rng.bytes(n);
        let n = rng.below(60) as usize;
        let body = rng.bytes(n);
        let (a, b) = (
            retry_integrity_tag(&odcid, &body),
            r::retry::retry_integrity_tag(&odcid, &body),
        );
        assert_eq!(a, b, "case {case}");
        let mut packet = body.clone();
        if let Ok(tag) = a {
            packet.extend_from_slice(&tag);
        }
        if rng.chance(30) && !packet.is_empty() {
            let i = rng.below(packet.len() as u64) as usize;
            packet[i] ^= 1;
        }
        if rng.chance(10) {
            packet.truncate(rng.below(17) as usize);
        }
        assert_eq!(
            verify_retry_integrity(&odcid, &packet),
            r::retry::verify_retry_integrity(&odcid, &packet),
            "case {case}"
        );
    }
}
