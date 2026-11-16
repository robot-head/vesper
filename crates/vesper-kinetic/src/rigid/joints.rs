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
//!
//! **Drives and joint friction** ([`RigidWorld::set_joint_drive`],
//! [`RigidWorld::set_joint_friction`]) act on the free coordinate of a
//! hinge (the angle of `b` relative to `a` about the axis, from the pose
//! at [`RigidWorld::add_joint`]) or a prismatic joint (the offset of `b`'s
//! anchor from `a`'s along the axis):
//!
//! * a position drive is one more compliant XPBD part in the position
//!   pass, towards the target coordinate, with its accumulated `λ` bounded
//!   by `max_force · h²` (a servo that cannot exceed its torque);
//! * a velocity drive (a motor) and Coulomb joint friction act in the
//!   velocity pass, after the contacts' and in the same colour order: the
//!   motor's impulse moves the coordinate rate towards the target and is
//!   bounded by `max_force · h`; friction then opposes the remaining rate
//!   with an impulse bounded by `friction · h`;
//! * static joint friction is positional, like the contacts' (Müller et al.
//!   2020 §3.5): at the end of the joint's position part, the coordinate's
//!   motion since the start of the substep is undone when the impulse that
//!   takes is within `friction · h²`. Without it gravity would make a held
//!   joint creep by `g h²` every substep. So a joint sticks while the load
//!   stays below the friction torque (force) and slides with the friction
//!   impulse of the velocity pass otherwise.
//!
//! A compliant position drive is an undamped XPBD spring (it oscillates
//! about the target); a rigid one (`compliance = 0`) settles at once.

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

/// A drive on a hinge's angle or a prismatic joint's offset.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum JointDrive {
    /// Motor: drive the coordinate rate (rad/s or m/s) to `target`, with a
    /// torque (force) of at most `max_force`.
    Velocity { target: f32, max_force: f32 },
    /// Servo: an XPBD constraint pulling the coordinate (rad or m) to
    /// `target` with `compliance` (`0` is rigid), plus implicit viscous
    /// `damping` of the coordinate rate (N·m·s/rad or N·s/m) in the velocity
    /// pass, each with a torque (force) of at most `max_force`.
    Position {
        target: f32,
        compliance: f32,
        damping: f32,
        max_force: f32,
    },
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
    drive: Option<JointDrive>,
    friction: f32,
    lambda_drive: f32,
    /// Free coordinate at the start of the substep (joints with friction).
    coord_start: f32,
}

impl JointState {
    pub(super) fn has_friction(&self) -> bool {
        self.friction > 0.0
    }

