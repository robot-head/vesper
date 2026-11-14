//! Differentiable XPBD (§36, Tensor gradients): reverse-mode adjoints of a
//! particle XPBD simulation with respect to initial positions, initial
//! velocities and per-constraint compliance, exported as `vesper-tensor`
//! host tensors.
//!
//! The forward model is the [`crate::world::World`] substep in binary64:
//! semi-implicit integration under gravity, one Gauss–Seidel pass over the
//! distance constraints in list order with `Δλ = −C / (w_a + w_b + α/h²)`,
//! then `v = (x − x_prev)/h`. With [`DiffOptions`] the pass can also run in
//! the World's colour order ([`DiffOrder::WorldColors`], the same
//! [`crate::coloring::Colorer`] over joints then contacts) and include the
//! World's sphere–sphere and ground contacts ([`DiffContacts`]): each tick's
//! contact set is found with the World's reach test, and each contact
//! projects only while it penetrates. Contact activation is a step
//! function, so the gradient is that of the branch the forward pass took
//! (the usual piecewise derivative of a hybrid system); it is exact away
//! from the activation boundaries and undefined on them. The adjoint is computed by recording every scalar
//! operation on a tape and sweeping it backwards (exact derivatives of the
//! floating-point program's real-number semantics, not an approximation);
//! the tests check it against central finite differences.

// `!(x > y)` is deliberate: it is also true when `x` is NaN.
#![allow(clippy::neg_cmp_op_on_partial_ord)]

use crate::coloring::{Colorer, ConstraintBodies};
use vesper_tensor::host::{Bindings, Tensor};

/// One particle.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DiffParticle {
    pub pos: [f64; 3],
    pub vel: [f64; 3],
    /// `0.0` marks a static particle.
    pub inv_mass: f64,
}

/// A distance constraint with XPBD compliance.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DiffConstraint {
    pub a: usize,
    pub b: usize,
    pub rest_length: f64,
    pub compliance: f64,
}

/// A differentiable scene.
#[derive(Debug, Clone, PartialEq)]
pub struct DiffScene {
    pub particles: Vec<DiffParticle>,
    pub constraints: Vec<DiffConstraint>,
    pub gravity: [f64; 3],
    pub dt: f64,
    pub substeps: u32,
    pub ticks: u32,
}

/// Typed failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffError {
    InvalidScene(&'static str),
    /// A constraint's endpoints coincided during the simulation (its
    /// direction, and so its derivative, is undefined).
    Degenerate {
        tick: u32,
        constraint: usize,
    },
    /// The tape would exceed [`MAX_TAPE`] operations.
    TapeTooLarge,
    /// A seed slice has the wrong length.
    SeedLength,
}

impl std::fmt::Display for DiffError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidScene(what) => write!(f, "invalid scene: {what}"),
            Self::Degenerate { tick, constraint } => {
                write!(f, "constraint {constraint} degenerate at tick {tick}")
            }
            Self::TapeTooLarge => write!(f, "tape exceeds {MAX_TAPE} operations"),
            Self::SeedLength => write!(f, "seed length does not match the particle count"),
        }
    }
}

impl std::error::Error for DiffError {}

/// Largest tape, in recorded operations (each 40 bytes).
pub const MAX_TAPE: usize = 1 << 24;

const NONE: u32 = u32::MAX;

/// Reverse-mode tape over scalar operations.
struct Tape {
    val: Vec<f64>,
    parents: Vec<[(u32, f64); 2]>,
}

#[derive(Clone, Copy)]
struct Var(u32);

