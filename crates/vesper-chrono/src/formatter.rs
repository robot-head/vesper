//! CoreSight trace formatter removal (§31.1 item 3).
//!
//! A CoreSight trace sink that merges several trace sources (a TPIU port, or
//! an ETF/ETB/ETR buffer in formatted mode) wraps their bytes in 16-byte
//! frames carrying 7-bit trace source IDs ("CoreSight Architecture
//! Specification", trace formatter). This module removes that framing and
//! returns one byte stream per source ID, ready for [`crate::ete`].
//!
//! Frame layout, as this module reads the specification:
//!
//! * Bytes 0, 2, …, 14 are *flag-carrying* bytes. If bit 0 is 1 the byte is
//!   an ID change to `byte >> 1`. Otherwise it is a data byte whose bit 0 is
//!   taken from the auxiliary byte.
//! * Bytes 1, 3, …, 13 are always data.
//! * Byte 15 holds the auxiliary bits: bit `n` belongs to byte `2n`. For a
//!   data byte it is that byte's bit 0. For an ID change at byte `2n` (`n` <
//!   7) it says whether the following data byte `2n + 1` still belongs to the
//!   previous ID (1) or already to the new one (0). An ID change at byte 14
//!   has no following byte.
//!
//! ID 0x00 is the null source (padding, discarded). IDs 0x01–0x6F are trace
//! sources. 0x7B (flush response) and 0x7D (trigger) carry no trace data and
//! their bytes are discarded and counted. The other IDs 0x70–0x7F are
//! reserved and rejected; 0x7F is what a frame-synchronisation packet looks
//! like, so it also marks a frame boundary that this decoder did not expect.
//!
//! [`FormatterMode::Frames`] is an ETF/ETB/ETR buffer: frames back to back
//! from offset 0. [`FormatterMode::FrameSync`] is a TPIU capture: decoding
//! starts after the first full-frame synchronisation packet
//! (`FF FF FF 7F`), and further synchronisation packets between frames are
//! skipped. Half-word synchronisation (TPIU `FF 7F` inside frames) is not
//! supported and surfaces as a reserved ID.
//!
//! The encoding was implemented from the specification text and is tested
//! against an independently written frame encoder in the tests, not against
//! a capture from Arm hardware.

use std::collections::BTreeMap;
use std::fmt;

/// Formatter frame length.
pub const FRAME_LEN: usize = 16;
/// A full-frame synchronisation packet.
pub const FSYNC: [u8; 4] = [0xFF, 0xFF, 0xFF, 0x7F];

/// Where frames start in the input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatterMode {
    /// ETF/ETB/ETR memory: aligned frames from offset 0, no sync packets.
    Frames,
    /// TPIU port capture: frames between full-frame sync packets.
    FrameSync,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FormatterLimits {
    /// Maximum total output bytes across streams.
    pub max_output: usize,
}

impl Default for FormatterLimits {
    fn default() -> Self {
        Self {
            max_output: 64 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatterError {
    /// Bytes left over that do not form a whole frame.
    PartialFrame {
        offset: usize,
        len: usize,
    },
    /// [`FormatterMode::FrameSync`] input without a sync packet.
    NoSync,
    /// A reserved trace source ID.
    ReservedId {
        offset: usize,
        id: u8,
    },
    /// An ID change at byte 14 whose auxiliary bit is set (no data byte
    /// follows it, so the bit has no meaning).
    BadAuxBit {
        offset: usize,
    },
    OutputBudget {
        limit: usize,
    },
}

impl fmt::Display for FormatterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "coresight formatter: {self:?}")
    }
}
impl std::error::Error for FormatterError {}

/// Deformatted output.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Deformatted {
    /// Trace bytes per source ID (0x01–0x6F), in arrival order.
    pub streams: BTreeMap<u8, Vec<u8>>,
    pub frames: usize,
    /// Full-frame sync packets skipped.
    pub syncs: usize,
    /// Data bytes before the first ID change (source unknown), discarded.
    pub unattributed: usize,
    /// Null-ID (0x00) padding bytes, discarded.
    pub padding: usize,
    /// Bytes under the trigger (0x7D) and flush (0x7B) IDs, discarded.
    pub control: usize,
}

