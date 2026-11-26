use super::binding;
use object::{Object, ObjectSection, ObjectSymbol};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use vesper::{check, native, parser};

fn checked(source: &str) -> check::Checked {
    let parsed = parser::parse(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let checked = check::check(&parsed.module);
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
    checked
}

#[test]
fn ordinary_native_objects_use_opt_in_elf_with_full_fallback() {
    static EMITTED: AtomicU64 = AtomicU64::new(0);
    let code = checked("fn main() -> u64 { 42 }");
    let data = checked("fn n(s: Str) -> u64 { s.len() } fn main() -> u64 { n(\"fallback\") }");
    let object = |checked: &check::Checked| {
        native::object_with_options(&checked.world, "main", None, false)
            .unwrap()
            .0
    };
    let baseline_code = object(&code);
    let baseline_data = object(&data);
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
    assert_eq!(object(&data), baseline_data);
    let (observed, _) =
        native::object_with_observer(&code.world, "main", None, false, Some(&mut |_| {})).unwrap();
    assert_eq!(observed, baseline_code);
    assert_eq!(EMITTED.load(Relaxed), 1);
}
