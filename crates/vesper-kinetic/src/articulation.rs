//! Reduced-coordinate articulations (§36): Featherstone's articulated-body
//! algorithm beside the XPBD joint chains of [`crate::rigid`].
//!
//! An [`Articulation`] is a tree of links fixed to the world at its roots.
//! Each link hangs from its parent by one revolute or prismatic joint, so
//! the state is one coordinate `q` and one rate `q̇` per link and the
//! constraints hold exactly: there is no drift to project away, unlike an
//! XPBD chain.
//!
//! The dynamics are the articulated-body algorithm (Featherstone, *Rigid
//! Body Dynamics Algorithms*, 2008, §7.3, table 7.1) in spatial vector
//! algebra:
//!
//! 1. outward: link transforms `ᵢXₚ = X_J(q) X_T`, velocities
//!    `vᵢ = ᵢXₚ vₚ + Sᵢ q̇ᵢ`, bias accelerations `cᵢ = vᵢ ×ᵐ Sᵢ q̇ᵢ`, and
//!    bias forces `pᴬᵢ = vᵢ ×* Iᵢ vᵢ`;
//! 2. inward: `Uᵢ = Iᴬᵢ Sᵢ`, `dᵢ = Sᵢᵀ Uᵢ`, `uᵢ = τᵢ − Sᵢᵀ pᴬᵢ`, and the
//!    parent accumulates `ᵢXₚᵀ (Iᴬᵢ − Uᵢ Uᵢᵀ / dᵢ) ᵢXₚ` and
//!    `ᵢXₚᵀ (pᴬᵢ + Iᵃᵢ cᵢ + Uᵢ uᵢ / dᵢ)`;
//! 3. outward: `q̈ᵢ = (uᵢ − Uᵢᵀ a′ᵢ) / dᵢ` with `a′ᵢ = ᵢXₚ aₚ + cᵢ`, where the
//!    world's acceleration is `−g` (gravity as a base acceleration).
//!
//! Integration is semi-implicit Euler per substep (`q̇ += h q̈`, then
//! `q += h q̇`).
//!
//! **Drives, damping and friction.** A joint drive is a PD controller
//! `τ = k (q* − q) + c (q̇* − q̇)`. It and the passive joint damping are
//! evaluated *implicitly* at the end of the substep: with semi-implicit
//! Euler this adds `k h² + c h` to `dᵢ` and leaves the explicit part in
//! `τᵢ`, so stiff position servos and hard velocity motors stay stable at
//! any gain. When the drive torque exceeds the drive's `max_effort`, either
//! explicitly or as solved (the implicit part `−(k h² + c h) q̈` included,
//! checked after the pass, which is then redone), it is clamped and applied
//! explicitly (a saturated motor).
//! Coulomb joint friction uses two passes: a frictionless pass predicts
//! `q̇ᵢ + h q̈ᵢ`, the torque that would stop the joint against its
//! articulated inertia `dᵢ` is applied when it is within `±friction`
//! (sticking) and `∓friction` otherwise (sliding), then a second pass
//! computes the accelerations. The stick torque uses each joint's own
//! articulated inertia, so for several sticking joints at once it is an
//! approximation (it is exact for one joint, see `tests/articulation.rs`).
//!
//! All arithmetic is binary64 with single IEEE-754 operations in a fixed
//! order (no fused multiply-add), and `sin`/`cos` come from
//! [`crate::fastmath`], so the state evolves to the same bits on every
//! target. Steps do not allocate after construction.
//!
//! Not here: floating bases, kinematic loops, joint limits, and collision
//! of articulation links (the links are not bodies of a
//! [`crate::RigidWorld`]).

use crate::fastmath;
use sha2::Digest;

type V = [f64; 3];
type M3 = [[f64; 3]; 3];
type Sv = [f64; 6];
type M6 = [[f64; 6]; 6];

/// Most links in one articulation.
pub const MAX_ARTICULATION_LINKS: usize = 4096;

/// A one-degree-of-freedom joint. The axis is in the parent's frame at
/// `q = 0` (equivalently, in the link's frame: the joint axis does not move
/// with its own coordinate).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ArticulationJoint {
    /// Rotation by `q` radians about `axis` (right-handed).
    Revolute { axis: V },
    /// Translation by `q` metres along `axis`.
    Prismatic { axis: V },
}

