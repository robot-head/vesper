//! Freestanding images (wave seven lane 27): object and image checks on
//! every host; QEMU boots and the `ld.lld` cross-link when those binaries
//! exist (set `VESPER_REQUIRE_QEMU=1` to make a missing QEMU a failure).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use vesper::native::freestanding::{
    self as fs, FreestandingError, FreestandingImage, FreestandingTarget, ImageOptions, elf,
};
use vesper::{check, parser, runtime::Runtime};

const CHECKED_SUM: &str = include_str!("fixtures/freestanding/checked_sum.ves");
const ABORT: &str = include_str!("fixtures/freestanding/abort.ves");

/// Programs that run to completion, with what they compute.
const PROGRAMS: &[(&str, &str)] = &[
    ("scalar", "fn main() -> i64 { 2 + 3 * 4 }"),
    (
        "negative",
        "fn main() -> i64 { 0 - 9223372036854775807 - 1 }",
    ),
    ("bool", "fn main() -> bool { 3 > 2 }"),
    ("unit", "fn main() -> Unit { () }"),
    ("u8", "fn main() -> u8 { 255 }"),
    (
        "inout",
        "fn inc(inout x: i64) { x = wrapping_add(x, 1); } fn main() -> i64 { let x = 41; inc(inout x); x }",
    ),
    (
        "handler",
        "linear effect Ask { fn ask() -> i64; } fn ask_twice() -> i64 eff [Ask] { wrapping_add(Ask::ask(), Ask::ask()) } fn main() -> i64 { handle Ask in { ask_twice() } with { return x => x, ask(k) => k(21), } }",
    ),
    (
        "enum",
        "fn make(x: i64) -> Option[i64] { if x > 0 { Some(x) } else { None } } fn main() -> i64 { match make(42) { Some(v) => v, None => 0, } }",
    ),
    (
        "division",
        "fn divide(b: i64) -> i64 eff [Fail[RuntimeError]] { 84 / b } fn main() -> i64 { handle Fail[RuntimeError] in { divide(0) } with { return x => x, raise(e, k) => match e { RuntimeError::DivisionByZero => 42, RuntimeError::IndexOutOfBounds => 0, }, } }",
    ),
    ("checked_sum", CHECKED_SUM),
];

fn world(source: &str) -> check::World {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    let c = check::check(&p.module);
    assert!(c.diagnostics.is_empty(), "{:?}", c.diagnostics);
    c.world
}

fn build(source: &str, target: FreestandingTarget) -> Result<FreestandingImage, FreestandingError> {
    fs::build_image(&world(source), "main", &ImageOptions::new(target))
}

fn word(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}

/// Bytes at a virtual address of a linked image.
fn at_address(image: &FreestandingImage, address: u64, len: usize) -> Vec<u8> {
    let seg = image
        .layout
        .segments
        .iter()
        .find(|s| address >= s.address && address + len as u64 <= s.address + s.file_bytes)
        .expect("address in a loaded segment");
    let off = (seg.file_offset + address - seg.address) as usize;
    image.elf[off..off + len].to_vec()
}

#[test]
fn targets_parse_and_reject_hosted_triples() {
    for t in FreestandingTarget::ALL {
        assert_eq!(FreestandingTarget::parse(t.triple()).unwrap(), t);
    }
    for bad in [
        "x86_64-unknown-linux-gnu",
        "riscv64gc-unknown-linux-gnu",
        "aarch64-apple-darwin",
        "thumbv7em-none-eabihf",
    ] {
        assert!(matches!(
            FreestandingTarget::parse(bad),
            Err(FreestandingError::UnsupportedTarget(_))
        ));
    }
}

