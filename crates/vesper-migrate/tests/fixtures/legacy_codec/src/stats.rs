//! Decoder statistics (safe; executed by the shadow tests).

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DecoderStats {
    pub last_crc: u32,
    pub record_count: i32,
    pub bytes_seen: u64,
}

impl DecoderStats {
    pub fn observe(&mut self, len: usize) {
        self.bytes_seen += len as u64;
    }

    pub fn summary(&self) -> u64 {
        self.bytes_seen ^ u64::from(self.last_crc)
    }

    pub fn reset(&mut self) {
        *self = DecoderStats::default();
    }

    pub fn into_parts(self) -> (u32, i32) {
        (self.last_crc, self.record_count)
    }
}
