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
//!
//! The hypervisor scenarios run `virt,aia=aplic-imsic,aia-guests=2 -smp 4`
//! (two guest interrupt files per hart; the default `rv64` CPU has the H
//! extension) and drive `hstatus.VGEIN`, `hgeie`, `hgeip`, `vsiselect` /
//! `vsireg`, `vstopei` and `mip.SGEIP/VSEIP`. An op that traps on QEMU
//! (illegal instruction, mcause 2) prints [`FAULT`]; the model prints the
//! same for a typed error. The PCI scenario adds QEMU's `edu` device
//! (00:01.0, requester ID 0x08), programs its MSI capability and makes it
//! raise MSIs; the model routes requester ID 0x08 through the host's
//! `msi-parent` and `dma-ranges` ([`RiscvIntc::pci_msi_write`]).

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use vesper_silicon::dtb::{Platform, requester_id};
use vesper_silicon::intc::{MEIP, Privilege, RiscvIntc, SEIP};
use vesper_silicon::ledger::Ledger;

#[path = "common/aia_hyp_expected.rs"]
mod aia_hyp_expected;
use aia_hyp_expected::{FAULT, HYPERVISOR_EXPECTED};

const PROBE: &[u8] = include_bytes!("../fixtures/qemu/aia-probe.elf");
const APLIC_DTB: &[u8] = include_bytes!("../fixtures/dtb/qemu-riscv64-virt-aplic.dtb");
const IMSIC_DTB: &[u8] = include_bytes!("../fixtures/dtb/qemu-riscv64-virt-aplic-imsic.dtb");
const GUESTS_DTB: &[u8] =
    include_bytes!("../fixtures/dtb/qemu-riscv64-virt-aplic-imsic-guests.dtb");

const M: u64 = 0x0c00_0000;
const S: u64 = 0x0d00_0000;
const IDC: u64 = M + 0x4000;
const M_FILE: u64 = 0x2400_0000;
const UART: &str = "/soc/serial@10000000";
const S_FILE: u64 = 0x2800_0000;
const PCI_HOST: &str = "/soc/pci@30000000";
/// ECAM configuration space of the `edu` device at 00:01.0.
const EDU_CFG: u64 = 0x3000_0000 + (1 << 15);
/// Where the probe places `edu`'s BAR0 (the 32-bit MMIO window).
const EDU_BAR: u64 = 0x4000_0000;

/// The machine a scenario runs on: `-machine`, `-smp`, extra devices and
/// the matching QEMU 8.2.2 devicetree dump.
#[derive(Clone, Copy)]
struct Machine {
    opts: &'static str,
    smp: &'static str,
    edu: bool,
    dtb: &'static [u8],
    /// QEMU's `aia-guests` (GEILEN) when the machine has guest files.
    geilen: Option<u32>,
}

const APLIC_M: Machine = Machine {
    opts: "virt,aia=aplic",
    smp: "2",
    edu: false,
    dtb: APLIC_DTB,
    geilen: None,
};
const IMSIC_M: Machine = Machine {
    opts: "virt,aia=aplic-imsic",
    smp: "2",
    edu: false,
    dtb: IMSIC_DTB,
    geilen: None,
};
const GUESTS_M: Machine = Machine {
    opts: "virt,aia=aplic-imsic,aia-guests=2",
    smp: "4",
    edu: false,
    dtb: GUESTS_DTB,
    geilen: Some(2),
};
const GUESTS_EDU_M: Machine = Machine {
    edu: true,
    ..GUESTS_M
};

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
    /// `hstatus.VGEIN = n`.
    Vgein(u32),
    HgeieW(u64),
    HgeieR,
    Hgeip,
    VsiW(u32, u64),
    VsiR(u32),
    Vstopei,
    Vsclaim,
    /// `mip & (SGEIP | VSEIP)`.
    HMip,
    /// One-time `edu` setup: BAR0 and the command register (memory space,
    /// bus master). Nothing on the model side.
    EduInit,
    /// `edu` writes an MSI with this address and data: the probe programs
    /// the MSI capability (at 0x40, 64-bit) and raises then acknowledges an
    /// `edu` interrupt; the model calls `pci_msi_write` for 00:01.0.
    EduMsi(u64, u32),
}

