//! Rollback snapshot ring with linear tickets (§36.2 `KineticRollback`).
//!
//! [`SnapshotRing::capture`] copies the whole tiled world into a slot reserved
//! at construction (no allocation) and returns a [`RollbackTicket`]. The
//! ticket is the only way to use or free the slot: it is not `Clone` or
//! `Copy`, is `#[must_use]`, and leaves only through
//! [`SnapshotRing::confirm`] (retire committed history) or
//! [`SnapshotRing::rewind`] / [`SnapshotRing::rewind_and_resimulate`]
//! (restore it). A refused consuming call hands the ticket back. A ticket
//! dropped any other way is a leak: the ring counts it
//! ([`SnapshotRing::leaked_tickets`]) and its slot stays pinned, so committed
//! history is never silently released or overwritten.
//!
//! This is host-library discipline enforced by Rust ownership. It is not an
//! implementation of the Vesper source typing rules for `linear struct
//! RollbackCheckpoint`.

use crate::tile::RigidBodyTile;
use crate::world::{StepError, World};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_RING_ID: AtomicU64 = AtomicU64::new(1);

/// Proof of a captured checkpoint (`linear struct RollbackCheckpoint`).
#[must_use = "a RollbackTicket must be confirmed or rewound; dropping it pins its ring slot"]
#[derive(Debug)]
pub struct RollbackTicket {
    saved_tick: u64,
    ring_slot: u16,
    ring_id: u64,
    leaks: Arc<AtomicU64>,
    armed: bool,
}

impl RollbackTicket {
    /// Tick the checkpoint holds.
    pub fn saved_tick(&self) -> u64 {
        self.saved_tick
    }

    /// Ring slot it pins.
    pub fn ring_slot(&self) -> u16 {
        self.ring_slot
    }

    fn disarm(mut self) {
        self.armed = false;
    }
}

impl Drop for RollbackTicket {
    fn drop(&mut self) {
        if self.armed {
            self.leaks.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Typed rollback failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RollbackError {
    /// Every slot holds an unconsumed checkpoint.
    RingFull,
    /// The ticket belongs to another ring.
    ForeignTicket,
    /// The world's body count differs from the ring's.
    WorldShape { expected: usize, found: usize },
    /// The world is not the one the ring was built for (its collider table
    /// and joints, which give the snapshot meaning, may differ).
    ForeignWorld,
    /// The ring was built with zero slots or more than `u16::MAX + 1`.
    InvalidCapacity(usize),
}

impl std::fmt::Display for RollbackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RingFull => write!(f, "rollback ring full"),
            Self::ForeignTicket => write!(f, "ticket belongs to another ring"),
            Self::WorldShape { expected, found } => {
                write!(f, "world has {found} bodies, ring expects {expected}")
            }
            Self::InvalidCapacity(n) => write!(f, "invalid ring capacity {n}"),
            Self::ForeignWorld => write!(f, "world is not the one this ring snapshots"),
        }
    }
}

impl std::error::Error for RollbackError {}

/// Failure of [`SnapshotRing::rewind_and_resimulate`].
#[derive(Debug)]
pub enum ResimulateError {
    /// The rewind was refused; the ticket is returned.
    Refused(RollbackTicket, RollbackError),
    /// The rewind happened (ticket consumed) but a re-simulated step failed.
    Step { tick: u64, error: StepError },
}

#[derive(Debug)]
struct Slot {
    tiles: Vec<RigidBodyTile>,
    tick: u64,
    occupied: bool,
}

/// Preallocated ring of world snapshots.
#[derive(Debug)]
pub struct SnapshotRing {
    id: u64,
    world_id: u64,
    body_count: usize,
    slots: Vec<Slot>,
    next: usize,
    leaks: Arc<AtomicU64>,
}

impl SnapshotRing {
    /// A ring of `capacity` snapshots of `world`. It only captures and
    /// restores that world, so restored collider ids and joint indices always
    /// refer to the tables they were captured with.
    pub fn new(world: &World, capacity: usize) -> Result<Self, RollbackError> {
        if capacity == 0 || capacity > usize::from(u16::MAX) + 1 {
            return Err(RollbackError::InvalidCapacity(capacity));
        }
        let tiles = world.bodies.tiles.len();
        Ok(Self {
            id: NEXT_RING_ID.fetch_add(1, Ordering::Relaxed),
            world_id: world.id,
            body_count: world.bodies.len(),
            slots: (0..capacity)
                .map(|_| Slot {
                    tiles: vec![RigidBodyTile::EMPTY; tiles],
                    tick: 0,
                    occupied: false,
                })
                .collect(),
            next: 0,
            leaks: Arc::new(AtomicU64::new(0)),
        })
    }

