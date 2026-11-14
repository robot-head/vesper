//! Host-only snapshot metadata validation and owned page resolution (§37).
//!
//! HVA interval checks do not prove physical/EPT isolation. [`SnapshotMap`]
//! actions are plans; [`slot_plan`] turns them into verified KVM memory-slot
//! plans, [`fork`] models copy-on-write snapshot forks with per-clone nonce
//! reseeding, and [`host`] executes userfaultfd lazy restore and a gated KVM
//! guest where the kernel allows it (reporting `Unavailable` otherwise).
//! Wave seven adds zero-copy `UFFDIO_CONTINUE` over a sealed shared memfd and
//! hot-set prefetch ([`hotset`]), fork clones that remap the snapshot
//! `MAP_PRIVATE` (`mapped_fork`, Linux) with hardware-plus-OS clone entropy
//! ([`entropy`]), the VirtIO MMIO block/net/vsock and VMGENID device models
//! ([`devices`]), PVH direct boot ([`pvh`]) and the VMM seccomp-bpf profile
//! ([`seccomp`]).
//! Immutable output copies remove writable file aliases after verification; CAS
//! filesystem permissions are not a defence against a privileged concurrent writer.
#![deny(unsafe_code)]

pub mod devices;
pub mod entropy;
pub mod fork;
pub mod guest_mem;
pub mod host;
pub mod hotset;
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
mod hw_rng;
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
mod hw_rng {
    pub fn instruction() -> Option<&'static str> {
        None
    }
    pub fn word() -> Option<u64> {
        None
    }
}
#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    any(feature = "uffd", feature = "kvm", feature = "seccomp")
))]
mod linux_host;
#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    feature = "uffd"
))]
pub mod mapped_fork;
pub mod page_source;
pub mod pvh;
pub mod seccomp;
pub mod slot_plan;
pub mod virtio;

use std::fmt;
use std::io::{self, Read};
use vesper_loom_cas::{Domain, ObjectId, ObjectKind, Store, blob_id};

pub const PAGE_SIZE_4K: u64 = 4096;
pub const PAGE_BYTES: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Budget,
    EmptyLayout,
    EmptyRange,
    Unaligned,
    Overflow,
    DuplicateSlot,
    GuestOverlap,
    HostOverlap,
    IncompleteMappings,
    InvalidIdentity,
    InvalidPageRange,
    PermissionMismatch,
    UnknownFault,
    UnsafeObject,
    WrongLength,
    CorruptObject,
    UnsupportedPlatform,
    /// A forked clone's memory was requested before `reseed_after_fork`.
    NotReseeded,
    Io(io::ErrorKind),
}
impl From<io::Error> for Error {
    fn from(value: io::Error) -> Self {
        Self::Io(value.kind())
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Error {}

/// Exclusive endpoint must itself fit u64; a range ending at 2^64 is rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageRange {
    start: u64,
    end: u64,
}
impl PageRange {
    pub fn new(start: u64, len: u64) -> Result<Self, Error> {
        if len == 0 {
            return Err(Error::EmptyRange);
        }
        if !start.is_multiple_of(PAGE_SIZE_4K) || !len.is_multiple_of(PAGE_SIZE_4K) {
            return Err(Error::Unaligned);
        }
        Ok(Self {
            start,
            end: start.checked_add(len).ok_or(Error::Overflow)?,
        })
    }
    pub fn start(self) -> u64 {
        self.start
    }
    pub fn end(self) -> u64 {
        self.end
    }
    pub fn byte_len(self) -> u64 {
        self.end - self.start
    }
    pub fn contains(self, address: u64) -> bool {
        self.start <= address && address < self.end
    }
    pub fn overlaps(self, other: Self) -> bool {
        self.start < other.end && other.start < self.end
    }
}

/// Explicit limits are checked before cloning metadata or reading any CAS file.
/// Hard ceilings: 4096 slots, 1M pages, 4GiB guest bytes, 16MiB/blob and
/// 8,388,608 pair comparisons. Each resolution reads at most one bounded blob.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub slots: usize,
    pub pages: usize,
    pub guest_bytes: u64,
    pub blob_bytes: u64,
    pub pair_checks: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            slots: 256,
            pages: 65536,
            guest_bytes: 256 * 1024 * 1024,
            blob_bytes: 16 * 1024 * 1024,
            pair_checks: 32768,
        }
    }
}
impl Limits {
    fn validate(self) -> Result<(), Error> {
        if self.slots == 0
            || self.slots > 4096
            || self.pages == 0
            || self.pages > 1_048_576
            || self.guest_bytes == 0
            || self.guest_bytes > 4 * 1024 * 1024 * 1024
            || self.blob_bytes < PAGE_SIZE_4K
            || self.blob_bytes > 16 * 1024 * 1024
            || self.pair_checks > 8_388_608
        {
            return Err(Error::Budget);
        }
        Ok(())
    }
}

