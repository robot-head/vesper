//! XPBD soft bodies (§36): particles with distance and tetrahedral volume
//! constraints (Macklin, Müller and Chentanez, "XPBD", MIG 2016; Müller et
//! al., "Detailed Rigid Body Simulation with XPBD", 2020, for substepping).
//!
//! Each substep integrates the particles under gravity, projects every
//! constraint once in colour order ([`crate::coloring::MultiColorer`], so a
//! colour's constraints touch disjoint particles), resolves ground contact
//! with positional friction, and derives velocities `v = (x − x₀)/h`.
//!
//! * distance: `C = |x_a − x_b| − l₀`;
//! * volume: `C = 6 (V − V₀)` with `6V = (x₁ − x₀) × (x₂ − x₀) · (x₃ − x₀)`
//!   and gradients `∇₁ = (x₂ − x₀) × (x₃ − x₀)`, `∇₂ = (x₃ − x₀) × (x₁ −
//!   x₀)`, `∇₃ = (x₁ − x₀) × (x₂ − x₀)`, `∇₀ = −(∇₁ + ∇₂ + ∇₃)`;
//! * `Δλ = (−C − α̃ λ) / (Σ wᵢ |∇ᵢ|² + α̃)`, `α̃ = α / h²`, `Δxᵢ = wᵢ Δλ ∇ᵢ`.
//!
//! [`SoftWorld::set_solver_threads`] splits each colour across scoped
//! threads. Every constraint of a colour reads only its own particles,
//! which no other constraint of the colour writes, so each correction is
//! computed from the same values whichever thread computes it; the
//! corrections are then applied serially in the colour's visiting order.
//! The result is bit-identical for every thread count (and equal to the
//! one-thread Gauss–Seidel pass). Arithmetic is binary32 through
//! [`crate::world::det`]: no fused multiply-add, fixed order.

// `!(x > y)` is deliberate: it is also true when `x` is NaN.
#![allow(clippy::neg_cmp_op_on_partial_ord)]

use crate::coloring::{ColoringError, MultiColorer};
use crate::snapshot::WorldHashes;
use crate::world::det;
use sha2::Digest;

type V3 = [f32; 3];

/// Most solver threads.
pub const MAX_SOFT_THREADS: usize = 64;

/// One particle (`inv_mass == 0.0` pins it in place).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SoftParticle {
    pub pos: V3,
    pub vel: V3,
    pub inv_mass: f32,
}

/// A soft-body constraint.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SoftConstraint {
    /// Keep particles `a` and `b` at `rest` apart.
    Distance {
        a: u32,
        b: u32,
        rest: f32,
        compliance: f32,
    },
    /// Keep the signed volume of tetrahedron `tet` at `rest_volume`
    /// (positive: `tet` is positively oriented).
    Volume {
        tet: [u32; 4],
        rest_volume: f32,
        compliance: f32,
    },
}

/// Soft world parameters.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SoftConfig {
    pub dt: f32,
    pub substeps: u32,
    pub gravity: V3,
    /// Height of a static ground plane `y = h`.
    pub ground: Option<f32>,
    /// Positional friction coefficient against the ground.
    pub ground_friction: f32,
}

impl Default for SoftConfig {
    fn default() -> Self {
        Self {
            dt: 1.0 / 120.0,
            substeps: 16,
            gravity: [0.0, -9.81, 0.0],
            ground: Some(0.0),
            ground_friction: 0.5,
        }
    }
}

/// Typed soft-body failures. A failed step leaves the world unchanged.
#[derive(Debug, Clone, PartialEq)]
pub enum SoftError {
    InvalidConfig(&'static str),
    InvalidParticle {
        particle: usize,
    },
    InvalidConstraint {
        constraint: usize,
        reason: &'static str,
    },
    Coloring(ColoringError),
    InvalidThreads(usize),
    NonFinite {
        particle: usize,
    },
    TickOverflow,
}

impl std::fmt::Display for SoftError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidConfig(w) => write!(f, "invalid soft world configuration: {w}"),
            Self::InvalidParticle { particle } => write!(f, "particle {particle} is invalid"),
            Self::InvalidConstraint { constraint, reason } => {
                write!(f, "constraint {constraint}: {reason}")
            }
            Self::Coloring(e) => write!(f, "constraint colouring: {e}"),
            Self::InvalidThreads(n) => {
                write!(f, "solver threads must be 1..={MAX_SOFT_THREADS}, got {n}")
            }
            Self::NonFinite { particle } => write!(f, "particle {particle} left the finite range"),
            Self::TickOverflow => write!(f, "tick counter overflow"),
        }
    }
}