/// A PD joint drive: `τ = stiffness (target_position − q) + damping
/// (target_velocity − q̇)`, clamped to `±max_effort`. A velocity motor has
/// `stiffness = 0`; a position servo usually has both.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ArticulationDrive {
    pub target_position: f64,
    pub target_velocity: f64,
    /// N·m/rad (revolute) or N/m (prismatic), `>= 0`.
    pub stiffness: f64,
    /// N·m·s/rad or N·s/m, `>= 0`.
    pub damping: f64,
    /// Largest drive torque or force magnitude, `> 0` (may be infinite).
    pub max_effort: f64,
}

/// One link and the joint to its parent.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ArticulationLink {
    /// Index of the parent link, which must come earlier; `None` hangs the
    /// link from the world.
    pub parent: Option<usize>,
    pub joint: ArticulationJoint,
    /// Joint origin in the parent's frame (world frame for a root). The
    /// link's frame has its origin at the joint.
    pub joint_offset: V,
    /// Mass in kilograms, `> 0`.
    pub mass: f64,
    /// Centre of mass in the link's frame.
    pub com: V,
    /// Rotational inertia about the centre of mass in the link's frame
    /// (symmetric, positive semidefinite).
    pub inertia: M3,
    /// Initial joint coordinate and rate.
    pub q: f64,
    pub qd: f64,
    /// Passive viscous joint damping (`τ = −damping q̇`), `>= 0`.
    pub damping: f64,
    /// Coulomb joint friction torque or force magnitude, `>= 0`.
    pub friction: f64,
    pub drive: Option<ArticulationDrive>,
}

impl ArticulationLink {
    /// A link with no damping, friction or drive, at rest at `q = 0`.
    pub fn new(
        parent: Option<usize>,
        joint: ArticulationJoint,
        joint_offset: V,
        mass: f64,
        com: V,
        inertia: M3,
    ) -> Self {
        Self {
            parent,
            joint,
            joint_offset,
            mass,
            com,
            inertia,
            q: 0.0,
            qd: 0.0,
            damping: 0.0,
            friction: 0.0,
            drive: None,
        }
    }
}

/// Articulation parameters.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ArticulationConfig {
    pub dt: f64,
    pub substeps: u32,
    pub gravity: V,
}

impl Default for ArticulationConfig {
    fn default() -> Self {
        Self {
            dt: 1.0 / 120.0,
            substeps: 8,
            gravity: [0.0, -9.81, 0.0],
        }
    }
}

/// Typed articulation failures. A failed step leaves the state unchanged.
#[derive(Debug, Clone, PartialEq)]
pub enum ArticulationError {
    InvalidConfig(&'static str),
    InvalidLink {
        link: usize,
        reason: &'static str,
    },
    TooManyLinks {
        links: usize,
    },
    UnknownLink {
        link: usize,
    },
    /// A joint's articulated inertia `dᵢ` is not positive (a massless
    /// subtree on that joint).
    Singular {
        link: usize,
    },
    /// The state left the finite range.
    NonFinite {
        link: usize,
    },
    TickOverflow,
}

impl std::fmt::Display for ArticulationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidConfig(w) => write!(f, "invalid articulation configuration: {w}"),
            Self::InvalidLink { link, reason } => write!(f, "link {link}: {reason}"),
            Self::TooManyLinks { links } => write!(
                f,
                "{links} links exceed the limit of {MAX_ARTICULATION_LINKS}"
            ),
            Self::UnknownLink { link } => write!(f, "no link {link}"),
            Self::Singular { link } => {
                write!(
                    f,
                    "joint {link} has no articulated inertia (massless subtree)"
                )
            }
            Self::NonFinite { link } => write!(f, "link {link} left the finite range"),
            Self::TickOverflow => write!(f, "tick counter overflow"),
        }
    }
}

impl std::error::Error for ArticulationError {}

// ----- 3-vectors and 3x3 matrices (binary64, fixed order) --------------------