#[test]
fn images_link_for_both_targets() {
    for (name, source) in PROGRAMS {
        for target in FreestandingTarget::ALL {
            let image = build(source, target).unwrap_or_else(|e| panic!("{name} on {target}: {e}"));
            let info = elf::read_executable(&image.elf).unwrap();
            assert_eq!(info.e_type, 2, "ET_EXEC");
            let machine = match target {
                FreestandingTarget::Riscv64Imac => elf::EM_RISCV,
                FreestandingTarget::Aarch64 => elf::EM_AARCH64,
            };
            assert_eq!(info.machine, machine);
            assert_eq!(info.entry, image.board.image_base);
            assert_eq!(image.entry_address, image.board.image_base);
            assert_eq!(
                image.symbol("_start").unwrap().address,
                image.board.image_base
            );
            // Every loaded byte sits inside RAM, and the stack is last.
            let ram_end = image.board.ram_base + image.board.ram_bytes;
            for (addr, mem, _) in &info.segment_memory {
                assert!(*addr >= image.board.ram_base && addr + mem <= ram_end);
            }
            assert_eq!(
                image.layout.stack_top - image.layout.stack_bottom,
                16 * 1024
            );
            assert_eq!(
                image.symbol("__stack_top").unwrap().address,
                image.layout.stack_top
            );
            // Inputs are relocatable objects for the same machine.
            for (_, object) in &image.objects {
                assert_eq!(elf::e_machine(object).unwrap(), machine);
                assert_eq!(u16::from_le_bytes([object[16], object[17]]), 1, "ET_REL");
            }
            // No imports remain: no libc, allocator or Pulse symbol is named.
            for s in &info.symbols {
                assert!(
                    !s.0.starts_with("vesper_fiber") && s.0 != "malloc",
                    "{}",
                    s.0
                );
            }
        }
    }
}

#[test]
fn entry_stub_masks_interrupts_first() {
    // csrci mstatus, 8 (mstatus.MIE) and msr daifset, #0xf (DAIF.I among them),
    // the bits Silicon's interrupt model names.
    assert_eq!(fs::stub::riscv_mie_mask(), 1 << 3);
    assert_eq!(fs::stub::aarch64_daifset_mask() & 0b0010, 0b0010);
    let rv = build(PROGRAMS[0].1, FreestandingTarget::Riscv64Imac).unwrap();
    let first = word(&at_address(&rv, rv.entry_address, 4), 0);
    assert_eq!(first, 0x300 << 20 | 8 << 15 | 7 << 12 | 0x73, "{first:#x}");
    let arm = build(PROGRAMS[0].1, FreestandingTarget::Aarch64).unwrap();
    let first = word(&at_address(&arm, arm.entry_address, 4), 0);
    assert_eq!(first, 0xd503_4fdf, "{first:#x}");
    // VBAR_EL1 needs a 2 KiB aligned table, and the stub provides it.
    let text = &arm.layout.sections[0];
    assert!(text.2 >= 0x800 + 16 * 0x80);
}

#[test]
fn stack_bounds_come_from_silicon() {
    for target in FreestandingTarget::ALL {
        let image = build(CHECKED_SUM, target).unwrap();
        assert_eq!(image.stack.len(), 2);
        let main = &image.stack[0];
        assert_eq!(main.root, "vesper_freestanding_main");
        assert!(main.worst_case_bytes > 0 && main.worst_case_bytes <= u64::from(main.budget_bytes));
        assert!(
            main.worst_path.iter().any(|f| f == "main"),
            "{:?}",
            main.worst_path
        );
        assert_eq!(image.stack[1].root, "vesper_freestanding_trap");
        // Every user function has a measured frame (16-byte multiples).
        for f in ["main", "vesper_freestanding_main", "vesper_rt_putc"] {
            let bytes = image.frames[f];
            assert!(bytes > 0 && bytes.is_multiple_of(16), "{f}: {bytes}");
        }
        // The worst path is the sum of its frames (the exit stub adds 16
        // bytes on AArch64 semihosting, 0 on RISC-V).
        let sum: u64 = main
            .worst_path
            .iter()
            .map(|f| u64::from(*image.frames.get(f).unwrap_or(&0)))
            .sum();
        let exit = u64::from(fs::stub::exit_stack_bytes(target, image.board.exit));
        assert!(sum == main.worst_case_bytes || sum + exit == main.worst_case_bytes);
    }
}

#[test]
fn small_stacks_and_recursion_are_rejected() {
    let mut options = ImageOptions::new(FreestandingTarget::Riscv64Imac);
    options.stack_bytes = 32;
    match fs::build_image(&world(CHECKED_SUM), "main", &options) {
        Err(FreestandingError::Stack(findings)) => {
            assert!(
                findings
                    .iter()
                    .any(|f| f.rule == "silicon.stack.budget_exceeded"),
                "{findings:?}"
            )
        }
        other => panic!("{other:?}"),
    }
    let recursive = "fn down(n: i64) -> i64 { if n == 0 { 0 } else { down(wrapping_sub(n, 1)) } } fn main() -> i64 { down(10) }";
    for target in FreestandingTarget::ALL {
        match build(recursive, target) {
            Err(FreestandingError::Stack(findings)) => {
                assert!(
                    findings.iter().any(|f| f.rule == "silicon.stack.recursion"),
                    "{findings:?}"
                )
            }
            other => panic!("{other:?}"),
        }
    }
}