impl std::error::Error for SoftError {}

#[inline]
fn add(a: V3, b: V3) -> V3 {
    [
        det::add(a[0], b[0]),
        det::add(a[1], b[1]),
        det::add(a[2], b[2]),
    ]
}
#[inline]
fn sub(a: V3, b: V3) -> V3 {
    [
        det::sub(a[0], b[0]),
        det::sub(a[1], b[1]),
        det::sub(a[2], b[2]),
    ]
}
#[inline]
fn scale(a: V3, s: f32) -> V3 {
    [det::mul(a[0], s), det::mul(a[1], s), det::mul(a[2], s)]
}
#[inline]
fn cross(a: V3, b: V3) -> V3 {
    [
        det::sub(det::mul(a[1], b[2]), det::mul(a[2], b[1])),
        det::sub(det::mul(a[2], b[0]), det::mul(a[0], b[2])),
        det::sub(det::mul(a[0], b[1]), det::mul(a[1], b[0])),
    ]
}
#[inline]
fn length(a: V3) -> f32 {
    det::sqrt(det::dot(a, a))
}

/// Six times the signed volume of tetrahedron `p`.
#[inline]
fn six_volume(p: [V3; 4]) -> f32 {
    det::dot(cross(sub(p[1], p[0]), sub(p[2], p[0])), sub(p[3], p[0]))
}

/// Signed volume of a tetrahedron (binary64, for reporting).
fn volume64(p: [V3; 4]) -> f64 {
    let d = |a: V3, b: V3| {
        [
            f64::from(a[0]) - f64::from(b[0]),
            f64::from(a[1]) - f64::from(b[1]),
            f64::from(a[2]) - f64::from(b[2]),
        ]
    };
    let (u, v, w) = (d(p[1], p[0]), d(p[2], p[0]), d(p[3], p[0]));
    let c = [
        u[1] * v[2] - u[2] * v[1],
        u[2] * v[0] - u[0] * v[2],
        u[0] * v[1] - u[1] * v[0],
    ];
    (c[0] * w[0] + c[1] * w[1] + c[2] * w[2]) / 6.0
}

/// Corrections of one constraint: up to four particle deltas.
#[derive(Debug, Clone, Copy, Default)]
struct Correction {
    delta: [V3; 4],
    lambda: f32,
}

/// A deterministic XPBD soft-body world.
#[derive(Debug)]
pub struct SoftWorld {
    config: SoftConfig,
    pos: Vec<V3>,
    vel: Vec<V3>,
    inv_mass: Vec<f32>,
    prev: Vec<V3>,
    constraints: Vec<SoftConstraint>,
    bodies: Vec<u32>,
    offsets: Vec<u32>,
    lambda: Vec<f32>,
    colorer: MultiColorer,
    out: Vec<Correction>,
    threads: usize,
    parallel_threshold: usize,
    tick: u64,
}

