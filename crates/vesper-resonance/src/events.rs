//! Sample-accurate parameter events (design §32.1 item 5, §32.2).
//!
//! [`RtParamEvent`] is the 16-byte `RtParamEventWire` frame: the stated
//! fields (`u16le`, `u16le`, `u32le`, `u32le`, `u32le`) add up to exactly 16
//! bytes with no padding, so the stated layout is kept.
//!
//! [`process_with_events`] splits a block at every event offset, applies the
//! events at that offset (in wire order), then renders the sub-block. Events
//! are validated as a whole before anything is applied: offsets must be
//! inside the block and non-decreasing, targets finite, and the addressed
//! parameter must exist. Nothing here allocates.

use std::fmt;

/// Wire size of one event.
pub const EVENT_WIRE_BYTES: usize = 16;

/// One decoded parameter event.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RtParamEvent {
    /// Sample offset inside the block.
    pub sample_offset: u16,
    /// Addressed node (voice, joint, ...).
    pub node_id: u16,
    /// Addressed parameter of that node.
    pub param_id: u32,
    /// Target value.
    pub target: f32,
    /// Ramp length in samples (0 = jump).
    pub ramp_samples: u32,
}

impl RtParamEvent {
    /// Encode as the 16-byte little-endian wire frame.
    #[cfg(vesper_port)]
    pub fn encode(&self) -> [u8; EVENT_WIRE_BYTES] {
        let mut b = [0u8; EVENT_WIRE_BYTES];
        crate::port::ok(crate::port::event_encode(&self.wire(), &mut b));
        b
    }

    #[cfg(vesper_port)]
    fn wire(&self) -> crate::port::repr::Wire {
        crate::port::repr::Wire::from_value(&crate::port::Wire {
            sample_offset: self.sample_offset,
            node_id: self.node_id,
            param_id: self.param_id,
            target: self.target.to_bits(),
            ramp_samples: self.ramp_samples,
        })
    }

    /// Decode one wire frame. Non-finite targets are rejected.
    #[cfg(vesper_port)]
    pub fn decode(b: &[u8; EVENT_WIRE_BYTES]) -> Result<Self, EventError> {
        Self::decode_at(b, 0)
    }

    #[cfg(vesper_port)]
    fn decode_at(bytes: &[u8], at: usize) -> Result<Self, EventError> {
        use crate::port::EventDecodeError as E;
        match crate::port::event_decode(bytes, at as u64) {
            Ok(w) => {
                let w = w.to_value();
                Ok(RtParamEvent {
                    sample_offset: w.sample_offset,
                    node_id: w.node_id,
                    param_id: w.param_id,
                    target: f32::from_bits(w.target),
                    ramp_samples: w.ramp_samples,
                })
            }
            Err(E::EventFault(f)) => Err(f.into()),
            Err(e) => panic!("the Vesper port of vesper-resonance failed: {e:?}"),
        }
    }

    /// Encode as the 16-byte little-endian wire frame.
    #[cfg(not(vesper_port))]
    pub fn encode(&self) -> [u8; EVENT_WIRE_BYTES] {
        let mut b = [0u8; EVENT_WIRE_BYTES];
        b[0..2].copy_from_slice(&self.sample_offset.to_le_bytes());
        b[2..4].copy_from_slice(&self.node_id.to_le_bytes());
        b[4..8].copy_from_slice(&self.param_id.to_le_bytes());
        b[8..12].copy_from_slice(&self.target.to_bits().to_le_bytes());
        b[12..16].copy_from_slice(&self.ramp_samples.to_le_bytes());
        b
    }

    /// Decode one wire frame. Non-finite targets are rejected.
    #[cfg(not(vesper_port))]
    pub fn decode(b: &[u8; EVENT_WIRE_BYTES]) -> Result<Self, EventError> {
        let target = f32::from_bits(u32::from_le_bytes([b[8], b[9], b[10], b[11]]));
        if !target.is_finite() {
            return Err(EventError::NonFiniteTarget);
        }
        Ok(RtParamEvent {
            sample_offset: u16::from_le_bytes([b[0], b[1]]),
            node_id: u16::from_le_bytes([b[2], b[3]]),
            param_id: u32::from_le_bytes([b[4], b[5], b[6], b[7]]),
            target,
            ramp_samples: u32::from_le_bytes([b[12], b[13], b[14], b[15]]),
        })
    }
}

/// Event decoding and validation errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventError {
    /// Byte length is not a multiple of 16.
    TrailingBytes(usize),
    /// More events than the caller's budget (`poll_param_events(max)`).
    OverBudget {
        /// The budget.
        max: usize,
    },
    /// A target is NaN or infinite.
    NonFiniteTarget,
    /// An offset is not inside the block.
    OffsetOutOfBlock {
        /// The offset.
        offset: u16,
        /// The block length.
        block: usize,
    },
    /// Offsets decrease.
    Unsorted {
        /// Index of the first out-of-order event.
        index: usize,
    },
    /// The addressed parameter does not exist.
    UnknownParam {
        /// Node.
        node_id: u16,
        /// Parameter.
        param_id: u32,
    },
}

