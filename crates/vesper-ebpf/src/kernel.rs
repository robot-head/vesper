//! The one gated `unsafe` module: `BPF_MAP_CREATE` and `BPF_PROG_LOAD`
//! through the raw `bpf(2)` syscall, used as kernel load evidence.
//!
//! Every program is loaded with the kernel verifier log enabled so a
//! rejection carries the kernel's reason. Nothing here attaches a program
//! to an interface; loaded programs and maps are closed on drop.
//!
//! When the host denies `bpf()` (no `CAP_BPF`, seccomp, or no syscall),
//! [`load`] returns [`LoadError::Unavailable`] and callers report the
//! evidence as unavailable instead of failing.

#![allow(unsafe_code)]

use crate::insn::{RawInsn, pseudo};
use crate::program::{MapDef, Program};
use std::fmt;

const BPF_MAP_CREATE: libc::c_long = 0;
const BPF_MAP_LOOKUP_ELEM: libc::c_long = 1;
const BPF_PROG_LOAD: libc::c_long = 5;
const BPF_PROG_TEST_RUN: libc::c_long = 10;
const BPF_OBJ_GET_INFO_BY_FD: libc::c_long = 15;
const BPF_BTF_LOAD: libc::c_long = 18;
const ATTR_SIZE: usize = 144;
const LOG_SIZE: usize = 1 << 22;

/// An owned kernel file descriptor (map or program).
#[derive(Debug)]
pub struct Fd(i32);

impl Fd {
    pub fn raw(&self) -> i32 {
        self.0
    }
}

impl Drop for Fd {
    fn drop(&mut self) {
        // SAFETY: `self.0` is a descriptor this module obtained from bpf()
        // and nothing else closes it.
        unsafe {
            libc::close(self.0);
        }
    }
}

/// A program the kernel verifier accepted, with the maps it references.
#[derive(Debug)]
pub struct Loaded {
    pub prog: Fd,
    pub maps: Vec<Fd>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoadError {
    /// The host does not permit `bpf()`; `errno` says why.
    Unavailable { errno: i32 },
    /// The kernel refused to create a map.
    MapCreate { map: String, errno: i32 },
    /// The kernel verifier rejected the program.
    Rejected { errno: i32, log: String },
    /// The program is malformed for loading (bad map index, name).
    Invalid(String),
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoadError::Unavailable { errno } => write!(f, "bpf() unavailable (errno {errno})"),
            LoadError::MapCreate { map, errno } => write!(f, "map {map}: errno {errno}"),
            LoadError::Rejected { errno, log } => {
                write!(f, "kernel verifier rejected (errno {errno}):\n{log}")
            }
            LoadError::Invalid(m) => write!(f, "invalid program: {m}"),
        }
    }
}

impl std::error::Error for LoadError {}

fn put_u32(a: &mut [u8; ATTR_SIZE], at: usize, v: u32) {
    a[at..at + 4].copy_from_slice(&v.to_ne_bytes());
}

fn put_u64(a: &mut [u8; ATTR_SIZE], at: usize, v: u64) {
    a[at..at + 8].copy_from_slice(&v.to_ne_bytes());
}

fn put_name(a: &mut [u8; ATTR_SIZE], at: usize, name: &str) -> Result<(), LoadError> {
    let b = name.as_bytes();
    // BPF_OBJ_NAME_LEN is 16 including the terminating NUL, and the kernel
    // accepts only [A-Za-z0-9_.].
    if b.len() > 15
        || !b
            .iter()
            .all(|c| c.is_ascii_alphanumeric() || *c == b'_' || *c == b'.')
    {
        return Err(LoadError::Invalid(format!("object name {name:?}")));
    }
    a[at..at + b.len()].copy_from_slice(b);
    Ok(())
}