use Op::*;

impl Op {
    fn encode(self) -> Vec<[u64; 3]> {
        let one = match self {
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
            Vgein(n) => [15, 0, u64::from(n) << 12],
            HgeieW(v) => [16, 0, v],
            HgeieR => [17, 0, 0],
            Hgeip => [18, 0, 0],
            VsiW(i, v) => [19, u64::from(i), v],
            VsiR(i) => [20, u64::from(i), 0],
            Vstopei => [21, 0, 0],
            Vsclaim => [22, 0, 0],
            HMip => [23, 0, 0],
            EduInit => {
                return vec![[1, EDU_CFG + 0x10, EDU_BAR], [1, EDU_CFG + 4, 6]];
            }
            EduMsi(addr, data) => {
                return vec![
                    [1, EDU_CFG + 0x44, addr & 0xffff_ffff],
                    [1, EDU_CFG + 0x48, addr >> 32],
                    [1, EDU_CFG + 0x4c, u64::from(data)],
                    [1, EDU_CFG + 0x40, 1 << 16],
                    [1, EDU_BAR + 0x60, 1],
                    [1, EDU_BAR + 0x64, 1],
                ];
            }
        };
        vec![one]
    }
}

/// Runs `ops` on the model of the machine `dtb` describes, M-mode hart 0.
/// A typed error stands for the trap QEMU takes ([`FAULT`]).
fn model(machine: Machine, ops: &[Op]) -> Vec<u64> {
    let p = Platform::from_dtb(machine.dtb).unwrap();
    let ledger = Ledger::new();
    let mut intc = RiscvIntc::new(&p, Privilege::Machine, &ledger).unwrap();
    if let Some(n) = machine.geilen {
        intc.limit_guest_files(n).unwrap();
    }
    let mut out = Vec::new();
    let val = |r: Result<u64, vesper_silicon::intc::IntcError>| r.unwrap_or(FAULT);
    let unit = |r: Result<(), vesper_silicon::intc::IntcError>| r.err().map(|_| FAULT);
    const M_: Privilege = Privilege::Machine;
    const S_: Privilege = Privilege::Supervisor;
    for &op in ops {
        let v = match op {
            W(a, v) => unit(intc.write32(a, v)),
            R(a) => Some(val(intc.read32(a).map(u64::from))),
            Ier(v) => {
                intc.set_device_line(&p, UART, v & 2 != 0).unwrap();
                None
            }
            MiW(i, v) => unit(intc.imsic_csr_write(0, M_, i, v)),
            MiR(i) => Some(val(intc.imsic_csr_read(0, M_, i))),
            Mtopei => Some(val(intc.topei(0, M_).map(u64::from))),
            Mclaim => Some(val(intc.claim_topei(0, M_).map(u64::from))),
            SiW(i, v) => unit(intc.imsic_csr_write(0, S_, i, v)),
            SiR(i) => Some(val(intc.imsic_csr_read(0, S_, i))),
            Stopei => Some(val(intc.topei(0, S_).map(u64::from))),
            Sclaim => Some(val(intc.claim_topei(0, S_).map(u64::from))),
            Mip => Some(
                (u64::from(intc.external_level(0, MEIP)) << 11)
                    | (u64::from(intc.external_level(0, SEIP)) << 9),
            ),
            Vgein(n) => {
                intc.set_vgein(0, n);
                None
            }
            HgeieW(v) => {
                intc.set_hgeie(0, v);
                None
            }
            HgeieR => Some(intc.hgeie(0)),
            Hgeip => Some(intc.hgeip(0)),
            VsiW(i, v) => unit(intc.vs_imsic_csr_write(0, i, v)),
            VsiR(i) => Some(val(intc.vs_imsic_csr_read(0, i))),
            Vstopei => Some(val(intc.vstopei(0).map(u64::from))),
            Vsclaim => Some(val(intc.claim_vstopei(0).map(u64::from))),
            HMip => Some(intc.hyp_pending(0)),
            EduInit => None,
            EduMsi(addr, data) => {
                let rid = requester_id(0, 1, 0).unwrap();
                // The MSI capability's message data is 16 bits wide.
                let r = intc.pci_msi_write(&p, PCI_HOST, rid, addr, data & 0xffff);
                if let Ok(m) = &r {
                    assert_eq!(m.device_id, 0x08);
                    assert_eq!(
                        m.in_parent,
                        m.imsic.as_deref() == Some("/soc/imsics@28000000")
                    );
                }
                unit(r.map(|_| ()))
            }
        };
        out.extend(v);
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

/// Runs `ops` on QEMU `machine`.
fn qemu(bin: &PathBuf, machine: Machine, ops: &[Op]) -> Vec<u64> {
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
        for w in op.encode().into_iter().flatten() {
            bytes.extend_from_slice(&w.to_le_bytes());
        }
    }
    bytes.extend_from_slice(&[0; 24]);
    std::fs::write(&script, bytes).unwrap();
    let mut cmd = Command::new(bin);
    if machine.edu {
        cmd.args(["-device", "edu"]);
    }
    let mut child = cmd
        .args([
            "-machine",
            machine.opts,
            "-smp",
            machine.smp,
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

fn check(name: &str, machine: Machine, ops: &[Op], recorded: &[u64]) {
    let m = model(machine, ops);
    if std::env::var("VESPER_AIA_PRINT").is_ok() {
        eprintln!("{name} model: {m:x?}");
        if let Some(bin) = qemu_binary() {
            eprintln!("{name} qemu : {:x?}", qemu(&bin, machine, ops));
        }
    }
    assert_eq!(m, recorded, "{name}: model vs recorded QEMU 8.2.2 output");
    if let Some(bin) = qemu_binary() {
        let q = qemu(&bin, machine, ops);
        assert_eq!(m, q, "{name}: model vs live {}", qemu_version(&bin).trim());
    }
}

#[test]
fn msi_mode_matches_qemu() {
    check("msi", IMSIC_M, &msi_script(), MSI_EXPECTED);
}

#[test]
fn direct_mode_matches_qemu() {
    check("direct", APLIC_M, &direct_script(), DIRECT_EXPECTED);
}

#[test]
fn supervisor_level_matches_qemu() {
    check(
        "supervisor",
        IMSIC_M,
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
    type Case<'a> = (&'a str, Machine, Vec<Op>, u64, u64);
    let cases: Vec<Case> = vec![
        (
            // §4.7: in MSI mode a level source's pending bit may be set by
            // setipnum only while the rectified input is high.
            "setipnum with the level low",
            IMSIC_M,
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
            APLIC_M,
            with(direct_setup.clone(), &[Ier(2), Ier(0), R(IDC + 0x18)]),
            0,
            0xa0003,
        ),
        (
            // §4.8.1: a claimi that returns 0 clears iforce, which drops
            // the hart's interrupt.
            "claimi clearing iforce",
            APLIC_M,
            with(direct_setup.clone(), &[W(IDC + 4, 1), R(IDC + 0x1c), Mip]),
            0,
            0x800,
        ),
        (
            // §4.5.2: a source not delegated to a domain is inactive there
            // and its sourcecfg reads zero.
            "child sourcecfg after undelegation",
            IMSIC_M,
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
            APLIC_M,
            with(
                direct_setup.clone(),
                &[W(M + 0x28, 1), Ier(2), R(M + 0x1d00)],
            ),
            0,
            0x400,
        ),
        (
            // §4.5.4/§4.9.1: an S-level domain's MSI address takes LHXW,
            // HHXW and HHXS from mmsiaddrcfgh (smsiaddrcfgh holds only
            // LHXS). QEMU 8.2.2 reads them from smsiaddrcfgh, where they
            // are zero, so it drops the hart index and hart 1's guest 2
            // MSI lands in hart 0's guest 2 file.
            "S-level MSI hart index",
            GUESTS_M,
            vec![
                W(M + 0x1bc0, 0x24000),
                W(M + 0x1bc4, 2 << 12),
                W(M + 0x1bc8, 0x28000),
                W(M + 0x1bcc, 2 << 20),
                W(M + 0x28, 0x400),
                W(S, 0x104),
                W(S + 0x28, 6),
                W(S + 0x3028, (1 << 18) | (2 << 12) | 11),
                W(S + 0x1edc, 10),
                Ier(2),
                Vgein(2),
                VsiR(0x80),
            ],
            0,
            0x800,
        ),
        (
            // Reserved source modes 2 and 3 (model choice: stored as
            // Inactive; QEMU keeps the value).
            "reserved source mode",
            APLIC_M,
            vec![W(M + 0x28, 2), R(M + 0x28)],
            0,
            2,
        ),
    ];
    let live = qemu_binary().map(|b| {
        let v = qemu_version(&b);
        (b, v)
    });
    for (name, machine, ops, spec, qemu_8_2) in cases {
        let m = model(machine, &ops);
        assert_eq!(*m.last().unwrap(), spec, "{name}: model");
        if let Some((bin, version)) = &live {
            let q = qemu(bin, machine, &ops);
            if version.contains("version 8.2.") {
                assert_eq!(*q.last().unwrap(), qemu_8_2, "{name}: QEMU 8.2");
            } else {
                eprintln!("{name}: {} gives {:#x}", version.trim(), q.last().unwrap());
            }
        }
    }
}

/// Guest interrupt files: GEILEN and `hgeie`'s writable bits, VGEIN
/// selecting the file behind `vsiselect`/`vsireg`/`vstopei` (and the
/// illegal-instruction trap when it names none), `hgeip` following each
/// guest file's output (delivery, enables, threshold), SGEIP versus VSEIP,
/// and the VS-level claim.
fn hypervisor_script() -> Vec<Op> {
    vec![
        HgeieR,
        HgeieW(0xffff),
        HgeieR,
        Hgeip,
        HMip,
        Vgein(1),
        VsiW(0x70, 1),
        VsiR(0x70),
        VsiW(0xc0, 1 << 5),
        VsiR(0xc0),
        Vstopei,
        W(S_FILE + 0x1000, 5),
        Vstopei,
        Hgeip,
        HMip,
        VsiR(0x80),
        // A threshold at or below the identity silences the file.
        VsiW(0x72, 5),
        Vstopei,
        Hgeip,
        HMip,
        VsiW(0x72, 6),
        Hgeip,
        VsiW(0x72, 0),
        // hgeie gates SGEIP only; VSEIP follows hgeip[VGEIN].
        HgeieW(0),
        HMip,
        HgeieW(6),
        HMip,
        Vsclaim,
        Vstopei,
        Hgeip,
        HMip,
        // VGEIN 0 and VGEIN > GEILEN name no guest file.
        Vgein(0),
        VsiR(0x70),
        Vstopei,
        Vsclaim,
        VsiW(0x70, 1),
        HMip,
        Vgein(3),
        VsiR(0x70),
        Vstopei,
        // Guest 2: an MSI before delivery and enable is pending but silent.
        Vgein(2),
        VsiR(0x70),
        W(S_FILE + 0x2000, 7),
        Hgeip,
        VsiW(0xc0, 1 << 7),
        Hgeip,
        VsiW(0x70, 1),
        Hgeip,
        HMip,
        Vstopei,
        // VGEIN back to guest 1: VSEIP drops, SGEIP stays.
        Vgein(1),
        HMip,
        Vstopei,
        // The guest files do not touch the hart's own S file.
        SiR(0x80),
        Stopei,
        // Odd eip numbers and reserved selectors trap in the guest too.
        Vgein(2),
        VsiR(0x81),
        VsiR(0x60),
        Vsclaim,
        Hgeip,
        HMip,
    ]
}

/// An S-level APLIC domain forwards to a guest file: the target's guest
/// index (bits 17:12), the root's `smsiaddrcfgh.LHXS` for the guest
/// stride, `hgeip` and the VS-level claim.
fn aplic_guest_script() -> Vec<Op> {
    vec![
        W(M + 0x1bc0, 0x24000),
        W(M + 0x1bc4, 2 << 12),
        W(M + 0x1bc8, 0x28000),
        W(M + 0x1bcc, 2 << 20),
        R(M + 0x1bc4),
        R(M + 0x1bcc),
        W(M + 0x28, 0x400),
        W(S, 0x104),
        W(S + 0x28, 6),
        W(S + 0x3028, (1 << 12) | 9),
        R(S + 0x3028),
        W(S + 0x1edc, 10),
        Vgein(1),
        VsiW(0x70, 1),
        VsiW(0xc0, 1 << 9),
        HgeieW(2),
        Ier(2),
        Hgeip,
        HMip,
        Vstopei,
        SiR(0x80),
        Vsclaim,
        Hgeip,
        Ier(0),
        // Guest 2 of the same hart.
        W(S + 0x3028, (2 << 12) | 11),
        R(S + 0x3028),
        Vgein(2),
        VsiW(0x70, 1),
        VsiW(0xc0, 1 << 11),
        Ier(2),
        Hgeip,
        HMip,
        Vsclaim,
        Ier(0),
    ]
}

/// PCI MSIs from QEMU's `edu` (00:01.0, requester ID 0x08) through the
/// host's `msi-parent`: into the hart's S file, into guest file 1, into
/// hart 1's file, and into the M-level IMSIC, which is not the host's
/// `msi-parent` but which nothing stops a function from writing without an
/// IOMMU (the model delivers it and flags it, `in_parent: false`).
fn pci_script() -> Vec<Op> {
    vec![
        EduInit,
        SiW(0x70, 1),
        SiW(0xc0, 1 << 9),
        EduMsi(S_FILE, 9),
        Stopei,
        Sclaim,
        Vgein(1),
        VsiW(0x70, 1),
        VsiW(0xc0, 1 << 12),
        HgeieW(2),
        EduMsi(S_FILE + 0x1000, 12),
        Hgeip,
        HMip,
        Vstopei,
        Vsclaim,
        // MSI message data is 16 bits: the high half of a big-endian
        // seteipnum value never reaches the IMSIC.
        EduMsi(S_FILE + 0x1004, 12u32.swap_bytes()),
        Vstopei,
        // Hart 1's S file.
        EduMsi(S_FILE + 0x4000, 3),
        Stopei,
        // The M-level IMSIC is not this host's msi-parent.
        EduMsi(M_FILE, 3),
        MiR(0x80),
        Hgeip,
    ]
}

/// Recorded on QEMU 8.2.2 (Ubuntu 1:8.2.2+ds-0ubuntu1.18).
const APLIC_GUEST_EXPECTED: &[u64] = &[
    0x2000, 0x200000, 0x1009, 2, 0x1400, 0x90009, 0, 0x90009, 0, 0x200b, 4, 0x400, 0xb000b,
];
/// Recorded on QEMU 8.2.2 (Ubuntu 1:8.2.2+ds-0ubuntu1.18).
const PCI_EXPECTED: &[u64] = &[0x90009, 0x90009, 2, 0x1400, 0xc000c, 0xc000c, 0, 0, 8, 0];

#[test]
fn hypervisor_guest_files_match_qemu() {
    check(
        "hypervisor",
        GUESTS_M,
        &hypervisor_script(),
        HYPERVISOR_EXPECTED,
    );
}

#[test]
fn aplic_to_guest_file_matches_qemu() {
    check(
        "aplic-guest",
        GUESTS_M,
        &aplic_guest_script(),
        APLIC_GUEST_EXPECTED,
    );
}

#[test]
fn pci_msi_matches_qemu() {
    check("pci", GUESTS_EDU_M, &pci_script(), PCI_EXPECTED);
}