#[inline]
fn add3(a: V, b: V) -> V {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

#[inline]
fn sub3(a: V, b: V) -> V {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

#[inline]
fn scale3(a: V, s: f64) -> V {
    [a[0] * s, a[1] * s, a[2] * s]
}

#[inline]
fn dot3(a: V, b: V) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

#[inline]
fn cross3(a: V, b: V) -> V {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

#[inline]
fn mv3(m: &M3, v: V) -> V {
    [
        m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
        m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
        m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
    ]
}

/// `mᵀ v`.
#[inline]
fn mtv3(m: &M3, v: V) -> V {
    [
        m[0][0] * v[0] + m[1][0] * v[1] + m[2][0] * v[2],
        m[0][1] * v[0] + m[1][1] * v[1] + m[2][1] * v[2],
        m[0][2] * v[0] + m[1][2] * v[1] + m[2][2] * v[2],
    ]
}

fn mm3(a: &M3, b: &M3) -> M3 {
    let mut out = [[0.0; 3]; 3];
    for (i, row) in out.iter_mut().enumerate() {
        for (j, cell) in row.iter_mut().enumerate() {
            *cell = a[i][0] * b[0][j] + a[i][1] * b[1][j] + a[i][2] * b[2][j];
        }
    }
    out
}

fn transpose3(a: &M3) -> M3 {
    let mut out = [[0.0; 3]; 3];
    for (i, row) in a.iter().enumerate() {
        for (j, v) in row.iter().enumerate() {
            out[j][i] = *v;
        }
    }
    out
}

const I3: M3 = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

fn skew(v: V) -> M3 {
    [[0.0, -v[2], v[1]], [v[2], 0.0, -v[0]], [-v[1], v[0], 0.0]]
}

// ----- spatial algebra -------------------------------------------------------

/// A Plücker transform from frame A to frame B: B's axes are `e` times A's
/// coordinates and B's origin is at `r` in A's coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Xform {
    e: M3,
    r: V,
}

impl Xform {
    /// Motion vector `[ω; v]` from A to B: `[E ω; E (v − r × ω)]`.
    #[inline]
    fn motion(&self, m: Sv) -> Sv {
        let w = [m[0], m[1], m[2]];
        let v = [m[3], m[4], m[5]];
        let ew = mv3(&self.e, w);
        let ev = mv3(&self.e, sub3(v, cross3(self.r, w)));
        [ew[0], ew[1], ew[2], ev[0], ev[1], ev[2]]
    }

    /// Force vector `[n; f]` from B back to A (`Xᵀ` applied to a force in
    /// B): `[Eᵀ n + r × Eᵀ f; Eᵀ f]`.
    #[inline]
    fn force_back(&self, f: Sv) -> Sv {
        let n = mtv3(&self.e, [f[0], f[1], f[2]]);
        let ff = mtv3(&self.e, [f[3], f[4], f[5]]);
        let n = add3(n, cross3(self.r, ff));
        [n[0], n[1], n[2], ff[0], ff[1], ff[2]]
    }

    /// The 6×6 motion matrix `[[E, 0], [−E r×, E]]`.
    fn matrix(&self) -> M6 {
        let erx = mm3(&self.e, &skew(self.r));
        let mut out = [[0.0; 6]; 6];
        for i in 0..3 {
            for j in 0..3 {
                out[i][j] = self.e[i][j];
                out[i + 3][j + 3] = self.e[i][j];
                out[i + 3][j] = -erx[i][j];
            }
        }
        out
    }
}

/// `v ×ᵐ m` (spatial motion cross product).
#[inline]
fn crm(v: Sv, m: Sv) -> Sv {
    let w = [v[0], v[1], v[2]];
    let vl = [v[3], v[4], v[5]];
    let mw = [m[0], m[1], m[2]];
    let mv = [m[3], m[4], m[5]];
    let a = cross3(w, mw);
    let b = add3(cross3(w, mv), cross3(vl, mw));
    [a[0], a[1], a[2], b[0], b[1], b[2]]
}

/// `v ×* f` (spatial force cross product).
#[inline]
fn crf(v: Sv, f: Sv) -> Sv {
    let w = [v[0], v[1], v[2]];
    let vl = [v[3], v[4], v[5]];
    let n = [f[0], f[1], f[2]];
    let ff = [f[3], f[4], f[5]];
    let a = add3(cross3(w, n), cross3(vl, ff));
    let b = cross3(w, ff);
    [a[0], a[1], a[2], b[0], b[1], b[2]]
}

#[inline]
fn mv6(m: &M6, v: Sv) -> Sv {
    let mut out = [0.0; 6];
    for (o, row) in out.iter_mut().zip(m) {
        let mut s = 0.0;
        for k in 0..6 {
            s += row[k] * v[k];
        }
        *o = s;
    }
    out
}

#[inline]
fn dot6(a: Sv, b: Sv) -> f64 {
    let mut s = 0.0;
    for k in 0..6 {
        s += a[k] * b[k];
    }
    s
}

/// `Xᵀ M X` for the 6×6 motion matrix `X`.
fn congruence(x: &M6, m: &M6) -> M6 {
    let mut mx = [[0.0; 6]; 6];
    for i in 0..6 {
        for j in 0..6 {
            let mut s = 0.0;
            for k in 0..6 {
                s += m[i][k] * x[k][j];
            }
            mx[i][j] = s;
        }
    }
    let mut out = [[0.0; 6]; 6];
    for i in 0..6 {
        for j in 0..6 {
            let mut s = 0.0;
            for k in 0..6 {
                s += x[k][i] * mx[k][j];
            }
            out[i][j] = s;
        }
    }
    out
}

/// Spatial inertia about the link origin: `[[I_c + m c× c×ᵀ, m c×],
/// [m c×ᵀ, m 1]]`.
fn spatial_inertia(mass: f64, com: V, ic: &M3) -> M6 {
    let cx = skew(com);
    let cxt = transpose3(&cx);
    let ccx = mm3(&cx, &cxt);
    let mut out = [[0.0; 6]; 6];
    for i in 0..3 {
        for j in 0..3 {
            out[i][j] = ic[i][j] + mass * ccx[i][j];
            out[i][j + 3] = mass * cx[i][j];
            out[i + 3][j] = mass * cxt[i][j];
        }
        out[i + 3][i + 3] = mass;
    }
    out
}

/// Rotation matrix of `angle` about the unit `axis` (Rodrigues).
fn rotation(axis: V, angle: f64) -> M3 {
    let (s, c) = (fastmath::sin(angle), fastmath::cos(angle));
    let k = skew(axis);
    let k2 = mm3(&k, &k);
    let mut out = I3;
    for i in 0..3 {
        for j in 0..3 {
            out[i][j] = out[i][j] + s * k[i][j] + (1.0 - c) * k2[i][j];
        }
    }
    out
}

#[derive(Debug, Clone, Copy)]
struct Link {
    parent: Option<usize>,
    revolute: bool,
    axis: V,
    offset: V,
    mass: f64,
    com: V,
    inertia: M6,
    damping: f64,
    friction: f64,
    drive: Option<ArticulationDrive>,
    /// Motion subspace `S` in the link frame.
    s: Sv,
}

/// Per-link scratch of one ABA pass.
#[derive(Debug, Clone, Copy)]
struct Scratch {
    x: Xform,
    xm: M6,
    v: Sv,
    c: Sv,
    ia: M6,
    pa: Sv,
    u_vec: Sv,
    d: f64,
    u: f64,
    a: Sv,
}

const ZERO_SCRATCH: Scratch = Scratch {
    x: Xform { e: I3, r: [0.0; 3] },
    xm: [[0.0; 6]; 6],
    v: [0.0; 6],
    c: [0.0; 6],
    ia: [[0.0; 6]; 6],
    pa: [0.0; 6],
    u_vec: [0.0; 6],
    d: 0.0,
    u: 0.0,
    a: [0.0; 6],
};

/// A fixed-base tree of links with one-degree-of-freedom joints, simulated
/// in reduced coordinates by the articulated-body algorithm.
#[derive(Debug, Clone)]
pub struct Articulation {
    config: ArticulationConfig,
    links: Vec<Link>,
    q: Vec<f64>,
    qd: Vec<f64>,
    qdd: Vec<f64>,
    /// Held external joint torques (forces), added every substep.
    tau: Vec<f64>,
    /// Joint friction torques of the last substep.
    friction_tau: Vec<f64>,
    /// Per link, this substep: the explicit torque of a drive whose solved
    /// effort exceeded `max_effort` (`±max_effort`), or 0 for none.
    drive_clamp: Vec<f64>,
    scratch: Vec<Scratch>,
    saved: Vec<(f64, f64)>,
    tick: u64,
}

fn finite_v(v: V) -> bool {
    v.iter().all(|x| x.is_finite())
}

fn unit(v: V) -> Option<V> {
    let l = dot3(v, v).sqrt();
    (l.is_finite() && l > 1e-12).then(|| scale3(v, 1.0 / l))
}

fn validate_drive(d: &ArticulationDrive) -> bool {
    d.target_position.is_finite()
        && d.target_velocity.is_finite()
        && d.stiffness.is_finite()
        && d.stiffness >= 0.0
        && d.damping.is_finite()
        && d.damping >= 0.0
        && !d.max_effort.is_nan()
        && d.max_effort > 0.0
}

impl Articulation {
    /// Validate the links and build the articulation.
    pub fn new(
        config: ArticulationConfig,
        links: &[ArticulationLink],
    ) -> Result<Self, ArticulationError> {
        if !(config.dt.is_finite() && config.dt > 0.0) {
            return Err(ArticulationError::InvalidConfig(
                "dt must be finite and positive",
            ));
        }
        if config.substeps == 0 || config.substeps > 64 {
            return Err(ArticulationError::InvalidConfig(
                "substeps must be in 1..=64",
            ));
        }
        if !finite_v(config.gravity) {
            return Err(ArticulationError::InvalidConfig("gravity must be finite"));
        }
        if links.len() > MAX_ARTICULATION_LINKS {
            return Err(ArticulationError::TooManyLinks { links: links.len() });
        }
        let mut out = Vec::with_capacity(links.len());
        for (i, l) in links.iter().enumerate() {
            let bad = |reason| ArticulationError::InvalidLink { link: i, reason };
            if l.parent.is_some_and(|p| p >= i) {
                return Err(bad("parent must be an earlier link"));
            }
            let (revolute, axis) = match l.joint {
                ArticulationJoint::Revolute { axis } => (true, axis),
                ArticulationJoint::Prismatic { axis } => (false, axis),
            };
            let Some(axis) = unit(axis) else {
                return Err(bad("joint axis must be finite and non-zero"));
            };
            if !(l.mass.is_finite() && l.mass > 0.0) {
                return Err(bad("mass must be finite and positive"));
            }
            if !(finite_v(l.joint_offset) && finite_v(l.com)) {
                return Err(bad("joint offset and centre of mass must be finite"));
            }
            let ic = &l.inertia;
            if !ic.iter().all(|r| finite_v(*r)) {
                return Err(bad("inertia must be finite"));
            }
            for a in 0..3 {
                for b in 0..3 {
                    if ic[a][b] != ic[b][a] {
                        return Err(bad("inertia must be symmetric"));
                    }
                }
                if ic[a][a] < 0.0 {
                    return Err(bad("inertia diagonal must be >= 0"));
                }
            }
            if !(l.q.is_finite() && l.qd.is_finite()) {
                return Err(bad("initial q and qd must be finite"));
            }
            if !(l.damping.is_finite() && l.damping >= 0.0) {
                return Err(bad("damping must be finite and >= 0"));
            }
            if !(l.friction.is_finite() && l.friction >= 0.0) {
                return Err(bad("friction must be finite and >= 0"));
            }
            if l.drive.is_some_and(|d| !validate_drive(&d)) {
                return Err(bad(
                    "drive targets must be finite, gains >= 0, max_effort > 0",
                ));
            }
            let s = if revolute {
                [axis[0], axis[1], axis[2], 0.0, 0.0, 0.0]
            } else {
                [0.0, 0.0, 0.0, axis[0], axis[1], axis[2]]
            };
            out.push(Link {
                parent: l.parent,
                revolute,
                axis,
                offset: l.joint_offset,
                mass: l.mass,
                com: l.com,
                inertia: spatial_inertia(l.mass, l.com, ic),
                damping: l.damping,
                friction: l.friction,
                drive: l.drive,
                s,
            });
        }
        let n = links.len();
        Ok(Self {
            config,
            links: out,
            q: links.iter().map(|l| l.q).collect(),
            qd: links.iter().map(|l| l.qd).collect(),
            qdd: vec![0.0; n],
            tau: vec![0.0; n],
            friction_tau: vec![0.0; n],
            drive_clamp: vec![0.0; n],
            scratch: vec![ZERO_SCRATCH; n],
            saved: vec![(0.0, 0.0); n],
            tick: 0,
        })
    }

    pub fn len(&self) -> usize {
        self.links.len()
    }

    pub fn is_empty(&self) -> bool {
        self.links.is_empty()
    }

    pub fn tick(&self) -> u64 {
        self.tick
    }

    pub fn config(&self) -> &ArticulationConfig {
        &self.config
    }

    /// Joint coordinates.
    pub fn q(&self) -> &[f64] {
        &self.q
    }

    /// Joint rates.
    pub fn qd(&self) -> &[f64] {
        &self.qd
    }

    /// Joint accelerations of the last substep (or of the last
    /// [`Articulation::forward_dynamics`] call).
    pub fn qdd(&self) -> &[f64] {
        &self.qdd
    }

    /// Joint friction torques (forces) applied in the last substep.
    pub fn friction_torques(&self) -> &[f64] {
        &self.friction_tau
    }

    /// Set a joint's coordinate and rate.
    pub fn set_state(&mut self, link: usize, q: f64, qd: f64) -> Result<(), ArticulationError> {
        if link >= self.links.len() {
            return Err(ArticulationError::UnknownLink { link });
        }
        if !(q.is_finite() && qd.is_finite()) {
            return Err(ArticulationError::InvalidLink {
                link,
                reason: "q and qd must be finite",
            });
        }
        self.q[link] = q;
        self.qd[link] = qd;
        Ok(())
    }

    /// Hold an external joint torque (force) on `link` until changed.
    pub fn set_joint_torque(&mut self, link: usize, tau: f64) -> Result<(), ArticulationError> {
        if link >= self.links.len() {
            return Err(ArticulationError::UnknownLink { link });
        }
        if !tau.is_finite() {
            return Err(ArticulationError::InvalidLink {
                link,
                reason: "joint torque must be finite",
            });
        }
        self.tau[link] = tau;
        Ok(())
    }

    /// Replace (or remove) a joint's drive.
    pub fn set_drive(
        &mut self,
        link: usize,
        drive: Option<ArticulationDrive>,
    ) -> Result<(), ArticulationError> {
        if link >= self.links.len() {
            return Err(ArticulationError::UnknownLink { link });
        }
        if drive.is_some_and(|d| !validate_drive(&d)) {
            return Err(ArticulationError::InvalidLink {
                link,
                reason: "drive targets must be finite, gains >= 0, max_effort > 0",
            });
        }
        self.links[link].drive = drive;
        Ok(())
    }

    /// Joint torque terms for link `i` and substep `h`: the explicit part
    /// and the implicit coefficient added to `dᵢ`.
    fn joint_terms(&self, i: usize, h: f64) -> (f64, f64) {
        let l = &self.links[i];
        let (q, qd) = (self.q[i], self.qd[i]);
        let mut explicit = self.tau[i] - l.damping * qd;
        let mut implicit = l.damping * h;
        if l.drive.is_some() && self.drive_clamp[i] != 0.0 {
            explicit += self.drive_clamp[i];
        } else if let Some(d) = l.drive {
            let raw = d.stiffness * (d.target_position - q) + d.damping * (d.target_velocity - qd);
            if raw.abs() > d.max_effort {
                explicit += if raw > 0.0 {
                    d.max_effort
                } else {
                    -d.max_effort
                };
            } else {
                // Spring at `q + h q̇ + h² q̈`, damper at `q̇ + h q̈`.
                explicit += d.stiffness * (d.target_position - q - h * qd)
                    + d.damping * (d.target_velocity - qd);
                implicit += d.stiffness * h * h + d.damping * h;
            }
        }
        (explicit, implicit)
    }

    /// One articulated-body pass: fills `qdd` from the current state with
    /// joint torques `explicit + extra`.
    fn aba(&mut self, h: f64, with_friction: bool) -> Result<(), ArticulationError> {
        let n = self.links.len();
        // Pass 1: transforms, velocities, bias terms.
        for i in 0..n {
            let l = self.links[i];
            let q = self.q[i];
            let x = if l.revolute {
                Xform {
                    e: transpose3(&rotation(l.axis, q)),
                    r: l.offset,
                }
            } else {
                Xform {
                    e: I3,
                    r: add3(l.offset, scale3(l.axis, q)),
                }
            };
            let vj = l.s.map(|s| s * self.qd[i]);
            let v = match l.parent {
                Some(p) => {
                    let vp = x.motion(self.scratch[p].v);
                    let mut v = [0.0; 6];
                    for k in 0..6 {
                        v[k] = vp[k] + vj[k];
                    }
                    v
                }
                None => vj,
            };
            let sc = &mut self.scratch[i];
            sc.x = x;
            sc.xm = x.matrix();
            sc.v = v;
            sc.c = crm(v, vj);
            sc.ia = l.inertia;
            sc.pa = crf(v, mv6(&l.inertia, v));
        }
        // Pass 2: articulated inertias, inward.
        for i in (0..n).rev() {
            let l = self.links[i];
            let (explicit, implicit) = self.joint_terms(i, h);
            let tau = explicit
                + if with_friction {
                    self.friction_tau[i]
                } else {
                    0.0
                };
            let sc = self.scratch[i];
            let u_vec = mv6(&sc.ia, l.s);
            let d = dot6(l.s, u_vec) + implicit;
            if d.is_nan() || d <= 0.0 {
                return Err(ArticulationError::Singular { link: i });
            }
            let u = tau - dot6(l.s, sc.pa);
            self.scratch[i].u_vec = u_vec;
            self.scratch[i].d = d;
            self.scratch[i].u = u;
            if let Some(p) = l.parent {
                let mut ia = sc.ia;
                for (r, row) in ia.iter_mut().enumerate() {
                    for (c, cell) in row.iter_mut().enumerate() {
                        *cell -= u_vec[r] * u_vec[c] / d;
                    }
                }
                let iac = mv6(&ia, sc.c);
                let mut pa = [0.0; 6];
                for k in 0..6 {
                    pa[k] = sc.pa[k] + iac[k] + u_vec[k] * (u / d);
                }
                let add_ia = congruence(&sc.xm, &ia);
                let add_pa = sc.x.force_back(pa);
                let ps = &mut self.scratch[p];
                for r in 0..6 {
                    for c in 0..6 {
                        ps.ia[r][c] += add_ia[r][c];
                    }
                    ps.pa[r] += add_pa[r];
                }
            }
        }
        // Pass 3: accelerations, outward. The world accelerates at -g.
        let g = self.config.gravity;
        let a0 = [0.0, 0.0, 0.0, -g[0], -g[1], -g[2]];
        for i in 0..n {
            let l = self.links[i];
            let sc = self.scratch[i];
            let ap = match l.parent {
                Some(p) => self.scratch[p].a,
                None => a0,
            };
            let xa = sc.x.motion(ap);
            let mut a1 = [0.0; 6];
            for k in 0..6 {
                a1[k] = xa[k] + sc.c[k];
            }
            let qdd = (sc.u - dot6(sc.u_vec, a1)) / sc.d;
            let mut a = [0.0; 6];
            for k in 0..6 {
                a[k] = a1[k] + l.s[k] * qdd;
            }
            self.scratch[i].a = a;
            self.qdd[i] = qdd;
        }
        Ok(())
    }

    /// Joint accelerations at the current state (no drives' implicit terms:
    /// `h = 0`, no friction), without integrating. For checking the
    /// dynamics against closed forms.
    pub fn forward_dynamics(&mut self) -> Result<&[f64], ArticulationError> {
        self.aba(0.0, false)?;
        Ok(&self.qdd)
    }

    /// The effort an unsaturated implicit drive exerts at the solved `q̈`.
    fn drive_effort(&self, i: usize, h: f64) -> Option<(f64, f64)> {
        let d = self.links[i].drive?;
        if self.drive_clamp[i] != 0.0 {
            return None;
        }
        let (q, qd) = (self.q[i], self.qd[i]);
        let raw = d.stiffness * (d.target_position - q) + d.damping * (d.target_velocity - qd);
        if raw.abs() > d.max_effort {
            return None;
        }
        let tau = d.stiffness * (d.target_position - q - h * qd)
            + d.damping * (d.target_velocity - qd)
            - (d.stiffness * h * h + d.damping * h) * self.qdd[i];
        Some((tau, d.max_effort))
    }

    fn substep(&mut self, h: f64) -> Result<(), ArticulationError> {
        let any_friction = self.links.iter().any(|l| l.friction > 0.0);
        self.drive_clamp.fill(0.0);
        self.aba(h, false)?;
        // An implicit drive whose solved effort (including the implicit
        // `−(k h² + c h) q̈` part) exceeds `max_effort` saturates: one more
        // pass with it clamped and explicit.
        let mut clamped = false;
        for i in 0..self.links.len() {
            if let Some((tau, max)) = self.drive_effort(i, h)
                && tau.abs() > max
            {
                self.drive_clamp[i] = if tau > 0.0 { max } else { -max };
                clamped = true;
            }
        }
        if clamped {
            self.aba(h, false)?;
        }
        if any_friction {
            for i in 0..self.links.len() {
                let f = self.links[i].friction;
                if f == 0.0 {
                    self.friction_tau[i] = 0.0;
                    continue;
                }
                let predicted = self.qd[i] + h * self.qdd[i];
                let stick = -(self.scratch[i].d * predicted / h);
                self.friction_tau[i] = if stick.abs() <= f {
                    stick
                } else if predicted > 0.0 {
                    -f
                } else {
                    f
                };
            }
            self.aba(h, true)?;
        }
        for i in 0..self.links.len() {
            self.qd[i] += h * self.qdd[i];
            self.q[i] += h * self.qd[i];
        }
        Ok(())
    }

    /// Advance one tick (`substeps` substeps). Returns the SHA-256 of the
    /// state ([`Articulation::hash`]). On error the state is unchanged.
    pub fn step(&mut self) -> Result<[u8; 32], ArticulationError> {
        let next = self
            .tick
            .checked_add(1)
            .ok_or(ArticulationError::TickOverflow)?;
        for i in 0..self.q.len() {
            self.saved[i] = (self.q[i], self.qd[i]);
        }
        let h = self.config.dt / f64::from(self.config.substeps);
        let mut result = Ok(());
        for _ in 0..self.config.substeps {
            result = self.substep(h);
            if result.is_err() {
                break;
            }
            if let Some(link) =
                (0..self.q.len()).find(|&i| !(self.q[i].is_finite() && self.qd[i].is_finite()))
            {
                result = Err(ArticulationError::NonFinite { link });
                break;
            }
        }
        if let Err(e) = result {
            for i in 0..self.q.len() {
                (self.q[i], self.qd[i]) = self.saved[i];
            }
            return Err(e);
        }
        self.tick = next;
        Ok(self.hash())
    }

    /// World pose of every link: `(origin, rotation)` with `world = R ·
    /// link + origin`.
    pub fn link_poses(&self) -> Vec<(V, M3)> {
        let mut out: Vec<(V, M3)> = Vec::with_capacity(self.links.len());
        for (i, l) in self.links.iter().enumerate() {
            let (e, r) = if l.revolute {
                (transpose3(&rotation(l.axis, self.q[i])), l.offset)
            } else {
                (I3, add3(l.offset, scale3(l.axis, self.q[i])))
            };
            let (pp, rp) = match l.parent {
                Some(p) => out[p],
                None => ([0.0; 3], I3),
            };
            out.push((add3(pp, mv3(&rp, r)), mm3(&rp, &transpose3(&e))));
        }
        out
    }

    /// World position of a link's centre of mass.
    pub fn com_world(&self, link: usize) -> Option<V> {
        let poses = self.link_poses();
        let (p, r) = poses.get(link)?;
        Some(add3(*p, mv3(r, self.links[link].com)))
    }

    /// Kinetic and potential energy (`½ Σ vᵢᵀ Iᵢ vᵢ`, `−Σ mᵢ g · cᵢ`).
    pub fn energy(&self) -> (f64, f64) {
        let poses = self.link_poses();
        let mut v: Vec<Sv> = Vec::with_capacity(self.links.len());
        let (mut ke, mut pe) = (0.0, 0.0);
        for (i, l) in self.links.iter().enumerate() {
            let x = if l.revolute {
                Xform {
                    e: transpose3(&rotation(l.axis, self.q[i])),
                    r: l.offset,
                }
            } else {
                Xform {
                    e: I3,
                    r: add3(l.offset, scale3(l.axis, self.q[i])),
                }
            };
            let vj = l.s.map(|s| s * self.qd[i]);
            let vi = match l.parent {
                Some(p) => {
                    let vp = x.motion(v[p]);
                    let mut out = [0.0; 6];
                    for k in 0..6 {
                        out[k] = vp[k] + vj[k];
                    }
                    out
                }
                None => vj,
            };
            ke += 0.5 * dot6(vi, mv6(&l.inertia, vi));
            let (p, r) = poses[i];
            let c = add3(p, mv3(&r, l.com));
            pe -= l.mass * dot3(self.config.gravity, c);
            v.push(vi);
        }
        (ke, pe)
    }

    /// SHA-256 of the tick, the link count and every `q`, `q̇` bit pattern
    /// (little-endian), in link order.
    pub fn hash(&self) -> [u8; 32] {
        let mut sha = sha2::Sha256::new();
        sha.update(self.tick.to_le_bytes());
        sha.update((self.links.len() as u64).to_le_bytes());
        for (q, qd) in self.q.iter().zip(&self.qd) {
            sha.update(q.to_bits().to_le_bytes());
            sha.update(qd.to_bits().to_le_bytes());
        }
        sha.finalize().into()
    }
}

/// Mass, centre of mass and inertia about the centre of mass of a thin
/// uniform rod of `length` along `dir` (unit) from the link origin.
pub fn rod_link_inertia(mass: f64, length: f64, dir: V) -> (V, M3) {
    let com = scale3(dir, 0.5 * length);
    let i = mass * length * length / 12.0;
    // I (1 − d dᵀ) for a rod along d.
    let mut m = [[0.0; 3]; 3];
    for a in 0..3 {
        for b in 0..3 {
            let delta = if a == b { 1.0 } else { 0.0 };
            m[a][b] = i * (delta - dir[a] * dir[b]);
        }
    }
    (com, m)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transform_round_trip_preserves_power() {
        // vᵀ f is frame independent: (X v)ᵀ f_B = vᵀ (Xᵀ f_B).
        let x = Xform {
            e: transpose3(&rotation(unit([1.0, 2.0, 3.0]).unwrap(), 0.7)),
            r: [0.3, -1.0, 2.0],
        };
        let v = [0.1, 0.2, -0.3, 1.0, -2.0, 0.5];
        let f = [0.4, -0.1, 0.9, -0.2, 0.3, 1.1];
        let lhs = dot6(x.motion(v), f);
        let rhs = dot6(v, x.force_back(f));
        assert!((lhs - rhs).abs() < 1e-12);
        // The matrix form agrees with the direct form.
        let m = x.matrix();
        let a = mv6(&m, v);
        let b = x.motion(v);
        for k in 0..6 {
            assert!((a[k] - b[k]).abs() < 1e-12);
        }
    }
}
