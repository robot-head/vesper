//! Fixed-timestep XPBD world (§36.3): Morton-cell broadphase, deterministic
//! colouring and graph-coloured positional projection, with no allocation per
//! step after construction.
//!
//! All float arithmetic goes through [`det`] helpers: every operation is a
//! single IEEE-754 binary32 operation in round-to-nearest-even. Rust never
//! contracts `a * b + c` into a fused multiply-add (it has no fast-math
//! contraction; `f32::mul_add` is the only way to get one and this crate never
//! calls it), and the only non-basic operation used is `sqrt`, which IEEE-754
//! requires to be correctly rounded. No transcendental function is called.

use crate::coloring::{Colorer, ColoringError, ConstraintBodies};
use crate::morton;
use crate::snapshot::{KineticSnapshotWireHdr, hash_world};
use crate::tile::{Body, TiledBodies};

/// Deterministic binary32 operations (`det_add`, `det_mul`, ... in §36.3).
pub mod det {
    #[inline]
    pub fn add(a: f32, b: f32) -> f32 {
        a + b
    }
    #[inline]
    pub fn sub(a: f32, b: f32) -> f32 {
        a - b
    }
    #[inline]
    pub fn mul(a: f32, b: f32) -> f32 {
        a * b
    }
    #[inline]
    pub fn div(a: f32, b: f32) -> f32 {
        a / b
    }
    #[inline]
    pub fn neg(a: f32) -> f32 {
        -a
    }
    #[inline]
    pub fn sqrt(a: f32) -> f32 {
        a.sqrt()
    }
    /// `max` with a fixed answer for every input: `b` unless `a > b`, so
    /// signed zeros and NaNs never depend on the platform `max`.
    #[inline]
    pub fn max(a: f32, b: f32) -> f32 {
        if a > b { a } else { b }
    }
    /// Three-component dot product, evaluated left to right.
    #[inline]
    pub fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
        add(add(mul(a[0], b[0]), mul(a[1], b[1])), mul(a[2], b[2]))
    }
}

/// A distance joint between two bodies (XPBD compliance `alpha`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DistanceConstraint {
    pub a: u32,
    pub b: u32,
    pub rest_length: f32,
    pub compliance: f32,
}

/// Sphere collider referenced by a body's `collider_asset_id`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SphereCollider {
    pub radius: f32,
}

/// World parameters.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WorldConfig {
    /// Fixed tick (default 1/120 s).
    pub dt: f32,
    /// XPBD substeps per tick (default 4).
    pub substeps: u32,
    pub gravity: [f32; 3],
    /// Height of an infinite static ground plane, if any.
    pub ground_y: Option<f32>,
    /// Broadphase cell edge; must be at least the largest collider diameter
    /// plus `contact_margin`.
    pub cell_size: f32,
    /// Corner of the 1024³-cell broadphase grid; positions outside clamp to
    /// the border cells (still correct, just more candidate pairs).
    pub grid_origin: [f32; 3],
    /// Speculative distance at which a contact is kept for the tick.
    pub contact_margin: f32,
    /// Contact storage reserved at construction.
    pub max_contacts: usize,
}

impl Default for WorldConfig {
    fn default() -> Self {
        Self {
            dt: 1.0 / 120.0,
            substeps: 4,
            gravity: [0.0, -9.81, 0.0],
            ground_y: Some(0.0),
            cell_size: 1.0,
            grid_origin: [-512.0, -512.0, -512.0],
            contact_margin: 0.05,
            max_contacts: 4096,
        }
    }
}

