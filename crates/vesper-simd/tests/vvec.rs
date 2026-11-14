//! `VVec` strip-mining: every profile available here must agree bit for bit
//! with a scalar reference at its own vector length.
//!
//! Scalar profiles at many lane counts always run. SVE and RVV run when the
//! CPU has them (under QEMU: see .github/workflows/scalable-vectors.yml);
//! `VESPER_REQUIRE_SVE=1` / `VESPER_REQUIRE_RVV=1` turn "unavailable" into a
//! failure so a QEMU job cannot silently skip them.
use vesper_simd::ScanResult;
use vesper_simd::vvec::{
    IsaProfile, MAX_LANES, Rvv, Scalar, Sve, VMask, VVec, VectorIsa, VvecError, kernels,
    scan_record_delimiters,
};

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.next() as u8).collect()
    }
}

fn reference(data: &[u8], delim: u8) -> ScanResult {
    ScanResult {
        matches: data.iter().filter(|b| **b == delim).count() as u64,
        xor: data.iter().fold(0, |a, b| a ^ b),
    }
}

fn required(var: &str) -> bool {
    std::env::var(var).is_ok_and(|v| v == "1")
}

/// A mask with lanes chosen from `rng`, within `p`'s lanes.
fn random_mask<P: IsaProfile>(p: P, rng: &mut Rng) -> VMask<P> {
    let mut m = VMask::none(p);
    for i in 0..p.lanes() {
        if rng.next() & 3 != 0 {
            m = m.or(p.active_mask(i, i + 1).and_not(p.active_mask(0, i)));
        }
    }
    m
}

fn vec_of<P: IsaProfile>(p: P, bytes: &[u8]) -> VVec<P> {
    let mask = p.active_mask(0, bytes.len().min(p.lanes()));
    VVec::load_masked(bytes, 0, &mask, 0).unwrap()
}

