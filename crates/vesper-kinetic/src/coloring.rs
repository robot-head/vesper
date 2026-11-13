//! Deterministic greedy constraint-graph colouring (§36.1 item 2, §36.4).
//!
//! Constraints are visited in a stable order (sorted by their body ids, ties
//! broken by input index) and each one takes the lowest colour that no
//! earlier constraint sharing one of its bodies holds. Every colour is
//! therefore an island of constraints with pairwise disjoint bodies, so the
//! solver may project a colour's constraints in any order (or in parallel)
//! and still get the same bits. `lean/KineticColoring.lean` proves the
//! disjointness of the modelled greedy colouring; this Rust code is checked
//! against it on concrete inputs, not proved.

/// Index of a body in the world.
pub type BodyId = u32;

/// Most colours a colouring may use (one bit per colour per body).
pub const MAX_COLORS: usize = 64;

/// The bodies a constraint touches: two bodies, or one body against static
/// geometry (`b == None`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConstraintBodies {
    pub a: BodyId,
    pub b: Option<BodyId>,
}

impl ConstraintBodies {
    fn key(self) -> (BodyId, BodyId) {
        match self.b {
            Some(b) if b < self.a => (b, self.a),
            Some(b) => (self.a, b),
            None => (self.a, self.a),
        }
    }

    /// Whether two constraints touch a common body.
    pub fn shares_body(self, other: Self) -> bool {
        let mine = [Some(self.a), self.b];
        let theirs = [Some(other.a), other.b];
        mine.iter()
            .flatten()
            .any(|x| theirs.iter().flatten().any(|y| x == y))
    }
}

/// Typed colouring failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColoringError {
    /// A constraint names a body outside `0..body_count`.
    BodyOutOfRange { constraint: usize, body: BodyId },
    /// A two-body constraint names the same body twice.
    SelfConstraint { constraint: usize, body: BodyId },
    /// A body takes part in more than [`MAX_COLORS`] mutually conflicting
    /// constraints.
    TooManyColors { constraint: usize },
    /// More constraints than the colourer was sized for.
    CapacityExceeded { constraints: usize, capacity: usize },
}

impl std::fmt::Display for ColoringError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BodyOutOfRange { constraint, body } => {
                write!(f, "constraint {constraint} names unknown body {body}")
            }
            Self::SelfConstraint { constraint, body } => {
                write!(f, "constraint {constraint} links body {body} to itself")
            }
            Self::TooManyColors { constraint } => {
                write!(
                    f,
                    "constraint {constraint} needs more than {MAX_COLORS} colours"
                )
            }
            Self::CapacityExceeded {
                constraints,
                capacity,
            } => write!(
                f,
                "{constraints} constraints exceed colourer capacity {capacity}"
            ),
        }
    }
}

impl std::error::Error for ColoringError {}

/// Reusable colourer with preallocated buffers: after construction,
/// [`Colorer::color`] does not allocate.
#[derive(Debug)]
pub struct Colorer {
    body_masks: Vec<u64>,
    order: Vec<u32>,
    colors: Vec<u8>,
    layer_offsets: Vec<u32>,
    sorted: Vec<u32>,
    layer_count: usize,
}

impl Colorer {
    /// A colourer for worlds of `body_count` bodies and up to
    /// `max_constraints` constraints per call.
    pub fn new(body_count: usize, max_constraints: usize) -> Self {
        Self {
            body_masks: vec![0; body_count],
            order: Vec::with_capacity(max_constraints),
            colors: vec![0; max_constraints],
            layer_offsets: Vec::with_capacity(MAX_COLORS + 1),
            sorted: Vec::with_capacity(max_constraints),
            layer_count: 0,
        }
    }

    /// Colour `constraints`. On success, [`Colorer::layers`] yields the colour
    /// islands in colour order, each listing constraint indices in the stable
    /// visiting order.
    pub fn color(&mut self, constraints: &[ConstraintBodies]) -> Result<(), ColoringError> {
        let capacity = self.colors.len();
        if constraints.len() > capacity || constraints.len() > u32::MAX as usize {
            return Err(ColoringError::CapacityExceeded {
                constraints: constraints.len(),
                capacity,
            });
        }
        let bodies = self.body_masks.len();
        for (constraint, c) in constraints.iter().enumerate() {
            for body in [Some(c.a), c.b].into_iter().flatten() {
                if body as usize >= bodies {
                    return Err(ColoringError::BodyOutOfRange { constraint, body });
                }
            }
            if c.b == Some(c.a) {
                return Err(ColoringError::SelfConstraint {
                    constraint,
                    body: c.a,
                });
            }
        }
        self.body_masks.fill(0);
        self.order.clear();
        self.order.extend(0..constraints.len() as u32);
        // Keys are unique (index breaks ties), so an unstable sort is
        // deterministic and allocation-free.
        self.order
            .sort_unstable_by_key(|&i| (constraints[i as usize].key(), i));
        let mut used_colors = 0usize;
        for &i in &self.order {
            let c = constraints[i as usize];
            let mut busy = self.body_masks[c.a as usize];
            if let Some(b) = c.b {
                busy |= self.body_masks[b as usize];
            }
            let free = !busy;
            if free == 0 {
                return Err(ColoringError::TooManyColors {
                    constraint: i as usize,
                });
            }
            let color = free.trailing_zeros();
            let bit = 1u64 << color;
            self.body_masks[c.a as usize] |= bit;
            if let Some(b) = c.b {
                self.body_masks[b as usize] |= bit;
            }
            self.colors[i as usize] = color as u8;
            used_colors = used_colors.max(color as usize + 1);
        }
        // Bucket by colour, keeping visiting order inside each colour.
        self.layer_offsets.clear();
        self.sorted.clear();
        for color in 0..used_colors {
            self.layer_offsets.push(self.sorted.len() as u32);
            for &i in &self.order {
                if self.colors[i as usize] as usize == color {
                    self.sorted.push(i);
                }
            }
        }
        self.layer_offsets.push(self.sorted.len() as u32);
        self.layer_count = used_colors;
        Ok(())
    }

    /// Number of colours the last colouring used.
    pub fn layer_count(&self) -> usize {
        self.layer_count
    }

    /// Constraint indices of colour `color`.
    pub fn layer(&self, color: usize) -> &[u32] {
        let start = self.layer_offsets[color] as usize;
        let end = self.layer_offsets[color + 1] as usize;
        &self.sorted[start..end]
    }

    /// Colour islands in colour order.
    pub fn layers(&self) -> impl Iterator<Item = &[u32]> + '_ {
        (0..self.layer_count).map(|c| self.layer(c))
    }

    /// Colour of constraint `index` in the last colouring.
    pub fn color_of(&self, index: usize) -> u8 {
        self.colors[index]
    }

    /// Constraint indices in the stable visiting order of the last colouring.
    pub fn visiting_order(&self) -> &[u32] {
        &self.order
    }
}

/// Independent checker: every island has pairwise body-disjoint constraints
/// and every constraint appears in exactly one island.
pub fn islands_are_disjoint(colorer: &Colorer, constraints: &[ConstraintBodies]) -> bool {
    let mut seen = vec![false; constraints.len()];
    for layer in colorer.layers() {
        for (n, &i) in layer.iter().enumerate() {
            if std::mem::replace(&mut seen[i as usize], true) {
                return false;
            }
            for &j in &layer[n + 1..] {
                if constraints[i as usize].shares_body(constraints[j as usize]) {
                    return false;
                }
            }
        }
    }
    seen.iter().all(|s| *s)
}
