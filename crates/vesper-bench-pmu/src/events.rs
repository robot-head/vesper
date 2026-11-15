//! Extended hardware-event selection (§41.1): L1D and LLC read misses,
//! branch mispredictions, reference cycles, SIMD operations and Top-Down
//! (TMA) slots, each encoded only where the OS exposes it.
//!
//! Encodings come from two places, in order:
//!
//! 1. Linux generic events (`PERF_TYPE_HARDWARE`, `PERF_TYPE_HW_CACHE`),
//!    which the kernel maps to the CPU's own event or rejects.
//! 2. Named sysfs aliases of a core PMU device
//!    (`/sys/bus/event_source/devices/<pmu>/events/<alias>`), encoded with
//!    that device's `format/` bit layout. Intel publishes `slots` and
//!    `topdown-be-bound`; Arm PMUv3 publishes `stall_slot`,
//!    `stall_slot_backend`, `ase_spec` and `sve_inst_spec`.
//!
//! An event with neither encoding is [`UnavailableReason::EventNotExposed`].
//! Parsing is pure and tested on fixtures; opening events is Linux-only.
use crate::{Unavailable, UnavailableReason};
use std::collections::BTreeMap;

/// Events beyond the cycles/instructions pair every provider reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HardwareEvent {
    RefCycles,
    L1dReadMisses,
    LlcReadMisses,
    BranchMispredicts,
    SimdOperations,
    TmaSlots,
    TmaBackendBoundSlots,
}
impl HardwareEvent {
    pub const ALL: [HardwareEvent; 7] = [
        Self::RefCycles,
        Self::L1dReadMisses,
        Self::LlcReadMisses,
        Self::BranchMispredicts,
        Self::SimdOperations,
        Self::TmaSlots,
        Self::TmaBackendBoundSlots,
    ];
    pub const fn name(self) -> &'static str {
        match self {
            Self::RefCycles => "ref-cycles",
            Self::L1dReadMisses => "l1d-read-misses",
            Self::LlcReadMisses => "llc-read-misses",
            Self::BranchMispredicts => "branch-mispredicts",
            Self::SimdOperations => "simd-ops",
            Self::TmaSlots => "tma-slots",
            Self::TmaBackendBoundSlots => "tma-backend-bound",
        }
    }
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|e| e.name() == name)
    }
    /// The Linux generic encoding `(type, config)`, if one exists.
    pub const fn generic(self) -> Option<(u32, u64)> {
        // include/uapi/linux/perf_event.h
        const HARDWARE: u32 = 0;
        const HW_CACHE: u32 = 3;
        const L1D: u64 = 0;
        const LL: u64 = 2;
        const OP_READ: u64 = 0;
        const RESULT_MISS: u64 = 1;
        match self {
            Self::RefCycles => Some((HARDWARE, 9)),
            Self::BranchMispredicts => Some((HARDWARE, 5)),
            Self::L1dReadMisses => Some((HW_CACHE, L1D | (OP_READ << 8) | (RESULT_MISS << 16))),
            Self::LlcReadMisses => Some((HW_CACHE, LL | (OP_READ << 8) | (RESULT_MISS << 16))),
            _ => None,
        }
    }
    /// sysfs alias names, Intel first, then Arm.
    pub const fn sysfs_aliases(self) -> &'static [&'static str] {
        match self {
            Self::TmaSlots => &["slots", "stall_slot"],
            Self::TmaBackendBoundSlots => &["topdown-be-bound", "stall_slot_backend"],
            Self::SimdOperations => &["ase_spec", "sve_inst_spec"],
            _ => &[],
        }
    }
}

/// One resolved event encoding for `perf_event_open`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventEncoding {
    pub event: HardwareEvent,
    pub event_type: u32,
    pub config: u64,
    pub config1: u64,
    /// `generic` or `sysfs:<pmu>/<alias>`.
    pub source: String,
}

/// A parsed `format/<term>` file such as `config:0-7,21` or `config1:0-15`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormatSpec {
    /// 0 = config, 1 = config1.
    pub word: u8,
    /// Inclusive bit ranges, low to high.
    pub ranges: Vec<(u32, u32)>,
}

