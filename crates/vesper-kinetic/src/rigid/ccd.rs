//! Continuous collision detection by conservative advancement.
//!
//! Without CCD, a contact is speculative: the narrowphase runs on the poses
//! at the start of the tick and the margin grows by each body's sweep, so
//! the solver sees the normal and contact points of the *start* pose. A fast
//! or fast-spinning body can then meet its partner on a different feature
//! than the one that was closest at the start, and a thin wall can be
//! crossed between substeps. With CCD a body whose sweep this tick exceeds
//! [`CcdConfig::motion_fraction`] of its smallest extent is advanced along
//! its ballistic path (`x + v·t + ½ g t²`, orientation integrated like a
//! substep) by conservative advancement: each iteration moves to
//! `s += d / (μ_a + μ_b)` where `d` is the GJK distance and `μ` bounds how
//! far any point of the body moves per unit of `s`. The manifold is then
//! built on the poses at the time of impact, so its normal and anchors are
//! the impact's. If no impact is found within the tick the speculative
//! contact of the start pose is kept. All arithmetic is fixed-order
//! binary32/binary64 as in the rest of the world.

use super::{
    GroundPlane, RigidError, RigidWorld, State, add, det, length, normalize_q, qmul, scale,
    world_core,
};
use crate::gjk;

/// When CCD runs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CcdConfig {
    /// A body is "fast" when its sweep this tick exceeds this fraction of
    /// its smallest extent (its rounding radius or smallest half extent).
    pub motion_fraction: f32,
    /// Conservative-advancement iterations per pair (at least 1).
    pub max_iterations: u32,
}

impl Default for CcdConfig {
    fn default() -> Self {
        Self {
            motion_fraction: 0.5,
            max_iterations: 32,
        }
    }
}

impl RigidWorld {
    /// Enable (or with `None` disable) continuous collision detection.
    pub fn set_ccd(&mut self, config: Option<CcdConfig>) -> Result<(), RigidError> {
        if let Some(c) = config
            && !(c.motion_fraction.is_finite() && c.motion_fraction > 0.0 && c.max_iterations > 0)
        {
            return Err(RigidError::InvalidConfig(
                "ccd motion_fraction must be positive and max_iterations >= 1",
            ));
        }
        self.ccd = config;
        Ok(())
    }

    /// Contacts of the last step built at a time of impact.
    pub fn last_ccd_hits(&self) -> usize {
        self.ccd_hits
    }

    /// Smallest extent of body `i`'s collider.
    fn min_extent(&self, i: usize) -> f32 {
        use super::ColliderShape as C;
        match &self.colliders[self.bodies[i].collider as usize].shape {
            C::Sphere { radius } | C::Capsule { radius, .. } | C::Segment { radius, .. } => *radius,
            C::Box { half_extents } => half_extents.iter().copied().fold(f32::MAX, det::min),
            C::ConvexHull { vertices } => {
                // Half the smallest axis-aligned extent of the hull.
                let mut lo = [f32::MAX; 3];
                let mut hi = [f32::MIN; 3];
                for v in vertices {
                    for k in 0..3 {
                        lo[k] = det::min(lo[k], v[k]);
                        hi[k] = det::max(hi[k], v[k]);
                    }
                }
                let e = (0..3)
                    .map(|k| det::sub(hi[k], lo[k]))
                    .fold(f32::MAX, det::min);
                det::mul(e, 0.5)
            }
        }
    }

    fn is_fast(&self, i: usize) -> bool {
        let Some(c) = self.ccd else {
            return false;
        };
        self.bodies[i].inv_mass > 0.0
            && self.sweeps[i] > det::mul(c.motion_fraction, self.min_extent(i))
    }

    /// Body `i`'s ballistic pose at fraction `s` of the tick.
    fn pose_at(&self, i: usize, s: f32) -> State {
        let mut st = self.bodies[i];
        if st.inv_mass == 0.0 || s == 0.0 {
            return st;
        }
        let t = det::mul(s, self.config.dt);
        let g = self.config.gravity;
        st.x = add(
            add(st.x, scale(st.v, t)),
            scale(g, det::mul(0.5, det::mul(t, t))),
        );
        let spin = qmul([st.w[0], st.w[1], st.w[2], 0.0], st.q);
        let half = det::mul(0.5, t);
        st.q = normalize_q([
            det::add(st.q[0], det::mul(half, spin[0])),
            det::add(st.q[1], det::mul(half, spin[1])),
            det::add(st.q[2], det::mul(half, spin[2])),
            det::add(st.q[3], det::mul(half, spin[3])),
        ]);
        st
    }