#[test]
fn unsupported_entries_are_typed_errors() {
    let cases = [
        ("fn main(x: i64) -> i64 { x }", "parameters"),
        ("fn main() -> f64 { 1.5 }", "result type"),
        (
            "copy struct Pair { x: i64, y: i64 } fn main() -> Pair { Pair { x: 1, y: 2 } }",
            "result type",
        ),
        (
            "linear effect Ask { fn ask() -> i64; } fn main() -> i64 eff [Ask] { Ask::ask() }",
            "other than Fail",
        ),
    ];
    for (source, needle) in cases {
        for target in FreestandingTarget::ALL {
            match build(source, target) {
                Err(FreestandingError::UnsupportedEntry(m)) => assert!(m.contains(needle), "{m}"),
                other => panic!("{source}: {other:?}"),
            }
        }
    }
    assert!(matches!(
        fs::build_image(
            &world(PROGRAMS[0].1),
            "missing",
            &ImageOptions::new(FreestandingTarget::Aarch64)
        ),
        Err(FreestandingError::UnsupportedEntry(_))
    ));
}

#[test]
fn floating_point_is_rejected_on_riscv64imac() {
    let float = "fn half(x: f64) -> f64 { x / 2.0 } fn main() -> bool { half(3.0) > 1.0 }";
    match build(float, FreestandingTarget::Riscv64Imac) {
        Err(FreestandingError::UnsupportedOperation { what, .. }) => {
            assert!(what.contains("floating point"), "{what}")
        }
        other => panic!("{other:?}"),
    }
    // AArch64 has FP/SIMD (the stub enables it through CPACR_EL1).
    build(float, FreestandingTarget::Aarch64).unwrap();
    // `%` on f64 lowers to a call to libm's `fmod`: there is no libm.
    let fmod = "fn rem(a: f64, b: f64) -> f64 { a % b } fn main() -> bool { rem(7.0, 2.0) > 0.5 }";
    match build(fmod, FreestandingTarget::Aarch64) {
        Err(FreestandingError::Stack(findings)) => assert!(
            findings
                .iter()
                .any(|f| f.rule == "silicon.stack.unbounded_external_call"),
            "{findings:?}"
        ),
        Err(FreestandingError::UnresolvedSymbol { symbol, .. }) => assert_eq!(symbol, "fmod"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn unhandled_failures_become_abort_sites() {
    for target in FreestandingTarget::ALL {
        let image = build(ABORT, target).unwrap();
        let aborts: Vec<_> = image
            .traps
            .iter()
            .filter(|t| t.function == "vesper_freestanding_main")
            .collect();
        assert_eq!(aborts.len(), 2, "{:?}", image.traps);
        assert_eq!(aborts[0].message, "unhandled Fail[RuntimeError]");
        assert_eq!(aborts[1].message, "unhandled Fail");
        // The linked table holds every site's PC, inside its function.
        let table = image.symbol("vesper_trap_table").unwrap().address;
        let count = u64::from_le_bytes(at_address(&image, table, 8).try_into().unwrap());
        assert_eq!(count, image.traps.len() as u64);
        for (i, site) in image.traps.iter().enumerate() {
            let pc = u64::from_le_bytes(
                at_address(&image, table + 8 + 24 * i as u64, 8)
                    .try_into()
                    .unwrap(),
            );
            assert_eq!(pc, site.pc);
            let f = image
                .symbols
                .iter()
                .find(|s| s.name == site.function && s.size > 0)
                .unwrap();
            assert!(pc >= f.address && pc < f.address + f.size);
            let msg_ptr = u64::from_le_bytes(
                at_address(&image, table + 16 + 24 * i as u64, 8)
                    .try_into()
                    .unwrap(),
            );
            let len = u64::from_le_bytes(
                at_address(&image, table + 24 + 24 * i as u64, 8)
                    .try_into()
                    .unwrap(),
            );
            assert_eq!(
                at_address(&image, msg_ptr, len as usize),
                site.message.as_bytes()
            );
        }
    }
}

#[test]
fn linker_script_names_every_imported_symbol() {
    for target in FreestandingTarget::ALL {
        let image = build(PROGRAMS[0].1, target).unwrap();
        let script = &image.linker_script;
        for needle in [
            "ENTRY(_start)",
            "KEEP(*(.text.boot))",
            "__bss_start = .;",
            "__bss_end = .;",
            "__stack_top = .;",
            "/DISCARD/",
        ] {
            assert!(script.contains(needle), "{needle}");
        }
        assert!(script.contains(&format!(". = {:#x};", image.board.image_base)));
    }
}

// --- gated: QEMU and ld.lld ------------------------------------------------

fn which(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(program))
        .find(|p| p.is_file())
}

fn qemu(target: FreestandingTarget) -> Option<PathBuf> {
    let (program, _) = fs::qemu_command(target, "");
    let found = which(program);
    if found.is_none() {
        assert!(
            std::env::var_os("VESPER_REQUIRE_QEMU").is_none(),
            "{program} is required but was not found"
        );
        eprintln!("{program} not found: QEMU boot not exercised for {target}");
    }
    found
}

fn scratch(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("vesper-freestanding-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Boot `elf` and return `(UART output, exit status)`.
fn boot(qemu: &Path, target: FreestandingTarget, elf: &Path) -> (String, i32) {
    let (_, args) = fs::qemu_command(target, elf.to_str().unwrap());
    let mut child = Command::new(qemu)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            let out = child.wait_with_output().unwrap();
            let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
            assert!(status.code().is_some(), "QEMU killed: {stdout}");
            return (stdout, status.code().unwrap());
        }
        if Instant::now() > deadline {
            child.kill().unwrap();
            let out = child.wait_with_output().unwrap();
            panic!("QEMU timed out: {}", String::from_utf8_lossy(&out.stdout));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn qemu_boots_match_the_evaluator() {
    for target in FreestandingTarget::ALL {
        let Some(qemu) = qemu(target) else { continue };
        let dir = scratch(target.triple());
        for (name, source) in PROGRAMS {
            let w = world(source);
            let expected = Runtime::new(&w).run("main", vec![]).unwrap().to_string();
            let image = fs::build_image(&w, "main", &ImageOptions::new(target)).unwrap();
            let path = dir.join(format!("{name}.elf"));
            std::fs::write(&path, &image.elf).unwrap();
            let (out, status) = boot(&qemu, target, &path);
            assert_eq!(
                out,
                format!("vesper: main() = {expected}\n"),
                "{name} on {target}"
            );
            assert_eq!(status, 0, "{name} on {target}: {out}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn qemu_abort_goes_through_the_trap_handler() {
    for target in FreestandingTarget::ALL {
        let Some(qemu) = qemu(target) else { continue };
        let dir = scratch(&format!("abort-{}", target.triple()));
        let image = build(ABORT, target).unwrap();
        let path = dir.join("abort.elf");
        std::fs::write(&path, &image.elf).unwrap();
        let (out, status) = boot(&qemu, target, &path);
        let site = &image
            .traps
            .iter()
            .find(|t| t.message == "unhandled Fail[RuntimeError]")
            .unwrap();
        assert!(
            out.starts_with(&format!(
                "vesper: abort: unhandled Fail[RuntimeError] pc=0x{:016x} cause=0x",
                site.pc
            )),
            "{out}"
        );
        // Illegal instruction (RISC-V mcause 2) / unknown reason (ESR.EC 0).
        let cause = match target {
            FreestandingTarget::Riscv64Imac => "cause=0x0000000000000002",
            FreestandingTarget::Aarch64 => "cause=0x0000000002000000",
        };
        assert!(out.contains(cause), "{out}");
        assert_eq!(status, fs::EXIT_ABORT as i32);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn ld_lld_links_the_same_objects_with_the_script() {
    let Some(lld) = which("ld.lld") else {
        eprintln!("ld.lld not found: linker-script cross-link not exercised");
        return;
    };
    for target in FreestandingTarget::ALL {
        let dir = scratch(&format!("lld-{}", target.triple()));
        let image = build(CHECKED_SUM, target).unwrap();
        let linked = fs::link_with_external(&image, &lld, &dir).unwrap();
        let info = elf::read_executable(&linked).unwrap();
        assert_eq!(info.entry, image.board.image_base);
        // Same symbols at the same addresses as the built-in linker.
        for name in [
            "_start",
            "vesper_freestanding_main",
            "vesper_freestanding_trap",
            "main",
            "__stack_top",
        ] {
            let ours = image.symbol(name).unwrap().address;
            let theirs = info.symbols.iter().find(|(n, _)| n == name).unwrap().1;
            assert_eq!(ours, theirs, "{name} on {target}");
        }
        if let Some(qemu) = qemu(target) {
            let w = world(CHECKED_SUM);
            let expected = Runtime::new(&w).run("main", vec![]).unwrap().to_string();
            let (out, status) = boot(&qemu, target, &dir.join("image.lld.elf"));
            assert_eq!(out, format!("vesper: main() = {expected}\n"));
            assert_eq!(status, 0);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