    pub(super) fn reset(&mut self) {
        self.lambda_pos = 0.0;
        self.lambda_ang = 0.0;
        self.lambda_lim = 0.0;
        self.lambda_drive = 0.0;
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
            drive: None,
            friction: 0.0,
            lambda_drive: 0.0,
            coord_start: 0.0,
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
        // A joint added at run time must act: wake what it binds, or a
        // sleeping body would skip the new constraint indefinitely.
        self.wake(joint.a as usize);
        if let Some(b) = joint.b {
            self.wake(b as usize);
        }
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

    /// Set or clear the drive of hinge or prismatic joint `i`. A ball joint,
    /// a non-finite target, a negative compliance or a `max_force` that is
    /// not positive is [`RigidError::InvalidJoint`].
    pub fn set_joint_drive(&mut self, i: usize, drive: Option<JointDrive>) -> Result<(), RigidError> {
        let bad = |reason| RigidError::InvalidJoint { joint: i, reason };
        let j = self.joints.get(i).ok_or(bad("no such joint"))?;
        if drive.is_some() && matches!(j.joint.kind, JointKind::Ball) {
            return Err(bad("drives need a hinge or prismatic joint"));
        }
        let ok = match drive {
            None => true,
            Some(JointDrive::Velocity { target, max_force }) => {
                target.is_finite() && !max_force.is_nan() && max_force > 0.0
            }
            Some(JointDrive::Position {
                target,
                compliance,
                damping,
                max_force,
            }) => {
                target.is_finite()
                    && compliance.is_finite()
                    && compliance >= 0.0
                    && damping.is_finite()
                    && damping >= 0.0
                    && !max_force.is_nan()
                    && max_force > 0.0
            }
        };
        if !ok {
            return Err(bad(
                "drive target must be finite, compliance and damping >= 0, max_force > 0",
            ));
        }
        let (a, b) = (j.joint.a, j.joint.b);
        self.joints[i].drive = drive;
        self.wake(a as usize);
        if let Some(b) = b {
            self.wake(b as usize);
        }
        Ok(())
    }

    /// Set the Coulomb friction torque (hinge, N·m) or force (prismatic, N)
    /// of joint `i`; `0` disables it.
    pub fn set_joint_friction(&mut self, i: usize, friction: f32) -> Result<(), RigidError> {
        let bad = |reason| RigidError::InvalidJoint { joint: i, reason };
        let j = self.joints.get(i).ok_or(bad("no such joint"))?;
        if friction != 0.0 && matches!(j.joint.kind, JointKind::Ball) {
            return Err(bad("joint friction needs a hinge or prismatic joint"));
        }
        if !(friction.is_finite() && friction >= 0.0) {
            return Err(bad("joint friction must be finite and >= 0"));
        }
        self.joints[i].friction = friction;
        Ok(())
    }

    /// The drive of joint `i`.
    pub fn joint_drive(&self, i: usize) -> Option<JointDrive> {
        self.joints.get(i).and_then(|j| j.drive)
    }

    /// The free coordinate of hinge or prismatic joint `i`: the angle of
    /// `b` relative to `a` about the axis (radians, in `[-π, π]`) or the
    /// offset of `b`'s anchor from `a`'s along the axis (metres). `None`
    /// for a ball joint or past the end.
    pub fn joint_coordinate(&self, i: usize) -> Option<f32> {
        let j = self.joints.get(i)?;
        match j.joint.kind {
            JointKind::Ball => None,
            JointKind::Hinge { .. } => Some(self.hinge_angle(j).0),
            JointKind::Prismatic { .. } => {
                let (pa, _, pb, _) = self.joint_points(j);
                let (_, qa) = self.pose(Some(j.joint.a));
                Some(dot(sub(pb, pa), rotate(qa, j.axis_a)))
            }
        }
    }

    /// The rate of joint `i`'s free coordinate (rad/s or m/s).
    pub fn joint_rate(&self, i: usize) -> Option<f32> {
        let j = self.joints.get(i)?;
        match j.joint.kind {
            JointKind::Ball => None,
            _ => Some(self.coordinate_rate(j).0),
        }
    }

    /// The free coordinate of a hinge or prismatic joint (`0` for a ball).
    fn coordinate(&self, j: &JointState) -> f32 {
        match j.joint.kind {
            JointKind::Ball => 0.0,
            JointKind::Hinge { .. } => self.hinge_angle(j).0,
            JointKind::Prismatic { .. } => {
                let (pa, _, pb, _) = self.joint_points(j);
                let (_, qa) = self.pose(Some(j.joint.a));
                dot(sub(pb, pa), rotate(qa, j.axis_a))
            }
        }
    }

    /// Record the free coordinate of every joint with friction at the start
    /// of a substep.
    pub(super) fn record_joint_coordinates(&mut self) {
        for k in 0..self.joints.len() {
            if self.joints[k].friction > 0.0 {
                let c = self.coordinate(&self.joints[k]);
                self.joints[k].coord_start = c;
            }
        }
    }

    /// Positional static friction of joint `j`: undo the coordinate's motion
    /// this substep if that takes an impulse within `friction · h²`.
    fn static_joint_friction(&mut self, j: &JointState, h2: f32) -> bool {
        let (a, b) = (j.joint.a, j.joint.b);
        let bound = det::mul(j.friction, h2);
        match j.joint.kind {
            JointKind::Ball => false,
            JointKind::Hinge { .. } => {
                let (phi, axis, ua, ub) = self.hinge_angle(j);
                let mut delta = det::sub(phi, j.coord_start);
                let pi = std::f32::consts::PI;
                if delta > pi {
                    delta = det::sub(delta, 2.0 * pi);
                } else if delta < -pi {
                    delta = det::add(delta, 2.0 * pi);
                }
                if delta == 0.0 {
                    return false;
                }
                let mut w = angular_weight(&self.bodies[a as usize], axis);
                if let Some(sb) = self.body_or_none(b) {
                    w = det::add(w, angular_weight(sb, axis));
                }
                if !(w > 0.0) || det::div(delta.abs(), w) > bound {
                    return false;
                }
                let goal = add(
                    scale(ua, fastmath::cosf(j.coord_start)),
                    scale(cross(axis, ua), fastmath::sinf(j.coord_start)),
                );
                let mut lambda = 0.0;
                self.bounded_angular_part((a, b), cross(goal, ub), 0.0, f32::INFINITY, &mut lambda);
                true
            }
            JointKind::Prismatic { .. } => {
                let (pa, ra, pb, rb) = self.joint_points(j);
                let ra = if b.is_some() { add(ra, sub(pb, pa)) } else { ra };
                let (_, qa) = self.pose(Some(a));
                let axis = rotate(qa, j.axis_a);
                let delta = det::sub(dot(sub(pb, pa), axis), j.coord_start);
                if delta == 0.0 {
                    return false;
                }
                let sa = &self.bodies[a as usize];
                let mut w = det::add(sa.inv_mass, angular_weight(sa, cross(ra, axis)));
                if let Some(sb) = self.body_or_none(b) {
                    w = det::add(w, det::add(sb.inv_mass, angular_weight(sb, cross(rb, axis))));
                }
                if !(w > 0.0) || det::div(delta.abs(), w) > bound {
                    return false;
                }
                let mut lambda = 0.0;
                self.bounded_positional_part(
                    (a, b),
                    (ra, rb),
                    (axis, delta),
                    0.0,
                    f32::INFINITY,
                    &mut lambda,
                );
                true
            }
        }
    }

    /// Hinge angle and the world vectors it is measured with: `(φ, axis,
    /// u_a, u_b)`.
    fn hinge_angle(&self, j: &JointState) -> (f32, V3, V3, V3) {
        let (_, qa) = self.pose(Some(j.joint.a));
        let (_, qb) = self.pose(j.joint.b);
        let axis = rotate(qa, j.axis_a);
        let ua = rotate(qa, j.ref_a);
        let ub = rotate(qb, j.ref_b);
        let sin = dot(cross(ua, ub), axis);
        let cos = dot(ua, ub);
        (fastmath::atan2f(sin, cos), axis, ua, ub)
    }

    /// Rate of the free coordinate, the world axis, and the arms (zero for
    /// a hinge).
    fn coordinate_rate(&self, j: &JointState) -> (f32, V3, V3, V3) {
        let (_, qa) = self.pose(Some(j.joint.a));
        let axis = rotate(qa, j.axis_a);
        let sa = &self.bodies[j.joint.a as usize];
        let sb = self.body_or_none(j.joint.b);
        match j.joint.kind {
            JointKind::Prismatic { .. } => {
                let (pa, ra, pb, rb) = self.joint_points(j);
                // a's material point at b's anchor (see the slide part).
                let ra = if j.joint.b.is_some() {
                    add(ra, sub(pb, pa))
                } else {
                    ra
                };
                let va = add(sa.v, cross(sa.w, ra));
                let vb = sb.map_or([0.0; 3], |sb| add(sb.v, cross(sb.w, rb)));
                (dot(sub(vb, va), axis), axis, ra, rb)
            }
            _ => {
                let wb = sb.map_or([0.0; 3], |sb| sb.w);
                (dot(sub(wb, sa.w), axis), axis, [0.0; 3], [0.0; 3])
            }
        }
    }

    /// Velocity pass of joint `k`: the motor, then Coulomb friction.
    pub(super) fn joint_velocity(&mut self, k: usize, h: f32) {
        let j = self.joints[k];
        // (target rate, implicit damping, bound); a velocity motor is
        // infinitely stiff in rate.
        let motor = match j.drive {
            Some(JointDrive::Velocity { target, max_force }) => Some((target, None, max_force)),
            Some(JointDrive::Position {
                damping, max_force, ..
            }) if damping > 0.0 => Some((0.0, Some(damping), max_force)),
            _ => None,
        };
        if motor.is_none() && j.friction == 0.0 {
            return;
        }
        let (a, b) = (j.joint.a, j.joint.b);
        if self.sleep.is_some() && !self.live(a, b) {
            return;
        }
        let prismatic = matches!(j.joint.kind, JointKind::Prismatic { .. });
        let (rate, axis, ra, rb) = self.coordinate_rate(&j);
        // Generalized inverse mass of the coordinate.
        let sa = &self.bodies[a as usize];
        let mut w = if prismatic {
            det::add(sa.inv_mass, angular_weight(sa, cross(ra, axis)))
        } else {
            angular_weight(sa, axis)
        };
        if let Some(sb) = self.body_or_none(b) {
            w = det::add(
                w,
                if prismatic {
                    det::add(sb.inv_mass, angular_weight(sb, cross(rb, axis)))
                } else {
                    angular_weight(sb, axis)
                },
            );
        }
        if !(w > 0.0) {
            return;
        }
        let mut rate = rate;
        if let Some((target, damping, max_force)) = motor {
            let bound = det::mul(max_force, h);
            let mut want = det::div(det::sub(target, rate), w);
            if let Some(c) = damping {
                // Backward Euler: Δ = −c h q̇' with q̇' = q̇ + w Δ.
                let chw = det::mul(det::mul(c, h), w);
                want = det::mul(want, det::div(chw, det::add(1.0, chw)));
            }
            let imp = det::max(det::neg(bound), if want < bound { want } else { bound });
            self.coordinate_impulse((a, b), prismatic, axis, (ra, rb), imp);
            rate = self.coordinate_rate(&j).0;
        }
        if j.friction > 0.0 && rate != 0.0 {
            let bound = det::mul(j.friction, h);
            let stop = det::div(rate.abs(), w);
            let mag = if stop < bound { stop } else { bound };
            let imp = if rate > 0.0 { det::neg(mag) } else { mag };
            self.coordinate_impulse((a, b), prismatic, axis, (ra, rb), imp);
        }
    }

    /// Velocity impulse `imp` on the free coordinate: `+imp` along the axis
    /// on `b` and `−imp` on `a` (angular for a hinge, linear at the anchors
    /// for a prismatic joint).
    fn coordinate_impulse(
        &mut self,
        (a, b): (u32, Option<u32>),
        prismatic: bool,
        axis: V3,
        (ra, rb): (V3, V3),
        imp: f32,
    ) {
        let p = scale(axis, imp);
        let sa = &mut self.bodies[a as usize];
        if sa.inv_mass > 0.0 {
            if prismatic {
                sa.v = sub(sa.v, scale(p, sa.inv_mass));
                sa.w = sub(sa.w, super::inv_inertia_world(sa, cross(ra, p)));
            } else {
                sa.w = sub(sa.w, super::inv_inertia_world(sa, p));
            }
        }
        if let Some(b) = b {
            let sb = &mut self.bodies[b as usize];
            if sb.inv_mass > 0.0 {
                if prismatic {
                    sb.v = add(sb.v, scale(p, sb.inv_mass));
                    sb.w = add(sb.w, super::inv_inertia_world(sb, cross(rb, p)));
                } else {
                    sb.w = add(sb.w, super::inv_inertia_world(sb, p));
                }
            }
        }
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

    /// [`RigidWorld::angular_part`] with the accumulated `|λ|` bounded.
    fn bounded_angular_part(
        &mut self,
        (a, b): (u32, Option<u32>),
        dq: V3,
        alpha: f32,
        bound: f32,
        lambda: &mut f32,
    ) {
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
        let total = det::add(*lambda, dl);
        let clamped = det::max(det::neg(bound), if total < bound { total } else { bound });
        let dl = det::sub(clamped, *lambda);
        *lambda = clamped;
        self.apply_rotation_to(a, b, scale(n, dl));
    }

    /// A positional part along the unit `axis` with signed violation `c`
    /// (`b`'s anchor is `c` too far along the axis), its accumulated `λ`
    /// bounded by `±bound`.
    fn bounded_positional_part(
        &mut self,
        (a, b): (u32, Option<u32>),
        (ra, rb): (V3, V3),
        (axis, c): (V3, f32),
        alpha: f32,
        bound: f32,
        lambda: &mut f32,
    ) {
        if c == 0.0 {
            return;
        }
        let sa = &self.bodies[a as usize];
        let mut w = det::add(sa.inv_mass, angular_weight(sa, cross(ra, axis)));
        if let Some(sb) = self.body_or_none(b) {
            w = det::add(w, det::add(sb.inv_mass, angular_weight(sb, cross(rb, axis))));
        }
        let denom = det::add(w, alpha);
        if !(denom > 0.0) {
            return;
        }
        let dl = det::div(det::sub(c, det::mul(alpha, *lambda)), denom);
        let total = det::add(*lambda, dl);
        let clamped = det::max(det::neg(bound), if total < bound { total } else { bound });
        let dl = det::sub(clamped, *lambda);
        *lambda = clamped;
        self.apply_positional_to(a, b, ra, rb, scale(axis, dl));
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
        if let Some(JointDrive::Position {
            target,
            compliance,
            max_force,
            ..
        }) = j.drive
        {
            let alpha_d = det::div(compliance, h2);
            let bound = det::mul(max_force, h2);
            match j.joint.kind {
                JointKind::Hinge { .. } => {
                    let (phi, axis, ua, ub) = self.hinge_angle(&j);
                    if phi != target {
                        let goal = add(
                            scale(ua, fastmath::cosf(target)),
                            scale(cross(axis, ua), fastmath::sinf(target)),
                        );
                        let mut lambda = j.lambda_drive;
                        self.bounded_angular_part(
                            (a, b),
                            cross(goal, ub),
                            alpha_d,
                            bound,
                            &mut lambda,
                        );
                        j.lambda_drive = lambda;
                    }
                }
                JointKind::Prismatic { .. } => {
                    let (pa, ra, pb, rb) = self.joint_points(&j);
                    // Both impulses at b's anchor between two bodies, as
                    // for the slide itself below.
                    let ra = if b.is_some() { add(ra, sub(pb, pa)) } else { ra };
                    let (_, qa) = self.pose(Some(a));
                    let axis = rotate(qa, j.axis_a);
                    let s = dot(sub(pb, pa), axis);
                    let mut lambda = j.lambda_drive;
                    self.bounded_positional_part(
                        (a, b),
                        (ra, rb),
                        (axis, det::sub(s, target)),
                        alpha_d,
                        bound,
                        &mut lambda,
                    );
                    j.lambda_drive = lambda;
                }
                JointKind::Ball => {}
            }
        }
        let (pa, mut ra, pb, rb) = self.joint_points(&j);
        let d = sub(pb, pa);
        if let (JointKind::Prismatic { .. }, Some(_)) = (j.joint.kind, b) {
            // Between two bodies both impulses act at b's anchor: a's arm
            // then carries the axial separation, which is the Jacobian
            // term of the slide axis turning with a. With a's arm ending
            // at its own anchor the equal and opposite impulses would sit
            // on different points of the axis and leak angular momentum.
            // Against the world there is no momentum to keep, and the
            // short arm keeps the slider from being spun against its
            // orientation lock.
            ra = add(ra, d);
        }
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
        // The hinge undo turns the bodies about their centres of mass, and
        // pinning the anchors again turns them back a little: two rounds
        // leave a residual creep (2.4e-3 rad in 1 s for the held rod of
        // tests/joint_drives.rs, against 2e-2 rad without the rounds).
        for _ in 0..2 {
            if !(j.friction > 0.0 && self.static_joint_friction(&j, h2)) {
                break;
            }
            if !matches!(j.joint.kind, JointKind::Hinge { .. }) {
                break;
            }
            let (pa, ra, pb, rb) = self.joint_points(&j);
            let mut lambda = j.lambda_pos;
            self.positional_part((a, b), (ra, rb), sub(pb, pa), alpha, &mut lambda);
            j.lambda_pos = lambda;
        }
        self.joints[k] = j;
    }
}
