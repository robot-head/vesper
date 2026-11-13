//! Gated hardware trace capture through `perf_event_open` (Linux only).
//!
//! [`probe`] looks for a branch-trace PMU under
//! `/sys/bus/event_source/devices` (`intel_pt`, `cs_etm`, or the
//! statistical-profiling `arm_spe_*`). [`capture_command`] opens an AUX-area
//! perf event for a child process on that PMU, lets the child run, and returns
//! the raw trace bytes. Without the PMU, or when the kernel refuses the event
//! (for example `perf_event_paranoid` above 1 without `CAP_PERFMON`), it
//! returns [`CaptureError::Unavailable`] with the reason; nothing is
//! simulated.
//!
//! The child is started as `sh -c 'read _; exec "$0" "$@"' program args...`:
//! it waits on its stdin pipe, the event is opened with `enable_on_exec` on
//! its pid, and a newline releases it, so tracing begins at the `exec` of the
//! program and no code of it runs untraced. The AUX ring is drained while the
//! child runs, and `PERF_RECORD_AUX` truncation or `PERF_RECORD_LOST` marks
//! the capture as lossy.
//!
//! This is the only module of the crate with `unsafe` code (the wave-six
//! contract allows `perf_event_open` capture here). On the development host
//! there is no `intel_pt`, `cs_etm` or `arm_spe` PMU, so only the unavailable
//! path has been exercised; the open/mmap/AUX path has not run on hardware.

use std::fmt;
use std::path::{Path, PathBuf};

/// Default sysfs directory of perf PMUs.
pub const SYSFS_PMU_ROOT: &str = "/sys/bus/event_source/devices";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TracePmu {
    /// Intel Processor Trace (decode with [`crate::ipt`]).
    IntelPt,
    /// Arm CoreSight ETM/ETE (decode with [`crate::ete`] after removing the
    /// CoreSight frame formatter, when the sink uses one).
    CoreSight,
    /// Arm Statistical Profiling Extension: sampled records, not a branch
    /// trace; returned raw, not decoded by this crate.
    ArmSpe,
}

impl TracePmu {
    pub fn sysfs_names(self) -> &'static [&'static str] {
        match self {
            Self::IntelPt => &["intel_pt"],
            Self::CoreSight => &["cs_etm"],
            Self::ArmSpe => &["arm_spe_0", "arm_spe"],
        }
    }
}

/// A PMU found in sysfs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PmuProbe {
    pub pmu: TracePmu,
    /// The dynamic perf event type from `<pmu>/type`.
    pub event_type: u32,
    pub dir: PathBuf,
    /// `config` bits enabling timestamps and branch tracing, from the PMU's
    /// `format/` directory where present.
    pub config: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureError {
    /// Hardware tracing cannot run here; `reason` says why.
    Unavailable {
        reason: String,
    },
    /// The PMU exists but a sysfs file is malformed.
    BadSysfs {
        path: PathBuf,
        detail: String,
    },
    /// An OS call failed for another reason.
    Os {
        call: &'static str,
        errno: i32,
    },
    /// The child could not be started or waited for.
    Child {
        detail: String,
    },
    InvalidConfig(&'static str),
}

impl fmt::Display for CaptureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "chrono capture: {self:?}")
    }
}
impl std::error::Error for CaptureError {}

/// Parse a sysfs format file such as `config:10` or `config:0-3`, returning
/// the mask it selects in `config` (other config words are ignored).
pub fn parse_format(text: &str) -> Option<u64> {
    let text = text.trim();
    let bits = text.strip_prefix("config:")?;
    let (lo, hi) = match bits.split_once('-') {
        Some((a, b)) => (a.parse::<u32>().ok()?, b.parse::<u32>().ok()?),
        None => {
            let b = bits.parse::<u32>().ok()?;
            (b, b)
        }
    };
    if lo > hi || hi > 63 {
        return None;
    }
    let width = hi - lo + 1;
    let mask = if width == 64 {
        u64::MAX
    } else {
        (1u64 << width) - 1
    };
    Some(mask << lo)
}

fn read_trim(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
}