impl fmt::Display for EventError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for EventError {}

#[cfg(vesper_port)]
impl From<crate::port::EventFault> for EventError {
    fn from(f: crate::port::EventFault) -> Self {
        use crate::port::EventFault as F;
        match f {
            F::TrailingBytes(n) => EventError::TrailingBytes(n as usize),
            F::OverBudget(max) => EventError::OverBudget { max: max as usize },
            F::NonFiniteTarget => EventError::NonFiniteTarget,
            F::OffsetOutOfBlock(offset, block) => EventError::OffsetOutOfBlock {
                offset,
                block: block as usize,
            },
            F::Unsorted(index) => EventError::Unsorted {
                index: index as usize,
            },
        }
    }
}

/// The error of a port call whose only crate error is an `EventFault`.
#[cfg(vesper_port)]
fn event_fault<E: std::fmt::Debug>(e: E, fault: Option<crate::port::EventFault>) -> EventError {
    match fault {
        Some(f) => f.into(),
        None => panic!("the Vesper port of vesper-resonance failed: {e:?}"),
    }
}

/// Decode `bytes` into the caller's preallocated `out`, returning the filled
/// prefix. Fails without partial results on any malformed frame.
#[cfg(vesper_port)]
pub fn decode_events<'a>(
    bytes: &[u8],
    out: &'a mut [RtParamEvent],
) -> Result<&'a [RtParamEvent], EventError> {
    use crate::port::EventFramesError as E;
    let n = crate::port::event_frames(bytes.len() as u64, out.len() as u64).map_err(|e| {
        let f = match e {
            E::EventFault(f) => Some(f),
            _ => None,
        };
        event_fault(e, f)
    })? as usize;
    for (k, slot) in out.iter_mut().take(n).enumerate() {
        *slot = RtParamEvent::decode_at(bytes, k * EVENT_WIRE_BYTES)?;
    }
    Ok(&out[..n])
}

/// Decode `bytes` into the caller's preallocated `out`, returning the filled
/// prefix. Fails without partial results on any malformed frame.
#[cfg(not(vesper_port))]
pub fn decode_events<'a>(
    bytes: &[u8],
    out: &'a mut [RtParamEvent],
) -> Result<&'a [RtParamEvent], EventError> {
    if !bytes.len().is_multiple_of(EVENT_WIRE_BYTES) {
        return Err(EventError::TrailingBytes(bytes.len() % EVENT_WIRE_BYTES));
    }
    let n = bytes.len() / EVENT_WIRE_BYTES;
    if n > out.len() {
        return Err(EventError::OverBudget { max: out.len() });
    }
    let (frames, _) = bytes.as_chunks::<EVENT_WIRE_BYTES>();
    for (slot, frame) in out.iter_mut().zip(frames) {
        *slot = RtParamEvent::decode(frame)?;
    }
    Ok(&out[..n])
}

/// A linearly ramped parameter. The ramp lands exactly on its target after
/// `ramp_samples` samples.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ParamRamp {
    current: f32,
    target: f32,
    step: f32,
    remaining: u32,
}

impl ParamRamp {
    /// A parameter resting at `value`.
    pub const fn new(value: f32) -> Self {
        ParamRamp {
            current: value,
            target: value,
            step: 0.0,
            remaining: 0,
        }
    }

    #[cfg(vesper_port)]
    fn mirror(&self) -> crate::port::repr::Ramp {
        crate::port::repr::Ramp::from_value(&crate::port::Ramp {
            current: self.current.to_bits(),
            target: self.target.to_bits(),
            step: self.step.to_bits(),
            remaining: self.remaining,
        })
    }

    #[cfg(vesper_port)]
    fn load(&mut self, r: &crate::port::repr::Ramp) {
        let r = r.to_value();
        self.current = f32::from_bits(r.current);
        self.target = f32::from_bits(r.target);
        self.step = f32::from_bits(r.step);
        self.remaining = r.remaining;
    }

    /// Start a ramp to `target` over `ramp_samples` (0 jumps immediately).
    #[cfg(vesper_port)]
    pub fn set(&mut self, target: f32, ramp_samples: u32) {
        let mut r = self.mirror();
        crate::port::ramp_set(&mut r, target.to_bits(), ramp_samples);
        self.load(&r);
    }

    /// Value for the next sample.
    #[cfg(vesper_port)]
    pub fn next_value(&mut self) -> f32 {
        let mut r = self.mirror();
        crate::port::ok(crate::port::ramp_next(&mut r));
        self.load(&r);
        self.current
    }