impl SoftWorld {
    /// Build a world, validating every input. Constraints are coloured once
    /// here (the topology is fixed).
    pub fn new(
        config: SoftConfig,
        particles: &[SoftParticle],
        constraints: &[SoftConstraint],
    ) -> Result<Self, SoftError> {
        let finite3 = |v: V3| v.iter().all(|x| x.is_finite());
        if !(config.dt.is_finite() && config.dt > 0.0) {
            return Err(SoftError::InvalidConfig("dt must be finite and positive"));
        }
        if config.substeps == 0 || config.substeps > 64 {
            return Err(SoftError::InvalidConfig("substeps must be in 1..=64"));
        }
        if !finite3(config.gravity)
            || config.ground.is_some_and(|g| !g.is_finite())
            || !(config.ground_friction.is_finite() && config.ground_friction >= 0.0)
        {
            return Err(SoftError::InvalidConfig(
                "gravity, ground and friction must be finite (friction >= 0)",
            ));
        }
        if particles.len() > (u32::MAX / 2) as usize || constraints.len() > (u32::MAX / 8) as usize
        {
            return Err(SoftError::InvalidConfig(
                "too many particles or constraints",
            ));
        }
        for (i, p) in particles.iter().enumerate() {
            if !(finite3(p.pos) && finite3(p.vel) && p.inv_mass.is_finite() && p.inv_mass >= 0.0) {
                return Err(SoftError::InvalidParticle { particle: i });
            }
        }
        let n = particles.len() as u32;
        let mut bodies = Vec::with_capacity(constraints.len() * 4);
        let mut offsets = Vec::with_capacity(constraints.len() + 1);
        offsets.push(0u32);
        for (i, c) in constraints.iter().enumerate() {
            let bad = |reason| SoftError::InvalidConstraint {
                constraint: i,
                reason,
            };
            match *c {
                SoftConstraint::Distance {
                    a,
                    b,
                    rest,
                    compliance,
                } => {
                    if a >= n || b >= n || a == b {
                        return Err(bad("distance constraint names a bad particle pair"));
                    }
                    if !(rest.is_finite()
                        && rest >= 0.0
                        && compliance.is_finite()
                        && compliance >= 0.0)
                    {
                        return Err(bad("rest length and compliance must be finite and >= 0"));
                    }
                    bodies.extend([a, b]);
                }
                SoftConstraint::Volume {
                    tet,
                    rest_volume,
                    compliance,
                } => {
                    if tet.iter().any(|&p| p >= n) || (0..4).any(|k| tet[..k].contains(&tet[k])) {
                        return Err(bad("tetrahedron names bad or repeated particles"));
                    }
                    if !(rest_volume.is_finite() && rest_volume != 0.0) {
                        return Err(bad("tetrahedron rest volume must be finite and non-zero"));
                    }
                    if !(compliance.is_finite() && compliance >= 0.0) {
                        return Err(bad("compliance must be finite and >= 0"));
                    }
                    bodies.extend(tet);
                }
            }
            offsets.push(bodies.len() as u32);
        }
        let mut colorer = MultiColorer::new(particles.len(), constraints.len());
        colorer
            .color(&bodies, &offsets)
            .map_err(SoftError::Coloring)?;
        Ok(Self {
            config,
            pos: particles.iter().map(|p| p.pos).collect(),
            vel: particles
                .iter()
                .map(|p| if p.inv_mass == 0.0 { [0.0; 3] } else { p.vel })
                .collect(),
            inv_mass: particles.iter().map(|p| p.inv_mass).collect(),
            prev: vec![[0.0; 3]; particles.len()],
            constraints: constraints.to_vec(),
            bodies,
            offsets,
            lambda: vec![0.0; constraints.len()],
            colorer,
            out: vec![Correction::default(); constraints.len()],
            threads: 1,
            parallel_threshold: 64,
            tick: 0,
        })
    }

