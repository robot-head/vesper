use super::*;
use object::{Object, ObjectSection, ObjectSymbol};

fn reference(data: &[u8], delim: u8) -> ScanResult {
    ScanResult {
        matches: data.iter().filter(|byte| **byte == delim).count() as u64,
        xor: data.iter().fold(0, |a, b| a ^ b),
    }
}

fn scanner() -> Option<JitScanner> {
    match JitScanner::compile_profile(IsaProfileKind::Baseline128) {
        Ok(scanner) => Some(scanner),
        Err(Error::MissingBaselineFeatures | Error::UnsupportedHost) => {
            eprintln!("Baseline128 unavailable on this host; native execution skipped");
            None
        }
        Err(e) => panic!("JIT code generation: {e}"),
    }
}

#[test]
fn simulated_discovery_cannot_mint_capabilities() {
    for missing in 0..4 {
        let mut bits = [true; 4];
        bits[missing] = false;
        assert_eq!(
            check_features(Features::X86 {
                sse3: bits[0],
                ssse3: bits[1],
                sse41: bits[2],
                sse42: bits[3],
            }),
            Err(Error::MissingBaselineFeatures)
        );
    }
    assert_eq!(
        check_features(Features::Aarch64 { neon: false }),
        Err(Error::MissingBaselineFeatures)
    );
    assert_eq!(check_features(Features::Aarch64 { neon: true }), Ok(()));
    assert_eq!(check_features(Features::Other), Err(Error::UnsupportedHost));
    assert_eq!(
        Baseline128Capability::detect().is_ok(),
        profile_support(IsaProfileKind::Baseline128).is_ok()
    );
}

#[test]
fn unsupported_profiles_reject_before_executable_creation() {
    for profile in [
        IsaProfileKind::Avx2,
        IsaProfileKind::Avx512,
        IsaProfileKind::Sve2,
        IsaProfileKind::Rvv1,
    ] {
        assert_eq!(
            profile_support(profile),
            Err(Error::UnsupportedProfile(profile))
        );
        assert!(
            matches!(JitScanner::compile_profile(profile), Err(Error::UnsupportedProfile(p)) if p == profile)
        );
        for target in [ElfTarget::X86_64, ElfTarget::Aarch64] {
            assert!(
                matches!(ObjectArtifact::emit(target, profile), Err(Error::UnsupportedProfile(p)) if p == profile)
            );
        }
    }
}

#[test]
fn native_all_byte_values_and_delimiters() {
    let Some(scanner) = scanner() else { return };
    let data: Vec<u8> = (0..=255).cycle().take(768).collect();
    for delim in 0..=255 {
        assert_eq!(scanner.scan(&data, delim), reference(&data, delim));
    }
    for len in [0, 1, 15, 16, 17, 255, 256, 257] {
        for byte in [0, 255, 37] {
            let data = vec![byte; len];
            assert_eq!(scanner.scan(&data, byte), reference(&data, byte));
            assert_eq!(
                scanner.scan(&data, byte ^ 0xff),
                reference(&data, byte ^ 0xff)
            );
        }
    }
}

#[test]
fn native_seeded_all_tails_unaligned_and_large() {
    let Some(scanner) = scanner() else { return };
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    let data: Vec<u8> = (0..1_048_609)
        .map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed as u8
        })
        .collect();
    for offset in 0..32 {
        for len in 0..=193 {
            let slice = &data[offset..offset + len];
            for delim in [0, 1, 127, 255] {
                assert_eq!(
                    scanner.scan(slice, delim),
                    reference(slice, delim),
                    "offset={offset}, len={len}, delim={delim}"
                );
            }
        }
    }
    for len in [4095, 4096, 4097, 65_537, 1_048_577] {
        let slice = &data[7..7 + len];
        for delim in [0, 255, 42] {
            assert_eq!(scanner.scan(slice, delim), reference(slice, delim));
        }
    }
}