impl Tape {
    fn push(&mut self, v: f64, p: [(u32, f64); 2]) -> Result<Var, DiffError> {
        if self.val.len() >= MAX_TAPE {
            return Err(DiffError::TapeTooLarge);
        }
        self.val.push(v);
        self.parents.push(p);
        Ok(Var((self.val.len() - 1) as u32))
    }
    fn leaf(&mut self, v: f64) -> Result<Var, DiffError> {
        self.push(v, [(NONE, 0.0), (NONE, 0.0)])
    }
    fn v(&self, x: Var) -> f64 {
        self.val[x.0 as usize]
    }
    fn add(&mut self, a: Var, b: Var) -> Result<Var, DiffError> {
        self.push(self.v(a) + self.v(b), [(a.0, 1.0), (b.0, 1.0)])
    }
    fn sub(&mut self, a: Var, b: Var) -> Result<Var, DiffError> {
        self.push(self.v(a) - self.v(b), [(a.0, 1.0), (b.0, -1.0)])
    }
    fn mul(&mut self, a: Var, b: Var) -> Result<Var, DiffError> {
        let (x, y) = (self.v(a), self.v(b));
        self.push(x * y, [(a.0, y), (b.0, x)])
    }
    fn scale(&mut self, a: Var, k: f64) -> Result<Var, DiffError> {
        self.push(self.v(a) * k, [(a.0, k), (NONE, 0.0)])
    }
    fn div(&mut self, a: Var, b: Var) -> Result<Var, DiffError> {
        let (x, y) = (self.v(a), self.v(b));
        self.push(x / y, [(a.0, 1.0 / y), (b.0, -x / (y * y))])
    }
    fn sqrt(&mut self, a: Var) -> Result<Var, DiffError> {
        let s = self.v(a).sqrt();
        self.push(s, [(a.0, 0.5 / s), (NONE, 0.0)])
    }
    fn add_const(&mut self, a: Var, k: f64) -> Result<Var, DiffError> {
        self.push(self.v(a) + k, [(a.0, 1.0), (NONE, 0.0)])
    }

    /// Adjoints of every tape entry for the seeds.
    fn backward(&self, seeds: &[(Var, f64)]) -> Vec<f64> {
        let mut adj = vec![0.0; self.val.len()];
        for (v, s) in seeds {
            adj[v.0 as usize] += *s;
        }
        for i in (0..self.val.len()).rev() {
            let a = adj[i];
            if a == 0.0 {
                continue;
            }
            for (p, d) in self.parents[i] {
                if p != NONE {
                    adj[p as usize] += a * d;
                }
            }
        }
        adj
    }
}

fn validate(scene: &DiffScene) -> Result<(), DiffError> {
    let finite3 = |v: &[f64; 3]| v.iter().all(|x| x.is_finite());
    if !(scene.dt.is_finite() && scene.dt > 0.0) {
        return Err(DiffError::InvalidScene("dt must be finite and positive"));
    }
    if scene.substeps == 0 || scene.substeps > 64 {
        return Err(DiffError::InvalidScene("substeps must be in 1..=64"));
    }
    if !finite3(&scene.gravity) {
        return Err(DiffError::InvalidScene("gravity must be finite"));
    }
    for p in &scene.particles {
        if !(finite3(&p.pos) && finite3(&p.vel) && p.inv_mass.is_finite() && p.inv_mass >= 0.0) {
            return Err(DiffError::InvalidScene("particle fields must be finite"));
        }
    }
    let n = scene.particles.len();
    for c in &scene.constraints {
        if c.a >= n || c.b >= n || c.a == c.b {
            return Err(DiffError::InvalidScene(
                "constraint names a bad particle pair",
            ));
        }
        if !(c.rest_length.is_finite()
            && c.rest_length >= 0.0
            && c.compliance.is_finite()
            && c.compliance >= 0.0)
        {
            return Err(DiffError::InvalidScene(
                "rest length and compliance must be finite and >= 0",
            ));
        }
    }
    Ok(())
}

/// Order of the Gauss–Seidel pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DiffOrder {
    /// Joints in list order, then contacts in detection order.
    #[default]
    List,
    /// The World's order: colour by colour, each colour in the colourer's
    /// stable visiting order, over joints followed by the tick's contacts.
    WorldColors,
}

/// The World's sphere and ground contacts, per particle.
#[derive(Debug, Clone, PartialEq)]
pub struct DiffContacts {
    /// Sphere radius of every particle.
    pub radii: Vec<f64>,
    /// Contact compliance of every particle (a pair contact uses the sum,
    /// like the World's `compliance_alpha`).
    pub compliance: Vec<f64>,
    /// Height of the static ground plane, if any.
    pub ground_y: Option<f64>,
    /// Speculative contact distance (the World's `contact_margin`).
    pub margin: f64,
}