fn bpf(cmd: libc::c_long, attr: &mut [u8; ATTR_SIZE]) -> Result<i32, i32> {
    // SAFETY: `attr` is a valid, writable buffer of ATTR_SIZE bytes laid
    // out as the prefix of `union bpf_attr` for `cmd`; any pointers stored
    // in it point to live buffers owned by the caller for the whole call.
    let r = unsafe {
        libc::syscall(
            libc::SYS_bpf,
            cmd,
            attr.as_mut_ptr() as *mut libc::c_void,
            ATTR_SIZE as libc::c_uint,
        )
    };
    if r < 0 {
        Err(std::io::Error::last_os_error().raw_os_error().unwrap_or(0))
    } else {
        i32::try_from(r).map_err(|_| 0)
    }
}

fn unavailable(errno: i32) -> bool {
    errno == libc::EPERM || errno == libc::ENOSYS || errno == libc::EACCES
}

/// Create one map.
pub fn create_map(def: &MapDef) -> Result<Fd, LoadError> {
    let mut a = [0u8; ATTR_SIZE];
    put_u32(&mut a, 0, def.kind.kernel_type());
    put_u32(&mut a, 4, def.key_size);
    put_u32(&mut a, 8, def.value_size);
    put_u32(&mut a, 12, def.max_entries);
    put_u32(&mut a, 16, def.flags);
    put_name(&mut a, 28, &def.name)?;
    match bpf(BPF_MAP_CREATE, &mut a) {
        Ok(fd) => Ok(Fd(fd)),
        Err(e) if unavailable(e) => Err(LoadError::Unavailable { errno: e }),
        Err(e) => Err(LoadError::MapCreate {
            map: def.name.clone(),
            errno: e,
        }),
    }
}

/// Rewrite map indices in `LD_IMM64 BPF_PSEUDO_MAP_FD` to `fds`.
pub fn relocate(prog: &Program, fds: &[i32]) -> Result<Vec<RawInsn>, LoadError> {
    let mut insns = prog.insns.clone();
    for &(at, map) in &prog.map_relocs {
        let fd = *fds
            .get(map)
            .ok_or_else(|| LoadError::Invalid(format!("relocation to missing map {map}")))?;
        let slot = insns
            .get_mut(at)
            .ok_or_else(|| LoadError::Invalid(format!("relocation at missing slot {at}")))?;
        if slot.src != pseudo::MAP_FD {
            return Err(LoadError::Invalid(format!("slot {at} is not a map load")));
        }
        slot.imm = fd;
    }
    Ok(insns)
}

/// Create the program's maps, apply CO-RE relocations and kfunc ids
/// ([`prepare`]) and load the program.
pub fn load(prog: &Program) -> Result<Loaded, LoadError> {
    probe()?;
    load_unprobed(prog)
}

/// The host kernel's parsed BTF, read once.
fn vmlinux() -> Result<&'static crate::btf::Btf, LoadError> {
    static BTF: std::sync::OnceLock<Result<crate::btf::Btf, String>> = std::sync::OnceLock::new();
    BTF.get_or_init(|| crate::btf::host_vmlinux().map_err(|e| e.to_string()))
        .as_ref()
        .map_err(|e| LoadError::Invalid(e.clone()))
}

/// What a loader does before `BPF_PROG_LOAD`: CO-RE relocations and kfunc
/// BTF ids against the running kernel's BTF.
pub fn prepare(prog: &Program) -> Result<Program, LoadError> {
    if prog.core_relos.is_empty() && prog.kfuncs.is_empty() {
        return Ok(prog.clone());
    }
    let btf = vmlinux()?;
    let p = crate::core::apply(prog, btf).map_err(|e| LoadError::Invalid(e.to_string()))?;
    crate::btf::resolve_kfuncs(&p, btf).map_err(|e| LoadError::Invalid(e.to_string()))
}

/// Load `prog` with its program BTF, func info, line info and CO-RE
/// relocation records (`BPF_PROG_LOAD` `core_relos`): the kernel applies
/// the CO-RE relocations itself, against its own BTF. kfunc ids are still
/// resolved here, as a loader does.
pub fn load_with_ext(prog: &Program) -> Result<Loaded, LoadError> {
    probe()?;
    load_inner(prog, true)
}

