//! Rigid bodies with orientation (§36.1–§36.3): GJK/EPA colliders (spheres,
//! boxes, capsules, convex hulls), contact manifolds, angular XPBD dynamics,
//! per-collider materials and rolling friction.
//!
//! The solver follows Müller et al., "Detailed Rigid Body Simulation with
//! Extended Position Based Dynamics" (SCA 2020): substeps integrate
//! position and orientation, project every contact point once with the
//! generalized inverse mass `w = 1/m + (r × n)ᵀ I⁻¹ (r × n)` and apply the
//! positional correction to both position and orientation, then derive
//! linear and angular velocities and run a velocity pass for dynamic
//! friction, restitution and rolling friction. Static friction is
//! positional (§3.5 of the paper).
//!
//! Per tick: an AABB broadphase (the LBVH or the sort-and-sweep kernels)
//! over swept collider bounds, a binary64 GJK/EPA narrowphase
//! ([`crate::gjk`]) that builds up to four contact points per pair
//! ([`ContactManifold`]), and the deterministic colouring
//! ([`crate::coloring`]) of manifolds. Each manifold keeps its normal for
//! the tick and stores its points as body-local anchors, so penetration is
//! re-measured every substep as the bodies move and turn.
//!
//! All solver arithmetic is binary32 through [`det`]: single IEEE-754
//! operations in a fixed order, no fused multiply-add, `sqrt` the only
//! non-basic operation. The narrowphase is binary64 with the same rules.
//! The state therefore evolves to the same bits on every target; the
//! tests pin a golden hash.
//!
//! This world is separate from [`crate::World`], which keeps the §36.2
//! `RigidBodyTile` layout (positions without orientation) and its
//! wave-five golden hash. Joints are not modelled here.

// `!(x > y)` is deliberate: it is also true when `x` is NaN.
#![allow(clippy::neg_cmp_op_on_partial_ord)]

use crate::coloring::{Colorer, ColoringError, ConstraintBodies};
use crate::gjk::{self, GjkError, Shape};
use crate::lbvh::{Aabb, Lbvh, LbvhError};
use crate::sap::{SapError, SweepAndPrune};
use crate::snapshot::{KineticSnapshotWireHdr, WorldHashes};
use crate::world::det;
use sha2::Digest;

/// Most points in one contact manifold.
pub const MAX_MANIFOLD_POINTS: usize = 4;

/// Largest convex hull accepted.
pub const MAX_HULL_VERTICES: usize = 64;

type V3 = [f32; 3];
type Q = [f32; 4];

/// A collider's geometry in its body's frame (centred on the body origin).
#[derive(Debug, Clone, PartialEq)]
pub enum ColliderShape {
    Sphere {
        radius: f32,
    },
    /// A box with the given half extents along the body's x, y, z axes.
    Box {
        half_extents: V3,
    },
    /// The segment from `-half_height` to `+half_height` on the body's y
    /// axis, swept by `radius`.
    Capsule {
        half_height: f32,
        radius: f32,
    },
    /// The convex hull of the vertices (4 to [`MAX_HULL_VERTICES`], with
    /// volume).
    ConvexHull {
        vertices: Vec<V3>,
    },
}

/// Surface material. A contact combines the two materials by the
/// arithmetic mean of each coefficient (restitution: the larger).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct RigidMaterial {
    /// Coulomb static coefficient (positional).
    pub static_friction: f32,
    /// Coulomb dynamic coefficient (velocity pass).
    pub dynamic_friction: f32,
    /// Normal restitution in `[0, 1]`.
    pub restitution: f32,
    /// Rolling resistance, in metres: the rolling torque is at most
    /// `rolling_friction × normal force`.
    pub rolling_friction: f32,
}

/// A collider: shape and material.
#[derive(Debug, Clone, PartialEq)]
pub struct RigidCollider {
    pub shape: ColliderShape,
    pub material: RigidMaterial,
}

/// One rigid body.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RigidBody {
    pub pos: V3,
    /// Unit quaternion `[x, y, z, w]` from body to world.
    pub orientation: Q,
    pub vel: V3,
    /// Angular velocity in world coordinates (rad/s).
    pub ang_vel: V3,
    /// `0.0` marks a static body (its inverse inertia is ignored).
    pub inv_mass: f32,
    /// Inverse principal moments of inertia about the body axes (`0.0`
    /// locks rotation about that axis). See [`sphere_inv_inertia`],
    /// [`box_inv_inertia`], [`capsule_inv_inertia`].
    pub inv_inertia: V3,
    pub collider: u32,
    pub layer_mask: u32,
}

impl RigidBody {
    /// A dynamic body at rest with identity orientation.
    pub fn new(pos: V3, inv_mass: f32, inv_inertia: V3, collider: u32) -> Self {
        Self {
            pos,
            orientation: [0.0, 0.0, 0.0, 1.0],
            vel: [0.0; 3],
            ang_vel: [0.0; 3],
            inv_mass,
            inv_inertia,
            collider,
            layer_mask: 1,
        }
    }
}

/// Inverse inertia of a solid sphere of inverse mass `inv_mass`.
pub fn sphere_inv_inertia(inv_mass: f32, radius: f32) -> V3 {
    let i = det::div(inv_mass, det::mul(0.4, det::mul(radius, radius)));
    [i; 3]
}

/// Inverse inertia of a solid box with half extents `h`.
pub fn box_inv_inertia(inv_mass: f32, h: V3) -> V3 {
    let sq = |a: f32| det::mul(a, a);
    let f = |a: f32, b: f32| det::div(det::mul(3.0, inv_mass), det::add(sq(a), sq(b)));
    [f(h[1], h[2]), f(h[0], h[2]), f(h[0], h[1])]
}

/// Inverse inertia of a solid capsule (cylinder of height `2·half_height`
/// plus two hemispheres, uniform density) about its axes (y is the axis).
pub fn capsule_inv_inertia(inv_mass: f32, half_height: f32, radius: f32) -> V3 {
    let (r, hgt) = (f64::from(radius), 2.0 * f64::from(half_height));
    let m = 1.0 / f64::from(inv_mass);
    // Volume shares (π cancels).
    let vc = r * r * hgt;
    let vs = 4.0 / 3.0 * r * r * r;
    let mc = m * vc / (vc + vs);
    let ms = m - mc;
    let axial = mc * r * r / 2.0 + ms * 2.0 * r * r / 5.0;
    let lateral = mc * (r * r / 4.0 + hgt * hgt / 12.0)
        + ms * (2.0 * r * r / 5.0 + hgt * hgt / 4.0 + 3.0 * hgt * r / 8.0);
    let (a, l) = ((1.0 / axial) as f32, (1.0 / lateral) as f32);
    [l, a, l]
}

/// Static ground plane `y = height`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GroundPlane {
    pub height: f32,
    pub material: RigidMaterial,
}

/// Broadphase for [`RigidWorld`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RigidBroadphase {
    #[default]
    Lbvh,
    SweepAndPrune,
}

/// Rigid world parameters.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RigidConfig {
    pub dt: f32,
    pub substeps: u32,
    pub gravity: V3,
    pub ground: Option<GroundPlane>,
    /// Speculative contact distance.
    pub contact_margin: f32,
    /// Most manifolds per tick (pairs plus ground contacts).
    pub max_manifolds: usize,
    pub broadphase: RigidBroadphase,
}

