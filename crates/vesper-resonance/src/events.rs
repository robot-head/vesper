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

/// Decode `bytes` into the caller's preallocated `out`, returning the filled
/// prefix. Fails without partial results on any malformed frame.
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

    /// Start a ramp to `target` over `ramp_samples` (0 jumps immediately).
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

/// Parameter lookup for event application.
pub trait ParamTable {
    /// The ramp addressed by an event, or `None` if it does not exist.
    fn ramp(&mut self, node_id: u16, param_id: u32) -> Option<&mut ParamRamp>;
}

/// Validate `events` for a block of `block` samples against `params`.
pub fn validate_events<P: ParamTable>(
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
        if params.ramp(ev.node_id, ev.param_id).is_none() {
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
            if let Some(r) = params.ramp(ev.node_id, ev.param_id) {
                r.set(ev.target, ev.ramp_samples);
            }
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