    /// Start a ramp to `target` over `ramp_samples` (0 jumps immediately).
    #[cfg(not(vesper_port))]
    pub fn set(&mut self, target: f32, ramp_samples: u32) {
        self.target = target;
        if ramp_samples == 0 {
            self.current = target;
            self.step = 0.0;
            self.remaining = 0;
        } else {
            self.step = (target - self.current) / ramp_samples as f32;
            self.remaining = ramp_samples;
        }
    }

    /// Value for the next sample.
    #[cfg(not(vesper_port))]
    pub fn next_value(&mut self) -> f32 {
        if self.remaining > 0 {
            self.remaining -= 1;
            self.current = if self.remaining == 0 {
                self.target
            } else {
                self.current + self.step
            };
        }
        self.current
    }

    /// Current value without advancing.
    pub fn value(&self) -> f32 {
        self.current
    }

    /// Samples left in the ramp.
    pub fn remaining(&self) -> u32 {
        self.remaining
    }
}

/// Lanes per SIMD parameter-ramp tile (matches the 16-voice SVF tile).
pub const RAMP_LANES: usize = 16;

/// Sixteen independent linear parameter ramps packed in structure-of-arrays
/// layout (64-byte aligned, one cache line per vector field) so SIMD paths can
/// step all 16 lanes in parallel, bit-identical to [`ParamRamp::next_value`].
#[derive(Clone, Copy, Debug, PartialEq)]
#[repr(C, align(64))]
pub struct RampTile {
    /// Current value of each lane.
    pub current: [f32; RAMP_LANES],
    /// Target value of each lane.
    pub target: [f32; RAMP_LANES],
    /// Per-sample linear step of each lane.
    pub step: [f32; RAMP_LANES],
    /// Remaining samples in each lane's active ramp.
    pub remaining: [u32; RAMP_LANES],
}

impl RampTile {
    /// Sixteen resting ramps at `value`.
    pub const fn splat(value: f32) -> Self {
        RampTile {
            current: [value; RAMP_LANES],
            target: [value; RAMP_LANES],
            step: [0.0; RAMP_LANES],
            remaining: [0; RAMP_LANES],
        }
    }

    /// Configure `lane` to ramp from its current value to `target` over
    /// `ramp_samples` (`0` jumps immediately), matching [`ParamRamp::set`].
    #[cfg(vesper_port)]
    pub fn set_lane(&mut self, lane: usize, target: f32, ramp_samples: u32) {
        use crate::port::bits_mut;
        crate::port::ok(crate::port::tile_set_lane(
            bits_mut(&mut self.current),
            bits_mut(&mut self.target),
            bits_mut(&mut self.step),
            &mut self.remaining,
            lane as u64,
            target.to_bits(),
            ramp_samples,
        ));
    }

    /// Advance one `lane` by one sample in scalar form, matching
    /// [`ParamRamp::next_value`].
    #[cfg(vesper_port)]
    pub fn advance_lane_scalar(&mut self, lane: usize) -> f32 {
        use crate::port::{bits, bits_mut};
        f32::from_bits(crate::port::ok(crate::port::tile_advance_lane(
            bits_mut(&mut self.current),
            bits(&self.target),
            bits(&self.step),
            &mut self.remaining,
            lane as u64,
        )))
    }

    /// Advance all 16 lanes by one sample in scalar form.
    #[cfg(vesper_port)]
    pub fn scalar_next(&mut self) {
        use crate::port::{bits, bits_mut};
        crate::port::ok(crate::port::tile_next(
            bits_mut(&mut self.current),
            bits(&self.target),
            bits(&self.step),
            &mut self.remaining,
        ));
    }

    /// Configure `lane` to ramp from its current value to `target` over
    /// `ramp_samples` (`0` jumps immediately), matching [`ParamRamp::set`].
    #[cfg(not(vesper_port))]
    pub fn set_lane(&mut self, lane: usize, target: f32, ramp_samples: u32) {
        self.target[lane] = target;
        if ramp_samples == 0 {
            self.current[lane] = target;
            self.step[lane] = 0.0;
            self.remaining[lane] = 0;
        } else {
            self.step[lane] = (target - self.current[lane]) / ramp_samples as f32;
            self.remaining[lane] = ramp_samples;
        }
    }

    /// Snapshot `lane` as a scalar [`ParamRamp`].
    pub fn lane(&self, lane: usize) -> ParamRamp {
        ParamRamp {
            current: self.current[lane],
            target: self.target[lane],
            step: self.step[lane],
            remaining: self.remaining[lane],
        }
    }

    /// Overwrite `lane` from a scalar [`ParamRamp`].
    pub fn set_lane_ramp(&mut self, lane: usize, ramp: ParamRamp) {
        self.current[lane] = ramp.current;
        self.target[lane] = ramp.target;
        self.step[lane] = ramp.step;
        self.remaining[lane] = ramp.remaining;
    }

