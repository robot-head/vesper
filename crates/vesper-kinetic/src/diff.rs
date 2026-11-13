//! Differentiable XPBD (§36, Tensor gradients): reverse-mode adjoints of a
//! particle XPBD simulation with respect to initial positions, initial
//! velocities and per-constraint compliance, exported as `vesper-tensor`
//! host tensors.
//!
//! The forward model is the [`crate::world::World`] substep in binary64:
//! semi-implicit integration under gravity, one Gauss–Seidel pass over the
//! distance constraints in list order with `Δλ = −C / (w_a + w_b + α/h²)`,
//! then `v = (x − x_prev)/h`. Contacts are not part of this model: their
//! activation is not differentiable, and the World's contact set changes
//! between ticks. The adjoint is computed by recording every scalar
//! operation on a tape and sweeping it backwards (exact derivatives of the
//! floating-point program's real-number semantics, not an approximation);
//! the tests check it against central finite differences.

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
    Degenerate { tick: u32, constraint: usize },
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
            return Err(DiffError::InvalidScene("constraint names a bad particle pair"));
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

struct Recorded {
    tape: Tape,
    pos0: Vec<[Var; 3]>,
    vel0: Vec<[Var; 3]>,
    alpha: Vec<Var>,
    pos: Vec<[Var; 3]>,
    vel: Vec<[Var; 3]>,
}

fn record(scene: &DiffScene) -> Result<Recorded, DiffError> {
    validate(scene)?;
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
    let h = scene.dt / f64::from(scene.substeps);
    let inv_h = 1.0 / h;
    let inv_h2 = inv_h * inv_h;
    let g = scene.gravity;
    let mut pos = pos0.clone();
    let mut vel = vel0.clone();
    let w: Vec<f64> = scene.particles.iter().map(|p| p.inv_mass).collect();
    for tick in 0..scene.ticks {
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
            for (ci, c) in scene.constraints.iter().enumerate() {
                let (wa, wb) = (w[c.a], w[c.b]);
                if wa + wb == 0.0 {
                    continue;
                }
                let d = [
                    t.sub(pos[c.a][0], pos[c.b][0])?,
                    t.sub(pos[c.a][1], pos[c.b][1])?,
                    t.sub(pos[c.a][2], pos[c.b][2])?,
                ];
                let dd0 = t.mul(d[0], d[0])?;
                let dd1 = t.mul(d[1], d[1])?;
                let dd2 = t.mul(d[2], d[2])?;
                let s01 = t.add(dd0, dd1)?;
                let dd = t.add(s01, dd2)?;
                if !(t.v(dd) > 1e-24) {
                    return Err(DiffError::Degenerate {
                        tick,
                        constraint: ci,
                    });
                }
                let len = t.sqrt(dd)?;
                let cval = t.add_const(len, -c.rest_length)?;
                let at = t.scale(alpha[ci], inv_h2)?;
                let denom = t.add_const(at, wa + wb)?;
                let neg_c = t.scale(cval, -1.0)?;
                let dl = t.div(neg_c, denom)?;
                for k in 0..3 {
                    let nk = t.div(d[k], len)?;
                    let step = t.mul(dl, nk)?;
                    if wa > 0.0 {
                        let sa = t.scale(step, wa)?;
                        pos[c.a][k] = t.add(pos[c.a][k], sa)?;
                    }
                    if wb > 0.0 {
                        let sb = t.scale(step, wb)?;
                        pos[c.b][k] = t.sub(pos[c.b][k], sb)?;
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
        pos,
        vel,
    })
}

/// Final positions and velocities.
pub fn simulate(scene: &DiffScene) -> Result<(Vec<[f64; 3]>, Vec<[f64; 3]>), DiffError> {
    let r = record(scene)?;
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
    let n = scene.particles.len();
    if seed_pos.len() != n || seed_vel.len() != n {
        return Err(DiffError::SeedLength);
    }
    if !seed_pos.iter().chain(seed_vel).flatten().all(|x| x.is_finite()) {
        return Err(DiffError::InvalidScene("seeds must be finite"));
    }
    let r = record(scene)?;
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
            .map(|p| [adj[p[0].0 as usize], adj[p[1].0 as usize], adj[p[2].0 as usize]])
            .collect()
    };
    Ok(Gradients {
        loss,
        d_pos0: pick(&r.pos0),
        d_vel0: pick(&r.vel0),
        d_compliance: r.alpha.iter().map(|a| adj[a.0 as usize]).collect(),
    })
}