/// Typed world construction and step failures. A failed step leaves the
/// world unchanged.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StepError {
    /// Bad configuration value.
    InvalidConfig(&'static str),
    /// A body or joint field is non-finite or negative where it must not be.
    InvalidBody { body: usize, field: &'static str },
    /// A body names a collider that does not exist.
    UnknownCollider { body: usize, collider: u32 },
    /// A joint is malformed.
    InvalidJoint { joint: usize, reason: &'static str },
    /// More candidate contacts than `max_contacts`.
    ContactOverflow { capacity: usize },
    /// Colouring failed.
    Coloring(ColoringError),
    /// The tick counter would overflow.
    TickOverflow,
    /// Bodies move so fast that the swept contact search would need more
    /// than [`MAX_SEARCH_RADIUS`] cells in each direction.
    SweepTooLarge { body: usize },
}

impl std::fmt::Display for StepError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidConfig(what) => write!(f, "invalid world configuration: {what}"),
            Self::InvalidBody { body, field } => write!(f, "body {body} has invalid {field}"),
            Self::UnknownCollider { body, collider } => {
                write!(f, "body {body} names unknown collider {collider}")
            }
            Self::InvalidJoint { joint, reason } => write!(f, "joint {joint}: {reason}"),
            Self::ContactOverflow { capacity } => {
                write!(f, "more than {capacity} contacts in one tick")
            }
            Self::Coloring(e) => write!(f, "constraint colouring: {e}"),
            Self::TickOverflow => write!(f, "tick counter overflow"),
            Self::SweepTooLarge { body } => write!(
                f,
                "body {body} moves too far in one tick for the swept contact search"
            ),
        }
    }
}

impl std::error::Error for StepError {}

impl From<ColoringError> for StepError {
    fn from(e: ColoringError) -> Self {
        Self::Coloring(e)
    }
}

/// Most broadphase cells searched in each direction around a body. The
/// search radius grows with the largest per-tick sweep so that fast bodies
/// still meet every body they can reach during the tick's substeps.
pub const MAX_SEARCH_RADIUS: i32 = 4;

static NEXT_WORLD_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

#[derive(Debug, Clone, Copy)]
enum Contact {
    Pair(u32, u32),
    Ground(u32),
}

/// A deterministic XPBD world.
#[derive(Debug)]
pub struct World {
    config: WorldConfig,
    pub(crate) bodies: TiledBodies,
    pub(crate) tick: u64,
    pub(crate) id: u64,
    colliders: Vec<SphereCollider>,
    joints: Vec<DistanceConstraint>,
    // Per-step scratch, reserved at construction.
    prev: Vec<[f32; 3]>,
    sweep: Vec<f32>,
    cells: Vec<(u32, u32)>,
    contacts: Vec<Contact>,
    constraint_bodies: Vec<ConstraintBodies>,
    colorer: Colorer,
    last_layer_count: usize,
}

fn finite(v: f32) -> bool {
    v.is_finite()
}

impl World {
    /// Build a world. Validates every input and reserves all per-step
    /// storage.
    pub fn new(
        config: WorldConfig,
        bodies: &[Body],
        colliders: &[SphereCollider],
        joints: &[DistanceConstraint],
    ) -> Result<Self, StepError> {
        if !(finite(config.dt) && config.dt > 0.0) {
            return Err(StepError::InvalidConfig("dt must be finite and positive"));
        }
        if config.substeps == 0 || config.substeps > 64 {
            return Err(StepError::InvalidConfig("substeps must be in 1..=64"));
        }
        if !(config.gravity.iter().all(|g| finite(*g))
            && config.grid_origin.iter().all(|g| finite(*g))
            && config.ground_y.is_none_or(finite))
        {
            return Err(StepError::InvalidConfig("non-finite vector"));
        }
        if !(finite(config.cell_size) && config.cell_size > 0.0) {
            return Err(StepError::InvalidConfig("cell_size must be positive"));
        }
        if !(finite(config.contact_margin) && config.contact_margin >= 0.0) {
            return Err(StepError::InvalidConfig("contact_margin must be >= 0"));
        }
        if bodies.len() > u32::MAX as usize / 2 {
            return Err(StepError::InvalidConfig("too many bodies"));
        }
        let mut max_diameter = 0.0f32;
        for c in colliders {
            if !(finite(c.radius) && c.radius > 0.0) {
                return Err(StepError::InvalidConfig("collider radius must be positive"));
            }
            max_diameter = det::max(det::add(c.radius, c.radius), max_diameter);
        }
        if det::add(max_diameter, config.contact_margin) > config.cell_size {
            return Err(StepError::InvalidConfig(
                "cell_size smaller than largest collider diameter plus margin",
            ));
        }
        for (i, b) in bodies.iter().enumerate() {
            if !b.pos.iter().chain(&b.vel).all(|v| finite(*v)) {
                return Err(StepError::InvalidBody {
                    body: i,
                    field: "position or velocity",
                });
            }
            if !(finite(b.inv_mass) && b.inv_mass >= 0.0) {
                return Err(StepError::InvalidBody {
                    body: i,
                    field: "inv_mass",
                });
            }
            if !(finite(b.compliance_alpha) && b.compliance_alpha >= 0.0) {
                return Err(StepError::InvalidBody {
                    body: i,
                    field: "compliance_alpha",
                });
            }
            if b.collider_asset_id as usize >= colliders.len() {
                return Err(StepError::UnknownCollider {
                    body: i,
                    collider: b.collider_asset_id,
                });
            }
        }
        for (j, joint) in joints.iter().enumerate() {
            if joint.a as usize >= bodies.len() || joint.b as usize >= bodies.len() {
                return Err(StepError::InvalidJoint {
                    joint: j,
                    reason: "unknown body",
                });
            }
            if joint.a == joint.b {
                return Err(StepError::InvalidJoint {
                    joint: j,
                    reason: "links a body to itself",
                });
            }
            if !(finite(joint.rest_length)
                && joint.rest_length >= 0.0
                && finite(joint.compliance)
                && joint.compliance >= 0.0)
            {
                return Err(StepError::InvalidJoint {
                    joint: j,
                    reason: "rest length and compliance must be finite and >= 0",
                });
            }
        }
        let constraint_capacity = joints
            .len()
            .checked_add(config.max_contacts)
            .ok_or(StepError::InvalidConfig("max_contacts too large"))?;
        Ok(Self {
            config,
            bodies: TiledBodies::from_bodies(bodies),
            tick: 0,
            id: NEXT_WORLD_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            colliders: colliders.to_vec(),
            joints: joints.to_vec(),
            prev: vec![[0.0; 3]; bodies.len()],
            sweep: vec![0.0; bodies.len()],
            cells: Vec::with_capacity(bodies.len()),
            contacts: Vec::with_capacity(config.max_contacts),
            constraint_bodies: Vec::with_capacity(constraint_capacity),
            colorer: Colorer::new(bodies.len(), constraint_capacity),
            last_layer_count: 0,
        })
    }

