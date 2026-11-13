//! Step-machine programs shared by the exploration tests.
#![allow(dead_code)]

use vesper_crucible::{
    Footprint, IoRequest, IoResult, PropError, SimFiber, SimStep, SplitMix64, StepCx, Wake,
};

// ---------------------------------------------------------------------------
// Planted race: a request whose reply can arrive after its timeout fired.

pub const PENDING: u64 = 0;
pub const TIMED_OUT: u64 = 1;
pub const APPLIED: u64 = 2;

#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Rpc {
    pub pending: bool,
    pub timed_out: bool,
    /// How many times the request's effect was applied (reply or fallback).
    pub applied: u32,
}

pub enum RpcFiber {
    /// Sends the request, parks on the external reply, then applies it.
    /// With `checks_timeout` false it applies even after the watchdog has
    /// already applied the fallback: the planted bug.
    Requester { pc: u8, checks_timeout: bool },
    /// Sleeps `timeout_ns`, then applies a fallback if no reply came.
    Watchdog { pc: u8, timeout_ns: u64 },
}

impl SimFiber<Rpc> for RpcFiber {
    fn footprint(&self) -> Footprint {
        match self {
            Self::Requester { pc: 0, .. } => Footprint::none().write(PENDING),
            Self::Requester { .. } => Footprint::none()
                .write(PENDING)
                .read(TIMED_OUT)
                .write(APPLIED),
            Self::Watchdog { pc: 0, .. } => Footprint::none(),
            Self::Watchdog { .. } => Footprint::none()
                .write(PENDING)
                .write(TIMED_OUT)
                .write(APPLIED),
        }
    }

    fn step(&mut self, w: &mut Rpc, cx: &mut StepCx) -> Result<SimStep, PropError> {
        match self {
            Self::Requester { pc, checks_timeout } => {
                if *pc == 0 {
                    *pc = 1;
                    cx.write(PENDING);
                    w.pending = true;
                    return Ok(SimStep::Io(IoRequest::External {
                        result: IoResult::Success { bytes: 8 },
                    }));
                }
                assert!(matches!(cx.reason(), Wake::ExternalIo { seq: 0, .. }));
                cx.write(PENDING);
                cx.read(TIMED_OUT);
                w.pending = false;
                if !(*checks_timeout && w.timed_out) {
                    cx.write(APPLIED);
                    w.applied += 1;
                }
                Ok(SimStep::Done)
            }
            Self::Watchdog { pc, timeout_ns } => {
                if *pc == 0 {
                    *pc = 1;
                    return Ok(SimStep::Sleep { ns: *timeout_ns });
                }
                assert!(matches!(cx.reason(), Wake::Timer { seq: 0 }));
                cx.write(PENDING);
                if w.pending {
                    cx.write(TIMED_OUT);
                    cx.write(APPLIED);
                    w.timed_out = true;
                    w.pending = false;
                    w.applied += 1;
                }
                Ok(SimStep::Done)
            }
        }
    }
}

pub fn rpc(checks_timeout: bool) -> (Rpc, Vec<RpcFiber>) {
    (
        Rpc::default(),
        vec![
            RpcFiber::Requester {
                pc: 0,
                checks_timeout,
            },
            RpcFiber::Watchdog {
                pc: 0,
                timeout_ns: 100,
            },
        ],
    )
}

pub fn applied_once(w: &Rpc) -> Result<(), PropError> {
    if w.applied == 1 {
        Ok(())
    } else {
        Err(PropError::fail(
            "double-apply",
            format!("request applied {} times", w.applied),
        ))
    }
}

// ---------------------------------------------------------------------------
// Small random programs over a few shared cells, with yields, timers,
// scheduled and external I/O. Every step folds what it observes (values,
// wake reason, virtual time) into a per-fiber accumulator so that the final
// world distinguishes schedules whose order matters.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Read(u64),
    Write(u64),
    /// Read then write the same cell in one atomic step.
    Update(u64),
    /// A step with no shared access.
    Local,
    /// Read a cell and fail the run if its value is odd.
    Check(u64),
    Sleep(u64),
    Scheduled(u64),
    External(u64),
}

pub const CELLS: u64 = 3;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Cells {
    pub cells: Vec<u64>,
    pub acc: Vec<u64>,
}

pub struct OpFiber {
    pub id: usize,
    pub ops: Vec<Op>,
    pub pc: usize,
}

