//! `@vesper/crypto` over `vesper-crypto-sys` (Rust-exit SH6 lane 17, #1412):
//! the Vesper side of the one crypto shim.
//!
//! A driver package that depends on `packages/vesper-crypto` runs every
//! wrapper in the JIT, bound to the shim's symbols, on the published
//! vectors (the same ones crates/vesper-crypto-sys/tests/vectors.rs checks
//! from Rust), and folds each output into one word that must equal the same
//! fold of the expected bytes. It also checks the Vesper TLS 1.3
//! `HKDF-Expand-Label` on RFC 9001 §A.1, that failures raise the shim's
//! status and write nothing, that every handle is released, and that
//! `crypto.vsys` is the package's `// @export vsys` block.
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
use vesper::{ffi_lowering, packages, runtime::Value};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "vesper-crypto-shim-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(dir.join("src")).unwrap();
        Self(dir)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

const FNV0: u64 = 14695981039346656037;
const FNV_PRIME: u64 = 1099511628211;

fn fold(acc: u64, bytes: &[u8]) -> u64 {
    bytes
        .iter()
        .fold(acc, |h, b| (h ^ u64::from(*b)).wrapping_mul(FNV_PRIME))
}

fn hex(s: &str) -> Vec<u8> {
    let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn word(v: &Value) -> u64 {
    match v {
        Value::Int(n) => *n as u64,
        Value::UInt(n, _) => *n,
        other => panic!("not a word: {other:?}"),
    }
}

const DRIVER: &str = r#"use @vesper/crypto::{
    CryptoError,
    aead_free,
    aead_new,
    aead_open,
    aead_seal,
    aes_encrypt_block,
    aes_free,
    aes_new,
    chacha20_block,
    ct_eq,
    ed25519_public,
    ed25519_sign,
    ed25519_verify,
    hash,
    hash_into,
    hasher_clone,
    hasher_digest,
    hasher_free,
    hasher_new,
    hasher_update,
    hkdf_expand,
    hkdf_expand_label,
    hkdf_extract,
    hmac,
    mlkem768_decaps,
    mlkem768_encaps,
    mlkem768_isa_available,
    mlkem768_isa_detect,
    mlkem768_keygen,
    mlkem768_validate,
    random,
    tls13_label,
    wipe,
    x25519,
    x25519_base,
    zeros
};

fn nibble(c: u8) -> u8 {
    if c >= 97 {
        wrapping_u8(wrapping_sub(widen_u64(c), 87))
    } else {
        wrapping_u8(wrapping_sub(widen_u64(c), 48))
    }
}

// Bytes from lowercase hex digits.
fn hex(in s: Slice[u8]) -> ChunkSeq[u8] {
    let mut out: ChunkSeq[u8] = ChunkSeq::with_capacity[u8](s.len() / 2);
    let mut i: u64 = 0;
    loop {
        if wrapping_add(i, 1) >= wrapping_add(s.len(), 1) {
            break;
        }
        let hi = match s.try_get(i) {
            Option::Some(x) => nibble(x),
            Option::None => 0,
        };
        let lo = match s.try_get(wrapping_add(i, 1)) {
            Option::Some(x) => nibble(x),
            Option::None => 0,
        };
        out.push(wrapping_u8(wrapping_add(wrapping_mul(widen_u64(hi), 16), widen_u64(lo))));
        i = wrapping_add(i, 2);
    }
    out
}

fn fold(acc: u64, in s: Slice[u8]) -> u64 {
    let mut h = acc;
    let mut i: u64 = 0;
    loop {
        if i >= s.len() {
            break h;
        }
        let b = match s.try_get(i) {
            Option::Some(x) => x,
            Option::None => 0,
        };
        h = wrapping_mul(h ^ widen_u64(b), 1099511628211);
        i = wrapping_add(i, 1);
    }
}

fn fnv0() -> u64 {
    14695981039346656037
}

// The status a call raised, or 0.
fn status_of(e: CryptoError) -> u64 {
    widen_u64(e.status)
}

