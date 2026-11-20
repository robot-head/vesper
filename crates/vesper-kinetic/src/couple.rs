//! Two-way soft-rigid coupling (§36): the particles of a
//! [`SoftWorld`](crate::soft::SoftWorld) against the bodies of a
//! [`RigidWorld`](crate::rigid::RigidWorld).
//!
//! [`CoupledWorld::step`] advances the rigid world one tick, then the soft
//! world one tick with an extra contact pass in every soft substep, then
//! applies the reaction impulses to the rigid bodies:
//!
//! * In each soft substep (after the soft ground contact), every dynamic
//!   particle, a sphere of [`CouplingConfig::particle_radius`], is tested in
//!   index order against every body in index order whose bounding sphere it
//!   reaches, by a binary64 GJK/EPA query that gives the signed distance
//!   `d`, the normal `n` from the body to the particle and the arm `r` of
//!   the witness point on the body. A penetration (`d < 0`) is projected
//!   like an XPBD contact with the generalized inverse masses `w_p = 1/m_p`
//!   and `w_b = 1/m_b + (r × n)ᵀ I⁻¹ (r × n)`: `Δλ = −d / (w_p + w_b)`, the
//!   particle moves by `w_p Δλ n` and the body gets the positional impulse
//!   `−Δλ n` at `r` (position and orientation). Positional Coulomb friction
//!   then removes the relative tangential motion of the substep (relative
//!   to the body's material point) with a positional impulse of at most
//!   `μ Δλ`, split the same way.
//! * Against a static body the particle's velocity is the usual `Δx / h` of
//!   its substep. A dynamic body is kinematic for the whole soft step, so
//!   its share `−Δλ` becomes velocity over the tick: the impulse `−Δλ / dt`
//!   at `r` is accumulated per body and applied to its linear and angular
//!   velocity after the soft step, and the particle's start position is
//!   moved so that its velocity changes by `Δx / dt` too. The two impulses
//!   are then equal and opposite. Against a dynamic body a contact
//!   therefore removes an approach velocity over about one tick rather
//!   than one substep (the positions never interpenetrate by more than one
//!   substep's motion).
//!
//! Every impulse therefore acts equally and oppositely on a particle and a
//! body, so linear momentum is conserved (checked in `tests/couple.rs`). The
//! order is fixed and the arithmetic is fixed-order binary32 (solver) and
//! binary64 (queries) as in the two worlds, so the result is bit-identical
//! for any thread count of either world.
//!
//! Not here: the bodies' response within the tick (a body reacts to the
//! soft contacts one tick late, which is the usual staggered two-way
//! coupling), self-collision of soft bodies, and a broadphase (the pass is
//! `O(particles × bodies × substeps)` with a bounding-sphere reject).

use crate::rigid::{RigidError, RigidWorld};
use crate::soft::{SoftError, SoftWorld};
use crate::world::det;
use sha2::Digest;

/// Tangential motion below this length gets no friction direction.
const TANGENT_MIN: f32 = 1.0e-12;

type V3 = [f32; 3];

/// Coupling parameters.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CouplingConfig {
    /// Collision radius of every particle, `> 0`.
    pub particle_radius: f32,
    /// Coulomb friction coefficient between particles and bodies, `>= 0`.
    pub friction: f32,
}

impl Default for CouplingConfig {
    fn default() -> Self {
        Self {
            particle_radius: 0.02,
            friction: 0.5,
        }
    }
}

/// Typed coupling failures.
#[derive(Debug, Clone, PartialEq)]
pub enum CoupledError {
    InvalidConfig(&'static str),
    /// The rigid step failed: neither world advanced.
    Rigid(RigidError),
    /// The soft step failed: the rigid world advanced, the soft world did
    /// not.
    Soft(SoftError),
    /// A contact query failed (non-finite particle) or the reaction made a
    /// body non-finite: both worlds advanced.
    Coupling(RigidError),
}

impl std::fmt::Display for CoupledError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidConfig(w) => write!(f, "invalid coupling: {w}"),
            Self::Rigid(e) => write!(f, "rigid step: {e}"),
            Self::Soft(e) => write!(f, "soft step: {e}"),
            Self::Coupling(e) => write!(f, "soft-rigid coupling: {e}"),
        }
    }
}

impl std::error::Error for CoupledError {}

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
fn dot(a: V3, b: V3) -> f32 {
    det::add(
        det::add(det::mul(a[0], b[0]), det::mul(a[1], b[1])),
        det::mul(a[2], b[2]),
    )
}

/// A rigid world and a soft world stepped together with two-way contact.
#[derive(Debug)]
pub struct CoupledWorld {
    rigid: RigidWorld,
    soft: SoftWorld,
    config: CouplingConfig,
    contacts: usize,
    /// Per body: accumulated linear and angular reaction impulses.
    acc: Vec<(V3, V3)>,
}

impl CoupledWorld {
    /// Couple two worlds with the same time step.
    pub fn new(
        rigid: RigidWorld,
        soft: SoftWorld,
        config: CouplingConfig,
    ) -> Result<Self, CoupledError> {
        if !(config.particle_radius.is_finite() && config.particle_radius > 0.0) {
            return Err(CoupledError::InvalidConfig(
                "particle_radius must be finite and positive",
            ));
        }
        if !(config.friction.is_finite() && config.friction >= 0.0) {
            return Err(CoupledError::InvalidConfig(
                "friction must be finite and >= 0",
            ));
        }
        if rigid.config().dt != soft.soft_config().dt {
            return Err(CoupledError::InvalidConfig(
                "the rigid and soft worlds need the same dt",
            ));
        }
        let nb = rigid.len();
        Ok(Self {
            rigid,
            soft,
            config,
            contacts: 0,
            acc: Vec::with_capacity(nb),
        })
    }