/// Options for [`simulate_with`] and [`gradients_with`].
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DiffOptions {
    pub order: DiffOrder,
    pub contacts: Option<DiffContacts>,
}

/// One constraint of a tick's pass.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Item {
    Joint(usize),
    Pair(usize, usize),
    Ground(usize),
}

struct Recorded {
    tape: Tape,
    pos0: Vec<[Var; 3]>,
    vel0: Vec<[Var; 3]>,
    alpha: Vec<Var>,
    radius: Vec<Var>,
    contact_alpha: Vec<Var>,
    pos: Vec<[Var; 3]>,
    vel: Vec<[Var; 3]>,
    activations: usize,
}

fn validate_contacts(scene: &DiffScene, c: &DiffContacts) -> Result<(), DiffError> {
    let n = scene.particles.len();
    if c.radii.len() != n || c.compliance.len() != n {
        return Err(DiffError::InvalidScene(
            "contact radii and compliance need one entry per particle",
        ));
    }
    let ok = |v: f64| v.is_finite() && v >= 0.0;
    if !c.radii.iter().chain(&c.compliance).all(|v| ok(*v)) || !ok(c.margin) {
        return Err(DiffError::InvalidScene(
            "contact radii, compliance and margin must be finite and >= 0",
        ));
    }
    if c.ground_y.is_some_and(|g| !g.is_finite()) {
        return Err(DiffError::InvalidScene("ground height must be finite"));
    }
    Ok(())
}

/// The World's contact search for one tick on the current values: pairs
/// within `r_i + r_j + margin + sweep_i + sweep_j`, ground contacts within
/// `margin + sweep_i`, with `sweep = |v|₁·dt + |g|₁·dt²` for dynamic
/// particles.
fn detect(
    scene: &DiffScene,
    c: &DiffContacts,
    pos: &[[f64; 3]],
    vel: &[[f64; 3]],
    out: &mut Vec<Item>,
) {
    let n = pos.len();
    let dt = scene.dt;
    let g1 = scene.gravity.iter().map(|g| g.abs()).sum::<f64>();
    let sweep: Vec<f64> = (0..n)
        .map(|i| {
            if scene.particles[i].inv_mass == 0.0 {
                0.0
            } else {
                vel[i].iter().map(|v| v.abs()).sum::<f64>() * dt + g1 * dt * dt
            }
        })
        .collect();
    for i in 0..n {
        let wi = scene.particles[i].inv_mass;
        if let Some(ground) = c.ground_y
            && wi > 0.0
            && pos[i][1] - c.radii[i] - ground < c.margin + sweep[i]
        {
            out.push(Item::Ground(i));
        }
        for j in i + 1..n {
            if wi == 0.0 && scene.particles[j].inv_mass == 0.0 {
                continue;
            }
            let d = [
                pos[i][0] - pos[j][0],
                pos[i][1] - pos[j][1],
                pos[i][2] - pos[j][2],
            ];
            let reach = c.radii[i] + c.radii[j] + c.margin + sweep[i] + sweep[j];
            if d[0] * d[0] + d[1] * d[1] + d[2] * d[2] < reach * reach {
                out.push(Item::Pair(i, j));
            }
        }
    }
}