pub fn sha2() -> u64 eff [Ffi[Crypto]] {
    handle Fail[CryptoError] in {
        let a = hash(1, in "abc".as_bytes());
        let b = hash(2, in "abc".as_bytes());
        let c = hash(3, in "abc".as_bytes());
        let mut acc = fold(fnv0(), in a.as_slice());
        acc = fold(acc, in b.as_slice());
        fold(acc, in c.as_slice())
    } with {
        return v => v,
        raise(e, _) => status_of(e),
    }
}

// SHA-256 "abc" fed in two pieces, with a snapshot after the first.
pub fn running() -> u64 eff [Ffi[Crypto]] {
    let mut h = match hasher_new(1) {
        Option::Some(h) => h,
        Option::None => return 1,
    };
    hasher_update(inout h, in "a".as_bytes());
    let snap = match hasher_clone(in h) {
        Option::Some(s) => s,
        Option::None => {
            hasher_free(h);
            return 2;
        },
    };
    hasher_update(inout h, in "bc".as_bytes());
    let mut full = zeros(32);
    let mut part = zeros(32);
    let r = handle Fail[CryptoError] in {
        hasher_digest(in h, inout full.as_slice_mut());
        hasher_digest(in snap, inout part.as_slice_mut());
        0
    } with {
        return v => v,
        raise(e, _) => status_of(e),
    };
    hasher_free(snap);
    hasher_free(h);
    if r != 0 {
        return r;
    }
    fold(fold(fnv0(), in full.as_slice()), in part.as_slice())
}

pub fn mac_kdf() -> u64 eff [Ffi[Crypto]] {
    handle Fail[CryptoError] in {
        let mut key = zeros(20);
        key.fill(11);
        let mut tag = zeros(32);
        hmac(1, in key.as_slice(), in "Hi There".as_bytes(), inout tag.as_slice_mut());
        let mut ikm = zeros(22);
        ikm.fill(11);
        let salt = hex(in "000102030405060708090a0b0c".as_bytes());
        let info = hex(in "f0f1f2f3f4f5f6f7f8f9".as_bytes());
        let mut prk = zeros(32);
        hkdf_extract(1, in salt.as_slice(), in ikm.as_slice(), inout prk.as_slice_mut());
        let mut okm = zeros(42);
        hkdf_expand(1, in prk.as_slice(), in info.as_slice(), inout okm.as_slice_mut());
        let mut acc = fold(fnv0(), in tag.as_slice());
        acc = fold(acc, in prk.as_slice());
        fold(acc, in okm.as_slice())
    } with {
        return v => v,
        raise(e, _) => status_of(e),
    }
}

// RFC 9001 §A.1: the client Initial secret, key, IV and header key, through
// the Vesper HKDF-Expand-Label.
pub fn quic_initial() -> u64 eff [Ffi[Crypto]] {
    handle Fail[CryptoError] in {
        let salt = hex(in "38762cf7f55934b34d179ae6a4c80cadccbb7f0a".as_bytes());
        let dcid = hex(in "8394c8f03e515708".as_bytes());
        let mut initial = zeros(32);
        hkdf_extract(1, in salt.as_slice(), in dcid.as_slice(), inout initial.as_slice_mut());
        let mut client = zeros(32);
        hkdf_expand_label(1, in initial.as_slice(), in "client in".as_bytes(), in "".as_bytes(), inout client.as_slice_mut());
        let mut key = zeros(16);
        let mut iv = zeros(12);
        let mut hp = zeros(16);
        hkdf_expand_label(1, in client.as_slice(), in "quic key".as_bytes(), in "".as_bytes(), inout key.as_slice_mut());
        hkdf_expand_label(1, in client.as_slice(), in "quic iv".as_bytes(), in "".as_bytes(), inout iv.as_slice_mut());
        hkdf_expand_label(1, in client.as_slice(), in "quic hp".as_bytes(), in "".as_bytes(), inout hp.as_slice_mut());
        let mut acc = fold(fnv0(), in client.as_slice());
        acc = fold(acc, in key.as_slice());
        acc = fold(acc, in iv.as_slice());
        fold(acc, in hp.as_slice())
    } with {
        return v => v,
        raise(e, _) => status_of(e),
    }
}