fn bad(what: &'static str) -> Unavailable {
    Unavailable::new(UnavailableReason::InvalidMetadata, what)
}

pub fn parse_format(text: &str) -> Result<FormatSpec, Unavailable> {
    let (word, bits) = text
        .trim()
        .split_once(':')
        .ok_or_else(|| bad("sysfs format"))?;
    let word = match word {
        "config" => 0,
        "config1" => 1,
        // config2/config3 need a larger perf_event_attr than this crate's v0.
        _ => return Err(bad("sysfs format word")),
    };
    let mut ranges = Vec::new();
    for part in bits.split(',') {
        let (lo, hi) = match part.split_once('-') {
            Some((lo, hi)) => (lo, hi),
            None => (part, part),
        };
        let lo: u32 = lo.parse().map_err(|_| bad("sysfs format bit"))?;
        let hi: u32 = hi.parse().map_err(|_| bad("sysfs format bit"))?;
        if lo > hi || hi > 63 {
            return Err(bad("sysfs format range"));
        }
        ranges.push((lo, hi));
    }
    if ranges.is_empty() || ranges.len() > 8 {
        return Err(bad("sysfs format ranges"));
    }
    Ok(FormatSpec { word, ranges })
}

/// Scatters `value` into the format's bit ranges, low bits first.
fn place(spec: &FormatSpec, mut value: u64) -> Result<u64, Unavailable> {
    let mut out = 0u64;
    for &(lo, hi) in &spec.ranges {
        let width = hi - lo + 1;
        let mask = if width == 64 {
            u64::MAX
        } else {
            (1u64 << width) - 1
        };
        out |= (value & mask) << lo;
        value = if width == 64 { 0 } else { value >> width };
    }
    if value != 0 {
        return Err(bad("sysfs event value exceeds format"));
    }
    Ok(out)
}

/// Encodes a sysfs event string such as `event=0x00,umask=0x81` with the
/// device's format files. A bare term (`edge`) means value 1.
pub fn encode_sysfs_event(
    text: &str,
    formats: &BTreeMap<String, FormatSpec>,
) -> Result<(u64, u64), Unavailable> {
    let mut words = [0u64; 2];
    let text = text.trim();
    if text.is_empty() || text.len() > 512 {
        return Err(bad("sysfs event"));
    }
    for term in text.split(',') {
        let (name, value) = match term.split_once('=') {
            Some((n, v)) => (n.trim(), v.trim()),
            None => (term.trim(), "1"),
        };
        let value = if let Some(hex) = value.strip_prefix("0x") {
            u64::from_str_radix(hex, 16)
        } else {
            value.parse()
        }
        .map_err(|_| bad("sysfs event value"))?;
        let spec = formats.get(name).ok_or_else(|| bad("sysfs event term"))?;
        words[usize::from(spec.word)] |= place(spec, value)?;
    }
    Ok((words[0], words[1]))
}

/// A read-only view of one PMU device directory, so resolution can be
/// tested without sysfs.
pub trait PmuDevice {
    fn name(&self) -> &str;
    fn type_id(&self) -> Option<u32>;
    fn event(&self, alias: &str) -> Option<String>;
    fn format(&self, term: &str) -> Option<String>;
    fn format_terms(&self) -> Vec<String>;
}

fn formats_of(device: &dyn PmuDevice) -> Result<BTreeMap<String, FormatSpec>, Unavailable> {
    let mut formats = BTreeMap::new();
    for term in device.format_terms().into_iter().take(64) {
        if let Some(text) = device.format(&term) {
            formats.insert(term, parse_format(&text)?);
        }
    }
    Ok(formats)
}

/// Resolves `event` to an encoding: generic first, then the first device
/// alias that encodes. Malformed sysfs data is an error, not a skip.
pub fn resolve_event(
    event: HardwareEvent,
    devices: &[&dyn PmuDevice],
) -> Result<EventEncoding, Unavailable> {
    if let Some((event_type, config)) = event.generic() {
        return Ok(EventEncoding {
            event,
            event_type,
            config,
            config1: 0,
            source: "generic".into(),
        });
    }
    for device in devices {
        for alias in event.sysfs_aliases() {
            let Some(text) = device.event(alias) else {
                continue;
            };
            let event_type = device.type_id().ok_or_else(|| bad("sysfs pmu type"))?;
            let (config, config1) = encode_sysfs_event(&text, &formats_of(*device)?)?;
            return Ok(EventEncoding {
                event,
                event_type,
                config,
                config1,
                source: format!("sysfs:{}/{alias}", device.name()),
            });
        }
    }
    Err(Unavailable::new(
        UnavailableReason::EventNotExposed,
        event.name(),
    ))
}

