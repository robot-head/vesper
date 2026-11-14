//! Contact gradients for rigid boxes, capsules (any orientation), spheres
//! and convex hulls (§36.1 item 5).
//!
//! The model is a binary64 rigid XPBD simulation with the structure of
//! [`crate::rigid::RigidWorld`]: substeps integrate position and
//! orientation (`q ← normalize(q + ½ h [ω, 0] q)`), contacts are projected
//! once per substep in list order with the generalized inverse mass
//! `w = Σ 1/m + (r × n)ᵀ I⁻¹ (r × n)` and compliance `α̃ = α/h²`, and
//! velocities come from the positions (`v = Δx/h`, `ω = 2 vec(q q̄₀)/h`).
//! Normal contacts only: no friction or restitution in this model.
//!
//! Contacts are found once per tick from the values at the start of the
//! tick, as *features* whose geometry is then a smooth function of the
//! state during the tick:
//!
//! * against the ground plane, every core point (box corner, hull vertex,
//!   capsule endpoint, sphere centre) within the margin;
//! * between a polytope (box or hull) and anything, the face of either body
//!   with the largest separation (face-only SAT) is the reference face, and
//!   each core point of the other body that is within the margin of that
//!   face's plane and projects inside the reference body is a contact with
//!   `C = r − (n·(p − x_ref) − d)`, `n` the face normal turning with the
//!   reference body;
//! * between two capsules (or spheres), the closest points of the core
//!   segments, recomputed every substep: `C = r_a + r_b − |p_a − p_b|`.
//!
//! A contact projects only while `C > 0`. Activation, the clamps of the
//! segment parameters and the feature choice are branches on values, so
//! the derivative is that of the branch the forward pass took: exact away
//! from the switching surfaces and undefined on them. Edge–edge contact
//! between two polytopes is not modelled (face-only SAT); a scene whose
//! deepest feature pair is edge–edge gets face contacts instead.
//!
//! Derivatives are forward mode: the same generic code runs on `f64` for
//! [`simulate`] and on a dual number carrying [`TANGENTS`] tangents for
//! [`gradients`], which seeds the initial state and the contact compliance
//! in chunks. The value part of every dual operation is the `f64`
//! operation, so both runs take the same branches and the gradient belongs
//! to exactly the trajectory [`simulate`] returns. The tests check it
//! against central finite differences.

// `!(x > y)` is deliberate: it is also true when `x` is NaN.
#![allow(clippy::neg_cmp_op_on_partial_ord)]

use std::ops::{Add, Div, Mul, Neg, Sub};

/// Tangents carried by one forward pass.
pub const TANGENTS: usize = 16;

/// Largest hull accepted.
pub const MAX_DIFF_HULL_VERTICES: usize = 32;

/// A collider shape in its body's frame.
#[derive(Debug, Clone, PartialEq)]
pub enum DiffShape {
    Sphere {
        radius: f64,
    },
    Box {
        half_extents: [f64; 3],
    },
    /// The segment `a`–`b` swept by `radius`, in any body-frame direction.
    Capsule {
        a: [f64; 3],
        b: [f64; 3],
        radius: f64,
    },
    /// Convex hull of 4 to [`MAX_DIFF_HULL_VERTICES`] vertices with volume.
    Hull {
        vertices: Vec<[f64; 3]>,
    },
}

/// One body: pose, velocities, mass properties and a shape index.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DiffRigidBody {
    pub pos: [f64; 3],
    /// Unit quaternion `[x, y, z, w]`.
    pub orientation: [f64; 4],
    pub vel: [f64; 3],
    pub ang_vel: [f64; 3],
    /// `0.0` marks a static body.
    pub inv_mass: f64,
    /// Inverse principal moments about the body axes.
    pub inv_inertia: [f64; 3],
    pub shape: usize,
}

/// A differentiable rigid scene.
#[derive(Debug, Clone, PartialEq)]
pub struct DiffRigidScene {
    pub bodies: Vec<DiffRigidBody>,
    pub shapes: Vec<DiffShape>,
    pub gravity: [f64; 3],
    pub dt: f64,
    pub substeps: u32,
    pub ticks: u32,
    /// Ground plane `y = h`.
    pub ground: Option<f64>,
    /// Contact compliance `α` (m/N), shared by every contact.
    pub compliance: f64,
    /// Speculative contact distance.
    pub margin: f64,
}

/// Per-body state (also the layout of seeds and gradients).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct DiffRigidState {
    pub pos: [f64; 3],
    pub orientation: [f64; 4],
    pub vel: [f64; 3],
    pub ang_vel: [f64; 3],
}

/// Gradients of `Σ seed · final state` with respect to every body's initial
/// state and the contact compliance.
#[derive(Debug, Clone, PartialEq)]
pub struct RigidGradients {
    pub loss: f64,
    pub initial: Vec<DiffRigidState>,
    pub compliance: f64,
    /// Contact activations (contact-substeps that projected), by feature:
    /// `[ground, face, segment pair]`.
    pub activations: [usize; 3],
}

