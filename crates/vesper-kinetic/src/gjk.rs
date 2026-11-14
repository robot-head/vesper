//! GJK distance and EPA penetration for convex shapes (§36.3 narrowphase).
//!
//! Shapes are a core (point, segment or convex vertex set) swept by a
//! radius: spheres, capsules and convex polytopes. GJK runs on the cores, so
//! separated rounded shapes get exact distances (core distance minus radii)
//! without the slow convergence of curved supports. When the cores
//! intersect, EPA runs on the cores' (polyhedral) Minkowski difference and
//! the radii are added to its depth, so EPA terminates exactly too.
//!
//! The closest point of each GJK simplex is found by checking every face of
//! the simplex (Johnson's sub-simplex enumeration solved directly), which is
//! simple and robust at this size. All arithmetic is plain binary64 with no
//! fused multiply-add, so results are the same on every IEEE-754 target.

// `!(x > y)` is deliberate: it is also true when `x` is NaN.
#![allow(clippy::neg_cmp_op_on_partial_ord)]

/// A convex shape.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Shape<'a> {
    Sphere {
        center: [f64; 3],
        radius: f64,
    },
    /// The segment `a`–`b` swept by `radius`.
    Capsule {
        a: [f64; 3],
        b: [f64; 3],
        radius: f64,
    },
    /// The convex hull of `vertices` (at least one).
    Polytope {
        vertices: &'a [[f64; 3]],
    },
}

/// Typed narrowphase failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GjkError {
    /// A coordinate or radius is non-finite, a radius is negative, or a
    /// polytope has no vertices.
    InvalidShape,
    /// GJK or EPA did not converge within its iteration budget.
    NoConvergence,
    /// The shapes overlap but their Minkowski difference is flat (zero
    /// volume), so there is no unique penetration direction.
    Degenerate,
}

impl std::fmt::Display for GjkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidShape => write!(f, "invalid convex shape"),
            Self::NoConvergence => write!(f, "GJK/EPA did not converge"),
            Self::Degenerate => write!(f, "flat Minkowski difference"),
        }
    }
}

impl std::error::Error for GjkError {}

/// Result of a narrowphase query. `distance < 0` is a penetration of depth
/// `-distance`. `normal` is the unit direction from `a` toward `b`: moving
/// `b` by `-distance · normal` (or `a` by the opposite) just separates them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Separation {
    pub distance: f64,
    pub normal: [f64; 3],
    /// Witness point on `a` (closest, or deepest for a penetration).
    pub point_a: [f64; 3],
    /// Witness point on `b`.
    pub point_b: [f64; 3],
}

type V = [f64; 3];

