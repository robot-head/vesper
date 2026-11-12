//! Adapter from native continuations to `vesper_pulse::scheduler`.
//!
//! [`NativeTask`] implements [`Resume`]. The scheduler's ownership rules carry
//! over unchanged: every queue transition moves the task (and therefore its
//! one-shot [`Parked`] continuation), and `Resume::resume(self, ..)` consumes
//! it. I/O requests move the task's `File` and a fresh buffer into the
//! driver; the completion moves them back.
//!
//! Park operation mapping (prototype op tags):
//! * `OP_YIELD` → `Step::Yield`, resumed with `Value(0)` on `Event::Yield`.
//! * `OP_READ(len, dest)` → `Step::Io(Read)`; the completion copies the bytes
//!   into the data frame at `dest` (bounds-checked) and resumes `Value(n)`.
//!   An I/O error resumes `Error(errno)`.
//! * `Event::Cancelled(_)` resumes `Cancelled` exactly once, after recovering
//!   any owned file from the interrupted event.
//! * Any other op resumes `Error(ENOSYS)` immediately.
use crate::abi::exit;
use crate::fiber::{Exit, Fiber, Outcome, Parked, Resumption};
use crate::prototype::{OP_READ, OP_YIELD};
use std::fs::File;
use vesper_pulse::io_driver::{Operation, Request};
use vesper_pulse::scheduler::{Event, Resume, Step};

pub const EBADF: u64 = 9;
pub const EFAULT: u64 = 14;
pub const EINVAL: u64 = 22;
pub const ENOSYS: u64 = 38;
pub const EIO: u64 = 5;
/// Largest read a prototype task may request.
pub const MAX_READ: u64 = 4096;

enum State {
    Fresh(Fiber),
    Parked(Parked),
}

/// A scheduled native fiber plus the resources its operations use.
pub struct NativeTask {
    /// `None` only transiently inside `resume`.
    state: Option<State>,
    file: Option<File>,
    read_offset: u64,
}

impl NativeTask {
    pub fn new(fiber: Fiber, file: Option<File>) -> Self {
        Self {
            state: Some(State::Fresh(fiber)),
            file,
            read_offset: 0,
        }
    }
    pub fn fiber_id(&self) -> u64 {
        match &self.state {
            Some(State::Fresh(f)) => f.id(),
            Some(State::Parked(k)) => k.fiber_id(),
            None => 0,
        }
    }
    /// The suspended continuation, if parked (for relocation in tests).
    pub fn parked_mut(&mut self) -> Option<&mut Parked> {
        match &mut self.state {
            Some(State::Parked(k)) => Some(k),
            _ => None,
        }
    }
}

/// Completion payload: `[status, result (8 bytes LE), data frame image...]`.
pub fn encode_exit(e: &Exit) -> Vec<u8> {
    let mut out = Vec::with_capacity(9 + e.data.len());
    out.push(e.status as u8);
    out.extend_from_slice(&e.result.to_le_bytes());
    out.extend_from_slice(&e.data);
    out
}

/// Decoded completion payload.
#[derive(Debug, PartialEq, Eq)]
pub struct Completed {
    pub status: u64,
    pub result: u64,
    pub data: Vec<u8>,
}

impl Completed {
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 9 {
            return None;
        }
        Some(Self {
            status: u64::from(bytes[0]),
            result: u64::from_le_bytes(bytes[1..9].try_into().ok()?),
            data: bytes[9..].to_vec(),
        })
    }
    pub fn word(&self, index: usize) -> u64 {
        u64::from_le_bytes(self.data[index * 8..index * 8 + 8].try_into().unwrap())
    }
}

/// Destination range of a read inside the data frame.
fn destination(k: &Parked, len: usize) -> Result<std::ops::Range<usize>, u64> {
    let dest = usize::try_from(k.request().arg1).map_err(|_| EFAULT)?;
    let end = dest.checked_add(len).ok_or(EFAULT)?;
    if end > k.data().len() {
        return Err(EFAULT);
    }
    Ok(dest..end)
}

fn deliver(k: &mut Parked, bytes: &[u8]) -> Result<(), u64> {
    let range = destination(k, bytes.len())?;
    k.data_mut()[range].copy_from_slice(bytes);
    Ok(())
}

