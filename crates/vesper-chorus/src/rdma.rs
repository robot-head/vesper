//! Software model of one-sided RDMA writes with linear leases (§25.1 item 3,
//! §25.2 `InFlightRdmaWrite`, `RdmaQp`).
//!
//! An [`RdmaFabric`] holds simulated RNICs, one per node. Registering a
//! buffer yields a linear [`RdmaRegion`]; its copyable [`LocalKey`] and
//! [`RemoteSlotGrant`] name it in work requests the way an lkey and an rkey
//! do. Posting a write returns a linear [`InFlightRdmaWrite`] lease. While
//! any lease on a region is outstanding the host cannot mutate the region
//! ([`RdmaError::RegionBusy`]) or deregister it ([`RdmaError::InFlight`]).
//! The simulated RNIC copies the bytes when [`RdmaFabric::progress`] runs,
//! which is after the post, so a host write between post and completion
//! would corrupt the transfer on real hardware; here it is rejected.
//!
//! Keys carry a generation: a stale `LocalKey` is
//! [`RdmaError::UseAfterDeregister`] at post time, and a write to a
//! deregistered (or re-registered) remote region completes with
//! [`WcStatus::RemoteInvalidKey`]. Keys are identifiers, not secrets.
//!
//! No real RDMA NIC, ibverbs, RoCE or CXL device is used; this validates the
//! modelled ownership protocol only.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};

/// Access rights of a registered region.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Access {
    pub remote_write: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WcStatus {
    Success,
    /// Remote key unknown: region deregistered or never registered.
    RemoteInvalidKey,
    /// Remote region does not permit writes, or the range is out of bounds.
    RemoteAccess,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RdmaError {
    UnknownNode(u32),
    /// The local key names a region that was deregistered.
    UseAfterDeregister,
    /// Host access to a region while a write from it is in flight.
    RegionBusy {
        in_flight: u32,
    },
    /// Deregistration while writes are in flight; the region is returned.
    InFlight {
        in_flight: u32,
    },
    OutOfBounds {
        offset: u64,
        len: u64,
        region_len: u64,
    },
    TooManyRegions,
    /// The lease belongs to another fabric.
    ForeignLease,
    /// The region, key or grant belongs to another fabric.
    ForeignHandle,
    /// The work request completed with an error status.
    Completion(WcStatus),
}

impl std::fmt::Display for RdmaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for RdmaError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalKey {
    fabric: u64,
    node: u32,
    slot: u32,
    generation: u32,
}

/// What a remote peer needs to write into a region.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RemoteSlotGrant {
    /// Fabric the region is registered on.
    pub fabric: u64,
    pub node: u32,
    pub rkey: u32,
    pub len: u64,
}

/// Completion seen by the writer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorkCompletion {
    pub wr_id: u64,
    pub status: WcStatus,
    pub bytes: u64,
}

/// Completion seen by the target of a `RDMA_WRITE_WITH_IMM`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecvCompletion {
    pub from: u32,
    pub imm: u32,
    pub bytes: u64,
}

struct Region {
    generation: u32,
    buf: Vec<u8>,
    access: Access,
    in_flight: u32,
}

struct PendingWr {
    wr_id: u64,
    src: LocalKey,
    offset: u64,
    len: u64,
    remote: RemoteSlotGrant,
    remote_offset: u64,
    imm: u32,
}

#[derive(Default)]
struct Rnic {
    regions: BTreeMap<u32, Region>,
    next_slot: u32,
    generation: u32,
    send_queue: VecDeque<PendingWr>,
    cq: BTreeMap<u64, WorkCompletion>,
    recv_cq: VecDeque<RecvCompletion>,
}

struct Fabric {
    id: u64,
    nics: Vec<Rnic>,
    next_wr: u64,
    leaked_leases: u64,
    leaked_regions: u64,
    /// Source region of every posted, not yet consumed work request.
    pending_src: BTreeMap<u64, LocalKey>,
}

static NEXT_FABRIC: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

pub const MAX_REGIONS: u32 = 1 << 15;

fn rkey(slot: u32, generation: u32) -> u32 {
    (slot << 16) | (generation & 0xffff)
}

/// Shared handle to the simulated fabric.
#[derive(Clone)]
pub struct RdmaFabric {
    inner: Arc<Mutex<Fabric>>,
}

