//! XPBD joints (Müller et al. 2020, §3.3): ball, hinge and prismatic, with
//! angle and distance limits.
//!
//! Every joint is a set of XPBD constraints solved in the position pass,
//! coloured together with the contact manifolds (joints follow the
//! manifolds in the constraint list), so a colour's joints and contacts
//! touch disjoint bodies. Each part is projected once per substep with
//!
//! `Δλ = (c − α̃ λ) / (w_a + w_b + α̃)`, `α̃ = α / h²`,
//!
//! where `c` is the violation and `w` the generalized inverse mass of the
//! part (positional: `1/m + (r × n)ᵀ I⁻¹ (r × n)`; angular: `nᵀ I⁻¹ n`).
//! Velocities follow from the positional corrections like every other
//! constraint. Parts, in solve order:
//!
//! * hinge: align the hinge axes (`Δq = a_a × a_b`), then the optional angle
//!   limit about the axis, then the anchors;
//! * prismatic: lock the relative orientation to the rest orientation, then
//!   pin the anchors' offset perpendicular to the slide axis and clamp it
//!   along the axis to the optional limits;
//! * ball: pin the anchors together.
//!
//! With `b == None` the joint attaches `a` to the static world: `anchor_b`
//! and hinge `axis_b` are world coordinates.

use super::{
    IDENTITY, Q, RigidError, RigidWorld, State, V3, add, angular_weight, conj, cross, det, dot,
    finite3, length, normalize_q, perpendicular, qmul, rotate, scale, sub,
};
use crate::fastmath;

/// Joint type.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum JointKind {
    /// Anchors coincide; rotation is free.
    Ball,
    /// Anchors coincide and `axis_a` (in `a`'s frame) stays aligned with
    /// `axis_b` (in `b`'s frame, or world). `limits` bounds the rotation of
    /// `b` relative to `a` about the axis, in radians within `[-π, π]`,
    /// measured from the pose at [`RigidWorld::add_joint`].
    Hinge {
        axis_a: V3,
        axis_b: V3,
        limits: Option<[f32; 2]>,
    },
    /// The relative orientation stays as it was at
    /// [`RigidWorld::add_joint`]; `b`'s anchor moves only along `axis_a`
    /// (in `a`'s frame) through `a`'s anchor, between the optional
    /// `limits` (metres along the axis).
    Prismatic {
        axis_a: V3,
        limits: Option<[f32; 2]>,
    },
}

/// A joint between body `a` and body `b` (or the world).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RigidJoint {
    pub a: u32,
    pub b: Option<u32>,
    /// Anchor in `a`'s frame.
    pub anchor_a: V3,
    /// Anchor in `b`'s frame (world point when `b == None`).
    pub anchor_b: V3,
    pub kind: JointKind,
    /// Positional compliance (m/N; `0` is rigid).
    pub compliance: f32,
    /// Angular compliance (rad/(N·m); `0` is rigid).
    pub angular_compliance: f32,
    /// Whether `a` and `b` still collide with each other.
    pub collide_connected: bool,
}

impl RigidJoint {
    /// A rigid ball joint.
    pub fn ball(a: u32, b: Option<u32>, anchor_a: V3, anchor_b: V3) -> Self {
        Self {
            a,
            b,
            anchor_a,
            anchor_b,
            kind: JointKind::Ball,
            compliance: 0.0,
            angular_compliance: 0.0,
            collide_connected: false,
        }
    }
}

/// A joint and its solver state.
#[derive(Debug, Clone, Copy)]
pub(super) struct JointState {
    pub(super) joint: RigidJoint,
    /// Unit axes (hinge: `a` and `b` frames; prismatic: `a` frame).
    axis_a: V3,
    axis_b: V3,
    /// Hinge limit references, perpendicular to the axes.
    ref_a: V3,
    ref_b: V3,
    /// Prismatic rest orientation `q̄_b q_a` at creation.
    rest: Q,
    lambda_pos: f32,
    lambda_ang: f32,
    lambda_lim: f32,
    /// Positional λ of the last substep, for the constraint force.
    last_lambda: f32,
}