fn load_unprobed(prog: &Program) -> Result<Loaded, LoadError> {
    load_inner(prog, false)
}

fn load_inner(prog: &Program, kernel_ext: bool) -> Result<Loaded, LoadError> {
    let prepared = if kernel_ext {
        if prog.kfuncs.is_empty() {
            prog.clone()
        } else {
            crate::btf::resolve_kfuncs(prog, vmlinux()?)
                .map_err(|e| LoadError::Invalid(e.to_string()))?
        }
    } else {
        prepare(prog)?
    };
    let prog = &prepared;
    let mut maps = Vec::new();
    for def in &prog.maps {
        maps.push(create_map(def)?);
    }
    let fds: Vec<i32> = maps.iter().map(Fd::raw).collect();
    let insns = relocate(prog, &fds)?;
    let bytes = crate::insn::slots_to_bytes(&insns);
    let mut license = prog.license.clone().into_bytes();
    license.push(0);
    let insn_cnt = u32::try_from(insns.len()).map_err(|_| LoadError::Invalid("too long".into()))?;
    // Programs that pass callbacks need program BTF with func_info.
    let has_callbacks = insns
        .iter()
        .any(|i| i.code == 0x18 && i.src == pseudo::FUNC);
    let (mut func_info, mut line_info, mut core_relos) = (Vec::new(), Vec::new(), Vec::new());
    let btf_fd = if has_callbacks || kernel_ext {
        let mut p = prog.clone();
        // kfunc ids are already in the instructions: no extern types
        p.kfuncs.clear();
        let ob = crate::btf_ext::build(&p, "prog").map_err(|e| LoadError::Invalid(e.to_string()))?;
        func_info = crate::btf::FuncInfo::bytes(&ob.func_info);
        for r in &ob.line_records {
            for w in [r[0] / 8, r[1], r[2], r[3]] {
                line_info.extend_from_slice(&w.to_le_bytes());
            }
        }
        for r in &ob.core_records {
            for w in r {
                core_relos.extend_from_slice(&w.to_le_bytes());
            }
        }
        Some(load_btf(&ob.btf)?)
    } else {
        None
    };
    let name: String = prog
        .name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .take(15)
        .collect();
    // First attempt without a log (a verbose log can overflow and fail an
    // otherwise accepted load with ENOSPC); on rejection, load again with
    // log_level 1 to collect the kernel's reason.
    let attempt = |log: Option<&mut Vec<u8>>| -> Result<Result<i32, i32>, LoadError> {
        let mut a = [0u8; ATTR_SIZE];
        put_u32(&mut a, 0, prog.prog_type.kernel_type());
        put_u32(&mut a, 4, insn_cnt);
        put_u64(&mut a, 8, bytes.as_ptr() as u64);
        put_u64(&mut a, 16, license.as_ptr() as u64);
        if let Some(log) = log {
            put_u32(&mut a, 24, 1);
            put_u32(&mut a, 28, log.len() as u32);
            put_u64(&mut a, 32, log.as_mut_ptr() as u64);
        }
        put_name(&mut a, 48, &name)?;
        if let Some(fd) = &btf_fd {
            put_u32(&mut a, 72, fd.raw() as u32);
            put_u32(&mut a, 76, crate::btf::FuncInfo::REC_SIZE);
            put_u64(&mut a, 80, func_info.as_ptr() as u64);
            put_u32(&mut a, 88, (func_info.len() / 8) as u32);
            if !line_info.is_empty() {
                put_u32(&mut a, 92, 16);
                put_u64(&mut a, 96, line_info.as_ptr() as u64);
                put_u32(&mut a, 104, (line_info.len() / 16) as u32);
            }
            if !core_relos.is_empty() {
                put_u32(&mut a, 116, (core_relos.len() / 16) as u32);
                put_u64(&mut a, 128, core_relos.as_ptr() as u64);
                put_u32(&mut a, 136, 16);
            }
        }
        Ok(bpf(BPF_PROG_LOAD, &mut a))
    };
    match attempt(None)? {
        Ok(fd) => Ok(Loaded { prog: Fd(fd), maps }),
        // EPERM: the host lets us create maps but not load XDP programs
        // (verifier rejections are EACCES, EINVAL, E2BIG, ...)
        Err(e) if e == libc::ENOSYS || e == libc::EPERM => Err(LoadError::Unavailable { errno: e }),
        Err(e) => {
            let mut log = vec![0u8; LOG_SIZE];
            let again = attempt(Some(&mut log))?;
            let end = log.iter().position(|&b| b == 0).unwrap_or(log.len());
            let text = String::from_utf8_lossy(&log[..end]).into_owned();
            match again {
                // accepted with a log after failing without one: report the
                // first failure anyway, it is not deterministic evidence
                Ok(fd) => {
                    drop(Fd(fd));
                    Err(LoadError::Rejected {
                        errno: e,
                        log: text,
                    })
                }
                Err(_) => Err(LoadError::Rejected {
                    errno: e,
                    log: text,
                }),
            }
        }
    }
}