impl NativeTask {
    fn resume_parked(&mut self, k: Parked, r: Resumption) -> Result<Outcome, String> {
        k.resume(r)
            .map_err(|rejected| format!("native resume rejected: {}", rejected.error))
    }
    /// Turn an outcome into a scheduler step, resolving operations that need
    /// no scheduler round trip.
    fn step(mut self, mut outcome: Outcome) -> Step<Self> {
        loop {
            let k = match outcome {
                Outcome::Exited(e) => {
                    return match e.status {
                        exit::COMPLETE | exit::CANCELLED => Step::Complete(encode_exit(&e)),
                        exit::FAILED => Step::Failed(format!(
                            "native fiber {} failed with code {}",
                            e.fiber_id, e.result
                        )),
                        other => Step::Failed(format!(
                            "native fiber {} violated the ABI (status {other})",
                            e.fiber_id
                        )),
                    };
                }
                Outcome::Parked(k) => k,
            };
            let request = k.request();
            let immediate = match request.op {
                OP_YIELD => {
                    self.state = Some(State::Parked(k));
                    return Step::Yield(self);
                }
                OP_READ if request.arg0 > MAX_READ => Resumption::error(request.evidence, EINVAL),
                OP_READ => match self.file.take() {
                    Some(file) => {
                        let len = request.arg0 as usize;
                        if let Err(code) = destination(&k, len) {
                            // Reject the destination before submitting I/O.
                            self.file = Some(file);
                            Resumption::error(request.evidence, code)
                        } else {
                            let offset = self.read_offset;
                            self.state = Some(State::Parked(k));
                            return Step::Io(Request {
                                file,
                                buffer: vec![0; len],
                                continuation: self,
                                operation: Operation::Read,
                                offset,
                            });
                        }
                    }
                    None => Resumption::error(request.evidence, EBADF),
                },
                _ => Resumption::error(request.evidence, ENOSYS),
            };
            outcome = match self.resume_parked(k, immediate) {
                Ok(o) => o,
                Err(message) => return Step::Failed(message),
            };
        }
    }
}

impl Resume for NativeTask {
    fn resume(mut self, event: Event) -> Step<Self> {
        let Some(state) = self.state.take() else {
            return Step::Failed("native task resumed while running".into());
        };
        let outcome = match (state, event) {
            (State::Fresh(fiber), Event::Start) => fiber.start(),
            (State::Fresh(fiber), Event::Cancelled(_)) => {
                // Never started: no obligations exist. Report cancellation.
                drop(fiber);
                let mut out = vec![exit::CANCELLED as u8];
                out.extend_from_slice(&0u64.to_le_bytes());
                return Step::Complete(out);
            }
            (State::Parked(k), Event::Yield) => {
                let r = Resumption::value(k.request().evidence, 0);
                match self.resume_parked(k, r) {
                    Ok(o) => o,
                    Err(m) => return Step::Failed(m),
                }
            }
            (State::Parked(mut k), Event::Io(io)) => {
                self.file = Some(io.file);
                let evidence = k.request().evidence;
                let r = match io.result {
                    Ok(n) => {
                        // A Driver is an ordinary safe trait implementation.
                        // Do not turn a malformed completion into a successful
                        // read with more bytes than were requested/delivered.
                        if n > io.buffer.len() || n as u64 > k.request().arg0 {
                            Resumption::error(evidence, EIO)
                        } else if let Some(offset) = self.read_offset.checked_add(n as u64) {
                            match deliver(&mut k, &io.buffer[..n]) {
                                Ok(()) => {
                                    self.read_offset = offset;
                                    Resumption::value(evidence, n as u64)
                                }
                                Err(code) => Resumption::error(evidence, code),
                            }
                        } else {
                            Resumption::error(evidence, EIO)
                        }
                    }
                    Err(e) => {
                        Resumption::error(evidence, e.raw_os_error().map_or(EIO, |c| c as u64))
                    }
                };
                match self.resume_parked(k, r) {
                    Ok(o) => o,
                    Err(m) => return Step::Failed(m),
                }
            }
            (State::Parked(k), Event::Cancelled(interrupted)) => {
                if let Event::Io(io) = *interrupted {
                    self.file = Some(io.file);
                }
                let r = Resumption::cancelled(k.request().evidence);
                match self.resume_parked(k, r) {
                    Ok(o) => o,
                    Err(m) => return Step::Failed(m),
                }
            }
            (State::Fresh(_), _) => {
                return Step::Failed("native task resumed before start".into());
            }
            (State::Parked(k), Event::Start) => {
                self.state = Some(State::Parked(k));
                return Step::Failed("native task started twice".into());
            }
        };
        self.step(outcome)
    }
}