/// Everything a profile must do, checked against plain Rust at the same
/// lane count.
fn check_profile<P: IsaProfile>(p: P, seed: u64) {
    let n = p.lanes();
    assert!((1..=MAX_LANES).contains(&n));
    let mut rng = Rng(seed);

    // Governing predicates.
    for total in [0, 1, n - 1, n, n + 1, 3 * n + 5] {
        for offset in [0, 1, n / 2, n, total, total + 1] {
            let m = p.active_mask(offset, total);
            let want = n.min(total.saturating_sub(offset));
            assert_eq!(m.count_ones(), want, "{p:?} active_mask({offset}, {total})");
            assert_eq!(p.count(&m), want);
            for lane in 0..MAX_LANES {
                assert_eq!(m.test(lane), lane < want);
            }
        }
    }
    let all = VMask::all(p);
    assert_eq!(all.count_ones(), n);
    assert_eq!((!all).count_ones(), 0);
    assert_eq!(!VMask::none(p), all);

    // Splat.
    for v in [0u8, 1, 0x5c, 0x80, 0xff] {
        assert_eq!(VVec::splat(p, v).as_slice(), vec![v; n].as_slice());
    }

    // Masked loads and stores against a model, including strips that run
    // past the end of the slice with only in-bounds lanes active.
    for round in 0..64 {
        let len = (rng.next() as usize) % (3 * n + 2);
        let data = rng.bytes(len);
        let offset = if len == 0 {
            0
        } else {
            (rng.next() as usize) % len
        };
        let mask = random_mask(p, &mut rng).and(p.active_mask(offset, len));
        let passthru = rng.next() as u8;
        let v = VVec::load_masked(&data, offset, &mask, passthru).unwrap();
        for i in 0..n {
            let want = if mask.test(i) {
                data[offset + i]
            } else {
                passthru
            };
            assert_eq!(v.as_slice()[i], want, "{p:?} round {round} lane {i}");
        }
        let mut out = rng.bytes(len);
        let before = out.clone();
        let src = VVec::splat(p, 0xa5).bit_xor(&v);
        src.store_masked(&mut out, offset, &mask).unwrap();
        for (j, (got, old)) in out.iter().zip(&before).enumerate() {
            let lane = j.wrapping_sub(offset);
            let want = if j >= offset && mask.test(lane) {
                src.as_slice()[lane]
            } else {
                *old
            };
            assert_eq!(*got, want, "{p:?} store round {round} byte {j}");
        }
    }

    // An active lane past the end is a typed error, not a partial result.
    let data = rng.bytes(n);
    let mask = p.active_mask(0, n);
    assert_eq!(
        VVec::load_masked(&data, 1, &mask, 0),
        Err(VvecError::OutOfBounds {
            offset: 1,
            lane: n - 1,
            len: n
        })
    );
    let mut out = vec![0u8; n];
    assert!(VVec::splat(p, 1).store_masked(&mut out, 1, &mask).is_err());
    assert_eq!(out, vec![0u8; n]);
    assert!(VVec::load_masked(&data, usize::MAX, &mask, 0).is_err());
    // An empty mask may sit anywhere.
    let none = VMask::none(p);
    assert_eq!(
        VVec::load_masked(&data, usize::MAX, &none, 7).unwrap(),
        VVec::splat(p, 7)
    );

    // Lane-wise operations and reductions.
    for _ in 0..64 {
        let (a, b) = (rng.bytes(n), rng.bytes(n));
        let mut b = b;
        for i in 0..n {
            if rng.next() & 3 == 0 {
                b[i] = a[i];
            }
        }
        let (va, vb) = (vec_of(p, &a), vec_of(p, &b));
        let eq = va.simd_eq(&vb);
        let lt = va.simd_lt(&vb);
        for i in 0..n {
            assert_eq!(eq.test(i), a[i] == b[i]);
            assert_eq!(lt.test(i), a[i] < b[i]);
        }
        assert!(!eq.test(n) && !lt.test(n));
        let zip =
            |f: fn(u8, u8) -> u8| a.iter().zip(&b).map(|(x, y)| f(*x, *y)).collect::<Vec<_>>();
        assert_eq!(va.bit_xor(&vb).as_slice(), zip(|x, y| x ^ y));
        assert_eq!(va.bit_and(&vb).as_slice(), zip(|x, y| x & y));
        assert_eq!(va.bit_or(&vb).as_slice(), zip(|x, y| x | y));
        assert_eq!(va.wrapping_add(&vb).as_slice(), zip(u8::wrapping_add));
        assert_eq!(va.reduce_xor(), a.iter().fold(0, |x, y| x ^ y));
    }

    // The §14.3 scanner, per-op and whole-loop, at every offset and length.
    let buf = {
        let mut b = rng.bytes(4096 + 64);
        for i in (0..b.len()).step_by(7) {
            b[i] = b'\n';
        }
        b
    };
    for delim in [0u8, 7, b'\n', 255] {
        for offset in 0..32 {
            for len in (0..300).chain([1000, 4096]) {
                let data = &buf[offset..offset + len];
                let want = reference(data, delim);
                let (m, x) = scan_record_delimiters(p, data, delim);
                assert_eq!(
                    ScanResult { matches: m, xor: x },
                    want,
                    "{p:?} VVec scan off {offset} len {len}"
                );
                assert_eq!(
                    kernels::scan(&p, data, delim),
                    want,
                    "{p:?} kernel off {offset} len {len}"
                );
            }
        }
    }
}

/// A hardware profile must also agree with the scalar profile of the same
/// width on identical inputs, operation by operation.
fn check_against_scalar<P: IsaProfile>(p: P) {
    let s = Scalar::new(p.lanes()).unwrap();
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    for _ in 0..256 {
        let len = (rng.next() as usize) % (2 * p.lanes() + 1) + 1;
        let data = rng.bytes(len);
        let offset = (rng.next() as usize) % len;
        let pass = rng.next() as u8;
        let hw = VVec::load_masked(&data, offset, &p.active_mask(offset, len), pass).unwrap();
        let sw = VVec::load_masked(&data, offset, &s.active_mask(offset, len), pass).unwrap();
        assert_eq!(hw.as_slice(), sw.as_slice());
        assert_eq!(hw.reduce_xor(), sw.reduce_xor());
        let needle = data[offset];
        assert_eq!(
            hw.simd_eq(&VVec::splat(p, needle)).words(),
            sw.simd_eq(&VVec::splat(s, needle)).words()
        );
        assert_eq!(
            hw.simd_lt(&VVec::splat(p, needle)).words(),
            sw.simd_lt(&VVec::splat(s, needle)).words()
        );
    }
}

