//! The unified 64-bit choice tape and its shrink key.

use std::fmt;

/// Upper bound on the number of choices one tape (or one run) may hold.
/// Parsing a longer saved tape is a typed error.
pub const MAX_TAPE_LEN: usize = 1 << 20;

/// Header line of the saved-tape text format.
pub const TAPE_HEADER: &str = "vesper-crucible-tape/1";

/// The unified choice tape (§29.1.2, §29.5): every structured value a
/// generator draws and every SimPulse scheduling decision is one `u64`
/// entry. Replaying the same tape against the same deterministic property
/// reproduces the same run.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct ChoiceTape {
    pub values: Vec<u64>,
}

/// The design's shortlex shrink measure: length first, then the sum of the
/// choices. The sum is held in `u128`, which cannot overflow for tapes of at
/// most [`MAX_TAPE_LEN`] entries (2^20 * 2^64 < 2^128).
///
/// This is the `(len, sum)` order of `choice_tape_shrink_terminates`; it is
/// coarser than lexicographic shortlex (distinct tapes of equal length and
/// sum share a key, and neither is a reduction of the other).
/// `lean/CrucibleShrink.lean` proves the order well-founded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ShrinkKey {
    pub len: usize,
    pub sum: u128,
}

impl ShrinkKey {
    pub fn of(values: &[u64]) -> Self {
        #[cfg(vesper_port)]
        {
            let key = crate::binding::tape_key(values);
            Self {
                len: values.len(),
                sum: (u128::from(key.hi) << 64) | u128::from(key.lo),
            }
        }
        #[cfg(not(vesper_port))]
        {
            crate::engine::tape::key_of(values)
        }
    }

    /// Strictly smaller in the `(len, sum)` lexicographic order.
    pub fn smaller_than(self, other: Self) -> bool {
        #[cfg(vesper_port)]
        {
            crate::binding::key_smaller(&self.port(), &other.port())
        }
        #[cfg(not(vesper_port))]
        {
            crate::engine::tape::key_smaller_than(self, other)
        }
    }

    /// The key as the Vesper mirror (length, then the sum's two words).
    #[cfg(vesper_port)]
    fn port(self) -> crate::binding::repr::Key {
        crate::binding::repr::Key {
            len: self.len as u64,
            hi: (self.sum >> 64) as u64,
            lo: self.sum as u64,
        }
    }
}

/// Errors parsing a saved tape. Never a panic on untrusted text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TapeError {
    MissingHeader,
    UnknownHeader(String),
    BadValue { index: usize },
    TooLong { limit: usize },
}

impl fmt::Display for TapeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingHeader => write!(f, "saved tape is empty (missing `{TAPE_HEADER}`)"),
            Self::UnknownHeader(h) => write!(f, "unknown tape header {h:?}"),
            Self::BadValue { index } => {
                write!(f, "choice {index} is not 1-16 lowercase hex digits")
            }
            Self::TooLong { limit } => write!(f, "tape holds more than {limit} choices"),
        }
    }
}

impl std::error::Error for TapeError {}

impl ChoiceTape {
    pub fn new(values: Vec<u64>) -> Self {
        Self { values }
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    pub fn key(&self) -> ShrinkKey {
        ShrinkKey::of(&self.values)
    }

    /// Serialize as the text format: the header line, then one lowercase hex
    /// value per line. The format is stable so tapes can be checked in as
    /// regression cases.
    pub fn to_text(&self) -> String {
        #[cfg(vesper_port)]
        {
            String::from_utf8(crate::binding::tape_to_text(&self.values))
                .expect("the tape text format is ASCII")
        }
        #[cfg(not(vesper_port))]
        {
            crate::engine::tape::to_text(&self.values)
        }
    }

    /// Parse the text format. Blank lines are ignored; every other line must
    /// be 1-16 lowercase hex digits.
    pub fn from_text(text: &str) -> Result<Self, TapeError> {
        #[cfg(vesper_port)]
        {
            let parsed = crate::binding::tape_parse(text, MAX_TAPE_LEN as u64);
            let view = parsed.as_ref();
            match view.code() {
                0 => Ok(Self {
                    values: view.values().to_vec(),
                }),
                1 => Err(TapeError::MissingHeader),
                2 => {
                    let header = &text[view.start() as usize..view.end() as usize];
                    Err(TapeError::UnknownHeader(header.chars().take(64).collect()))
                }
                3 => Err(TapeError::BadValue {
                    index: view.index() as usize,
                }),
                _ => Err(TapeError::TooLong {
                    limit: MAX_TAPE_LEN,
                }),
            }
        }
        #[cfg(not(vesper_port))]
        {
            crate::engine::tape::from_text(text)
        }
    }
}
