//! PVH direct boot and the VM Generation ID device, without KVM.
use vesper_aether::devices::vmgenid::*;
use vesper_aether::fork::VmGenId;
use vesper_aether::guest_mem::{GuestMemory, MemError};
use vesper_aether::pvh::*;

const CODE: [u8; 10] = [0x8B, 0x03, 0xE6, 0x10, 0x8B, 0x43, 0x04, 0xE6, 0x10, 0xF4];

fn mem() -> GuestMemory {
    GuestMemory::new(&[(0, 2 << 20), (0x1000_0000, 1 << 20)]).unwrap()
}

#[test]
fn pvh_loads_segments_start_info_memmap_and_cmdline() {
    let mut m = mem();
    let elf = build_test_pvh_elf(0x10_0000, &CODE);
    let image = parse_pvh_elf(&elf, &m).unwrap();
    assert_eq!(image.entry, 0x10_0000);
    assert_eq!(image.segments.len(), 1);
    let cpu = load_pvh(&image, &elf, &mut m, "console=hvc0 quiet").unwrap();
    // Code is at the entry.
    let mut code = [0u8; 10];
    m.read(0x10_0000, &mut code).unwrap();
    assert_eq!(code, CODE);
    // hvm_start_info.
    assert_eq!(m.read_u32(START_INFO_GPA).unwrap(), HVM_START_MAGIC);
    assert_eq!(m.read_u32(START_INFO_GPA + 4).unwrap(), 1);
    assert_eq!(m.read_u32(START_INFO_GPA + 12).unwrap(), 0, "no modules");
    assert_eq!(m.read_u64(START_INFO_GPA + 24).unwrap(), CMDLINE_GPA);
    assert_eq!(m.read_u64(START_INFO_GPA + 32).unwrap(), 0, "no ACPI RSDP");
    assert_eq!(m.read_u64(START_INFO_GPA + 40).unwrap(), MEMMAP_GPA);
    assert_eq!(m.read_u32(START_INFO_GPA + 48).unwrap(), 2);
    // Memory map: both RAM regions.
    assert_eq!(m.read_u64(MEMMAP_GPA).unwrap(), 0);
    assert_eq!(m.read_u64(MEMMAP_GPA + 8).unwrap(), 2 << 20);
    assert_eq!(m.read_u32(MEMMAP_GPA + 16).unwrap(), HVM_MEMMAP_TYPE_RAM);
    assert_eq!(m.read_u64(MEMMAP_GPA + 24).unwrap(), 0x1000_0000);
    assert_eq!(m.read_u64(MEMMAP_GPA + 32).unwrap(), 1 << 20);
    // Command line, NUL terminated.
    let mut c = [0u8; 20];
    m.read(CMDLINE_GPA, &mut c).unwrap();
    assert_eq!(&c, b"console=hvc0 quiet\0\0");
    // vCPU state of the PVH ABI.
    assert_eq!(cpu.rip, 0x10_0000);
    assert_eq!(cpu.rbx, START_INFO_GPA);
    assert_eq!(cpu.cr0 & 1, 1, "CR0.PE");
    assert_eq!(cpu.cr0 & (1 << 31), 0, "paging off");
    assert_eq!((cpu.cr4, cpu.efer), (0, 0));
    assert_eq!(cpu.rflags & (1 << 9), 0, "IF clear");
    assert_eq!(
        (cpu.cs.base, cpu.cs.limit, cpu.cs.kind),
        (0, 0xffff_ffff, 0xb)
    );
    assert!(cpu.cs.db && cpu.cs.s && cpu.cs.present && cpu.cs.g && !cpu.cs.l);
    assert_eq!(
        (cpu.data.base, cpu.data.limit, cpu.data.kind),
        (0, 0xffff_ffff, 0x3)
    );
    assert_eq!((cpu.tr.kind, cpu.tr.limit, cpu.tr.s), (0xb, 0x67, false));
    // The boot GDT matches the cached segments.
    assert_eq!(m.read_u64(BOOT_GDT_GPA + 8).unwrap(), 0x00cf_9b00_0000_ffff);
    assert_eq!(
        m.read_u64(BOOT_GDT_GPA + 16).unwrap(),
        0x00cf_9300_0000_ffff
    );
    assert_eq!(cpu.gdt_base, BOOT_GDT_GPA);
}

fn patch(elf: &mut [u8], at: usize, bytes: &[u8]) {
    elf[at..at + bytes.len()].copy_from_slice(bytes);
}