fn mix(a: u64, b: u64) -> u64 {
    (a ^ b)
        .wrapping_mul(0x9e37_79b9_7f4a_7c15)
        .rotate_left(17)
        .wrapping_add(b)
}

/// Accumulator object of fiber `id` (only that fiber touches it).
pub fn acc_obj(id: usize) -> u64 {
    100 + id as u64
}

impl SimFiber<Cells> for OpFiber {
    fn footprint(&self) -> Footprint {
        let fp = Footprint::none().write(acc_obj(self.id));
        match self.ops.get(self.pc) {
            Some(Op::Read(c) | Op::Check(c)) => fp.read(*c),
            Some(Op::Write(c) | Op::Update(c)) => fp.write(*c),
            _ => fp,
        }
    }

    fn step(&mut self, w: &mut Cells, cx: &mut StepCx) -> Result<SimStep, PropError> {
        let me = self.id;
        cx.write(acc_obj(me));
        let reason = match cx.reason() {
            Wake::Start => 1,
            Wake::Yield => 2,
            Wake::Timer { seq } => 3 + 7 * u64::from(seq),
            Wake::ScheduledIo { result, .. } | Wake::ExternalIo { result, .. } => match result {
                IoResult::Success { bytes } => 10 + bytes,
                IoResult::Error { code } => 1000 + code.unsigned_abs() as u64,
                IoResult::Cancelled => 4,
            },
        };
        w.acc[me] = mix(mix(w.acc[me], reason), cx.now_ns());
        let op = self.ops[self.pc];
        self.pc += 1;
        let tag = (me as u64 + 1) * 1000 + self.pc as u64;
        let next = match op {
            Op::Read(c) => {
                cx.read(c);
                w.acc[me] = mix(w.acc[me], w.cells[c as usize]);
                SimStep::Yield
            }
            Op::Write(c) => {
                cx.write(c);
                w.cells[c as usize] = tag;
                SimStep::Yield
            }
            Op::Update(c) => {
                cx.write(c);
                let v = &mut w.cells[c as usize];
                *v = mix(*v, tag);
                SimStep::Yield
            }
            Op::Local => SimStep::Yield,
            Op::Check(c) => {
                cx.read(c);
                if w.cells[c as usize] % 2 == 1 {
                    return Err(PropError::fail("odd", format!("cell {c} by fiber {me}")));
                }
                SimStep::Yield
            }
            Op::Sleep(ns) => SimStep::Sleep { ns },
            Op::Scheduled(ns) => SimStep::Io(IoRequest::Scheduled {
                delay_ns: ns,
                result: IoResult::Success { bytes: ns },
            }),
            Op::External(bytes) => SimStep::Io(IoRequest::External {
                result: IoResult::Success { bytes },
            }),
        };
        Ok(if self.pc == self.ops.len() && next == SimStep::Yield {
            SimStep::Done
        } else {
            next
        })
    }
}

/// After a parking op that is the last one, the fiber still needs a final
/// step to finish: `random_program` appends `Local` in that case.
pub fn random_program(seed: u64, fibers: usize, max_ops: usize) -> Vec<Vec<Op>> {
    let mut rng = SplitMix64(seed);
    (0..fibers)
        .map(|_| {
            let n = 1 + rng.index(max_ops);
            let mut ops: Vec<Op> = (0..n)
                .map(|_| {
                    let cell = rng.below_inclusive(CELLS - 1);
                    match rng.index(11) {
                        10 => Op::Check(cell),
                        0 | 1 => Op::Read(cell),
                        2 | 3 => Op::Write(cell),
                        4 | 5 => Op::Update(cell),
                        6 => Op::Local,
                        7 => Op::Sleep(1 + rng.below_inclusive(2) * 10),
                        8 => Op::Scheduled(1 + rng.below_inclusive(2) * 10),
                        _ => Op::External(rng.below_inclusive(3)),
                    }
                })
                .collect();
            if matches!(
                ops.last(),
                Some(Op::Sleep(_) | Op::Scheduled(_) | Op::External(_))
            ) {
                ops.push(Op::Local);
            }
            ops
        })
        .collect()
}

pub fn build(program: &[Vec<Op>]) -> (Cells, Vec<OpFiber>) {
    (
        Cells {
            cells: vec![0; CELLS as usize],
            acc: vec![0; program.len()],
        },
        program
            .iter()
            .enumerate()
            .map(|(id, ops)| OpFiber {
                id,
                ops: ops.clone(),
                pc: 0,
            })
            .collect(),
    )
}