impl Default for RigidConfig {
    fn default() -> Self {
        Self {
            dt: 1.0 / 120.0,
            substeps: 8,
            gravity: [0.0, -9.81, 0.0],
            ground: Some(GroundPlane {
                height: 0.0,
                material: RigidMaterial {
                    static_friction: 0.5,
                    dynamic_friction: 0.4,
                    restitution: 0.0,
                    rolling_friction: 0.0,
                },
            }),
            contact_margin: 0.02,
            max_manifolds: 1024,
            broadphase: RigidBroadphase::Lbvh,
        }
    }
}

/// Typed rigid-world failures. A failed step leaves the world unchanged.
#[derive(Debug, Clone, PartialEq)]
pub enum RigidError {
    InvalidConfig(&'static str),
    InvalidCollider {
        collider: usize,
        reason: &'static str,
    },
    InvalidBody {
        body: usize,
        field: &'static str,
    },
    UnknownCollider {
        body: usize,
        collider: u32,
    },
    ManifoldOverflow {
        capacity: usize,
    },
    Narrowphase {
        a: usize,
        b: usize,
        error: GjkError,
    },
    Coloring(ColoringError),
    Lbvh(LbvhError),
    SweepAndPrune(SapError),
    TickOverflow,
    /// The state left the finite range (a body was launched to infinity).
    NonFinite {
        body: usize,
    },
}

impl std::fmt::Display for RigidError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidConfig(w) => write!(f, "invalid rigid world configuration: {w}"),
            Self::InvalidCollider { collider, reason } => {
                write!(f, "collider {collider}: {reason}")
            }
            Self::InvalidBody { body, field } => write!(f, "body {body} has invalid {field}"),
            Self::UnknownCollider { body, collider } => {
                write!(f, "body {body} names unknown collider {collider}")
            }
            Self::ManifoldOverflow { capacity } => {
                write!(f, "more than {capacity} contact manifolds in one tick")
            }
            Self::Narrowphase { a, b, error } => write!(f, "narrowphase {a}-{b}: {error}"),
            Self::Coloring(e) => write!(f, "manifold colouring: {e}"),
            Self::Lbvh(e) => write!(f, "LBVH broadphase: {e}"),
            Self::SweepAndPrune(e) => write!(f, "sort-and-sweep broadphase: {e}"),
            Self::TickOverflow => write!(f, "tick counter overflow"),
            Self::NonFinite { body } => write!(f, "body {body} left the finite range"),
        }
    }
}

impl std::error::Error for RigidError {}

/// One contact point of a manifold.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ContactPoint {
    /// Anchor on body `a`'s core (the shape without its rounding radius),
    /// in `a`'s frame. The surface point is the anchor moved by
    /// `radius_a` against the normal, so a rolling sphere's contact point
    /// stays under its centre.
    pub local_a: V3,
    /// Anchor on body `b`'s core in `b`'s frame, or the world point on the
    /// ground.
    pub local_b: V3,
    /// Penetration when the manifold was built (negative: a gap within the
    /// margin).
    pub depth: f32,
    lambda_n: f32,
    vn_prev: f32,
}

/// Up to four contact points between body `a` and body `b` (or the ground,
/// `b == None`) sharing one normal, which points from `b` to `a`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ContactManifold {
    pub a: u32,
    pub b: Option<u32>,
    pub normal: V3,
    pub count: usize,
    pub points: [ContactPoint; MAX_MANIFOLD_POINTS],
    /// Rounding radius of `a`'s collider (sphere and capsule radius).
    pub radius_a: f32,
    /// Rounding radius of `b`'s collider (zero for the ground).
    pub radius_b: f32,
    material: RigidMaterial,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct State {
    x: V3,
    q: Q,
    v: V3,
    w: V3,
    inv_mass: f32,
    inv_inertia: V3,
    collider: u32,
    layer: u32,
}

/// A deterministic XPBD rigid-body world.
#[derive(Debug)]
pub struct RigidWorld {
    config: RigidConfig,
    colliders: Vec<RigidCollider>,
    bodies: Vec<State>,
    tick: u64,
    // Per-tick scratch.
    prev: Vec<(V3, Q)>,
    prev_vel: Vec<(V3, V3)>,
    integrated: Vec<(V3, Q)>,
    manifolds: Vec<ContactManifold>,
    constraint_bodies: Vec<ConstraintBodies>,
    colorer: Colorer,
    boxes: Vec<Aabb>,
    sweeps: Vec<f32>,
    pairs: Vec<(u32, u32)>,
    lbvh: Lbvh,
    sap: SweepAndPrune,
    verts_a: Vec<[f64; 3]>,
    verts_b: Vec<[f64; 3]>,
    layers: usize,
}

// ----- vector and quaternion helpers (binary32, fixed order) -----------------

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
fn dot(a: V3, b: V3) -> f32 {
    det::dot(a, b)
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
    det::sqrt(dot(a, a))
}

/// `q ⊗ p` (Hamilton product, `[x, y, z, w]`).
#[inline]
fn qmul(q: Q, p: Q) -> Q {
    let (qv, qw) = ([q[0], q[1], q[2]], q[3]);
    let (pv, pw) = ([p[0], p[1], p[2]], p[3]);
    let c = cross(qv, pv);
    let v = add(add(scale(pv, qw), scale(qv, pw)), c);
    [v[0], v[1], v[2], det::sub(det::mul(qw, pw), dot(qv, pv))]
}

#[inline]
fn scale4(q: Q, s: f32) -> Q {
    [
        det::mul(q[0], s),
        det::mul(q[1], s),
        det::mul(q[2], s),
        det::mul(q[3], s),
    ]
}

#[inline]
fn conj(q: Q) -> Q {
    [det::neg(q[0]), det::neg(q[1]), det::neg(q[2]), q[3]]
}

/// Rotate `v` by unit quaternion `q`: `v + 2w(u × v) + 2u × (u × v)`.
#[inline]
fn rotate(q: Q, v: V3) -> V3 {
    let u = [q[0], q[1], q[2]];
    let t = scale(cross(u, v), 2.0);
    add(add(v, scale(t, q[3])), cross(u, t))
}

#[inline]
fn rotate_inv(q: Q, v: V3) -> V3 {
    rotate(conj(q), v)
}

#[inline]
fn normalize_q(q: Q) -> Q {
    let n = det::sqrt(det::add(
        det::add(
            det::add(det::mul(q[0], q[0]), det::mul(q[1], q[1])),
            det::mul(q[2], q[2]),
        ),
        det::mul(q[3], q[3]),
    ));
    if !(n > 0.0) {
        return [0.0, 0.0, 0.0, 1.0];
    }
    let s = det::div(1.0, n);
    [
        det::mul(q[0], s),
        det::mul(q[1], s),
        det::mul(q[2], s),
        det::mul(q[3], s),
    ]
}

/// `I⁻¹ x` in world coordinates for a body with orientation `q`.
#[inline]
fn inv_inertia_world(s: &State, x: V3) -> V3 {
    let l = rotate_inv(s.q, x);
    let l = [
        det::mul(l[0], s.inv_inertia[0]),
        det::mul(l[1], s.inv_inertia[1]),
        det::mul(l[2], s.inv_inertia[2]),
    ];
    rotate(s.q, l)
}