/// A registered memory region. Linear: exits only through
/// [`RdmaFabric::deregister`].
#[must_use = "a registered region must be deregistered"]
pub struct RdmaRegion {
    key: LocalKey,
    len: u64,
    fabric: Option<RdmaFabric>,
}

impl RdmaRegion {
    pub fn local_key(&self) -> LocalKey {
        self.key
    }
    pub fn grant(&self) -> RemoteSlotGrant {
        RemoteSlotGrant {
            fabric: self.key.fabric,
            node: self.key.node,
            rkey: rkey(self.key.slot, self.key.generation),
            len: self.len,
        }
    }
    pub fn len(&self) -> u64 {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl std::fmt::Debug for RdmaRegion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RdmaRegion")
            .field("key", &self.key)
            .field("len", &self.len)
            .finish()
    }
}

impl Drop for RdmaRegion {
    fn drop(&mut self) {
        if let Some(f) = self.fabric.take() {
            f.lock().leaked_regions += 1;
        }
    }
}

/// Lease on an in-flight one-sided write. The source region stays pinned
/// and immutable until the lease is consumed by [`RdmaFabric::await_wc`] or
/// a successful [`RdmaFabric::poll_wc`].
#[must_use = "an in-flight RDMA write must be awaited"]
pub struct InFlightRdmaWrite {
    fabric_id: u64,
    node: u32,
    wr_id: u64,
    fabric: Option<RdmaFabric>,
}

impl InFlightRdmaWrite {
    pub fn wr_id(&self) -> u64 {
        self.wr_id
    }
}

impl std::fmt::Debug for InFlightRdmaWrite {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InFlightRdmaWrite")
            .field("node", &self.node)
            .field("wr_id", &self.wr_id)
            .finish()
    }
}

impl Drop for InFlightRdmaWrite {
    fn drop(&mut self) {
        if let Some(f) = self.fabric.take() {
            // The region stays pinned forever: that is the detected leak.
            f.lock().leaked_leases += 1;
        }
    }
}