/// Typed failures.
#[derive(Debug, Clone, PartialEq)]
pub enum RigidDiffError {
    InvalidScene(&'static str),
    InvalidShape {
        shape: usize,
        reason: &'static str,
    },
    InvalidBody {
        body: usize,
        reason: &'static str,
    },
    SeedLength,
    /// More than this many contacts in one tick.
    TooManyContacts(usize),
    NonFinite,
}

impl std::fmt::Display for RigidDiffError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidScene(w) => write!(f, "invalid scene: {w}"),
            Self::InvalidShape { shape, reason } => write!(f, "shape {shape}: {reason}"),
            Self::InvalidBody { body, reason } => write!(f, "body {body}: {reason}"),
            Self::SeedLength => write!(f, "one seed per body is required"),
            Self::TooManyContacts(n) => write!(f, "more than {n} contacts in one tick"),
            Self::NonFinite => write!(f, "the simulation left the finite range"),
        }
    }
}

impl std::error::Error for RigidDiffError {}

/// Most contacts in one tick.
pub const MAX_DIFF_CONTACTS: usize = 4096;

// ----- scalars ----------------------------------------------------------------

/// The scalar the model is generic over: `f64` or a dual number.
trait Real:
    Copy
    + Add<Output = Self>
    + Sub<Output = Self>
    + Mul<Output = Self>
    + Div<Output = Self>
    + Neg<Output = Self>
{
    fn c(x: f64) -> Self;
    fn v(self) -> f64;
    fn sqrt(self) -> Self;
}

impl Real for f64 {
    fn c(x: f64) -> Self {
        x
    }
    fn v(self) -> f64 {
        self
    }
    fn sqrt(self) -> Self {
        f64::sqrt(self)
    }
}

/// A dual number with [`TANGENTS`] tangent components.
#[derive(Debug, Clone, Copy)]
struct Dual {
    v: f64,
    d: [f64; TANGENTS],
}

impl Dual {
    fn seeded(v: f64, slot: Option<usize>) -> Self {
        let mut d = [0.0; TANGENTS];
        if let Some(k) = slot {
            d[k] = 1.0;
        }
        Self { v, d }
    }
}

impl Add for Dual {
    type Output = Self;
    fn add(self, o: Self) -> Self {
        let mut d = self.d;
        for (x, y) in d.iter_mut().zip(o.d) {
            *x += y;
        }
        Self { v: self.v + o.v, d }
    }
}
impl Sub for Dual {
    type Output = Self;
    fn sub(self, o: Self) -> Self {
        let mut d = self.d;
        for (x, y) in d.iter_mut().zip(o.d) {
            *x -= y;
        }
        Self { v: self.v - o.v, d }
    }
}
impl Mul for Dual {
    type Output = Self;
    fn mul(self, o: Self) -> Self {
        let mut d = [0.0; TANGENTS];
        for (k, x) in d.iter_mut().enumerate() {
            *x = self.d[k] * o.v + self.v * o.d[k];
        }
        Self { v: self.v * o.v, d }
    }
}
impl Div for Dual {
    type Output = Self;
    fn div(self, o: Self) -> Self {
        let v = self.v / o.v;
        let mut d = [0.0; TANGENTS];
        for (k, x) in d.iter_mut().enumerate() {
            *x = (self.d[k] - v * o.d[k]) / o.v;
        }
        Self { v, d }
    }
}
impl Neg for Dual {
    type Output = Self;
    fn neg(self) -> Self {
        Self {
            v: -self.v,
            d: self.d.map(|x| -x),
        }
    }
}
impl Real for Dual {
    fn c(x: f64) -> Self {
        Self::seeded(x, None)
    }
    fn v(self) -> f64 {
        self.v
    }
    fn sqrt(self) -> Self {
        let s = self.v.sqrt();
        let k = if s > 0.0 { 0.5 / s } else { 0.0 };
        Self {
            v: s,
            d: self.d.map(|x| x * k),
        }
    }
}

type V<R> = [R; 3];