#[test]
fn pvh_rejects_malformed_images() {
    let m = mem();
    let good = build_test_pvh_elf(0x10_0000, &CODE);
    let ph_load = 64;
    let ph_note = 64 + 56;
    let check = |elf: &[u8], want: PvhError| {
        assert_eq!(parse_pvh_elf(elf, &m).unwrap_err(), want);
    };
    check(&good[..40], PvhError::Truncated);
    let mut e = good.clone();
    e[0] = 0;
    check(&e, PvhError::BadMagic);
    let mut e = good.clone();
    e[4] = 1;
    check(&e, PvhError::NotElf64LittleEndian);
    let mut e = good.clone();
    patch(&mut e, 18, &183u16.to_le_bytes());
    check(&e, PvhError::NotX86_64Executable);
    let mut e = good.clone();
    patch(&mut e, 56, &0u16.to_le_bytes());
    check(&e, PvhError::BadProgramHeaders);
    let mut e = good.clone();
    patch(&mut e, 32, &u64::MAX.to_le_bytes());
    check(&e, PvhError::BadProgramHeaders);
    // Segment file range past the end of the file.
    let mut e = good.clone();
    patch(&mut e, ph_load + 32, &10_000u64.to_le_bytes());
    patch(&mut e, ph_load + 40, &10_000u64.to_le_bytes());
    check(&e, PvhError::SegmentFileRange { index: 0 });
    // Segment outside guest RAM.
    let mut e = good.clone();
    patch(&mut e, ph_load + 24, &0x8000_0000u64.to_le_bytes());
    check(&e, PvhError::SegmentOutsideRam { index: 0 });
    // Segment over the start_info page.
    let mut e = good.clone();
    patch(&mut e, ph_load + 24, &0x6000u64.to_le_bytes());
    check(&e, PvhError::SegmentOverBootData { index: 0 });
    // memsz < filesz.
    let mut e = good.clone();
    patch(&mut e, ph_load + 40, &1u64.to_le_bytes());
    check(&e, PvhError::SegmentFileRange { index: 0 });
    // Missing PVH note (rename the note owner).
    let mut e = good.clone();
    let note_off = u64::from_le_bytes(e[ph_note + 8..ph_note + 16].try_into().unwrap()) as usize;
    patch(&mut e, note_off + 12, b"Xex\0");
    check(&e, PvhError::MissingPvhNote);
    // Note descriptor size not 4 or 8.
    let mut e = good.clone();
    patch(&mut e, note_off + 4, &2u32.to_le_bytes());
    check(&e, PvhError::MalformedNote);
    // Note running past its segment.
    let mut e = good.clone();
    patch(&mut e, note_off, &1000u32.to_le_bytes());
    check(&e, PvhError::MalformedNote);
    // Entry outside the executable segment.
    let mut e = good.clone();
    patch(&mut e, note_off + 16, &0x20_0000u32.to_le_bytes());
    check(&e, PvhError::EntryNotExecutable { entry: 0x20_0000 });
    // Non-executable segment holding the entry.
    let mut e = good.clone();
    patch(&mut e, ph_load + 4, &4u32.to_le_bytes());
    check(&e, PvhError::EntryNotExecutable { entry: 0x10_0000 });
    // Duplicate PVH note: make the LOAD header a second copy of the note
    // header's note range (a PT_NOTE twice).
    let mut e = good.clone();
    let note_hdr = e[ph_note..ph_note + 56].to_vec();
    patch(&mut e, ph_load, &note_hdr);
    check(&e, PvhError::DuplicatePvhNote);
    // 8-byte descriptor above 4 GiB.
    let mut note = Vec::new();
    for v in [4u32, 8, XEN_ELFNOTE_PHYS32_ENTRY] {
        note.extend_from_slice(&v.to_le_bytes());
    }
    note.extend_from_slice(b"Xen\0");
    note.extend_from_slice(&(1u64 << 32).to_le_bytes());
    let mut e = good.clone();
    let start = e.len();
    e.extend_from_slice(&note);
    patch(&mut e, ph_note + 8, &(start as u64).to_le_bytes());
    patch(&mut e, ph_note + 32, &(note.len() as u64).to_le_bytes());
    check(&e, PvhError::EntryAbove4G);
    // Command line limits.
    let mut m2 = mem();
    let img = parse_pvh_elf(&good, &m2).unwrap();
    assert_eq!(
        load_pvh(&img, &good, &mut m2, &"x".repeat(5000)).unwrap_err(),
        PvhError::CmdlineTooLong
    );
    assert_eq!(
        load_pvh(&img, &good, &mut m2, "a\0b").unwrap_err(),
        PvhError::CmdlineNul
    );
    // RAM that does not cover the boot structures.
    let high = GuestMemory::new(&[(0x10_0000, 1 << 20)]).unwrap();
    let mut high2 = high.clone();
    let img = parse_pvh_elf(&good, &high).unwrap();
    assert_eq!(
        load_pvh(&img, &good, &mut high2, "").unwrap_err(),
        PvhError::BootDataOutsideRam
    );
}