    /// Bound on how far any point of body `i` moves per unit of `s`:
    /// `(|v| + |g|·dt)·dt + |ω|·dt·R`. The normalized linear quaternion
    /// path turns by `2·atan(|ω| t / 2) ≤ |ω| t`, so the bound holds for
    /// [`RigidWorld::pose_at`].
    fn motion_bound(&self, i: usize) -> f64 {
        let st = &self.bodies[i];
        if st.inv_mass == 0.0 {
            return 0.0;
        }
        let dt = f64::from(self.config.dt);
        let len = |v: [f32; 3]| f64::from(length(v));
        (len(st.v) + len(self.config.gravity) * dt) * dt
            + len(st.w) * dt * f64::from(self.bound_radius(st.collider))
    }

    fn distance_at(
        &mut self,
        a: usize,
        b: usize,
        sa: &State,
        sb: &State,
    ) -> Result<f64, RigidError> {
        let ca = &self.colliders[sa.collider as usize].shape;
        let cb = &self.colliders[sb.collider as usize].shape;
        let ra = world_core(ca, sa, &mut self.verts_a);
        let rb = world_core(cb, sb, &mut self.verts_b);
        let shape_a = super::gjk_shape(&self.verts_a, ra);
        let shape_b = super::gjk_shape(&self.verts_b, rb);
        gjk::separation(&shape_a, &shape_b)
            .map(|s| s.distance)
            .map_err(|error| RigidError::Narrowphase { a, b, error })
    }

    /// Time-of-impact poses of `a` and `b`, when one of them is fast, they
    /// are apart by more than the contact margin now, and they meet within
    /// the tick.
    pub(super) fn ccd_pair(
        &mut self,
        a: usize,
        b: usize,
    ) -> Result<Option<(State, State)>, RigidError> {
        let Some(cfg) = self.ccd else {
            return Ok(None);
        };
        if !(self.is_fast(a) || self.is_fast(b)) {
            return Ok(None);
        }
        let mu = self.motion_bound(a) + self.motion_bound(b);
        if !(mu > 0.0) {
            return Ok(None);
        }
        let margin = f64::from(self.config.contact_margin);
        let target = 0.5 * margin;
        let (sa0, sb0) = (self.bodies[a], self.bodies[b]);
        let mut d = self.distance_at(a, b, &sa0, &sb0)?;
        if d <= margin {
            return Ok(None);
        }
        let mut s = 0.0f64;
        for _ in 0..cfg.max_iterations {
            let step = (d - target) / mu;
            s += step;
            if s > 1.0 {
                return Ok(None);
            }
            let (pa, pb) = (self.pose_at(a, s as f32), self.pose_at(b, s as f32));
            d = self.distance_at(a, b, &pa, &pb)?;
            if d <= margin {
                self.ccd_hits += 1;
                return Ok(Some((pa, pb)));
            }
        }
        Ok(None)
    }

    /// Time-of-impact pose of body `i` against the ground, when `i` is fast,
    /// above the margin now, and reaches the plane within the tick.
    pub(super) fn ccd_ground(&mut self, i: usize, ground: GroundPlane) -> Option<State> {
        let cfg = self.ccd?;
        if !self.is_fast(i) {
            return None;
        }
        let mu = self.motion_bound(i);
        let margin = f64::from(self.config.contact_margin);
        let target = 0.5 * margin;
        let h = f64::from(ground.height);
        let height = |w: &mut Self, st: &State| -> f64 {
            let shape = &w.colliders[st.collider as usize].shape;
            let r = world_core(shape, st, &mut w.verts_b);
            w.verts_b.iter().map(|v| v[1]).fold(f64::INFINITY, f64::min) - r - h
        };
        let s0 = self.bodies[i];
        let mut d = height(self, &s0);
        if d <= margin || !(mu > 0.0) {
            return None;
        }
        let mut s = 0.0f64;
        for _ in 0..cfg.max_iterations {
            s += (d - target) / mu;
            if s > 1.0 {
                return None;
            }
            let p = self.pose_at(i, s as f32);
            d = height(self, &p);
            if d <= margin {
                self.ccd_hits += 1;
                return Some(p);
            }
        }
        None
    }
}
