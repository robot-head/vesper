//! Fuzz-style parser tests: truncations, byte flips, splices and random
//! bytes never panic, and valid encodings round-trip.

mod support;

use support::*;
use vesper_aegis::cca::{CcaToken, verify_cca_token};
use vesper_aegis::snp::{SnpReport, verify_snp_report};
use vesper_aegis::tdx::{TdxQuote, verify_tdx_quote};
use vesper_aegis::*;

fn mutate(rng: &mut SplitMix, seed: &[u8]) -> Vec<u8> {
    let mut v = seed.to_vec();
    match rng.below(6) {
        0 => v.truncate(rng.below(v.len() as u64 + 1) as usize),
        1 => {
            for _ in 0..=rng.below(8) {
                let i = rng.below(v.len() as u64) as usize;
                if let Some(b) = v.get_mut(i) {
                    *b ^= 1 << rng.below(8);
                }
            }
        }
        2 => {
            let i = rng.below(v.len() as u64) as usize;
            let extra: Vec<u8> = (0..rng.below(64)).map(|_| rng.next() as u8).collect();
            v.splice(i..i, extra);
        }
        3 => {
            // Overwrite a little-endian length-looking word with an extreme.
            let i = rng.below(v.len().saturating_sub(4) as u64) as usize;
            let w = [0u32, 1, 0xFFFF, 0xFFFF_FFFF, 0x7FFF_FFFF][rng.below(5) as usize];
            if i + 4 <= v.len() {
                v[i..i + 4].copy_from_slice(&w.to_le_bytes());
            }
        }
        4 => v = (0..rng.below(2048)).map(|_| rng.next() as u8).collect(),
        _ => {
            let i = rng.below(v.len() as u64) as usize;
            v.truncate(i);
            v.extend_from_slice(&seed[..i.min(seed.len())]);
        }
    }
    v
}

#[test]
fn tdx_parser_never_panics_and_round_trips() {
    let pck = chain(Curve::P256, Curve::P256, false);
    let ak = Signer::generate(Curve::P256);
    let q = tdx_quote_struct(&pck, &ak, &TdxParams::default());
    let seed = q.encode();
    assert_eq!(TdxQuote::parse(&seed).unwrap(), q);
    let rt = runtime();
    let mut rng = SplitMix(0xA1);
    let mut accepted = 0;
    for _ in 0..3000 {
        let m = mutate(&mut rng, &seed);
        if let Ok(p) = TdxQuote::parse(&m) {
            assert_eq!(p.encode(), m, "parse must be injective on accepted inputs");
        }
        if verify_tdx_quote(&m, &pck.root, NOW_NS, &rt).is_ok() {
            // Only the unauthenticated tail of the PEM chain (for example
            // the redundant root copy) may differ in an accepted quote.
            accepted += 1;
            let p = TdxQuote::parse(&m).unwrap();
            let mut tail_only = p.clone();
            tail_only.pck_chain_pem = q.pck_chain_pem.clone();
            assert_eq!(tail_only, q);
        }
    }
    assert!(accepted >= 1, "the unmutated-equivalent cases verify");
    for n in 0..seed.len().min(700) {
        assert!(TdxQuote::parse(&seed[..n]).is_err());
    }
}

#[test]
fn snp_parser_never_panics_and_round_trips() {
    let ark = chain(Curve::P384, Curve::P384, false);
    let r = snp_report_struct(&ark.leaf, &SnpParams::default());
    let seed = r.encode();
    assert_eq!(seed.len(), 1184);
    assert_eq!(SnpReport::parse(&seed).unwrap(), r);
    let rt = runtime();
    let mut rng = SplitMix(0xB2);
    for _ in 0..3000 {
        let m = mutate(&mut rng, &seed);
        if let Ok(p) = SnpReport::parse(&m) {
            assert_eq!(p.encode(), m);
        }
        if verify_snp_report(&m, &ark.der_chain(), &ark.root, NOW_NS, &rt).is_ok() {
            assert_eq!(m, seed);
        }
    }
    // High bytes of the signature components must be zero.
    let mut t = seed.clone();
    t[0x2A0 + 60] = 1;
    assert!(matches!(
        SnpReport::parse(&t),
        Err(ParseError::NonZeroReserved { .. })
    ));
}

#[test]
fn cca_parser_never_panics() {
    let iak = chain(Curve::P384, Curve::P256, false);
    let rak = Signer::generate(Curve::P384);
    let seed = cca_token(&iak.leaf, &rak, &CcaParams::default());
    CcaToken::parse(&seed).unwrap();
    let rt = runtime();
    let mut rng = SplitMix(0xC3);
    for _ in 0..3000 {
        let m = mutate(&mut rng, &seed);
        let _ = CcaToken::parse(&m);
        if verify_cca_token(&m, &iak.der_chain(), &iak.root, NOW_NS, &rt).is_ok() {
            assert_eq!(m, seed);
        }
    }
    // Deeply nested CBOR is rejected, not a stack overflow.
    let mut deep = vec![0xD9, 0x01, 0x8F]; // tag 399
    deep.extend(std::iter::repeat_n(0x81u8, 100_000)); // nested arrays
    deep.push(0x00);
    assert!(CcaToken::parse(&deep).is_err());
    // Over-budget token.
    assert!(matches!(
        CcaToken::parse(&vec![0u8; 70_000]),
        Err(ParseError::OverBudget { .. })
    ));
    // Duplicate claim keys are ambiguous.
    use vesper_aegis::cca::CcaRealmClaims;
    let dup = [0xA2, 0x0A, 0x01, 0x0A, 0x02];
    assert!(matches!(
        CcaRealmClaims::from_cbor(&dup),
        Err(ParseError::Cbor {
            reason: "duplicate claim key",
            ..
        })
    ));
}

#[test]
fn claims_wire_decoder_never_panics() {
    let mut rng = SplitMix(0xD4);
    for len in 0..300 {
        let v: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
        let r = AegisAttestationClaimsWire::decode(&v);
        if len != 128 {
            assert!(r.is_err());
        }
        if let Ok(w) = r {
            assert_eq!(&w.encode()[..], &v[..]);
        }
    }
    let mut ok = [0u8; 128];
    ok[0] = 2;
    assert!(AegisAttestationClaimsWire::decode(&ok).is_ok());
    ok[4] = 0x08; // unknown flag bit
    assert!(AegisAttestationClaimsWire::decode(&ok).is_err());
    ok[4] = 0;
    ok[0] = 4; // unknown architecture
    assert!(AegisAttestationClaimsWire::decode(&ok).is_err());
}