#[test]
fn pvh_never_panics_on_truncations_or_byte_flips() {
    let m = mem();
    let good = build_test_pvh_elf(0x10_0000, &CODE);
    for n in 0..good.len() {
        let _ = parse_pvh_elf(&good[..n], &m);
    }
    for i in 0..good.len() {
        for bit in 0..8 {
            let mut e = good.clone();
            e[i] ^= 1 << bit;
            if let Ok(img) = parse_pvh_elf(&e, &m) {
                let mut mm = mem();
                let _ = load_pvh(&img, &e, &mut mm, "");
            }
        }
    }
}

#[test]
fn guest_memory_layout_is_validated() {
    assert_eq!(GuestMemory::new(&[]).unwrap_err(), MemError::NoRegions);
    assert_eq!(
        GuestMemory::new(&[(0, 0)]).unwrap_err(),
        MemError::EmptyRegion
    );
    assert_eq!(
        GuestMemory::new(&[(1, 4096)]).unwrap_err(),
        MemError::Unaligned
    );
    assert_eq!(
        GuestMemory::new(&[(0, 8192), (4096, 4096)]).unwrap_err(),
        MemError::Overlap
    );
    assert_eq!(
        GuestMemory::new(&[(u64::MAX - 4095, 8192)]).unwrap_err(),
        MemError::Overflow
    );
    assert_eq!(
        GuestMemory::new(&[(0, 2 << 30)]).unwrap_err(),
        MemError::TooLarge
    );
    let mut m = GuestMemory::new(&[(0, 4096), (8192, 4096)]).unwrap();
    // An access spanning the hole between regions is refused whole.
    assert!(matches!(
        m.write(4000, &[0; 200]),
        Err(MemError::OutOfBounds { .. })
    ));
    assert!(matches!(
        m.read(u64::MAX - 2, &mut [0; 8]),
        Err(MemError::OutOfBounds { .. })
    ));
    m.write(8192, &[9; 16]).unwrap();
    assert_eq!(m.read_u64(8192).unwrap(), 0x0909_0909_0909_0909);
}

#[test]
fn vmgenid_publishes_before_notifying_and_never_reuses() {
    let mut m = mem();
    assert_eq!(
        VmGenIdDevice::new(0x7004, 5).unwrap_err(),
        VmGenIdError::BadAddress
    );
    assert_eq!(
        VmGenIdDevice::new(0x7ff8, 5).unwrap_err(),
        VmGenIdError::BadAddress
    );
    let mut d = VmGenIdDevice::new(0x9000, 5).unwrap();
    assert_eq!(d.acpi_addr_package(), [0x9000, 0]);
    let node = d.device_tree_node();
    assert_eq!(
        (
            node.compatible,
            node.reg_address,
            node.reg_size,
            node.interrupt
        ),
        ("microsoft,vmgenid", 0x9000, 16, 5)
    );
    let g1 = VmGenId { lo: 1, hi: 77 };
    d.boot(&mut m, g1).unwrap();
    assert!(!d.take_notification(), "boot does not notify");
    assert_eq!(d.read_guest(&m).unwrap(), VmGenIdDevice::encode(g1));
    let g2 = VmGenId { lo: 2, hi: 77 };
    d.regenerate(&mut m, g2).unwrap();
    // The guest's notification handler reads the new id.
    assert!(d.take_notification());
    assert_eq!(d.read_guest(&m).unwrap(), VmGenIdDevice::encode(g2));
    assert_eq!(d.regenerate(&mut m, g1).unwrap_err(), VmGenIdError::Reused);
    assert_eq!(
        d.regenerate(&mut m, VmGenId { lo: 0, hi: 0 }).unwrap_err(),
        VmGenIdError::ZeroId
    );
    assert_eq!(d.current(), Some(g2));
    assert_eq!(d.notifications(), 1);
    let mut far = VmGenIdDevice::new(0x4000_0000, 5).unwrap();
    assert!(matches!(far.boot(&mut m, g1), Err(VmGenIdError::Memory(_))));
    let wire = VmGenIdDevice::encode(VmGenId {
        lo: 0x0102_0304_0506_0708,
        hi: 0x1112_1314_1516_1718,
    });
    assert_eq!(wire[0], 0x08);
    assert_eq!(wire[15], 0x11);
}