// The label for "quic key", 16 bytes: 00 10 0e "tls13 quic key" 00.
pub fn label_bytes() -> u64 {
    handle Fail[CryptoError] in {
        let l = tls13_label(16, in "quic key".as_bytes(), in "".as_bytes());
        fold(fnv0(), in l.as_slice())
    } with {
        return v => v,
        raise(e, _) => status_of(e),
    }
}

// A 250-byte label does not fit the length byte: Length (1).
pub fn label_too_long() -> u64 {
    let long = zeros(250);
    handle Fail[CryptoError] in {
        let l = tls13_label(16, in long.as_slice(), in "".as_bytes());
        wrapping_add(100, l.len())
    } with {
        return v => v,
        raise(e, _) => status_of(e),
    }
}

// RFC 8439 §2.8.2 sealed in place, then opened back; a flipped AAD bit
// raises Auth (2) and leaves the buffer as it was.
pub fn chacha_poly() -> u64 eff [Ffi[Crypto]] {
    let key = hex(in "808182838485868788898a8b8c8d8e8f909192939495969798999a9b9c9d9e9f".as_bytes());
    let nonce = hex(in "070000004041424344454647".as_bytes());
    let a = match aead_new(3, in key.as_slice()) {
        Option::Some(a) => a,
        Option::None => return 1,
    };
    let pt = "Ladies and Gentlemen of the class of '99: If I could offer you only one tip for the future, sunscreen would be it.".as_bytes();
    let mut buf = hex(in "50515253c0c1c2c3c4c5c6c7".as_bytes());
    buf.extend(in pt);
    let mut i: u64 = 0;
    loop {
        if i >= 16 {
            break;
        }
        buf.push(0);
        i = wrapping_add(i, 1);
    }
    let r = handle Fail[CryptoError] in {
        aead_seal(in a, in nonce.as_slice(), inout buf.as_slice_mut(), 12, pt.len());
        let sealed = fold(fnv0(), in buf.as_slice());
        let mut bad = ChunkSeq::with_capacity[u8](buf.len());
        bad.extend(in buf.as_slice());
        let _f = bad.try_set(0, 81);
        let tampered = handle Fail[CryptoError] in {
            aead_open(in a, in nonce.as_slice(), inout bad.as_slice_mut(), 12);
            0
        } with {
            return v => v,
            raise(e, _) => status_of(e),
        };
        let unchanged = match bad.try_get(1) {
            Option::Some(x) => x == 81,
            Option::None => false,
        };
        aead_open(in a, in nonce.as_slice(), inout buf.as_slice_mut(), 12);
        let same = ct_eq(in buf[12..wrapping_add(12, pt.len())], in pt);
        if tampered == 2 && unchanged && same {
            sealed
        } else {
            3
        }
    } with {
        return v => v,
        raise(e, _) => status_of(e),
    };
    aead_free(a);
    r
}

// GCM spec case 2 (AES-128, zero key and nonce, 16 zero bytes), and an AES-256
// key of the wrong length is refused.
pub fn gcm() -> u64 eff [Ffi[Crypto]] {
    let key = zeros(16);
    let nonce = zeros(12);
    match aead_new(2, in key.as_slice()) {
        Option::Some(w) => {
            aead_free(w);
            return 4;
        },
        Option::None => {},
    }
    let a = match aead_new(1, in key.as_slice()) {
        Option::Some(a) => a,
        Option::None => return 1,
    };
    let mut buf = zeros(32);
    let r = handle Fail[CryptoError] in {
        aead_seal(in a, in nonce.as_slice(), inout buf.as_slice_mut(), 0, 16);
        fold(fnv0(), in buf.as_slice())
    } with {
        return v => v,
        raise(e, _) => status_of(e),
    };
    aead_free(a);
    r
}