/// Whether the host lets this process use `bpf()` for XDP programs:
/// creates a map and loads a trivial XDP program.
pub fn probe() -> Result<(), LoadError> {
    let def = MapDef::new("vesper_probe", crate::program::MapKind::Array, 4, 4, 1);
    create_map(&def)?;
    let mut asm = crate::program::Asm::new();
    asm.mov64(crate::insn::Reg::R0, 2);
    asm.exit();
    let prog = asm
        .finish("vesper_probe", crate::program::ProgramType::Xdp)
        .map_err(|e| LoadError::Invalid(e.to_string()))?;
    load_unprobed(&prog).map(|_| ())
}

/// Result of `BPF_PROG_TEST_RUN` on one packet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestRun {
    pub retval: u32,
    pub packet: Vec<u8>,
}

/// Run a loaded XDP program once over `packet` in the kernel
/// (`BPF_PROG_TEST_RUN`), on the calling CPU.
pub fn test_run(prog: &Fd, packet: &[u8]) -> Result<TestRun, LoadError> {
    let mut out = vec![0u8; packet.len() + 512];
    let mut a = [0u8; ATTR_SIZE];
    put_u32(&mut a, 0, prog.raw() as u32);
    put_u32(&mut a, 8, packet.len() as u32);
    put_u32(&mut a, 12, out.len() as u32);
    put_u64(&mut a, 16, packet.as_ptr() as u64);
    put_u64(&mut a, 24, out.as_mut_ptr() as u64);
    put_u32(&mut a, 32, 1);
    match bpf(BPF_PROG_TEST_RUN, &mut a) {
        Ok(_) => {
            let retval = u32::from_ne_bytes(a[4..8].try_into().expect("4 bytes"));
            let n = u32::from_ne_bytes(a[12..16].try_into().expect("4 bytes")) as usize;
            out.truncate(n.min(out.len()));
            Ok(TestRun {
                retval,
                packet: out,
            })
        }
        Err(e) if unavailable(e) => Err(LoadError::Unavailable { errno: e }),
        Err(e) => Err(LoadError::Invalid(format!("test run failed: errno {e}"))),
    }
}

/// Number of possible CPUs (the stride of per-CPU map values).
pub fn possible_cpus() -> Result<usize, LoadError> {
    let s = std::fs::read_to_string("/sys/devices/system/cpu/possible")
        .map_err(|e| LoadError::Invalid(format!("reading possible CPUs: {e}")))?;
    let mut n = 0usize;
    for part in s.trim().split(',') {
        let (lo, hi) = match part.split_once('-') {
            Some((a, b)) => (a.parse::<usize>(), b.parse::<usize>()),
            None => (part.parse::<usize>(), part.parse::<usize>()),
        };
        match (lo, hi) {
            (Ok(lo), Ok(hi)) if hi >= lo => n = n.max(hi + 1),
            _ => return Err(LoadError::Invalid(format!("bad CPU list {s:?}"))),
        }
    }
    Ok(n)
}