/// Whole-object length and aligned page offset are part of immutable identity.
/// Only artifact-domain blobs can back guest snapshot pages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CasPage {
    pub object: ObjectId,
    pub object_len: u64,
    pub offset: u64,
}
impl CasPage {
    fn validate(self, limit: u64) -> Result<(), Error> {
        validate_blob(self.object, self.object_len, limit)?;
        let range = PageRange::new(self.offset, PAGE_SIZE_4K)?;
        if range.end > self.object_len {
            return Err(Error::InvalidPageRange);
        }
        Ok(())
    }
}
fn validate_blob(object: ObjectId, len: u64, limit: u64) -> Result<(), Error> {
    if object.domain != Domain::Artifact || object.kind != ObjectKind::Blob {
        return Err(Error::InvalidIdentity);
    }
    if len == 0 || !len.is_multiple_of(PAGE_SIZE_4K) {
        return Err(Error::InvalidPageRange);
    }
    if len > limit {
        return Err(Error::Budget);
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Permission {
    Private,
    /// Exact slot content identity; every mapped page must agree with this span.
    ReadOnlyCas {
        object: ObjectId,
        object_len: u64,
        offset: u64,
    },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemorySlot {
    pub vm_id: u64,
    pub slot_id: u32,
    pub guest_start: u64,
    /// Host virtual address, never a host physical address assertion.
    pub host_virtual_start: u64,
    pub byte_len: u64,
    pub permission: Permission,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageMapping {
    Zero,
    ReadOnlyCas(CasPage),
    PrivateCow(CasPage),
}

/// Complete per-slot metadata replaces an ambiguous optional dirty bitmap.
/// Exactly byte_len/4096 entries are required; holes have explicit Zero entries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlotSnapshot {
    pub slot: MemorySlot,
    pub pages: Vec<PageMapping>,
}
#[derive(Debug)]
struct ValidatedSlot {
    slot: MemorySlot,
    guest: PageRange,
    host: PageRange,
    pages: Vec<PageMapping>,
}

/// Construction is the only way to create a resolvable map. No mutable metadata
/// access or transferable slot/page handles are exposed; rebuilding cannot leave
/// an old handle usable with a new map. No VM memory is mutated by this API.
#[derive(Debug)]
pub struct SnapshotMap {
    slots: Vec<ValidatedSlot>,
    limits: Limits,
}
impl SnapshotMap {
    pub fn validate(input: &[SlotSnapshot], limits: Limits) -> Result<Self, Error> {
        limits.validate()?;
        if input.is_empty() {
            return Err(Error::EmptyLayout);
        }
        if input.len() > limits.slots {
            return Err(Error::Budget);
        }
        let checks = input
            .len()
            .checked_mul(input.len() - 1)
            .ok_or(Error::Overflow)?
            / 2;
        if checks > limits.pair_checks {
            return Err(Error::Budget);
        }
        // First pass validates all sizes before copying any page vectors.
        let mut total_pages = 0usize;
        let mut total_bytes = 0u64;
        for entry in input {
            let s = entry.slot;
            PageRange::new(s.guest_start, s.byte_len)?;
            PageRange::new(s.host_virtual_start, s.byte_len)?;
            total_bytes = total_bytes.checked_add(s.byte_len).ok_or(Error::Overflow)?;
            let count = usize::try_from(s.byte_len / PAGE_SIZE_4K).map_err(|_| Error::Budget)?;
            total_pages = total_pages.checked_add(count).ok_or(Error::Overflow)?;
            if total_pages > limits.pages || total_bytes > limits.guest_bytes {
                return Err(Error::Budget);
            }
            if entry.pages.len() != count {
                return Err(Error::IncompleteMappings);
            }
            if let Permission::ReadOnlyCas {
                object,
                object_len,
                offset,
            } = s.permission
            {
                validate_blob(object, object_len, limits.blob_bytes)?;
                if PageRange::new(offset, s.byte_len)?.end > object_len {
                    return Err(Error::InvalidPageRange);
                }
            }
            for (index, mapping) in entry.pages.iter().enumerate() {
                if let PageMapping::ReadOnlyCas(p) | PageMapping::PrivateCow(p) = mapping {
                    p.validate(limits.blob_bytes)?;
                }
                match (s.permission, mapping) {
                    (Permission::Private, PageMapping::Zero | PageMapping::PrivateCow(_)) => {}
                    (
                        Permission::ReadOnlyCas {
                            object,
                            object_len,
                            offset,
                        },
                        PageMapping::ReadOnlyCas(p),
                    ) => {
                        let delta = u64::try_from(index)
                            .map_err(|_| Error::Overflow)?
                            .checked_mul(PAGE_SIZE_4K)
                            .ok_or(Error::Overflow)?;
                        if p.object != object
                            || p.object_len != object_len
                            || p.offset != offset.checked_add(delta).ok_or(Error::Overflow)?
                        {
                            return Err(Error::PermissionMismatch);
                        }
                    }
                    _ => return Err(Error::PermissionMismatch),
                }
            }
        }
        let mut slots: Vec<ValidatedSlot> = Vec::with_capacity(input.len());
        for entry in input {
            let s = entry.slot;
            let guest = PageRange::new(s.guest_start, s.byte_len)?;
            let host = PageRange::new(s.host_virtual_start, s.byte_len)?;
            for other in &slots {
                if s.vm_id == other.slot.vm_id {
                    if s.slot_id == other.slot.slot_id {
                        return Err(Error::DuplicateSlot);
                    }
                    if guest.overlaps(other.guest) {
                        return Err(Error::GuestOverlap);
                    }
                }
                if host.overlaps(other.host) {
                    // Conservative: partial overlaps are refused even when content
                    // happens to match. A bool or merely equal digest is insufficient.
                    if host != other.host
                        || !matches!(s.permission, Permission::ReadOnlyCas { .. })
                        || s.permission != other.slot.permission
                    {
                        return Err(Error::HostOverlap);
                    }
                }
            }
            slots.push(ValidatedSlot {
                slot: s,
                guest,
                host,
                pages: entry.pages.clone(),
            });
        }
        Ok(Self { slots, limits })
    }

    pub fn slot_count(&self) -> usize {
        self.slots.len()
    }

    /// Faults may be byte addresses; output identifies the enclosing 4KiB page.
    /// Unknown VM, inter-slot hole and exclusive-end faults fail explicitly.
    pub fn resolve(
        &self,
        store: &Store,
        vm_id: u64,
        fault_gpa: u64,
    ) -> Result<ResolvedPage, Error> {
        let slot = self
            .slots
            .iter()
            .find(|s| s.slot.vm_id == vm_id && s.guest.contains(fault_gpa))
            .ok_or(Error::UnknownFault)?;
        let index = usize::try_from((fault_gpa - slot.guest.start) / PAGE_SIZE_4K)
            .map_err(|_| Error::Overflow)?;
        let page_gpa = fault_gpa - fault_gpa % PAGE_SIZE_4K;
        let (action, bytes) = match slot.pages.get(index).ok_or(Error::IncompleteMappings)? {
            PageMapping::Zero => (UffdPageAction::ZeroPage4K, Box::new([0; PAGE_BYTES])),
            PageMapping::ReadOnlyCas(p) => (
                UffdPageAction::ContinueSharedCas { identity: *p },
                read_page(store, *p, self.limits.blob_bytes)?,
            ),
            PageMapping::PrivateCow(p) => (
                UffdPageAction::CopyPrivateCow { identity: *p },
                read_page(store, *p, self.limits.blob_bytes)?,
            ),
        };
        Ok(ResolvedPage {
            vm_id,
            slot_id: slot.slot.slot_id,
            page_gpa,
            action,
            bytes,
        })
    }
}

/// Proposed operations only. ContinueSharedCas needs a future verified immutable
/// memfd backing/registration; these owned bytes are not such a registration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UffdPageAction {
    ZeroPage4K,
    ContinueSharedCas { identity: CasPage },
    CopyPrivateCow { identity: CasPage },
}
#[derive(Debug)]
pub struct ResolvedPage {
    vm_id: u64,
    slot_id: u32,
    page_gpa: u64,
    action: UffdPageAction,
    bytes: Box<[u8; PAGE_BYTES]>,
}
impl ResolvedPage {
    pub fn vm_id(&self) -> u64 {
        self.vm_id
    }
    pub fn slot_id(&self) -> u32 {
        self.slot_id
    }
    pub fn page_gpa(&self) -> u64 {
        self.page_gpa
    }
    pub fn action(&self) -> UffdPageAction {
        self.action
    }
    pub fn bytes(&self) -> &[u8; PAGE_BYTES] {
        &self.bytes
    }
    pub fn into_bytes(self) -> Box<[u8; PAGE_BYTES]> {
        self.bytes
    }
}

#[cfg(any(unix, windows))]
fn read_page(store: &Store, page: CasPage, limit: u64) -> Result<Box<[u8; PAGE_BYTES]>, Error> {
    use std::fs::OpenOptions;
    page.validate(limit)?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // Open reparse points themselves. Deny concurrent write/delete handles;
        // an already open writable alias causes the open to fail as well.
        options.custom_flags(0x0020_0000).share_mode(0x0000_0001);
    }
    let mut file = options.open(store.object_path(&page.object))?;
    let metadata = file.metadata()?;
    if !safe_metadata(&metadata) {
        return Err(Error::UnsafeObject);
    }
    if metadata.len() != page.object_len {
        return Err(Error::WrongLength);
    }
    let size = usize::try_from(page.object_len).map_err(|_| Error::Budget)?;
    // Exact owned buffer, bounded by validated blob_bytes. Hash THIS buffer,
    // then copy the requested page from it; never verify and reopen a pathname.
    let mut bytes = vec![0u8; size];
    file.read_exact(&mut bytes)?;
    let mut trailing = [0u8; 1];
    if file.read(&mut trailing)? != 0 {
        return Err(Error::WrongLength);
    }
    if blob_id(Domain::Artifact, &bytes) != page.object {
        return Err(Error::CorruptObject);
    }
    let after = file.metadata()?;
    if !safe_metadata(&after) || after.len() != page.object_len {
        return Err(Error::UnsafeObject);
    }
    let offset = usize::try_from(page.offset).map_err(|_| Error::Overflow)?;
    let end = offset.checked_add(PAGE_BYTES).ok_or(Error::Overflow)?;
    let slice = bytes.get(offset..end).ok_or(Error::InvalidPageRange)?;
    let mut output = Box::new([0u8; PAGE_BYTES]);
    output.copy_from_slice(slice);
    Ok(output)
}
#[cfg(any(unix, windows))]
fn safe_metadata(metadata: &std::fs::Metadata) -> bool {
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || !metadata.permissions().readonly()
    {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // Conservative Unix alias policy; owned bytes still provide integrity
        // against mutation by privileged writers or pre-existing descriptors.
        metadata.mode() & 0o222 == 0 && metadata.nlink() == 1
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        // Reject every reparse-point kind, including non-symlink redirections.
        // share_mode above excludes writable aliases while the handle lives.
        metadata.file_attributes() & 0x0000_0400 == 0
    }
}
#[cfg(not(any(unix, windows)))]
fn read_page(_store: &Store, _page: CasPage, _limit: u64) -> Result<Box<[u8; PAGE_BYTES]>, Error> {
    Err(Error::UnsupportedPlatform)
}