pub fn blocks() -> u64 eff [Ffi[Crypto]] {
    let k128 = hex(in "000102030405060708090a0b0c0d0e0f".as_bytes());
    let k256 = hex(in "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f".as_bytes());
    let b1 = match aes_new(in k128.as_slice()) {
        Option::Some(b) => b,
        Option::None => return 1,
    };
    let b2 = match aes_new(in k256.as_slice()) {
        Option::Some(b) => b,
        Option::None => {
            aes_free(b1);
            return 2;
        },
    };
    let mut x = hex(in "00112233445566778899aabbccddeeff".as_bytes());
    let mut y = hex(in "00112233445566778899aabbccddeeff".as_bytes());
    let mut ks = zeros(64);
    let r = handle Fail[CryptoError] in {
        aes_encrypt_block(in b1, inout x.as_slice_mut());
        aes_encrypt_block(in b2, inout y.as_slice_mut());
        let nonce = hex(in "000000090000004a00000000".as_bytes());
        let mut key = zeros(32);
        let mut i: u64 = 0;
        loop {
            if i >= 32 {
                break;
            }
            let _s = key.try_set(i, wrapping_u8(i));
            i = wrapping_add(i, 1);
        }
        chacha20_block(in key.as_slice(), 1, in nonce.as_slice(), inout ks.as_slice_mut());
        let mut acc = fold(fnv0(), in x.as_slice());
        acc = fold(acc, in y.as_slice());
        fold(acc, in ks.as_slice())
    } with {
        return v => v,
        raise(e, _) => status_of(e),
    };
    aes_free(b2);
    aes_free(b1);
    r
}

pub fn curves() -> u64 eff [Ffi[Crypto]] {
    handle Fail[CryptoError] in {
        let a = hex(in "77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a".as_bytes());
        let b = hex(in "5dab087e624a8a4b79e17f8b83800ee66f3bb1292618b6fd1c2f8b27ff88e0eb".as_bytes());
        let mut pa = zeros(32);
        let mut pb = zeros(32);
        x25519_base(in a.as_slice(), inout pa.as_slice_mut());
        x25519_base(in b.as_slice(), inout pb.as_slice_mut());
        let mut s1 = zeros(32);
        let mut s2 = zeros(32);
        x25519(in a.as_slice(), in pb.as_slice(), inout s1.as_slice_mut());
        x25519(in b.as_slice(), in pa.as_slice(), inout s2.as_slice_mut());
        let zero = zeros(32);
        let mut s3 = zeros(32);
        let small = handle Fail[CryptoError] in {
            x25519(in a.as_slice(), in zero.as_slice(), inout s3.as_slice_mut());
            0
        } with {
            return v => v,
            raise(e, _) => status_of(e),
        };
        let seed = hex(in "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60".as_bytes());
        let mut pk = zeros(32);
        let mut sig = zeros(64);
        ed25519_public(in seed.as_slice(), inout pk.as_slice_mut());
        ed25519_sign(in seed.as_slice(), in "".as_bytes(), inout sig.as_slice_mut());
        let good = ed25519_verify(in pk.as_slice(), in "".as_bytes(), in sig.as_slice());
        let bad = ed25519_verify(in pk.as_slice(), in "x".as_bytes(), in sig.as_slice());
        if !ct_eq(in s1.as_slice(), in s2.as_slice()) || small != 2 || !good || bad {
            return 5;
        }
        let mut acc = fold(fnv0(), in pa.as_slice());
        acc = fold(acc, in pb.as_slice());
        acc = fold(acc, in s1.as_slice());
        acc = fold(acc, in pk.as_slice());
        fold(acc, in sig.as_slice())
    } with {
        return v => v,
        raise(e, _) => status_of(e),
    }
}