fn record_with(scene: &DiffScene, opts: &DiffOptions) -> Result<Recorded, DiffError> {
    validate(scene)?;
    if let Some(c) = &opts.contacts {
        validate_contacts(scene, c)?;
    }
    let mut t = Tape {
        val: Vec::new(),
        parents: Vec::new(),
    };
    let mut pos0 = Vec::new();
    let mut vel0 = Vec::new();
    for p in &scene.particles {
        pos0.push([t.leaf(p.pos[0])?, t.leaf(p.pos[1])?, t.leaf(p.pos[2])?]);
        vel0.push([t.leaf(p.vel[0])?, t.leaf(p.vel[1])?, t.leaf(p.vel[2])?]);
    }
    let mut alpha = Vec::new();
    for c in &scene.constraints {
        alpha.push(t.leaf(c.compliance)?);
    }
    let (mut radius, mut contact_alpha) = (Vec::new(), Vec::new());
    if let Some(c) = &opts.contacts {
        for i in 0..scene.particles.len() {
            radius.push(t.leaf(c.radii[i])?);
            contact_alpha.push(t.leaf(c.compliance[i])?);
        }
    }
    let n = scene.particles.len();
    let mut items: Vec<Item> = Vec::new();
    let mut bodies: Vec<ConstraintBodies> = Vec::new();
    let mut colorer = Colorer::new(n, 0);
    let mut activations = 0usize;
    let h = scene.dt / f64::from(scene.substeps);
    let inv_h = 1.0 / h;
    let inv_h2 = inv_h * inv_h;
    let g = scene.gravity;
    let mut pos = pos0.clone();
    let mut vel = vel0.clone();
    let w: Vec<f64> = scene.particles.iter().map(|p| p.inv_mass).collect();
    for tick in 0..scene.ticks {
        // The tick's constraint list: joints, then contacts.
        items.clear();
        items.extend((0..scene.constraints.len()).map(Item::Joint));
        if let Some(c) = &opts.contacts {
            let value = |v: &[[Var; 3]]| -> Vec<[f64; 3]> {
                v.iter()
                    .map(|p| [t.v(p[0]), t.v(p[1]), t.v(p[2])])
                    .collect()
            };
            detect(scene, c, &value(&pos), &value(&vel), &mut items);
        }
        if opts.order == DiffOrder::WorldColors {
            bodies.clear();
            for item in &items {
                bodies.push(match *item {
                    Item::Joint(ci) => ConstraintBodies {
                        a: scene.constraints[ci].a as u32,
                        b: Some(scene.constraints[ci].b as u32),
                    },
                    Item::Pair(a, b) => ConstraintBodies {
                        a: a as u32,
                        b: Some(b as u32),
                    },
                    Item::Ground(a) => ConstraintBodies {
                        a: a as u32,
                        b: None,
                    },
                });
            }
            if colorer.capacity() < bodies.len() {
                colorer = Colorer::new(n, bodies.len());
            }
            colorer
                .color(&bodies)
                .map_err(|_| DiffError::InvalidScene("constraints need more than 64 colours"))?;
            let order: Vec<Item> = colorer
                .layers()
                .flatten()
                .map(|&k| items[k as usize])
                .collect();
            items.clear();
            items.extend(order);
        }
        for _ in 0..scene.substeps {
            let prev = pos.clone();
            for i in 0..pos.len() {
                if w[i] == 0.0 {
                    continue;
                }
                for k in 0..3 {
                    vel[i][k] = t.add_const(vel[i][k], g[k] * h)?;
                    let dx = t.scale(vel[i][k], h)?;
                    pos[i][k] = t.add(pos[i][k], dx)?;
                }
            }
            for item in &items {
                match *item {
                    Item::Joint(ci) => {
                        let c = scene.constraints[ci];
                        let (wa, wb) = (w[c.a], w[c.b]);
                        if wa + wb == 0.0 {
                            continue;
                        }
                        let at = t.scale(alpha[ci], inv_h2)?;
                        let target = Target::Const(c.rest_length);
                        let degenerate = DiffError::Degenerate {
                            tick,
                            constraint: ci,
                        };
                        project_pair(
                            &mut t, &mut pos, c.a, c.b, wa, wb, target, at, false, degenerate,
                        )?;
                    }
                    Item::Pair(a, b) => {
                        let (wa, wb) = (w[a], w[b]);
                        let rsum = t.add(radius[a], radius[b])?;
                        let asum = t.add(contact_alpha[a], contact_alpha[b])?;
                        let at = t.scale(asum, inv_h2)?;
                        let degenerate = DiffError::Degenerate {
                            tick,
                            constraint: usize::MAX,
                        };
                        if project_pair(
                            &mut t,
                            &mut pos,
                            a,
                            b,
                            wa,
                            wb,
                            Target::Var(rsum),
                            at,
                            true,
                            degenerate,
                        )? {
                            activations += 1;
                        }
                    }
                    Item::Ground(a) => {
                        let ground = opts
                            .contacts
                            .as_ref()
                            .and_then(|c| c.ground_y)
                            .expect("ground contacts need a ground");
                        let wa = w[a];
                        let below = t.sub(pos[a][1], radius[a])?;
                        let cval = t.add_const(below, -ground)?;
                        if !(t.v(cval) < 0.0) || wa == 0.0 {
                            continue;
                        }
                        activations += 1;
                        let at = t.scale(contact_alpha[a], inv_h2)?;
                        let denom = t.add_const(at, wa)?;
                        let neg_c = t.scale(cval, -1.0)?;
                        let dl = t.div(neg_c, denom)?;
                        let step = t.scale(dl, wa)?;
                        pos[a][1] = t.add(pos[a][1], step)?;
                    }
                }
            }
            for i in 0..pos.len() {
                if w[i] == 0.0 {
                    continue;
                }
                for k in 0..3 {
                    let dx = t.sub(pos[i][k], prev[i][k])?;
                    vel[i][k] = t.scale(dx, inv_h)?;
                }
            }
        }
    }
    Ok(Recorded {
        tape: t,
        pos0,
        vel0,
        alpha,
        radius,
        contact_alpha,
        pos,
        vel,
        activations,
    })
}