    fn check_world(&self, world: &World) -> Result<(), RollbackError> {
        if world.id != self.world_id {
            return Err(RollbackError::ForeignWorld);
        }
        if world.bodies.len() != self.body_count {
            return Err(RollbackError::WorldShape {
                expected: self.body_count,
                found: world.bodies.len(),
            });
        }
        Ok(())
    }

    /// Copy `world` into a free slot (`capture_checkpoint`).
    pub fn capture(&mut self, world: &World) -> Result<RollbackTicket, RollbackError> {
        self.check_world(world)?;
        let n = self.slots.len();
        let slot = (0..n)
            .map(|k| (self.next + k) % n)
            .find(|&s| !self.slots[s].occupied)
            .ok_or(RollbackError::RingFull)?;
        let entry = &mut self.slots[slot];
        entry.tiles.copy_from_slice(&world.bodies.tiles);
        entry.tick = world.tick;
        entry.occupied = true;
        self.next = (slot + 1) % n;
        Ok(RollbackTicket {
            saved_tick: world.tick,
            ring_slot: slot as u16,
            ring_id: self.id,
            leaks: Arc::clone(&self.leaks),
            armed: true,
        })
    }

    fn check_ticket(&self, ticket: &RollbackTicket) -> Result<(), RollbackError> {
        if ticket.ring_id != self.id {
            return Err(RollbackError::ForeignTicket);
        }
        Ok(())
    }

    /// Retire a committed checkpoint and free its slot (`confirm_and_retire`).
    pub fn confirm(
        &mut self,
        ticket: RollbackTicket,
    ) -> Result<u64, (RollbackTicket, RollbackError)> {
        if let Err(e) = self.check_ticket(&ticket) {
            return Err((ticket, e));
        }
        let slot = &mut self.slots[usize::from(ticket.ring_slot)];
        slot.occupied = false;
        let tick = slot.tick;
        ticket.disarm();
        Ok(tick)
    }

    /// Restore `world` to the checkpoint and free the slot. Returns the
    /// restored tick.
    pub fn rewind(
        &mut self,
        ticket: RollbackTicket,
        world: &mut World,
    ) -> Result<u64, (RollbackTicket, RollbackError)> {
        if let Err(e) = self
            .check_ticket(&ticket)
            .and_then(|()| self.check_world(world))
        {
            return Err((ticket, e));
        }
        let slot = &mut self.slots[usize::from(ticket.ring_slot)];
        world.bodies.tiles.copy_from_slice(&slot.tiles);
        world.tick = slot.tick;
        slot.occupied = false;
        let tick = slot.tick;
        ticket.disarm();
        Ok(tick)
    }

    /// `rewind_and_resimulate`: restore, then run `ticks` steps, calling
    /// `input(tick, world)` before each step to re-apply that tick's inputs.
    /// Returns the final tick.
    pub fn rewind_and_resimulate<F>(
        &mut self,
        ticket: RollbackTicket,
        world: &mut World,
        ticks: u64,
        mut input: F,
    ) -> Result<u64, ResimulateError>
    where
        F: FnMut(u64, &mut World),
    {
        self.rewind(ticket, world)
            .map_err(|(t, e)| ResimulateError::Refused(t, e))?;
        for _ in 0..ticks {
            input(world.tick(), world);
            world.step().map_err(|error| ResimulateError::Step {
                tick: world.tick(),
                error,
            })?;
        }
        Ok(world.tick())
    }

    /// Tickets of this ring dropped without being consumed (the leak
    /// auditor). Their slots stay pinned.
    pub fn leaked_tickets(&self) -> u64 {
        self.leaks.load(Ordering::Relaxed)
    }

    /// Slots currently holding a checkpoint.
    pub fn occupied(&self) -> usize {
        self.slots.iter().filter(|s| s.occupied).count()
    }

    /// Total slots.
    pub fn capacity(&self) -> usize {
        self.slots.len()
    }
}
