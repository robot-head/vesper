//! Strict, bounded external-checker protocol. No event is positive evidence
//! until it belongs to the exact inventory and the complete session terminates.
use super::{
    ControlKind, ControlRecord, Coverage, Digest, Error, ErrorKind, InventoryTotals, Limits,
    PROFILE,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;
use std::mem::size_of;

fn invalid(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::Protocol, message)
}
fn coverage_error(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::Coverage, message)
}
fn limit() -> Error {
    Error::new(ErrorKind::OutputLimit, "protocol metadata/count limit")
}
fn add(a: usize, b: usize) -> Result<usize, Error> {
    a.checked_add(b).ok_or_else(limit)
}
fn mul(a: usize, b: usize) -> Result<usize, Error> {
    a.checked_mul(b).ok_or_else(limit)
}
fn sum(values: &[u64]) -> Result<u64, Error> {
    values
        .iter()
        .try_fold(0u64, |a, n| a.checked_add(*n).ok_or_else(limit))
}
fn reserve<T>(v: &mut Vec<T>, n: usize) -> Result<(), Error> {
    v.try_reserve_exact(n)
        .map_err(|_| Error::new(ErrorKind::Allocation, "protocol allocation"))
}
fn text(s: &str) -> Result<String, Error> {
    let mut out = String::new();
    out.try_reserve_exact(s.len())
        .map_err(|_| Error::new(ErrorKind::Allocation, "protocol string"))?;
    out.push_str(s);
    Ok(out)
}
fn hash_text(hash: &mut blake3::Hasher, s: &str) {
    hash.update(&(s.len() as u32).to_le_bytes());
    hash.update(s.as_bytes());
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Component {
    String(String),
    Number(u64),
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Name {
    pub components: Vec<Component>,
}
impl Name {
    pub fn first(&self) -> Option<&str> {
        match self.components.first() {
            Some(Component::String(s)) => Some(s),
            _ => None,
        }
    }
    pub fn is(&self, parts: &[&str]) -> bool {
        self.components.len() == parts.len()
            && self
                .components
                .iter()
                .zip(parts)
                .all(|(a, b)| matches!(a,Component::String(s) if s.as_str()==*b))
    }
    pub fn strings(&self) -> Option<Vec<&str>> {
        self.components
            .iter()
            .map(|c| match c {
                Component::String(s) => Some(s.as_str()),
                Component::Number(_) => None,
            })
            .collect()
    }
    pub fn hash(&self, hash: &mut blake3::Hasher) {
        hash.update(&(self.components.len() as u16).to_le_bytes());
        for part in &self.components {
            match part {
                Component::String(s) => {
                    hash.update(&[0]);
                    hash_text(hash, s);
                }
                Component::Number(n) => {
                    hash.update(&[1]);
                    hash.update(&n.to_le_bytes());
                }
            }
        }
    }
    fn encoded_len(&self) -> Result<usize, Error> {
        let mut bytes = 2;
        for c in &self.components {
            bytes = add(
                bytes,
                match c {
                    Component::String(s) => add(5, s.len())?,
                    Component::Number(_) => 9,
                },
            )?;
        }
        Ok(bytes)
    }
    fn retained_bytes(&self) -> Result<usize, Error> {
        let mut bytes = mul(self.components.capacity(), size_of::<Component>())?;
        for c in &self.components {
            if let Component::String(s) = c {
                bytes = add(bytes, s.capacity())?;
            }
        }
        Ok(bytes)
    }
    fn key(&self) -> Result<Vec<u8>, Error> {
        let mut out = Vec::new();
        reserve(&mut out, self.encoded_len()?)?;
        out.extend_from_slice(&(self.components.len() as u16).to_le_bytes());
        for c in &self.components {
            match c {
                Component::String(s) => {
                    out.push(0);
                    out.extend_from_slice(&(s.len() as u32).to_le_bytes());
                    out.extend_from_slice(s.as_bytes());
                }
                Component::Number(n) => {
                    out.push(1);
                    out.extend_from_slice(&n.to_le_bytes());
                }
            }
        }
        Ok(out)
    }
    fn validate(&self) -> Result<(), Error> {
        if self.components.is_empty()
            || self.components.len() > 128
            || self.encoded_len()? > 4096
            || self
                .components
                .iter()
                .any(|c| matches!(c,Component::String(s) if s.is_empty()||s.len()>1024))
        {
            return Err(invalid("name limits"));
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModuleRecord {
    pub id: u32,
    pub name: Name,
    pub path: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Constant {
    pub id: u32,
    pub name: Name,
    pub module_id: u32,
    pub form: u8,
    pub skip: u8,
    pub owners: Vec<u32>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Inventory {
    pub target: u8,
    pub modules: Vec<ModuleRecord>,
    pub constants: Vec<Constant>,
    pub totals: InventoryTotals,
    pub blake3: Digest,
}
impl Inventory {
    pub fn retained_bytes(&self) -> Result<usize, Error> {
        let mut bytes = add(
            size_of::<Self>(),
            mul(self.modules.capacity(), size_of::<ModuleRecord>())?,
        )?;
        bytes = add(
            bytes,
            mul(self.constants.capacity(), size_of::<Constant>())?,
        )?;
        for m in &self.modules {
            bytes = add(bytes, add(m.name.retained_bytes()?, m.path.capacity())?)?;
        }
        for c in &self.constants {
            bytes = add(
                bytes,
                add(
                    c.name.retained_bytes()?,
                    mul(c.owners.capacity(), size_of::<u32>())?,
                )?,
            )?;
        }
        Ok(bytes)
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedControl {
    pub id: String,
    pub source: String,
    pub kind: ControlKind,
    pub expected_accept: bool,
    pub expected_reason: String,
}
#[derive(Debug, Clone)]
pub struct SessionResult {
    pub inventory: Option<Inventory>,
    pub coverage: Option<Coverage>,
    pub controls: Vec<ControlRecord>,
    pub inventory_blake3: Option<Digest>,
    pub coverage_blake3: Option<Digest>,
    pub controls_blake3: Option<Digest>,
}

pub fn controls_hash(controls: &[ControlRecord]) -> Digest {
    let mut hash = blake3::Hasher::new();
    hash.update(b"vesper.lean4lean42.controls\0");
    hash.update(&1u16.to_le_bytes());
    hash.update(&(controls.len() as u32).to_le_bytes());
    for (i, c) in controls.iter().enumerate() {
        hash.update(&(i as u32).to_le_bytes());
        hash_text(&mut hash, &c.id);
        hash.update(&[
            match c.kind {
                ControlKind::RawDeclaration => 0,
                ControlKind::CheckerExpr => 1,
                ControlKind::Condition => 2,
            },
            u8::from(c.expected_accept),
            u8::from(c.actual_accept),
        ]);
        hash_text(&mut hash, &c.expected_reason);
        hash_text(&mut hash, &c.actual_reason);
        hash.update(&[u8::from(c.matched)]);
    }
    *hash.finalize().as_bytes()
}

struct Budget {
    used: usize,
    cap: usize,
}
impl Budget {
    fn charge(&mut self, n: usize) -> Result<(), Error> {
        self.used = add(self.used, n)?;
        if self.used > self.cap {
            return Err(limit());
        }
        Ok(())
    }
}
fn arr(value: &Value, n: usize) -> Result<&[Value], Error> {
    let a = value.as_array().ok_or_else(|| invalid("array expected"))?;
    if a.len() != n {
        return Err(invalid("array arity"));
    }
    Ok(a)
}
fn uint(v: &Value) -> Result<u64, Error> {
    v.as_u64()
        .ok_or_else(|| invalid("unsigned integer expected"))
}
fn id(v: &Value) -> Result<u32, Error> {
    u32::try_from(uint(v)?).map_err(|_| invalid("ordinal range"))
}
fn string(v: &Value, cap: usize) -> Result<&str, Error> {
    let s = v.as_str().ok_or_else(|| invalid("string expected"))?;
    if s.len() > cap {
        return Err(limit());
    }
    Ok(s)
}
fn boolean(v: &Value) -> Result<bool, Error> {
    match uint(v)? {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(invalid("boolean control")),
    }
}
fn counts<const N: usize>(v: &Value) -> Result<[u64; N], Error> {
    let a = arr(v, N)?;
    let mut out = [0; N];
    for (slot, value) in out.iter_mut().zip(a) {
        *slot = uint(value)?;
    }
    Ok(out)
}

/// Only arrays, normal escaped UTF8 strings and unsigned decimal controls.
/// The scan bounds parser recursion and excludes number forms serde could normalize.
fn decode(bytes: &[u8], cap: usize) -> Result<Value, Error> {
    if bytes.is_empty() || bytes.len() > cap {
        return Err(limit());
    }
    std::str::from_utf8(bytes).map_err(|_| invalid("UTF8"))?;
    let (mut i, mut depth) = (0, 0usize);
    while i < bytes.len() {
        match bytes[i] {
            b'[' => {
                depth += 1;
                if depth > 8 {
                    return Err(limit());
                }
                i += 1;
            }
            b']' => {
                depth = depth
                    .checked_sub(1)
                    .ok_or_else(|| invalid("array nesting"))?;
                i += 1;
            }
            b'"' => {
                i += 1;
                let mut closed = false;
                while i < bytes.len() {
                    match bytes[i] {
                        b'"' => {
                            i += 1;
                            closed = true;
                            break;
                        }
                        b'\\' => {
                            i += 2;
                            if i > bytes.len() {
                                return Err(invalid("string escape"));
                            }
                        }
                        0..=31 => return Err(invalid("string control")),
                        _ => i += 1,
                    }
                }
                if !closed {
                    return Err(invalid("unterminated string"));
                }
            }
            b'0'..=b'9' => {
                let start = i;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
                if i - start > 20 {
                    return Err(limit());
                }
                if i < bytes.len()
                    && !matches!(bytes[i], b',' | b']' | b' ' | b'\r' | b'\n' | b'\t')
                {
                    return Err(invalid("nondecimal control"));
                }
            }
            b',' | b' ' | b'\r' | b'\n' | b'\t' => i += 1,
            _ => return Err(invalid("unsupported JSON form")),
        }
    }
    if depth != 0 {
        return Err(invalid("array nesting"));
    }
    serde_json::from_slice(bytes).map_err(|e| invalid(e.to_string()))
}
fn parse_name(v: &Value, budget: &mut Budget) -> Result<Name, Error> {
    let a = v.as_array().ok_or_else(|| invalid("name array"))?;
    if a.is_empty() || a.len() > 128 {
        return Err(limit());
    }
    let mut encoded = 2usize;
    let mut retained = mul(a.len(), size_of::<Component>())?;
    for component in a {
        let pair = arr(component, 2)?;
        match uint(&pair[0])? {
            0 => {
                let s = string(&pair[1], 1024)?;
                if s.is_empty() {
                    return Err(invalid("empty name component"));
                }
                encoded = add(encoded, add(5, s.len())?)?;
                retained = add(retained, s.len())?;
            }
            1 => {
                let s = string(&pair[1], 20)?;
                canonical_u64(s)?;
                encoded = add(encoded, 9)?;
            }
            _ => return Err(invalid("name component tag")),
        }
    }
    if encoded > 4096 {
        return Err(limit());
    }
    budget.charge(retained)?;
    let mut components = Vec::new();
    reserve(&mut components, a.len())?;
    for component in a {
        let pair = arr(component, 2)?;
        components.push(if uint(&pair[0])? == 0 {
            Component::String(text(string(&pair[1], 1024)?)?)
        } else {
            Component::Number(canonical_u64(string(&pair[1], 20)?)?)
        });
    }
    Ok(Name { components })
}
fn canonical_u64(s: &str) -> Result<u64, Error> {
    if s.is_empty()
        || s.len() > 20
        || !s.bytes().all(|b| b.is_ascii_digit())
        || (s.len() > 1 && s.starts_with('0'))
    {
        return Err(invalid("canonical numeric name component"));
    }
    s.parse().map_err(|_| invalid("numeric name range"))
}
fn owner_ids(v: &Value, n: usize, budget: &mut Budget) -> Result<Vec<u32>, Error> {
    let a = v.as_array().ok_or_else(|| invalid("owner IDs array"))?;
    if a.len() > 64 {
        return Err(limit());
    }
    budget.charge(mul(a.len(), size_of::<u32>())?)?;
    let mut out = Vec::new();
    reserve(&mut out, a.len())?;
    for value in a {
        let next = id(value)?;
        if next as usize >= n || out.last().is_some_and(|prior| *prior >= next) {
            return Err(invalid("owner IDs ordered/range"));
        }
        out.push(next);
    }
    Ok(out)
}

const ROOTS: [&str; 33] = [
    "r01_add_zero",
    "r02_zero_add",
    "r03_sub_zero",
    "r04_mul_one",
    "r05_one_mul",
    "r06_mul_zero",
    "r07_zero_mul",
    "r08_self_sub",
    "r09_wadd_zero",
    "r10_wsub_zero",
    "r11_wmul_one",
    "r12_wadd_comm",
    "r13_wmul_comm",
    "r14_wadd_assoc",
    "r15_wmul_assoc",
    "r16_wadd_cancel",
    "r17_wmul_zero",
    "r18_wself_sub",
    "r19_checked_add_constants",
    "r20_checked_sub_constants",
    "r21_checked_mul_constants",
    "r22_wrapping_add_constants",
    "r23_wrapping_sub_constants",
    "r24_wrapping_mul_constants",
    "value_fits",
    "literal_exact",
    "value_toInt_injective",
    "literal_roundtrip",
    "total_const",
    "total_var",
    "total_of_eq",
    "congr_checked",
    "congr_wrapping",
];
fn target_module(target: u8) -> &'static [&'static str] {
    match target {
        0 => &["RewriteWitness"],
        1 => &["ArithmeticOracle"],
        _ => &["Lean4Lean", "VesperControls"],
    }
}
fn root_ids(inventory: &Inventory) -> Result<Vec<u32>, Error> {
    let mut roots = Vec::new();
    reserve(&mut roots, 37)?;
    if inventory.target <= 1 {
        for root in ROOTS {
            let c = inventory
                .constants
                .iter()
                .find(|c| c.name.is(&["Vesper", "RewriteWitness", root]))
                .ok_or_else(|| coverage_error("missing required rewrite root"))?;
            if c.skip != 0
                || c.form != 2
                || !inventory.modules[c.module_id as usize]
                    .name
                    .is(&["RewriteWitness"])
            {
                return Err(coverage_error(
                    "required rewrite root form/origin/eligibility",
                ));
            }
            roots.push(c.id);
        }
        if inventory.target == 1 {
            for root in ["checkExpr", "checkProgram", "evalSteps", "processRequest"] {
                let c = inventory
                    .constants
                    .iter()
                    .find(|c| c.name.is(&["Vesper", "ArithmeticOracle", root]))
                    .ok_or_else(|| coverage_error("missing required arithmetic root"))?;
                if c.skip != 0
                    || c.form != 1
                    || !inventory.modules[c.module_id as usize]
                        .name
                        .is(&["ArithmeticOracle"])
                {
                    return Err(coverage_error(
                        "required arithmetic root form/origin/eligibility",
                    ));
                }
                roots.push(c.id);
            }
        }
    } else {
        let c = inventory
            .constants
            .iter()
            .find(|c| c.name.is(&["Vesper", "Lean4Lean42Controls", "runControls"]))
            .ok_or_else(|| coverage_error("missing control baseline root"))?;
        if c.skip != 0
            || c.form != 1
            || !inventory.modules[c.module_id as usize]
                .name
                .is(target_module(2))
        {
            return Err(coverage_error("control baseline root binding"));
        }
        roots.push(c.id);
    }
    Ok(roots)
}

fn validate_owners(inventory: &Inventory) -> Result<(), Error> {
    let constants = &inventory.constants;
    for c in constants {
        match c.form {
            0..=3 => {
                if !c.owners.is_empty() {
                    return Err(coverage_error("scalar owns a group"));
                }
            }
            4 => {
                if c.owners.is_empty() || !c.owners.contains(&c.id) {
                    return Err(coverage_error("inductive group membership"));
                }
                for member in &c.owners {
                    let other = &constants[*member as usize];
                    if other.form != 4 || other.owners != c.owners || other.skip != c.skip {
                        return Err(coverage_error("mixed inductive group"));
                    }
                }
            }
            5 => {
                if c.owners.len() != 1
                    || constants[c.owners[0] as usize].form != 4
                    || constants[c.owners[0] as usize].skip != c.skip
                {
                    return Err(coverage_error("constructor owner"));
                }
            }
            6 => {
                if c.owners.is_empty() {
                    return Err(coverage_error("recursor owners"));
                }
                for owner in &c.owners {
                    let other = &constants[*owner as usize];
                    if other.form != 4 || other.owners != c.owners || other.skip != c.skip {
                        return Err(coverage_error("recursor group"));
                    }
                }
            }
            7 => {
                if c.owners.len() != 4 || c.skip != 0 {
                    return Err(coverage_error("quotient group"));
                }
                for name in [
                    &["Quot"][..],
                    &["Quot", "mk"],
                    &["Quot", "lift"],
                    &["Quot", "ind"],
                ] {
                    let member = c
                        .owners
                        .iter()
                        .map(|i| &constants[*i as usize])
                        .find(|member| member.name.is(name))
                        .ok_or_else(|| coverage_error("quotient names"))?;
                    if member.form != 7 || member.owners != c.owners || member.skip != 0 {
                        return Err(coverage_error("quotient metadata"));
                    }
                }
            }
            _ => return Err(invalid("constant form")),
        }
    }
    Ok(())
}
fn validate_inventory(inv: &Inventory, limits: &Limits, budget: &mut Budget) -> Result<(), Error> {
    if inv.target > 2
        || inv.modules.is_empty()
        || inv.modules.len() > limits.declarations
        || inv.constants.len() > limits.declarations
    {
        return Err(invalid("expected inventory limits"));
    }
    let mut modules = BTreeSet::new();
    let mut constants = BTreeSet::new();
    let mut totals = totals_zero(inv.modules.len() as u64);
    let mut hash = inventory_hasher(
        inv.target,
        inv.modules.len() as u64,
        inv.constants.len() as u64,
    );
    for (index, m) in inv.modules.iter().enumerate() {
        m.name.validate()?;
        if m.id as usize != index || m.path.is_empty() || m.path.len() > 4096 {
            return Err(invalid("expected module binding"));
        }
        budget.charge(add(m.name.encoded_len()?, 128)?)?;
        if !modules.insert(m.name.key()?) {
            return Err(invalid("expected duplicate module"));
        }
        hash_module(&mut hash, m);
    }
    for (index, c) in inv.constants.iter().enumerate() {
        c.name.validate()?;
        if c.id as usize != index
            || c.module_id as usize >= inv.modules.len()
            || c.form > 7
            || c.skip > 3
            || c.owners.len() > 64
            || c.owners.windows(2).any(|p| p[0] >= p[1])
            || c.owners
                .iter()
                .any(|id| *id as usize >= inv.constants.len())
        {
            return Err(invalid("expected constant metadata"));
        }
        budget.charge(add(c.name.encoded_len()?, 128)?)?;
        if !constants.insert(c.name.key()?) {
            return Err(invalid("expected duplicate constant"));
        }
        totals.forms[c.form as usize] += 1;
        totals.eligibility[c.skip as usize] += 1;
        if inv.modules[c.module_id as usize]
            .name
            .is(target_module(inv.target))
        {
            totals.target_eligibility[c.skip as usize] += 1;
        }
        totals.encoded_name_bytes = totals
            .encoded_name_bytes
            .checked_add(c.name.encoded_len()? as u64)
            .ok_or_else(limit)?;
        hash_constant(&mut hash, c);
    }
    if totals != inv.totals
        || totals.encoded_name_bytes > limits.name_bytes as u64
        || totals.target_eligibility[0] == 0
        || *hash.finalize().as_bytes() != inv.blake3
    {
        return Err(coverage_error("expected inventory integrity"));
    }
    validate_owners(inv)?;
    budget.charge(37 * size_of::<u32>())?;
    root_ids(inv)?;
    Ok(())
}
fn zero_coverage() -> Coverage {
    Coverage {
        additions: 0,
        covered: 0,
        constructor_compares: 0,
        recursor_compares: 0,
        skipped: [0; 3],
        target_covered: 0,
        required_roots: 0,
        required_roots_covered: 0,
        remaining: 0,
        pending: 0,
        constructor_pending: 0,
        recursor_pending: 0,
    }
}
fn totals_zero(modules: u64) -> InventoryTotals {
    InventoryTotals {
        modules,
        forms: [0; 8],
        eligibility: [0; 4],
        target_eligibility: [0; 4],
        encoded_name_bytes: 0,
    }
}
fn inventory_hasher(target: u8, modules: u64, constants: u64) -> blake3::Hasher {
    let mut h = blake3::Hasher::new();
    h.update(b"vesper.lean4lean42.inventory\0");
    h.update(&1u16.to_le_bytes());
    h.update(&[target]);
    h.update(&modules.to_le_bytes());
    h.update(&constants.to_le_bytes());
    h
}
fn hash_module(hash: &mut blake3::Hasher, m: &ModuleRecord) {
    hash.update(&m.id.to_le_bytes());
    m.name.hash(hash);
}
fn hash_constant(hash: &mut blake3::Hasher, c: &Constant) {
    hash.update(&c.id.to_le_bytes());
    c.name.hash(hash);
    hash.update(&c.module_id.to_le_bytes());
    hash.update(&[c.form, c.skip]);
    hash.update(&(c.owners.len() as u16).to_le_bytes());
    for i in &c.owners {
        hash.update(&i.to_le_bytes());
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Hello,
    Header,
    Modules,
    Constants,
    InventoryEnd,
    Work,
    ControlsHeader,
    Controls,
    Terminal,
    Done,
}
pub struct Machine<'a> {
    mode: u8,
    target: u8,
    token: String,
    limits: Limits,
    registry: &'a [ExpectedControl],
    expected: Option<&'a Inventory>,
    phase: Phase,
    failed: bool,
    budget: Budget,
    header: Option<InventoryTotals>,
    actual: InventoryTotals,
    modules: Vec<ModuleRecord>,
    constants: Vec<Constant>,
    module_cursor: usize,
    constant_cursor: usize,
    constant_count: usize,
    unique_modules: BTreeSet<Vec<u8>>,
    unique_constants: BTreeSet<Vec<u8>>,
    inventory_hash: Option<blake3::Hasher>,
    inventory_digest: Option<Digest>,
    coverage_hash: Option<blake3::Hasher>,
    coverage: Option<Coverage>,
    bits: Vec<u8>,
    roots: Vec<u32>,
    next_skip: usize,
    event_seq: u64,
    controls: Vec<ControlRecord>,
    control_count: usize,
}

impl<'a> Machine<'a> {
    pub fn new(
        mode: u8,
        target: u8,
        token: &str,
        limits: &Limits,
        controls: &'a [ExpectedControl],
        expected_inventory: Option<&'a Inventory>,
    ) -> Result<Self, Error> {
        limits.validate()?;
        if !matches!((mode, target), (0, 0..=2) | (1, 0..=1) | (2 | 3, 2))
            || token.len() != 32
            || !token
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(invalid("mode/target/token"));
        }
        if (mode == 1) != expected_inventory.is_some() {
            return Err(invalid("expected inventory mode"));
        }
        if mode == 2 && controls.is_empty() || controls.len() > 256 {
            return Err(Error::new(ErrorKind::Controls, "control registry count"));
        }
        for (i, c) in controls.iter().enumerate() {
            if c.id.is_empty()
                || c.id.len() > 128
                || !c.id.bytes().all(|b| (32..=126).contains(&b))
                || c.expected_reason.len() > 128
                || !c.expected_reason.bytes().all(|b| (32..=126).contains(&b))
                || c.source != "tools/lean4lean42/controls/VesperControls.lean"
                || controls[..i].iter().any(|prior| prior.id == c.id)
            {
                return Err(Error::new(ErrorKind::Controls, "control registry schema"));
            }
        }
        let mut budget = Budget {
            used: 0,
            cap: limits.inventory_bytes,
        };
        budget.charge(add(size_of::<Self>(), 32)?)?;
        if let Some(expected) = expected_inventory {
            budget.charge(expected.retained_bytes()?)?;
            if expected.target != target
                || expected.constants.len() > limits.declarations
                || expected.modules.len() > limits.declarations
            {
                return Err(invalid("expected inventory target/cap"));
            }
            validate_inventory(expected, limits, &mut budget)?;
        }
        Ok(Self {
            mode,
            target,
            token: text(token)?,
            limits: limits.clone(),
            registry: controls,
            expected: expected_inventory,
            phase: Phase::Hello,
            failed: false,
            budget,
            header: None,
            actual: totals_zero(0),
            modules: Vec::new(),
            constants: Vec::new(),
            module_cursor: 0,
            constant_cursor: 0,
            constant_count: 0,
            unique_modules: BTreeSet::new(),
            unique_constants: BTreeSet::new(),
            inventory_hash: None,
            inventory_digest: None,
            coverage_hash: None,
            coverage: None,
            bits: Vec::new(),
            roots: Vec::new(),
            next_skip: 0,
            event_seq: 0,
            controls: Vec::new(),
            control_count: 0,
        })
    }
    pub fn coverage(&self) -> Option<&Coverage> {
        self.coverage.as_ref()
    }
    pub fn progress(&self) -> Option<Coverage> {
        self.coverage.clone()
    }
    pub fn inventory_totals(&self) -> Option<&InventoryTotals> {
        self.header.as_ref()
    }
    pub fn inventory_blake3(&self) -> Option<Digest> {
        self.inventory_digest
    }
    pub fn feed(&mut self, bytes: &[u8]) -> Result<(), Error> {
        if self.failed {
            return Err(invalid("failed session"));
        }
        let result = self.feed_inner(bytes);
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    fn inventory(&self) -> InventoryRef<'_> {
        if let Some(expected) = self.expected {
            InventoryRef {
                modules: &expected.modules,
                constants: &expected.constants,
            }
        } else {
            InventoryRef {
                modules: &self.modules,
                constants: &self.constants,
            }
        }
    }
    fn feed_inner(&mut self, bytes: &[u8]) -> Result<(), Error> {
        let value = decode(bytes, self.limits.frame_bytes)?;
        let a = value.as_array().ok_or_else(|| invalid("message array"))?;
        if a.len() < 2 || uint(&a[0])? != 1 {
            return Err(invalid("schema"));
        }
        let tag = uint(&a[1])?;
        if self.phase == Phase::Done {
            return Err(invalid("message after done"));
        }
        if tag == 10 {
            let a = arr(&value, 4)?;
            let category = string(&a[2], 32)?;
            let detail = string(&a[3], 4096)?;
            let kind = match category {
                "checker" => ErrorKind::ExternalRejection,
                "coverage" => ErrorKind::Coverage,
                "limit" => ErrorKind::OutputLimit,
                "import" => ErrorKind::ToolIdentity,
                "controls" => ErrorKind::Controls,
                "io" => ErrorKind::Io,
                "internal" => ErrorKind::Protocol,
                _ => return Err(invalid("error category")),
            };
            return Err(Error::new(kind, detail));
        }
        match (self.phase, tag) {
            (Phase::Hello, 0) => {
                let a = arr(&value, 7)?;
                if string(&a[2], 32)? != self.token
                    || uint(&a[3])? != u64::from(self.mode)
                    || uint(&a[4])? != u64::from(self.target)
                    || string(&a[5], 32)? != "4.24.0"
                    || string(&a[6], 64)? != PROFILE
                {
                    return Err(invalid("hello binding"));
                }
                self.phase = match self.mode {
                    0 | 1 => Phase::Header,
                    2 => Phase::ControlsHeader,
                    _ => Phase::Terminal,
                };
            }
            (Phase::Header, 1) => self.header(a)?,
            (Phase::Modules, 2) => self.module(a)?,
            (Phase::Constants, 3) => self.constant(a)?,
            (Phase::InventoryEnd, 4) => {
                arr(&value, 2)?;
                self.inventory_end()?;
            }
            (Phase::Work, 6) => self.skipped(a)?,
            (Phase::Work, 5) => self.cover(a)?,
            (Phase::ControlsHeader, 7) => {
                arr(&value, 3)?;
                let n = uint(&a[2])? as usize;
                if n != self.registry.len() {
                    return Err(Error::new(
                        ErrorKind::Controls,
                        "control count differs from registry",
                    ));
                }
                self.budget.charge(mul(n, size_of::<ControlRecord>())?)?;
                reserve(&mut self.controls, n)?;
                self.control_count = n;
                self.phase = Phase::Controls;
            }
            (Phase::Controls, 8) => self.control(a)?,
            (Phase::Terminal | Phase::Work, 9) => self.done(a)?,
            _ => return Err(invalid("message phase/tag")),
        }
        Ok(())
    }
    fn header(&mut self, a: &[Value]) -> Result<(), Error> {
        if a.len() != 7 {
            return Err(invalid("inventory header arity"));
        }
        let modules = uint(&a[2])?;
        let forms = counts::<8>(&a[3])?;
        let eligibility = counts::<4>(&a[4])?;
        let target_eligibility = counts::<4>(&a[5])?;
        let encoded_name_bytes = uint(&a[6])?;
        let n = sum(&forms)?;
        if modules == 0
            || modules > self.limits.declarations as u64
            || n > self.limits.declarations as u64
            || n != sum(&eligibility)?
            || sum(&target_eligibility)? > n
            || target_eligibility[0] == 0
            || encoded_name_bytes > self.limits.name_bytes as u64
        {
            return Err(limit());
        }
        let header = InventoryTotals {
            modules,
            forms,
            eligibility,
            target_eligibility,
            encoded_name_bytes,
        };
        if self.expected.is_some_and(|e| e.totals != header) {
            return Err(coverage_error("native/replay inventory totals differ"));
        }
        if self.expected.is_none() {
            self.budget.charge(add(
                mul(modules as usize, size_of::<ModuleRecord>())?,
                mul(n as usize, size_of::<Constant>())?,
            )?)?;
            reserve(&mut self.modules, modules as usize)?;
            reserve(&mut self.constants, n as usize)?;
        }
        self.constant_count = n as usize;
        self.inventory_hash = Some(inventory_hasher(self.target, modules, n));
        self.actual = totals_zero(modules);
        self.header = Some(header);
        self.phase = Phase::Modules;
        Ok(())
    }
    fn module(&mut self, a: &[Value]) -> Result<(), Error> {
        if a.len() != 5 || id(&a[2])? as usize != self.module_cursor {
            return Err(invalid("module record ID/arity"));
        }
        let mut temporary = Budget {
            used: 0,
            cap: self.limits.frame_bytes * 8,
        };
        let budget = if self.expected.is_some() {
            &mut temporary
        } else {
            &mut self.budget
        };
        let name = parse_name(&a[3], budget)?;
        let path = string(&a[4], 4096)?;
        if path.is_empty() {
            return Err(invalid("module path"));
        }
        budget.charge(path.len())?;
        let module = ModuleRecord {
            id: self.module_cursor as u32,
            name,
            path: text(path)?,
        };
        if let Some(expected) = self.expected {
            if expected.modules.get(self.module_cursor) != Some(&module) {
                return Err(coverage_error("native/replay module differs"));
            }
        } else {
            self.budget.charge(add(module.name.encoded_len()?, 128)?)?;
            let key = module.name.key()?;
            if !self.unique_modules.insert(key) {
                return Err(invalid("duplicate module name"));
            }
        }
        hash_module(
            self.inventory_hash
                .as_mut()
                .ok_or_else(|| invalid("inventory hasher"))?,
            &module,
        );
        if self.expected.is_none() {
            self.modules.push(module);
        }
        self.module_cursor += 1;
        if self.module_cursor
            == self
                .header
                .as_ref()
                .ok_or_else(|| invalid("header missing"))?
                .modules as usize
        {
            self.phase = if self.constant_count == 0 {
                Phase::InventoryEnd
            } else {
                Phase::Constants
            };
        }
        Ok(())
    }
    fn constant(&mut self, a: &[Value]) -> Result<(), Error> {
        if a.len() != 8 || id(&a[2])? as usize != self.constant_cursor {
            return Err(invalid("constant record ID/arity"));
        }
        let mut temporary = Budget {
            used: 0,
            cap: self.limits.frame_bytes * 8,
        };
        let budget = if self.expected.is_some() {
            &mut temporary
        } else {
            &mut self.budget
        };
        let name = parse_name(&a[3], budget)?;
        let module_id = id(&a[4])?;
        let form = uint(&a[5])?;
        let skip = uint(&a[6])?;
        if module_id as usize >= self.module_cursor || form > 7 || skip > 3 {
            return Err(invalid("constant tag/module"));
        }
        let owners = owner_ids(&a[7], self.constant_count, budget)?;
        if form <= 3 && !owners.is_empty() {
            return Err(coverage_error("scalar owners"));
        }
        let c = Constant {
            id: self.constant_cursor as u32,
            name,
            module_id,
            form: form as u8,
            skip: skip as u8,
            owners,
        };
        if let Some(expected) = self.expected {
            if expected.constants.get(self.constant_cursor) != Some(&c) {
                return Err(coverage_error("native/replay constant differs"));
            }
        } else {
            self.budget.charge(add(c.name.encoded_len()?, 128)?)?;
            let key = c.name.key()?;
            if !self.unique_constants.insert(key) {
                return Err(invalid("duplicate constant name"));
            }
        }
        self.actual.forms[c.form as usize] += 1;
        self.actual.eligibility[c.skip as usize] += 1;
        let inventory = self.inventory();
        if inventory.modules[c.module_id as usize]
            .name
            .is(target_module(self.target))
        {
            self.actual.target_eligibility[c.skip as usize] += 1;
        }
        self.actual.encoded_name_bytes = self
            .actual
            .encoded_name_bytes
            .checked_add(c.name.encoded_len()? as u64)
            .ok_or_else(limit)?;
        if self.actual.encoded_name_bytes > self.limits.name_bytes as u64 {
            return Err(limit());
        }
        hash_constant(
            self.inventory_hash
                .as_mut()
                .ok_or_else(|| invalid("inventory hasher"))?,
            &c,
        );
        if self.expected.is_none() {
            self.constants.push(c);
        }
        self.constant_cursor += 1;
        if self.constant_cursor == self.constant_count {
            self.phase = Phase::InventoryEnd;
        }
        Ok(())
    }
    fn inventory_end(&mut self) -> Result<(), Error> {
        if self.header.as_ref() != Some(&self.actual) {
            return Err(coverage_error("recomputed inventory totals differ"));
        }
        let digest = *self
            .inventory_hash
            .as_ref()
            .ok_or_else(|| invalid("inventory hash"))?
            .finalize()
            .as_bytes();
        if self.expected.is_some_and(|e| e.blake3 != digest) {
            return Err(coverage_error("native/replay inventory hash differs"));
        }
        self.inventory_digest = Some(digest);
        self.budget.charge(37 * size_of::<u32>())?;
        if self.expected.is_none() {
            let view = Inventory {
                target: self.target,
                modules: std::mem::take(&mut self.modules),
                constants: std::mem::take(&mut self.constants),
                totals: self.actual.clone(),
                blake3: digest,
            };
            let checked = (|| {
                validate_owners(&view)?;
                root_ids(&view)
            })();
            self.modules = view.modules;
            self.constants = view.constants;
            self.roots = checked?;
        } else {
            self.roots = root_ids(self.expected.ok_or_else(|| invalid("expected inventory"))?)?;
            validate_owners(self.expected.ok_or_else(|| invalid("expected inventory"))?)?;
        }
        if self.mode == 1 {
            let bytes = self.constant_count.div_ceil(8);
            self.budget.charge(bytes)?;
            reserve(&mut self.bits, bytes)?;
            self.bits.resize(bytes, 0);
            let mut c = zero_coverage();
            c.required_roots = self.roots.len() as u64;
            c.remaining = self.actual.eligibility[0];
            self.coverage = Some(c);
            let mut h = blake3::Hasher::new();
            h.update(b"vesper.lean4lean42.coverage\0");
            h.update(&1u16.to_le_bytes());
            h.update(&[self.target]);
            self.coverage_hash = Some(h);
            self.phase = Phase::Work;
        } else {
            self.phase = Phase::Terminal;
        }
        Ok(())
    }
    fn skipped(&mut self, a: &[Value]) -> Result<(), Error> {
        if a.len() != 4 || self.event_seq != 0 {
            return Err(invalid("skipped phase/arity"));
        }
        let id = id(&a[2])?;
        let skip = uint(&a[3])?;
        let constants = self.inventory().constants;
        let expected = constants
            .iter()
            .skip(self.next_skip)
            .find(|c| c.skip != 0)
            .ok_or_else(|| coverage_error("unexpected skip"))?;
        if id != expected.id || skip != u64::from(expected.skip) {
            return Err(coverage_error("skip order/metadata"));
        }
        self.next_skip = id as usize + 1;
        self.coverage
            .as_mut()
            .ok_or_else(|| invalid("coverage missing"))?
            .skipped[skip as usize - 1] += 1;
        let hash = self
            .coverage_hash
            .as_mut()
            .ok_or_else(|| invalid("coverage hash"))?;
        hash.update(&[6]);
        hash.update(&id.to_le_bytes());
        hash.update(&[skip as u8]);
        Ok(())
    }
    fn covered(&self, id: u32) -> bool {
        self.bits
            .get(id as usize / 8)
            .is_some_and(|byte| byte & (1 << (id % 8)) != 0)
    }
    fn cover(&mut self, a: &[Value]) -> Result<(), Error> {
        if a.len() != 5 || uint(&a[2])? != self.event_seq {
            return Err(coverage_error("coverage sequence/arity"));
        }
        if self.event_seq == 0
            && self
                .inventory()
                .constants
                .iter()
                .skip(self.next_skip)
                .any(|c| c.skip != 0)
        {
            return Err(coverage_error("missing skipped records"));
        }
        let action = uint(&a[3])?;
        if action > 4 {
            return Err(coverage_error("coverage action"));
        }
        let mut temporary = Budget { used: 0, cap: 1024 };
        let ids = owner_ids(&a[4], self.constant_count, &mut temporary)?;
        if ids.is_empty() {
            return Err(coverage_error("empty coverage event"));
        }
        {
            let inv = self.inventory();
            for id in &ids {
                let c = &inv.constants[*id as usize];
                if c.skip != 0 || self.covered(*id) {
                    return Err(coverage_error("skipped/duplicate coverage"));
                }
            }
            let c = &inv.constants[ids[0] as usize];
            match action {
                0 => {
                    if ids.len() != 1 || c.form > 3 {
                        return Err(coverage_error("scalar coverage"));
                    }
                }
                1 => {
                    if c.form != 4 || c.owners != ids {
                        return Err(coverage_error("inductive group event"));
                    }
                    for id in &ids {
                        if inv.constants[*id as usize].form != 4 {
                            return Err(coverage_error("mixed inductive event"));
                        }
                    }
                }
                2 => {
                    if c.form != 7 || c.owners != ids {
                        return Err(coverage_error("quotient event"));
                    }
                    let eq = inv
                        .constants
                        .iter()
                        .find(|c| c.name.is(&["Eq"]))
                        .ok_or_else(|| coverage_error("Eq prerequisite missing"))?;
                    if !self.covered(eq.id) {
                        return Err(coverage_error("quotient before Eq"));
                    }
                }
                3 | 4 => {
                    let form = if action == 3 { 5 } else { 6 };
                    if ids.len() != 1
                        || c.form != form
                        || c.owners.iter().any(|owner| !self.covered(*owner))
                    {
                        return Err(coverage_error(
                            "postponed comparison before exact owner group",
                        ));
                    }
                }
                _ => return Err(coverage_error("action")),
            }
        }
        for id in &ids {
            let c = &self.inventory().constants[*id as usize];
            let target = self.inventory().modules[c.module_id as usize]
                .name
                .is(target_module(self.target));
            self.bits[*id as usize / 8] |= 1 << (*id % 8);
            let required = self.roots.contains(id);
            let coverage = self.coverage.as_mut().ok_or_else(|| invalid("coverage"))?;
            coverage.covered += 1;
            coverage.remaining -= 1;
            if target {
                coverage.target_covered += 1;
            }
            if required {
                coverage.required_roots_covered += 1;
            }
        }
        let coverage = self.coverage.as_mut().ok_or_else(|| invalid("coverage"))?;
        match action {
            0..=2 => coverage.additions += 1,
            3 => coverage.constructor_compares += 1,
            4 => coverage.recursor_compares += 1,
            _ => {}
        }
        let hash = self
            .coverage_hash
            .as_mut()
            .ok_or_else(|| invalid("coverage hash"))?;
        hash.update(&[5]);
        hash.update(&self.event_seq.to_le_bytes());
        hash.update(&[action as u8]);
        hash.update(&(ids.len() as u16).to_le_bytes());
        for id in &ids {
            hash.update(&id.to_le_bytes());
        }
        self.event_seq += 1;
        Ok(())
    }
    fn control(&mut self, a: &[Value]) -> Result<(), Error> {
        if a.len() != 9 || uint(&a[2])? as usize != self.controls.len() {
            return Err(invalid("control ordinal/arity"));
        }
        let index = self.controls.len();
        let expected = self
            .registry
            .get(index)
            .ok_or_else(|| Error::new(ErrorKind::Controls, "extra control"))?;
        let (id, expected_accept, actual_accept, expected_reason, actual_reason, matched) = (
            string(&a[3], 128)?,
            boolean(&a[4])?,
            boolean(&a[5])?,
            string(&a[6], 128)?,
            string(&a[7], 128)?,
            boolean(&a[8])?,
        );
        if id != expected.id
            || expected_accept != expected.expected_accept
            || expected_reason != expected.expected_reason
            || !actual_reason.bytes().all(|b| (32..=126).contains(&b))
            || !matched
            || actual_accept != expected_accept
            || actual_reason != expected_reason
        {
            return Err(Error::new(
                ErrorKind::Controls,
                "control expectation/result mismatch",
            ));
        }
        self.budget.charge(add(
            id.len(),
            add(expected_reason.len(), actual_reason.len())?,
        )?)?;
        self.controls.push(ControlRecord {
            id: text(id)?,
            kind: expected.kind,
            expected_accept,
            actual_accept,
            expected_reason: text(expected_reason)?,
            actual_reason: text(actual_reason)?,
            matched,
        });
        if self.controls.len() == self.control_count {
            self.phase = Phase::Terminal;
        }
        Ok(())
    }
    fn done(&mut self, a: &[Value]) -> Result<(), Error> {
        if a.len() != 19
            || string(&a[2], 32)? != self.token
            || uint(&a[3])? != u64::from(self.mode)
            || uint(&a[4])? != u64::from(self.target)
        {
            return Err(invalid("done envelope"));
        }
        let actual = Coverage {
            additions: uint(&a[5])?,
            covered: uint(&a[6])?,
            constructor_compares: uint(&a[7])?,
            recursor_compares: uint(&a[8])?,
            skipped: [uint(&a[9])?, uint(&a[10])?, uint(&a[11])?],
            target_covered: uint(&a[12])?,
            required_roots: 0,
            required_roots_covered: 0,
            remaining: uint(&a[13])?,
            pending: uint(&a[14])?,
            constructor_pending: uint(&a[15])?,
            recursor_pending: uint(&a[16])?,
        };
        let controls = uint(&a[17])?;
        let matched = uint(&a[18])?;
        if self.mode == 1 {
            let c = self
                .coverage
                .as_ref()
                .ok_or_else(|| invalid("coverage missing"))?;
            let inv = self.inventory();
            let ctor = inv
                .constants
                .iter()
                .filter(|c| c.skip == 0 && c.form == 5)
                .count() as u64;
            let rec = inv
                .constants
                .iter()
                .filter(|c| c.skip == 0 && c.form == 6)
                .count() as u64;
            if c.covered != self.actual.eligibility[0]
                || c.target_covered != self.actual.target_eligibility[0]
                || c.required_roots_covered != c.required_roots
                || c.constructor_compares != ctor
                || c.recursor_compares != rec
                || c.skipped != self.actual.eligibility[1..]
                || c.remaining != 0
                || controls != 0
                || matched != 0
            {
                return Err(coverage_error("incomplete exact coverage"));
            }
            let mut expected = c.clone();
            expected.required_roots = 0;
            expected.required_roots_covered = 0;
            if actual != expected {
                return Err(coverage_error("terminal coverage counters"));
            }
        } else {
            let mut expected = zero_coverage();
            if self.mode == 0 {
                expected.skipped = [
                    self.actual.eligibility[1],
                    self.actual.eligibility[2],
                    self.actual.eligibility[3],
                ];
            }
            if actual != expected {
                return Err(coverage_error("nonreplay work counters"));
            }
            let count = if self.mode == 2 {
                self.registry.len() as u64
            } else {
                0
            };
            if controls != count || matched != count || self.controls.len() as u64 != count {
                return Err(Error::new(ErrorKind::Controls, "terminal controls count"));
            }
        }
        self.phase = Phase::Done;
        Ok(())
    }
    pub fn finish(self) -> Result<SessionResult, Error> {
        if self.failed || self.phase != Phase::Done {
            return Err(invalid("incomplete/failed session"));
        }
        let inventory = if self.mode == 0 {
            Some(Inventory {
                target: self.target,
                modules: self.modules,
                constants: self.constants,
                totals: self.actual,
                blake3: self
                    .inventory_digest
                    .ok_or_else(|| invalid("inventory hash missing"))?,
            })
        } else {
            None
        };
        let controls_blake3 = if self.mode == 2 {
            Some(controls_hash(&self.controls))
        } else {
            None
        };
        Ok(SessionResult {
            inventory,
            coverage: self.coverage,
            controls: self.controls,
            inventory_blake3: self.inventory_digest,
            coverage_blake3: self.coverage_hash.map(|h| *h.finalize().as_bytes()),
            controls_blake3,
        })
    }
}
struct InventoryRef<'a> {
    modules: &'a [ModuleRecord],
    constants: &'a [Constant],
}
