use crate::{Error, ErrorKind};

/// Admission limits for scalar JSON number conversion to finite binary64.
///
/// Work is a deterministic logical allowance, not a CPU-time guarantee. The
/// complete JSON document must already have passed `IndexedJson` validation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FloatLimits {
    pub max_number_bytes: usize,
    pub max_work: usize,
}

impl Default for FloatLimits {
    fn default() -> Self {
        Self {
            max_number_bytes: 4096,
            max_work: 32 * 1024 * 1024,
        }
    }
}

impl FloatLimits {
    /// Maximum supported admission settings. Smaller positive limits are valid.
    pub const HARD: Self = Self {
        max_number_bytes: 65_536,
        max_work: 128 * 1024 * 1024,
    };

    pub fn validate(self) -> Result<(), Error> {
        if self.max_number_bytes == 0 || self.max_number_bytes > Self::HARD.max_number_bytes {
            return Err(Error::new(ErrorKind::InputLimit, 0));
        }
        if self.max_work == 0 || self.max_work > Self::HARD.max_work {
            return Err(Error::new(ErrorKind::WorkLimit, 0));
        }
        Ok(())
    }

    // Callers relocate errors to the original Number token before returning.
    pub(crate) fn preflight(self, bytes: usize) -> Result<(), Error> {
        self.validate()?;
        if bytes > self.max_number_bytes {
            return Err(Error::new(ErrorKind::InputLimit, 0));
        }
        let work = bytes
            .checked_mul(bytes)
            .and_then(|n| n.checked_add(512))
            .ok_or(Error::new(ErrorKind::Overflow, 0))?;
        if work > self.max_work {
            return Err(Error::new(ErrorKind::WorkLimit, 0));
        }
        Ok(())
    }
}