impl RdmaFabric {
    pub fn new(nodes: u32) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Fabric {
                id: NEXT_FABRIC.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
                nics: (0..nodes).map(|_| Rnic::default()).collect(),
                next_wr: 1,
                leaked_leases: 0,
                leaked_regions: 0,
                pending_src: BTreeMap::new(),
            })),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Fabric> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Register `buf` on `node`'s RNIC.
    pub fn register(
        &self,
        node: u32,
        buf: Vec<u8>,
        access: Access,
    ) -> Result<RdmaRegion, RdmaError> {
        let mut g = self.lock();
        let fabric = g.id;
        let nic = g
            .nics
            .get_mut(node as usize)
            .ok_or(RdmaError::UnknownNode(node))?;
        if nic.regions.len() as u32 >= MAX_REGIONS {
            return Err(RdmaError::TooManyRegions);
        }
        let slot = (0..MAX_REGIONS)
            .map(|i| (nic.next_slot + i) % MAX_REGIONS)
            .find(|s| !nic.regions.contains_key(s))
            .ok_or(RdmaError::TooManyRegions)?;
        nic.next_slot = (slot + 1) % MAX_REGIONS;
        nic.generation = nic.generation.wrapping_add(1);
        let generation = nic.generation;
        let len = buf.len() as u64;
        nic.regions.insert(
            slot,
            Region {
                generation,
                buf,
                access,
                in_flight: 0,
            },
        );
        Ok(RdmaRegion {
            key: LocalKey {
                fabric,
                node,
                slot,
                generation,
            },
            len,
            fabric: Some(self.clone()),
        })
    }

    /// Deregister and return the host buffer. Fails, returning the region,
    /// while a write from it is in flight.
    pub fn deregister(&self, mut region: RdmaRegion) -> Result<Vec<u8>, (RdmaError, RdmaRegion)> {
        let mut g = self.lock();
        let k = region.key;
        if k.fabric != g.id {
            drop(g);
            return Err((RdmaError::ForeignHandle, region));
        }
        let Some(nic) = g.nics.get_mut(k.node as usize) else {
            drop(g);
            return Err((RdmaError::UnknownNode(k.node), region));
        };
        match nic.regions.get(&k.slot) {
            Some(r) if r.generation == k.generation && r.in_flight > 0 => {
                let n = r.in_flight;
                drop(g);
                return Err((RdmaError::InFlight { in_flight: n }, region));
            }
            Some(r) if r.generation == k.generation => {}
            _ => {
                drop(g);
                return Err((RdmaError::UseAfterDeregister, region));
            }
        }
        let r = nic.regions.remove(&k.slot).expect("checked");
        region.fabric = None;
        Ok(r.buf)
    }

    fn region(g: &mut Fabric, k: LocalKey) -> Result<&mut Region, RdmaError> {
        if k.fabric != g.id {
            return Err(RdmaError::ForeignHandle);
        }
        let nic = g
            .nics
            .get_mut(k.node as usize)
            .ok_or(RdmaError::UnknownNode(k.node))?;
        match nic.regions.get_mut(&k.slot) {
            Some(r) if r.generation == k.generation => Ok(r),
            _ => Err(RdmaError::UseAfterDeregister),
        }
    }

    /// Host write into a region (for example filling the next batch).
    pub fn write_local(
        &self,
        region: &RdmaRegion,
        offset: u64,
        data: &[u8],
    ) -> Result<(), RdmaError> {
        let mut g = self.lock();
        let r = Self::region(&mut g, region.key)?;
        if r.in_flight > 0 {
            return Err(RdmaError::RegionBusy {
                in_flight: r.in_flight,
            });
        }
        let end = offset
            .checked_add(data.len() as u64)
            .filter(|e| *e <= r.buf.len() as u64)
            .ok_or(RdmaError::OutOfBounds {
                offset,
                len: data.len() as u64,
                region_len: r.buf.len() as u64,
            })?;
        r.buf[offset as usize..end as usize].copy_from_slice(data);
        Ok(())
    }

    /// Host read of a region (always allowed: the RNIC only reads too).
    pub fn read_local(&self, region: &RdmaRegion) -> Result<Vec<u8>, RdmaError> {
        let mut g = self.lock();
        Ok(Self::region(&mut g, region.key)?.buf.clone())
    }

    /// Post `RDMA_WRITE_WITH_IMM` of `src[offset..offset+len]` into
    /// `remote[remote_offset..]`.
    #[allow(clippy::too_many_arguments)]
    pub fn post_write_with_imm(
        &self,
        src: LocalKey,
        offset: u64,
        len: u64,
        remote: RemoteSlotGrant,
        remote_offset: u64,
        imm: u32,
    ) -> Result<InFlightRdmaWrite, RdmaError> {
        let mut g = self.lock();
        if remote.fabric != g.id {
            return Err(RdmaError::ForeignHandle);
        }
        if remote.node as usize >= g.nics.len() {
            return Err(RdmaError::UnknownNode(remote.node));
        }
        let r = Self::region(&mut g, src)?;
        let region_len = r.buf.len() as u64;
        if offset.checked_add(len).is_none_or(|e| e > region_len) {
            return Err(RdmaError::OutOfBounds {
                offset,
                len,
                region_len,
            });
        }
        r.in_flight = r.in_flight.checked_add(1).ok_or(RdmaError::RegionBusy {
            in_flight: u32::MAX,
        })?;
        let wr_id = g.next_wr;
        g.next_wr += 1;
        let fabric_id = g.id;
        g.pending_src.insert(wr_id, src);
        g.nics[src.node as usize].send_queue.push_back(PendingWr {
            wr_id,
            src,
            offset,
            len,
            remote,
            remote_offset,
            imm,
        });
        Ok(InFlightRdmaWrite {
            fabric_id,
            node: src.node,
            wr_id,
            fabric: Some(self.clone()),
        })
    }

    /// Let every RNIC execute up to `max` queued work requests. Returns how
    /// many ran.
    pub fn progress(&self, max: usize) -> usize {
        let mut g = self.lock();
        let mut done = 0;
        for node in 0..g.nics.len() {
            while done < max {
                let Some(wr) = g.nics[node].send_queue.pop_front() else {
                    break;
                };
                done += 1;
                // DMA read of the source happens now, not at post time.
                let data = {
                    let r = &g.nics[node].regions[&wr.src.slot];
                    r.buf[wr.offset as usize..(wr.offset + wr.len) as usize].to_vec()
                };
                let status = Self::place(&mut g, &wr, &data);
                if status == WcStatus::Success {
                    g.nics[wr.remote.node as usize]
                        .recv_cq
                        .push_back(RecvCompletion {
                            from: node as u32,
                            imm: wr.imm,
                            bytes: wr.len,
                        });
                }
                g.nics[node].cq.insert(
                    wr.wr_id,
                    WorkCompletion {
                        wr_id: wr.wr_id,
                        status,
                        bytes: if status == WcStatus::Success {
                            wr.len
                        } else {
                            0
                        },
                    },
                );
            }
        }
        done
    }

    fn place(g: &mut Fabric, wr: &PendingWr, data: &[u8]) -> WcStatus {
        let slot = wr.remote.rkey >> 16;
        let generation = wr.remote.rkey & 0xffff;
        let nic = &mut g.nics[wr.remote.node as usize];
        let Some(r) = nic.regions.get_mut(&slot) else {
            return WcStatus::RemoteInvalidKey;
        };
        if r.generation & 0xffff != generation {
            return WcStatus::RemoteInvalidKey;
        }
        if !r.access.remote_write {
            return WcStatus::RemoteAccess;
        }
        let Some(end) = wr
            .remote_offset
            .checked_add(wr.len)
            .filter(|e| *e <= r.buf.len() as u64)
        else {
            return WcStatus::RemoteAccess;
        };
        r.buf[wr.remote_offset as usize..end as usize].copy_from_slice(data);
        WcStatus::Success
    }

    /// Non-blocking completion check: returns the lease if the write has not
    /// completed.
    pub fn poll_wc(
        &self,
        mut lease: InFlightRdmaWrite,
    ) -> Result<Result<WorkCompletion, RdmaError>, InFlightRdmaWrite> {
        if lease.fabric_id != self.lock().id {
            return Ok(Err(RdmaError::ForeignLease));
        }
        match self.complete(&mut lease) {
            Some(r) => Ok(r),
            None => Err(lease),
        }
    }

    /// Drive the RNIC until `lease` completes, then consume it.
    pub fn await_wc(&self, mut lease: InFlightRdmaWrite) -> Result<WorkCompletion, RdmaError> {
        if lease.fabric_id != self.lock().id {
            return Err(RdmaError::ForeignLease);
        }
        loop {
            if let Some(r) = self.complete(&mut lease) {
                return r;
            }
            if self.progress(usize::MAX) == 0 {
                // Nothing queued and no completion: cannot happen for a lease
                // from this fabric, but never spin.
                return Err(RdmaError::ForeignLease);
            }
        }
    }

    fn complete(&self, lease: &mut InFlightRdmaWrite) -> Option<Result<WorkCompletion, RdmaError>> {
        let mut g = self.lock();
        let node = lease.node as usize;
        let wc = g.nics[node].cq.remove(&lease.wr_id)?;
        lease.fabric = None;
        let src = g.pending_src.remove(&lease.wr_id);
        if let Some(k) = src
            && let Some(r) = g.nics[node].regions.get_mut(&k.slot)
            && r.generation == k.generation
        {
            r.in_flight -= 1;
        }
        Some(match wc.status {
            WcStatus::Success => Ok(wc),
            s => Err(RdmaError::Completion(s)),
        })
    }

    /// Next receive completion (immediate data) on `node`.
    pub fn poll_recv(&self, node: u32) -> Option<RecvCompletion> {
        self.lock().nics.get_mut(node as usize)?.recv_cq.pop_front()
    }

    /// Leases and regions dropped without their consuming exit.
    pub fn leaks(&self) -> (u64, u64) {
        let g = self.lock();
        (g.leaked_leases, g.leaked_regions)
    }

    /// Read a region by its remote grant (for tests standing in for the
    /// remote host's own view of its memory).
    pub fn read_remote(&self, grant: RemoteSlotGrant) -> Option<Vec<u8>> {
        let g = self.lock();
        if grant.fabric != g.id {
            return None;
        }
        let r = g
            .nics
            .get(grant.node as usize)?
            .regions
            .get(&(grant.rkey >> 16))?;
        (r.generation & 0xffff == grant.rkey & 0xffff).then(|| r.buf.clone())
    }
}