/// The host's core PMU devices under `/sys/bus/event_source/devices`.
pub struct SysfsDevice {
    name: String,
    path: std::path::PathBuf,
}
fn read_small(path: &std::path::Path) -> Option<String> {
    use std::io::Read;
    let file = std::fs::File::open(path).ok()?;
    let mut text = String::new();
    file.take(4096).read_to_string(&mut text).ok()?;
    Some(text.trim().to_string())
}
impl PmuDevice for SysfsDevice {
    fn name(&self) -> &str {
        &self.name
    }
    fn type_id(&self) -> Option<u32> {
        read_small(&self.path.join("type"))?.parse().ok()
    }
    fn event(&self, alias: &str) -> Option<String> {
        read_small(&self.path.join("events").join(alias))
    }
    fn format(&self, term: &str) -> Option<String> {
        read_small(&self.path.join("format").join(term))
    }
    fn format_terms(&self) -> Vec<String> {
        std::fs::read_dir(self.path.join("format"))
            .map(|d| {
                d.flatten()
                    .filter_map(|e| e.file_name().into_string().ok())
                    .collect()
            })
            .unwrap_or_default()
    }
}
/// Core PMU devices (`cpu`, `cpu_core`, `armv8_pmuv3*`); empty off Linux.
pub fn host_core_pmus() -> Vec<SysfsDevice> {
    let root = std::path::Path::new("/sys/bus/event_source/devices");
    let mut devices: Vec<SysfsDevice> = std::fs::read_dir(root)
        .map(|d| {
            d.flatten()
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|n| n == "cpu" || n == "cpu_core" || n.starts_with("armv8_pmuv3"))
                .map(|name| SysfsDevice {
                    path: root.join(&name),
                    name,
                })
                .collect()
        })
        .unwrap_or_default();
    devices.sort_by(|a, b| a.name.cmp(&b.name));
    devices
}
/// [`resolve_event`] against [`host_core_pmus`].
pub fn resolve_host_event(event: HardwareEvent) -> Result<EventEncoding, Unavailable> {
    let devices = host_core_pmus();
    let refs: Vec<&dyn PmuDevice> = devices.iter().map(|d| d as &dyn PmuDevice).collect();
    resolve_event(event, &refs)
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture {
        name: &'static str,
        type_id: Option<u32>,
        events: &'static [(&'static str, &'static str)],
        formats: &'static [(&'static str, &'static str)],
    }
    impl PmuDevice for Fixture {
        fn name(&self) -> &str {
            self.name
        }
        fn type_id(&self) -> Option<u32> {
            self.type_id
        }
        fn event(&self, alias: &str) -> Option<String> {
            self.events
                .iter()
                .find(|(a, _)| *a == alias)
                .map(|(_, t)| t.to_string())
        }
        fn format(&self, term: &str) -> Option<String> {
            self.formats
                .iter()
                .find(|(a, _)| *a == term)
                .map(|(_, t)| t.to_string())
        }
        fn format_terms(&self) -> Vec<String> {
            self.formats.iter().map(|(t, _)| t.to_string()).collect()
        }
    }
    // Shapes copied from an Intel Ice Lake and an Arm Neoverse N1 sysfs tree.
    const INTEL: Fixture = Fixture {
        name: "cpu",
        type_id: Some(4),
        events: &[
            ("slots", "event=0x00,umask=0x4"),
            ("topdown-be-bound", "event=0x00,umask=0x83"),
        ],
        formats: &[
            ("event", "config:0-7"),
            ("umask", "config:8-15"),
            ("edge", "config:18"),
            ("cmask", "config:24-31"),
        ],
    };
    const ARM: Fixture = Fixture {
        name: "armv8_pmuv3_0",
        type_id: Some(8),
        events: &[
            ("stall_slot", "event=0x003F"),
            ("stall_slot_backend", "event=0x003D"),
            ("ase_spec", "event=0x0074"),
        ],
        formats: &[("event", "config:0-15"), ("long", "config1:0")],
    };

    #[test]
    fn generic_events_use_the_kernel_abi_encodings() {
        let l1 = resolve_event(HardwareEvent::L1dReadMisses, &[]).unwrap();
        assert_eq!((l1.event_type, l1.config), (3, 0x10000));
        let llc = resolve_event(HardwareEvent::LlcReadMisses, &[]).unwrap();
        assert_eq!((llc.event_type, llc.config), (3, 0x10002));
        let br = resolve_event(HardwareEvent::BranchMispredicts, &[]).unwrap();
        assert_eq!(
            (br.event_type, br.config, br.source.as_str()),
            (0, 5, "generic")
        );
        let refc = resolve_event(HardwareEvent::RefCycles, &[]).unwrap();
        assert_eq!((refc.event_type, refc.config), (0, 9));
    }

    #[test]
    fn tma_and_simd_come_from_sysfs_aliases_or_are_not_exposed() {
        let slots = resolve_event(HardwareEvent::TmaSlots, &[&INTEL]).unwrap();
        assert_eq!((slots.event_type, slots.config), (4, 0x0400));
        assert_eq!(slots.source, "sysfs:cpu/slots");
        let be = resolve_event(HardwareEvent::TmaBackendBoundSlots, &[&INTEL]).unwrap();
        assert_eq!(be.config, 0x8300);
        let arm = resolve_event(HardwareEvent::TmaBackendBoundSlots, &[&ARM]).unwrap();
        assert_eq!((arm.event_type, arm.config), (8, 0x3d));
        let simd = resolve_event(HardwareEvent::SimdOperations, &[&INTEL, &ARM]).unwrap();
        assert_eq!(simd.source, "sysfs:armv8_pmuv3_0/ase_spec");
        assert_eq!(
            resolve_event(HardwareEvent::SimdOperations, &[&INTEL])
                .unwrap_err()
                .reason,
            UnavailableReason::EventNotExposed
        );
        assert_eq!(
            resolve_event(HardwareEvent::TmaSlots, &[])
                .unwrap_err()
                .reason,
            UnavailableReason::EventNotExposed
        );
    }

    #[test]
    fn formats_scatter_bits_and_reject_malformed_data() {
        let split = parse_format("config:0-3,8-11").unwrap();
        assert_eq!(split.ranges, [(0, 3), (8, 11)]);
        assert_eq!(place(&split, 0xab).unwrap(), 0xa0b);
        assert!(place(&split, 0x1ab).is_err());
        let mut formats = BTreeMap::new();
        formats.insert("event".to_string(), parse_format("config:0-7").unwrap());
        formats.insert("edge".to_string(), parse_format("config:18").unwrap());
        formats.insert("long".to_string(), parse_format("config1:0").unwrap());
        assert_eq!(
            encode_sysfs_event("event=0x3c,edge,long=1", &formats).unwrap(),
            (0x3c | 1 << 18, 1)
        );
        for bad in ["event=0x100", "nope=1", "event=zz", "", "event=0x1,edge=2"] {
            assert!(encode_sysfs_event(bad, &formats).is_err(), "{bad}");
        }
        for bad in [
            "config2:0-7",
            "config:7-0",
            "config:64",
            "config",
            "config:a",
        ] {
            assert!(parse_format(bad).is_err(), "{bad}");
        }
        let broken = Fixture {
            formats: &[("event", "config:0-3")],
            ..ARM
        };
        assert_eq!(
            resolve_event(HardwareEvent::TmaSlots, &[&broken])
                .unwrap_err()
                .reason,
            UnavailableReason::InvalidMetadata
        );
        let untyped = Fixture {
            type_id: None,
            ..ARM
        };
        assert!(resolve_event(HardwareEvent::TmaSlots, &[&untyped]).is_err());
    }

    #[test]
    fn names_round_trip() {
        for event in HardwareEvent::ALL {
            assert_eq!(HardwareEvent::parse(event.name()), Some(event));
        }
        assert_eq!(HardwareEvent::parse("cycles"), None);
    }
}