/// `xᵀ I⁻¹ x` (the rotational part of a generalized inverse mass).
#[inline]
fn angular_weight(s: &State, x: V3) -> f32 {
    let l = rotate_inv(s.q, x);
    det::add(
        det::add(
            det::mul(det::mul(l[0], l[0]), s.inv_inertia[0]),
            det::mul(det::mul(l[1], l[1]), s.inv_inertia[1]),
        ),
        det::mul(det::mul(l[2], l[2]), s.inv_inertia[2]),
    )
}

fn finite3(v: V3) -> bool {
    v.iter().all(|x| x.is_finite())
}

fn combine(a: RigidMaterial, b: RigidMaterial) -> RigidMaterial {
    let mean = |x: f32, y: f32| det::mul(det::add(x, y), 0.5);
    RigidMaterial {
        static_friction: mean(a.static_friction, b.static_friction),
        dynamic_friction: mean(a.dynamic_friction, b.dynamic_friction),
        restitution: det::max(a.restitution, b.restitution),
        rolling_friction: mean(a.rolling_friction, b.rolling_friction),
    }
}

fn validate_material(m: &RigidMaterial) -> bool {
    let ok = |v: f32| v.is_finite() && v >= 0.0;
    ok(m.static_friction)
        && ok(m.dynamic_friction)
        && ok(m.rolling_friction)
        && ok(m.restitution)
        && m.restitution <= 1.0
}

/// Signed volume test: whether a vertex set spans three dimensions.
fn has_volume(v: &[V3]) -> bool {
    let p0 = v[0].map(f64::from);
    let d = |p: &V3| {
        [
            f64::from(p[0]) - p0[0],
            f64::from(p[1]) - p0[1],
            f64::from(p[2]) - p0[2],
        ]
    };
    let scale = v
        .iter()
        .flat_map(|p| p.iter().map(|x| f64::from(*x).abs()))
        .fold(1e-30f64, f64::max);
    for i in 1..v.len() {
        for j in i + 1..v.len() {
            for k in j + 1..v.len() {
                let (a, b, c) = (d(&v[i]), d(&v[j]), d(&v[k]));
                let det3 = a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0])
                    + a[2] * (b[0] * c[1] - b[1] * c[0]);
                if det3.abs() > 1e-9 * scale * scale * scale {
                    return true;
                }
            }
        }
    }
    false
}

// ----- narrowphase geometry (binary64) -----------------------------------------

fn to64(v: V3) -> [f64; 3] {
    v.map(f64::from)
}

fn rotate64(q: Q, v: [f64; 3]) -> [f64; 3] {
    let q = q.map(f64::from);
    let u = [q[0], q[1], q[2]];
    let c1 = cross64(u, v);
    let t = [2.0 * c1[0], 2.0 * c1[1], 2.0 * c1[2]];
    let c2 = cross64(u, t);
    [
        v[0] + q[3] * t[0] + c2[0],
        v[1] + q[3] * t[1] + c2[1],
        v[2] + q[3] * t[2] + c2[2],
    ]
}