impl JointState {
    pub(super) fn reset(&mut self) {
        self.lambda_pos = 0.0;
        self.lambda_ang = 0.0;
        self.lambda_lim = 0.0;
    }
}

fn unit(v: V3) -> Option<V3> {
    let l = length(v);
    (l.is_finite() && l > 1.0e-6).then(|| scale(v, det::div(1.0, l)))
}

impl RigidWorld {
    fn pose(&self, b: Option<u32>) -> (V3, Q) {
        match b {
            Some(b) => {
                let s = &self.bodies[b as usize];
                (s.x, s.q)
            }
            None => ([0.0; 3], IDENTITY),
        }
    }

    /// Add a joint; returns its index. Hinge axes and limit references are
    /// captured from the current poses. Bodies joined with
    /// `collide_connected == false` stop colliding with each other.
    pub fn add_joint(&mut self, joint: RigidJoint) -> Result<usize, RigidError> {
        let n = self.bodies.len();
        let bad = |reason| RigidError::InvalidJoint {
            joint: self.joints.len(),
            reason,
        };
        if joint.a as usize >= n || joint.b.is_some_and(|b| b as usize >= n) {
            return Err(bad("joint names an unknown body"));
        }
        if joint.b == Some(joint.a) {
            return Err(bad("joint links a body to itself"));
        }
        let fixed = |i: Option<u32>| i.is_none_or(|i| self.bodies[i as usize].inv_mass == 0.0);
        if fixed(Some(joint.a)) && fixed(joint.b) {
            return Err(bad("joint links two static bodies"));
        }
        if !(finite3(joint.anchor_a) && finite3(joint.anchor_b)) {
            return Err(bad("joint anchors must be finite"));
        }
        let ok = |c: f32| c.is_finite() && c >= 0.0;
        if !(ok(joint.compliance) && ok(joint.angular_compliance)) {
            return Err(bad("joint compliance must be finite and >= 0"));
        }
        if self.joints.len() + 1 > (u32::MAX / 4) as usize {
            return Err(bad("too many joints"));
        }
        let (_, qa) = self.pose(Some(joint.a));
        let (_, qb) = self.pose(joint.b);
        let mut st = JointState {
            joint,
            axis_a: [0.0; 3],
            axis_b: [0.0; 3],
            ref_a: [0.0; 3],
            ref_b: [0.0; 3],
            rest: IDENTITY,
            lambda_pos: 0.0,
            lambda_ang: 0.0,
            lambda_lim: 0.0,
            last_lambda: 0.0,
        };
        let check_limits = |l: Option<[f32; 2]>, max: f32| {
            l.is_none_or(|[lo, hi]| {
                lo.is_finite() && hi.is_finite() && lo <= hi && lo >= -max && hi <= max
            })
        };
        match joint.kind {
            JointKind::Ball => {}
            JointKind::Hinge {
                axis_a,
                axis_b,
                limits,
            } => {
                let (Some(ua), Some(ub)) = (unit(axis_a), unit(axis_b)) else {
                    return Err(bad("hinge axes must be finite and non-zero"));
                };
                if !check_limits(limits, std::f32::consts::PI) {
                    return Err(bad("hinge limits must satisfy -pi <= lo <= hi <= pi"));
                }
                st.axis_a = ua;
                st.axis_b = ub;
                st.ref_a = perpendicular(ua);
                // b's reference: a's reference carried into b's frame, then
                // made perpendicular to b's axis.
                let w = rotate(conj(qb), rotate(qa, st.ref_a));
                let w = sub(w, scale(ub, dot(w, ub)));
                st.ref_b = unit(w).unwrap_or_else(|| perpendicular(ub));
            }
            JointKind::Prismatic { axis_a, limits } => {
                let Some(ua) = unit(axis_a) else {
                    return Err(bad("prismatic axis must be finite and non-zero"));
                };
                if !check_limits(limits, f32::MAX) {
                    return Err(bad("prismatic limits must be finite with lo <= hi"));
                }
                st.axis_a = ua;
                st.rest = normalize_q(qmul(conj(qb), qa));
            }
        }
        self.joints.push(st);
        if !joint.collide_connected
            && let Some(b) = joint.b
        {
            let key = if joint.a < b {
                (joint.a, b)
            } else {
                (b, joint.a)
            };
            if let Err(at) = self.connected.binary_search(&key) {
                self.connected.insert(at, key);
            }
        }
        // Room for every manifold plus every joint in the colouring.
        let need = self.config.max_manifolds + self.joints.len();
        if self.colorer.capacity() < need {
            self.colorer = crate::coloring::Colorer::new(n, need);
        }
        self.constraint_bodies
            .reserve(need.saturating_sub(self.constraint_bodies.len()));
        Ok(self.joints.len() - 1)
    }

