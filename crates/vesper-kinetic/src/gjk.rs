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

// The port-backed build forwards `separation`, `distance` and `intersects`
// to the Vesper port (`vesper/src/gjk.ves`); the frozen Rust engine lives in
// `seed/src/engine/gjk.rs` for the compiler's closure (SH6 lane 9, #1404).

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

pub(crate) type V = [f64; 3];

#[inline]
pub(crate) fn add(a: V, b: V) -> V {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
#[inline]
pub(crate) fn scale(a: V, s: f64) -> V {
    [a[0] * s, a[1] * s, a[2] * s]
}
#[inline]
pub(crate) fn dot(a: V, b: V) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
#[inline]
pub(crate) fn norm(a: V) -> f64 {
    dot(a, a).sqrt()
}

impl Shape<'_> {
    pub(crate) fn radius(&self) -> f64 {
        match self {
            Shape::Sphere { radius, .. } | Shape::Capsule { radius, .. } => *radius,
            Shape::Polytope { .. } => 0.0,
        }
    }

    /// Support point of the core in direction `d` (first maximiser on
    /// ties, so the choice is deterministic).
    pub(crate) fn core_support(&self, d: V) -> V {
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
}

/// Distance between `a` and `b` (GJK), or penetration depth (EPA) as a
/// negative distance, with the contact normal and witness points.
pub fn separation(a: &Shape, b: &Shape) -> Result<Separation, GjkError> {
    #[cfg(vesper_port)]
    {
        separation_port(a, b)
    }
    #[cfg(not(vesper_port))]
    {
        crate::engine::gjk::separation(a, b)
    }
}

/// A shape as the port lends it: kind and numbers (see the port's module
/// gjk). Spheres and capsules use `buf`; a polytope lends its vertices.
#[cfg(vesper_port)]
fn port_shape<'a>(s: &'a Shape, buf: &'a mut [f64; 7]) -> (u32, &'a [f64]) {
    match *s {
        Shape::Sphere { center, radius } => {
            buf[..3].copy_from_slice(&center);
            buf[3] = radius;
            (0, &buf[..4])
        }
        Shape::Capsule { a, b, radius } => {
            buf[..3].copy_from_slice(&a);
            buf[3..6].copy_from_slice(&b);
            buf[6] = radius;
            (1, &buf[..])
        }
        Shape::Polytope { vertices } => (2, vertices.as_flattened()),
    }
}

#[cfg(vesper_port)]
fn separation_port(a: &Shape, b: &Shape) -> Result<Separation, GjkError> {
    let (mut ba, mut bb) = ([0.0; 7], [0.0; 7]);
    let (ka, da) = port_shape(a, &mut ba);
    let (kb, db) = port_shape(b, &mut bb);
    let mut out = [0.0; 10];
    crate::port::gjk_separation(ka, da, kb, db, &mut out)?;
    Ok(Separation {
        distance: out[0],
        normal: [out[1], out[2], out[3]],
        point_a: [out[4], out[5], out[6]],
        point_b: [out[7], out[8], out[9]],
    })
}

/// Distance only, `0` for touching or overlapping shapes.
pub fn distance(a: &Shape, b: &Shape) -> Result<f64, GjkError> {
    Ok(separation(a, b)?.distance.max(0.0))
}

/// Whether the shapes overlap (or touch).
pub fn intersects(a: &Shape, b: &Shape) -> Result<bool, GjkError> {
    Ok(separation(a, b)?.distance <= 0.0)
}