impl Deformatted {
    pub fn stream(&self, id: u8) -> &[u8] {
        self.streams.get(&id).map_or(&[], Vec::as_slice)
    }
}

struct State<'a> {
    out: &'a mut Deformatted,
    id: Option<u8>,
    total: usize,
    limits: FormatterLimits,
}

impl State<'_> {
    fn emit(&mut self, byte: u8) -> Result<(), FormatterError> {
        match self.id {
            None => self.out.unattributed += 1,
            Some(0) => self.out.padding += 1,
            Some(0x7B | 0x7D) => self.out.control += 1,
            Some(id) => {
                self.total += 1;
                if self.total > self.limits.max_output {
                    return Err(FormatterError::OutputBudget {
                        limit: self.limits.max_output,
                    });
                }
                self.out.streams.entry(id).or_default().push(byte);
            }
        }
        Ok(())
    }
    fn set_id(&mut self, offset: usize, byte: u8) -> Result<(), FormatterError> {
        let id = byte >> 1;
        if (0x70..=0x7F).contains(&id) && id != 0x7B && id != 0x7D {
            return Err(FormatterError::ReservedId { offset, id });
        }
        self.id = Some(id);
        Ok(())
    }
    fn frame(&mut self, at: usize, f: &[u8]) -> Result<(), FormatterError> {
        let aux = f[15];
        for n in 0..7 {
            let b = f[2 * n];
            let flag = (aux >> n) & 1;
            if b & 1 == 1 {
                if flag == 1 {
                    // The next byte still belongs to the previous ID.
                    self.emit(f[2 * n + 1])?;
                    self.set_id(at + 2 * n, b)?;
                } else {
                    self.set_id(at + 2 * n, b)?;
                    self.emit(f[2 * n + 1])?;
                }
            } else {
                self.emit((b & 0xFE) | flag)?;
                self.emit(f[2 * n + 1])?;
            }
        }
        let b = f[14];
        let flag = aux >> 7;
        if b & 1 == 1 {
            if flag != 0 {
                return Err(FormatterError::BadAuxBit { offset: at + 15 });
            }
            self.set_id(at + 14, b)?;
        } else {
            self.emit((b & 0xFE) | flag)?;
        }
        self.out.frames += 1;
        Ok(())
    }
}

/// Remove the formatter framing from `bytes`.
pub fn deformat(
    bytes: &[u8],
    mode: FormatterMode,
    limits: &FormatterLimits,
) -> Result<Deformatted, FormatterError> {
    let mut out = Deformatted::default();
    let mut state = State {
        out: &mut out,
        id: None,
        total: 0,
        limits: *limits,
    };
    let mut at = match mode {
        FormatterMode::Frames => 0,
        FormatterMode::FrameSync => {
            let first = bytes
                .windows(FSYNC.len())
                .position(|w| w == FSYNC)
                .ok_or(FormatterError::NoSync)?;
            state.out.syncs += 1;
            first + FSYNC.len()
        }
    };
    while at < bytes.len() {
        let rest = &bytes[at..];
        if mode == FormatterMode::FrameSync && rest.starts_with(&FSYNC) {
            state.out.syncs += 1;
            at += FSYNC.len();
            continue;
        }
        if rest.len() < FRAME_LEN {
            // A sync packet's leading bytes can end a TPIU capture.
            if mode == FormatterMode::FrameSync && FSYNC.starts_with(rest) {
                break;
            }
            return Err(FormatterError::PartialFrame {
                offset: at,
                len: rest.len(),
            });
        }
        state.frame(at, &rest[..FRAME_LEN])?;
        at += FRAME_LEN;
    }
    Ok(out)
}
