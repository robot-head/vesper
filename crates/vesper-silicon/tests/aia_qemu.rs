//! Cross-check of the AIA model against QEMU's APLIC and IMSIC.
//!
//! The same register scripts run on the model ([`RiscvIntc`] built from the
//! QEMU 8.2.2 `virt` dumps) and, when `qemu-system-riscv64` is installed,
//! on QEMU itself: `fixtures/qemu/aia-probe.elf` (built from
//! `fixtures/qemu/aia-probe.S`) is a bare-metal M-mode interpreter that
//! executes the script QEMU's loader places at 0x8010_0000 and prints one
//! hex line per read. The NS16550's transmit-empty interrupt (IER bit 1)
//! serves as a device line the script can raise and drop.
//!
//! The model follows the AIA 1.0 specification; QEMU 8.2.2 departs from it
//! in a few places, so those cases are left out of the compared scripts and
//! checked separately ([`qemu_8_2_departures_from_the_specification`]).
//!
//! Without QEMU the model side still runs against the recorded QEMU output
//! below; `VESPER_REQUIRE_QEMU=1` makes a missing QEMU a failure.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use vesper_silicon::dtb::Platform;
use vesper_silicon::intc::{MEIP, Privilege, RiscvIntc, SEIP};
use vesper_silicon::ledger::Ledger;

const PROBE: &[u8] = include_bytes!("../fixtures/qemu/aia-probe.elf");
const APLIC_DTB: &[u8] = include_bytes!("../fixtures/dtb/qemu-riscv64-virt-aplic.dtb");
const IMSIC_DTB: &[u8] = include_bytes!("../fixtures/dtb/qemu-riscv64-virt-aplic-imsic.dtb");

const M: u64 = 0x0c00_0000;
const S: u64 = 0x0d00_0000;
const IDC: u64 = M + 0x4000;
const M_FILE: u64 = 0x2400_0000;
const UART: &str = "/soc/serial@10000000";

#[derive(Clone, Copy, Debug)]
enum Op {
    W(u64, u32),
    R(u64),
    /// NS16550 IER write: bit 1 raises the UART's line (its transmitter is
    /// always empty in the probe).
    Ier(u8),
    MiW(u32, u64),
    MiR(u32),
    Mtopei,
    Mclaim,
    SiW(u32, u64),
    SiR(u32),
    Stopei,
    Sclaim,
    /// `mip & (MEIP | SEIP)`.
    Mip,
}

use Op::*;

impl Op {
    fn encode(self) -> [u64; 3] {
        match self {
            W(a, v) => [1, a, u64::from(v)],
            R(a) => [2, a, 0],
            Ier(v) => [4, 0x1000_0001, u64::from(v)],
            MiW(i, v) => [6, u64::from(i), v],
            MiR(i) => [7, u64::from(i), 0],
            Mtopei => [8, 0, 0],
            Mclaim => [9, 0, 0],
            SiW(i, v) => [10, u64::from(i), v],
            SiR(i) => [11, u64::from(i), 0],
            Stopei => [12, 0, 0],
            Sclaim => [13, 0, 0],
            Mip => [14, 0, 0],
        }
    }
}

/// Runs `ops` on the model of the machine `dtb` describes, M-mode hart 0.
fn model(dtb: &[u8], ops: &[Op]) -> Vec<u64> {
    let p = Platform::from_dtb(dtb).unwrap();
    let ledger = Ledger::new();
    let mut intc = RiscvIntc::new(&p, Privilege::Machine, &ledger).unwrap();
    let mut out = Vec::new();
    for &op in ops {
        match op {
            W(a, v) => intc.write32(a, v).unwrap(),
            R(a) => out.push(u64::from(intc.read32(a).unwrap())),
            Ier(v) => {
                intc.set_device_line(&p, UART, v & 2 != 0).unwrap();
            }
            MiW(i, v) => intc.imsic_csr_write(0, Privilege::Machine, i, v).unwrap(),
            MiR(i) => out.push(intc.imsic_csr_read(0, Privilege::Machine, i).unwrap()),
            Mtopei => out.push(u64::from(intc.topei(0, Privilege::Machine).unwrap())),
            Mclaim => out.push(u64::from(intc.claim_topei(0, Privilege::Machine).unwrap())),
            SiW(i, v) => intc
                .imsic_csr_write(0, Privilege::Supervisor, i, v)
                .unwrap(),
            SiR(i) => out.push(intc.imsic_csr_read(0, Privilege::Supervisor, i).unwrap()),
            Stopei => out.push(u64::from(intc.topei(0, Privilege::Supervisor).unwrap())),
            Sclaim => out.push(u64::from(
                intc.claim_topei(0, Privilege::Supervisor).unwrap(),
            )),
            Mip => out.push(
                (u64::from(intc.external_level(0, MEIP)) << 11)
                    | (u64::from(intc.external_level(0, SEIP)) << 9),
            ),
        }
    }
    out
}

