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
//!
//! **Re-sweeping** ([`RigidWorld::set_ccd_resweep`]). The ballistic path
//! ignores what a body hits earlier in the tick, so a fast body that
//! strikes a floor and slides into a wall gets its wall contact from the
//! path through the floor. With re-sweeping, times of impact are found for
//! every fast pair (and the ground) first, then processed in order of time
//! (ties by body indices). The earliest impact of a body is accepted on its
//! ballistic path and changes the body's path for the rest of the tick:
//! from the impact pose it moves with the velocity after a linear impulse
//! that removes the approach along the contact normal (with the contact's
//! restitution, split by inverse mass). Every later impact of a body whose
//! path changed is found again by conservative advancement along the new
//! paths from the latest change; one that no longer happens falls back to
//! the speculative contact of the start pose. Fast pairs with no impact on
//! their ballistic paths are kept too, and re-swept the same way when an
//! impact changes one of their paths. The re-swept impacts are
//! ordered by their first-pass times, so a re-swept impact can come later
//! than one processed after it: this is one pass of time-of-impact
//! ordering, not a full sub-stepping solver.

use super::{
    ContactManifold, GroundPlane, RigidError, RigidWorld, State, Toi, add, det, dot, length,
    normalize_q, qmul, scale, sub, world_core,
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

    /// Re-sweep later impacts of bodies whose path an earlier impact in the
    /// same tick changed (off by default; needs [`RigidWorld::set_ccd`]).
    pub fn set_ccd_resweep(&mut self, on: bool) {
        self.ccd_resweep = on;
    }

    /// Contacts of the last step built at a re-swept time of impact.
    pub fn last_ccd_resweeps(&self) -> usize {
        self.ccd_resweeps
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

    pub(super) fn is_fast(&self, i: usize) -> bool {
        let Some(c) = self.ccd else {
            return false;
        };
        self.bodies[i].inv_mass > 0.0
            && self.sweeps[i] > det::mul(c.motion_fraction, self.min_extent(i))
    }

    /// Body `i`'s ballistic pose at fraction `s` of the tick.
    fn pose_at(&self, i: usize, s: f32) -> State {
        self.ballistic(self.bodies[i], s)
    }

    /// The ballistic pose of `st` after fraction `s` of a tick.
    fn ballistic(&self, mut st: State, s: f32) -> State {
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
        self.motion_bound_of(&self.bodies[i])
    }

    fn motion_bound_of(&self, st: &State) -> f64 {
        if st.inv_mass == 0.0 {
            return 0.0;
        }
        let dt = f64::from(self.config.dt);
        let len = |v: [f32; 3]| f64::from(length(v));
        (len(st.v) + len(self.config.gravity) * dt) * dt
            + len(st.w) * dt * f64::from(self.bound_radius(st.collider))
    }

    #[allow(clippy::too_many_arguments)]
    fn distance_at(
        &self,
        a: usize,
        b: usize,
        sa: &State,
        sb: &State,
        va: &mut Vec<[f64; 3]>,
        vb: &mut Vec<[f64; 3]>,
    ) -> Result<f64, RigidError> {
        let ca = &self.colliders[sa.collider as usize].shape;
        let cb = &self.colliders[sb.collider as usize].shape;
        let ra = world_core(ca, sa, va);
        let rb = world_core(cb, sb, vb);
        let shape_a = super::gjk_shape(va, ra);
        let shape_b = super::gjk_shape(vb, rb);
        gjk::separation(&shape_a, &shape_b)
            .map(|s| s.distance)
            .map_err(|error| RigidError::Narrowphase { a, b, error })
    }

    /// Time-of-impact poses of `a` and `b`, when one of them is fast, they
    /// are apart by more than the contact margin now, and they meet within
    /// the tick.
    pub(super) fn ccd_pair(
        &self,
        a: usize,
        b: usize,
        va: &mut Vec<[f64; 3]>,
        vb: &mut Vec<[f64; 3]>,
    ) -> Result<Option<(f64, State, State)>, RigidError> {
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
        let mut d = self.distance_at(a, b, &sa0, &sb0, va, vb)?;
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
            d = self.distance_at(a, b, &pa, &pb, va, vb)?;
            if d <= margin {
                return Ok(Some((s, pa, pb)));
            }
        }
        Ok(None)
    }

    /// Time-of-impact pose of body `i` against the ground, when `i` is fast,
    /// above the margin now, and reaches the plane within the tick.
    pub(super) fn ccd_ground(&mut self, i: usize, ground: GroundPlane) -> Option<(f64, State)> {
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
                return Some((s, p));
            }
        }
        None
    }

    /// Body `i`'s pose at fraction `s` of the tick on its current path (the
    /// ballistic path, or the path after its last accepted impact).
    fn path(&self, i: usize, s: f64) -> State {
        match self.overrides[i] {
            Some((s0, st)) if s >= s0 => self.ballistic(st, (s - s0) as f32),
            _ => self.pose_at(i, s as f32),
        }
    }

    fn path_bound(&self, i: usize) -> f64 {
        let mut mu = self.motion_bound(i);
        if let Some((_, st)) = self.overrides[i] {
            mu = mu.max(self.motion_bound_of(&st));
        }
        mu
    }

    fn path_start(&self, i: usize) -> f64 {
        self.overrides[i].map_or(0.0, |(s0, _)| s0)
    }

    /// Conservative advancement of `a` against `b` (or the ground) along the
    /// current paths from fraction `from`.
    fn resweep(
        &mut self,
        a: usize,
        b: Option<usize>,
        from: f64,
    ) -> Result<Option<(f64, State, State)>, RigidError> {
        let Some(cfg) = self.ccd else {
            return Ok(None);
        };
        let mu = self.path_bound(a) + b.map_or(0.0, |b| self.path_bound(b));
        let margin = f64::from(self.config.contact_margin);
        let target = 0.5 * margin;
        let mut va = std::mem::take(&mut self.verts_a);
        let mut vb = std::mem::take(&mut self.verts_b);
        let ground = self.config.ground;
        let mut gap = |w: &Self, s: f64| -> Result<(f64, State, State), RigidError> {
            let pa = w.path(a, s);
            match b {
                Some(b) => {
                    let pb = w.path(b, s);
                    Ok((w.distance_at(a, b, &pa, &pb, &mut va, &mut vb)?, pa, pb))
                }
                None => {
                    let h = ground.map_or(f64::NEG_INFINITY, |g| f64::from(g.height));
                    let shape = &w.colliders[pa.collider as usize].shape;
                    let r = world_core(shape, &pa, &mut vb);
                    let low = vb.iter().map(|v| v[1]).fold(f64::INFINITY, f64::min);
                    Ok((low - r - h, pa, pa))
                }
            }
        };
        let mut s = from;
        let mut result = Ok(None);
        for _ in 0..=cfg.max_iterations {
            match gap(self, s) {
                Err(e) => {
                    result = Err(e);
                    break;
                }
                Ok((d, pa, pb)) => {
                    if d <= margin {
                        result = Ok(Some((s, pa, pb)));
                        break;
                    }
                    if !(mu > 0.0) {
                        break;
                    }
                    s += (d - target) / mu;
                    if s > 1.0 {
                        break;
                    }
                }
            }
        }
        self.verts_a = va;
        self.verts_b = vb;
        result
    }

    /// Change the paths of `m`'s bodies after an impact at fraction `s`
    /// with poses `pa`, `pb`: velocity at the impact, then a linear impulse
    /// that removes the approach along the normal (with restitution).
    fn redirect(&mut self, m: &ContactManifold, s: f64, mut pa: State, mut pb: State) {
        let t = det::mul(s as f32, self.config.dt);
        let g = self.config.gravity;
        let a = m.a as usize;
        if pa.inv_mass > 0.0 {
            pa.v = add(pa.v, scale(g, t));
        }
        let wb = match m.b {
            Some(_) if pb.inv_mass > 0.0 => {
                pb.v = add(pb.v, scale(g, t));
                pb.inv_mass
            }
            _ => 0.0,
        };
        let vb = if m.b.is_some() { pb.v } else { [0.0; 3] };
        let vn = dot(sub(pa.v, vb), m.normal);
        let wsum = det::add(pa.inv_mass, wb);
        if vn < 0.0 && wsum > 0.0 {
            let j = det::div(
                det::neg(det::mul(det::add(1.0, m.material.restitution), vn)),
                wsum,
            );
            pa.v = add(pa.v, scale(m.normal, det::mul(j, pa.inv_mass)));
            if m.b.is_some() {
                pb.v = sub(pb.v, scale(m.normal, det::mul(j, wb)));
            }
        }
        if pa.inv_mass > 0.0 {
            self.overrides[a] = Some((s, pa));
        }
        if let Some(b) = m.b
            && pb.inv_mass > 0.0
        {
            self.overrides[b as usize] = Some((s, pb));
        }
    }

    /// Build the deferred time-of-impact manifolds in time order, re-sweeping
    /// impacts of bodies whose path an earlier impact changed.
    pub(super) fn resolve_tois(&mut self) -> Result<(), RigidError> {
        let mut tois = std::mem::take(&mut self.tois);
        tois.sort_unstable_by_key(|t| (t.s.to_bits(), t.a, t.b.unwrap_or(u32::MAX)));
        self.overrides.fill(None);
        let mut result = Ok(());
        for t in &tois {
            result = self.resolve_toi(t);
            if result.is_err() {
                break;
            }
        }
        tois.clear();
        self.tois = tois;
        result
    }

    fn resolve_toi(&mut self, t: &Toi) -> Result<(), RigidError> {
        let a = t.a as usize;
        let b = t.b.map(|b| b as usize);
        let changed = self.overrides[a].is_some() || b.is_some_and(|b| self.overrides[b].is_some());
        let hit = if changed {
            let from = self
                .path_start(a)
                .max(b.map_or(0.0, |b| self.path_start(b)));
            let hit = self.resweep(a, b, from)?;
            if hit.is_some() {
                self.ccd_resweeps += 1;
            }
            hit
        } else if t.s <= 1.0 {
            self.ccd_hits += 1;
            Some((t.s, t.sa, t.sb))
        } else {
            None
        };
        let before = self.manifolds.len();
        match (b, hit) {
            (Some(b), Some((s, pa, pb))) => {
                let mut va = std::mem::take(&mut self.verts_a);
                let mut vb = std::mem::take(&mut self.verts_b);
                let m = self.pair_manifold_at(a, b, pa, pb, &mut va, &mut vb);
                self.verts_a = va;
                self.verts_b = vb;
                if let Some(m) = m? {
                    self.push_manifold(m)?;
                    self.redirect(&m, s, pa, pb);
                }
            }
            (Some(b), None) => {
                let (sa, sb) = (self.bodies[a], self.bodies[b]);
                let mut va = std::mem::take(&mut self.verts_a);
                let mut vb = std::mem::take(&mut self.verts_b);
                let m = self.pair_manifold_at(a, b, sa, sb, &mut va, &mut vb);
                self.verts_a = va;
                self.verts_b = vb;
                if let Some(m) = m? {
                    self.push_manifold(m)?;
                }
            }
            (None, hit) => {
                let Some(ground) = self.config.ground else {
                    return Ok(());
                };
                let (s, at) = hit.map_or((0.0, self.bodies[a]), |(s, p, _)| (s, p));
                let shape = &self.colliders[at.collider as usize].shape;
                let r = world_core(shape, &at, &mut self.verts_a);
                self.ground_manifold(a, at, ground, r)?;
                if hit.is_some() && self.manifolds.len() > before {
                    let m = self.manifolds[self.manifolds.len() - 1];
                    self.redirect(&m, s, at, at);
                }
            }
        }
        Ok(())
    }
}
