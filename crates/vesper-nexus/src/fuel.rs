//! Deterministic fuel accounting and quantum scheduling (§30.1 item 4).
//!
//! Wasmtime meters guest instructions with fuel. A tenant gets a total budget
//! and a quantum; the guest yields back to the host each time it burns one
//! quantum (`Store::fuel_async_yield_interval`) and traps with out-of-fuel once
//! the budget is gone. Which tenant runs next is a [`QuantumScheduler`]
//! decision. The trait mirrors SimPulse's `next_task`/`yield_task`/`finish`
//! shape; [`crate::pulse::SimPulseScheduler`] drives it from a SimPulse run
//! queue.
//!
//! Nothing here reads a real clock. Epoch ticks are advanced by the scheduler
//! loop itself (one tick per round), so epoch deadlines are deterministic too.

use crate::table::TenantId;
use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FuelError {
    FuelExhausted { requested: u64, remaining: u64 },
    ZeroQuantum,
}
impl fmt::Display for FuelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FuelExhausted {
                requested,
                remaining,
            } => write!(
                f,
                "fuel exhausted: requested {requested}, remaining {remaining}"
            ),
            Self::ZeroQuantum => write!(f, "fuel quantum must be nonzero"),
        }
    }
}
impl std::error::Error for FuelError {}

/// Host-side fuel meter (the §30.5 `FuelAccountant`), used for host-charged
/// work such as host calls; guest instruction fuel is metered by Wasmtime.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FuelAccountant {
    remaining: u64,
    consumed: u64,
}
impl FuelAccountant {
    pub const fn new(budget: u64) -> Self {
        Self {
            remaining: budget,
            consumed: 0,
        }
    }
    pub const fn remaining(&self) -> u64 {
        self.remaining
    }
    pub const fn consumed(&self) -> u64 {
        self.consumed
    }
    /// Charge `cost`. An over-budget charge drains the meter and fails, so a
    /// guest cannot retry with smaller charges to squeeze in extra work.
    #[inline]
    pub fn charge(&mut self, cost: u64) -> Result<(), FuelError> {
        if cost > self.remaining {
            let remaining = self.remaining;
            self.consumed += remaining;
            self.remaining = 0;
            return Err(FuelError::FuelExhausted {
                requested: cost,
                remaining,
            });
        }
        self.remaining -= cost;
        self.consumed += cost;
        Ok(())
    }
}

/// A tenant's scheduling parameters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TenantQuantum {
    pub tenant: TenantId,
    /// Fuel per slice before the guest is preempted.
    pub quantum: u64,
    /// Total fuel before the guest traps out of fuel.
    pub budget: u64,
}

/// SimPulse-style run queue for preemptible plugin calls.
///
/// `Token` is whatever the scheduler uses to name a runnable job (a SimPulse
/// `RunToken`, an index). Each slot index passed to [`Self::admit`] is stable
/// for the job's lifetime.
pub trait QuantumScheduler {
    type Token;
    /// A job became runnable.
    fn admit(&mut self, job: usize, tenant: TenantId);
    /// Pick the next job to run for one quantum, or `None` when idle.
    fn next_job(&mut self) -> Option<(Self::Token, usize)>;
    /// The job exhausted its quantum and was preempted; it stays runnable.
    fn preempted(&mut self, token: Self::Token);
    /// The job returned or trapped; it leaves the queue.
    fn finished(&mut self, token: Self::Token);
}

/// FIFO round robin: a preempted job goes to the back of the queue.
#[derive(Debug)]
pub struct RoundRobin {
    queue: std::collections::VecDeque<usize>,
}
impl RoundRobin {
    pub fn with_capacity(n: usize) -> Self {
        Self {
            queue: std::collections::VecDeque::with_capacity(n),
        }
    }
}
impl QuantumScheduler for RoundRobin {
    type Token = usize;
    fn admit(&mut self, job: usize, _tenant: TenantId) {
        self.queue.push_back(job);
    }
    fn next_job(&mut self) -> Option<(usize, usize)> {
        self.queue.pop_front().map(|j| (j, j))
    }
    fn preempted(&mut self, token: usize) {
        self.queue.push_back(token);
    }
    fn finished(&mut self, _token: usize) {}
}

/// One entry of the deterministic schedule trace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SliceEvent {
    /// Tenant ran one full quantum and was preempted.
    Preempted { tenant: TenantId, slice: u32 },
    /// Tenant's call completed in this slice.
    Completed { tenant: TenantId, slice: u32 },
    /// Tenant's call trapped in this slice (out of fuel, epoch, guest trap).
    Trapped { tenant: TenantId, slice: u32 },
}

/// Per-tenant accounting at the end of a scheduled run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TenantUsage {
    pub tenant: TenantId,
    pub slices: u32,
    pub fuel_consumed: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn charge_drains_on_overrun() {
        let mut f = FuelAccountant::new(10);
        f.charge(4).unwrap();
        assert_eq!(
            f.charge(7),
            Err(FuelError::FuelExhausted {
                requested: 7,
                remaining: 6
            })
        );
        assert_eq!((f.remaining(), f.consumed()), (0, 10));
        assert!(f.charge(1).is_err());
        f.charge(0).unwrap();
    }

    #[test]
    fn round_robin_requeues_preempted_jobs() {
        let mut rr = RoundRobin::with_capacity(2);
        rr.admit(0, 1);
        rr.admit(1, 2);
        let (t, j) = rr.next_job().unwrap();
        assert_eq!(j, 0);
        rr.preempted(t);
        assert_eq!(rr.next_job().unwrap().1, 1);
        assert_eq!(rr.next_job().unwrap().1, 0);
        assert!(rr.next_job().is_none());
    }
}