/// Look for a trace PMU under `root` (normally [`SYSFS_PMU_ROOT`]), in the
/// order Intel PT, CoreSight, SPE.
pub fn probe(root: &Path) -> Result<PmuProbe, CaptureError> {
    for pmu in [TracePmu::IntelPt, TracePmu::CoreSight, TracePmu::ArmSpe] {
        for name in pmu.sysfs_names() {
            let dir = root.join(name);
            let type_path = dir.join("type");
            let Some(text) = read_trim(&type_path) else {
                continue;
            };
            let event_type = text.parse::<u32>().map_err(|e| CaptureError::BadSysfs {
                path: type_path.clone(),
                detail: e.to_string(),
            })?;
            let mut config = 0u64;
            // Timestamps (`tsc` on PT, `timestamp` on ETM) and branch
            // tracing (`branch`, PT's default but explicit when listed).
            for field in ["tsc", "timestamp", "branch", "ts_enable"] {
                let path = dir.join("format").join(field);
                if let Some(text) = read_trim(&path) {
                    config |= parse_format(&text).ok_or_else(|| CaptureError::BadSysfs {
                        path: path.clone(),
                        detail: format!("unsupported format {text:?}"),
                    })?;
                }
            }
            return Ok(PmuProbe {
                pmu,
                event_type,
                dir,
                config,
            });
        }
    }
    let paranoid = read_trim(Path::new("/proc/sys/kernel/perf_event_paranoid"))
        .map(|p| format!(", perf_event_paranoid = {p}"))
        .unwrap_or_default();
    Err(CaptureError::Unavailable {
        reason: format!(
            "no intel_pt, cs_etm or arm_spe PMU under {}{paranoid}",
            root.display()
        ),
    })
}

/// Capture parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureConfig {
    /// Data ring pages (a power of two).
    pub data_pages: usize,
    /// AUX (trace) area pages (a power of two).
    pub aux_pages: usize,
    /// Trace user space only.
    pub user_only: bool,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            data_pages: 8,
            aux_pages: 256,
            user_only: true,
        }
    }
}

/// Raw trace bytes captured from a child process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedTrace {
    pub pmu: TracePmu,
    pub aux: Vec<u8>,
    /// True when trace data was lost: the kernel reported a truncated AUX
    /// record or lost records, or the AUX ring was overrun.
    pub wrapped: bool,
    pub exit_code: Option<i32>,
}

/// Trace `program args...` from its `exec` to its exit.
pub fn capture_command(
    root: &Path,
    program: &str,
    args: &[&str],
    config: CaptureConfig,
) -> Result<CapturedTrace, CaptureError> {
    let probe = probe(root)?;
    if !config.data_pages.is_power_of_two() || !config.aux_pages.is_power_of_two() {
        return Err(CaptureError::InvalidConfig(
            "ring sizes must be powers of two",
        ));
    }
    #[cfg(target_os = "linux")]
    {
        linux::capture(&probe, program, args, config)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (probe, program, args);
        Err(CaptureError::Unavailable {
            reason: "perf_event_open capture is Linux-only".into(),
        })
    }
}

#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
mod linux {
    use super::{CaptureConfig, CaptureError, CapturedTrace, PmuProbe};
    use std::io::Write;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::process::{Command, Stdio};
    use std::sync::atomic::{Ordering, fence};

    /// `perf_event_attr` up to `PERF_ATTR_SIZE_VER5` (112 bytes). No implicit
    /// padding: every field is naturally aligned and the tail is explicit.
    #[repr(C)]
    #[derive(Default)]
    struct Attr {
        event_type: u32,
        size: u32,
        config: u64,
        sample_period: u64,
        sample_type: u64,
        read_format: u64,
        flags: u64,
        wakeup_events: u32,
        bp_type: u32,
        config1: u64,
        config2: u64,
        branch_sample_type: u64,
        sample_regs_user: u64,
        sample_stack_user: u32,
        clockid: i32,
        sample_regs_intr: u64,
        aux_watermark: u32,
        sample_max_stack: u16,
        reserved: u16,
    }
    const _: () = assert!(std::mem::size_of::<Attr>() == 112);

    const FLAG_DISABLED: u64 = 1 << 0;
    const FLAG_EXCLUDE_KERNEL: u64 = 1 << 5;
    const FLAG_EXCLUDE_HV: u64 = 1 << 6;
    const FLAG_ENABLE_ON_EXEC: u64 = 1 << 12;
    const PERF_FLAG_FD_CLOEXEC: libc::c_ulong = 1 << 3;
    // _IO('$', 1)
    const PERF_EVENT_IOC_DISABLE: libc::c_ulong = 0x2401;

