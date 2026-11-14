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
    /// A k-body constraint names no body, more than
    /// [`MAX_CONSTRAINT_BODIES`], or has malformed offsets.
    BadArity { constraint: usize },
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
            Self::BadArity { constraint } => write!(
                f,
                "constraint {constraint} must name 1 to {MAX_CONSTRAINT_BODIES} bodies"
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

    /// Most constraints one call may colour.
    pub fn capacity(&self) -> usize {
        self.colors.len()
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

/// Most bodies one k-body constraint may touch (a tetrahedron's four
/// particles).
pub const MAX_CONSTRAINT_BODIES: usize = 4;

/// Greedy colouring of constraints that touch any number of bodies (1 to
/// [`MAX_CONSTRAINT_BODIES`]): soft-body distance and tetrahedral volume
/// constraints, or joints and contacts given as body lists.
///
/// Constraint `i` touches `bodies[offsets[i]..offsets[i + 1]]`. The visiting
/// order sorts constraints by their smallest body id, then their largest,
/// then their index; each constraint takes the lowest colour that none of
/// its bodies already holds. Two-body constraints visited in the same order
/// get the same colours as [`Colorer`]. `lean/KineticColoring.lean` models
/// this colouring over body lists (`colorAllSets`) and proves that a
/// colour's constraints touch pairwise disjoint bodies; the Rust test
/// compares the Rust colours with the model's on concrete inputs.
#[derive(Debug)]
pub struct MultiColorer {
    body_masks: Vec<u64>,
    order: Vec<u32>,
    keys: Vec<(BodyId, BodyId)>,
    colors: Vec<u8>,
    layer_offsets: Vec<u32>,
    sorted: Vec<u32>,
    layer_count: usize,
}

impl MultiColorer {
    /// A colourer for `body_count` bodies and up to `max_constraints`
    /// constraints per call; [`MultiColorer::color`] does not allocate.
    pub fn new(body_count: usize, max_constraints: usize) -> Self {
        Self {
            body_masks: vec![0; body_count],
            order: Vec::with_capacity(max_constraints),
            keys: vec![(0, 0); max_constraints],
            colors: vec![0; max_constraints],
            layer_offsets: Vec::with_capacity(MAX_COLORS + 1),
            sorted: Vec::with_capacity(max_constraints),
            layer_count: 0,
        }
    }

    /// Colour the constraints given by `bodies` and `offsets`
    /// (`offsets.len()` is the constraint count plus one).
    pub fn color(&mut self, bodies: &[BodyId], offsets: &[u32]) -> Result<(), ColoringError> {
        let count = offsets.len().saturating_sub(1);
        let capacity = self.colors.len();
        if count > capacity || count > u32::MAX as usize {
            return Err(ColoringError::CapacityExceeded {
                constraints: count,
                capacity,
            });
        }
        if offsets.first().is_some_and(|o| *o != 0) {
            return Err(ColoringError::BadArity { constraint: 0 });
        }
        let n = self.body_masks.len();
        for i in 0..count {
            let (lo, hi) = (offsets[i] as usize, offsets[i + 1] as usize);
            if hi <= lo || hi - lo > MAX_CONSTRAINT_BODIES || hi > bodies.len() {
                return Err(ColoringError::BadArity { constraint: i });
            }
            let set = &bodies[lo..hi];
            for (k, &b) in set.iter().enumerate() {
                if b as usize >= n {
                    return Err(ColoringError::BodyOutOfRange {
                        constraint: i,
                        body: b,
                    });
                }
                if set[..k].contains(&b) {
                    return Err(ColoringError::SelfConstraint {
                        constraint: i,
                        body: b,
                    });
                }
            }
            let min = set.iter().copied().min().unwrap_or(0);
            let max = set.iter().copied().max().unwrap_or(0);
            self.keys[i] = (min, max);
        }
        self.body_masks.fill(0);
        self.order.clear();
        self.order.extend(0..count as u32);
        let keys = &self.keys;
        self.order.sort_unstable_by_key(|&i| (keys[i as usize], i));
        let mut used = 0usize;
        for &i in &self.order {
            let set = &bodies[offsets[i as usize] as usize..offsets[i as usize + 1] as usize];
            let busy = set
                .iter()
                .fold(0u64, |m, &b| m | self.body_masks[b as usize]);
            let free = !busy;
            if free == 0 {
                return Err(ColoringError::TooManyColors {
                    constraint: i as usize,
                });
            }
            let color = free.trailing_zeros();
            for &b in set {
                self.body_masks[b as usize] |= 1u64 << color;
            }
            self.colors[i as usize] = color as u8;
            used = used.max(color as usize + 1);
        }
        self.layer_offsets.clear();
        self.sorted.clear();
        for color in 0..used {
            self.layer_offsets.push(self.sorted.len() as u32);
            for &i in &self.order {
                if self.colors[i as usize] as usize == color {
                    self.sorted.push(i);
                }
            }
        }
        self.layer_offsets.push(self.sorted.len() as u32);
        self.layer_count = used;
        Ok(())
    }

    /// Most constraints one call may colour.
    pub fn capacity(&self) -> usize {
        self.colors.len()
    }

    /// Number of colours the last colouring used.
    pub fn layer_count(&self) -> usize {
        self.layer_count
    }

    /// Constraint indices of colour `color`, in visiting order.
    pub fn layer(&self, color: usize) -> &[u32] {
        let start = self.layer_offsets[color] as usize;
        let end = self.layer_offsets[color + 1] as usize;
        &self.sorted[start..end]
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

/// Independent checker for [`MultiColorer`]: every colour's constraints
/// touch pairwise disjoint bodies and every constraint has one colour.
pub fn sets_are_disjoint(colorer: &MultiColorer, bodies: &[BodyId], offsets: &[u32]) -> bool {
    let count = offsets.len().saturating_sub(1);
    let set = |i: u32| &bodies[offsets[i as usize] as usize..offsets[i as usize + 1] as usize];
    let mut seen = vec![false; count];
    for c in 0..colorer.layer_count() {
        let layer = colorer.layer(c);
        for (n, &i) in layer.iter().enumerate() {
            if std::mem::replace(&mut seen[i as usize], true) {
                return false;
            }
            for &j in &layer[n + 1..] {
                if set(i).iter().any(|b| set(j).contains(b)) {
                    return false;
                }
            }
        }
    }
    seen.iter().all(|s| *s)
}