    /// A box of `cells` tetrahedralised cubes (5 tetrahedra each, mirrored
    /// in a checkerboard so faces match), with distance constraints on
    /// every tetrahedron edge (deduplicated) and a volume constraint per
    /// tetrahedron. Particles have equal mass `mass / count`.
    pub fn tetrahedral_box(
        origin: V3,
        cell: f32,
        cells: [u32; 3],
        mass: f32,
        compliance: f32,
        volume_compliance: f32,
    ) -> Result<(Vec<SoftParticle>, Vec<SoftConstraint>), SoftError> {
        if !(cell.is_finite() && cell > 0.0 && mass.is_finite() && mass > 0.0)
            || cells.iter().any(|c| *c == 0 || *c > 64)
        {
            return Err(SoftError::InvalidConfig(
                "cell size and mass must be positive, 1..=64 cells per axis",
            ));
        }
        let [nx, ny, nz] = cells.map(|c| c as usize + 1);
        let count = nx * ny * nz;
        let w = det::div(count as f32, mass);
        let idx = |i: usize, j: usize, k: usize| ((k * ny + j) * nx + i) as u32;
        let mut particles = Vec::with_capacity(count);
        for k in 0..nz {
            for j in 0..ny {
                for i in 0..nx {
                    particles.push(SoftParticle {
                        pos: [
                            det::add(origin[0], det::mul(cell, i as f32)),
                            det::add(origin[1], det::mul(cell, j as f32)),
                            det::add(origin[2], det::mul(cell, k as f32)),
                        ],
                        vel: [0.0; 3],
                        inv_mass: w,
                    });
                }
            }
        }
        // Five-tetrahedron split of a cube with corners c[0..8] (bit 0: x,
        // bit 1: y, bit 2: z), and its mirror.
        const EVEN: [[usize; 4]; 5] = [
            [0, 1, 2, 4],
            [1, 3, 2, 7],
            [1, 4, 5, 7],
            [2, 7, 6, 4],
            [1, 2, 4, 7],
        ];
        const ODD: [[usize; 4]; 5] = [
            [1, 0, 3, 5],
            [0, 2, 3, 6],
            [0, 5, 4, 6],
            [3, 6, 7, 5],
            [0, 3, 5, 6],
        ];
        let mut tets = Vec::new();
        for k in 0..nz - 1 {
            for j in 0..ny - 1 {
                for i in 0..nx - 1 {
                    let c: Vec<u32> = (0..8)
                        .map(|b| idx(i + (b & 1), j + ((b >> 1) & 1), k + ((b >> 2) & 1)))
                        .collect();
                    let split = if (i + j + k) % 2 == 0 { &EVEN } else { &ODD };
                    for t in split {
                        tets.push(t.map(|v| c[v]));
                    }
                }
            }
        }
        let mut edges: Vec<(u32, u32)> = Vec::new();
        for t in &tets {
            for a in 0..4 {
                for b in a + 1..4 {
                    let (x, y) = (t[a].min(t[b]), t[a].max(t[b]));
                    edges.push((x, y));
                }
            }
        }
        edges.sort_unstable();
        edges.dedup();
        let mut constraints = Vec::with_capacity(edges.len() + tets.len());
        for (a, b) in edges {
            let rest = length(sub(particles[a as usize].pos, particles[b as usize].pos));
            constraints.push(SoftConstraint::Distance {
                a,
                b,
                rest,
                compliance,
            });
        }
        for mut t in tets {
            let p = t.map(|v| particles[v as usize].pos);
            let mut v = det::div(six_volume(p), 6.0);
            if v < 0.0 {
                t.swap(2, 3);
                v = det::neg(v);
            }
            constraints.push(SoftConstraint::Volume {
                tet: t,
                rest_volume: v,
                compliance: volume_compliance,
            });
        }
        Ok((particles, constraints))
    }

    /// Solve each colour on up to `threads` scoped threads (1 by default).
    /// The result is bit-identical for every count.
    pub fn set_solver_threads(&mut self, threads: usize) -> Result<(), SoftError> {
        if threads == 0 || threads > MAX_SOFT_THREADS {
            return Err(SoftError::InvalidThreads(threads));
        }
        self.threads = threads;
        Ok(())
    }

    /// Colours with fewer constraints than this stay on one thread.
    pub fn set_parallel_threshold(&mut self, min_constraints: usize) {
        self.parallel_threshold = min_constraints.max(1);
    }

    pub fn tick(&self) -> u64 {
        self.tick
    }

    pub fn len(&self) -> usize {
        self.pos.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pos.is_empty()
    }

    /// Particle `i`.
    pub fn particle(&self, i: usize) -> Option<SoftParticle> {
        Some(SoftParticle {
            pos: *self.pos.get(i)?,
            vel: self.vel[i],
            inv_mass: self.inv_mass[i],
        })
    }

    /// Colours of the constraint graph.
    pub fn layer_count(&self) -> usize {
        self.colorer.layer_count()
    }

    /// The colourer (for checking).
    pub fn colorer(&self) -> &MultiColorer {
        &self.colorer
    }

    /// Body lists and offsets of the constraints, as coloured.
    pub fn constraint_sets(&self) -> (&[u32], &[u32]) {
        (&self.bodies, &self.offsets)
    }

    /// Sum of the tetrahedra's signed volumes now (binary64).
    pub fn volume(&self) -> f64 {
        self.constraints
            .iter()
            .filter_map(|c| match c {
                SoftConstraint::Volume { tet, .. } => {
                    Some(volume64(tet.map(|v| self.pos[v as usize])))
                }
                SoftConstraint::Distance { .. } => None,
            })
            .sum()
    }

