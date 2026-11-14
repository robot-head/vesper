//! VM Generation ID device (Microsoft "Virtual Machine Generation ID", the
//! device Linux binds as `vmgenid`).
//!
//! The device exposes a 128-bit generation identifier at an 8-byte aligned
//! guest physical address that does not cross a page, and notifies the guest
//! after it changes (ACPI `Notify(…, 0x80)` or, under device tree,
//! `microsoft,vmgenid`'s interrupt). The host must write the new identifier
//! *before* signalling, and a restored or forked VM must never see the
//! identifier of a sibling. This model enforces both orderings: it refuses to
//! publish a generation it has already published, and [`VmGenIdDevice::regenerate`]
//! writes guest memory before it raises the notification.
//!
//! The identifier is the 16-byte little-endian `VmGenId` (`lo` then `hi`). The
//! ACPI `ADDR` package and a device-tree node description are produced as
//! plain values; no AML is assembled (the unikernel target boots without ACPI).
#![forbid(unsafe_code)]

use crate::PAGE_SIZE_4K;
use crate::fork::VmGenId;
use crate::guest_mem::{GuestMemory, MemError};
use std::collections::BTreeSet;

pub const VMGENID_BYTES: u64 = 16;
/// ACPI `_HID` and `_CID` of the device.
pub const ACPI_HID: &str = "VMGENCTR";
pub const ACPI_CID: &str = "VM_Gen_Counter";
/// Device-tree `compatible` string (Linux 6.10+).
pub const DT_COMPATIBLE: &str = "microsoft,vmgenid";
/// ACPI notification value for "generation changed".
pub const ACPI_NOTIFY_VALUE: u8 = 0x80;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VmGenIdError {
    /// The id must be 8-byte aligned and not cross a page (spec requirement).
    BadAddress,
    /// The all-zero id is reserved here as "never published".
    ZeroId,
    /// This generation was already published by this device.
    Reused,
    Memory(MemError),
}
impl From<MemError> for VmGenIdError {
    fn from(value: MemError) -> Self {
        Self::Memory(value)
    }
}

/// Device-tree node values for `microsoft,vmgenid`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VmGenIdDtNode {
    pub compatible: &'static str,
    pub reg_address: u64,
    pub reg_size: u64,
    pub interrupt: u32,
}

#[derive(Debug)]
pub struct VmGenIdDevice {
    gpa: u64,
    interrupt: u32,
    current: Option<VmGenId>,
    published: BTreeSet<VmGenId>,
    notify_pending: bool,
    notifications: u64,
}

impl VmGenIdDevice {
    pub fn new(gpa: u64, interrupt: u32) -> Result<Self, VmGenIdError> {
        let end_in_page = (gpa % PAGE_SIZE_4K)
            .checked_add(VMGENID_BYTES)
            .ok_or(VmGenIdError::BadAddress)?;
        if !gpa.is_multiple_of(8) || end_in_page > PAGE_SIZE_4K {
            return Err(VmGenIdError::BadAddress);
        }
        Ok(Self {
            gpa,
            interrupt,
            current: None,
            published: BTreeSet::new(),
            notify_pending: false,
            notifications: 0,
        })
    }
    pub fn encode(id: VmGenId) -> [u8; 16] {
        let mut b = [0u8; 16];
        b[..8].copy_from_slice(&id.lo.to_le_bytes());
        b[8..].copy_from_slice(&id.hi.to_le_bytes());
        b
    }
    pub fn current(&self) -> Option<VmGenId> {
        self.current
    }
    pub fn notifications(&self) -> u64 {
        self.notifications
    }
    /// Takes the pending notification (the VMM injects the interrupt).
    pub fn take_notification(&mut self) -> bool {
        std::mem::take(&mut self.notify_pending)
    }
    /// ACPI `ADDR` method result: low and high 32 bits of the id's address.
    pub fn acpi_addr_package(&self) -> [u32; 2] {
        [self.gpa as u32, (self.gpa >> 32) as u32]
    }
    pub fn device_tree_node(&self) -> VmGenIdDtNode {
        VmGenIdDtNode {
            compatible: DT_COMPATIBLE,
            reg_address: self.gpa,
            reg_size: VMGENID_BYTES,
            interrupt: self.interrupt,
        }
    }
    fn write(&mut self, memory: &mut GuestMemory, id: VmGenId) -> Result<(), VmGenIdError> {
        if id.lo == 0 && id.hi == 0 {
            return Err(VmGenIdError::ZeroId);
        }
        if self.published.contains(&id) {
            return Err(VmGenIdError::Reused);
        }
        memory.write(self.gpa, &Self::encode(id))?;
        self.published.insert(id);
        self.current = Some(id);
        Ok(())
    }
    /// Initial id at boot: written, no notification.
    pub fn boot(&mut self, memory: &mut GuestMemory, id: VmGenId) -> Result<(), VmGenIdError> {
        self.write(memory, id)
    }
    /// After a restore or fork: writes the new id, then raises the notification.
    pub fn regenerate(
        &mut self,
        memory: &mut GuestMemory,
        id: VmGenId,
    ) -> Result<(), VmGenIdError> {
        self.write(memory, id)?;
        self.notify_pending = true;
        self.notifications += 1;
        Ok(())
    }
    /// What the guest reads at the id address.
    pub fn read_guest(&self, memory: &GuestMemory) -> Result<[u8; 16], VmGenIdError> {
        let mut b = [0u8; 16];
        memory.read(self.gpa, &mut b)?;
        Ok(b)
    }
}
