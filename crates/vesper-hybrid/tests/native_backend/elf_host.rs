use super::binding;
use object::{Object, ObjectSection, ObjectSymbol, RelocationFlags, RelocationTarget, elf};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::{fs, process::Command};
use vesper::{check, native, parser};

fn checked(source: &str) -> check::Checked {
    let parsed = parser::parse(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let checked = check::check(&parsed.module);
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
    checked
}

#[test]
fn ordinary_native_objects_use_opt_in_elf_with_strings_and_vtables() {
    static EMITTED: AtomicU64 = AtomicU64::new(0);
    let code = checked("fn main() -> u64 { 42 }");
    let data = checked("fn n(s: Str) -> u64 { s.len() } fn main() -> u64 { n(\"read-only\") }");
    let vtable = checked(
        "
        trait Read { fn read(in self) -> i64; }
        impl Read for i64 { fn read(in self) -> i64 { self } }
        fn take(in value: dyn Read) -> i64 { value.read() }
        fn main() -> i64 { let value: i64 = 42; take(in value) }
    ",
    );
    let growth = checked(include_str!("../../../../tests/fixtures/seq/growth.ves"));
    let object = |checked: &check::Checked| {
        native::object_with_options(&checked.world, "main", None, false)
            .unwrap()
            .0
    };
    let baseline_code = object(&code);
    let baseline_data = object(&data);
    let baseline_vtable = object(&vtable);
    let baseline_growth = object(&growth);
    assert!(
        object::File::parse(baseline_data.as_slice())
            .unwrap()
            .sections()
            .any(|section| {
                section.kind() == object::SectionKind::ReadOnlyData && section.size() != 0
            })
    );
    assert!(
        native::elf::install_object_encoder(|words, bytes| {
            let object = binding::x86_64_elf_object(words, bytes);
            if object.is_empty() {
                None
            } else {
                EMITTED.fetch_add(1, Relaxed);
                Some(object)
            }
        })
        .is_ok()
    );

    let candidate_code = object(&code);
    assert_eq!(EMITTED.load(Relaxed), 1);
    let candidate = object::File::parse(candidate_code.as_slice()).unwrap();
    let baseline = object::File::parse(baseline_code.as_slice()).unwrap();
    assert_eq!(
        candidate.section_by_name(".text").unwrap().data().unwrap(),
        baseline.section_by_name(".text").unwrap().data().unwrap(),
    );
    let file_symbols = |file: &object::File<'_>| {
        file.symbols()
            .filter(|symbol| symbol.kind() == object::SymbolKind::File)
            .map(|symbol| symbol.name_bytes().unwrap().to_vec())
            .collect::<Vec<_>>()
    };
    assert_eq!(file_symbols(&candidate), file_symbols(&baseline));
    let candidate_data = object(&data);
    assert_eq!(EMITTED.load(Relaxed), 2);
    let actual = object::File::parse(candidate_data.as_slice()).unwrap();
    let expected = object::File::parse(baseline_data.as_slice()).unwrap();
    for name in [".text", ".rodata"] {
        let actual = actual.section_by_name(name).unwrap();
        let expected = expected.section_by_name(name).unwrap();
        assert_eq!(actual.align(), expected.align());
        assert_eq!(actual.flags(), expected.flags());
        assert_eq!(actual.data().unwrap(), expected.data().unwrap());
    }
    let relocations = |file: &object::File<'_>| {
        file.section_by_name(".text")
            .unwrap()
            .relocations()
            .map(|(offset, relocation)| {
                let RelocationTarget::Symbol(id) = relocation.target() else {
                    panic!("expected symbol relocation")
                };
                let symbol = file.symbol_by_index(id).unwrap();
                (
                    offset,
                    symbol.name_bytes().unwrap().to_vec(),
                    relocation.flags(),
                    relocation.addend(),
                )
            })
            .collect::<Vec<_>>()
    };
    assert!(!relocations(&expected).is_empty());
    assert_eq!(relocations(&actual), relocations(&expected));
    let (observed, _) =
        native::object_with_observer(&code.world, "main", None, false, Some(&mut |_| {})).unwrap();
    assert_eq!(observed, baseline_code);
    assert_eq!(EMITTED.load(Relaxed), 2);

    assert!(native::machine::install_integer_encoder(|_| None).is_ok());
    assert!(
        native::machine::install_relocation_encoder(|words| {
            let packet = binding::x86_64_relocatable_block(words);
            (!packet.is_empty()).then_some(packet)
        })
        .is_ok()
    );
    let candidate_data = object(&data);
    assert_eq!(EMITTED.load(Relaxed), 3);
    let actual = object::File::parse(candidate_data.as_slice()).unwrap();
    let pc32 = RelocationFlags::Elf {
        r_type: elf::R_X86_64_PC32,
    };
    assert!(
        relocations(&actual).iter().any(|(_, _, flags, addend)| {
            *flags == pc32 && *addend == -(size_of::<i32>() as i64)
        })
    );
    let candidate_vtable = object(&vtable);
    assert_eq!(EMITTED.load(Relaxed), 4);
    let table = object::File::parse(candidate_vtable.as_slice()).unwrap();
    assert!(
        table
            .section_by_name(".data.rel.ro")
            .unwrap()
            .relocations()
            .any(|(_, relocation)| relocation.flags()
                == RelocationFlags::Elf {
                    r_type: elf::R_X86_64_64,
                })
    );
    let candidate_growth = object(&growth);
    assert_eq!(EMITTED.load(Relaxed), 5);
    let sequence = object::File::parse(candidate_growth.as_slice()).unwrap();
    assert!(
        sequence
            .symbols()
            .any(|symbol| symbol.is_undefined() && symbol.name() == Ok("memmove"))
    );
    if Command::new("cc").arg("--version").output().is_err() {
        return;
    }
    let dir = std::env::temp_dir().join(format!("vesper-sh7-data-{}", std::process::id()));
    fs::create_dir(&dir).unwrap();
    for (name, object, result) in [
        ("rodata", candidate_data, 9),
        ("vtable-baseline", baseline_vtable, 42),
        ("vtable-candidate", candidate_vtable, 42),
        ("growth-baseline", baseline_growth, 122),
        ("growth-candidate", candidate_growth, 122),
    ] {
        let obj = dir.join(format!("{name}.o"));
        let exe = dir.join(name);
        fs::write(&obj, object).unwrap();
        let link = Command::new("cc")
            .arg(&obj)
            .arg("-o")
            .arg(&exe)
            .output()
            .unwrap();
        assert!(
            link.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&link.stderr)
        );
        assert_eq!(
            Command::new(&exe).status().unwrap().code(),
            Some(result),
            "{name}"
        );
    }
    fs::remove_dir_all(dir).unwrap();
}