    /// Number of joints.
    pub fn joint_count(&self) -> usize {
        self.joints.len()
    }

    /// The joint at `i`.
    pub fn joint(&self, i: usize) -> Option<RigidJoint> {
        self.joints.get(i).map(|j| j.joint)
    }

    /// Magnitude of joint `i`'s positional constraint force over the last
    /// substep (`λ / h²`, newtons), or `None` past the end.
    pub fn joint_force(&self, i: usize) -> Option<f32> {
        let h = det::div(self.config.dt, self.config.substeps as f32);
        self.joints
            .get(i)
            .map(|j| det::div(j.last_lambda, det::mul(h, h)))
    }

    /// World anchors and arms of a joint.
    fn joint_points(&self, j: &JointState) -> (V3, V3, V3, V3) {
        let sa = &self.bodies[j.joint.a as usize];
        let ra = rotate(sa.q, j.joint.anchor_a);
        let pa = add(sa.x, ra);
        match j.joint.b {
            Some(b) => {
                let sb = &self.bodies[b as usize];
                let rb = rotate(sb.q, j.joint.anchor_b);
                (pa, ra, add(sb.x, rb), rb)
            }
            None => (pa, ra, j.joint.anchor_b, [0.0; 3]),
        }
    }

    fn body_or_none(&self, b: Option<u32>) -> Option<&State> {
        b.map(|b| &self.bodies[b as usize])
    }

    /// One XPBD positional part: move `a`'s anchor by `delta` towards `b`'s.
    fn positional_part(
        &mut self,
        (a, b): (u32, Option<u32>),
        (ra, rb): (V3, V3),
        delta: V3,
        alpha: f32,
        lambda: &mut f32,
    ) {
        let c = length(delta);
        if !(c > 0.0) {
            return;
        }
        let n = scale(delta, det::div(1.0, c));
        let sa = &self.bodies[a as usize];
        let mut w = det::add(sa.inv_mass, angular_weight(sa, cross(ra, n)));
        if let Some(sb) = self.body_or_none(b) {
            w = det::add(w, det::add(sb.inv_mass, angular_weight(sb, cross(rb, n))));
        }
        let denom = det::add(w, alpha);
        if !(denom > 0.0) {
            return;
        }
        let dl = det::div(det::sub(c, det::mul(alpha, *lambda)), denom);
        *lambda = det::add(*lambda, dl);
        self.apply_positional_to(a, b, ra, rb, scale(n, dl));
    }

    /// One XPBD angular part: rotate `a` by `dq` (and `b` by `-dq`), scaled
    /// by the generalized inverse inertia.
    fn angular_part(&mut self, a: u32, b: Option<u32>, dq: V3, alpha: f32, lambda: &mut f32) {
        let theta = length(dq);
        if !(theta > 0.0) {
            return;
        }
        let n = scale(dq, det::div(1.0, theta));
        let mut w = angular_weight(&self.bodies[a as usize], n);
        if let Some(sb) = self.body_or_none(b) {
            w = det::add(w, angular_weight(sb, n));
        }
        let denom = det::add(w, alpha);
        if !(denom > 0.0) {
            return;
        }
        let dl = det::div(det::sub(theta, det::mul(alpha, *lambda)), denom);
        *lambda = det::add(*lambda, dl);
        self.apply_rotation_to(a, b, scale(n, dl));
    }

