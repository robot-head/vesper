//! Relocatable object emission for both ABI v1 targets.
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