    /// Ticks simulated so far.
    pub fn tick(&self) -> u64 {
        self.tick
    }

    /// The bodies.
    pub fn bodies(&self) -> &TiledBodies {
        &self.bodies
    }

    /// The configuration.
    pub fn config(&self) -> &WorldConfig {
        &self.config
    }

    /// Colours used by the last step.
    pub fn last_layer_count(&self) -> usize {
        self.last_layer_count
    }

    /// Add `dv` to the velocity of a dynamic body (a rollback "input").
    /// Rejects unknown bodies, static bodies and non-finite deltas.
    pub fn apply_velocity_change(&mut self, body: usize, dv: [f32; 3]) -> Result<(), StepError> {
        if body >= self.bodies.len() || !dv.iter().all(|v| finite(*v)) {
            return Err(StepError::InvalidBody {
                body,
                field: "velocity change",
            });
        }
        if self.bodies.inv_mass(body) == 0.0 {
            return Err(StepError::InvalidBody {
                body,
                field: "velocity change on a static body",
            });
        }
        let v = self.bodies.vel(body);
        let next = [
            det::add(v[0], dv[0]),
            det::add(v[1], dv[1]),
            det::add(v[2], dv[2]),
        ];
        if !next.iter().all(|x| finite(*x)) {
            return Err(StepError::InvalidBody {
                body,
                field: "velocity change overflows",
            });
        }
        self.bodies.set_vel(body, next);
        Ok(())
    }

    fn radius(&self, body: usize) -> f32 {
        self.colliders[self.bodies.collider(body) as usize].radius
    }

    fn cell_of(&self, p: [f32; 3]) -> [u16; 3] {
        let mut out = [0u16; 3];
        for axis in 0..3 {
            let q = det::div(
                det::sub(p[axis], self.config.grid_origin[axis]),
                self.config.cell_size,
            )
            .floor();
            // Saturating float-to-int conversion is defined for every input.
            out[axis] = (q as i32).clamp(0, i32::from(morton::AXIS_MAX)) as u16;
        }
        out
    }

    /// Conservative bound on how far body `i` can travel this tick:
    /// `(|vx| + |vy| + |vz|)·dt + (|gx| + |gy| + |gz|)·dt²` (an L1 bound on the
    /// L2 distance covered by the substeps' ballistic motion; constraint
    /// corrections are not included).
    fn sweep_of(&self, i: usize) -> f32 {
        if self.bodies.inv_mass(i) == 0.0 {
            return 0.0;
        }
        let v = self.bodies.vel(i);
        let g = self.config.gravity;
        let dt = self.config.dt;
        let speed = det::add(det::add(v[0].abs(), v[1].abs()), v[2].abs());
        let accel = det::add(det::add(g[0].abs(), g[1].abs()), g[2].abs());
        det::add(det::mul(speed, dt), det::mul(det::mul(accel, dt), dt))
    }

