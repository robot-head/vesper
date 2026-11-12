//! BPF ELF emission: round trip through this crate's reader, parsing by
//! aya's object reader, and (when available) kernel validation of the BTF
//! and a load of the program rebuilt from the object.

mod common;

use common::corpus::corpus;
use vesper_ebpf::elf::{maps_btf, parse_maps_btf, read_object, write_object};
use vesper_ebpf::mitigator::{MitigatorConfig, build};

#[test]
fn round_trips_every_accepted_corpus_program() {
    for case in corpus().into_iter().filter(|c| c.rule.is_none()) {
        let bytes = write_object(&case.prog).unwrap();
        let back = read_object(&bytes).unwrap();
        assert_eq!(back.insns, case.prog.insns, "{}", case.name);
        assert_eq!(back.maps, case.prog.maps, "{}", case.name);
        assert_eq!(back.map_relocs, case.prog.map_relocs, "{}", case.name);
        assert_eq!(back.license, case.prog.license, "{}", case.name);
    }
}

#[test]
fn aya_parses_the_mitigator_object() {
    let p = build(&MitigatorConfig::default()).unwrap();
    let bytes = write_object(&p).unwrap();
    let obj = aya_obj::Object::parse(&bytes).expect("aya-obj parses the object");
    assert_eq!(obj.license.to_str().unwrap(), "Dual MIT/GPL");
    let counts = &obj.maps["syn_counts"];
    assert_eq!(
        (
            counts.map_type(),
            counts.key_size(),
            counts.value_size(),
            counts.max_entries()
        ),
        (5, 4, 8, 65_536)
    );
    let alerts = &obj.maps["syn_alerts"];
    assert_eq!((alerts.map_type(), alerts.max_entries()), (27, 1 << 16));
    let prog = &obj.programs["mitigate_syn_flood"];
    assert!(matches!(prog.section, aya_obj::ProgramSection::Xdp { .. }));
    let f = obj.functions.values().next().expect("function");
    assert_eq!(f.instructions.len(), p.insns.len());
    // aya resolves every map relocation to the fd it is given
    let mut obj = obj;
    let maps: Vec<(String, aya_obj::Map)> = obj
        .maps
        .iter()
        .map(|(n, m)| (n.clone(), m.clone()))
        .collect();
    let fd_of = |n: &str| if n == "syn_counts" { 100 } else { 101 };
    obj.relocate_maps(
        maps.iter().map(|(n, m)| (n.as_str(), fd_of(n), m)),
        &Default::default(),
    )
    .expect("aya relocates the map references");
    let f = obj.functions.values().next().expect("function");
    for &(slot, map) in &p.map_relocs {
        let ins = &f.instructions[slot];
        assert_eq!(ins.src_reg(), 1, "BPF_PSEUDO_MAP_FD at {slot}");
        assert_eq!(ins.imm, if map == 0 { 100 } else { 101 });
    }
}

#[test]
fn btf_round_trips_and_rejects_garbage() {
    let p = build(&MitigatorConfig::default()).unwrap();
    let (btf, offsets, size) = maps_btf(&p.maps);
    assert_eq!(offsets, vec![0, 32]);
    assert_eq!(size, 48);
    let back: Vec<_> = parse_maps_btf(&btf)
        .unwrap()
        .into_iter()
        .map(|(_, d)| d)
        .collect();
    assert_eq!(back, p.maps);
    assert!(parse_maps_btf(&btf[..10]).is_err());
    let mut bad = btf.clone();
    bad[0] = 0;
    assert!(parse_maps_btf(&bad).is_err());
    for cut in 0..btf.len() {
        // never panics on truncated input
        let _ = parse_maps_btf(&btf[..cut]);
    }
    assert!(read_object(b"not an elf").is_err());
}

#[cfg(target_os = "linux")]
#[test]
fn kernel_accepts_the_btf_and_the_object_program() {
    use vesper_ebpf::kernel;
    if let Err(e) = kernel::probe() {
        eprintln!("kernel BPF evidence unavailable on this host: {e}");
        return;
    }
    let p = build(&MitigatorConfig::default()).unwrap();
    let (btf, _, _) = maps_btf(&p.maps);
    kernel::load_btf(&btf).expect("kernel validates the .BTF blob");
    let from_obj = read_object(&write_object(&p).unwrap()).unwrap();
    kernel::load(&from_obj).expect("kernel loads the program rebuilt from the object");
}