#[test]
fn scalar_profiles_at_many_vector_lengths() {
    for lanes in [1, 3, 16, 17, 32, 48, 64, 100, 128, 192, 256] {
        check_profile(Scalar::new(lanes).unwrap(), lanes as u64 + 1);
    }
    assert_eq!(Scalar::new(0), Err(VvecError::InvalidLanes(0)));
    assert_eq!(
        Scalar::new(MAX_LANES + 1),
        Err(VvecError::InvalidLanes(MAX_LANES + 1))
    );
}

#[test]
fn sve_profile_matches_scalar() {
    match Sve::detect() {
        Ok(p) => {
            eprintln!(
                "SVE vector length: {} bytes ({} bits)",
                p.lanes(),
                p.lanes() * 8
            );
            check_profile(p, 11);
            check_against_scalar(p);
        }
        Err(e) => {
            assert!(!required("VESPER_REQUIRE_SVE"), "SVE required: {e}");
            assert!(
                matches!(e, VvecError::Unavailable(VectorIsa::Sve))
                    || cfg!(target_arch = "aarch64")
            );
            eprintln!("SVE unavailable here ({e}); skipped");
        }
    }
}

#[test]
fn rvv_profile_matches_scalar() {
    match Rvv::detect() {
        Ok(p) => {
            eprintln!("RVV VLEN: {} bits", p.lanes() * 8);
            check_profile(p, 13);
            check_against_scalar(p);
        }
        Err(e) => {
            assert!(!required("VESPER_REQUIRE_RVV"), "RVV required: {e}");
            eprintln!("RVV unavailable here ({e}); skipped");
        }
    }
}

#[test]
fn hardware_isas_report_consistently() {
    assert!(VectorIsa::Scalar.is_available());
    assert_eq!(VectorIsa::Scalar.hardware_vector_bytes(), None);
    for isa in [VectorIsa::Sve, VectorIsa::Rvv] {
        let bytes = isa.hardware_vector_bytes();
        assert_eq!(bytes.is_some(), isa.is_available(), "{isa:?}");
        if let Some(b) = bytes {
            assert!(b >= 16 && b % 16 == 0, "{isa:?} reports {b} bytes");
        }
    }
    if !cfg!(target_arch = "aarch64") {
        assert!(!VectorIsa::Sve.is_available());
    }
    if !cfg!(target_arch = "riscv64") {
        assert!(!VectorIsa::Rvv.is_available());
    }
}

/// Inactive lanes must not be read: a strip whose tail lies on a
/// `PROT_NONE` page loads and stores without faulting.
#[cfg(unix)]
#[test]
fn masked_access_skips_a_guard_page() {
    fn run<P: IsaProfile>(p: P) {
        // SAFETY: plain anonymous mappings, unmapped before returning.
        unsafe {
            let page = libc::sysconf(libc::_SC_PAGESIZE) as usize;
            let base = libc::mmap(
                std::ptr::null_mut(),
                2 * page,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            );
            assert_ne!(base, libc::MAP_FAILED);
            assert_eq!(
                libc::mprotect(base.cast::<u8>().add(page).cast(), page, libc::PROT_NONE),
                0
            );
            let data = std::slice::from_raw_parts_mut(base.cast::<u8>(), page);
            for (i, b) in data.iter_mut().enumerate() {
                *b = i as u8;
            }
            for k in 1..=p.lanes().min(64) {
                // The last `k` bytes of the page; lanes past them are on the
                // guard page and must stay inactive.
                let offset = page - k;
                let mask = p.active_mask(offset, page);
                let v = VVec::load_masked(data, offset, &mask, 0xee).unwrap();
                for i in 0..p.lanes() {
                    let want = if i < k { (offset + i) as u8 } else { 0xee };
                    assert_eq!(v.as_slice()[i], want);
                }
                v.bit_xor(&VVec::splat(p, 0xff))
                    .store_masked(data, offset, &mask)
                    .unwrap();
                v.store_masked(data, offset, &mask).unwrap();
                let r = kernels::scan(&p, &data[offset..], (offset as u8).wrapping_add(1));
                assert_eq!(
                    r,
                    reference(&data[offset..], (offset as u8).wrapping_add(1))
                );
            }
            assert_eq!(libc::munmap(base, 2 * page), 0);
        }
    }
    run(Scalar::new(64).unwrap());
    if let Ok(p) = Sve::detect() {
        run(p);
    }
    if let Ok(p) = Rvv::detect() {
        run(p);
    }
}