/// Key size, value size and type of a map, from `BPF_OBJ_GET_INFO_BY_FD`.
fn map_info(map: &Fd) -> Result<(u32, u32, u32), LoadError> {
    // struct bpf_map_info: type, id, key_size, value_size, ... (80 bytes
    // in current kernels; the kernel fills at most `info_len` bytes)
    let mut info = [0u8; 80];
    let mut a = [0u8; ATTR_SIZE];
    put_u32(&mut a, 0, map.raw() as u32);
    put_u32(&mut a, 4, info.len() as u32);
    put_u64(&mut a, 8, info.as_mut_ptr() as u64);
    bpf(BPF_OBJ_GET_INFO_BY_FD, &mut a)
        .map_err(|e| LoadError::Invalid(format!("map info failed: errno {e}")))?;
    let u = |at: usize| u32::from_ne_bytes(info[at..at + 4].try_into().expect("4 bytes"));
    Ok((u(0), u(8), u(12)))
}

/// `BPF_MAP_LOOKUP_ELEM`. The key and value buffer sizes come from the
/// kernel's own description of the map, so the syscall never reads or
/// writes past them: `key` must be exactly the map's key size, and the
/// returned value is `value_size` bytes, or for per-CPU maps the value
/// rounded up to 8 bytes times the number of possible CPUs. `None` when
/// the key is absent.
pub fn lookup(map: &Fd, key: &[u8]) -> Result<Option<Vec<u8>>, LoadError> {
    let (ty, key_size, value_size) = map_info(map)?;
    if key.len() != key_size as usize {
        return Err(LoadError::Invalid(format!(
            "key of {} bytes for a map with {key_size}-byte keys",
            key.len()
        )));
    }
    // BPF_MAP_TYPE_PERCPU_HASH, _PERCPU_ARRAY, _LRU_PERCPU_HASH,
    // _PERCPU_CGROUP_STORAGE
    let value_len = if matches!(ty, 5 | 6 | 10 | 21) {
        (value_size as usize).div_ceil(8) * 8 * possible_cpus()?
    } else {
        value_size as usize
    };
    let mut value = vec![0u8; value_len];
    let mut a = [0u8; ATTR_SIZE];
    put_u32(&mut a, 0, map.raw() as u32);
    put_u64(&mut a, 8, key.as_ptr() as u64);
    put_u64(&mut a, 16, value.as_mut_ptr() as u64);
    match bpf(BPF_MAP_LOOKUP_ELEM, &mut a) {
        Ok(_) => Ok(Some(value)),
        Err(e) if e == libc::ENOENT => Ok(None),
        Err(e) => Err(LoadError::Invalid(format!("map lookup failed: errno {e}"))),
    }
}

/// Pin the calling thread to one CPU so per-CPU map state and test runs
/// stay on it.
pub fn pin_to_cpu(cpu: usize) -> Result<(), LoadError> {
    // SAFETY: `set` is a plain bitmask initialised by CPU_ZERO before use,
    // and sched_setaffinity only reads it.
    let r = unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        libc::CPU_ZERO(&mut set);
        libc::CPU_SET(cpu, &mut set);
        libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &set)
    };
    if r == 0 {
        Ok(())
    } else {
        Err(LoadError::Invalid(format!(
            "sched_setaffinity: {}",
            std::io::Error::last_os_error()
        )))
    }
}

/// A kernel `BPF_MAP_TYPE_RINGBUF` mapped into this process: the consumer
/// page (read-write) and the producer page plus the data area, which the
/// kernel maps twice back to back (read-only).
#[derive(Debug)]
pub struct MappedRing {
    consumer: *mut u64,
    consumer_len: usize,
    producer: *const u8,
    producer_len: usize,
    page: usize,
    size: usize,
}

