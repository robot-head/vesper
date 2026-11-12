//! Relocatable object emission for the ABI v2 targets (ELF and Mach-O).
use object::{Architecture, Object, ObjectSection, ObjectSymbol, RelocationTarget};
use vesper_fiber_abi::clif::PARK_SYMBOL;
use vesper_fiber_abi::prototype::{
    CANCEL_REPARK_SYMBOL, NESTED_SYMBOL, OptLevel, PULSE_READ_SYMBOL, STALE_BASE_SYMBOL,
    emit_object,
};

fn check(triple: &str, arch: Architecture, call_reloc: u32) {
    for opt in [OptLevel::None, OptLevel::Speed] {
        let (bytes, clif) = emit_object(triple, opt).unwrap();
        assert!(clif.contains("call fn0"));
        let file = object::File::parse(&*bytes).unwrap();
        assert_eq!(file.architecture(), arch);
        assert_eq!(file.format(), object::BinaryFormat::Elf);
        for name in [
            PULSE_READ_SYMBOL,
            NESTED_SYMBOL,
            STALE_BASE_SYMBOL,
            CANCEL_REPARK_SYMBOL,
        ] {
            let sym = file
                .symbols()
                .find(|s| s.name() == Ok(name))
                .unwrap_or_else(|| panic!("{triple}: missing {name}"));
            assert!(sym.is_definition() && sym.is_global(), "{triple}: {name}");
            assert!(sym.size() > 0);
        }
        let park = file
            .symbols()
            .find(|s| s.name() == Ok(PARK_SYMBOL))
            .expect("park import");
        assert!(park.is_undefined());
        // PIC objects call the import through the GOT: one GOT relocation
        // per park site.
        let text = file.section_by_name(".text").unwrap();
        let calls = text
            .relocations()
            .filter(|(_, r)| {
                r.target() == RelocationTarget::Symbol(park.index())
                    && matches!(r.flags(), object::RelocationFlags::Elf { r_type } if r_type == call_reloc)
            })
            .count();
        // pulse_read: 2 parks, nested: 2, stale_base: 1, cancel_repark: 2.
        assert_eq!(calls, 7, "{triple} {opt:?}");
    }
}

#[test]
fn aarch64_object_imports_park_through_the_got() {
    check(
        "aarch64-unknown-linux-gnu",
        Architecture::Aarch64,
        object::elf::R_AARCH64_ADR_GOT_PAGE,
    );
}

#[test]
fn x86_64_object_imports_park_through_the_got() {
    check(
        "x86_64-unknown-linux-gnu",
        Architecture::X86_64,
        object::elf::R_X86_64_GOTPCREL,
    );
}

#[test]
fn unsupported_targets_are_rejected() {
    assert!(emit_object("riscv64gc-unknown-linux-gnu", OptLevel::None).is_err());
    assert!(emit_object("not a triple", OptLevel::None).is_err());
}

/// Darwin AArch64: Mach-O, underscore-prefixed C symbols, the Apple arm64
/// calling convention, and GOT-indirect calls to the park import.
#[test]
fn aarch64_darwin_object_is_macho_and_imports_park_through_the_got() {
    for opt in [OptLevel::None, OptLevel::Speed] {
        let (bytes, clif) = emit_object("aarch64-apple-darwin", opt).unwrap();
        assert!(clif.contains("apple_aarch64"), "{clif}");
        assert!(!clif.contains("system_v"));
        let file = object::File::parse(&*bytes).unwrap();
        assert_eq!(file.architecture(), Architecture::Aarch64);
        assert_eq!(file.format(), object::BinaryFormat::MachO);
        for name in [
            PULSE_READ_SYMBOL,
            NESTED_SYMBOL,
            STALE_BASE_SYMBOL,
            CANCEL_REPARK_SYMBOL,
        ] {
            let mangled = format!("_{name}");
            let sym = file
                .symbols()
                .find(|s| s.name() == Ok(mangled.as_str()))
                .unwrap_or_else(|| panic!("missing {mangled}"));
            assert!(sym.is_definition() && sym.is_global(), "{mangled}");
        }
        let park_name = format!("_{PARK_SYMBOL}");
        let park = file
            .symbols()
            .find(|s| s.name() == Ok(park_name.as_str()))
            .expect("park import");
        assert!(park.is_undefined());
        let text = file.section_by_name("__text").unwrap();
        let got_pages = text
            .relocations()
            .filter(|(_, r)| {
                r.target() == RelocationTarget::Symbol(park.index())
                    && matches!(
                        r.flags(),
                        object::RelocationFlags::MachO { r_type, .. }
                            if r_type == object::macho::ARM64_RELOC_GOT_LOAD_PAGE21
                    )
            })
            .count();
        assert_eq!(got_pages, 7, "{opt:?}");
    }
}