/// A constraint target length: fixed (a joint's rest length) or on the tape
/// (a contact's radius sum).
#[derive(Clone, Copy)]
enum Target {
    Const(f64),
    Var(Var),
}

/// Project a two-particle distance constraint (equality, or an inequality
/// that only pushes) on the tape, as the World does. Returns whether it
/// projected.
#[allow(clippy::too_many_arguments)]
fn project_pair(
    t: &mut Tape,
    pos: &mut [[Var; 3]],
    a: usize,
    b: usize,
    wa: f64,
    wb: f64,
    target: Target,
    alpha_tilde: Var,
    inequality: bool,
    degenerate: DiffError,
) -> Result<bool, DiffError> {
    if wa + wb == 0.0 {
        return Ok(false);
    }
    let d = [
        t.sub(pos[a][0], pos[b][0])?,
        t.sub(pos[a][1], pos[b][1])?,
        t.sub(pos[a][2], pos[b][2])?,
    ];
    let dd0 = t.mul(d[0], d[0])?;
    let dd1 = t.mul(d[1], d[1])?;
    let dd2 = t.mul(d[2], d[2])?;
    let s01 = t.add(dd0, dd1)?;
    let dd = t.add(s01, dd2)?;
    if !(t.v(dd) > 1e-24) {
        return Err(degenerate);
    }
    let len = t.sqrt(dd)?;
    let cval = match target {
        Target::Const(r) => t.add_const(len, -r)?,
        Target::Var(r) => t.sub(len, r)?,
    };
    if inequality && !(t.v(cval) < 0.0) {
        return Ok(false);
    }
    let denom = t.add_const(alpha_tilde, wa + wb)?;
    let neg_c = t.scale(cval, -1.0)?;
    let dl = t.div(neg_c, denom)?;
    for k in 0..3 {
        let nk = t.div(d[k], len)?;
        let step = t.mul(dl, nk)?;
        if wa > 0.0 {
            let sa = t.scale(step, wa)?;
            pos[a][k] = t.add(pos[a][k], sa)?;
        }
        if wb > 0.0 {
            let sb = t.scale(step, wb)?;
            pos[b][k] = t.sub(pos[b][k], sb)?;
        }
    }
    Ok(true)
}

/// Final `(positions, velocities)`.
pub type FinalState = (Vec<[f64; 3]>, Vec<[f64; 3]>);

/// Run the forward model without recording gradients.
pub fn simulate(scene: &DiffScene) -> Result<FinalState, DiffError> {
    simulate_with(scene, &DiffOptions::default())
}

/// [`simulate`] with a constraint order and optional contacts.
pub fn simulate_with(scene: &DiffScene, opts: &DiffOptions) -> Result<FinalState, DiffError> {
    let r = record_with(scene, opts)?;
    let get = |v: &[[Var; 3]]| -> Vec<[f64; 3]> {
        v.iter()
            .map(|p| [r.tape.v(p[0]), r.tape.v(p[1]), r.tape.v(p[2])])
            .collect()
    };
    Ok((get(&r.pos), get(&r.vel)))
}

