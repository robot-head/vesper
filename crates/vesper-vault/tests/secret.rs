//! Secret scalar semantics: masks, selects, comparisons and the tag check.

use vesper_vault::{SecMask64, SecU8, SecU32, SecU64, SecVec, verify_tag_16_ct};

#[test]
fn masks_are_all_zero_or_all_one() {
    for (a, b) in [
        (0u64, 0u64),
        (1, 0),
        (0, 1),
        (u64::MAX, u64::MAX),
        (7, 9),
        (1 << 63, 0),
    ] {
        let eq = SecU64::new(a).ct_eq(SecU64::new(b));
        assert_eq!(eq.declassify_bool(), a == b);
        let lt = SecU64::new(a).ct_lt(SecU64::new(b));
        assert_eq!(lt.declassify_bool(), a < b, "{a} < {b}");
        let z = SecU64::new(a).ct_is_zero();
        assert_eq!(z.declassify_bool(), a == 0);
        let sel = SecU64::ct_select(eq, SecU64::new(11), SecU64::new(22)).declassify();
        assert_eq!(sel, if a == b { 11 } else { 22 });
    }
    assert!(SecMask64::TRUE.declassify_bool());
    assert!(!SecMask64::FALSE.declassify_bool());
    assert!(!(!SecMask64::TRUE).declassify_bool());
    assert!((SecMask64::TRUE & SecMask64::from_public_bool(true)).declassify_bool());
    assert!((SecMask64::FALSE | SecMask64::from_public_bool(true)).declassify_bool());
}

#[test]
fn narrow_scalars_compare_and_select() {
    for a in 0..=255u8 {
        for b in [0u8, 1, 127, 128, 254, 255, a] {
            let (x, y) = (SecU8::new(a), SecU8::new(b));
            assert_eq!(x.ct_eq(y).declassify_bool(), a == b);
            assert_eq!(x.ct_lt(y).declassify_bool(), a < b);
            assert_eq!(SecU8::ct_select(x.ct_lt(y), x, y).declassify(), a.min(b));
        }
    }
    let a = SecU32::new(0xdead_beef);
    assert_eq!((a ^ SecU32::new(0xdead_beef)).declassify(), 0);
    assert_eq!(a.rotate_left(8).declassify(), 0xadbe_efde);
    assert_eq!(a.wrapping_add(SecU32::new(0x2152_4111)).declassify(), 0);
    assert_eq!(
        SecU32::new(3).wrapping_sub(SecU32::new(4)).declassify(),
        u32::MAX
    );
    assert_eq!(
        SecU32::new(3).shift_left(33).declassify(),
        6,
        "shift amount wraps mod width"
    );
}

#[test]
fn debug_never_prints_contents() {
    let s = format!(
        "{:?} {:?} {:?}",
        SecU64::new(0x1234_5678),
        SecU8::new(42),
        SecU64::new(1).ct_eq(SecU64::new(1))
    );
    assert!(
        !s.contains("1234") && !s.contains("42") && !s.contains("305419896"),
        "{s}"
    );
    assert!(s.contains("<secret>"));
    let v = SecVec::new([SecU8::new(0x41); 4]);
    assert!(!format!("{v:?}").contains("65"));
}

#[test]
fn secvec_visits_every_lane() {
    let a = SecVec::new([SecU8::new(1), SecU8::new(2), SecU8::new(3), SecU8::new(4)]);
    let mut lanes = [SecU8::new(1), SecU8::new(2), SecU8::new(3), SecU8::new(4)];
    assert!(a.ct_eq(SecVec::new(lanes)).declassify_bool());
    for i in 0..4 {
        lanes[i] = SecU8::new(99);
        assert!(!a.ct_eq(SecVec::new(lanes)).declassify_bool(), "lane {i}");
        lanes[i] = SecU8::new(i as u8 + 1);
    }
    let z = SecVec::<SecU8, 4>::zeroed();
    let picked = SecVec::ct_select(SecMask64::FALSE, a, z).declassify();
    assert!(picked.iter().all(|l| l.declassify() == 0));
    let x = a.bit_xor(a).bit_or(z).bit_and(a).declassify();
    assert!(x.iter().all(|l| l.declassify() == 0));
}

#[test]
fn load_le_is_bounds_checked() {
    let buf = [1u8, 0, 0, 0, 0, 0, 0, 0, 2];
    assert_eq!(SecU64::load_le(&buf, 0).unwrap().declassify(), 1);
    assert_eq!(SecU64::load_le(&buf, 1).unwrap().declassify(), 2 << 56);
    assert!(SecU64::load_le(&buf, 2).is_err());
    assert!(SecU64::load_le(&buf, usize::MAX).is_err());
}

/// Same computation as `VaultSpec.verifyTag16Ct`, transliterated, compared
/// with plain equality (the Lean theorem covers all inputs of the model).
#[test]
fn tag_compare_matches_equality() {
    let mut rng = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng
    };
    for _ in 0..2000 {
        let mut a = [0u8; 16];
        for c in a.chunks_mut(8) {
            c.copy_from_slice(&next().to_le_bytes());
        }
        assert!(verify_tag_16_ct(&a, &a));
        for byte in 0..16 {
            for bit in 0..8 {
                let mut b = a;
                b[byte] ^= 1 << bit;
                assert!(!verify_tag_16_ct(&a, &b));
            }
        }
    }
}