#[inline]
fn add(a: V, b: V) -> V {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
#[inline]
fn sub(a: V, b: V) -> V {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
#[inline]
fn scale(a: V, s: f64) -> V {
    [a[0] * s, a[1] * s, a[2] * s]
}
#[inline]
fn dot(a: V, b: V) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
#[inline]
fn cross(a: V, b: V) -> V {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
#[inline]
fn norm(a: V) -> f64 {
    dot(a, a).sqrt()
}

impl Shape<'_> {
    fn validate(&self) -> Result<(), GjkError> {
        let finite = |v: &V| v.iter().all(|x| x.is_finite());
        let ok = match self {
            Shape::Sphere { center, radius } => {
                finite(center) && radius.is_finite() && *radius >= 0.0
            }
            Shape::Capsule { a, b, radius } => {
                finite(a) && finite(b) && radius.is_finite() && *radius >= 0.0
            }
            Shape::Polytope { vertices } => !vertices.is_empty() && vertices.iter().all(finite),
        };
        if ok {
            Ok(())
        } else {
            Err(GjkError::InvalidShape)
        }
    }

    fn radius(&self) -> f64 {
        match self {
            Shape::Sphere { radius, .. } | Shape::Capsule { radius, .. } => *radius,
            Shape::Polytope { .. } => 0.0,
        }
    }

    /// Support point of the core in direction `d` (first maximiser on
    /// ties, so the choice is deterministic).
    fn core_support(&self, d: V) -> V {
        match self {
            Shape::Sphere { center, .. } => *center,
            Shape::Capsule { a, b, .. } => {
                if dot(*b, d) > dot(*a, d) {
                    *b
                } else {
                    *a
                }
            }
            Shape::Polytope { vertices } => {
                let mut best = vertices[0];
                let mut best_dot = dot(best, d);
                for v in &vertices[1..] {
                    let s = dot(*v, d);
                    if s > best_dot {
                        best = *v;
                        best_dot = s;
                    }
                }
                best
            }
        }
    }

    /// Support point of the full (rounded) shape in direction `d`.
    pub fn support(&self, d: V) -> V {
        let c = self.core_support(d);
        let r = self.radius();
        let n = norm(d);
        if r == 0.0 || n == 0.0 {
            c
        } else {
            add(c, scale(d, r / n))
        }
    }

    /// Any point of the core (for GJK's first direction).
    fn some_point(&self) -> V {
        match self {
            Shape::Sphere { center, .. } => *center,
            Shape::Capsule { a, .. } => *a,
            Shape::Polytope { vertices } => vertices[0],
        }
    }
}

/// One Minkowski-difference vertex with its source points.
#[derive(Debug, Clone, Copy)]
struct SupportPoint {
    w: V,
    a: V,
    b: V,
}

fn core_support_point(sa: &Shape, sb: &Shape, d: V) -> SupportPoint {
    let a = sa.core_support(d);
    let b = sb.core_support(scale(d, -1.0));
    SupportPoint { w: sub(a, b), a, b }
}

/// Closest point to the origin of the affine hull of `pts` (indices
/// `subset`), if it lies strictly inside that face: barycentric weights.
fn affine_closest(pts: &[SupportPoint], subset: &[usize]) -> Option<[f64; 4]> {
    let k = subset.len();
    let y0 = pts[subset[0]].w;
    let mut lambda = [0.0; 4];
    if k == 1 {
        lambda[0] = 1.0;
        return Some(lambda);
    }
    // Solve G μ = r, G_ij = e_i·e_j, r_i = −e_i·y0, e_i = y_i − y0.
    let e: Vec<V> = subset[1..].iter().map(|&i| sub(pts[i].w, y0)).collect();
    let m = k - 1;
    let mut g = [[0.0; 4]; 3];
    for i in 0..m {
        for j in 0..m {
            g[i][j] = dot(e[i], e[j]);
        }
        g[i][3] = -dot(e[i], y0);
    }
    // Gaussian elimination with partial pivoting.
    let scale_ref = (0..m).map(|i| g[i][i]).fold(0.0f64, f64::max);
    for col in 0..m {
        let mut piv = col;
        for row in col + 1..m {
            if g[row][col].abs() > g[piv][col].abs() {
                piv = row;
            }
        }
        if g[piv][col].abs() <= 1e-12 * scale_ref.max(f64::MIN_POSITIVE) {
            return None;
        }
        g.swap(col, piv);
        for row in 0..m {
            if row != col {
                let f = g[row][col] / g[col][col];
                let pivot_row = g[col];
                for (c, x) in g[row].iter_mut().enumerate().skip(col) {
                    *x -= f * pivot_row[c];
                }
            }
        }
    }
    let mut sum = 0.0;
    for i in 0..m {
        let mu = g[i][3] / g[i][i];
        if !(mu > 0.0) {
            return None;
        }
        lambda[i + 1] = mu;
        sum += mu;
    }
    lambda[0] = 1.0 - sum;
    if !(lambda[0] > 0.0) {
        return None;
    }
    Some(lambda)
}

/// Closest point of the simplex to the origin; reduces `simplex` to the
/// supporting face. Returns the point.
fn closest_on_simplex(simplex: &mut Vec<SupportPoint>, lambdas: &mut Vec<f64>) -> V {
    let n = simplex.len();
    let mut best: Option<(f64, Vec<usize>, [f64; 4])> = None;
    // Every non-empty subset, in a fixed order.
    for mask in 1u32..(1 << n) {
        let subset: Vec<usize> = (0..n).filter(|i| mask & (1 << i) != 0).collect();
        if let Some(l) = affine_closest(simplex, &subset) {
            let mut v = [0.0; 3];
            for (k, &i) in subset.iter().enumerate() {
                v = add(v, scale(simplex[i].w, l[k]));
            }
            let d2 = dot(v, v);
            if best.as_ref().is_none_or(|(b, _, _)| d2 < *b) {
                best = Some((d2, subset, l));
            }
        }
    }
    let (_, subset, l) = best.expect("a single vertex is always a valid face");
    let kept: Vec<SupportPoint> = subset.iter().map(|&i| simplex[i]).collect();
    *simplex = kept;
    lambdas.clear();
    lambdas.extend_from_slice(&l[..subset.len()]);
    let mut v = [0.0; 3];
    for (p, l) in simplex.iter().zip(lambdas.iter()) {
        v = add(v, scale(p.w, *l));
    }
    v
}

const GJK_MAX_ITER: usize = 128;

struct GjkOutcome {
    /// Distance between the cores (0 when they intersect).
    distance: f64,
    /// Closest point of the core Minkowski difference to the origin.
    v: V,
    point_a: V,
    point_b: V,
    simplex: Vec<SupportPoint>,
}

fn gjk_cores(sa: &Shape, sb: &Shape) -> Result<GjkOutcome, GjkError> {
    let start = sub(sa.some_point(), sb.some_point());
    let first = core_support_point(sa, sb, scale(start, -1.0));
    let mut simplex = vec![first];
    let mut lambdas = vec![1.0];
    let mut v = first.w;
    // Scale for the relative tolerances (no absolute floor, so tiny but
    // separated shapes keep their positive distance).
    let size = norm(first.w);
    let mut converged = false;
    let mut stalled = false;
    let mut best_vv = f64::INFINITY;
    for _ in 0..GJK_MAX_ITER {
        let vv = dot(v, v);
        if vv <= 1e-24 * size * size || simplex.len() == 4 {
            converged = true;
            break;
        }
        // No progress: rounding keeps the simplex from closing on the
        // origin (shapes touching or overlapping by a hair, often with
        // coplanar faces). Stop with the closest point found.
        if !(vv < best_vv) {
            converged = true;
            stalled = true;
            break;
        }
        best_vv = vv;
        let p = core_support_point(sa, sb, scale(v, -1.0));
        // Converged: the new support does not get closer than |v|.
        if vv - dot(v, p.w) <= 1e-12 * vv || simplex.iter().any(|s| s.w == p.w) {
            converged = true;
            break;
        }
        simplex.push(p);
        v = closest_on_simplex(&mut simplex, &mut lambdas);
    }
    if !converged {
        return Err(GjkError::NoConvergence);
    }
    let mut pa = [0.0; 3];
    let mut pb = [0.0; 3];
    for (s, l) in simplex.iter().zip(&lambdas) {
        pa = add(pa, scale(s.a, *l));
        pb = add(pb, scale(s.b, *l));
    }
    let vv = dot(v, v);
    // A full tetrahedron means the origin is inside it. A stall within
    // 1e-6 (relative) of the origin is treated as contact, so EPA measures
    // it (and reports the hair-width gap or overlap).
    let distance = if vv <= 1e-24 * size * size
        || simplex.len() == 4
        || (stalled && vv <= 1e-12 * size * size)
    {
        0.0
    } else {
        vv.sqrt()
    };
    Ok(GjkOutcome {
        distance,
        v,
        point_a: pa,
        point_b: pb,
        simplex,
    })
}

/// Distance between `a` and `b` (GJK), or penetration depth (EPA) as a
/// negative distance, with the contact normal and witness points.
pub fn separation(a: &Shape, b: &Shape) -> Result<Separation, GjkError> {
    a.validate()?;
    b.validate()?;
    let g = gjk_cores(a, b)?;
    let radii = a.radius() + b.radius();
    if g.distance > 0.0 {
        // n points from a to b: v = core_a − core_b is the closest point.
        let normal = scale(g.v, -1.0 / g.distance);
        return Ok(Separation {
            distance: g.distance - radii,
            normal,
            point_a: add(g.point_a, scale(normal, a.radius())),
            point_b: sub(g.point_b, scale(normal, b.radius())),
        });
    }
    // The cores overlap. The rounded shapes' Minkowski difference is the
    // cores' difference grown by the summed radius, so the penetration is
    // the cores' penetration plus the radii, along the same normal. EPA runs
    // on the cores, which are polyhedral, so it terminates exactly.
    match epa(a, b, &g.simplex)? {
        CorePenetration::Volume(sep) => Ok(Separation {
            distance: sep.distance - radii,
            normal: sep.normal,
            point_a: add(sep.point_a, scale(sep.normal, a.radius())),
            point_b: sub(sep.point_b, scale(sep.normal, b.radius())),
        }),
        CorePenetration::Flat(normal) => {
            if radii == 0.0 {
                return Err(GjkError::Degenerate);
            }
            Ok(Separation {
                distance: -radii,
                normal,
                point_a: add(g.point_a, scale(normal, a.radius())),
                point_b: sub(g.point_b, scale(normal, b.radius())),
            })
        }
    }
}

enum CorePenetration {
    /// EPA on a full-volume core difference.
    Volume(Separation),
    /// The core difference is flat (a point, segment or planar set) and
    /// contains the origin: any direction normal to it has zero core depth.
    Flat(V),
}

/// A unit vector normal to the flat set spanned by `dirs` (deterministic).
fn flat_normal(line: V, plane: V) -> V {
    let pn = norm(plane);
    if pn > 0.0 {
        return canonical_sign(scale(plane, 1.0 / pn));
    }
    let ln = norm(line);
    if ln > 0.0 {
        // Cross with the axis least aligned with the line.
        let l = scale(line, 1.0 / ln);
        let axis = if l[0].abs() <= l[1].abs() && l[0].abs() <= l[2].abs() {
            [1.0, 0.0, 0.0]
        } else if l[1].abs() <= l[2].abs() {
            [0.0, 1.0, 0.0]
        } else {
            [0.0, 0.0, 1.0]
        };
        let c = cross(l, axis);
        return canonical_sign(scale(c, 1.0 / norm(c)));
    }
    // A single point: separate along +y, the solver's fixed choice.
    [0.0, 1.0, 0.0]
}

fn canonical_sign(v: V) -> V {
    let first = v.iter().copied().find(|x| *x != 0.0).unwrap_or(1.0);
    if first < 0.0 { scale(v, -1.0) } else { v }
}

/// Distance only, `0` for touching or overlapping shapes.
pub fn distance(a: &Shape, b: &Shape) -> Result<f64, GjkError> {
    Ok(separation(a, b)?.distance.max(0.0))
}

/// Whether the shapes overlap (or touch).
pub fn intersects(a: &Shape, b: &Shape) -> Result<bool, GjkError> {
    Ok(separation(a, b)?.distance <= 0.0)
}

#[derive(Debug, Clone, Copy)]
struct Face {
    v: [usize; 3],
    n: V,
    dist: f64,
}

const EPA_MAX_ITER: usize = 256;
const EPA_TOL: f64 = 1e-9;

fn make_face(pts: &[SupportPoint], i: usize, j: usize, k: usize, inside: V) -> Option<Face> {
    let (a, b, c) = (pts[i].w, pts[j].w, pts[k].w);
    let n = cross(sub(b, a), sub(c, a));
    let len = norm(n);
    if !(len > 0.0) {
        return None;
    }
    let mut n = scale(n, 1.0 / len);
    let mut v = [i, j, k];
    if dot(n, sub(inside, a)) > 0.0 {
        n = scale(n, -1.0);
        v = [i, k, j];
    }
    Some(Face {
        v,
        n,
        dist: dot(n, a),
    })
}

fn epa(sa: &Shape, sb: &Shape, seed: &[SupportPoint]) -> Result<CorePenetration, GjkError> {
    // Candidate points: the GJK simplex plus supports in 14 directions.
    let mut cand: Vec<SupportPoint> = seed.to_vec();
    for d in [
        [1.0, 0.0, 0.0],
        [-1.0, 0.0, 0.0],
        [0.0, 1.0, 0.0],
        [0.0, -1.0, 0.0],
        [0.0, 0.0, 1.0],
        [0.0, 0.0, -1.0],
        [1.0, 1.0, 1.0],
        [-1.0, -1.0, -1.0],
        [1.0, -1.0, 1.0],
        [-1.0, 1.0, -1.0],
        [1.0, 1.0, -1.0],
        [-1.0, -1.0, 1.0],
        [-1.0, 1.0, 1.0],
        [1.0, -1.0, -1.0],
    ] {
        cand.push(core_support_point(sa, sb, d));
    }
    // Initial tetrahedron of large volume.
    let p0 = 0;
    let far = |from: &dyn Fn(V) -> f64, cand: &[SupportPoint]| -> usize {
        let mut best = 0;
        let mut best_v = f64::NEG_INFINITY;
        for (i, c) in cand.iter().enumerate() {
            let v = from(c.w);
            if v > best_v {
                best_v = v;
                best = i;
            }
        }
        best
    };
    let a0 = cand[p0].w;
    let p1 = far(&|w| norm(sub(w, a0)), &cand);
    let a1 = cand[p1].w;
    // The fixed directions can all pick the ends of a long thin hull, so
    // query the directions perpendicular to the line found so far (and
    // then to the plane) before concluding the difference is flat.
    let u = flat_normal(sub(a1, a0), [0.0; 3]);
    let v = cross(sub(a1, a0), u);
    for d in [u, scale(u, -1.0), v, scale(v, -1.0)] {
        cand.push(core_support_point(sa, sb, d));
    }
    let p2 = far(&|w| norm(cross(sub(w, a0), sub(a1, a0))), &cand);
    let a2 = cand[p2].w;
    let plane = cross(sub(a1, a0), sub(a2, a0));
    for d in [plane, scale(plane, -1.0)] {
        cand.push(core_support_point(sa, sb, d));
    }
    let p3 = far(&|w| dot(sub(w, a0), plane).abs(), &cand);
    let a3 = cand[p3].w;
    let extent = norm(sub(a1, a0));
    if !(extent > 1e-12) {
        return Ok(CorePenetration::Flat(flat_normal([0.0; 3], [0.0; 3])));
    }
    if !(norm(plane) > 1e-12 * extent * extent) {
        return Ok(CorePenetration::Flat(flat_normal(sub(a1, a0), [0.0; 3])));
    }
    let volume = dot(sub(a3, a0), plane).abs();
    if !(volume > 1e-12 * extent * extent * extent) {
        return Ok(CorePenetration::Flat(flat_normal(sub(a1, a0), plane)));
    }
    let mut pts = vec![cand[p0], cand[p1], cand[p2], cand[p3]];
    let inside = scale(add(add(a0, a1), add(a2, a3)), 0.25);
    let mut faces: Vec<Face> = Vec::new();
    for (i, j, k) in [(0, 1, 2), (0, 1, 3), (0, 2, 3), (1, 2, 3)] {
        faces.push(make_face(&pts, i, j, k, inside).ok_or(GjkError::Degenerate)?);
    }
    let size = extent.max(1.0);
    for _ in 0..EPA_MAX_ITER {
        // Closest face (first on ties).
        let mut best = 0;
        for (i, f) in faces.iter().enumerate() {
            if f.dist < faces[best].dist {
                best = i;
            }
        }
        let face = faces[best];
        let p = core_support_point(sa, sb, face.n);
        let gain = dot(p.w, face.n) - face.dist;
        if gain <= EPA_TOL * size {
            return Ok(CorePenetration::Volume(epa_result(&pts, &face)));
        }
        // Remove faces visible from p; keep the horizon.
        let mut horizon: Vec<[usize; 2]> = Vec::new();
        let mut kept = Vec::with_capacity(faces.len());
        for f in &faces {
            if dot(f.n, sub(p.w, pts[f.v[0]].w)) > EPA_TOL * size {
                for e in [[f.v[0], f.v[1]], [f.v[1], f.v[2]], [f.v[2], f.v[0]]] {
                    if let Some(pos) = horizon.iter().position(|h| *h == [e[1], e[0]]) {
                        horizon.swap_remove(pos);
                    } else {
                        horizon.push(e);
                    }
                }
            } else {
                kept.push(*f);
            }
        }
        if horizon.is_empty() {
            return Ok(CorePenetration::Volume(epa_result(&pts, &face)));
        }
        pts.push(p);
        let new = pts.len() - 1;
        faces = kept;
        for e in horizon {
            match make_face(&pts, e[0], e[1], new, inside) {
                Some(f) => faces.push(f),
                None => return Ok(CorePenetration::Volume(epa_result(&pts, &face))),
            }
        }
    }
    Err(GjkError::NoConvergence)
}

fn epa_result(pts: &[SupportPoint], face: &Face) -> Separation {
    // Barycentric coordinates of the origin's projection onto the face.
    let p = scale(face.n, face.dist);
    let (a, b, c) = (pts[face.v[0]], pts[face.v[1]], pts[face.v[2]]);
    let v0 = sub(b.w, a.w);
    let v1 = sub(c.w, a.w);
    let v2 = sub(p, a.w);
    let d00 = dot(v0, v0);
    let d01 = dot(v0, v1);
    let d11 = dot(v1, v1);
    let d20 = dot(v2, v0);
    let d21 = dot(v2, v1);
    let den = d00 * d11 - d01 * d01;
    let (u, v, w) = if den != 0.0 {
        let v = (d11 * d20 - d01 * d21) / den;
        let w = (d00 * d21 - d01 * d20) / den;
        (1.0 - v - w, v, w)
    } else {
        (1.0, 0.0, 0.0)
    };
    let point_a = add(add(scale(a.a, u), scale(b.a, v)), scale(c.a, w));
    let point_b = add(add(scale(a.b, u), scale(b.b, v)), scale(c.b, w));
    Separation {
        distance: -face.dist,
        normal: face.n,
        point_a,
        point_b,
    }
}