fn qemu_binary() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("VESPER_QEMU_RISCV64") {
        return Some(PathBuf::from(p));
    }
    let found = std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|d| d.join("qemu-system-riscv64"))
            .find(|p| p.is_file())
    });
    if found.is_none() && std::env::var("VESPER_REQUIRE_QEMU").as_deref() == Ok("1") {
        panic!("VESPER_REQUIRE_QEMU=1 but qemu-system-riscv64 is not on PATH");
    }
    found
}

fn qemu_version(bin: &PathBuf) -> String {
    Command::new(bin)
        .arg("--version")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

/// Distinguishes the scratch directories of concurrent QEMU runs.
static RUN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Runs `ops` on QEMU `virt` with `aia` (`aplic` or `aplic-imsic`).
fn qemu(bin: &PathBuf, aia: &str, ops: &[Op]) -> Vec<u64> {
    let dir = std::env::temp_dir().join(format!(
        "vesper-aia-{}-{}",
        std::process::id(),
        RUN.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let elf = dir.join("aia-probe.elf");
    let script = dir.join("script.bin");
    std::fs::write(&elf, PROBE).unwrap();
    let mut bytes = Vec::new();
    for op in ops {
        for w in op.encode() {
            bytes.extend_from_slice(&w.to_le_bytes());
        }
    }
    bytes.extend_from_slice(&[0; 24]);
    std::fs::write(&script, bytes).unwrap();
    let mut child = Command::new(bin)
        .args([
            "-machine",
            &format!("virt,aia={aia}"),
            "-smp",
            "2",
            "-m",
            "256M",
            "-bios",
            "none",
            "-kernel",
        ])
        .arg(&elf)
        .arg("-device")
        .arg(format!("loader,file={},addr=0x80100000", script.display()))
        .args(["-display", "none", "-monitor", "none", "-serial", "stdio"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start qemu-system-riscv64");
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("QEMU did not power off within 60 s");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let out = child.wait_with_output().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let text = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = text.lines().map(str::trim).collect();
    assert!(
        lines.contains(&"E"),
        "probe did not finish: {text:?} / {}",
        String::from_utf8_lossy(&out.stderr)
    );
    lines
        .iter()
        .take_while(|l| **l != "E")
        .map(|l| u64::from_str_radix(l, 16).expect("hex line"))
        .collect()
}

/// M-level MSI delivery: IMSIC file setup, APLIC MSI mode, a level and an
/// edge source, setipnum re-triggering, direct MSIs and the threshold.
fn msi_script() -> Vec<Op> {
    vec![
        MiW(0x70, 1),
        MiR(0x70),
        MiW(0x72, 0),
        MiW(0xc0, (1 << 5) | (1 << 7)),
        MiR(0xc0),
        W(M, 0x104),
        R(M),
        W(M + 0x1bc0, 0x24000),
        W(M + 0x1bc4, 0),
        R(M + 0x1bc0),
        R(M + 0x1bc4),
        W(M + 0x28, 6),
        R(M + 0x28),
        W(M + 0x3028, 5),
        R(M + 0x3028),
        W(M + 0x1edc, 10),
        R(M + 0x1e00),
        Mip,
        Mtopei,
        Ier(2),
        R(M + 0x1c00),
        Mip,
        Mtopei,
        Mclaim,
        Mtopei,
        Mip,
        W(M + 0x1cdc, 10),
        Mtopei,
        Mclaim,
        Ier(0),
        R(M + 0x1c00),
        R(M + 0x1d00),
        W(M + 0x28, 4),
        Ier(2),
        Mtopei,
        Mclaim,
        Ier(0),
        Ier(2),
        Mtopei,
        Mclaim,
        Ier(0),
        MiW(0xc0, 1 << 7),
        W(M_FILE, 7),
        W(M_FILE, 5),
        Mtopei,
        MiR(0x80),
        MiW(0x72, 7),
        Mtopei,
        Mip,
        MiW(0x72, 0),
        Mtopei,
        Mclaim,
        MiR(0x80),
    ]
}

/// The same QEMU output, recorded on QEMU 8.2.2 (Ubuntu 1:8.2.2+ds-0ubuntu1.18).
const MSI_EXPECTED: &[u64] = &[
    1,
    0xa0,
    0x8000_0104,
    0x24000,
    0,
    6,
    5,
    0x400,
    0,
    0,
    0,
    0x800,
    0x50005,
    0x50005,
    0,
    0,
    0x50005,
    0x50005,
    0,
    0,
    0x50005,
    0x50005,
    0x50005,
    0x50005,
    0x70007,
    0xa0,
    0,
    0,
    0x70007,
    0x70007,
    0x20,
];

/// Direct delivery through IDC 0: priorities, threshold, claims, edge and
/// detached modes, iforce, inactive sources and read-only DM.
fn direct_script() -> Vec<Op> {
    let idc = IDC;
    vec![
        W(M, 0x104),
        R(M),
        W(idc, 1),
        W(idc + 8, 0),
        R(idc),
        R(idc + 8),
        W(M + 0x28, 6),
        W(M + 0x3028, 0),
        R(M + 0x3028),
        W(M + 0x3028, 3),
        R(M + 0x3028),
        W(M + 0x1edc, 10),
        R(idc + 0x18),
        Mip,
        Ier(2),
        R(M + 0x1c00),
        R(idc + 0x18),
        Mip,
        R(idc + 0x1c),
        R(idc + 0x18),
        R(M + 0x1c00),
        W(idc + 8, 3),
        R(idc + 0x18),
        Mip,
        W(idc + 8, 4),
        R(idc + 0x18),
        Ier(0),
        W(M + 0x28, 4),
        W(M + 0x1ddc, 10),
        R(M + 0x1c00),
        Ier(2),
        R(M + 0x1c00),
        R(idc + 0x1c),
        R(M + 0x1c00),
        Ier(0),
        R(M + 0x1c00),
        W(M + 0x1cdc, 10),
        R(M + 0x1c00),
        W(M + 0x1ddc, 10),
        R(M + 0x1c00),
        W(idc + 4, 1),
        R(idc + 0x18),
        Mip,
        R(idc + 0x1c),
        R(idc + 4),
        W(M + 0x28, 1),
        W(M + 0x1cdc, 10),
        R(M + 0x1c00),
        Ier(2),
        R(M + 0x1c00),
        Ier(0),
        W(M + 0x28, 0),
        R(M + 0x1c00),
        R(M + 0x1e00),
        R(M + 0x28),
        W(M + 0x28, 0x400),
        R(M + 0x28),
        W(M, 0x4),
        R(M),
        R(M + 0x1bc0),
    ]
}

const DIRECT_EXPECTED: &[u64] = &[
    0x8000_0100,
    1,
    0,
    1,
    3,
    0,
    0,
    0x400,
    0xa0003,
    0x800,
    0xa0003,
    0xa0003,
    0x400,
    0,
    0,
    0xa0003,
    0,
    0x400,
    0xa0003,
    0,
    0,
    0x400,
    0,
    0,
    0x800,
    0,
    0,
    0x400,
    0x400,
    0,
    0,
    0,
    0x400,
    0x8000_0000,
    0,
];

/// S-level delivery: the root's smsiaddrcfg, delegation to the S domain,
/// the S-level IMSIC file, and the lock.
fn supervisor_script() -> Vec<Op> {
    vec![
        W(M + 0x1bc0, 0x24000),
        W(M + 0x1bc4, 0),
        W(M + 0x1bc8, 0x28000),
        W(M + 0x1bcc, 0),
        R(M + 0x1bc8),
        R(M + 0x1bcc),
        W(M + 0x28, 0x400),
        R(M + 0x28),
        R(S + 0x28),
        R(S + 0x1bc0),
        R(S + 0x1bc8),
        R(S),
        W(S, 0x104),
        W(S + 0x28, 6),
        R(S + 0x28),
        W(S + 0x3028, 9),
        R(S + 0x3028),
        W(S + 0x1edc, 10),
        R(S + 0x1e00),
        SiW(0x70, 1),
        SiW(0xc0, 1 << 9),
        SiR(0x70),
        SiR(0xc0),
        Stopei,
        Ier(2),
        Stopei,
        Mip,
        Sclaim,
        Mip,
        Ier(0),
        W(M + 0x28, 0),
        R(M + 0x28),
        W(M + 0x1bc4, 0x8000_0000),
        W(M + 0x1bc0, 0x1),
        R(M + 0x1bc0),
        R(M + 0x1bc4),
        R(M + 0x1bc8),
    ]
}

const SUPERVISOR_EXPECTED: &[u64] = &[
    0x28000,
    0,
    0x400,
    0,
    0,
    0,
    0x8000_0004,
    6,
    9,
    0x400,
    1,
    0x200,
    0,
    0x90009,
    0x200,
    0x90009,
    0,
    0,
    0x24000,
    0x8000_0000,
    0x28000,
];

fn check(name: &str, dtb: &[u8], aia: &str, ops: &[Op], recorded: &[u64]) {
    let m = model(dtb, ops);
    assert_eq!(m, recorded, "{name}: model vs recorded QEMU 8.2.2 output");
    if let Some(bin) = qemu_binary() {
        let q = qemu(&bin, aia, ops);
        assert_eq!(m, q, "{name}: model vs live {}", qemu_version(&bin).trim());
    }
}

#[test]
fn msi_mode_matches_qemu() {
    check("msi", IMSIC_DTB, "aplic-imsic", &msi_script(), MSI_EXPECTED);
}

#[test]
fn direct_mode_matches_qemu() {
    check(
        "direct",
        APLIC_DTB,
        "aplic",
        &direct_script(),
        DIRECT_EXPECTED,
    );
}

#[test]
fn supervisor_level_matches_qemu() {
    check(
        "supervisor",
        IMSIC_DTB,
        "aplic-imsic",
        &supervisor_script(),
        SUPERVISOR_EXPECTED,
    );
}

/// Where QEMU 8.2.2 and the AIA 1.0 specification disagree, the model
/// follows the specification. Each case: the script, the value the
/// specification requires for the last read (the model's), and QEMU
/// 8.2.2's.
#[test]
fn qemu_8_2_departures_from_the_specification() {
    let msi_setup = vec![
        MiW(0x70, 1),
        MiW(0xc0, 1 << 5),
        W(M, 0x104),
        W(M + 0x1bc0, 0x24000),
        W(M + 0x28, 6),
        W(M + 0x3028, 5),
        W(M + 0x1edc, 10),
    ];
    let direct_setup = vec![
        W(M, 0x104),
        W(IDC, 1),
        W(M + 0x28, 6),
        W(M + 0x3028, 3),
        W(M + 0x1edc, 10),
    ];
    let with = |mut base: Vec<Op>, rest: &[Op]| {
        base.extend_from_slice(rest);
        base
    };
    type Case<'a> = (&'a str, &'a [u8], &'a str, Vec<Op>, u64, u64);
    let cases: Vec<Case> = vec![
        (
            // §4.7: in MSI mode a level source's pending bit may be set by
            // setipnum only while the rectified input is high.
            "setipnum with the level low",
            IMSIC_DTB,
            "aplic-imsic",
            with(
                msi_setup.clone(),
                &[Ier(2), Mclaim, Ier(0), W(M + 0x1cdc, 10), Mtopei],
            ),
            0,
            0x50005,
        ),
        (
            // §4.7: in direct mode a level source's pending bit is cleared
            // when the rectified input goes low.
            "direct level source after the input drops",
            APLIC_DTB,
            "aplic",
            with(direct_setup.clone(), &[Ier(2), Ier(0), R(IDC + 0x18)]),
            0,
            0xa0003,
        ),
        (
            // §4.8.1: a claimi that returns 0 clears iforce, which drops
            // the hart's interrupt.
            "claimi clearing iforce",
            APLIC_DTB,
            "aplic",
            with(direct_setup.clone(), &[W(IDC + 4, 1), R(IDC + 0x1c), Mip]),
            0,
            0x800,
        ),
        (
            // §4.5.2: a source not delegated to a domain is inactive there
            // and its sourcecfg reads zero.
            "child sourcecfg after undelegation",
            IMSIC_DTB,
            "aplic-imsic",
            vec![
                W(M + 0x28, 0x400),
                W(S + 0x28, 6),
                W(M + 0x28, 0),
                R(S + 0x28),
            ],
            0,
            6,
        ),
        (
            // A detached source's rectified input is zero (model choice;
            // QEMU reports the wire).
            "in_clrip of a detached source",
            APLIC_DTB,
            "aplic",
            with(
                direct_setup.clone(),
                &[W(M + 0x28, 1), Ier(2), R(M + 0x1d00)],
            ),
            0,
            0x400,
        ),
        (
            // Reserved source modes 2 and 3 (model choice: stored as
            // Inactive; QEMU keeps the value).
            "reserved source mode",
            APLIC_DTB,
            "aplic",
            vec![W(M + 0x28, 2), R(M + 0x28)],
            0,
            2,
        ),
    ];
    let live = qemu_binary().map(|b| {
        let v = qemu_version(&b);
        (b, v)
    });
    for (name, dtb, aia, ops, spec, qemu_8_2) in cases {
        let m = model(dtb, &ops);
        assert_eq!(*m.last().unwrap(), spec, "{name}: model");
        if let Some((bin, version)) = &live {
            let q = qemu(bin, aia, &ops);
            if version.contains("version 8.2.") {
                assert_eq!(*q.last().unwrap(), qemu_8_2, "{name}: QEMU 8.2");
            } else {
                eprintln!("{name}: {} gives {:#x}", version.trim(), q.last().unwrap());
            }
        }
    }
}
