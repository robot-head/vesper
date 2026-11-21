//! Known-answer tests for every `vcr_*` call, from the published vectors
//! (FIPS 180-4, FIPS 197, RFC 4231, RFC 5869, RFC 7748, RFC 8032, RFC 8439,
//! RFC 9001, the GCM specification), plus the status and no-write-on-error
//! rules of the ABI and the handle leak counts. The Vesper side of the same
//! calls is tested in tests/crypto_shim.rs at the repository root.

use std::sync::Mutex;
use vesper_crypto_sys::*;

/// Handle-making tests run one at a time, so the leak counts do not race.
static SERIAL: Mutex<()> = Mutex::new(());

fn hex(s: &str) -> Vec<u8> {
    let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn hash_of(alg: u32, data: &[u8]) -> Vec<u8> {
    let mut out = vec![0; vcr_hash_len(alg) as usize];
    let r = unsafe { vcr_hash(alg, data.as_ptr(), data.len(), out.as_mut_ptr(), out.len()) };
    assert_eq!(r, OK);
    out
}

#[test]
fn sha2_abc() {
    assert_eq!(
        hash_of(hash::SHA256, b"abc"),
        hex("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
    );
    assert_eq!(
        hash_of(hash::SHA384, b"abc"),
        hex(
            "cb00753f45a35e8bb5a03d699ac65007272c32ab0eded1631a8b605a43ff5bed\
             8086072ba1e7cc2358baeca134c825a7"
        )
    );
    assert_eq!(
        hash_of(hash::SHA512, b"abc"),
        hex(
            "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a\
             2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f"
        )
    );
    assert_eq!(vcr_hash_len(99), 0);
    let mut out = [0u8; 32];
    assert_eq!(
        unsafe { vcr_hash(99, [].as_ptr(), 0, out.as_mut_ptr(), 32) },
        UNSUPPORTED
    );
    assert_eq!(
        unsafe { vcr_hash(hash::SHA256, [].as_ptr(), 0, out.as_mut_ptr(), 31) },
        LENGTH
    );
}

#[test]
fn running_hash_snapshots_and_counts() {
    let _g = SERIAL.lock().unwrap();
    let before = live_handles();
    let h = vcr_hash_new(hash::SHA256);
    assert!(!h.is_null());
    unsafe { vcr_hash_update(h, b"a".as_ptr(), 1) };
    let snap = unsafe { vcr_hash_clone(h) };
    unsafe { vcr_hash_update(h, b"bc".as_ptr(), 2) };
    let mut out = [0u8; 32];
    assert_eq!(unsafe { vcr_hash_digest(h, out.as_mut_ptr(), 32) }, OK);
    assert_eq!(out.to_vec(), hash_of(hash::SHA256, b"abc"));
    // The state keeps running after a digest.
    assert_eq!(unsafe { vcr_hash_digest(h, out.as_mut_ptr(), 32) }, OK);
    assert_eq!(out.to_vec(), hash_of(hash::SHA256, b"abc"));
    assert_eq!(unsafe { vcr_hash_digest(snap, out.as_mut_ptr(), 32) }, OK);
    assert_eq!(out.to_vec(), hash_of(hash::SHA256, b"a"));
    assert_eq!(
        unsafe { vcr_hash_digest(snap, out.as_mut_ptr(), 31) },
        LENGTH
    );
    assert_eq!(live_handles()[2], before[2] + 2);
    unsafe {
        vcr_hash_free(h);
        vcr_hash_free(snap);
        vcr_hash_free(std::ptr::null_mut());
    }
    assert!(vcr_hash_new(0).is_null());
    assert_eq!(live_handles(), before);
}

#[test]
fn hmac_rfc4231_case_1() {
    let key = [0x0bu8; 20];
    let mut out = [0u8; 32];
    let r = unsafe {
        vcr_hmac(
            hash::SHA256,
            key.as_ptr(),
            20,
            b"Hi There".as_ptr(),
            8,
            out.as_mut_ptr(),
            32,
        )
    };
    assert_eq!(r, OK);
    assert_eq!(
        out.to_vec(),
        hex("b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7")
    );
}

#[test]
fn hkdf_rfc5869_case_1_and_quic_initial() {
    let ikm = [0x0bu8; 22];
    let salt = hex("000102030405060708090a0b0c");
    let info = hex("f0f1f2f3f4f5f6f7f8f9");
    let mut prk = [0u8; 32];
    let r = unsafe {
        vcr_hkdf_extract(
            hash::SHA256,
            salt.as_ptr(),
            salt.len(),
            ikm.as_ptr(),
            ikm.len(),
            prk.as_mut_ptr(),
            32,
        )
    };
    assert_eq!(r, OK);
    assert_eq!(
        prk.to_vec(),
        hex("077709362c2e32df0ddc3f0dc47bba6390b6c73bb50f9c3122ec844ad7c2b3e5")
    );
    let mut okm = [0u8; 42];
    let r = unsafe {
        vcr_hkdf_expand(
            hash::SHA256,
            prk.as_ptr(),
            32,
            info.as_ptr(),
            info.len(),
            okm.as_mut_ptr(),
            42,
        )
    };
    assert_eq!(r, OK);
    assert_eq!(
        okm.to_vec(),
        hex(
            "3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf\
             34007208d5b887185865"
        )
    );
    // Too long an output (more than 255 digests) or too short a PRK.
    let mut big = vec![0u8; 255 * 32 + 1];
    let r = unsafe {
        vcr_hkdf_expand(
            hash::SHA256,
            prk.as_ptr(),
            32,
            [].as_ptr(),
            0,
            big.as_mut_ptr(),
            big.len(),
        )
    };
    assert_eq!(r, LENGTH);
    assert!(big.iter().all(|b| *b == 0));
    let r = unsafe {
        vcr_hkdf_expand(
            hash::SHA256,
            prk.as_ptr(),
            31,
            [].as_ptr(),
            0,
            okm.as_mut_ptr(),
            42,
        )
    };
    assert_eq!(r, LENGTH);
    // RFC 9001 §A.1: the QUIC v1 initial secret.
    let salt = hex("38762cf7f55934b34d179ae6a4c80cadccbb7f0a");
    let dcid = hex("8394c8f03e515708");
    let r = unsafe {
        vcr_hkdf_extract(
            hash::SHA256,
            salt.as_ptr(),
            salt.len(),
            dcid.as_ptr(),
            dcid.len(),
            prk.as_mut_ptr(),
            32,
        )
    };
    assert_eq!(r, OK);
    assert_eq!(
        prk.to_vec(),
        hex("7db5df06e7a69e432496adedb00851923595221596ae2ae9fb8115c1e9ed0a44")
    );
}

fn aead(alg: u32, key: &[u8]) -> *mut Aead {
    unsafe { vcr_aead_new(alg, key.as_ptr(), key.len()) }
}

#[test]
fn aes_gcm_spec_cases_1_and_2() {
    let _g = SERIAL.lock().unwrap();
    let before = live_handles();
    let a = aead(aead::AES_128_GCM, &[0; 16]);
    assert!(!a.is_null());
    let nonce = [0u8; 12];
    // Case 1: empty plaintext and AAD.
    let mut buf = [0u8; 16];
    assert_eq!(
        unsafe { vcr_aead_seal(a, nonce.as_ptr(), 12, buf.as_mut_ptr(), 16, 0, 0) },
        OK
    );
    assert_eq!(buf.to_vec(), hex("58e2fccefa7e3061367f1d57a4e7455a"));
    // Case 2: 16 zero bytes.
    let mut buf = [0u8; 32];
    assert_eq!(
        unsafe { vcr_aead_seal(a, nonce.as_ptr(), 12, buf.as_mut_ptr(), 32, 0, 16) },
        OK
    );
    assert_eq!(
        buf.to_vec(),
        hex("0388dace60b6a392f328c2b971b2fe78ab6e47d42cec13bdf53a67b21257bddf")
    );
    assert_eq!(
        unsafe { vcr_aead_open(a, nonce.as_ptr(), 12, buf.as_mut_ptr(), 32, 0) },
        OK
    );
    assert_eq!(&buf[..16], &[0; 16]);
    unsafe { vcr_aead_free(a) };
    assert_eq!(live_handles(), before);
}

#[test]
fn chacha20_poly1305_rfc8439_2_8_2() {
    let _g = SERIAL.lock().unwrap();
    let key: Vec<u8> = (0x80..=0x9f).collect();
    let nonce = hex("070000004041424344454647");
    let aad = hex("50515253c0c1c2c3c4c5c6c7");
    let pt = b"Ladies and Gentlemen of the class of '99: If I could offer you only one tip for the future, sunscreen would be it.";
    let a = aead(aead::CHACHA20_POLY1305, &key);
    let mut buf = [aad.as_slice(), pt, &[0; 16]].concat();
    let r = unsafe {
        vcr_aead_seal(
            a,
            nonce.as_ptr(),
            12,
            buf.as_mut_ptr(),
            buf.len(),
            12,
            pt.len() as u64,
        )
    };
    assert_eq!(r, OK);
    assert_eq!(&buf[..12], aad.as_slice());
    assert_eq!(
        buf[12..28].to_vec(),
        hex("d31a8d34648e60db7b86afbc53ef7ec2")
    );
    assert_eq!(
        buf[buf.len() - 16..].to_vec(),
        hex("1ae10b594f09e26a7e902ecbd0600691")
    );
    // A flipped AAD bit fails, and the buffer stays as it was.
    let mut bad = buf.clone();
    bad[0] ^= 1;
    let copy = bad.clone();
    let r = unsafe { vcr_aead_open(a, nonce.as_ptr(), 12, bad.as_mut_ptr(), bad.len(), 12) };
    assert_eq!(r, AUTH);
    assert_eq!(bad, copy);
    let r = unsafe { vcr_aead_open(a, nonce.as_ptr(), 12, buf.as_mut_ptr(), buf.len(), 12) };
    assert_eq!(r, OK);
    assert_eq!(&buf[12..12 + pt.len()], pt.as_slice());
    unsafe { vcr_aead_free(a) };
}

#[test]
fn aead_lengths_and_codes() {
    let _g = SERIAL.lock().unwrap();
    let before = live_handles();
    assert!(aead(aead::AES_128_GCM, &[0; 32]).is_null());
    assert!(aead(aead::AES_256_GCM, &[0; 16]).is_null());
    assert!(aead(9, &[0; 32]).is_null());
    let a = aead(aead::AES_256_GCM, &[7; 32]);
    let mut buf = [5u8; 40];
    let n = [0u8; 12];
    // Nonce of the wrong length, buffer too short for aad + msg + tag, a
    // buffer too short to open: LENGTH, and nothing is written.
    assert_eq!(
        unsafe { vcr_aead_seal(a, n.as_ptr(), 11, buf.as_mut_ptr(), 40, 4, 4) },
        LENGTH
    );
    assert_eq!(
        unsafe { vcr_aead_seal(a, n.as_ptr(), 12, buf.as_mut_ptr(), 40, 4, 21) },
        LENGTH
    );
    assert_eq!(
        unsafe { vcr_aead_seal(a, n.as_ptr(), 12, buf.as_mut_ptr(), 40, u64::MAX, 1) },
        LENGTH
    );
    assert_eq!(
        unsafe { vcr_aead_open(a, n.as_ptr(), 12, buf.as_mut_ptr(), 40, 25) },
        LENGTH
    );
    assert!(buf.iter().all(|b| *b == 5));
    // Bytes past aad + msg + tag are left alone.
    assert_eq!(
        unsafe { vcr_aead_seal(a, n.as_ptr(), 12, buf.as_mut_ptr(), 40, 4, 4) },
        OK
    );
    assert!(buf[24..].iter().all(|b| *b == 5));
    assert_eq!(
        unsafe { vcr_aead_seal(std::ptr::null(), n.as_ptr(), 12, buf.as_mut_ptr(), 40, 4, 4) },
        FAILED
    );
    unsafe { vcr_aead_free(a) };
    assert_eq!(live_handles(), before);
}

#[test]
fn aes_block_fips197() {
    let _g = SERIAL.lock().unwrap();
    let before = live_handles();
    for (key, want) in [
        (
            hex("000102030405060708090a0b0c0d0e0f"),
            "69c4e0d86a7b0430d8cdb78070b4c55a",
        ),
        (
            hex("000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f"),
            "8ea2b7ca516745bfeafc49904b496089",
        ),
    ] {
        let b = unsafe { vcr_aes_new(key.as_ptr(), key.len()) };
        assert!(!b.is_null());
        let mut block = hex("00112233445566778899aabbccddeeff");
        assert_eq!(
            unsafe { vcr_aes_encrypt_block(b, block.as_mut_ptr(), 16) },
            OK
        );
        assert_eq!(block, hex(want));
        assert_eq!(
            unsafe { vcr_aes_encrypt_block(b, block.as_mut_ptr(), 15) },
            LENGTH
        );
        unsafe { vcr_aes_free(b) };
    }
    assert!(unsafe { vcr_aes_new([0u8; 24].as_ptr(), 24) }.is_null());
    assert_eq!(live_handles(), before);
}

#[test]
fn chacha20_block_rfc8439_2_3_2_and_max_counter() {
    let key: Vec<u8> = (0..32).collect();
    let nonce = hex("000000090000004a00000000");
    let mut out = [0u8; 64];
    let r = unsafe {
        vcr_chacha20_block(
            key.as_ptr(),
            32,
            1,
            nonce.as_ptr(),
            12,
            out.as_mut_ptr(),
            64,
        )
    };
    assert_eq!(r, OK);
    assert_eq!(
        out.to_vec(),
        hex(
            "10f1e7e4d13b5915500fdd1fa32071c4c7d1f4c733c068030422aa9ac3d46c4e\
             d2826446079faa0914c2d705d98b02a2b5129cd1de164eb9cbd083e8a2503c4e"
        )
    );
    // The largest counter is defined (the QUIC sample chooses it).
    let r = unsafe {
        vcr_chacha20_block(
            key.as_ptr(),
            32,
            u32::MAX,
            nonce.as_ptr(),
            12,
            out.as_mut_ptr(),
            64,
        )
    };
    assert_eq!(r, OK);
    let r = unsafe {
        vcr_chacha20_block(
            key.as_ptr(),
            31,
            1,
            nonce.as_ptr(),
            12,
            out.as_mut_ptr(),
            64,
        )
    };
    assert_eq!(r, LENGTH);
}

#[test]
fn x25519_rfc7748_6_1() {
    let a = hex("77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a");
    let b = hex("5dab087e624a8a4b79e17f8b83800ee66f3bb1292618b6fd1c2f8b27ff88e0eb");
    let mut pa = [0u8; 32];
    let mut pb = [0u8; 32];
    unsafe {
        assert_eq!(vcr_x25519_base(a.as_ptr(), 32, pa.as_mut_ptr(), 32), OK);
        assert_eq!(vcr_x25519_base(b.as_ptr(), 32, pb.as_mut_ptr(), 32), OK);
    }
    assert_eq!(
        pa.to_vec(),
        hex("8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a")
    );
    assert_eq!(
        pb.to_vec(),
        hex("de9edb7d7b7dc1b4d35b61c2ece435373f8343c85b78674dadfc7e146f882b4f")
    );
    let mut s1 = [0u8; 32];
    let mut s2 = [0u8; 32];
    unsafe {
        assert_eq!(
            vcr_x25519(a.as_ptr(), 32, pb.as_ptr(), 32, s1.as_mut_ptr(), 32),
            OK
        );
        assert_eq!(
            vcr_x25519(b.as_ptr(), 32, pa.as_ptr(), 32, s2.as_mut_ptr(), 32),
            OK
        );
    }
    let want = hex("4a5d9d5ba4ce2de1728e3bf480350f25e07e21c947d19e3376f09b3c1e161742");
    assert_eq!(s1.to_vec(), want);
    assert_eq!(s2.to_vec(), want);
    // A small-order point gives an all-zero secret: AUTH, nothing written.
    let mut s3 = [9u8; 32];
    assert_eq!(
        unsafe { vcr_x25519(a.as_ptr(), 32, [0u8; 32].as_ptr(), 32, s3.as_mut_ptr(), 32) },
        AUTH
    );
    assert_eq!(s3, [9u8; 32]);
}

#[test]
fn ed25519_rfc8032_test_1() {
    let seed = hex("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60");
    let mut pk = [0u8; 32];
    let mut sig = [0u8; 64];
    unsafe {
        assert_eq!(
            vcr_ed25519_public(seed.as_ptr(), 32, pk.as_mut_ptr(), 32),
            OK
        );
        assert_eq!(
            vcr_ed25519_sign(seed.as_ptr(), 32, [].as_ptr(), 0, sig.as_mut_ptr(), 64),
            OK
        );
    }
    assert_eq!(
        pk.to_vec(),
        hex("d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a")
    );
    assert_eq!(
        sig.to_vec(),
        hex(
            "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e06522490155\
             5fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b"
        )
    );
    unsafe {
        assert_eq!(
            vcr_ed25519_verify(pk.as_ptr(), 32, [].as_ptr(), 0, sig.as_ptr(), 64),
            OK
        );
        assert_eq!(
            vcr_ed25519_verify(pk.as_ptr(), 32, b"x".as_ptr(), 1, sig.as_ptr(), 64),
            AUTH
        );
        assert_eq!(
            vcr_ed25519_verify(pk.as_ptr(), 32, [].as_ptr(), 0, sig.as_ptr(), 63),
            LENGTH
        );
        // The identity is a small-order key: refused by strict verification.
        let mut id = [0u8; 32];
        id[0] = 1;
        assert_eq!(
            vcr_ed25519_verify(id.as_ptr(), 32, [].as_ptr(), 0, sig.as_ptr(), 64),
            AUTH
        );
    }
}

#[test]
fn mlkem768_round_trips_on_every_backend() {
    let seed: Vec<u8> = (0..64).collect();
    let m = [0x42u8; 32];
    let mut first: Option<(Vec<u8>, Vec<u8>, Vec<u8>)> = None;
    for isa in [isa::PORTABLE, isa::AVX2, isa::NEON] {
        let mut ek = vec![0u8; MLKEM768_EK_LEN];
        let mut dk = vec![0u8; MLKEM768_DK_LEN];
        let r = unsafe {
            vcr_mlkem768_keygen(
                isa,
                seed.as_ptr(),
                64,
                ek.as_mut_ptr(),
                ek.len(),
                dk.as_mut_ptr(),
                dk.len(),
            )
        };
        if vcr_mlkem768_isa_available(isa) == 0 {
            assert_eq!(r, UNSUPPORTED);
            continue;
        }
        assert_eq!(r, OK);
        assert_eq!(
            unsafe { vcr_mlkem768_validate(isa, ek.as_ptr(), ek.len()) },
            OK
        );
        let mut ct = vec![0u8; MLKEM768_CT_LEN];
        let mut ss = [0u8; 32];
        let r = unsafe {
            vcr_mlkem768_encaps(
                isa,
                ek.as_ptr(),
                ek.len(),
                m.as_ptr(),
                32,
                ct.as_mut_ptr(),
                ct.len(),
                ss.as_mut_ptr(),
                32,
            )
        };
        assert_eq!(r, OK);
        let mut back = [0u8; 32];
        let r = unsafe {
            vcr_mlkem768_decaps(
                isa,
                dk.as_ptr(),
                dk.len(),
                ct.as_ptr(),
                ct.len(),
                back.as_mut_ptr(),
                32,
            )
        };
        assert_eq!(r, OK);
        assert_eq!(back, ss);
        // Implicit rejection: a tampered ciphertext gives another secret.
        ct[0] ^= 1;
        let r = unsafe {
            vcr_mlkem768_decaps(
                isa,
                dk.as_ptr(),
                dk.len(),
                ct.as_ptr(),
                ct.len(),
                back.as_mut_ptr(),
                32,
            )
        };
        assert_eq!(r, OK);
        assert_ne!(back, ss);
        ct[0] ^= 1;
        // A coefficient >= q fails the modulus check.
        let mut bad = ek.clone();
        bad[0] = 0xff;
        bad[1] |= 0x0f;
        assert_eq!(
            unsafe { vcr_mlkem768_validate(isa, bad.as_ptr(), bad.len()) },
            AUTH
        );
        // Every backend computes the same bytes.
        let got = (ek, ct, ss.to_vec());
        match &first {
            None => first = Some(got),
            Some(f) => assert_eq!(f, &got, "backend {isa}"),
        }
    }
    assert!(first.is_some());
    assert_eq!(vcr_mlkem768_isa_available(7), 0);
}

#[test]
fn ecdsa_verifies_ring_signatures() {
    use ring::rand::SystemRandom;
    use ring::signature::{self, EcdsaKeyPair, KeyPair};
    let rng = SystemRandom::new();
    for (sign, alg) in [
        (
            &signature::ECDSA_P256_SHA256_FIXED_SIGNING,
            ecdsa::P256_SHA256_FIXED,
        ),
        (
            &signature::ECDSA_P384_SHA384_FIXED_SIGNING,
            ecdsa::P384_SHA384_FIXED,
        ),
        (
            &signature::ECDSA_P256_SHA256_ASN1_SIGNING,
            ecdsa::P256_SHA256_ASN1,
        ),
        (
            &signature::ECDSA_P384_SHA384_ASN1_SIGNING,
            ecdsa::P384_SHA384_ASN1,
        ),
    ] {
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(sign, &rng).unwrap();
        let kp = EcdsaKeyPair::from_pkcs8(sign, pkcs8.as_ref(), &rng).unwrap();
        let msg = b"attestation report";
        let sig = kp.sign(&rng, msg).unwrap();
        let pk = kp.public_key().as_ref();
        let v = |m: &[u8]| unsafe {
            vcr_ecdsa_verify(
                alg,
                pk.as_ptr(),
                pk.len(),
                m.as_ptr(),
                m.len(),
                sig.as_ref().as_ptr(),
                sig.as_ref().len(),
            )
        };
        assert_eq!(v(msg), OK, "alg {alg}");
        assert_eq!(v(b"attestation repors"), AUTH, "alg {alg}");
    }
    assert_eq!(
        unsafe { vcr_ecdsa_verify(9, [].as_ptr(), 0, [].as_ptr(), 0, [].as_ptr(), 0) },
        UNSUPPORTED
    );
}

#[test]
fn random_ct_eq_and_zeroize() {
    let mut a = [0u8; 64];
    let mut b = [0u8; 64];
    unsafe {
        assert_eq!(vcr_random(a.as_mut_ptr(), 64), OK);
        assert_eq!(vcr_random(b.as_mut_ptr(), 64), OK);
    }
    assert_ne!(a, b);
    unsafe {
        assert_eq!(vcr_ct_eq(a.as_ptr(), 64, a.as_ptr(), 64), 1);
        assert_eq!(vcr_ct_eq(a.as_ptr(), 64, b.as_ptr(), 64), 0);
        assert_eq!(vcr_ct_eq(a.as_ptr(), 64, a.as_ptr(), 63), 0);
        vcr_zeroize(a.as_mut_ptr(), 64);
    }
    assert_eq!(a, [0u8; 64]);
}

#[test]
fn symbols_cover_every_call() {
    let names: Vec<&str> = symbols().iter().map(|(n, _)| *n).collect();
    let vsys = include_str!("../../../packages/vesper-crypto/crypto.vsys");
    let declared: Vec<&str> = vsys
        .lines()
        .filter_map(|l| l.trim().strip_prefix("fn "))
        .map(|l| &l[..l.find('(').unwrap()])
        .collect();
    assert_eq!(
        names, declared,
        "symbols() and crypto.vsys list the same calls in order"
    );
}

/// RSA PSS and PKCS #1 v1.5 against ring signatures made with a throwaway
/// 2048-bit test key (tests/fixtures/rsa2048.pk8, PKCS #8 DER, made with
/// `openssl genrsa -traditional 2048 | openssl pkcs8 -topk8 -nocrypt -outform der`).
#[test]
fn rsa_verifies_ring_signatures() {
    use ring::rand::SystemRandom;
    use ring::signature::{self, KeyPair, RsaKeyPair};
    let kp = RsaKeyPair::from_pkcs8(include_bytes!("fixtures/rsa2048.pk8")).unwrap();
    let rng = SystemRandom::new();
    let pk = kp.public_key().as_ref();
    let msg = b"tuf root metadata";
    let pads: [(&'static dyn signature::RsaEncoding, u32); 6] = [
        (&signature::RSA_PSS_SHA256, rsa::PSS_SHA256),
        (&signature::RSA_PSS_SHA384, rsa::PSS_SHA384),
        (&signature::RSA_PSS_SHA512, rsa::PSS_SHA512),
        (&signature::RSA_PKCS1_SHA256, rsa::PKCS1_SHA256),
        (&signature::RSA_PKCS1_SHA384, rsa::PKCS1_SHA384),
        (&signature::RSA_PKCS1_SHA512, rsa::PKCS1_SHA512),
    ];
    for (pad, alg) in pads {
        let mut sig = vec![0u8; kp.public().modulus_len()];
        kp.sign(pad, &rng, msg, &mut sig).unwrap();
        let v = |a: u32, m: &[u8]| unsafe {
            vcr_rsa_verify(
                a,
                pk.as_ptr(),
                pk.len(),
                m.as_ptr(),
                m.len(),
                sig.as_ptr(),
                sig.len(),
            )
        };
        assert_eq!(v(alg, msg), OK, "alg {alg}");
        assert_eq!(v(alg, b"tuf root metadatb"), AUTH, "alg {alg}");
        // A PSS signature is not a PKCS #1 one, and back.
        let other = if alg <= rsa::PSS_SHA512 {
            alg + 3
        } else {
            alg - 3
        };
        assert_eq!(v(other, msg), AUTH, "alg {alg} as {other}");
    }
    assert_eq!(
        unsafe { vcr_rsa_verify(9, [].as_ptr(), 0, [].as_ptr(), 0, [].as_ptr(), 0) },
        UNSUPPORTED
    );
}