#[test]
fn cloned_handles_keep_code_live_after_original_drop() {
    let Some(original) = scanner() else { return };
    let clone = original.clone();
    let second = clone.clone();
    drop(original);
    let input = b"the handle owns executable code\0\xff";
    assert_eq!(clone.scan(input, b'e'), reference(input, b'e'));
    drop(clone);
    assert_eq!(second.scan(input, 255), reference(input, 255));
    assert_eq!(second.metadata().profile, IsaProfileKind::Baseline128);
    assert!(second.clif().contains("load.i8x16"));
}

#[test]
fn real_cross_target_elfs_and_exact_profile_metadata() {
    for (target, expected_arch) in [
        (ElfTarget::X86_64, object::Architecture::X86_64),
        (ElfTarget::Aarch64, object::Architecture::Aarch64),
    ] {
        let artifact = ObjectArtifact::emit(target, IsaProfileKind::Baseline128).unwrap();
        let object = object::File::parse(artifact.bytes()).unwrap();
        assert_eq!(object.format(), object::BinaryFormat::Elf);
        assert_eq!(object.kind(), object::ObjectKind::Relocatable);
        assert_eq!(object.architecture(), expected_arch);
        let symbol = object
            .symbols()
            .find(|s| s.name().ok() == Some("vesper_simd_scan_baseline128_v1"))
            .unwrap();
        assert!(!symbol.is_undefined());
        assert!(symbol.is_global());
        assert!(symbol.size() > 64);
        let section = object
            .section_by_index(symbol.section_index().unwrap())
            .unwrap();
        assert!(section.data().unwrap().len() >= symbol.size() as usize);
        assert!(
            object
                .symbols()
                .all(|s| !s.is_undefined() || s.name().unwrap_or_default().is_empty())
        );
        let meta = artifact.metadata();
        assert_eq!(meta.profile, IsaProfileKind::Baseline128);
        assert_eq!(meta.target, target.triple().to_string());
        assert_eq!(meta.vector_bits, 128);
        assert_eq!(meta.tail_strategy, "scalar-checked-remainder");
        assert!(artifact.clif().contains("load.i8x16"));
        assert!(artifact.clif().contains("icmp eq"));
        assert!(artifact.clif().contains("bxor"));
        if target == ElfTarget::X86_64 {
            assert_eq!(
                meta.enabled_isa_flags,
                ["has_sse3", "has_sse41", "has_sse42", "has_ssse3"]
            );
        } else {
            // AArch64 SIMD/FP is the baseline; no SVE extension enabled.
            assert!(
                meta.enabled_isa_flags
                    .iter()
                    .all(|flag| !flag.contains("sve"))
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn guard_pages_prove_no_front_or_tail_load_crosses_slice() {
    let Some(scanner) = scanner() else { return };
    // Two inaccessible neighbors catch accidental loads before/after the
    // readable page. A fault fails the test process, rather than being caught.
    struct Mapping {
        base: *mut libc::c_void,
        len: usize,
    }
    impl Drop for Mapping {
        fn drop(&mut self) {
            // SAFETY: mapping was obtained by mmap and is owned here.
            assert_eq!(unsafe { libc::munmap(self.base, self.len) }, 0);
        }
    }
    // SAFETY: sysconf takes a constant selector and has no pointer arguments.
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    assert!(page >= 4096);
    let page = page as usize;
    // SAFETY: creates a fresh anonymous mapping, with no accessible bytes yet.
    let base = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            page * 3,
            libc::PROT_NONE,
            libc::MAP_PRIVATE | libc::MAP_ANON,
            -1,
            0,
        )
    };
    assert_ne!(base, libc::MAP_FAILED);
    let _mapping = Mapping {
        base,
        len: page * 3,
    };
    // SAFETY: the middle page is inside the live mmap allocation.
    let middle = unsafe { base.cast::<u8>().add(page) };
    // SAFETY: whole middle page lies inside the owned mapping.
    assert_eq!(
        unsafe { libc::mprotect(middle.cast(), page, libc::PROT_READ | libc::PROT_WRITE) },
        0
    );
    // SAFETY: middle page is writable and live; no other references exist.
    let bytes = unsafe { std::slice::from_raw_parts_mut(middle, page) };
    for (i, byte) in bytes.iter_mut().enumerate() {
        *byte = i as u8;
    }
    for len in 0..=95 {
        for data in [&bytes[..len], &bytes[page - len..]] {
            for delim in [0, 255, 17] {
                assert_eq!(
                    scanner.scan(data, delim),
                    reference(data, delim),
                    "guard-page len={len}"
                );
            }
        }
    }
}

/// The Vesper port builds the same function as the Rust builder: the same
/// CLIF text and the same object bytes for both ELF targets, and the same
/// CLIF in a JIT module.
#[cfg(feature = "vesper-port")]
#[test]
fn vesper_port_builds_the_reference_scanner() {
    fn object_module(target: ElfTarget) -> ObjectModule {
        let isa = make_isa(target.triple()).unwrap();
        let builder = ObjectBuilder::new(isa, "vesper-simd", default_libcall_names()).unwrap();
        ObjectModule::new(builder)
    }
    for target in [ElfTarget::X86_64, ElfTarget::Aarch64] {
        let mut rust = object_module(target);
        let (_, rust_clif) = reference_define_scanner(&mut rust).unwrap();
        let mut vesper = object_module(target);
        let (_, vesper_clif) = define_scanner(&mut vesper, false).unwrap();
        assert_eq!(vesper_clif, rust_clif, "{target:?} CLIF");
        let rust_bytes = rust.finish().emit().unwrap();
        let vesper_bytes = vesper.finish().emit().unwrap();
        assert!(vesper_bytes == rust_bytes, "{target:?} object bytes differ");
        assert_eq!(
            ObjectArtifact::emit(target, IsaProfileKind::Baseline128)
                .unwrap()
                .bytes(),
            &rust_bytes[..]
        );
    }
    if profile_support(IsaProfileKind::Baseline128).is_ok() {
        let jit = |port: bool| {
            let isa = make_isa(Triple::host()).unwrap();
            let mut module = JITModule::new(JITBuilder::with_isa(isa, default_libcall_names()));
            let (_, clif) = if port {
                define_scanner(&mut module, true).unwrap()
            } else {
                reference_define_scanner(&mut module).unwrap()
            };
            // SAFETY: nothing was finalized, so no code escaped.
            unsafe { module.free_memory() };
            clif
        };
        assert_eq!(jit(true), jit(false));
    }
}

/// Every profile and simulated host gets the same answer from the port as
/// from the Rust decision it replaces.
#[cfg(feature = "vesper-port")]
#[test]
fn vesper_port_profile_decisions_match_rust() {
    let reference = |features: Features| -> Result<(), Error> {
        match features {
            Features::X86 {
                sse3,
                ssse3,
                sse41,
                sse42,
            } if sse3 && ssse3 && sse41 && sse42 => Ok(()),
            Features::Aarch64 { neon: true } => Ok(()),
            Features::Other => Err(Error::UnsupportedHost),
            _ => Err(Error::MissingBaselineFeatures),
        }
    };
    for bits in 0..16u32 {
        let bit = |i: u32| bits & (1 << i) != 0;
        for features in [
            Features::X86 {
                sse3: bit(0),
                ssse3: bit(1),
                sse41: bit(2),
                sse42: bit(3),
            },
            Features::Aarch64 { neon: bit(0) },
            Features::Other,
        ] {
            assert_eq!(check_features(features), reference(features));
        }
    }
    for profile in [
        IsaProfileKind::Baseline128,
        IsaProfileKind::Avx2,
        IsaProfileKind::Avx512,
        IsaProfileKind::Sve2,
        IsaProfileKind::Rvv1,
    ] {
        for features in [0, 2, 3] {
            for bits in [32, 64] {
                let want = if profile != IsaProfileKind::Baseline128 {
                    1
                } else if bits != 64 {
                    2
                } else {
                    features
                };
                assert_eq!(
                    port::profile_support(profile_code(profile), bits, features),
                    want,
                    "{profile:?} {features} {bits}"
                );
            }
        }
    }
}