// ML-KEM-768 on the detected backend and the portable one: the same keys,
// ciphertext and secret, decapsulation agrees, implicit rejection differs,
// and a coefficient >= q fails validation. Returns the fold of ek || ct || ss.
pub fn mlkem() -> u64 eff [Ffi[Crypto]] {
    handle Fail[CryptoError] in {
        let mut seed = zeros(64);
        let mut i: u64 = 0;
        loop {
            if i >= 64 {
                break;
            }
            let _s = seed.try_set(i, wrapping_u8(i));
            i = wrapping_add(i, 1);
        }
        let mut m = zeros(32);
        m.fill(66);
        let mut out: u64 = 0;
        let mut first: u64 = 0;
        let best = mlkem768_isa_detect();
        let mut isas: ChunkSeq[u32] = ChunkSeq::with_capacity[u32](2);
        isas.push(0);
        isas.push(best);
        for isa in isas {
            if !mlkem768_isa_available(isa) {
                return 9;
            }
            let mut ek = zeros(1184);
            let mut dk = zeros(2400);
            mlkem768_keygen(isa, in seed.as_slice(), inout ek.as_slice_mut(), inout dk.as_slice_mut());
            if !mlkem768_validate(isa, in ek.as_slice()) {
                return 6;
            }
            let mut ct = zeros(1088);
            let mut ss = zeros(32);
            mlkem768_encaps(isa, in ek.as_slice(), in m.as_slice(), inout ct.as_slice_mut(), inout ss.as_slice_mut());
            let mut back = zeros(32);
            mlkem768_decaps(isa, in dk.as_slice(), in ct.as_slice(), inout back.as_slice_mut());
            if !ct_eq(in back.as_slice(), in ss.as_slice()) {
                return 7;
            }
            let _t = ct.try_set(0, 255);
            mlkem768_decaps(isa, in dk.as_slice(), in ct.as_slice(), inout back.as_slice_mut());
            if ct_eq(in back.as_slice(), in ss.as_slice()) {
                return 8;
            }
            let _u = ek.try_set(0, 255);
            let _v = ek.try_set(1, 255);
            if mlkem768_validate(isa, in ek.as_slice()) {
                return 10;
            }
            wipe(inout dk.as_slice_mut());
            out = fold(fnv0(), in ss.as_slice());
            if first == 0 {
                first = out;
            } else if first != out {
                return 11;
            }
        }
        out
    } with {
        return v => v,
        raise(e, _) => status_of(e),
    }
}

// Two draws differ; a wrong output length raises Length (1) and an unknown
// algorithm Unsupported (4), writing nothing.
pub fn misc() -> u64 eff [Ffi[Crypto]] {
    let mut a = zeros(32);
    let mut b = zeros(32);
    let drawn = handle Fail[CryptoError] in {
        random(inout a.as_slice_mut());
        random(inout b.as_slice_mut());
        0
    } with {
        return v => v,
        raise(e, _) => status_of(e),
    };
    let mut short = zeros(31);
    short.fill(7);
    let length = handle Fail[CryptoError] in {
        hash_into(1, in "abc".as_bytes(), inout short.as_slice_mut());
        0
    } with {
        return v => v,
        raise(e, _) => status_of(e),
    };
    let kept = match short.try_get(0) {
        Option::Some(x) => x == 7,
        Option::None => false,
    };
    let unknown = handle Fail[CryptoError] in {
        hash_into(9, in "abc".as_bytes(), inout a.as_slice_mut());
        0
    } with {
        return v => v,
        raise(e, _) => status_of(e),
    };
    wipe(inout b.as_slice_mut());
    let zero = zeros(32);
    let wiped = ct_eq(in b.as_slice(), in zero.as_slice());
    if drawn == 0 && !ct_eq(in a.as_slice(), in zero.as_slice()) && length == 1 && kept && unknown == 4 && wiped {
        0
    } else {
        wrapping_add(wrapping_add(wrapping_mul(drawn, 1000), wrapping_mul(length, 100)), unknown)
    }
}

fn main() -> u64 {
    0
}
"#;