fn cross64(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn dot64(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn sub64(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn axpy64(a: [f64; 3], s: f64, d: [f64; 3]) -> [f64; 3] {
    [a[0] + s * d[0], a[1] + s * d[1], a[2] + s * d[2]]
}

/// World-space core vertices (into `out`) and radius of a collider.
fn world_core(shape: &ColliderShape, s: &State, out: &mut Vec<[f64; 3]>) -> f64 {
    out.clear();
    let x = to64(s.x);
    let place = |l: [f64; 3]| {
        let r = rotate64(s.q, l);
        [x[0] + r[0], x[1] + r[1], x[2] + r[2]]
    };
    match shape {
        ColliderShape::Sphere { radius } => {
            out.push(x);
            f64::from(*radius)
        }
        ColliderShape::Capsule {
            half_height,
            radius,
        } => {
            let h = f64::from(*half_height);
            out.push(place([0.0, -h, 0.0]));
            out.push(place([0.0, h, 0.0]));
            f64::from(*radius)
        }
        ColliderShape::Box { half_extents } => {
            let h = to64(*half_extents);
            for i in 0..8 {
                let sx = if i & 1 == 0 { -h[0] } else { h[0] };
                let sy = if i & 2 == 0 { -h[1] } else { h[1] };
                let sz = if i & 4 == 0 { -h[2] } else { h[2] };
                out.push(place([sx, sy, sz]));
            }
            0.0
        }
        ColliderShape::ConvexHull { vertices } => {
            for v in vertices {
                out.push(place(to64(*v)));
            }
            0.0
        }
    }
}

fn gjk_shape(core: &[[f64; 3]], radius: f64) -> Shape<'_> {
    match core.len() {
        1 if radius > 0.0 => Shape::Sphere {
            center: core[0],
            radius,
        },
        2 => Shape::Capsule {
            a: core[0],
            b: core[1],
            radius,
        },
        _ => Shape::Polytope { vertices: core },
    }
}

/// A candidate contact in world coordinates: points on `a` and `b` and the
/// penetration along the normal.
#[derive(Debug, Clone, Copy)]
struct Candidate {
    pa: [f64; 3],
    pb: [f64; 3],
    depth: f64,
}

/// Keep at most four candidates: the deepest, the farthest from it, the one
/// spanning the largest triangle, then the one farthest outside that
/// triangle's plane-projected area. Deterministic (first maximiser wins).
fn reduce(cands: &mut Vec<Candidate>, n: [f64; 3], merge: f64) {
    // Merge near-duplicates (keep the deeper; the earlier on ties).
    let mut kept: Vec<Candidate> = Vec::with_capacity(cands.len());
    for c in cands.iter() {
        match kept.iter_mut().find(|k| {
            let d = sub64(k.pa, c.pa);
            dot64(d, d) <= merge * merge
        }) {
            Some(k) => {
                if c.depth > k.depth {
                    *k = *c;
                }
            }
            None => kept.push(*c),
        }
    }
    if kept.len() <= MAX_MANIFOLD_POINTS {
        *cands = kept;
        return;
    }
    let mut out: Vec<Candidate> = Vec::with_capacity(MAX_MANIFOLD_POINTS);
    let pick = |kept: &[Candidate], score: &dyn Fn(&Candidate) -> f64| -> usize {
        let mut best = 0;
        let mut best_s = f64::NEG_INFINITY;
        for (i, c) in kept.iter().enumerate() {
            let s = score(c);
            if s > best_s {
                best = i;
                best_s = s;
            }
        }
        best
    };
    let i0 = pick(&kept, &|c| c.depth);
    out.push(kept.remove(i0));
    let p0 = out[0].pa;
    let i1 = pick(&kept, &|c| {
        let d = sub64(c.pa, p0);
        dot64(d, d)
    });
    out.push(kept.remove(i1));
    let p1 = out[1].pa;
    let i2 = pick(&kept, &|c| {
        let a = cross64(sub64(p1, p0), sub64(c.pa, p0));
        dot64(a, n).abs()
    });
    out.push(kept.remove(i2));
    let p2 = out[2].pa;
    let i3 = pick(&kept, &|c| {
        // Largest area added: the biggest of the three triangles the new
        // point forms with each edge, outside the existing triangle.
        let area = |u: [f64; 3], v: [f64; 3]| -> f64 {
            let a = cross64(sub64(v, u), sub64(c.pa, u));
            -dot64(a, n)
        };
        let orient = dot64(cross64(sub64(p1, p0), sub64(p2, p0)), n);
        let s = if orient >= 0.0 { 1.0 } else { -1.0 };
        (s * area(p0, p1))
            .max(s * area(p1, p2))
            .max(s * area(p2, p0))
    });
    out.push(kept.remove(i3));
    *cands = out;
}

// ----- the world ----------------------------------------------------------------

impl RigidWorld {
    /// Build a world, validating every input and reserving per-tick storage.
    pub fn new(
        config: RigidConfig,
        bodies: &[RigidBody],
        colliders: &[RigidCollider],
    ) -> Result<Self, RigidError> {
        if !(config.dt.is_finite() && config.dt > 0.0) {
            return Err(RigidError::InvalidConfig("dt must be finite and positive"));
        }
        if config.substeps == 0 || config.substeps > 64 {
            return Err(RigidError::InvalidConfig("substeps must be in 1..=64"));
        }
        if !finite3(config.gravity) {
            return Err(RigidError::InvalidConfig("gravity must be finite"));
        }
        if !(config.contact_margin.is_finite() && config.contact_margin >= 0.0) {
            return Err(RigidError::InvalidConfig("contact_margin must be >= 0"));
        }
        if let Some(g) = config.ground
            && !(g.height.is_finite() && validate_material(&g.material))
        {
            return Err(RigidError::InvalidConfig("ground height or material"));
        }
        if bodies.len() > (u32::MAX / 2) as usize || config.max_manifolds > (u32::MAX / 2) as usize
        {
            return Err(RigidError::InvalidConfig("too many bodies or manifolds"));
        }
        for (i, c) in colliders.iter().enumerate() {
            let bad = |reason| RigidError::InvalidCollider {
                collider: i,
                reason,
            };
            if !validate_material(&c.material) {
                return Err(bad(
                    "material coefficients must be finite, >= 0, restitution <= 1",
                ));
            }
            let pos = |v: f32| v.is_finite() && v > 0.0;
            match &c.shape {
                ColliderShape::Sphere { radius } => {
                    if !pos(*radius) {
                        return Err(bad("sphere radius must be positive"));
                    }
                }
                ColliderShape::Box { half_extents } => {
                    if !half_extents.iter().all(|h| pos(*h)) {
                        return Err(bad("box half extents must be positive"));
                    }
                }
                ColliderShape::Capsule {
                    half_height,
                    radius,
                } => {
                    if !(pos(*radius) && half_height.is_finite() && *half_height >= 0.0) {
                        return Err(bad("capsule radius must be positive, half height >= 0"));
                    }
                }
                ColliderShape::ConvexHull { vertices } => {
                    if vertices.len() < 4 || vertices.len() > MAX_HULL_VERTICES {
                        return Err(bad("convex hull needs 4 to 64 vertices"));
                    }
                    if !vertices.iter().all(|v| finite3(*v)) {
                        return Err(bad("convex hull vertices must be finite"));
                    }
                    if !has_volume(vertices) {
                        return Err(bad("convex hull is flat"));
                    }
                }
            }
        }
        let mut states = Vec::with_capacity(bodies.len());
        for (i, b) in bodies.iter().enumerate() {
            let bad = |field| RigidError::InvalidBody { body: i, field };
            if !(finite3(b.pos) && finite3(b.vel) && finite3(b.ang_vel)) {
                return Err(bad("position or velocity"));
            }
            if !(b.inv_mass.is_finite() && b.inv_mass >= 0.0) {
                return Err(bad("inv_mass"));
            }
            if !b.inv_inertia.iter().all(|v| v.is_finite() && *v >= 0.0) {
                return Err(bad("inv_inertia"));
            }
            if !b.orientation.iter().all(|v| v.is_finite()) {
                return Err(bad("orientation"));
            }
            let n2: f64 = b
                .orientation
                .iter()
                .map(|v| f64::from(*v) * f64::from(*v))
                .sum();
            if (n2 - 1.0).abs() > 1e-4 {
                return Err(bad("orientation must be a unit quaternion"));
            }
            if b.collider as usize >= colliders.len() {
                return Err(RigidError::UnknownCollider {
                    body: i,
                    collider: b.collider,
                });
            }
            let fixed = b.inv_mass == 0.0;
            states.push(State {
                x: b.pos,
                q: normalize_q(b.orientation),
                v: if fixed { [0.0; 3] } else { b.vel },
                w: if fixed { [0.0; 3] } else { b.ang_vel },
                inv_mass: b.inv_mass,
                inv_inertia: if fixed { [0.0; 3] } else { b.inv_inertia },
                collider: b.collider,
                layer: b.layer_mask,
            });
        }
        let n = bodies.len();
        let mut sap = SweepAndPrune::with_capacity(0);
        let lbvh = match config.broadphase {
            RigidBroadphase::Lbvh => Lbvh::with_capacity(n),
            RigidBroadphase::SweepAndPrune => {
                sap = SweepAndPrune::with_capacity(n);
                Lbvh::with_capacity(0)
            }
        };
        let max_pairs = config.max_manifolds.saturating_mul(4).saturating_add(n);
        if config.broadphase == RigidBroadphase::SweepAndPrune {
            sap.reserve_pairs(max_pairs);
        }
        Ok(Self {
            config,
            colliders: colliders.to_vec(),
            bodies: states,
            tick: 0,
            prev: vec![([0.0; 3], [0.0, 0.0, 0.0, 1.0]); n],
            prev_vel: vec![([0.0; 3], [0.0; 3]); n],
            integrated: vec![([0.0; 3], [0.0, 0.0, 0.0, 1.0]); n],
            manifolds: Vec::with_capacity(config.max_manifolds),
            constraint_bodies: Vec::with_capacity(config.max_manifolds),
            colorer: Colorer::new(n, config.max_manifolds),
            boxes: Vec::with_capacity(n),
            sweeps: vec![0.0; n],
            pairs: Vec::with_capacity(max_pairs),
            lbvh,
            sap,
            verts_a: Vec::with_capacity(MAX_HULL_VERTICES),
            verts_b: Vec::with_capacity(MAX_HULL_VERTICES),
            layers: 0,
        })
    }

    /// Ticks simulated.
    pub fn tick(&self) -> u64 {
        self.tick
    }

    /// The configuration.
    pub fn config(&self) -> &RigidConfig {
        &self.config
    }

    /// Number of bodies.
    pub fn len(&self) -> usize {
        self.bodies.len()
    }

    /// Whether the world has no bodies.
    pub fn is_empty(&self) -> bool {
        self.bodies.is_empty()
    }

    /// Body `i` (`None` past the end).
    pub fn body(&self, i: usize) -> Option<RigidBody> {
        self.bodies.get(i).map(|s| RigidBody {
            pos: s.x,
            orientation: s.q,
            vel: s.v,
            ang_vel: s.w,
            inv_mass: s.inv_mass,
            inv_inertia: s.inv_inertia,
            collider: s.collider,
            layer_mask: s.layer,
        })
    }

    /// The manifolds of the last step.
    pub fn manifolds(&self) -> &[ContactManifold] {
        &self.manifolds
    }

    /// Colours used by the last step.
    pub fn last_layer_count(&self) -> usize {
        self.layers
    }

    /// Add `dv` to a dynamic body's velocity (and `dw` to its angular
    /// velocity).
    pub fn apply_velocity_change(&mut self, body: usize, dv: V3, dw: V3) -> Result<(), RigidError> {
        let bad = |field| RigidError::InvalidBody { body, field };
        let s = self.bodies.get_mut(body).ok_or(bad("velocity change"))?;
        if s.inv_mass == 0.0 {
            return Err(bad("velocity change on a static body"));
        }
        let (v, w) = (add(s.v, dv), add(s.w, dw));
        if !(finite3(dv) && finite3(dw) && finite3(v) && finite3(w)) {
            return Err(bad("velocity change"));
        }
        s.v = v;
        s.w = w;
        Ok(())
    }

    /// SHA-256 and BLAKE3 of the canonical state: tick and body count
    /// (`u64le`), then per body the bit patterns (`u32le`) of position,
    /// orientation, velocity, angular velocity, inverse mass, inverse
    /// inertia, collider and layer mask.
    pub fn hash(&self) -> WorldHashes {
        let mut sha = sha2::Sha256::new();
        let mut b3 = blake3::Hasher::new();
        let mut feed = |bytes: &[u8]| {
            sha.update(bytes);
            b3.update(bytes);
        };
        feed(&self.tick.to_le_bytes());
        feed(&(self.bodies.len() as u64).to_le_bytes());
        for s in &self.bodies {
            let mut words = [0u32; 19];
            let fields =
                s.x.iter()
                    .chain(&s.q)
                    .chain(&s.v)
                    .chain(&s.w)
                    .chain(std::iter::once(&s.inv_mass))
                    .chain(&s.inv_inertia);
            for (w, f) in words.iter_mut().zip(fields) {
                *w = f.to_bits();
            }
            words[17] = s.collider;
            words[18] = s.layer;
            let mut record = [0u8; 76];
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

    /// Kinetic energy `Σ ½ m |v|² + ½ ωᵀ I ω` of dynamic bodies (binary64).
    pub fn kinetic_energy(&self) -> f64 {
        let mut e = 0.0;
        for s in &self.bodies {
            if s.inv_mass == 0.0 {
                continue;
            }
            let v = to64(s.v);
            e += 0.5 * dot64(v, v) / f64::from(s.inv_mass);
            let wl = to64(rotate_inv(s.q, s.w));
            for k in 0..3 {
                if s.inv_inertia[k] > 0.0 {
                    e += 0.5 * wl[k] * wl[k] / f64::from(s.inv_inertia[k]);
                }
            }
        }
        e
    }

    /// Bounding radius of a collider about its origin.
    fn bound_radius(&self, collider: u32) -> f32 {
        match &self.colliders[collider as usize].shape {
            ColliderShape::Sphere { radius } => *radius,
            ColliderShape::Capsule {
                half_height,
                radius,
            } => det::add(*half_height, *radius),
            ColliderShape::Box { half_extents } => length(*half_extents),
            ColliderShape::ConvexHull { vertices } => {
                vertices.iter().map(|v| length(*v)).fold(0.0, det::max)
            }
        }
    }

    /// How far any point of body `i` can move this tick (L1 bound on the
    /// ballistic motion plus the rotation sweep), zero for static bodies.
    fn sweep_of(&self, i: usize) -> f32 {
        let s = &self.bodies[i];
        if s.inv_mass == 0.0 {
            return 0.0;
        }
        let dt = self.config.dt;
        let l1 = |v: V3| det::add(det::add(v[0].abs(), v[1].abs()), v[2].abs());
        let lin = det::add(
            det::mul(l1(s.v), dt),
            det::mul(det::mul(l1(self.config.gravity), dt), dt),
        );
        let ang = det::mul(det::mul(l1(s.w), dt), self.bound_radius(s.collider));
        det::add(lin, ang)
    }

    /// Advance one tick. Returns the §36.2 header (BLAKE3 of
    /// [`RigidWorld::hash`]'s stream). On error the world is unchanged.
    pub fn step(&mut self) -> Result<KineticSnapshotWireHdr, RigidError> {
        let next_tick = self.tick.checked_add(1).ok_or(RigidError::TickOverflow)?;
        self.collide()?;
        self.constraint_bodies.clear();
        for m in &self.manifolds {
            self.constraint_bodies
                .push(ConstraintBodies { a: m.a, b: m.b });
        }
        self.colorer
            .color(&self.constraint_bodies)
            .map_err(RigidError::Coloring)?;
        self.layers = self.colorer.layer_count();
        let saved = self.bodies.clone();
        self.solve();
        if let Some(body) = self
            .bodies
            .iter()
            .position(|s| !(finite3(s.x) && finite3(s.v) && finite3(s.w)))
        {
            self.bodies = saved;
            return Err(RigidError::NonFinite { body });
        }
        self.tick = next_tick;
        Ok(self.header())
    }

    /// The snapshot header for the current state.
    pub fn header(&self) -> KineticSnapshotWireHdr {
        KineticSnapshotWireHdr {
            tick_number: self.tick,
            active_body_count: self.bodies.len() as u32,
            manifold_count: self.manifolds.len() as u32,
            world_state_blake3: self.hash().blake3,
        }
    }

    /// Broadphase and narrowphase: fill `self.manifolds`.
    fn collide(&mut self) -> Result<(), RigidError> {
        let n = self.bodies.len();
        let margin = self.config.contact_margin;
        self.manifolds.clear();
        self.boxes.clear();
        for i in 0..n {
            self.sweeps[i] = self.sweep_of(i);
            if !self.sweeps[i].is_finite() {
                return Err(RigidError::NonFinite { body: i });
            }
            let collider = self.bodies[i].collider;
            let shape = &self.colliders[collider as usize].shape;
            let r = world_core(shape, &self.bodies[i], &mut self.verts_a);
            let grow = f64::from(det::add(self.sweeps[i], det::mul(margin, 0.5))) + r;
            let mut lo = [f64::INFINITY; 3];
            let mut hi = [f64::NEG_INFINITY; 3];
            for v in &self.verts_a {
                for k in 0..3 {
                    lo[k] = lo[k].min(v[k]);
                    hi[k] = hi[k].max(v[k]);
                }
            }
            // Round outward so that no binary32 rounding can drop a pair.
            let out = |x: f64, up: bool| -> f32 {
                let slack = 1e-6 * (x.abs() + 1.0);
                (if up { x + slack } else { x - slack }) as f32
            };
            self.boxes.push(Aabb {
                min: [
                    out(lo[0] - grow, false),
                    out(lo[1] - grow, false),
                    out(lo[2] - grow, false),
                ],
                max: [
                    out(hi[0] + grow, true),
                    out(hi[1] + grow, true),
                    out(hi[2] + grow, true),
                ],
            });
            // Ground contacts.
            if let Some(ground) = self.config.ground
                && self.bodies[i].inv_mass > 0.0
                && lo[1] - r - f64::from(ground.height)
                    < f64::from(det::add(margin, self.sweeps[i]))
            {
                self.ground_manifold(i, ground, r)?;
            }
        }
        let max_pairs = self.pairs.capacity();
        match self.config.broadphase {
            RigidBroadphase::Lbvh => {
                self.lbvh.build(&self.boxes).map_err(RigidError::Lbvh)?;
                self.lbvh
                    .overlapping_pairs(&mut self.pairs, max_pairs)
                    .map_err(RigidError::Lbvh)?;
            }
            RigidBroadphase::SweepAndPrune => {
                self.sap
                    .build(&self.boxes)
                    .map_err(RigidError::SweepAndPrune)?;
                self.sap
                    .overlapping_pairs(&mut self.pairs, max_pairs)
                    .map_err(RigidError::SweepAndPrune)?;
            }
        }
        for k in 0..self.pairs.len() {
            let (a, b) = self.pairs[k];
            let (sa, sb) = (&self.bodies[a as usize], &self.bodies[b as usize]);
            if (sa.inv_mass == 0.0 && sb.inv_mass == 0.0) || sa.layer & sb.layer == 0 {
                continue;
            }
            self.pair_manifold(a as usize, b as usize)?;
        }
        Ok(())
    }

    fn push_manifold(&mut self, m: ContactManifold) -> Result<(), RigidError> {
        if self.manifolds.len() == self.config.max_manifolds {
            self.manifolds.clear();
            return Err(RigidError::ManifoldOverflow {
                capacity: self.config.max_manifolds,
            });
        }
        self.manifolds.push(m);
        Ok(())
    }

    fn ground_manifold(
        &mut self,
        i: usize,
        ground: GroundPlane,
        radius: f64,
    ) -> Result<(), RigidError> {
        let s = self.bodies[i];
        let margin = f64::from(det::add(self.config.contact_margin, self.sweeps[i]));
        let h = f64::from(ground.height);
        let mut cands: Vec<Candidate> = Vec::with_capacity(self.verts_a.len());
        for v in &self.verts_a {
            let pa = [v[0], v[1] - radius, v[2]];
            let depth = h - pa[1];
            if depth > -margin {
                cands.push(Candidate {
                    pa,
                    pb: [pa[0], h, pa[2]],
                    depth,
                });
            }
        }
        if cands.is_empty() {
            return Ok(());
        }
        let merge = 1e-4 * (f64::from(self.bound_radius(s.collider)) + 1e-3);
        reduce(&mut cands, [0.0, 1.0, 0.0], merge);
        let material = combine(
            self.colliders[s.collider as usize].material,
            ground.material,
        );
        let mut m = ContactManifold {
            a: i as u32,
            b: None,
            normal: [0.0, 1.0, 0.0],
            count: cands.len(),
            points: [ContactPoint::default(); MAX_MANIFOLD_POINTS],
            radius_a: radius as f32,
            radius_b: 0.0,
            material,
        };
        for (p, c) in m.points.iter_mut().zip(&cands) {
            let core = [c.pa[0], c.pa[1] + radius, c.pa[2]];
            let la = rotate64(conj(s.q), sub64(core, to64(s.x)));
            p.local_a = la.map(|v| v as f32);
            p.local_b = c.pb.map(|v| v as f32);
            p.depth = c.depth as f32;
        }
        self.push_manifold(m)
    }

    fn pair_manifold(&mut self, a: usize, b: usize) -> Result<(), RigidError> {
        let (sa, sb) = (self.bodies[a], self.bodies[b]);
        let ca = &self.colliders[sa.collider as usize];
        let cb = &self.colliders[sb.collider as usize];
        let ra = world_core(&ca.shape, &sa, &mut self.verts_a);
        let rb = world_core(&cb.shape, &sb, &mut self.verts_b);
        let shape_a = gjk_shape(&self.verts_a, ra);
        let shape_b = gjk_shape(&self.verts_b, rb);
        let sep = gjk::separation(&shape_a, &shape_b).map_err(|error| RigidError::Narrowphase {
            a,
            b,
            error,
        })?;
        let margin = f64::from(det::add(
            self.config.contact_margin,
            det::add(self.sweeps[a], self.sweeps[b]),
        ));
        if sep.distance > margin {
            return Ok(());
        }
        // Normal from b to a.
        let n = [-sep.normal[0], -sep.normal[1], -sep.normal[2]];
        let neg_n = sep.normal;
        // Extents along n: a's lowest, b's highest.
        let a_min = dot64(shape_a.support(neg_n), n);
        let b_max = dot64(shape_b.support(n), n);
        let scale = f64::from(det::max(
            self.bound_radius(sa.collider),
            self.bound_radius(sb.collider),
        ));
        let tol = 0.25 * f64::from(self.config.contact_margin) + 1e-6 * (scale + 1.0);
        let mut cands = vec![Candidate {
            pa: sep.point_a,
            pb: sep.point_b,
            depth: -sep.distance,
        }];
        // Vertices of b against a's support plane.
        if self.verts_a.len() > 1 || self.verts_b.len() > 1 {
            for v in &self.verts_b {
                let s = axpy64(*v, rb, n);
                let depth = dot64(s, n) - a_min;
                if depth > -margin {
                    let pa = axpy64(s, -depth, n);
                    let probe = Shape::Sphere {
                        center: pa,
                        radius: 0.0,
                    };
                    if gjk::distance(&probe, &shape_a).is_ok_and(|d| d <= tol) {
                        cands.push(Candidate { pa, pb: s, depth });
                    }
                }
            }
            for u in &self.verts_a {
                let s = axpy64(*u, -ra, n);
                let depth = b_max - dot64(s, n);
                if depth > -margin {
                    let pb = axpy64(s, depth, n);
                    let probe = Shape::Sphere {
                        center: pb,
                        radius: 0.0,
                    };
                    if gjk::distance(&probe, &shape_b).is_ok_and(|d| d <= tol) {
                        cands.push(Candidate { pa: s, pb, depth });
                    }
                }
            }
        }
        reduce(&mut cands, n, 1e-4 * (scale + 1e-3));
        let material = combine(ca.material, cb.material);
        let mut m = ContactManifold {
            a: a as u32,
            b: Some(b as u32),
            normal: n.map(|v| v as f32),
            count: cands.len(),
            points: [ContactPoint::default(); MAX_MANIFOLD_POINTS],
            radius_a: ra as f32,
            radius_b: rb as f32,
            material,
        };
        for (p, c) in m.points.iter_mut().zip(&cands) {
            let core_a = axpy64(c.pa, ra, n);
            let core_b = axpy64(c.pb, -rb, n);
            p.local_a = rotate64(conj(sa.q), sub64(core_a, to64(sa.x))).map(|v| v as f32);
            p.local_b = rotate64(conj(sb.q), sub64(core_b, to64(sb.x))).map(|v| v as f32);
            p.depth = c.depth as f32;
        }
        self.push_manifold(m)
    }

    /// World contact points (current state) of point `k` of manifold `m`.
    #[inline]
    fn points_of(&self, m: &ContactManifold, k: usize) -> (V3, V3, V3, V3) {
        let p = &m.points[k];
        let sa = &self.bodies[m.a as usize];
        let ra = sub(rotate(sa.q, p.local_a), scale(m.normal, m.radius_a));
        let pa = add(sa.x, ra);
        match m.b {
            Some(b) => {
                let sb = &self.bodies[b as usize];
                let rb = add(rotate(sb.q, p.local_b), scale(m.normal, m.radius_b));
                (pa, ra, add(sb.x, rb), rb)
            }
            None => (pa, ra, p.local_b, [0.0; 3]),
        }
    }

    /// Generalized inverse mass of a manifold's bodies in direction `d` at
    /// arms `ra`, `rb`.
    #[inline]
    fn weight(&self, m: &ContactManifold, ra: V3, rb: V3, d: V3) -> f32 {
        let sa = &self.bodies[m.a as usize];
        let mut w = det::add(sa.inv_mass, angular_weight(sa, cross(ra, d)));
        if let Some(b) = m.b {
            let sb = &self.bodies[b as usize];
            w = det::add(w, det::add(sb.inv_mass, angular_weight(sb, cross(rb, d))));
        }
        w
    }

    /// Apply the positional impulse `p` at arms `ra` (to `a`) and `rb`
    /// (`-p` to `b`): position and orientation (Müller et al. eq. 6–9).
    fn apply_positional(&mut self, m: &ContactManifold, ra: V3, rb: V3, p: V3) {
        let turn = |s: &mut State, r: V3, p: V3| {
            let dq = inv_inertia_world(s, cross(r, p));
            let rot = qmul([dq[0], dq[1], dq[2], 0.0], s.q);
            s.q = [
                det::add(s.q[0], det::mul(0.5, rot[0])),
                det::add(s.q[1], det::mul(0.5, rot[1])),
                det::add(s.q[2], det::mul(0.5, rot[2])),
                det::add(s.q[3], det::mul(0.5, rot[3])),
            ];
        };
        let a = &mut self.bodies[m.a as usize];
        if a.inv_mass > 0.0 {
            a.x = add(a.x, scale(p, a.inv_mass));
            turn(a, ra, p);
        }
        if let Some(b) = m.b {
            let b = &mut self.bodies[b as usize];
            if b.inv_mass > 0.0 {
                let np = scale(p, -1.0);
                b.x = add(b.x, scale(np, b.inv_mass));
                turn(b, rb, np);
            }
        }
    }

    /// Apply the velocity impulse `p` at arms `ra` (to `a`) and `rb` (`-p`
    /// to `b`).
    fn apply_impulse(&mut self, m: &ContactManifold, ra: V3, rb: V3, p: V3) {
        let a = &mut self.bodies[m.a as usize];
        if a.inv_mass > 0.0 {
            a.v = add(a.v, scale(p, a.inv_mass));
            a.w = add(a.w, inv_inertia_world(a, cross(ra, p)));
        }
        if let Some(b) = m.b {
            let b = &mut self.bodies[b as usize];
            if b.inv_mass > 0.0 {
                let np = scale(p, -1.0);
                b.v = add(b.v, scale(np, b.inv_mass));
                b.w = add(b.w, inv_inertia_world(b, cross(rb, np)));
            }
        }
    }

    /// Relative velocity of the contact points (`a` minus `b`).
    fn relative_velocity(&self, m: &ContactManifold, ra: V3, rb: V3, prev: bool) -> V3 {
        let (va, wa) = if prev {
            self.prev_vel[m.a as usize]
        } else {
            let s = &self.bodies[m.a as usize];
            (s.v, s.w)
        };
        let mut rel = add(va, cross(wa, ra));
        if let Some(b) = m.b {
            let (vb, wb) = if prev {
                self.prev_vel[b as usize]
            } else {
                let s = &self.bodies[b as usize];
                (s.v, s.w)
            };
            rel = sub(rel, add(vb, cross(wb, rb)));
        }
        rel
    }

    fn solve(&mut self) {
        let h = det::div(self.config.dt, self.config.substeps as f32);
        let inv_h = det::div(1.0, h);
        let g = self.config.gravity;
        let g1 = det::add(det::add(g[0].abs(), g[1].abs()), g[2].abs());
        let bounce_floor = det::mul(det::mul(2.0, g1), h);
        let mut manifolds = std::mem::take(&mut self.manifolds);
        for _ in 0..self.config.substeps {
            // Integrate.
            for (i, s) in self.bodies.iter_mut().enumerate() {
                self.prev[i] = (s.x, s.q);
                self.prev_vel[i] = (s.v, s.w);
                if s.inv_mass == 0.0 {
                    continue;
                }
                s.v = add(s.v, scale(g, h));
                s.x = add(s.x, scale(s.v, h));
                let spin = qmul([s.w[0], s.w[1], s.w[2], 0.0], s.q);
                let half_h = det::mul(0.5, h);
                s.q = normalize_q([
                    det::add(s.q[0], det::mul(half_h, spin[0])),
                    det::add(s.q[1], det::mul(half_h, spin[1])),
                    det::add(s.q[2], det::mul(half_h, spin[2])),
                    det::add(s.q[3], det::mul(half_h, spin[3])),
                ]);
                self.integrated[i] = (s.x, s.q);
            }
            // Pre-solve normal velocities for restitution.
            for m in manifolds.iter_mut() {
                for k in 0..m.count {
                    let (_, ra, _, rb) = self.points_of(m, k);
                    let rel = self.relative_velocity(m, ra, rb, true);
                    m.points[k].vn_prev = dot(rel, m.normal);
                    m.points[k].lambda_n = 0.0;
                }
            }
            // Positions, colour by colour.
            for color in 0..self.colorer.layer_count() {
                for idx in 0..self.colorer.layer(color).len() {
                    let mi = self.colorer.layer(color)[idx] as usize;
                    let mut m = manifolds[mi];
                    self.project_manifold(&mut m);
                    manifolds[mi] = m;
                }
            }
            // Velocities from positions. Only the projection's correction
            // `Δ = after − integrated` changes the velocities (`v += Δx/h`,
            // `ω += 2·Δq.xyz/(h·Δq.w)` with `Δq = q·q̄_integrated`), so a body
            // nothing touched keeps its velocities bit for bit.
            for (i, s) in self.bodies.iter_mut().enumerate() {
                if s.inv_mass == 0.0 {
                    continue;
                }
                let (ix, iq) = self.integrated[i];
                if s.x != ix {
                    s.v = add(s.v, scale(sub(s.x, ix), inv_h));
                }
                if s.q != iq {
                    s.q = normalize_q(s.q);
                    let dq = qmul(s.q, conj(iq));
                    let dq = if dq[3] >= 0.0 { dq } else { scale4(dq, -1.0) };
                    let denom = if dq[3] > 0.5 { det::mul(h, dq[3]) } else { h };
                    s.w = add(s.w, scale([dq[0], dq[1], dq[2]], det::div(2.0, denom)));
                }
            }
            // Velocity pass, colour by colour.
            for color in 0..self.colorer.layer_count() {
                for idx in 0..self.colorer.layer(color).len() {
                    let mi = self.colorer.layer(color)[idx] as usize;
                    let m = manifolds[mi];
                    self.velocity_manifold(&m, h, bounce_floor);
                }
            }
        }
        self.manifolds = manifolds;
    }

    /// Centroid of the manifold's anchors: arms and world points of the
    /// patch's centre now and at the start of the substep.
    fn centroid(&self, m: &ContactManifold) -> (V3, V3, V3, V3) {
        let inv = det::div(1.0, m.count as f32);
        let mut la = [0.0f32; 3];
        let mut lb = [0.0f32; 3];
        for p in &m.points[..m.count] {
            la = add(la, p.local_a);
            lb = add(lb, p.local_b);
        }
        let (la, lb) = (scale(la, inv), scale(lb, inv));
        let sa = &self.bodies[m.a as usize];
        let (xa, qa) = self.prev[m.a as usize];
        // Motion of the material point now at the contact (for a sphere,
        // the bottom point, not the centre).
        let ra = sub(rotate(sa.q, la), scale(m.normal, m.radius_a));
        let moved_a = sub(add(sa.x, ra), add(xa, rotate(qa, rotate(conj(sa.q), ra))));
        let (rb, moved_b) = match m.b {
            Some(b) => {
                let sb = &self.bodies[b as usize];
                let (xb, qb) = self.prev[b as usize];
                let rb = add(rotate(sb.q, lb), scale(m.normal, m.radius_b));
                let moved = sub(add(sb.x, rb), add(xb, rotate(qb, rotate(conj(sb.q), rb))));
                (rb, moved)
            }
            None => ([0.0; 3], [0.0; 3]),
        };
        (ra, rb, moved_a, moved_b)
    }

    /// Normal projection of every point of `m`, then positional static
    /// friction on the whole patch at its centroid: the patch's tangential
    /// motion this substep is undone while the needed tangential impulse is
    /// below `μ_s ×` the patch's normal impulse.
    fn project_manifold(&mut self, m: &mut ContactManifold) {
        let n = m.normal;
        let mut dl_sum = 0.0f32;
        for k in 0..m.count {
            let (pa, ra, pb, rb) = self.points_of(m, k);
            let depth = dot(sub(pb, pa), n);
            if !(depth > 0.0) {
                continue;
            }
            let w = self.weight(m, ra, rb, n);
            if !(w > 0.0) {
                continue;
            }
            let dl = det::div(depth, w);
            m.points[k].lambda_n = det::add(m.points[k].lambda_n, dl);
            dl_sum = det::add(dl_sum, dl);
            self.apply_positional(m, ra, rb, scale(n, dl));
        }
        if m.material.static_friction > 0.0 && dl_sum > 0.0 {
            let (ra, rb, moved_a, moved_b) = self.centroid(m);
            let dp = sub(moved_a, moved_b);
            let dt_vec = sub(dp, scale(n, dot(dp, n)));
            let lt = length(dt_vec);
            if lt > 1.0e-9 {
                let t = scale(dt_vec, det::div(1.0, lt));
                let wt = self.weight(m, ra, rb, t);
                if wt > 0.0 {
                    let dlt = det::div(lt, wt);
                    if dlt < det::mul(m.material.static_friction, dl_sum) {
                        self.apply_positional(m, ra, rb, scale(t, det::neg(dlt)));
                    }
                }
            }
        }
    }

    /// Velocity pass of `m`: per point, the normal velocity becomes the
    /// restitution of the pre-substep approach speed (zero below
    /// `2·|g|₁·h`); then dynamic friction and rolling friction act on the
    /// patch, with impulses bounded by `μ_d` and `μ_r` times the patch's
    /// normal impulse `Σλ_n / h`.
    fn velocity_manifold(&mut self, m: &ContactManifold, h: f32, bounce_floor: f32) {
        let n = m.normal;
        let mut lambda_sum = 0.0f32;
        for k in 0..m.count {
            let lambda_n = m.points[k].lambda_n;
            if !(lambda_n > 0.0) {
                continue;
            }
            lambda_sum = det::add(lambda_sum, lambda_n);
            let (_, ra, _, rb) = self.points_of(m, k);
            let vn = dot(self.relative_velocity(m, ra, rb, false), n);
            let vn_prev = m.points[k].vn_prev;
            let e = if vn_prev.abs() <= bounce_floor {
                0.0
            } else {
                m.material.restitution
            };
            let wanted = det::max(det::neg(det::mul(e, vn_prev)), 0.0);
            let dvn = det::sub(wanted, vn);
            if dvn == 0.0 {
                continue;
            }
            let w = self.weight(m, ra, rb, n);
            if w > 0.0 {
                self.apply_impulse(m, ra, rb, scale(n, det::div(dvn, w)));
            }
        }
        if !(lambda_sum > 0.0) {
            return;
        }
        let normal_impulse = det::div(lambda_sum, h);
        if m.material.dynamic_friction > 0.0 {
            let (ra, rb, _, _) = self.centroid(m);
            let rel = self.relative_velocity(m, ra, rb, false);
            let vt = sub(rel, scale(n, dot(rel, n)));
            let lvt = length(vt);
            if lvt > 1.0e-9 {
                let t = scale(vt, det::div(1.0, lvt));
                let wt = self.weight(m, ra, rb, t);
                if wt > 0.0 {
                    let stop = det::div(lvt, wt);
                    let limit = det::mul(m.material.dynamic_friction, normal_impulse);
                    let j = if limit < stop { limit } else { stop };
                    self.apply_impulse(m, ra, rb, scale(t, det::neg(j)));
                }
            }
        }
        // Torsional friction: the patch's spin about the normal is opposed
        // by dynamic friction acting at the patch's mean radius, so the
        // angular impulse is at most `μ_d · Σλ_n / h · r_patch`. A
        // one-point patch (a sphere) has no torsional friction.
        if m.material.dynamic_friction > 0.0 && m.count > 1 {
            let mut c = [0.0f32; 3];
            for k in 0..m.count {
                c = add(c, self.points_of(m, k).0);
            }
            let c = scale(c, det::div(1.0, m.count as f32));
            let mut radius = 0.0f32;
            for k in 0..m.count {
                let d = sub(self.points_of(m, k).0, c);
                radius = det::add(radius, length(sub(d, scale(n, dot(d, n)))));
            }
            let radius = det::div(radius, m.count as f32);
            let bound = det::mul(
                det::mul(m.material.dynamic_friction, normal_impulse),
                radius,
            );
            self.angular_friction(m, n, bound);
        }
        // Rolling friction: oppose the relative angular velocity
        // perpendicular to the normal, with angular impulse at most
        // `μ_r · Σλ_n / h`.
        if m.material.rolling_friction > 0.0 {
            let bound = det::mul(m.material.rolling_friction, normal_impulse);
            let wa = self.bodies[m.a as usize].w;
            let wb = m.b.map_or([0.0; 3], |b| self.bodies[b as usize].w);
            let rel = sub(wa, wb);
            let roll = sub(rel, scale(n, dot(rel, n)));
            let lr = length(roll);
            if lr > 1.0e-9 {
                self.angular_friction(m, scale(roll, det::div(1.0, lr)), bound);
            }
        }
    }

    /// Opposes the relative angular velocity of `m`'s bodies about the unit
    /// `axis` with an angular impulse of at most `bound`.
    fn angular_friction(&mut self, m: &ContactManifold, axis: V3, bound: f32) {
        let wa = self.bodies[m.a as usize].w;
        let wb = m.b.map_or([0.0; 3], |b| self.bodies[b as usize].w);
        let rel = dot(sub(wa, wb), axis);
        if rel == 0.0 || !(bound > 0.0) {
            return;
        }
        let mut wsum = angular_weight(&self.bodies[m.a as usize], axis);
        if let Some(b) = m.b {
            wsum = det::add(wsum, angular_weight(&self.bodies[b as usize], axis));
        }
        if !(wsum > 0.0) {
            return;
        }
        let stop = det::div(rel.abs(), wsum);
        let j = if stop < bound { stop } else { bound };
        let ang = scale(axis, if rel > 0.0 { det::neg(j) } else { j });
        let a = &mut self.bodies[m.a as usize];
        if a.inv_mass > 0.0 {
            a.w = add(a.w, inv_inertia_world(a, ang));
        }
        if let Some(b) = m.b {
            let b = &mut self.bodies[b as usize];
            if b.inv_mass > 0.0 {
                b.w = sub(b.w, inv_inertia_world(b, ang));
            }
        }
    }
}