    pub fn rigid(&self) -> &RigidWorld {
        &self.rigid
    }

    pub fn rigid_mut(&mut self) -> &mut RigidWorld {
        &mut self.rigid
    }

    pub fn soft(&self) -> &SoftWorld {
        &self.soft
    }

    pub fn soft_mut(&mut self) -> &mut SoftWorld {
        &mut self.soft
    }

    /// Particle-body contacts resolved in the last step.
    pub fn last_contacts(&self) -> usize {
        self.contacts
    }

    /// SHA-256 over both worlds' SHA-256 hashes and the contact count.
    pub fn hash(&self) -> [u8; 32] {
        let mut sha = sha2::Sha256::new();
        sha.update(self.rigid.hash().sha256);
        sha.update(self.soft.hash().sha256);
        sha.update((self.contacts as u64).to_le_bytes());
        sha.finalize().into()
    }

    /// Step the rigid world, then the soft world against the bodies, then
    /// apply the reaction impulses to the bodies.
    pub fn step(&mut self) -> Result<[u8; 32], CoupledError> {
        self.rigid.step().map_err(CoupledError::Rigid)?;
        let nb = self.rigid.len();
        self.acc.clear();
        self.acc.resize(nb, ([0.0; 3], [0.0; 3]));
        let radius = self.config.particle_radius;
        let mu = self.config.friction;
        let dt = self.rigid.config().dt;
        let rigid = &mut self.rigid;
        let acc = &mut self.acc;
        let mut error = None;
        let mut contacts = 0usize;
        let mut pass = |pos: &mut [V3], prev: &mut [V3], inv_mass: &[f32], h: f32| {
            if error.is_some() {
                return;
            }
            for i in 0..pos.len() {
                if inv_mass[i] == 0.0 {
                    continue;
                }
                #[allow(clippy::needless_range_loop)] // `b` also names the body
                for b in 0..nb {
                    let (centre, bound) = rigid.bounding_sphere(b);
                    let reach = det::add(bound, det::mul(2.0, radius));
                    let off = sub(pos[i], centre);
                    if dot(off, off) > det::mul(reach, reach) {
                        continue;
                    }
                    let (d, n, arm) =
                        match rigid.sphere_query(b, pos[i].map(f64::from), f64::from(radius)) {
                            Ok(q) => q,
                            Err(e) => {
                                error = Some(e);
                                return;
                            }
                        };
                    if d.is_nan() || d >= 0.0 {
                        continue;
                    }
                    let wp = inv_mass[i];
                    let wb = rigid.point_weight(b, arm, n);
                    let wsum = det::add(wp, wb);
                    contacts += 1;
                    // Normal: Δλ = −d / (w_p + w_b), split by inverse mass.
                    let dl = det::div(-(d as f32), wsum);
                    let before = pos[i];
                    pos[i] = add(pos[i], scale(n, det::mul(wp, dl)));
                    rigid.push_at(b, arm, scale(n, det::neg(dl)));
                    let mut impulse = scale(n, det::neg(dl));
                    // Positional Coulomb friction on the relative tangential
                    // motion this substep, bounded by μ Δλ.
                    if mu > 0.0 {
                        let vb = rigid.point_velocity(b, arm);
                        let motion = sub(sub(pos[i], prev[i]), scale(vb, h));
                        let tan = sub(motion, scale(n, dot(motion, n)));
                        let lt = det::sqrt(dot(tan, tan));
                        if lt > TANGENT_MIN {
                            let t = scale(tan, det::div(1.0, lt));
                            let wt = det::add(wp, rigid.point_weight(b, arm, t));
                            let stop = det::div(lt, wt);
                            let limit = det::mul(mu, dl);
                            let lam = if stop < limit { stop } else { limit };
                            pos[i] = sub(pos[i], scale(t, det::mul(wp, lam)));
                            rigid.push_at(b, arm, scale(t, lam));
                            impulse = add(impulse, scale(t, lam));
                        }
                    }
                    if wb > 0.0 {
                        // A dynamic body was kinematic for the whole tick,
                        // so its share becomes velocity over `dt`; the
                        // particle's must too (its start position moves
                        // with it so that `(x − x₀)/h` changes by
                        // `Δx / dt`), and the impulses stay equal and
                        // opposite.
                        let dx = sub(pos[i], before);
                        prev[i] = add(prev[i], scale(dx, det::sub(1.0, det::div(h, dt))));
                        let p = scale(impulse, det::div(1.0, dt));
                        acc[b].0 = add(acc[b].0, p);
                        acc[b].1 = add(acc[b].1, cross(arm, p));
                    }
                }
            }
        };
        self.soft.step_with(&mut pass).map_err(CoupledError::Soft)?;
        if let Some(e) = error {
            return Err(CoupledError::Coupling(e));
        }
        for (b, &(p, l)) in self.acc.iter().enumerate() {
            if p != [0.0; 3] || l != [0.0; 3] {
                self.rigid.impulse_sum(b, p, l);
            }
        }
        if let Some(body) = self.rigid.first_non_finite() {
            return Err(CoupledError::Coupling(RigidError::NonFinite { body }));
        }
        self.contacts = contacts;
        Ok(self.hash())
    }
}