fn driver() -> Temp {
    let temp = Temp::new();
    let crypto = fs::canonicalize("packages/vesper-crypto").unwrap();
    fs::write(
        temp.0.join("Loom.toml"),
        format!(
            "[package]\nname = \"@local/crypto-driver\"\nversion = \"0.1.0\"\nedition = \"2026\"\ncapability_ceiling = [\"Fail\", \"Alloc\", \"Ffi\"]\n\n[dependencies]\n\"@vesper/crypto\" = {{ version = \"^0.1\", path = \"{}\" }}\n",
            crypto.display().to_string().replace('\\', "/")
        ),
    )
    .unwrap();
    fs::write(temp.0.join("src/main.ves"), DRIVER).unwrap();
    temp
}

fn checked() -> (Temp, ffi_lowering::ForeignChecked) {
    let dir = driver();
    let assembled = packages::load(&dir.0).unwrap();
    assert!(
        assembled.ok(),
        "{:?} {:?}",
        assembled.diagnostics,
        assembled.manifest_errors
    );
    let imports = assembled.imports(None).unwrap();
    let checked =
        ffi_lowering::check_module(&assembled.module, &imports).unwrap_or_else(|e| panic!("{e}"));
    (dir, checked)
}

fn expected() -> Vec<(&'static str, u64)> {
    let sha = [
        hex("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"),
        hex(
            "cb00753f45a35e8bb5a03d699ac65007272c32ab0eded1631a8b605a43ff5bed\
             8086072ba1e7cc2358baeca134c825a7",
        ),
        hex(
            "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a\
             2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f",
        ),
    ]
    .concat();
    let abc = hex("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    let a = hex("ca978112ca1bbdcafac231b39a23dc4da786eff8147c4e72b9807785afee48bb");
    let mac_kdf = [
        hex("b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"),
        hex("077709362c2e32df0ddc3f0dc47bba6390b6c73bb50f9c3122ec844ad7c2b3e5"),
        hex(
            "3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf\
             34007208d5b887185865",
        ),
    ]
    .concat();
    // RFC 9001 §A.1.
    let quic = [
        hex("c00cf151ca5be075ed0ebfb5c80323c42d6b7db67881289af4008f1f6c357aea"),
        hex("1f369613dd76d5467730efcbe3b1a22d"),
        hex("fa044b2f42a3fd3b46fb255c"),
        hex("9f50449e04a0e810283a1e9933adedd2"),
    ]
    .concat();
    let label = [&[0x00, 0x10, 0x0e][..], b"tls13 quic key", &[0x00]].concat();
    // RFC 8439 §2.8.2 (AAD, ciphertext, tag), sealed by the shim from Rust;
    // crates/vesper-crypto-sys/tests/vectors.rs checks it against the RFC.
    let chacha = chacha_expected();
    let gcm = hex("0388dace60b6a392f328c2b971b2fe78ab6e47d42cec13bdf53a67b21257bddf");
    let blocks = [
        hex("69c4e0d86a7b0430d8cdb78070b4c55a"),
        hex("8ea2b7ca516745bfeafc49904b496089"),
        hex(
            "10f1e7e4d13b5915500fdd1fa32071c4c7d1f4c733c068030422aa9ac3d46c4e\
             d2826446079faa0914c2d705d98b02a2b5129cd1de164eb9cbd083e8a2503c4e",
        ),
    ]
    .concat();
    let curves = [
        hex("8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a"),
        hex("de9edb7d7b7dc1b4d35b61c2ece435373f8343c85b78674dadfc7e146f882b4f"),
        hex("4a5d9d5ba4ce2de1728e3bf480350f25e07e21c947d19e3376f09b3c1e161742"),
        hex("d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a"),
        hex(
            "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e06522490155\
             5fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b",
        ),
    ]
    .concat();
    vec![
        ("sha2", fold(FNV0, &sha)),
        ("running", fold(fold(FNV0, &abc), &a)),
        ("mac_kdf", fold(FNV0, &mac_kdf)),
        ("quic_initial", fold(FNV0, &quic)),
        ("label_bytes", fold(FNV0, &label)),
        ("label_too_long", 1),
        ("chacha_poly", fold(FNV0, &chacha)),
        ("gcm", fold(FNV0, &gcm)),
        ("blocks", fold(FNV0, &blocks)),
        ("curves", fold(FNV0, &curves)),
        ("mlkem", mlkem_expected()),
        ("misc", 0),
    ]
}

fn chacha_expected() -> Vec<u8> {
    use vesper_crypto_sys::*;
    let key: Vec<u8> = (0x80..=0x9f).collect();
    let nonce = hex("070000004041424344454647");
    let pt = b"Ladies and Gentlemen of the class of '99: If I could offer you only one tip for the future, sunscreen would be it.";
    let mut buf = [hex("50515253c0c1c2c3c4c5c6c7").as_slice(), pt, &[0; 16]].concat();
    unsafe {
        let a = vcr_aead_new(aead::CHACHA20_POLY1305, key.as_ptr(), key.len());
        let r = vcr_aead_seal(
            a,
            nonce.as_ptr(),
            12,
            buf.as_mut_ptr(),
            buf.len(),
            12,
            pt.len() as u64,
        );
        vcr_aead_free(a);
        assert_eq!(r, OK);
    }
    buf
}

/// The shared secret of the `mlkem` driver, computed by the shim from Rust.
fn mlkem_expected() -> u64 {
    use vesper_crypto_sys::*;
    let seed: Vec<u8> = (0..64).collect();
    let m = [66u8; 32];
    let mut ek = vec![0u8; MLKEM768_EK_LEN];
    let mut dk = vec![0u8; MLKEM768_DK_LEN];
    let mut ct = vec![0u8; MLKEM768_CT_LEN];
    let mut ss = [0u8; 32];
    unsafe {
        assert_eq!(
            vcr_mlkem768_keygen(
                isa::PORTABLE,
                seed.as_ptr(),
                64,
                ek.as_mut_ptr(),
                ek.len(),
                dk.as_mut_ptr(),
                dk.len()
            ),
            OK
        );
        assert_eq!(
            vcr_mlkem768_encaps(
                isa::PORTABLE,
                ek.as_ptr(),
                ek.len(),
                m.as_ptr(),
                32,
                ct.as_mut_ptr(),
                ct.len(),
                ss.as_mut_ptr(),
                32
            ),
            OK
        );
    }
    fold(FNV0, &ss)
}

/// Every wrapper in the JIT, bound to the shim, against the published
/// vectors; every handle is released.
#[test]
fn wrappers_match_the_vectors_in_the_jit() {
    if packages::vsys_target(None).is_err() {
        eprintln!("no .vsys target for this host; JIT evidence unavailable");
        return;
    }
    let (_dir, checked) = checked();
    let symbols = vesper_crypto_sys::symbols();
    let before = vesper_crypto_sys::live_handles();
    for (entry, want) in expected() {
        let got = checked
            .jit(entry, &symbols)
            .unwrap_or_else(|e| panic!("{entry}: {e}"));
        assert_eq!(word(&got), want, "{entry}");
    }
    assert_eq!(vesper_crypto_sys::live_handles(), before, "a handle leaked");
}

/// `crypto.vsys` (what dependent packages import) is the `// @export vsys`
/// block of src/lib.ves (what a library build of the package reads).
#[test]
fn vsys_file_is_the_export_block() {
    let lib = fs::read_to_string("packages/vesper-crypto/src/lib.ves").unwrap();
    let block: Vec<&str> = lib
        .lines()
        .take_while(|l| l.starts_with("// |") || l.starts_with("// @export"))
        .filter_map(|l| l.strip_prefix("// |"))
        .map(|l| l.strip_prefix(' ').unwrap_or(l))
        .collect();
    let file = fs::read_to_string("packages/vesper-crypto/crypto.vsys").unwrap();
    assert_eq!(block.join("\n"), file.replace("\r\n", "\n").trim_end());
}