    /// Sum of the tetrahedra's rest volumes.
    pub fn rest_volume(&self) -> f64 {
        self.constraints
            .iter()
            .filter_map(|c| match c {
                SoftConstraint::Volume { rest_volume, .. } => Some(f64::from(*rest_volume)),
                SoftConstraint::Distance { .. } => None,
            })
            .sum()
    }

    /// Largest relative distance-constraint error `|C| / l₀`.
    pub fn max_strain(&self) -> f32 {
        let mut worst = 0.0f32;
        for c in &self.constraints {
            if let SoftConstraint::Distance { a, b, rest, .. } = *c
                && rest > 0.0
            {
                let l = length(sub(self.pos[a as usize], self.pos[b as usize]));
                worst = det::max(worst, det::div(det::sub(l, rest).abs(), rest));
            }
        }
        worst
    }

    /// SHA-256 and BLAKE3 of the tick, particle count and every particle's
    /// position, velocity and inverse mass bits (`u32le`).
    pub fn hash(&self) -> WorldHashes {
        let mut sha = sha2::Sha256::new();
        let mut b3 = blake3::Hasher::new();
        let mut feed = |bytes: &[u8]| {
            sha.update(bytes);
            b3.update(bytes);
        };
        feed(&self.tick.to_le_bytes());
        feed(&(self.pos.len() as u64).to_le_bytes());
        for i in 0..self.pos.len() {
            for v in self.pos[i]
                .iter()
                .chain(&self.vel[i])
                .chain([&self.inv_mass[i]])
            {
                feed(&v.to_bits().to_le_bytes());
            }
        }
        WorldHashes {
            sha256: sha.finalize().into(),
            blake3: *b3.finalize().as_bytes(),
        }
    }

    /// The correction of constraint `c` from the current positions.
    fn correct(
        c: &SoftConstraint,
        pos: &[V3],
        inv_mass: &[f32],
        lambda: f32,
        inv_h2: f32,
    ) -> Correction {
        let mut out = Correction {
            lambda,
            ..Correction::default()
        };
        match *c {
            SoftConstraint::Distance {
                a,
                b,
                rest,
                compliance,
            } => {
                let (wa, wb) = (inv_mass[a as usize], inv_mass[b as usize]);
                let d = sub(pos[a as usize], pos[b as usize]);
                let l = length(d);
                if !(l > 0.0) {
                    return out;
                }
                let n = scale(d, det::div(1.0, l));
                let alpha = det::mul(compliance, inv_h2);
                let denom = det::add(det::add(wa, wb), alpha);
                if !(denom > 0.0) {
                    return out;
                }
                let cval = det::sub(l, rest);
                let dl = det::div(det::neg(det::add(cval, det::mul(alpha, lambda))), denom);
                out.lambda = det::add(lambda, dl);
                out.delta[0] = scale(n, det::mul(wa, dl));
                out.delta[1] = scale(n, det::neg(det::mul(wb, dl)));
            }
            SoftConstraint::Volume {
                tet,
                rest_volume,
                compliance,
            } => {
                let p = tet.map(|v| pos[v as usize]);
                let w = tet.map(|v| inv_mass[v as usize]);
                let (e1, e2, e3) = (sub(p[1], p[0]), sub(p[2], p[0]), sub(p[3], p[0]));
                let g1 = cross(e2, e3);
                let g2 = cross(e3, e1);
                let g3 = cross(e1, e2);
                let g0 = scale(add(add(g1, g2), g3), -1.0);
                let g = [g0, g1, g2, g3];
                let mut wsum = 0.0f32;
                for k in 0..4 {
                    wsum = det::add(wsum, det::mul(w[k], det::dot(g[k], g[k])));
                }
                let alpha = det::mul(compliance, inv_h2);
                let denom = det::add(wsum, alpha);
                if !(denom > 0.0) {
                    return out;
                }
                let cval = det::sub(det::dot(g3, e3), det::mul(6.0, rest_volume));
                let dl = det::div(det::neg(det::add(cval, det::mul(alpha, lambda))), denom);
                out.lambda = det::add(lambda, dl);
                for k in 0..4 {
                    out.delta[k] = scale(g[k], det::mul(w[k], dl));
                }
            }
        }
        out
    }