    // Offsets in `struct perf_event_mmap_page` (uapi/linux/perf_event.h).
    const AUX_HEAD: usize = 1056;
    const AUX_TAIL: usize = 1064;
    const AUX_OFFSET: usize = 1072;
    const AUX_SIZE: usize = 1080;

    struct Mapping {
        ptr: *mut u8,
        len: usize,
    }
    impl Drop for Mapping {
        fn drop(&mut self) {
            // SAFETY: an mmap we own, unmapped once.
            unsafe { libc::munmap(self.ptr.cast(), self.len) };
        }
    }

    fn errno() -> i32 {
        std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
    }

    fn map(
        fd: &OwnedFd,
        len: usize,
        offset: usize,
        call: &'static str,
    ) -> Result<Mapping, CaptureError> {
        // SAFETY: a fresh shared mapping of the perf fd; no address hint.
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd.as_raw_fd(),
                offset as libc::off_t,
            )
        };
        if ptr == libc::MAP_FAILED {
            return Err(CaptureError::Os {
                call,
                errno: errno(),
            });
        }
        Ok(Mapping {
            ptr: ptr.cast(),
            len,
        })
    }

    // perf_event_mmap_page data ring fields and record types.
    const DATA_HEAD: usize = 1024;
    const DATA_TAIL: usize = 1032;
    const PERF_RECORD_AUX: u32 = 11;
    const PERF_RECORD_LOST: u32 = 2;
    const PERF_AUX_FLAG_TRUNCATED: u64 = 1;

    struct Rings<'m> {
        base: &'m Mapping,
        aux: &'m Mapping,
        aux_size: u64,
        data_offset: usize,
        data_size: u64,
        bytes: Vec<u8>,
        truncated: bool,
    }

    impl Rings<'_> {
        fn read_u64(&self, offset: usize) -> u64 {
            // SAFETY: `offset` is a u64 field of the mapped header page.
            unsafe { self.base.ptr.add(offset).cast::<u64>().read_volatile() }
        }
        fn write_u64(&self, offset: usize, value: u64) {
            // SAFETY: as above; the tail fields are ours to write.
            unsafe {
                self.base
                    .ptr
                    .add(offset)
                    .cast::<u64>()
                    .write_volatile(value)
            }
        }
        fn data_byte(&self, at: u64) -> u8 {
            let off = self.data_offset + (at % self.data_size) as usize;
            // SAFETY: `off` lies inside the mapped data ring.
            unsafe { self.base.ptr.add(off).read_volatile() }
        }
        fn data_u64(&self, at: u64) -> u64 {
            (0..8).fold(0u64, |v, i| {
                v | (u64::from(self.data_byte(at + i)) << (8 * i))
            })
        }

        /// Copy new AUX bytes and consume data-ring records.
        fn drain(&mut self) {
            let head = self.read_u64(AUX_HEAD);
            fence(Ordering::Acquire);
            let mut at = self.read_u64(AUX_TAIL);
            if head.saturating_sub(at) > self.aux_size {
                self.truncated = true;
                at = head - self.aux_size;
            }
            while at < head {
                let off = (at % self.aux_size) as usize;
                let run = (self.aux_size - at % self.aux_size).min(head - at) as usize;
                // SAFETY: [off, off + run) lies inside the mapped AUX area.
                self.bytes.extend_from_slice(unsafe {
                    std::slice::from_raw_parts(self.aux.ptr.add(off), run)
                });
                at += run as u64;
            }
            fence(Ordering::Release);
            self.write_u64(AUX_TAIL, head);

            let head = self.read_u64(DATA_HEAD);
            fence(Ordering::Acquire);
            let mut at = self.read_u64(DATA_TAIL);
            while at + 8 <= head {
                let header = self.data_u64(at);
                let kind = header as u32;
                let size = u64::from((header >> 48) as u16);
                if size < 8 {
                    break;
                }
                // PERF_RECORD_AUX: aux_offset, aux_size, flags.
                if kind == PERF_RECORD_AUX
                    && size >= 32
                    && self.data_u64(at + 24) & PERF_AUX_FLAG_TRUNCATED != 0
                {
                    self.truncated = true;
                }
                if kind == PERF_RECORD_LOST {
                    self.truncated = true;
                }
                at += size;
            }
            fence(Ordering::Release);
            self.write_u64(DATA_TAIL, at.min(head));
        }
    }

    pub(super) fn capture(
        probe: &PmuProbe,
        program: &str,
        args: &[&str],
        config: CaptureConfig,
    ) -> Result<CapturedTrace, CaptureError> {
        let mut child = Command::new("/bin/sh")
            .arg("-c")
            .arg("read _ ; exec \"$0\" \"$@\"")
            .arg(program)
            .args(args)
            .stdin(Stdio::piped())
            .spawn()
            .map_err(|e| CaptureError::Child {
                detail: e.to_string(),
            })?;
        let result = trace_child(probe, &mut child, config);
        if result.is_err() {
            let _ = child.kill();
            let _ = child.wait();
        }
        result
    }

    fn trace_child(
        probe: &PmuProbe,
        child: &mut std::process::Child,
        config: CaptureConfig,
    ) -> Result<CapturedTrace, CaptureError> {
        let mut flags = FLAG_DISABLED | FLAG_ENABLE_ON_EXEC;
        if config.user_only {
            flags |= FLAG_EXCLUDE_KERNEL | FLAG_EXCLUDE_HV;
        }
        let attr = Attr {
            event_type: probe.event_type,
            size: std::mem::size_of::<Attr>() as u32,
            config: probe.config,
            flags,
            ..Default::default()
        };
        // SAFETY: `attr` is a valid, fully initialised perf_event_attr of
        // the size it declares; the other arguments are plain integers.
        let fd = unsafe {
            libc::syscall(
                libc::SYS_perf_event_open,
                &attr as *const Attr,
                child.id() as libc::pid_t,
                -1 as libc::c_int,
                -1 as libc::c_int,
                PERF_FLAG_FD_CLOEXEC,
            )
        };
        if fd < 0 {
            let e = errno();
            return Err(match e {
                libc::EACCES | libc::EPERM => CaptureError::Unavailable {
                    reason: format!(
                        "perf_event_open denied (errno {e}); needs CAP_PERFMON or perf_event_paranoid <= 1"
                    ),
                },
                libc::ENOENT | libc::ENODEV | libc::EOPNOTSUPP | libc::EINVAL => {
                    CaptureError::Unavailable {
                        reason: format!("the kernel rejected the trace event (errno {e})"),
                    }
                }
                _ => CaptureError::Os {
                    call: "perf_event_open",
                    errno: e,
                },
            });
        }
        // SAFETY: a new descriptor returned by the kernel, owned here.
        let fd = unsafe { OwnedFd::from_raw_fd(fd as libc::c_int) };
        // SAFETY: sysconf has no preconditions.
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
        let base_len = (1 + config.data_pages) * page;
        let aux_len = config.aux_pages * page;
        let base = map(&fd, base_len, 0, "mmap(perf data)")?;
        // SAFETY: the header page is mapped and these u64 fields lie inside it;
        // the kernel reads them at the AUX mmap below.
        unsafe {
            base.ptr
                .add(AUX_OFFSET)
                .cast::<u64>()
                .write_volatile(base_len as u64);
            base.ptr
                .add(AUX_SIZE)
                .cast::<u64>()
                .write_volatile(aux_len as u64);
        }
        let aux = map(&fd, aux_len, base_len, "mmap(perf aux)")?;

        // Release the child into `exec`, which enables the event.
        child
            .stdin
            .take()
            .ok_or_else(|| CaptureError::Child {
                detail: "child stdin missing".into(),
            })?
            .write_all(b"\n")
            .map_err(|e| CaptureError::Child {
                detail: e.to_string(),
            })?;
        // The AUX mapping is writable, so the kernel never overwrites unread
        // data: drain it (advancing aux_tail) while the child runs, and read
        // PERF_RECORD_AUX records to learn whether the kernel had to stop.
        let mut ring = Rings {
            base: &base,
            aux: &aux,
            aux_size: aux_len as u64,
            data_offset: page,
            data_size: (config.data_pages * page) as u64,
            bytes: Vec::new(),
            truncated: false,
        };
        let status = loop {
            ring.drain();
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(1)),
                Err(e) => {
                    return Err(CaptureError::Child {
                        detail: e.to_string(),
                    });
                }
            }
        };
        // SAFETY: an ioctl without argument on our perf fd.
        unsafe { libc::ioctl(fd.as_raw_fd(), PERF_EVENT_IOC_DISABLE, 0) };
        ring.drain();
        let (bytes, wrapped) = (std::mem::take(&mut ring.bytes), ring.truncated);
        drop(aux);
        drop(base);
        Ok(CapturedTrace {
            pmu: probe.pmu,
            aux: bytes,
            wrapped,
            exit_code: status.code(),
        })
    }
}