impl MappedRing {
    /// Map ring buffer `map` of `size` data bytes.
    pub fn new(map: &Fd, size: usize) -> Result<MappedRing, LoadError> {
        // SAFETY: sysconf has no preconditions.
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        let page = usize::try_from(page).map_err(|_| LoadError::Invalid("page size".into()))?;
        let producer_len = page
            .checked_add(
                size.checked_mul(2)
                    .ok_or_else(|| LoadError::Invalid("size".into()))?,
            )
            .ok_or_else(|| LoadError::Invalid("size".into()))?;
        // SAFETY: mapping a ringbuf fd with the offsets and protections
        // `kernel/bpf/ringbuf.c` documents; results are checked below.
        let (c, p) = unsafe {
            let c = libc::mmap(
                std::ptr::null_mut(),
                page,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                map.raw(),
                0,
            );
            let p = libc::mmap(
                std::ptr::null_mut(),
                producer_len,
                libc::PROT_READ,
                libc::MAP_SHARED,
                map.raw(),
                page as libc::off_t,
            );
            (c, p)
        };
        if c == libc::MAP_FAILED || p == libc::MAP_FAILED {
            let e = std::io::Error::last_os_error();
            // SAFETY: unmapping only the regions that were mapped.
            unsafe {
                if c != libc::MAP_FAILED {
                    libc::munmap(c, page);
                }
                if p != libc::MAP_FAILED {
                    libc::munmap(p, producer_len);
                }
            }
            return Err(LoadError::Invalid(format!("ringbuf mmap: {e}")));
        }
        Ok(MappedRing {
            consumer: c as *mut u64,
            consumer_len: page,
            producer: p as *const u8,
            producer_len,
            page,
            size,
        })
    }

    /// Copy out the data area and the positions as
    /// `(data, consumer, producer)`.
    pub fn snapshot(&self) -> (Vec<u8>, u64, u64) {
        // SAFETY: both pointers come from successful mmaps of at least the
        // lengths read; positions are read with volatile loads because the
        // kernel updates them concurrently.
        unsafe {
            let cons = std::ptr::read_volatile(self.consumer);
            let prod = std::ptr::read_volatile(self.producer as *const u64);
            let data = std::slice::from_raw_parts(self.producer.add(self.page), self.size).to_vec();
            (data, cons, prod)
        }
    }

    /// Publish a new consumer position (after reading records).
    pub fn set_consumer(&mut self, pos: u64) {
        // SAFETY: the consumer page is mapped read-write.
        unsafe { std::ptr::write_volatile(self.consumer, pos) }
    }
}

impl Drop for MappedRing {
    fn drop(&mut self) {
        // SAFETY: unmapping exactly the two regions mapped in `new`.
        unsafe {
            libc::munmap(self.consumer as *mut libc::c_void, self.consumer_len);
            libc::munmap(self.producer as *mut libc::c_void, self.producer_len);
        }
    }
}

/// `BPF_BTF_LOAD`: have the kernel validate a BTF blob.
pub fn load_btf(btf: &[u8]) -> Result<Fd, LoadError> {
    let mut log = vec![0u8; 1 << 16];
    let mut a = [0u8; ATTR_SIZE];
    put_u64(&mut a, 0, btf.as_ptr() as u64);
    put_u64(&mut a, 8, log.as_mut_ptr() as u64);
    put_u32(&mut a, 16, btf.len() as u32);
    put_u32(&mut a, 20, log.len() as u32);
    put_u32(&mut a, 24, 1);
    match bpf(BPF_BTF_LOAD, &mut a) {
        Ok(fd) => Ok(Fd(fd)),
        Err(e) if unavailable(e) => Err(LoadError::Unavailable { errno: e }),
        Err(e) => {
            let end = log.iter().position(|&b| b == 0).unwrap_or(log.len());
            Err(LoadError::Rejected {
                errno: e,
                log: String::from_utf8_lossy(&log[..end]).into_owned(),
            })
        }
    }
}