    /// Advance one tick. On error the world is unchanged.
    pub fn step(&mut self) -> Result<WorldHashes, SoftError> {
        let next = self.tick.checked_add(1).ok_or(SoftError::TickOverflow)?;
        let saved = (self.pos.clone(), self.vel.clone());
        let h = det::div(self.config.dt, self.config.substeps as f32);
        let inv_h = det::div(1.0, h);
        let inv_h2 = det::mul(inv_h, inv_h);
        let g = self.config.gravity;
        for _ in 0..self.config.substeps {
            for i in 0..self.pos.len() {
                self.prev[i] = self.pos[i];
                if self.inv_mass[i] == 0.0 {
                    continue;
                }
                self.vel[i] = add(self.vel[i], scale(g, h));
                self.pos[i] = add(self.pos[i], scale(self.vel[i], h));
            }
            self.lambda.fill(0.0);
            for color in 0..self.colorer.layer_count() {
                self.solve_color(color, inv_h2);
            }
            if let Some(ground) = self.config.ground {
                self.ground_contact(ground);
            }
            for i in 0..self.pos.len() {
                if self.inv_mass[i] == 0.0 {
                    continue;
                }
                self.vel[i] = scale(sub(self.pos[i], self.prev[i]), inv_h);
            }
        }
        if let Some(particle) = (0..self.pos.len()).position(|i| {
            !(self.pos[i]
                .iter()
                .chain(&self.vel[i])
                .all(|v| v.is_finite()))
        }) {
            (self.pos, self.vel) = saved;
            return Err(SoftError::NonFinite { particle });
        }
        self.tick = next;
        Ok(self.hash())
    }

    fn solve_color(&mut self, color: usize, inv_h2: f32) {
        let layer = self.colorer.layer(color);
        let count = layer.len();
        let threads = if count >= self.parallel_threshold {
            self.threads.min(count)
        } else {
            1
        };
        let (pos, inv_mass, constraints, lambda) =
            (&self.pos, &self.inv_mass, &self.constraints, &self.lambda);
        let out = &mut self.out[..count];
        if threads <= 1 {
            for (slot, &ci) in out.iter_mut().zip(layer) {
                let ci = ci as usize;
                *slot = Self::correct(&constraints[ci], pos, inv_mass, lambda[ci], inv_h2);
            }
        } else {
            let chunk = count.div_ceil(threads);
            std::thread::scope(|scope| {
                for (slots, ids) in out.chunks_mut(chunk).zip(layer.chunks(chunk)) {
                    scope.spawn(move || {
                        for (slot, &ci) in slots.iter_mut().zip(ids) {
                            let ci = ci as usize;
                            *slot =
                                Self::correct(&constraints[ci], pos, inv_mass, lambda[ci], inv_h2);
                        }
                    });
                }
            });
        }
        // Serial apply in visiting order.
        for (k, &ci) in layer.iter().enumerate() {
            let ci = ci as usize;
            let corr = self.out[k];
            self.lambda[ci] = corr.lambda;
            let (lo, hi) = (self.offsets[ci] as usize, self.offsets[ci + 1] as usize);
            for (slot, &p) in self.bodies[lo..hi].iter().enumerate() {
                let p = p as usize;
                self.pos[p] = add(self.pos[p], corr.delta[slot]);
            }
        }
    }

    /// Ground plane: push penetrating particles up, then positional
    /// friction removes up to `μ · depth` of the tangential motion of the
    /// substep (all of it below that bound).
    fn ground_contact(&mut self, ground: f32) {
        let mu = self.config.ground_friction;
        for i in 0..self.pos.len() {
            if self.inv_mass[i] == 0.0 {
                continue;
            }
            let depth = det::sub(ground, self.pos[i][1]);
            if !(depth > 0.0) {
                continue;
            }
            self.pos[i][1] = ground;
            if mu > 0.0 {
                let dx = det::sub(self.pos[i][0], self.prev[i][0]);
                let dz = det::sub(self.pos[i][2], self.prev[i][2]);
                let t = det::sqrt(det::add(det::mul(dx, dx), det::mul(dz, dz)));
                if t > 0.0 {
                    let keep = det::max(0.0, det::sub(1.0, det::div(det::mul(mu, depth), t)));
                    self.pos[i][0] = det::add(self.prev[i][0], det::mul(dx, keep));
                    self.pos[i][2] = det::add(self.prev[i][2], det::mul(dz, keep));
                }
            }
        }
    }
}