/// Gradients of `L = Σ seed_pos·x_T + seed_vel·v_T` (a vector–Jacobian
/// product; any differentiable loss supplies its own seeds).
#[derive(Debug, Clone, PartialEq)]
pub struct Gradients {
    pub loss: f64,
    pub d_pos0: Vec<[f64; 3]>,
    pub d_vel0: Vec<[f64; 3]>,
    pub d_compliance: Vec<f64>,
}

impl Gradients {
    /// The gradients as `vesper-tensor` f32 vectors: `d_loss_d_pos0` and
    /// `d_loss_d_vel0` (length `3n`, x/y/z per particle) and
    /// `d_loss_d_compliance` (length `m`). The narrowing to binary32 rounds
    /// to nearest.
    pub fn to_tensors(&self) -> Bindings {
        let flat = |v: &[[f64; 3]]| -> Vec<f32> {
            v.iter().flat_map(|p| p.iter().map(|x| *x as f32)).collect()
        };
        let mut out = Bindings::new();
        out.insert("d_loss_d_pos0".into(), Tensor::vector(flat(&self.d_pos0)));
        out.insert("d_loss_d_vel0".into(), Tensor::vector(flat(&self.d_vel0)));
        out.insert(
            "d_loss_d_compliance".into(),
            Tensor::vector(self.d_compliance.iter().map(|x| *x as f32).collect()),
        );
        out
    }
}

/// Reverse-mode adjoint of the whole simulation.
pub fn gradients(
    scene: &DiffScene,
    seed_pos: &[[f64; 3]],
    seed_vel: &[[f64; 3]],
) -> Result<Gradients, DiffError> {
    Ok(gradients_with(scene, &DiffOptions::default(), seed_pos, seed_vel)?.gradients)
}

/// [`Gradients`] plus the contact parameters' gradients.
#[derive(Debug, Clone, PartialEq)]
pub struct ContactGradients {
    pub gradients: Gradients,
    /// `dL/d radius` per particle (empty without contacts).
    pub d_radius: Vec<f64>,
    /// `dL/d contact compliance` per particle (empty without contacts).
    pub d_contact_compliance: Vec<f64>,
    /// Contact projections recorded (substeps × active contacts).
    pub contact_activations: usize,
}

/// Reverse-mode adjoint with a constraint order and optional contacts.
pub fn gradients_with(
    scene: &DiffScene,
    opts: &DiffOptions,
    seed_pos: &[[f64; 3]],
    seed_vel: &[[f64; 3]],
) -> Result<ContactGradients, DiffError> {
    let n = scene.particles.len();
    if seed_pos.len() != n || seed_vel.len() != n {
        return Err(DiffError::SeedLength);
    }
    if !seed_pos
        .iter()
        .chain(seed_vel)
        .flatten()
        .all(|x| x.is_finite())
    {
        return Err(DiffError::InvalidScene("seeds must be finite"));
    }
    let r = record_with(scene, opts)?;
    let mut seeds = Vec::with_capacity(6 * n);
    let mut loss = 0.0;
    for i in 0..n {
        for k in 0..3 {
            seeds.push((r.pos[i][k], seed_pos[i][k]));
            seeds.push((r.vel[i][k], seed_vel[i][k]));
            loss += seed_pos[i][k] * r.tape.v(r.pos[i][k]) + seed_vel[i][k] * r.tape.v(r.vel[i][k]);
        }
    }
    let adj = r.tape.backward(&seeds);
    let pick = |v: &[[Var; 3]]| -> Vec<[f64; 3]> {
        v.iter()
            .map(|p| {
                [
                    adj[p[0].0 as usize],
                    adj[p[1].0 as usize],
                    adj[p[2].0 as usize],
                ]
            })
            .collect()
    };
    Ok(ContactGradients {
        gradients: Gradients {
            loss,
            d_pos0: pick(&r.pos0),
            d_vel0: pick(&r.vel0),
            d_compliance: r.alpha.iter().map(|a| adj[a.0 as usize]).collect(),
        },
        d_radius: r.radius.iter().map(|a| adj[a.0 as usize]).collect(),
        d_contact_compliance: r.contact_alpha.iter().map(|a| adj[a.0 as usize]).collect(),
        contact_activations: r.activations,
    })
}