fn vadd<R: Real>(a: V<R>, b: V<R>) -> V<R> {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
fn vsub<R: Real>(a: V<R>, b: V<R>) -> V<R> {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn vscale<R: Real>(a: V<R>, s: R) -> V<R> {
    [a[0] * s, a[1] * s, a[2] * s]
}
fn vdot<R: Real>(a: V<R>, b: V<R>) -> R {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn vcross<R: Real>(a: V<R>, b: V<R>) -> V<R> {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn vc<R: Real>(a: [f64; 3]) -> V<R> {
    a.map(R::c)
}
fn vv<R: Real>(a: V<R>) -> [f64; 3] {
    a.map(R::v)
}

fn qmul<R: Real>(q: [R; 4], p: [R; 4]) -> [R; 4] {
    let (qv, pv) = ([q[0], q[1], q[2]], [p[0], p[1], p[2]]);
    let c = vcross(qv, pv);
    let v = vadd(vadd(vscale(pv, q[3]), vscale(qv, p[3])), c);
    [v[0], v[1], v[2], q[3] * p[3] - vdot(qv, pv)]
}
fn conj<R: Real>(q: [R; 4]) -> [R; 4] {
    [-q[0], -q[1], -q[2], q[3]]
}
fn rotate<R: Real>(q: [R; 4], v: V<R>) -> V<R> {
    let u = [q[0], q[1], q[2]];
    let t = vscale(vcross(u, v), R::c(2.0));
    vadd(vadd(v, vscale(t, q[3])), vcross(u, t))
}
fn normalize_q<R: Real>(q: [R; 4]) -> [R; 4] {
    let n = (q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3]).sqrt();
    q.map(|x| x / n)
}

// ----- geometry ----------------------------------------------------------------

/// Core points and planes of a shape (body frame).
#[derive(Debug, Clone)]
struct Geometry {
    points: Vec<[f64; 3]>,
    radius: f64,
    /// Outward face planes `n · x ≤ d` (polytopes only).
    planes: Vec<([f64; 3], f64)>,
    /// Each plane's face polygon, ordered around its normal.
    faces: Vec<Vec<[f64; 3]>>,
    /// A capsule or sphere (core is a point or segment).
    round: bool,
}

fn sub64(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn dot64(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross64(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    vcross(a, b)
}

/// Faces of a convex hull from its vertices: every plane through three
/// vertices that has all vertices on one side, deduplicated.
fn hull_planes(v: &[[f64; 3]]) -> Vec<([f64; 3], f64)> {
    let scale = v
        .iter()
        .flat_map(|p| p.iter().map(|x| x.abs()))
        .fold(1e-12f64, f64::max);
    let tol = 1e-9 * scale;
    let mut planes: Vec<([f64; 3], f64)> = Vec::new();
    for i in 0..v.len() {
        for j in i + 1..v.len() {
            for k in j + 1..v.len() {
                let n = cross64(sub64(v[j], v[i]), sub64(v[k], v[i]));
                let len = dot64(n, n).sqrt();
                if !(len > 1e-9 * scale * scale) {
                    continue;
                }
                let mut n = n.map(|x| x / len);
                let mut d = dot64(n, v[i]);
                let (mut above, mut below) = (false, false);
                for p in v {
                    let s = dot64(n, *p) - d;
                    above |= s > tol;
                    below |= s < -tol;
                }
                if above && below {
                    continue;
                }
                if above {
                    n = n.map(|x| -x);
                    d = -d;
                }
                if !planes
                    .iter()
                    .any(|(m, e)| dot64(*m, n) > 1.0 - 1e-9 && (e - d).abs() <= tol)
                {
                    planes.push((n, d));
                }
            }
        }
    }
    planes
}

/// The vertices on each plane, ordered by angle about the face centroid
/// (deterministic `fastmath::atan2`).
fn face_polygons(v: &[[f64; 3]], planes: &[([f64; 3], f64)]) -> Vec<Vec<[f64; 3]>> {
    let scale = v
        .iter()
        .flat_map(|p| p.iter().map(|x| x.abs()))
        .fold(1e-12f64, f64::max);
    planes
        .iter()
        .map(|(n, d)| {
            let mut pts: Vec<[f64; 3]> = v
                .iter()
                .filter(|p| (dot64(*n, **p) - d).abs() <= 1e-9 * scale)
                .copied()
                .collect();
            let k = pts.len() as f64;
            let c = pts.iter().fold([0.0; 3], |a, p| {
                [a[0] + p[0] / k, a[1] + p[1] / k, a[2] + p[2] / k]
            });
            // An in-plane basis.
            let u = {
                let t = if n[0].abs() < 0.57 {
                    [1.0, 0.0, 0.0]
                } else {
                    [0.0, 1.0, 0.0]
                };
                let u = cross64(*n, t);
                let l = dot64(u, u).sqrt();
                u.map(|x| x / l)
            };
            let w = cross64(*n, u);
            pts.sort_by(|a, b| {
                let ang = |p: &[f64; 3]| {
                    let r = sub64(*p, c);
                    crate::fastmath::atan2(dot64(r, w), dot64(r, u))
                };
                ang(a).total_cmp(&ang(b))
            });
            pts
        })
        .collect()
}

/// Sutherland–Hodgman: keep the part of polygon `poly` with `m · x ≤ e`.
/// The plane is fixed geometry of the reference body (constants); the
/// polygon's points carry derivatives, and the inside tests branch on
/// values.
fn clip<R: Real>(poly: &[V<R>], m: [f64; 3], e: f64) -> Vec<V<R>> {
    let mut out = Vec::with_capacity(poly.len() + 1);
    let k = poly.len();
    let (m, e) = (vc::<R>(m), R::c(e));
    for i in 0..k {
        let (p, q) = (poly[i], poly[(i + 1) % k]);
        let (sp, sq) = (vdot(m, p) - e, vdot(m, q) - e);
        if sp.v() <= 0.0 {
            out.push(p);
        }
        // A two-point polygon (a capsule's segment) has one edge.
        if (sp.v() <= 0.0) != (sq.v() <= 0.0) && (k > 2 || (k == 2 && i == 0)) {
            let t = sp / (sp - sq);
            out.push(vadd(p, vscale(vsub(q, p), t)));
        }
    }
    out
}

fn geometry(shape: &DiffShape) -> Geometry {
    match shape {
        DiffShape::Sphere { radius } => Geometry {
            points: vec![[0.0; 3]],
            radius: *radius,
            planes: Vec::new(),
            faces: Vec::new(),
            round: true,
        },
        DiffShape::Capsule { a, b, radius } => Geometry {
            points: vec![*a, *b],
            radius: *radius,
            planes: Vec::new(),
            faces: Vec::new(),
            round: true,
        },
        DiffShape::Box { half_extents: h } => {
            let mut points = Vec::with_capacity(8);
            for i in 0..8 {
                points.push([
                    if i & 1 == 0 { -h[0] } else { h[0] },
                    if i & 2 == 0 { -h[1] } else { h[1] },
                    if i & 4 == 0 { -h[2] } else { h[2] },
                ]);
            }
            let mut planes = Vec::with_capacity(6);
            for k in 0..3 {
                let mut n = [0.0; 3];
                n[k] = 1.0;
                planes.push((n, h[k]));
                n[k] = -1.0;
                planes.push((n, h[k]));
            }
            let faces = face_polygons(&points, &planes);
            Geometry {
                points,
                radius: 0.0,
                planes,
                faces,
                round: false,
            }
        }
        DiffShape::Hull { vertices } => {
            let planes = hull_planes(vertices);
            Geometry {
                points: vertices.clone(),
                radius: 0.0,
                faces: face_polygons(vertices, &planes),
                planes,
                round: false,
            }
        }
    }
}

// ----- the model -----------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
struct Body<R> {
    x: V<R>,
    q: [R; 4],
    v: V<R>,
    w: V<R>,
}

/// A contact feature for one tick.
#[derive(Debug, Clone, Copy)]
enum Feature<R> {
    /// Core point `point` of body `a` against the ground.
    Ground { a: usize, point: usize },
    /// A point fixed in body `inc`'s frame (`local`: a vertex or a point
    /// of the incident face clipped to the reference face, on the core)
    /// against face `face` of body `rf`.
    Face {
        rf: usize,
        face: usize,
        inc: usize,
        local: V<R>,
    },
    /// Closest points of the core segments of round bodies `a` and `b`.
    Segments { a: usize, b: usize },
}

struct Model<'s> {
    scene: &'s DiffRigidScene,
    geo: Vec<Geometry>,
}

/// Closest points of segments `p0 + s (p1 − p0)` and `q0 + t (q1 − q0)`
/// (Ericson, Real-Time Collision Detection, §5.1.9), with the parameter
/// clamps as value branches.
fn closest_segments<R: Real>(p0: V<R>, p1: V<R>, q0: V<R>, q1: V<R>) -> (V<R>, V<R>) {
    let d1 = vsub(p1, p0);
    let d2 = vsub(q1, q0);
    let r = vsub(p0, q0);
    let a = vdot(d1, d1);
    let e = vdot(d2, d2);
    let f = vdot(d2, r);
    let eps = 1e-18;
    let clamp01 = |x: R| {
        if x.v() < 0.0 {
            R::c(0.0)
        } else if x.v() > 1.0 {
            R::c(1.0)
        } else {
            x
        }
    };
    let (s, t);
    if a.v() <= eps && e.v() <= eps {
        return (p0, q0);
    }
    if a.v() <= eps {
        s = R::c(0.0);
        t = clamp01(f / e);
    } else {
        let c = vdot(d1, r);
        if e.v() <= eps {
            t = R::c(0.0);
            s = clamp01(-c / a);
        } else {
            let b = vdot(d1, d2);
            let denom = a * e - b * b;
            let s0 = if denom.v() > eps * a.v() * e.v() {
                clamp01((b * f - c * e) / denom)
            } else {
                R::c(0.0)
            };
            let t0 = (b * s0 + f) / e;
            if t0.v() < 0.0 {
                t = R::c(0.0);
                s = clamp01(-c / a);
            } else if t0.v() > 1.0 {
                t = R::c(1.0);
                s = clamp01((b - c) / a);
            } else {
                t = t0;
                s = s0;
            }
        }
    }
    (vadd(p0, vscale(d1, s)), vadd(q0, vscale(d2, t)))
}

impl<'s> Model<'s> {
    fn new(scene: &'s DiffRigidScene) -> Result<Self, RigidDiffError> {
        let finite = |v: &[f64]| v.iter().all(|x| x.is_finite());
        if !(scene.dt.is_finite() && scene.dt > 0.0) {
            return Err(RigidDiffError::InvalidScene(
                "dt must be finite and positive",
            ));
        }
        if scene.substeps == 0 || scene.substeps > 64 {
            return Err(RigidDiffError::InvalidScene("substeps must be in 1..=64"));
        }
        if !finite(&scene.gravity)
            || scene.ground.is_some_and(|g| !g.is_finite())
            || !(scene.compliance.is_finite() && scene.compliance >= 0.0)
            || !(scene.margin.is_finite() && scene.margin >= 0.0)
        {
            return Err(RigidDiffError::InvalidScene(
                "gravity, ground, compliance and margin must be finite (>= 0)",
            ));
        }
        let mut geo = Vec::with_capacity(scene.shapes.len());
        for (i, s) in scene.shapes.iter().enumerate() {
            let bad = |reason| RigidDiffError::InvalidShape { shape: i, reason };
            let pos = |x: f64| x.is_finite() && x > 0.0;
            match s {
                DiffShape::Sphere { radius } if !pos(*radius) => {
                    return Err(bad("radius must be positive"));
                }
                DiffShape::Capsule { a, b, radius }
                    if !(pos(*radius) && finite(a) && finite(b)) =>
                {
                    return Err(bad("capsule needs finite endpoints and a positive radius"));
                }
                DiffShape::Box { half_extents } if !half_extents.iter().all(|h| pos(*h)) => {
                    return Err(bad("half extents must be positive"));
                }
                DiffShape::Hull { vertices } => {
                    if vertices.len() < 4 || vertices.len() > MAX_DIFF_HULL_VERTICES {
                        return Err(bad("hull needs 4 to 32 vertices"));
                    }
                    if !vertices.iter().all(|v| finite(v)) {
                        return Err(bad("hull vertices must be finite"));
                    }
                }
                _ => {}
            }
            let g = geometry(s);
            if matches!(s, DiffShape::Hull { .. }) && g.planes.len() < 4 {
                return Err(bad("hull is flat"));
            }
            geo.push(g);
        }
        for (i, b) in scene.bodies.iter().enumerate() {
            let bad = |reason| RigidDiffError::InvalidBody { body: i, reason };
            if b.shape >= geo.len() {
                return Err(bad("unknown shape"));
            }
            if !(finite(&b.pos)
                && finite(&b.orientation)
                && finite(&b.vel)
                && finite(&b.ang_vel)
                && b.inv_mass.is_finite()
                && b.inv_mass >= 0.0
                && b.inv_inertia.iter().all(|v| v.is_finite() && *v >= 0.0))
            {
                return Err(bad("fields must be finite, masses >= 0"));
            }
            let n2: f64 = b.orientation.iter().map(|v| v * v).sum();
            if (n2 - 1.0).abs() > 1e-4 {
                return Err(bad("orientation must be a unit quaternion"));
            }
        }
        Ok(Self { scene, geo })
    }

    fn dynamic(&self, i: usize) -> bool {
        self.scene.bodies[i].inv_mass > 0.0
    }

    fn world_point<R: Real>(&self, b: &Body<R>, shape: usize, point: usize) -> (V<R>, V<R>) {
        let r = rotate(b.q, vc(self.geo[shape].points[point]));
        (vadd(b.x, r), r)
    }

    /// The tick's contact features from the current values.
    fn detect<R: Real>(
        &self,
        bodies: &[Body<R>],
        out: &mut Vec<Feature<R>>,
    ) -> Result<(), RigidDiffError> {
        out.clear();
        let sc = self.scene;
        let n = bodies.len();
        let margin = sc.margin;
        let push = |out: &mut Vec<Feature<R>>, f| {
            if out.len() == MAX_DIFF_CONTACTS {
                return Err(RigidDiffError::TooManyContacts(MAX_DIFF_CONTACTS));
            }
            out.push(f);
            Ok(())
        };
        let val = |b: &Body<R>| Body {
            x: vv(b.x),
            q: b.q.map(R::v),
            v: vv(b.v),
            w: vv(b.w),
        };
        let bodies64: Vec<Body<f64>> = bodies.iter().map(val).collect();
        for i in 0..n {
            if !self.dynamic(i) {
                continue;
            }
            if let Some(g) = sc.ground {
                let s = sc.bodies[i].shape;
                for p in 0..self.geo[s].points.len() {
                    let (w, _) = self.world_point(&bodies64[i], s, p);
                    if w[1] - self.geo[s].radius - g < margin {
                        push(out, Feature::Ground { a: i, point: p })?;
                    }
                }
            }
        }
        for i in 0..n {
            for j in i + 1..n {
                if !(self.dynamic(i) || self.dynamic(j)) {
                    continue;
                }
                let (si, sj) = (sc.bodies[i].shape, sc.bodies[j].shape);
                let (gi, gj) = (&self.geo[si], &self.geo[sj]);
                if gi.round && gj.round {
                    let (pi, pj) = (&bodies64[i], &bodies64[j]);
                    let ends = |b: &Body<f64>, s: usize| {
                        let g = &self.geo[s];
                        let a = self.world_point(b, s, 0).0;
                        let e = self.world_point(b, s, g.points.len() - 1).0;
                        (a, e)
                    };
                    let (a0, a1) = ends(pi, si);
                    let (b0, b1) = ends(pj, sj);
                    let (ca, cb) = closest_segments(a0, a1, b0, b1);
                    let d = dot64(sub64(ca, cb), sub64(ca, cb)).sqrt();
                    if d - gi.radius - gj.radius < margin {
                        push(out, Feature::Segments { a: i, b: j })?;
                    }
                    continue;
                }
                // Face-only SAT: the face (of either polytope) with the
                // largest separation against the other's support.
                // Each body's best face; the second body's face is the
                // reference only if it separates clearly more (a tolerance
                // of a tenth of the margin), so parallel resting faces,
                // which tie, do not switch reference between ticks.
                let mut sides: [Option<(f64, usize, usize)>; 2] = [None, None];
                for (side, (rf, inc)) in [(i, j), (j, i)].into_iter().enumerate() {
                    let best = &mut sides[side];
                    let (sr, si2) = (sc.bodies[rf].shape, sc.bodies[inc].shape);
                    let (br, bi) = (&bodies64[rf], &bodies64[inc]);
                    for (f, (nl, d)) in self.geo[sr].planes.iter().enumerate() {
                        let nw = rotate(br.q, *nl);
                        let off = dot64(nw, br.x) + d;
                        let mut lowest = f64::INFINITY;
                        for p in 0..self.geo[si2].points.len() {
                            let (w, _) = self.world_point(bi, si2, p);
                            lowest = lowest.min(dot64(nw, w) - off - self.geo[si2].radius);
                        }
                        if best.is_none_or(|(s, _, _)| lowest > s) {
                            *best = Some((lowest, rf, f));
                        }
                    }
                }
                let best = match sides {
                    [Some(x), Some(y)] => Some(if y.0 > x.0 + 0.1 * margin + 1e-12 {
                        y
                    } else {
                        x
                    }),
                    [x, y] => x.or(y),
                };
                let Some((sep, rf, face)) = best else {
                    continue;
                };
                if sep >= margin {
                    continue;
                }
                let inc = if rf == i { j } else { i };
                let (sr, si2) = (sc.bodies[rf].shape, sc.bodies[inc].shape);
                let (br, bi) = (&bodies64[rf], &bodies64[inc]);
                let (nl, d) = self.geo[sr].planes[face];
                let nw = rotate(br.q, nl);
                let r_inc = self.geo[si2].radius;
                // The incident core polygon in the reference frame: the
                // incident body's face most opposed to the normal, or its
                // core points (a capsule's segment, a sphere's centre).
                // Computed on the scalar type, so the clipped points carry
                // derivatives with respect to the poses at the tick start.
                let (bri, bii) = (&bodies[rf], &bodies[inc]);
                let to_ref = |p: [f64; 3]| {
                    let w = vadd(bii.x, rotate(bii.q, vc(p)));
                    rotate(conj(bri.q), vsub(w, bri.x))
                };
                let poly: Vec<V<R>> = if self.geo[si2].round {
                    self.geo[si2].points.iter().map(|p| to_ref(*p)).collect()
                } else {
                    let (mut pick, mut low) = (0, f64::INFINITY);
                    for (k, (m, _)) in self.geo[si2].planes.iter().enumerate() {
                        let dw = dot64(rotate(bi.q, *m), nw);
                        if dw < low {
                            pick = k;
                            low = dw;
                        }
                    }
                    self.geo[si2].faces[pick]
                        .iter()
                        .map(|p| to_ref(*p))
                        .collect()
                };
                // Clip against the reference face's side planes.
                let ref_poly = &self.geo[sr].faces[face];
                let k = ref_poly.len();
                let centre = ref_poly
                    .iter()
                    .fold([0.0; 3], |a, p| [a[0] + p[0], a[1] + p[1], a[2] + p[2]]);
                let centre = centre.map(|x| x / k as f64);
                let mut clipped = poly;
                for e in 0..k {
                    let (p, q) = (ref_poly[e], ref_poly[(e + 1) % k]);
                    let mut m = cross64(sub64(q, p), nl);
                    let len = dot64(m, m).sqrt();
                    if !(len > 0.0) {
                        continue;
                    }
                    m = m.map(|x| x / len);
                    if dot64(m, sub64(p, centre)) < 0.0 {
                        m = m.map(|x| -x);
                    }
                    clipped = clip(&clipped, m, dot64(m, p));
                    if clipped.is_empty() {
                        break;
                    }
                }
                for c in clipped {
                    let s = dot64(nl, vv(c)) - d - r_inc;
                    if s >= margin {
                        continue;
                    }
                    // Back to the incident body's frame.
                    let w = vadd(bri.x, rotate(bri.q, c));
                    let local = rotate(conj(bii.q), vsub(w, bii.x));
                    push(
                        out,
                        Feature::Face {
                            rf,
                            face,
                            inc,
                            local,
                        },
                    )?;
                }
            }
        }
        Ok(())
    }

    /// `(C, n, arm_a, arm_b, a, b)` of a feature now: positive `C` is
    /// penetration, `n` points from `b` to `a`, `b == None` for the ground.
    #[allow(clippy::type_complexity)]
    fn evaluate<R: Real>(
        &self,
        f: Feature<R>,
        bodies: &[Body<R>],
    ) -> (R, V<R>, V<R>, V<R>, usize, Option<usize>) {
        let sc = self.scene;
        match f {
            Feature::Ground { a, point } => {
                let s = sc.bodies[a].shape;
                let (w, r) = self.world_point(&bodies[a], s, point);
                let c = R::c(sc.ground.unwrap_or(0.0)) - (w[1] - R::c(self.geo[s].radius));
                let n = vc([0.0, 1.0, 0.0]);
                let arm = vsub(r, vscale(n, R::c(self.geo[s].radius)));
                (c, n, arm, vc([0.0; 3]), a, None)
            }
            Feature::Face {
                rf,
                face,
                inc,
                local,
            } => {
                let (sr, si) = (sc.bodies[rf].shape, sc.bodies[inc].shape);
                let (nl, d) = self.geo[sr].planes[face];
                let br = &bodies[rf];
                let n = rotate(br.q, vc(nl));
                let r = rotate(bodies[inc].q, local);
                let w = vadd(bodies[inc].x, r);
                let rad = R::c(self.geo[si].radius);
                let c = rad - (vdot(n, vsub(w, br.x)) - R::c(d));
                // Contact point on the incident surface and its arm on the
                // reference body.
                let surf = vsub(w, vscale(n, rad));
                let arm_inc = vsub(r, vscale(n, rad));
                let arm_ref = vsub(surf, br.x);
                (c, n, arm_inc, arm_ref, inc, Some(rf))
            }
            Feature::Segments { a, b } => {
                let (sa, sb) = (sc.bodies[a].shape, sc.bodies[b].shape);
                let ends = |i: usize, s: usize| {
                    let last = self.geo[s].points.len() - 1;
                    (
                        self.world_point(&bodies[i], s, 0).0,
                        self.world_point(&bodies[i], s, last).0,
                    )
                };
                let (a0, a1) = ends(a, sa);
                let (b0, b1) = ends(b, sb);
                let (pa, pb) = closest_segments(a0, a1, b0, b1);
                let d = vsub(pa, pb);
                let len = vdot(d, d).sqrt();
                let (ra, rb) = (R::c(self.geo[sa].radius), R::c(self.geo[sb].radius));
                let c = ra + rb - len;
                let n = if len.v() > 0.0 {
                    vscale(d, R::c(1.0) / len)
                } else {
                    vc([0.0, 1.0, 0.0])
                };
                let arm_a = vsub(vsub(pa, vscale(n, ra)), bodies[a].x);
                let arm_b = vsub(vadd(pb, vscale(n, rb)), bodies[b].x);
                (c, n, arm_a, arm_b, a, Some(b))
            }
        }
    }

    fn inv_inertia_world<R: Real>(&self, i: usize, b: &Body<R>, x: V<R>) -> V<R> {
        let l = rotate(conj(b.q), x);
        let ii = self.scene.bodies[i].inv_inertia;
        let l = [l[0] * R::c(ii[0]), l[1] * R::c(ii[1]), l[2] * R::c(ii[2])];
        rotate(b.q, l)
    }

    fn weight<R: Real>(&self, i: usize, b: &Body<R>, arm: V<R>, n: V<R>) -> R {
        if !self.dynamic(i) {
            return R::c(0.0);
        }
        let rn = vcross(arm, n);
        R::c(self.scene.bodies[i].inv_mass) + vdot(rn, self.inv_inertia_world(i, b, rn))
    }

    fn push_body<R: Real>(&self, i: usize, b: &mut Body<R>, arm: V<R>, p: V<R>) {
        if !self.dynamic(i) {
            return;
        }
        b.x = vadd(b.x, vscale(p, R::c(self.scene.bodies[i].inv_mass)));
        let dq = self.inv_inertia_world(i, b, vcross(arm, p));
        let rot = qmul([dq[0], dq[1], dq[2], R::c(0.0)], b.q);
        let half = R::c(0.5);
        b.q = [
            b.q[0] + half * rot[0],
            b.q[1] + half * rot[1],
            b.q[2] + half * rot[2],
            b.q[3] + half * rot[3],
        ];
    }

    fn run<R: Real>(
        &self,
        mut bodies: Vec<Body<R>>,
        alpha: R,
    ) -> Result<(Vec<Body<R>>, [usize; 3]), RigidDiffError> {
        let sc = self.scene;
        let h = R::c(sc.dt / f64::from(sc.substeps));
        let inv_h = R::c(f64::from(sc.substeps) / sc.dt);
        let alpha_t = alpha * inv_h * inv_h;
        let g = vc(sc.gravity);
        let mut features = Vec::new();
        let mut activations = [0usize; 3];
        let mut prev = bodies.clone();
        for _ in 0..sc.ticks {
            self.detect(&bodies, &mut features)?;
            for _ in 0..sc.substeps {
                prev.copy_from_slice(&bodies);
                for (i, b) in bodies.iter_mut().enumerate() {
                    if !self.dynamic(i) {
                        continue;
                    }
                    b.v = vadd(b.v, vscale(g, h));
                    b.x = vadd(b.x, vscale(b.v, h));
                    let spin = qmul([b.w[0], b.w[1], b.w[2], R::c(0.0)], b.q);
                    let half_h = R::c(0.5) * h;
                    b.q = normalize_q([
                        b.q[0] + half_h * spin[0],
                        b.q[1] + half_h * spin[1],
                        b.q[2] + half_h * spin[2],
                        b.q[3] + half_h * spin[3],
                    ]);
                }
                for f in &features {
                    let (c, n, arm_a, arm_b, a, b) = self.evaluate(*f, &bodies);
                    if !(c.v() > 0.0) {
                        continue;
                    }
                    let mut w = self.weight(a, &bodies[a], arm_a, n);
                    if let Some(b) = b {
                        w = w + self.weight(b, &bodies[b], arm_b, n);
                    }
                    let denom = w + alpha_t;
                    if !(denom.v() > 0.0) {
                        continue;
                    }
                    activations[match f {
                        Feature::Ground { .. } => 0,
                        Feature::Face { .. } => 1,
                        Feature::Segments { .. } => 2,
                    }] += 1;
                    let dl = c / denom;
                    let p = vscale(n, dl);
                    let mut ba = bodies[a];
                    self.push_body(a, &mut ba, arm_a, p);
                    bodies[a] = ba;
                    if let Some(b) = b {
                        let mut bb = bodies[b];
                        self.push_body(b, &mut bb, arm_b, vscale(p, R::c(-1.0)));
                        bodies[b] = bb;
                    }
                }
                for (i, b) in bodies.iter_mut().enumerate() {
                    if !self.dynamic(i) {
                        continue;
                    }
                    b.q = normalize_q(b.q);
                    b.v = vscale(vsub(b.x, prev[i].x), inv_h);
                    let dq = qmul(b.q, conj(prev[i].q));
                    let s = if dq[3].v() >= 0.0 {
                        R::c(2.0)
                    } else {
                        R::c(-2.0)
                    };
                    b.w = vscale([dq[0], dq[1], dq[2]], s * inv_h);
                }
            }
            if !bodies.iter().all(|b| {
                vv(b.x)
                    .iter()
                    .chain(&vv(b.v))
                    .chain(&vv(b.w))
                    .all(|x| x.is_finite())
            }) {
                return Err(RigidDiffError::NonFinite);
            }
        }
        Ok((bodies, activations))
    }
}

fn state_of<R: Real>(b: &Body<R>) -> DiffRigidState {
    DiffRigidState {
        pos: vv(b.x),
        orientation: b.q.map(R::v),
        vel: vv(b.v),
        ang_vel: vv(b.w),
    }
}

/// Run the scene in binary64; the final state of every body.
pub fn simulate(scene: &DiffRigidScene) -> Result<Vec<DiffRigidState>, RigidDiffError> {
    let model = Model::new(scene)?;
    let bodies = scene
        .bodies
        .iter()
        .map(|b| Body {
            x: b.pos,
            q: b.orientation,
            v: b.vel,
            w: b.ang_vel,
        })
        .collect();
    let (out, _) = model.run(bodies, scene.compliance)?;
    Ok(out.iter().map(state_of).collect())
}

/// Gradients of `loss = Σ_i seeds[i] · final[i]` (componentwise over
/// position, orientation, velocity and angular velocity) with respect to
/// the initial state of every body and the contact compliance.
pub fn gradients(
    scene: &DiffRigidScene,
    seeds: &[DiffRigidState],
) -> Result<RigidGradients, RigidDiffError> {
    let model = Model::new(scene)?;
    let n = scene.bodies.len();
    if seeds.len() != n {
        return Err(RigidDiffError::SeedLength);
    }
    const PER_BODY: usize = 13;
    let params = PER_BODY * n + 1;
    let mut grad = vec![0.0f64; params];
    let mut loss = 0.0;
    let mut activations = [0; 3];
    let flat = |s: &DiffRigidState| -> [f64; PER_BODY] {
        let mut out = [0.0; PER_BODY];
        out[..3].copy_from_slice(&s.pos);
        out[3..7].copy_from_slice(&s.orientation);
        out[7..10].copy_from_slice(&s.vel);
        out[10..].copy_from_slice(&s.ang_vel);
        out
    };
    for start in (0..params).step_by(TANGENTS) {
        let slot = |p: usize| (p >= start && p < start + TANGENTS).then(|| p - start);
        let bodies: Vec<Body<Dual>> = scene
            .bodies
            .iter()
            .enumerate()
            .map(|(i, b)| {
                let init = flat(&DiffRigidState {
                    pos: b.pos,
                    orientation: b.orientation,
                    vel: b.vel,
                    ang_vel: b.ang_vel,
                });
                let d: Vec<Dual> = (0..PER_BODY)
                    .map(|k| Dual::seeded(init[k], slot(i * PER_BODY + k)))
                    .collect();
                Body {
                    x: [d[0], d[1], d[2]],
                    q: [d[3], d[4], d[5], d[6]],
                    v: [d[7], d[8], d[9]],
                    w: [d[10], d[11], d[12]],
                }
            })
            .collect();
        let alpha = Dual::seeded(scene.compliance, slot(params - 1));
        let (out, acts) = model.run(bodies, alpha)?;
        activations = acts;
        let mut total = [0.0f64; TANGENTS];
        let mut value = 0.0;
        for (b, s) in out.iter().zip(seeds) {
            let comps = [
                b.x[0], b.x[1], b.x[2], b.q[0], b.q[1], b.q[2], b.q[3], b.v[0], b.v[1], b.v[2],
                b.w[0], b.w[1], b.w[2],
            ];
            for (c, k) in comps.iter().zip(flat(s)) {
                value += c.v * k;
                for (t, dt) in total.iter_mut().zip(c.d) {
                    *t += dt * k;
                }
            }
        }
        loss = value;
        for (k, t) in total.iter().enumerate() {
            if start + k < params {
                grad[start + k] = *t;
            }
        }
    }
    let unflat = |g: &[f64]| DiffRigidState {
        pos: [g[0], g[1], g[2]],
        orientation: [g[3], g[4], g[5], g[6]],
        vel: [g[7], g[8], g[9]],
        ang_vel: [g[10], g[11], g[12]],
    };
    Ok(RigidGradients {
        loss,
        initial: (0..n)
            .map(|i| unflat(&grad[i * PER_BODY..(i + 1) * PER_BODY]))
            .collect(),
        compliance: grad[params - 1],
        activations,
    })
}
