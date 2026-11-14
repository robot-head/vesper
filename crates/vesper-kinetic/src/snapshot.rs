//! §36.2 `KineticSnapshotWireHdr` and the canonical world hash.

use crate::tile::TiledBodies;
use sha2::Digest as _;

/// Encoded size: 8 + 4 + 4 + 32 bytes, no padding (the stated 48 bytes agree
/// with the fields).
pub const SNAPSHOT_HEADER_LEN: usize = 48;

/// Rollback/desync header: tick, body and manifold counts, and the BLAKE3
/// hash of the canonical little-endian world state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KineticSnapshotWireHdr {
    pub tick_number: u64,
    pub active_body_count: u32,
    pub manifold_count: u32,
    pub world_state_blake3: [u8; 32],
}

/// Typed header decode failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderError {
    /// Input is not exactly [`SNAPSHOT_HEADER_LEN`] bytes.
    Length(usize),
}

impl std::fmt::Display for HeaderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Length(n) => write!(f, "snapshot header is {n} bytes, expected 48"),
        }
    }
}

impl std::error::Error for HeaderError {}

impl KineticSnapshotWireHdr {
    /// Little-endian wire bytes.
    pub fn encode(&self) -> [u8; SNAPSHOT_HEADER_LEN] {
        let mut out = [0u8; SNAPSHOT_HEADER_LEN];
        out[0..8].copy_from_slice(&self.tick_number.to_le_bytes());
        out[8..12].copy_from_slice(&self.active_body_count.to_le_bytes());
        out[12..16].copy_from_slice(&self.manifold_count.to_le_bytes());
        out[16..48].copy_from_slice(&self.world_state_blake3);
        out
    }

    /// Parse wire bytes; any length other than 48 is rejected.
    pub fn decode(bytes: &[u8]) -> Result<Self, HeaderError> {
        let bytes: &[u8; SNAPSHOT_HEADER_LEN] = bytes
            .try_into()
            .map_err(|_| HeaderError::Length(bytes.len()))?;
        let mut hash = [0u8; 32];
        hash.copy_from_slice(&bytes[16..48]);
        Ok(Self {
            tick_number: u64::from_le_bytes(bytes[0..8].try_into().expect("8 bytes")),
            active_body_count: u32::from_le_bytes(bytes[8..12].try_into().expect("4 bytes")),
            manifold_count: u32::from_le_bytes(bytes[12..16].try_into().expect("4 bytes")),
            world_state_blake3: hash,
        })
    }
}

/// SHA-256 and BLAKE3 of the same canonical byte stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorldHashes {
    pub sha256: [u8; 32],
    pub blake3: [u8; 32],
}

/// Hash the canonical state: tick (`u64le`), body count (`u64le`), then for
/// every body in index order the IEEE-754 bit patterns (`u32le`) of
/// position, velocity, inverse mass and compliance, then collider id and
/// layer mask. Unused tile lanes are not hashed. Streams into both hashers;
/// does not allocate.
pub fn hash_world(tick: u64, bodies: &TiledBodies) -> WorldHashes {
    let mut sha = sha2::Sha256::new();
    let mut b3 = blake3::Hasher::new();
    let mut feed = |bytes: &[u8]| {
        sha.update(bytes);
        b3.update(bytes);
    };
    feed(&tick.to_le_bytes());
    feed(&(bodies.len() as u64).to_le_bytes());
    for i in 0..bodies.len() {
        let p = bodies.pos(i);
        let v = bodies.vel(i);
        let mut record = [0u8; 40];
        let words = [
            p[0].to_bits(),
            p[1].to_bits(),
            p[2].to_bits(),
            v[0].to_bits(),
            v[1].to_bits(),
            v[2].to_bits(),
            bodies.inv_mass(i).to_bits(),
            bodies.compliance(i).to_bits(),
            bodies.collider(i),
            bodies.layer_mask(i),
        ];
        for (chunk, w) in record.as_chunks_mut::<4>().0.iter_mut().zip(words) {
            chunk.copy_from_slice(&w.to_le_bytes());
        }
        feed(&record);
    }
    WorldHashes {
        sha256: sha.finalize().into(),
        blake3: *b3.finalize().as_bytes(),
    }
}