    /// Advance one `lane` by one sample in scalar form, matching
    /// [`ParamRamp::next_value`].
    #[cfg(not(vesper_port))]
    #[inline]
    pub fn advance_lane_scalar(&mut self, lane: usize) -> f32 {
        let rem = self.remaining[lane];
        if rem > 0 {
            let next_rem = rem - 1;
            self.remaining[lane] = next_rem;
            self.current[lane] = if next_rem == 0 {
                self.target[lane]
            } else {
                self.current[lane] + self.step[lane]
            };
        }
        self.current[lane]
    }

    /// Advance all 16 lanes by one sample in scalar form.
    #[cfg(not(vesper_port))]
    pub fn scalar_next(&mut self) {
        for lane in 0..RAMP_LANES {
            self.advance_lane_scalar(lane);
        }
    }
}

/// Parameter lookup for event application.
pub trait ParamTable {
    /// The scalar ramp addressed by an event, or `None` if addressed via SoA
    /// methods ([`ParamTable::has_ramp`] / [`ParamTable::set_ramp`]) or missing.
    fn ramp(&mut self, _node_id: u16, _param_id: u32) -> Option<&mut ParamRamp> {
        None
    }

    /// Whether `(node_id, param_id)` addresses a valid parameter ramp.
    fn has_ramp(&mut self, node_id: u16, param_id: u32) -> bool {
        self.ramp(node_id, param_id).is_some()
    }

    /// Start a ramp for `(node_id, param_id)` when it exists. Returns `false`
    /// when unknown.
    fn set_ramp(&mut self, node_id: u16, param_id: u32, target: f32, ramp_samples: u32) -> bool {
        if let Some(r) = self.ramp(node_id, param_id) {
            r.set(target, ramp_samples);
            true
        } else {
            false
        }
    }
}

/// Validate `events` for a block of `block` samples against `params`.
pub fn validate_events<P: ParamTable>(
    events: &[RtParamEvent],
    block: usize,
    params: &mut P,
) -> Result<(), EventError> {
    #[cfg(vesper_port)]
    {
        let mut prev = 0u16;
        for (k, ev) in events.iter().enumerate() {
            crate::port::event_check(
                ev.sample_offset,
                ev.target.to_bits(),
                prev,
                k as u64,
                block as u64,
            )
            .map_err(EventError::from)?;
            prev = ev.sample_offset;
            if !params.has_ramp(ev.node_id, ev.param_id) {
                return Err(EventError::UnknownParam {
                    node_id: ev.node_id,
                    param_id: ev.param_id,
                });
            }
        }
        Ok(())
    }
    #[cfg(not(vesper_port))]
    {
        validate_events_rust(events, block, params)
    }
}

#[cfg(not(vesper_port))]
fn validate_events_rust<P: ParamTable>(
    events: &[RtParamEvent],
    block: usize,
    params: &mut P,
) -> Result<(), EventError> {
    let mut prev = 0u16;
    for (k, ev) in events.iter().enumerate() {
        if usize::from(ev.sample_offset) >= block {
            return Err(EventError::OffsetOutOfBlock {
                offset: ev.sample_offset,
                block,
            });
        }
        if ev.sample_offset < prev {
            return Err(EventError::Unsorted { index: k });
        }
        if !ev.target.is_finite() {
            return Err(EventError::NonFiniteTarget);
        }
        prev = ev.sample_offset;
        if !params.has_ramp(ev.node_id, ev.param_id) {
            return Err(EventError::UnknownParam {
                node_id: ev.node_id,
                param_id: ev.param_id,
            });
        }
    }
    Ok(())
}

/// Process one block of `block` samples: validate `events`, then for each
/// maximal run of samples with no event inside, apply the events at its
/// start and call `render(params, start, end)`. Returns the number of
/// sub-blocks rendered.
pub fn process_with_events<P, R>(
    events: &[RtParamEvent],
    block: usize,
    params: &mut P,
    mut render: R,
) -> Result<usize, EventError>
where
    P: ParamTable,
    R: FnMut(&mut P, usize, usize),
{
    validate_events(events, block, params)?;
    let mut cursor = 0usize;
    let mut segments = 0usize;
    let mut k = 0usize;
    while cursor < block {
        while k < events.len() && usize::from(events[k].sample_offset) == cursor {
            let ev = events[k];
            let _ = params.set_ramp(ev.node_id, ev.param_id, ev.target, ev.ramp_samples);
            k += 1;
        }
        let end = events
            .get(k)
            .map_or(block, |ev| usize::from(ev.sample_offset));
        render(params, cursor, end);
        segments += 1;
        cursor = end;
    }
    Ok(segments)
}