    /// Broadphase and narrowphase: fills `self.contacts` with every pair (and
    /// ground contact) that is within reach during the tick: radii plus
    /// margin plus both bodies' sweeps. Contacts are kept for all substeps
    /// and only push when they actually penetrate.
    fn detect_contacts(&mut self) -> Result<(), StepError> {
        self.contacts.clear();
        self.cells.clear();
        let n = self.bodies.len();
        let mut max_sweep = 0.0f32;
        for i in 0..n {
            let s = self.sweep_of(i);
            if !s.is_finite() {
                return Err(StepError::SweepTooLarge { body: i });
            }
            self.sweep[i] = s;
            max_sweep = det::max(s, max_sweep);
            let [x, y, z] = self.cell_of(self.bodies.pos(i));
            self.cells.push((morton::encode_masked(x, y, z), i as u32));
        }
        self.cells.sort_unstable();
        let margin = self.config.contact_margin;
        let cell_size = self.config.cell_size;
        for i in 0..n {
            let wi = self.bodies.inv_mass(i);
            let pi = self.bodies.pos(i);
            let ri = self.radius(i);
            let si = self.sweep[i];
            if let Some(ground) = self.config.ground_y
                && wi > 0.0
                && det::sub(det::sub(pi[1], ri), ground) < det::add(margin, si)
            {
                self.push_contact(Contact::Ground(i as u32))?;
            }
            // Cells are at least one largest diameter plus margin wide, so a
            // partner within reach lies within `1 + ceil((si + max_sweep) /
            // cell_size)` cells.
            let extra = det::div(det::add(si, max_sweep), cell_size).ceil();
            if extra >= MAX_SEARCH_RADIUS as f32 {
                return Err(StepError::SweepTooLarge { body: i });
            }
            let r = 1 + extra as i32;
            let cell = self.cell_of(pi);
            for dz in -r..=r {
                for dy in -r..=r {
                    for dx in -r..=r {
                        let neighbour = [
                            i32::from(cell[0]) + dx,
                            i32::from(cell[1]) + dy,
                            i32::from(cell[2]) + dz,
                        ];
                        if neighbour
                            .iter()
                            .any(|c| *c < 0 || *c > i32::from(morton::AXIS_MAX))
                        {
                            continue;
                        }
                        let code = morton::encode_masked(
                            neighbour[0] as u16,
                            neighbour[1] as u16,
                            neighbour[2] as u16,
                        );
                        let start = self.cells.partition_point(|e| e.0 < code);
                        let mut k = start;
                        while k < self.cells.len() && self.cells[k].0 == code {
                            let j = self.cells[k].1 as usize;
                            k += 1;
                            if j <= i {
                                continue;
                            }
                            let wj = self.bodies.inv_mass(j);
                            if wi == 0.0 && wj == 0.0 {
                                continue;
                            }
                            if self.bodies.layer_mask(i) & self.bodies.layer_mask(j) == 0 {
                                continue;
                            }
                            let pj = self.bodies.pos(j);
                            let d = [
                                det::sub(pi[0], pj[0]),
                                det::sub(pi[1], pj[1]),
                                det::sub(pi[2], pj[2]),
                            ];
                            let reach = det::add(
                                det::add(det::add(ri, self.radius(j)), margin),
                                det::add(si, self.sweep[j]),
                            );
                            if det::dot(d, d) < det::mul(reach, reach) {
                                self.push_contact(Contact::Pair(i as u32, j as u32))?;
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn push_contact(&mut self, c: Contact) -> Result<(), StepError> {
        if self.contacts.len() == self.config.max_contacts {
            return Err(StepError::ContactOverflow {
                capacity: self.config.max_contacts,
            });
        }
        self.contacts.push(c);
        Ok(())
    }

    /// Advance one fixed tick. Returns the §36.2 snapshot header for the new
    /// state. On error the world is unchanged.
    pub fn step(&mut self) -> Result<KineticSnapshotWireHdr, StepError> {
        let next_tick = self.tick.checked_add(1).ok_or(StepError::TickOverflow)?;
        self.detect_contacts()?;
        self.constraint_bodies.clear();
        for j in &self.joints {
            self.constraint_bodies.push(ConstraintBodies {
                a: j.a,
                b: Some(j.b),
            });
        }
        for c in &self.contacts {
            self.constraint_bodies.push(match *c {
                Contact::Pair(a, b) => ConstraintBodies { a, b: Some(b) },
                Contact::Ground(a) => ConstraintBodies { a, b: None },
            });
        }
        self.colorer.color(&self.constraint_bodies)?;
        self.last_layer_count = self.colorer.layer_count();

        // Bodies are only mutated from here on; nothing below can fail.
        let h = det::div(self.config.dt, self.config.substeps as f32);
        let inv_h = det::div(1.0, h);
        let inv_h2 = det::mul(inv_h, inv_h);
        let g = self.config.gravity;
        let n = self.bodies.len();
        for _ in 0..self.config.substeps {
            for i in 0..n {
                let p = self.bodies.pos(i);
                self.prev[i] = p;
                if self.bodies.inv_mass(i) == 0.0 {
                    continue;
                }
                let v = self.bodies.vel(i);
                let v = [
                    det::add(v[0], det::mul(g[0], h)),
                    det::add(v[1], det::mul(g[1], h)),
                    det::add(v[2], det::mul(g[2], h)),
                ];
                self.bodies.set_vel(i, v);
                self.bodies.set_pos(
                    i,
                    [
                        det::add(p[0], det::mul(v[0], h)),
                        det::add(p[1], det::mul(v[1], h)),
                        det::add(p[2], det::mul(v[2], h)),
                    ],
                );
            }
            for color in 0..self.colorer.layer_count() {
                let layer_len = self.colorer.layer(color).len();
                for k in 0..layer_len {
                    let index = self.colorer.layer(color)[k] as usize;
                    self.project(index, inv_h2);
                }
            }
            for i in 0..n {
                if self.bodies.inv_mass(i) == 0.0 {
                    continue;
                }
                let p = self.bodies.pos(i);
                let q = self.prev[i];
                self.bodies.set_vel(
                    i,
                    [
                        det::mul(det::sub(p[0], q[0]), inv_h),
                        det::mul(det::sub(p[1], q[1]), inv_h),
                        det::mul(det::sub(p[2], q[2]), inv_h),
                    ],
                );
            }
        }
        self.tick = next_tick;
        Ok(self.header())
    }

    /// The snapshot header (tick, counts, BLAKE3 of canonical state).
    pub fn header(&self) -> KineticSnapshotWireHdr {
        KineticSnapshotWireHdr {
            tick_number: self.tick,
            active_body_count: self.bodies.len() as u32,
            manifold_count: self.contacts.len() as u32,
            world_state_blake3: hash_world(self.tick, &self.bodies).blake3,
        }
    }

    /// Project constraint `index` (joints first, then this tick's contacts).
    fn project(&mut self, index: usize, inv_h2: f32) {
        const EPS: f32 = 1.0e-9;
        let joints = self.joints.len();
        let (a, b, target, alpha, inequality) = if index < joints {
            let j = self.joints[index];
            (
                j.a as usize,
                Some(j.b as usize),
                j.rest_length,
                j.compliance,
                false,
            )
        } else {
            match self.contacts[index - joints] {
                Contact::Pair(a, b) => {
                    let (a, b) = (a as usize, b as usize);
                    (
                        a,
                        Some(b),
                        det::add(self.radius(a), self.radius(b)),
                        det::add(self.bodies.compliance(a), self.bodies.compliance(b)),
                        true,
                    )
                }
                Contact::Ground(a) => {
                    let a = a as usize;
                    (a, None, 0.0, self.bodies.compliance(a), true)
                }
            }
        };
        let alpha_tilde = det::mul(alpha, inv_h2);
        let wa = self.bodies.inv_mass(a);
        let pa = self.bodies.pos(a);
        match b {
            Some(b) => {
                let wb = self.bodies.inv_mass(b);
                let pb = self.bodies.pos(b);
                let d = [
                    det::sub(pa[0], pb[0]),
                    det::sub(pa[1], pb[1]),
                    det::sub(pa[2], pb[2]),
                ];
                let len = det::sqrt(det::dot(d, d));
                // Coincident centres have no direction; separate along +y
                // (a deterministic choice) so the correction is never dropped.
                let (len, d) = if len <= EPS {
                    (0.0, [0.0, 1.0, 0.0])
                } else {
                    (len, d)
                };
                let c = det::sub(len, target);
                if inequality && c >= 0.0 {
                    return;
                }
                let w = det::add(wa, wb);
                if w == 0.0 {
                    return;
                }
                let denom = det::max(det::add(w, alpha_tilde), EPS);
                let d_lambda = det::div(det::neg(c), denom);
                let inv_len = if len == 0.0 { 1.0 } else { det::div(1.0, len) };
                let n = [
                    det::mul(d[0], inv_len),
                    det::mul(d[1], inv_len),
                    det::mul(d[2], inv_len),
                ];
                let sa = det::mul(wa, d_lambda);
                let sb = det::mul(wb, d_lambda);
                self.bodies.set_pos(
                    a,
                    [
                        det::add(pa[0], det::mul(sa, n[0])),
                        det::add(pa[1], det::mul(sa, n[1])),
                        det::add(pa[2], det::mul(sa, n[2])),
                    ],
                );
                self.bodies.set_pos(
                    b,
                    [
                        det::sub(pb[0], det::mul(sb, n[0])),
                        det::sub(pb[1], det::mul(sb, n[1])),
                        det::sub(pb[2], det::mul(sb, n[2])),
                    ],
                );
            }
            None => {
                let ground = self.config.ground_y.unwrap_or(0.0);
                let c = det::sub(det::sub(pa[1], self.radius(a)), ground);
                if c >= 0.0 || wa == 0.0 {
                    return;
                }
                let denom = det::max(det::add(wa, alpha_tilde), EPS);
                let d_lambda = det::div(det::neg(c), denom);
                self.bodies
                    .set_pos(a, [pa[0], det::add(pa[1], det::mul(wa, d_lambda)), pa[2]]);
            }
        }
    }

    /// Total kinetic energy `Σ ½ m |v|²` over dynamic bodies (diagnostics).
    pub fn kinetic_energy(&self) -> f64 {
        (0..self.bodies.len())
            .filter(|&i| self.bodies.inv_mass(i) > 0.0)
            .map(|i| {
                let v = self.bodies.vel(i).map(f64::from);
                0.5 * (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]) / f64::from(self.bodies.inv_mass(i))
            })
            .sum()
    }

    /// Total gravitational potential energy relative to `y = 0`
    /// (diagnostics, assumes gravity along -y).
    pub fn potential_energy(&self) -> f64 {
        let g = -f64::from(self.config.gravity[1]);
        (0..self.bodies.len())
            .filter(|&i| self.bodies.inv_mass(i) > 0.0)
            .map(|i| g * f64::from(self.bodies.pos(i)[1]) / f64::from(self.bodies.inv_mass(i)))
            .sum()
    }

    /// Largest violation of any distance joint, `|len - rest|`.
    pub fn max_joint_error(&self) -> f32 {
        let mut worst = 0.0f32;
        for j in &self.joints {
            let pa = self.bodies.pos(j.a as usize);
            let pb = self.bodies.pos(j.b as usize);
            let d = [pa[0] - pb[0], pa[1] - pb[1], pa[2] - pb[2]];
            let len = det::sqrt(det::dot(d, d));
            worst = det::max((len - j.rest_length).abs(), worst);
        }
        worst
    }

    /// Deepest penetration of any pair or ground contact (0 if none).
    pub fn max_penetration(&self) -> f32 {
        let n = self.bodies.len();
        let mut worst = 0.0f32;
        for i in 0..n {
            let pi = self.bodies.pos(i);
            let ri = self.radius(i);
            if let Some(ground) = self.config.ground_y
                && self.bodies.inv_mass(i) > 0.0
            {
                worst = det::max(ground - (pi[1] - ri), worst);
            }
            for j in i + 1..n {
                if self.bodies.layer_mask(i) & self.bodies.layer_mask(j) == 0 {
                    continue;
                }
                let pj = self.bodies.pos(j);
                let d = [pi[0] - pj[0], pi[1] - pj[1], pi[2] - pj[2]];
                let len = det::sqrt(det::dot(d, d));
                worst = det::max(ri + self.radius(j) - len, worst);
            }
        }
        worst
    }
}
