//! The C build against the `blake3` crate (the vetted reference of the same
//! upstream version): every official test-vector length, keyed hashing,
//! key derivation, extended output, and incremental updates split at every
//! block and chunk boundary. Also the handle rules of the `.vsys` library:
//! a key that is not 32 bytes or a context that is not UTF-8 gives NULL.
use vesper_blake3_sys::*;

const LENS: [usize; 22] = [
    0, 1, 1023, 1024, 1025, 2048, 2049, 3072, 3073, 4096, 4097, 5120, 5121, 6144, 6145, 7168, 7169,
    8192, 8193, 16384, 31744, 102400,
];
const KEY: &[u8; 32] = b"whats the Elvish word for friend";
const CONTEXT: &str = "BLAKE3 2019-12-27 16:29:52 test vectors context";

fn input(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i % 251) as u8).collect()
}

/// Feeds `data` in `split`-byte pieces and reads `out_len` bytes.
fn c_hash(h: *mut std::ffi::c_void, data: &[u8], split: usize, out_len: usize) -> Vec<u8> {
    assert!(!h.is_null());
    let mut out = vec![0u8; out_len];
    unsafe {
        for piece in data.chunks(split.max(1)) {
            vesper_blake3_update(h, piece.as_ptr(), piece.len());
        }
        vesper_blake3_finalize(h, out.as_mut_ptr(), out.len());
        vesper_blake3_free(h);
    }
    out
}

fn crate_xof(mut h: blake3::Hasher, data: &[u8], out_len: usize) -> Vec<u8> {
    h.update(data);
    let mut out = vec![0u8; out_len];
    h.finalize_xof().fill(&mut out);
    out
}

#[test]
fn every_mode_matches_the_crate() {
    for n in LENS {
        let data = input(n);
        let plain = crate_xof(blake3::Hasher::new(), &data, 131);
        let keyed = crate_xof(blake3::Hasher::new_keyed(KEY), &data, 131);
        let derive = crate_xof(blake3::Hasher::new_derive_key(CONTEXT), &data, 131);
        assert_eq!(
            c_hash(unsafe { vesper_blake3_new() }, &data, n, 131),
            plain,
            "plain {n}"
        );
        let k = unsafe { vesper_blake3_new_keyed(KEY.as_ptr(), 32) };
        assert_eq!(c_hash(k, &data, n, 131), keyed, "keyed {n}");
        let d = unsafe { vesper_blake3_new_derive_key(CONTEXT.as_ptr(), CONTEXT.len()) };
        assert_eq!(c_hash(d, &data, n, 131), derive, "derive {n}");
        let mut one = vec![0u8; 32];
        unsafe { vesper_blake3_hash(data.as_ptr(), data.len(), one.as_mut_ptr(), 32) };
        assert_eq!(one, plain[..32], "one-shot {n}");
    }
}

#[test]
fn incremental_splits_match() {
    let data = input(9000);
    let want = crate_xof(blake3::Hasher::new(), &data, 64);
    for split in [1, 7, 63, 64, 65, 1000, 1023, 1024, 1025, 4096] {
        assert_eq!(
            c_hash(unsafe { vesper_blake3_new() }, &data, split, 64),
            want,
            "{split}"
        );
    }
}

#[test]
fn finalize_keeps_the_hasher_running() {
    let h = unsafe { vesper_blake3_new() };
    let mut a = [0u8; 32];
    let mut b = [0u8; 32];
    unsafe {
        vesper_blake3_update(h, b"ab".as_ptr(), 2);
        vesper_blake3_finalize(h, a.as_mut_ptr(), 32);
        vesper_blake3_update(h, b"c".as_ptr(), 1);
        vesper_blake3_finalize(h, b.as_mut_ptr(), 32);
        vesper_blake3_free(h);
    }
    assert_eq!(a, *blake3::hash(b"ab").as_bytes());
    assert_eq!(b, *blake3::hash(b"abc").as_bytes());
}

#[test]
fn bad_keys_and_contexts_give_null() {
    unsafe {
        assert!(vesper_blake3_new_keyed(KEY.as_ptr(), 31).is_null());
        assert!(vesper_blake3_new_keyed(KEY.as_ptr(), 33).is_null());
        for bad in [
            &[0xff][..],
            &[0xc0, 0x80],             // overlong
            &[0xe0, 0x80, 0x80],       // overlong
            &[0xed, 0xa0, 0x80],       // surrogate
            &[0xf4, 0x90, 0x80, 0x80], // above U+10FFFF
            &[0xe2, 0x82],             // truncated
            &[b'a', 0x80],
        ] {
            assert!(std::str::from_utf8(bad).is_err());
            assert!(
                vesper_blake3_new_derive_key(bad.as_ptr(), bad.len()).is_null(),
                "{bad:?}"
            );
        }
        for good in ["", "é", "€", "𐍈", "a\u{10ffff}"] {
            let h = vesper_blake3_new_derive_key(good.as_ptr(), good.len());
            let got = c_hash(h, b"x", 1, 32);
            let want = crate_xof(blake3::Hasher::new_derive_key(good), b"x", 32);
            assert_eq!(got, want, "{good:?}");
        }
    }
}

#[test]
fn vendored_c_is_the_crate_version() {
    let header = include_str!("../../../packages/vesper-std/hash/sys/c/blake3.h");
    assert!(
        header.contains("#define BLAKE3_VERSION_STRING \"1.8.7\""),
        "the vendored C and the pinned blake3 dev-dependency must be the same release"
    );
}