    /// Project joint `k` once (one substep of length `h`).
    pub(super) fn project_joint(&mut self, k: usize, h: f32) {
        let mut j = self.joints[k];
        let (a, b) = (j.joint.a, j.joint.b);
        if self.sleep.is_some() && !self.live(a, b) {
            return;
        }
        let h2 = det::mul(h, h);
        let alpha = det::div(j.joint.compliance, h2);
        let alpha_ang = det::div(j.joint.angular_compliance, h2);
        match j.joint.kind {
            JointKind::Ball => {}
            JointKind::Hinge { limits, .. } => {
                let (_, qa) = self.pose(Some(a));
                let (_, qb) = self.pose(b);
                let aa = rotate(qa, j.axis_a);
                let ab = rotate(qb, j.axis_b);
                let mut lambda = j.lambda_ang;
                self.angular_part(a, b, cross(aa, ab), alpha_ang, &mut lambda);
                j.lambda_ang = lambda;
                if let Some([lo, hi]) = limits {
                    let (_, qa) = self.pose(Some(a));
                    let (_, qb) = self.pose(b);
                    let axis = rotate(qa, j.axis_a);
                    let ua = rotate(qa, j.ref_a);
                    let ub = rotate(qb, j.ref_b);
                    let sin = dot(cross(ua, ub), axis);
                    let cos = dot(ua, ub);
                    let phi = fastmath::atan2f(sin, cos);
                    let clamped = det::max(lo, if phi < hi { phi } else { hi });
                    if clamped != phi {
                        // Where b's reference should be: a's reference
                        // turned by the clamped angle about the axis.
                        let target = add(
                            scale(ua, fastmath::cosf(clamped)),
                            scale(cross(axis, ua), fastmath::sinf(clamped)),
                        );
                        let mut lambda = j.lambda_lim;
                        self.angular_part(a, b, cross(target, ub), alpha_ang, &mut lambda);
                        j.lambda_lim = lambda;
                    }
                }
            }
            JointKind::Prismatic { .. } => {
                let (_, qa) = self.pose(Some(a));
                let (_, qb) = self.pose(b);
                // a's target orientation is q_b · rest.
                let e = qmul(qmul(qb, j.rest), conj(qa));
                let e = if e[3] >= 0.0 {
                    e
                } else {
                    super::scale4(e, -1.0)
                };
                let mut lambda = j.lambda_ang;
                self.angular_part(
                    a,
                    b,
                    [
                        det::mul(2.0, e[0]),
                        det::mul(2.0, e[1]),
                        det::mul(2.0, e[2]),
                    ],
                    alpha_ang,
                    &mut lambda,
                );
                j.lambda_ang = lambda;
            }
        }
        let (pa, ra, pb, rb) = self.joint_points(&j);
        let d = sub(pb, pa);
        let delta = match j.joint.kind {
            JointKind::Prismatic { limits, .. } => {
                let (_, qa) = self.pose(Some(a));
                let axis = rotate(qa, j.axis_a);
                let s = dot(d, axis);
                let along = match limits {
                    Some([lo, hi]) => {
                        let c = det::max(lo, if s < hi { s } else { hi });
                        det::sub(s, c)
                    }
                    None => 0.0,
                };
                // Remove the axial part twice: in binary32 one pass leaves
                // a rounding residue along the axis of about ulp(|d|),
                // which biases the slide when the offset is large and the
                // perpendicular error small.
                let perp = sub(d, scale(axis, s));
                let perp = sub(perp, scale(axis, dot(perp, axis)));
                add(perp, scale(axis, along))
            }
            _ => d,
        };
        let mut lambda = j.lambda_pos;
        self.positional_part((a, b), (ra, rb), delta, alpha, &mut lambda);
        j.lambda_pos = lambda;
        j.last_lambda = lambda;
        self.joints[k] = j;
    }
}
