//! AoSoA rigid-body tiles (§36.2 `tiled(16) copy struct RigidBodyTile`).

/// Bodies per tile.
pub const TILE_WIDTH: usize = 16;

/// Sixteen bodies, one column per field. 64-byte aligned so every hot
/// column starts on a cache line.
#[repr(C, align(64))]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RigidBodyTile {
    pub pos_x: [f32; TILE_WIDTH],
    pub pos_y: [f32; TILE_WIDTH],
    pub pos_z: [f32; TILE_WIDTH],
    pub inv_mass: [f32; TILE_WIDTH],
    pub vel_x: [f32; TILE_WIDTH],
    pub vel_y: [f32; TILE_WIDTH],
    pub vel_z: [f32; TILE_WIDTH],
    pub compliance_alpha: [f32; TILE_WIDTH],
    pub collider_asset_id: [u32; TILE_WIDTH],
    pub collision_layer_mask: [u32; TILE_WIDTH],
}

impl RigidBodyTile {
    /// An all-zero tile (unused lanes stay zero, static and maskless).
    pub const EMPTY: Self = Self {
        pos_x: [0.0; TILE_WIDTH],
        pos_y: [0.0; TILE_WIDTH],
        pos_z: [0.0; TILE_WIDTH],
        inv_mass: [0.0; TILE_WIDTH],
        vel_x: [0.0; TILE_WIDTH],
        vel_y: [0.0; TILE_WIDTH],
        vel_z: [0.0; TILE_WIDTH],
        compliance_alpha: [0.0; TILE_WIDTH],
        collider_asset_id: [0; TILE_WIDTH],
        collision_layer_mask: [0; TILE_WIDTH],
    };
}

/// One body's fields, as the caller sees them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Body {
    pub pos: [f32; 3],
    pub vel: [f32; 3],
    /// `0.0` marks a static body.
    pub inv_mass: f32,
    /// XPBD compliance (inverse stiffness) used by this body's contacts.
    pub compliance_alpha: f32,
    pub collider_asset_id: u32,
    pub collision_layer_mask: u32,
}

/// A fixed-length sequence of bodies stored as tiles
/// (`TiledSeq[RigidBodyTile]`).
#[derive(Debug, Clone, PartialEq)]
pub struct TiledBodies {
    pub(crate) tiles: Vec<RigidBodyTile>,
    len: usize,
}

#[inline]
const fn split(index: usize) -> (usize, usize) {
    (index / TILE_WIDTH, index % TILE_WIDTH)
}

impl TiledBodies {
    /// Pack `bodies` into tiles.
    pub fn from_bodies(bodies: &[Body]) -> Self {
        let mut tiles = vec![RigidBodyTile::EMPTY; bodies.len().div_ceil(TILE_WIDTH)];
        for (i, b) in bodies.iter().enumerate() {
            let (t, l) = split(i);
            let tile = &mut tiles[t];
            tile.pos_x[l] = b.pos[0];
            tile.pos_y[l] = b.pos[1];
            tile.pos_z[l] = b.pos[2];
            tile.vel_x[l] = b.vel[0];
            tile.vel_y[l] = b.vel[1];
            tile.vel_z[l] = b.vel[2];
            tile.inv_mass[l] = b.inv_mass;
            tile.compliance_alpha[l] = b.compliance_alpha;
            tile.collider_asset_id[l] = b.collider_asset_id;
            tile.collision_layer_mask[l] = b.collision_layer_mask;
        }
        Self {
            tiles,
            len: bodies.len(),
        }
    }

    /// Number of bodies.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether there are no bodies.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The tiles (the last one may be partly unused).
    pub fn tiles(&self) -> &[RigidBodyTile] {
        &self.tiles
    }

    /// Read body `index`; `None` past the end.
    pub fn get(&self, index: usize) -> Option<Body> {
        if index >= self.len {
            return None;
        }
        let (t, l) = split(index);
        let tile = &self.tiles[t];
        Some(Body {
            pos: [tile.pos_x[l], tile.pos_y[l], tile.pos_z[l]],
            vel: [tile.vel_x[l], tile.vel_y[l], tile.vel_z[l]],
            inv_mass: tile.inv_mass[l],
            compliance_alpha: tile.compliance_alpha[l],
            collider_asset_id: tile.collider_asset_id[l],
            collision_layer_mask: tile.collision_layer_mask[l],
        })
    }

    #[inline]
    pub(crate) fn pos(&self, index: usize) -> [f32; 3] {
        let (t, l) = split(index);
        let tile = &self.tiles[t];
        [tile.pos_x[l], tile.pos_y[l], tile.pos_z[l]]
    }

    #[inline]
    pub(crate) fn set_pos(&mut self, index: usize, p: [f32; 3]) {
        let (t, l) = split(index);
        let tile = &mut self.tiles[t];
        tile.pos_x[l] = p[0];
        tile.pos_y[l] = p[1];
        tile.pos_z[l] = p[2];
    }

    #[inline]
    pub(crate) fn vel(&self, index: usize) -> [f32; 3] {
        let (t, l) = split(index);
        let tile = &self.tiles[t];
        [tile.vel_x[l], tile.vel_y[l], tile.vel_z[l]]
    }

    #[inline]
    pub(crate) fn set_vel(&mut self, index: usize, v: [f32; 3]) {
        let (t, l) = split(index);
        let tile = &mut self.tiles[t];
        tile.vel_x[l] = v[0];
        tile.vel_y[l] = v[1];
        tile.vel_z[l] = v[2];
    }

    #[inline]
    pub(crate) fn inv_mass(&self, index: usize) -> f32 {
        let (t, l) = split(index);
        self.tiles[t].inv_mass[l]
    }

    #[inline]
    pub(crate) fn compliance(&self, index: usize) -> f32 {
        let (t, l) = split(index);
        self.tiles[t].compliance_alpha[l]
    }

    #[inline]
    pub(crate) fn collider(&self, index: usize) -> u32 {
        let (t, l) = split(index);
        self.tiles[t].collider_asset_id[l]
    }

    #[inline]
    pub(crate) fn layer_mask(&self, index: usize) -> u32 {
        let (t, l) = split(index);
        self.tiles[t].collision_layer_mask[l]
    }
}
