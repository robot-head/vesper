//! Strict, bounded wire codec for the existing finite resource model.
//! This module performs transport and constructor-shape checks, never inference.
use serde::Serialize;
use serde_json::{Value, json};
use std::fmt;
use std::io::{Read, Write};

pub const REQUEST_CAP: usize = 4_194_304;
pub const RESPONSE_CAP: usize = 262_144;
pub const MAX_REQUESTS: u64 = 4_096;
pub const MAX_SESSION_BYTES: u64 = 67_108_864;
pub const MAX_CONTROL: u64 = 9_999_999_999;
pub const MAX_ROWS: usize = 131_072;
pub const MAX_CODE_ROWS: usize = 2_048;
pub const MAX_INPUTS: usize = 65;
pub const MAX_DEPTH: usize = 256;
pub const MAX_NESTING: usize = 8;
pub const MAX_TOKENS: usize = 1_000_000;
pub const PROTOCOL_ID: &str = "vesper.resource-model42/1";
pub const LEAN_VERSION: &str = "4.24.0";
pub const LEAN_COMMIT: &str = "797c613eb9b6d4ec95db23e3e00af9ac6657f24b";
pub const BOUNDS: [u64; 12] = [
    REQUEST_CAP as u64,
    RESPONSE_CAP as u64,
    8,
    1_000_000,
    131_072,
    2_048,
    65,
    256,
    MAX_CONTROL,
    20,
    MAX_REQUESTS,
    MAX_SESSION_BYTES,
];
pub const DEFAULT_LIMITS: [u64; 14] = [
    256, 64, 64, 255, 16, 255, 16, 4096, 16, 65536, 256, 64, 128, 4096,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum WireErrorKind {
    Io,
    Truncated,
    FrameLength,
    Lexical,
    Quota,
    Schema,
    Decimal,
    Shape,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WireError {
    pub kind: WireErrorKind,
    pub detail: &'static str,
}
impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.detail)
    }
}
impl std::error::Error for WireError {}
fn error(kind: WireErrorKind, detail: &'static str) -> WireError {
    WireError { kind, detail }
}
fn schema() -> WireError {
    error(WireErrorKind::Schema, "invalid wire schema")
}
fn quota() -> WireError {
    error(WireErrorKind::Quota, "transport quota exceeded")
}
fn shape() -> WireError {
    error(WireErrorKind::Shape, "invalid postfix shape")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Natural(String);
impl Natural {
    pub fn parse(s: &str) -> Result<Self, WireError> {
        if s.is_empty()
            || s.len() > 20
            || !s.bytes().all(|b| b.is_ascii_digit())
            || (s.len() > 1 && s.starts_with('0'))
        {
            return Err(error(
                WireErrorKind::Decimal,
                "noncanonical natural decimal",
            ));
        }
        Ok(Self(s.to_owned()))
    }
    pub fn from_u64(n: u64) -> Self {
        Self(n.to_string())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Mode {
    Copy,
    Affine,
    Linear,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Floor {
    Copy,
    Affine,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum TypeRow {
    Unit,
    Nat,
    Ticket,
    Pair {
        floor: Floor,
        left_mode: Mode,
        right_mode: Mode,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum ValueRow {
    Unit,
    Nat(Natural),
    Handle(u64),
    Pair,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum SourceRow {
    Unit {
        site: u64,
    },
    Nat {
        site: u64,
        value: Natural,
    },
    Var {
        site: u64,
        index: u64,
    },
    Pair {
        site: u64,
        floor: Floor,
        left_mode: Mode,
        right_mode: Mode,
    },
    Let {
        site: u64,
    },
    Unpack {
        site: u64,
    },
    Ifz {
        site: u64,
    },
    Mint {
        site: u64,
    },
    Consume {
        site: u64,
    },
}
pub type TypeCode = Vec<TypeRow>;
pub type ValueCode = Vec<ValueRow>;
pub type SourceCode = Vec<SourceRow>;
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Request {
    pub limits: [u64; 14],
    pub input_types: Vec<TypeCode>,
    pub source: SourceCode,
    pub input_values: Vec<ValueCode>,
    pub fuel: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum TypeError {
    Nodes,
    Depth,
    FieldMode,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum ValidationError {
    InvalidLimits,
    InputCount,
    NoncopyInput(u64),
    InputType(u64, TypeError),
    RawNodes,
    RawDepth,
    Context,
    Site(u64),
    DuplicateSite(u64),
    VariableBound(u64),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum CheckErrorKind {
    UnknownVariable,
    TypeMismatch,
    DuplicateUse,
    UnusedLinear,
    FieldModality,
    LiteralOutOfRange,
    BranchOwnership,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CheckError {
    pub kind: CheckErrorKind,
    pub site: u64,
    pub slot: Option<u64>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum CheckProfile {
    TypeBound(u64, TypeError),
    Context(u64),
    AffineBranchDemand(u64, u64),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum InputError {
    Shape,
    NaturalRange,
    Handle,
    Depth,
    Noncopy,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum CapacityError {
    ExpressionNodes,
    RuntimeDepth,
    ValueNodes,
    ValueDepth,
    InitialValues,
    InputCount,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum InvocationError {
    InputCount(u64, u64),
    InputValue(u64, InputError),
    Capacity(CapacityError),
    FuelBound(u64, u64),
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum ResourceRefusal {
    Capacity(CapacityError),
    Resources(u64, u64),
    Events(u64, u64),
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum InvariantError {
    OpenAtom,
    ExpectedPair,
    ExpectedNat,
    ExpectedHandle,
    UnknownHandle(u64),
    DeadHandle(u64),
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourceStats {
    pub nodes: u64,
    pub depth: u64,
    pub max_context: u64,
    pub mints: u64,
    pub consumes: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CheckedInfo {
    pub result_type: TypeCode,
    pub uses: Vec<bool>,
    pub source_stats: SourceStats,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ValueStats {
    pub nodes: u64,
    pub depth: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InvocationStats {
    pub nodes: u64,
    pub depth: u64,
    pub max_value_nodes: u64,
    pub max_value_depth: u64,
    pub value_stats: Option<ValueStats>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum Status {
    Completed(ValueCode),
    FuelExhausted,
    ResourceLimit(ResourceRefusal),
    InvariantFailure(InvariantError),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum EventAction {
    Mint,
    Consume,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Event {
    pub action: EventAction,
    pub site: u64,
    pub id: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResidualOwner {
    pub id: u64,
    pub mint_site: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LedgerEntry {
    pub mint_site: u64,
    pub live: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Evaluation {
    pub checked: CheckedInfo,
    pub invocation: InvocationStats,
    pub status: Status,
    pub events: Vec<Event>,
    pub residual: Vec<ResidualOwner>,
    pub ledger: Vec<LedgerEntry>,
    pub transition_count: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum Outcome {
    ValidationError(ValidationError),
    CheckSemantic {
        error: CheckError,
        source_stats: SourceStats,
    },
    CheckProfile {
        reason: CheckProfile,
        source_stats: SourceStats,
    },
    InvocationError {
        checked: CheckedInfo,
        error: InvocationError,
    },
    EvalAdmissionError {
        checked: CheckedInfo,
        invocation: InvocationStats,
        error: InvocationError,
    },
    Evaluation(Evaluation),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Stage {
    ValidationError,
    CheckSemantic,
    CheckProfile,
    InvocationError,
    EvalAdmissionError,
    Evaluation,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ModelResponse {
    pub sequence: u64,
    pub outcome: Outcome,
}
impl ModelResponse {
    pub fn stage(&self) -> Stage {
        match self.outcome {
            Outcome::ValidationError(_) => Stage::ValidationError,
            Outcome::CheckSemantic { .. } => Stage::CheckSemantic,
            Outcome::CheckProfile { .. } => Stage::CheckProfile,
            Outcome::InvocationError { .. } => Stage::InvocationError,
            Outcome::EvalAdmissionError { .. } => Stage::EvalAdmissionError,
            Outcome::Evaluation(_) => Stage::Evaluation,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Ready {
    pub protocol: String,
    pub lean_version: String,
    pub lean_commit: String,
    pub bounds: [u64; 12],
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Bye {
    pub next_sequence: u64,
    pub counters: [u64; 7],
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum ServerMessage {
    Ready(Ready),
    Result(ModelResponse),
    Bye(Bye),
}

/// Scan the restricted lexical envelope before invoking serde_json.
fn lexical(bytes: &[u8], cap: usize) -> Result<(), WireError> {
    if bytes.is_empty() || bytes.len() > cap {
        return Err(quota());
    }
    let mut i = 0;
    let mut depth = 0usize;
    let mut tokens = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'[' => {
                depth += 1;
                tokens += 1;
                if depth > MAX_NESTING {
                    return Err(quota());
                }
                i += 1;
            }
            b']' => {
                depth = depth.checked_sub(1).ok_or_else(schema)?;
                i += 1;
            }
            b',' | b' ' | b'\t' | b'\r' | b'\n' => i += 1,
            b'"' => {
                tokens += 1;
                i += 1;
                let start = i;
                while i < bytes.len() && bytes[i] != b'"' {
                    if !(0x20..=0x7e).contains(&bytes[i]) || bytes[i] == b'\\' {
                        return Err(error(WireErrorKind::Lexical, "forbidden string byte"));
                    }
                    i += 1;
                    if i - start > 128 {
                        return Err(quota());
                    }
                }
                if i == bytes.len() {
                    return Err(error(WireErrorKind::Lexical, "unterminated string"));
                }
                i += 1;
            }
            b'0'..=b'9' => {
                tokens += 1;
                let start = i;
                let mut n = 0u64;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    n = n
                        .checked_mul(10)
                        .and_then(|n| n.checked_add(u64::from(bytes[i] - b'0')))
                        .ok_or_else(quota)?;
                    if n > MAX_CONTROL {
                        return Err(quota());
                    }
                    i += 1;
                }
                if i - start > 1 && bytes[start] == b'0' {
                    return Err(error(WireErrorKind::Lexical, "noncanonical control"));
                }
            }
            _ => return Err(error(WireErrorKind::Lexical, "forbidden JSON token")),
        }
        if tokens > MAX_TOKENS {
            return Err(quota());
        }
    }
    if depth != 0 {
        return Err(schema());
    }
    Ok(())
}
fn parse(bytes: &[u8], cap: usize) -> Result<Value, WireError> {
    lexical(bytes, cap)?;
    serde_json::from_slice(bytes).map_err(|_| schema())
}
fn array(v: &Value) -> Result<&[Value], WireError> {
    v.as_array().map(Vec::as_slice).ok_or_else(schema)
}
fn fields(v: &Value, len: usize) -> Result<&[Value], WireError> {
    let a = array(v)?;
    if a.len() != len {
        return Err(schema());
    }
    Ok(a)
}
fn number(v: &Value) -> Result<u64, WireError> {
    v.as_u64().filter(|n| *n <= MAX_CONTROL).ok_or_else(schema)
}
fn text(v: &Value) -> Result<&str, WireError> {
    v.as_str().ok_or_else(schema)
}
fn flag(v: &Value) -> Result<bool, WireError> {
    match number(v)? {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(schema()),
    }
}
fn natural(v: &Value) -> Result<Natural, WireError> {
    Natural::parse(text(v)?)
}
fn mode(v: &Value) -> Result<Mode, WireError> {
    match number(v)? {
        0 => Ok(Mode::Copy),
        1 => Ok(Mode::Affine),
        2 => Ok(Mode::Linear),
        _ => Err(schema()),
    }
}
fn floor(v: &Value) -> Result<Floor, WireError> {
    match number(v)? {
        0 => Ok(Floor::Copy),
        1 => Ok(Floor::Affine),
        _ => Err(schema()),
    }
}
fn tuple_numbers<const N: usize>(v: &Value) -> Result<[u64; N], WireError> {
    let a = fields(v, N)?;
    let mut out = [0; N];
    for (n, v) in out.iter_mut().zip(a) {
        *n = number(v)?;
    }
    Ok(out)
}
fn row(v: &Value) -> Result<(u64, &[Value]), WireError> {
    let a = array(v)?;
    let tag = number(a.first().ok_or_else(schema)?)?;
    Ok((tag, a))
}
fn code_rows(v: &Value) -> Result<&[Value], WireError> {
    let a = array(v)?;
    if a.is_empty() || a.len() > MAX_CODE_ROWS {
        return Err(quota());
    }
    Ok(a)
}
fn push_shape(
    stack: &mut Vec<(bool, usize)>,
    arity: usize,
    atoms: usize,
    atom: bool,
) -> Result<(), WireError> {
    if stack.len() < arity {
        return Err(shape());
    }
    let base = stack.len() - arity;
    if stack[base..base + atoms].iter().any(|(atom, _)| !atom) {
        return Err(shape());
    }
    let depth = 1 + stack[base..].iter().map(|(_, d)| *d).max().unwrap_or(0);
    if depth > MAX_DEPTH {
        return Err(quota());
    }
    stack.truncate(base);
    stack.push((atom, depth));
    Ok(())
}
fn final_shape(stack: &[(bool, usize)]) -> Result<(), WireError> {
    if stack.len() == 1 {
        Ok(())
    } else {
        Err(shape())
    }
}
fn type_code(v: &Value) -> Result<TypeCode, WireError> {
    let rows = code_rows(v)?;
    let mut out = Vec::with_capacity(rows.len());
    let mut stack = Vec::new();
    for r in rows {
        let (tag, a) = row(r)?;
        let (r, arity) = match (tag, a.len()) {
            (0, 1) => (TypeRow::Unit, 0),
            (1, 1) => (TypeRow::Nat, 0),
            (2, 1) => (TypeRow::Ticket, 0),
            (3, 4) => (
                TypeRow::Pair {
                    floor: floor(&a[1])?,
                    left_mode: mode(&a[2])?,
                    right_mode: mode(&a[3])?,
                },
                2,
            ),
            _ => return Err(schema()),
        };
        push_shape(&mut stack, arity, 0, false)?;
        out.push(r);
    }
    final_shape(&stack)?;
    Ok(out)
}
fn value_code(v: &Value) -> Result<ValueCode, WireError> {
    let rows = code_rows(v)?;
    let mut out = Vec::with_capacity(rows.len());
    let mut stack = Vec::new();
    for r in rows {
        let (tag, a) = row(r)?;
        let (r, arity) = match (tag, a.len()) {
            (0, 1) => (ValueRow::Unit, 0),
            (1, 2) => (ValueRow::Nat(natural(&a[1])?), 0),
            (2, 2) => (ValueRow::Handle(number(&a[1])?), 0),
            (3, 1) => (ValueRow::Pair, 2),
            _ => return Err(schema()),
        };
        push_shape(&mut stack, arity, 0, false)?;
        out.push(r);
    }
    final_shape(&stack)?;
    Ok(out)
}
fn source_code(v: &Value) -> Result<SourceCode, WireError> {
    let rows = code_rows(v)?;
    let mut out = Vec::with_capacity(rows.len());
    let mut stack = Vec::new();
    for r in rows {
        let (tag, a) = row(r)?;
        if a.len() < 2 {
            return Err(schema());
        }
        let site = number(&a[1])?;
        let (r, arity, atoms, atom) = match (tag, a.len()) {
            (0, 2) => (SourceRow::Unit { site }, 0, 0, true),
            (1, 3) => (
                SourceRow::Nat {
                    site,
                    value: natural(&a[2])?,
                },
                0,
                0,
                true,
            ),
            (2, 3) => (
                SourceRow::Var {
                    site,
                    index: number(&a[2])?,
                },
                0,
                0,
                true,
            ),
            (3, 5) => (
                SourceRow::Pair {
                    site,
                    floor: floor(&a[2])?,
                    left_mode: mode(&a[3])?,
                    right_mode: mode(&a[4])?,
                },
                2,
                2,
                false,
            ),
            (4, 2) => (SourceRow::Let { site }, 2, 0, false),
            (5, 2) => (SourceRow::Unpack { site }, 2, 1, false),
            (6, 2) => (SourceRow::Ifz { site }, 3, 1, false),
            (7, 2) => (SourceRow::Mint { site }, 0, 0, false),
            (8, 2) => (SourceRow::Consume { site }, 1, 1, false),
            _ => return Err(schema()),
        };
        push_shape(&mut stack, arity, atoms, atom)?;
        out.push(r);
    }
    final_shape(&stack)?;
    Ok(out)
}
pub fn decode_request(bytes: &[u8]) -> Result<(u64, Request), WireError> {
    let v = parse(bytes, REQUEST_CAP)?;
    let a = fields(&v, 8)?;
    if number(&a[0])? != 1 || text(&a[1])? != "eval" {
        return Err(schema());
    }
    let types = array(&a[4])?;
    let values = array(&a[6])?;
    if types.len() > MAX_INPUTS || values.len() > MAX_INPUTS {
        return Err(quota());
    }
    // Count all raw rows before any constructor stack is processed.
    let mut count = code_rows(&a[5])?.len();
    for code in types.iter().chain(values) {
        count = count
            .checked_add(code_rows(code)?.len())
            .ok_or_else(quota)?;
    }
    if count > MAX_ROWS {
        return Err(quota());
    }
    Ok((
        number(&a[2])?,
        Request {
            limits: tuple_numbers(&a[3])?,
            input_types: types.iter().map(type_code).collect::<Result<_, _>>()?,
            source: source_code(&a[5])?,
            input_values: values.iter().map(value_code).collect::<Result<_, _>>()?,
            fuel: number(&a[7])?,
        },
    ))
}
fn mode_num(v: Mode) -> u8 {
    match v {
        Mode::Copy => 0,
        Mode::Affine => 1,
        Mode::Linear => 2,
    }
}
fn floor_num(v: Floor) -> u8 {
    match v {
        Floor::Copy => 0,
        Floor::Affine => 1,
    }
}
fn type_json(code: &TypeCode) -> Value {
    Value::Array(
        code.iter()
            .map(|r| match r {
                TypeRow::Unit => json!([0]),
                TypeRow::Nat => json!([1]),
                TypeRow::Ticket => json!([2]),
                TypeRow::Pair {
                    floor,
                    left_mode,
                    right_mode,
                } => json!([
                    3,
                    floor_num(*floor),
                    mode_num(*left_mode),
                    mode_num(*right_mode)
                ]),
            })
            .collect(),
    )
}
fn value_json(code: &ValueCode) -> Value {
    Value::Array(
        code.iter()
            .map(|r| match r {
                ValueRow::Unit => json!([0]),
                ValueRow::Nat(n) => json!([1, n.as_str()]),
                ValueRow::Handle(id) => json!([2, id]),
                ValueRow::Pair => json!([3]),
            })
            .collect(),
    )
}
fn source_json(code: &SourceCode) -> Value {
    Value::Array(
        code.iter()
            .map(|r| match r {
                SourceRow::Unit { site } => json!([0, site]),
                SourceRow::Nat { site, value } => json!([1, site, value.as_str()]),
                SourceRow::Var { site, index } => json!([2, site, index]),
                SourceRow::Pair {
                    site,
                    floor,
                    left_mode,
                    right_mode,
                } => json!([
                    3,
                    site,
                    floor_num(*floor),
                    mode_num(*left_mode),
                    mode_num(*right_mode)
                ]),
                SourceRow::Let { site } => json!([4, site]),
                SourceRow::Unpack { site } => json!([5, site]),
                SourceRow::Ifz { site } => json!([6, site]),
                SourceRow::Mint { site } => json!([7, site]),
                SourceRow::Consume { site } => json!([8, site]),
            })
            .collect(),
    )
}
pub fn encode_request(sequence: u64, request: &Request) -> Result<Vec<u8>, WireError> {
    if request.input_types.len() > MAX_INPUTS || request.input_values.len() > MAX_INPUTS {
        return Err(quota());
    }
    let lengths = std::iter::once(request.source.len())
        .chain(request.input_types.iter().map(Vec::len))
        .chain(request.input_values.iter().map(Vec::len));
    let mut total = 0usize;
    for len in lengths {
        if len == 0 || len > MAX_CODE_ROWS {
            return Err(quota());
        }
        total = total.checked_add(len).ok_or_else(quota)?;
    }
    if total > MAX_ROWS {
        return Err(quota());
    }
    let v = json!([
        1,
        "eval",
        sequence,
        request.limits,
        request
            .input_types
            .iter()
            .map(type_json)
            .collect::<Vec<_>>(),
        source_json(&request.source),
        request
            .input_values
            .iter()
            .map(value_json)
            .collect::<Vec<_>>(),
        request.fuel
    ]);
    let bytes = serde_json::to_vec(&v).map_err(|_| schema())?;
    decode_request(&bytes)?;
    Ok(bytes)
}

pub const CONTROL_CAP: u64 = MAX_CONTROL;
pub const BYE_FRAME_RESERVE: u64 = 512;
fn named(v: &Value) -> Result<(&str, &[Value]), WireError> {
    let a = array(v)?;
    Ok((text(a.first().ok_or_else(schema)?)?, a))
}
macro_rules! bare_enum_codec {
    ($decode:ident, $encode:ident, $ty:ident, {$($variant:ident => $name:literal),+ $(,)?}) => {
        fn $decode(v: &Value) -> Result<$ty, WireError> {
            let (name, a) = named(v)?; if a.len() != 1 { return Err(schema()); }
            match name { $($name => Ok($ty::$variant),)+ _ => Err(schema()) }
        }
        fn $encode(v: &$ty) -> Value { json!([match v { $($ty::$variant => $name,)+ }]) }
    };
}
bare_enum_codec!(type_error, type_error_json, TypeError, { Nodes => "nodes", Depth => "depth", FieldMode => "fieldMode" });
bare_enum_codec!(input_error, input_error_json, InputError, { Shape => "shape", NaturalRange => "naturalRange", Handle => "handle", Depth => "depth", Noncopy => "noncopy" });
bare_enum_codec!(capacity_error, capacity_error_json, CapacityError, { ExpressionNodes => "expressionNodes", RuntimeDepth => "runtimeDepth", ValueNodes => "valueNodes", ValueDepth => "valueDepth", InitialValues => "initialValues", InputCount => "inputCount" });
fn validation_error(v: &Value) -> Result<ValidationError, WireError> {
    let (name, a) = named(v)?;
    Ok(match (name, a.len()) {
        ("invalidLimits", 1) => ValidationError::InvalidLimits,
        ("inputCount", 1) => ValidationError::InputCount,
        ("noncopyInput", 2) => ValidationError::NoncopyInput(number(&a[1])?),
        ("inputType", 3) => ValidationError::InputType(number(&a[1])?, type_error(&a[2])?),
        ("rawNodes", 1) => ValidationError::RawNodes,
        ("rawDepth", 1) => ValidationError::RawDepth,
        ("context", 1) => ValidationError::Context,
        ("site", 2) => ValidationError::Site(number(&a[1])?),
        ("duplicateSite", 2) => ValidationError::DuplicateSite(number(&a[1])?),
        ("variableBound", 2) => ValidationError::VariableBound(number(&a[1])?),
        _ => return Err(schema()),
    })
}
fn validation_json(v: &ValidationError) -> Value {
    match v {
        ValidationError::InvalidLimits => json!(["invalidLimits"]),
        ValidationError::InputCount => json!(["inputCount"]),
        ValidationError::NoncopyInput(i) => json!(["noncopyInput", i]),
        ValidationError::InputType(i, e) => json!(["inputType", i, type_error_json(e)]),
        ValidationError::RawNodes => json!(["rawNodes"]),
        ValidationError::RawDepth => json!(["rawDepth"]),
        ValidationError::Context => json!(["context"]),
        ValidationError::Site(s) => json!(["site", s]),
        ValidationError::DuplicateSite(s) => json!(["duplicateSite", s]),
        ValidationError::VariableBound(s) => json!(["variableBound", s]),
    }
}
fn check_kind(name: &str) -> Result<CheckErrorKind, WireError> {
    Ok(match name {
        "unknownVariable" => CheckErrorKind::UnknownVariable,
        "typeMismatch" => CheckErrorKind::TypeMismatch,
        "duplicateUse" => CheckErrorKind::DuplicateUse,
        "unusedLinear" => CheckErrorKind::UnusedLinear,
        "fieldModality" => CheckErrorKind::FieldModality,
        "literalOutOfRange" => CheckErrorKind::LiteralOutOfRange,
        "branchOwnership" => CheckErrorKind::BranchOwnership,
        _ => return Err(schema()),
    })
}
fn check_error(v: &Value) -> Result<CheckError, WireError> {
    let a = fields(v, 3)?;
    let slot = match array(&a[2])? {
        [] => None,
        [v] => Some(number(v)?),
        _ => return Err(schema()),
    };
    Ok(CheckError {
        kind: check_kind(text(&a[0])?)?,
        site: number(&a[1])?,
        slot,
    })
}
fn check_json(v: &CheckError) -> Value {
    let name = match v.kind {
        CheckErrorKind::UnknownVariable => "unknownVariable",
        CheckErrorKind::TypeMismatch => "typeMismatch",
        CheckErrorKind::DuplicateUse => "duplicateUse",
        CheckErrorKind::UnusedLinear => "unusedLinear",
        CheckErrorKind::FieldModality => "fieldModality",
        CheckErrorKind::LiteralOutOfRange => "literalOutOfRange",
        CheckErrorKind::BranchOwnership => "branchOwnership",
    };
    json!([name, v.site, v.slot.into_iter().collect::<Vec<_>>()])
}
fn check_profile(v: &Value) -> Result<CheckProfile, WireError> {
    let (name, a) = named(v)?;
    Ok(match (name, a.len()) {
        ("typeBound", 3) => CheckProfile::TypeBound(number(&a[1])?, type_error(&a[2])?),
        ("context", 2) => CheckProfile::Context(number(&a[1])?),
        ("affineBranchDemand", 3) => {
            CheckProfile::AffineBranchDemand(number(&a[1])?, number(&a[2])?)
        }
        _ => return Err(schema()),
    })
}
fn profile_json(v: &CheckProfile) -> Value {
    match v {
        CheckProfile::TypeBound(s, e) => json!(["typeBound", s, type_error_json(e)]),
        CheckProfile::Context(s) => json!(["context", s]),
        CheckProfile::AffineBranchDemand(s, i) => json!(["affineBranchDemand", s, i]),
    }
}
fn invocation_error(v: &Value) -> Result<InvocationError, WireError> {
    let (name, a) = named(v)?;
    Ok(match (name, a.len()) {
        ("inputCount", 3) => InvocationError::InputCount(number(&a[1])?, number(&a[2])?),
        ("inputValue", 3) => InvocationError::InputValue(number(&a[1])?, input_error(&a[2])?),
        ("capacity", 2) => InvocationError::Capacity(capacity_error(&a[1])?),
        ("fuelBound", 3) => InvocationError::FuelBound(number(&a[1])?, number(&a[2])?),
        _ => return Err(schema()),
    })
}
fn invocation_json(v: &InvocationError) -> Value {
    match v {
        InvocationError::InputCount(a, b) => json!(["inputCount", a, b]),
        InvocationError::InputValue(i, e) => json!(["inputValue", i, input_error_json(e)]),
        InvocationError::Capacity(e) => json!(["capacity", capacity_error_json(e)]),
        InvocationError::FuelBound(a, b) => json!(["fuelBound", a, b]),
    }
}
fn refusal(v: &Value) -> Result<ResourceRefusal, WireError> {
    let (name, a) = named(v)?;
    Ok(match (name, a.len()) {
        ("capacity", 2) => ResourceRefusal::Capacity(capacity_error(&a[1])?),
        ("resources", 3) => ResourceRefusal::Resources(number(&a[1])?, number(&a[2])?),
        ("events", 3) => ResourceRefusal::Events(number(&a[1])?, number(&a[2])?),
        _ => return Err(schema()),
    })
}
fn refusal_json(v: &ResourceRefusal) -> Value {
    match v {
        ResourceRefusal::Capacity(e) => json!(["capacity", capacity_error_json(e)]),
        ResourceRefusal::Resources(a, b) => json!(["resources", a, b]),
        ResourceRefusal::Events(a, b) => json!(["events", a, b]),
    }
}
fn invariant(v: &Value) -> Result<InvariantError, WireError> {
    let (name, a) = named(v)?;
    Ok(match (name, a.len()) {
        ("openAtom", 1) => InvariantError::OpenAtom,
        ("expectedPair", 1) => InvariantError::ExpectedPair,
        ("expectedNat", 1) => InvariantError::ExpectedNat,
        ("expectedHandle", 1) => InvariantError::ExpectedHandle,
        ("unknownHandle", 2) => InvariantError::UnknownHandle(number(&a[1])?),
        ("deadHandle", 2) => InvariantError::DeadHandle(number(&a[1])?),
        _ => return Err(schema()),
    })
}
fn invariant_json(v: &InvariantError) -> Value {
    match v {
        InvariantError::OpenAtom => json!(["openAtom"]),
        InvariantError::ExpectedPair => json!(["expectedPair"]),
        InvariantError::ExpectedNat => json!(["expectedNat"]),
        InvariantError::ExpectedHandle => json!(["expectedHandle"]),
        InvariantError::UnknownHandle(i) => json!(["unknownHandle", i]),
        InvariantError::DeadHandle(i) => json!(["deadHandle", i]),
    }
}
fn source_stats(v: &Value) -> Result<SourceStats, WireError> {
    let [nodes, depth, max_context, mints, consumes] = tuple_numbers(v)?;
    Ok(SourceStats {
        nodes,
        depth,
        max_context,
        mints,
        consumes,
    })
}
fn source_stats_json(v: &SourceStats) -> Value {
    json!([v.nodes, v.depth, v.max_context, v.mints, v.consumes])
}
fn checked(v: &Value) -> Result<CheckedInfo, WireError> {
    let a = fields(v, 3)?;
    let uses = array(&a[1])?;
    if uses.len() > MAX_INPUTS {
        return Err(quota());
    }
    Ok(CheckedInfo {
        result_type: type_code(&a[0])?,
        uses: uses.iter().map(flag).collect::<Result<_, _>>()?,
        source_stats: source_stats(&a[2])?,
    })
}
fn checked_json(v: &CheckedInfo) -> Value {
    json!([
        type_json(&v.result_type),
        v.uses.iter().map(|b| u8::from(*b)).collect::<Vec<_>>(),
        source_stats_json(&v.source_stats)
    ])
}
fn invocation_stats(v: &Value) -> Result<InvocationStats, WireError> {
    let a = fields(v, 5)?;
    let value_stats = match array(&a[4])? {
        [] => None,
        [n, d] => Some(ValueStats {
            nodes: number(n)?,
            depth: number(d)?,
        }),
        _ => return Err(schema()),
    };
    Ok(InvocationStats {
        nodes: number(&a[0])?,
        depth: number(&a[1])?,
        max_value_nodes: number(&a[2])?,
        max_value_depth: number(&a[3])?,
        value_stats,
    })
}
fn invocation_stats_json(v: &InvocationStats) -> Value {
    let value = match &v.value_stats {
        None => json!([]),
        Some(v) => json!([v.nodes, v.depth]),
    };
    json!([
        v.nodes,
        v.depth,
        v.max_value_nodes,
        v.max_value_depth,
        value
    ])
}
fn status(v: &Value) -> Result<Status, WireError> {
    let (name, a) = named(v)?;
    Ok(match (name, a.len()) {
        ("completed", 2) => Status::Completed(value_code(&a[1])?),
        ("fuelExhausted", 1) => Status::FuelExhausted,
        ("resourceLimit", 2) => Status::ResourceLimit(refusal(&a[1])?),
        ("invariantFailure", 2) => Status::InvariantFailure(invariant(&a[1])?),
        _ => return Err(schema()),
    })
}
fn status_json(v: &Status) -> Value {
    match v {
        Status::Completed(code) => json!(["completed", value_json(code)]),
        Status::FuelExhausted => json!(["fuelExhausted"]),
        Status::ResourceLimit(r) => json!(["resourceLimit", refusal_json(r)]),
        Status::InvariantFailure(e) => json!(["invariantFailure", invariant_json(e)]),
    }
}
fn result(sequence: u64, stage: &str, v: &Value) -> Result<ModelResponse, WireError> {
    let a = array(v)?;
    let outcome = match (stage, a.len()) {
        ("validation_error", 1) => Outcome::ValidationError(validation_error(&a[0])?),
        ("check_semantic", 2) => Outcome::CheckSemantic {
            error: check_error(&a[0])?,
            source_stats: source_stats(&a[1])?,
        },
        ("check_profile", 2) => Outcome::CheckProfile {
            reason: check_profile(&a[0])?,
            source_stats: source_stats(&a[1])?,
        },
        ("invocation_error", 2) => Outcome::InvocationError {
            checked: checked(&a[0])?,
            error: invocation_error(&a[1])?,
        },
        ("eval_admission_error", 3) => Outcome::EvalAdmissionError {
            checked: checked(&a[0])?,
            invocation: invocation_stats(&a[1])?,
            error: invocation_error(&a[2])?,
        },
        ("evaluation", 7) => {
            let events = array(&a[3])?
                .iter()
                .map(|v| {
                    let r = fields(v, 3)?;
                    Ok(Event {
                        action: match number(&r[0])? {
                            0 => EventAction::Mint,
                            1 => EventAction::Consume,
                            _ => return Err(schema()),
                        },
                        site: number(&r[1])?,
                        id: number(&r[2])?,
                    })
                })
                .collect::<Result<_, WireError>>()?;
            let residual = array(&a[4])?
                .iter()
                .map(|v| {
                    let [id, mint_site] = tuple_numbers(v)?;
                    Ok(ResidualOwner { id, mint_site })
                })
                .collect::<Result<_, WireError>>()?;
            let ledger = array(&a[5])?
                .iter()
                .map(|v| {
                    let r = fields(v, 2)?;
                    Ok(LedgerEntry {
                        mint_site: number(&r[0])?,
                        live: flag(&r[1])?,
                    })
                })
                .collect::<Result<_, WireError>>()?;
            Outcome::Evaluation(Evaluation {
                checked: checked(&a[0])?,
                invocation: invocation_stats(&a[1])?,
                status: status(&a[2])?,
                events,
                residual,
                ledger,
                transition_count: number(&a[6])?,
            })
        }
        _ => return Err(schema()),
    };
    Ok(ModelResponse { sequence, outcome })
}
fn result_json(v: &ModelResponse) -> Value {
    let (stage, payload) = match &v.outcome {
        Outcome::ValidationError(e) => ("validation_error", json!([validation_json(e)])),
        Outcome::CheckSemantic {
            error,
            source_stats,
        } => (
            "check_semantic",
            json!([check_json(error), source_stats_json(source_stats)]),
        ),
        Outcome::CheckProfile {
            reason,
            source_stats,
        } => (
            "check_profile",
            json!([profile_json(reason), source_stats_json(source_stats)]),
        ),
        Outcome::InvocationError { checked, error } => (
            "invocation_error",
            json!([checked_json(checked), invocation_json(error)]),
        ),
        Outcome::EvalAdmissionError {
            checked,
            invocation,
            error,
        } => (
            "eval_admission_error",
            json!([
                checked_json(checked),
                invocation_stats_json(invocation),
                invocation_json(error)
            ]),
        ),
        Outcome::Evaluation(e) => (
            "evaluation",
            json!([
                checked_json(&e.checked),
                invocation_stats_json(&e.invocation),
                status_json(&e.status),
                e.events
                    .iter()
                    .map(|v| json!([
                        match v.action {
                            EventAction::Mint => 0,
                            EventAction::Consume => 1,
                        },
                        v.site,
                        v.id
                    ]))
                    .collect::<Vec<_>>(),
                e.residual
                    .iter()
                    .map(|v| json!([v.id, v.mint_site]))
                    .collect::<Vec<_>>(),
                e.ledger
                    .iter()
                    .map(|v| json!([v.mint_site, u8::from(v.live)]))
                    .collect::<Vec<_>>(),
                e.transition_count
            ]),
        ),
    };
    json!([1, "result", v.sequence, stage, payload])
}
pub fn decode_message(bytes: &[u8]) -> Result<ServerMessage, WireError> {
    let v = parse(bytes, RESPONSE_CAP)?;
    let a = array(&v)?;
    if a.len() < 2 || number(&a[0])? != 1 {
        return Err(schema());
    }
    match (text(&a[1])?, a.len()) {
        ("ready", 6) => {
            let ready = Ready {
                protocol: text(&a[2])?.to_owned(),
                lean_version: text(&a[3])?.to_owned(),
                lean_commit: text(&a[4])?.to_owned(),
                bounds: tuple_numbers(&a[5])?,
            };
            if ready.protocol != PROTOCOL_ID
                || ready.lean_version != LEAN_VERSION
                || ready.lean_commit != LEAN_COMMIT
                || ready.bounds != BOUNDS
            {
                return Err(schema());
            }
            Ok(ServerMessage::Ready(ready))
        }
        ("result", 5) => Ok(ServerMessage::Result(result(
            number(&a[2])?,
            text(&a[3])?,
            &a[4],
        )?)),
        ("bye", 4) => {
            let bye = Bye {
                next_sequence: number(&a[2])?,
                counters: tuple_numbers(&a[3])?,
            };
            let partition = bye.counters[1..]
                .iter()
                .try_fold(0u64, |a, b| a.checked_add(*b))
                .ok_or_else(schema)?;
            if bye.counters[0] != partition
                || bye.next_sequence != bye.counters[0]
                || bye.next_sequence > MAX_REQUESTS
            {
                return Err(schema());
            }
            Ok(ServerMessage::Bye(bye))
        }
        _ => Err(schema()),
    }
}
/// Encode the exact server wire schema (useful for transport controls and codec tests).
fn checked_lengths(checked: &CheckedInfo) -> Result<(), WireError> {
    if checked.result_type.is_empty()
        || checked.result_type.len() > MAX_CODE_ROWS
        || checked.uses.len() > MAX_INPUTS
    {
        return Err(quota());
    }
    Ok(())
}
pub fn encode_message(message: &ServerMessage) -> Result<Vec<u8>, WireError> {
    // Bound owned DTO collections before constructing the intermediate JSON value.
    match message {
        ServerMessage::Ready(r)
            if r.protocol.len() > 128
                || r.lean_version.len() > 128
                || r.lean_commit.len() > 128 =>
        {
            return Err(quota());
        }
        ServerMessage::Result(r) => match &r.outcome {
            Outcome::InvocationError { checked, .. }
            | Outcome::EvalAdmissionError { checked, .. } => checked_lengths(checked)?,
            Outcome::Evaluation(e) => {
                checked_lengths(&e.checked)?;
                if e.events.len() > RESPONSE_CAP / 3
                    || e.residual.len() > RESPONSE_CAP / 3
                    || e.ledger.len() > RESPONSE_CAP / 3
                {
                    return Err(quota());
                }
                if let Status::Completed(code) = &e.status
                    && (code.is_empty() || code.len() > MAX_CODE_ROWS)
                {
                    return Err(quota());
                }
            }
            _ => {}
        },
        _ => {}
    }
    let v = match message {
        ServerMessage::Ready(r) => json!([
            1,
            "ready",
            r.protocol,
            r.lean_version,
            r.lean_commit,
            r.bounds
        ]),
        ServerMessage::Result(r) => result_json(r),
        ServerMessage::Bye(b) => json!([1, "bye", b.next_sequence, b.counters]),
    };
    let bytes = serde_json::to_vec(&v).map_err(|_| schema())?;
    decode_message(&bytes)?;
    Ok(bytes)
}

/// Boundary EOF is normal. Any partial header/payload is a fatal wire error.
pub fn read_frame(reader: &mut impl Read, cap: usize) -> Result<Option<Vec<u8>>, WireError> {
    let mut header = [0; 4];
    let mut read = 0;
    while read < 4 {
        match reader.read(&mut header[read..]) {
            Ok(0) if read == 0 => return Ok(None),
            Ok(0) => return Err(error(WireErrorKind::Truncated, "partial frame header")),
            Ok(n) => read += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return Err(error(WireErrorKind::Io, "frame read failed")),
        }
    }
    let len = u32::from_le_bytes(header) as usize;
    if len == 0 || len > cap {
        return Err(error(WireErrorKind::FrameLength, "invalid frame length"));
    }
    let mut payload = vec![0; len];
    let mut read = 0;
    while read < len {
        match reader.read(&mut payload[read..]) {
            Ok(0) => return Err(error(WireErrorKind::Truncated, "partial frame payload")),
            Ok(n) => read += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return Err(error(WireErrorKind::Io, "frame read failed")),
        }
    }
    Ok(Some(payload))
}
pub fn write_frame(writer: &mut impl Write, payload: &[u8], cap: usize) -> Result<(), WireError> {
    let len = u32::try_from(payload.len())
        .map_err(|_| error(WireErrorKind::FrameLength, "invalid frame length"))?;
    if len == 0 || payload.len() > cap {
        return Err(error(WireErrorKind::FrameLength, "invalid frame length"));
    }
    writer
        .write_all(&len.to_le_bytes())
        .and_then(|()| writer.write_all(payload))
        .and_then(|()| writer.flush())
        .map_err(|_| error(WireErrorKind::Io, "frame write failed"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{self, Cursor};

    fn request() -> Request {
        Request {
            limits: DEFAULT_LIMITS,
            input_types: vec![],
            source: vec![SourceRow::Unit { site: 0 }],
            input_values: vec![],
            fuel: 0,
        }
    }
    fn stats() -> SourceStats {
        SourceStats {
            nodes: 1,
            depth: 1,
            max_context: 0,
            mints: 0,
            consumes: 0,
        }
    }
    fn checked_info() -> CheckedInfo {
        CheckedInfo {
            result_type: vec![TypeRow::Unit],
            uses: vec![],
            source_stats: stats(),
        }
    }
    fn inv_stats() -> InvocationStats {
        InvocationStats {
            nodes: 1,
            depth: 1,
            max_value_nodes: 1,
            max_value_depth: 1,
            value_stats: Some(ValueStats { nodes: 1, depth: 1 }),
        }
    }
    fn roundtrip(outcome: Outcome) {
        // DTO codec fixture only: no worker, model prediction or execution claim.
        let message = ServerMessage::Result(ModelResponse {
            sequence: 4,
            outcome,
        });
        assert_eq!(
            decode_message(&encode_message(&message).unwrap()).unwrap(),
            message
        );
    }
    fn evaluation(status: Status) -> Outcome {
        Outcome::Evaluation(Evaluation {
            checked: checked_info(),
            invocation: inv_stats(),
            status,
            events: vec![
                Event {
                    action: EventAction::Mint,
                    site: 2,
                    id: 0,
                },
                Event {
                    action: EventAction::Consume,
                    site: 3,
                    id: 0,
                },
            ],
            residual: vec![ResidualOwner {
                id: 1,
                mint_site: 4,
            }],
            ledger: vec![
                LedgerEntry {
                    mint_site: 2,
                    live: false,
                },
                LedgerEntry {
                    mint_site: 4,
                    live: true,
                },
            ],
            transition_count: 7,
        })
    }
    #[test]
    fn canonical_naturals_preserve_semantic_range_errors() {
        for s in [
            "0",
            "18446744073709551615",
            "18446744073709551616",
            "99999999999999999999",
        ] {
            assert_eq!(Natural::parse(s).unwrap().as_str(), s);
        }
        for s in [
            "",
            "00",
            "01",
            "+1",
            "-1",
            "1.0",
            "1e2",
            " 1",
            "١",
            "100000000000000000000",
        ] {
            assert!(Natural::parse(s).is_err(), "{s}");
        }
        assert_eq!(Natural::from_u64(u64::MAX).as_str(), "18446744073709551615");
    }
    #[test]
    fn request_codec_all_rows_and_semantic_negatives() {
        let mut r = request();
        r.limits = [MAX_CONTROL; 14];
        r.limits[0] = 0;
        r.input_types = vec![
            vec![
                TypeRow::Unit,
                TypeRow::Nat,
                TypeRow::Pair {
                    floor: Floor::Affine,
                    left_mode: Mode::Linear,
                    right_mode: Mode::Copy,
                },
            ],
            vec![TypeRow::Ticket],
        ];
        r.input_values = vec![
            vec![
                ValueRow::Unit,
                ValueRow::Nat(Natural::parse("18446744073709551616").unwrap()),
                ValueRow::Pair,
            ],
            vec![ValueRow::Handle(MAX_CONTROL)],
        ];
        r.source = vec![
            SourceRow::Nat {
                site: MAX_CONTROL,
                value: Natural::parse("99999999999999999999").unwrap(),
            },
            SourceRow::Var {
                site: 0,
                index: MAX_CONTROL,
            },
            SourceRow::Pair {
                site: 0,
                floor: Floor::Copy,
                left_mode: Mode::Affine,
                right_mode: Mode::Linear,
            },
        ];
        assert_eq!(
            decode_request(&encode_request(9, &r).unwrap()).unwrap(),
            (9, r.clone())
        );
        let cases = vec![
            vec![
                SourceRow::Unit { site: 0 },
                SourceRow::Mint { site: 1 },
                SourceRow::Let { site: 2 },
            ],
            vec![
                SourceRow::Var { site: 0, index: 0 },
                SourceRow::Mint { site: 1 },
                SourceRow::Unpack { site: 2 },
            ],
            vec![
                SourceRow::Unit { site: 0 },
                SourceRow::Mint { site: 1 },
                SourceRow::Unit { site: 2 },
                SourceRow::Ifz { site: 3 },
            ],
            vec![
                SourceRow::Var { site: 0, index: 0 },
                SourceRow::Consume { site: 1 },
            ],
        ];
        for source in cases {
            r.source = source;
            assert_eq!(
                decode_request(&encode_request(0, &r).unwrap()).unwrap().1,
                r
            );
        }
        r.input_values.clear(); // Mismatched input counts belong to the actual model.
        assert!(encode_request(0, &r).is_ok());
    }
    #[test]
    fn every_diagnostic_constructor_roundtrips() {
        for t in [TypeError::Nodes, TypeError::Depth, TypeError::FieldMode] {
            roundtrip(Outcome::ValidationError(ValidationError::InputType(3, t)));
            roundtrip(Outcome::CheckProfile {
                reason: CheckProfile::TypeBound(7, t),
                source_stats: stats(),
            });
        }
        for e in [
            ValidationError::InvalidLimits,
            ValidationError::InputCount,
            ValidationError::NoncopyInput(2),
            ValidationError::RawNodes,
            ValidationError::RawDepth,
            ValidationError::Context,
            ValidationError::Site(8),
            ValidationError::DuplicateSite(4),
            ValidationError::VariableBound(6),
        ] {
            roundtrip(Outcome::ValidationError(e));
        }
        for kind in [
            CheckErrorKind::UnknownVariable,
            CheckErrorKind::TypeMismatch,
            CheckErrorKind::DuplicateUse,
            CheckErrorKind::UnusedLinear,
            CheckErrorKind::FieldModality,
            CheckErrorKind::LiteralOutOfRange,
            CheckErrorKind::BranchOwnership,
        ] {
            for slot in [None, Some(9)] {
                roundtrip(Outcome::CheckSemantic {
                    error: CheckError {
                        kind,
                        site: 4,
                        slot,
                    },
                    source_stats: stats(),
                });
            }
        }
        for reason in [
            CheckProfile::Context(2),
            CheckProfile::AffineBranchDemand(3, 5),
        ] {
            roundtrip(Outcome::CheckProfile {
                reason,
                source_stats: stats(),
            });
        }
        for reason in [
            InputError::Shape,
            InputError::NaturalRange,
            InputError::Handle,
            InputError::Depth,
            InputError::Noncopy,
        ] {
            roundtrip(Outcome::InvocationError {
                checked: checked_info(),
                error: InvocationError::InputValue(2, reason),
            });
        }
        for c in [
            CapacityError::ExpressionNodes,
            CapacityError::RuntimeDepth,
            CapacityError::ValueNodes,
            CapacityError::ValueDepth,
            CapacityError::InitialValues,
            CapacityError::InputCount,
        ] {
            roundtrip(Outcome::InvocationError {
                checked: checked_info(),
                error: InvocationError::Capacity(c),
            });
            roundtrip(evaluation(Status::ResourceLimit(
                ResourceRefusal::Capacity(c),
            )));
        }
        roundtrip(Outcome::InvocationError {
            checked: checked_info(),
            error: InvocationError::InputCount(2, 1),
        });
        roundtrip(Outcome::EvalAdmissionError {
            checked: checked_info(),
            invocation: inv_stats(),
            error: InvocationError::FuelBound(4, 3),
        });
        for r in [
            ResourceRefusal::Resources(5, 4),
            ResourceRefusal::Events(8, 7),
        ] {
            roundtrip(evaluation(Status::ResourceLimit(r)));
        }
        for e in [
            InvariantError::OpenAtom,
            InvariantError::ExpectedPair,
            InvariantError::ExpectedNat,
            InvariantError::ExpectedHandle,
            InvariantError::UnknownHandle(8),
            InvariantError::DeadHandle(9),
        ] {
            roundtrip(evaluation(Status::InvariantFailure(e)));
        }
        roundtrip(evaluation(Status::FuelExhausted));
        // Completed-value syntax alone; this does not assert model success.
        for code in [
            vec![ValueRow::Unit],
            vec![ValueRow::Nat(Natural::from_u64(2))],
            vec![ValueRow::Handle(4)],
            vec![ValueRow::Unit, ValueRow::Handle(0), ValueRow::Pair],
        ] {
            let s = Status::Completed(code);
            assert_eq!(status(&status_json(&s)).unwrap(), s);
        }
        let mut s = inv_stats();
        s.value_stats = None;
        assert_eq!(invocation_stats(&invocation_stats_json(&s)).unwrap(), s);
    }
    #[test]
    fn exact_ready_bye_and_stage() {
        let ready = ServerMessage::Ready(Ready {
            protocol: PROTOCOL_ID.into(),
            lean_version: LEAN_VERSION.into(),
            lean_commit: LEAN_COMMIT.into(),
            bounds: BOUNDS,
        });
        assert_eq!(
            decode_message(&encode_message(&ready).unwrap()).unwrap(),
            ready
        );
        let bye = ServerMessage::Bye(Bye {
            next_sequence: 6,
            counters: [6, 1, 1, 1, 1, 1, 1],
        });
        let bytes = encode_message(&bye).unwrap();
        assert!(bytes.len() + 4 <= BYE_FRAME_RESERVE as usize);
        assert_eq!(decode_message(&bytes).unwrap(), bye);
        assert!(decode_message(br#"[1,"bye",1,[1,0,0,0,0,0,0]]"#).is_err());
        assert!(decode_message(br#"[1,"bye",2,[1,1,0,0,0,0,0]]"#).is_err());
        let m = ModelResponse {
            sequence: 0,
            outcome: Outcome::ValidationError(ValidationError::InvalidLimits),
        };
        assert_eq!(m.stage(), Stage::ValidationError);
    }
    #[test]
    fn lexical_envelope_before_json() {
        for bytes in [
            b"{}".as_slice(),
            b"[true]",
            b"[null]",
            b"[-1]",
            b"[1.0]",
            b"[1e0]",
            b"[01]",
            br#"["a\nb"]"#,
            b"[10000000000]",
            "[\"é\"]".as_bytes(),
            b"[\"\x7f\"]",
            b"[\"\n\"]",
        ] {
            assert!(parse(bytes, REQUEST_CAP).is_err(), "{bytes:?}");
        }
        assert!(parse(b"[0,9999999999,\"printable / ascii\"]", REQUEST_CAP).is_ok());
        assert!(parse(b"[0 1]", REQUEST_CAP).is_err());
        assert!(parse(b"[] []", REQUEST_CAP).is_err());
        assert!(parse(&vec![b' '; REQUEST_CAP + 1], REQUEST_CAP).is_err());
        assert!(parse(format!("[\"{}\"]", "a".repeat(129)).as_bytes(), REQUEST_CAP).is_err());
        assert!(parse(format!("[\"{}\"]", "a".repeat(128)).as_bytes(), REQUEST_CAP).is_ok());
        assert!(parse(b"[[[[[[[[]]]]]]]]", REQUEST_CAP).is_ok());
        assert!(parse(b"[[[[[[[[[]]]]]]]]]", REQUEST_CAP).is_err());
        let mut tokens = Vec::with_capacity(2 * MAX_TOKENS + 2);
        tokens.push(b'[');
        for i in 0..MAX_TOKENS {
            if i != 0 {
                tokens.push(b',');
            }
            tokens.push(b'0');
        }
        tokens.push(b']');
        assert_eq!(
            parse(&tokens, REQUEST_CAP).unwrap_err().kind,
            WireErrorKind::Quota
        );
        tokens.truncate(tokens.len() - 3);
        tokens.push(b']');
        assert!(lexical(&tokens, REQUEST_CAP).is_ok());
    }
    #[test]
    fn malformed_rows_and_nonatom_operands() {
        let mut r = request();
        for source in [
            vec![],
            vec![SourceRow::Let { site: 0 }],
            vec![SourceRow::Unit { site: 0 }, SourceRow::Unit { site: 1 }],
            vec![SourceRow::Mint { site: 0 }, SourceRow::Consume { site: 1 }],
            vec![
                SourceRow::Mint { site: 0 },
                SourceRow::Unit { site: 1 },
                SourceRow::Pair {
                    site: 2,
                    floor: Floor::Copy,
                    left_mode: Mode::Copy,
                    right_mode: Mode::Copy,
                },
            ],
            vec![
                SourceRow::Mint { site: 0 },
                SourceRow::Unit { site: 1 },
                SourceRow::Unpack { site: 2 },
            ],
            vec![
                SourceRow::Mint { site: 0 },
                SourceRow::Unit { site: 1 },
                SourceRow::Unit { site: 2 },
                SourceRow::Ifz { site: 3 },
            ],
        ] {
            r.source = source;
            assert!(encode_request(0, &r).is_err());
        }
        for v in [
            json!([[3]]),
            json!([[0], [0]]),
            json!([[0, 0]]),
            json!([[9]]),
            json!([[1, "01"]]),
        ] {
            assert!(value_code(&v).is_err());
        }
        for v in [
            json!([[3, 0, 0, 0]]),
            json!([[0], [1], [3, 2, 0, 0]]),
            json!([[0], [1], [3, 0, 3, 0]]),
            json!([[0, 9]]),
        ] {
            assert!(type_code(&v).is_err());
        }
    }
    #[test]
    fn independent_input_row_and_depth_quotas() {
        let mut r = request();
        r.input_types = vec![vec![TypeRow::Unit]; 65];
        r.input_values = vec![vec![ValueRow::Unit]; 65];
        assert!(encode_request(0, &r).is_ok());
        r.input_values.push(vec![ValueRow::Unit]);
        assert!(encode_request(0, &r).is_err());
        r.input_values.clear();
        r.input_types.push(vec![TypeRow::Unit]);
        assert!(encode_request(0, &r).is_err());
        r = request();
        let mut code = vec![TypeRow::Unit];
        for _ in 1..256 {
            code.push(TypeRow::Unit);
            code.push(TypeRow::Pair {
                floor: Floor::Copy,
                left_mode: Mode::Copy,
                right_mode: Mode::Copy,
            });
        }
        r.input_types = vec![code.clone()];
        assert!(encode_request(0, &r).is_ok());
        code.push(TypeRow::Unit);
        code.push(TypeRow::Pair {
            floor: Floor::Copy,
            left_mode: Mode::Copy,
            right_mode: Mode::Copy,
        });
        r.input_types = vec![code];
        assert!(encode_request(0, &r).is_err());
        fn balanced(depth: usize, out: &mut TypeCode) {
            if depth == 0 {
                out.push(TypeRow::Unit);
            } else {
                balanced(depth - 1, out);
                balanced(depth - 1, out);
                out.push(TypeRow::Pair {
                    floor: Floor::Copy,
                    left_mode: Mode::Copy,
                    right_mode: Mode::Copy,
                });
            }
        }
        let mut large = vec![];
        balanced(10, &mut large);
        assert_eq!(large.len(), 2047);
        r = request();
        r.input_types = vec![large.clone(); 64];
        r.input_values = vec![vec![ValueRow::Unit]; 63];
        // 64*2047 type rows + 63 value rows + 1 source row = 131072.
        assert!(encode_request(0, &r).is_ok());
        r.input_values.push(vec![ValueRow::Unit]);
        assert!(encode_request(0, &r).is_err());
        r.input_values.clear();
        r.input_types.push(large);
        assert!(encode_request(0, &r).is_err());
        fn balanced_source(depth: usize, out: &mut SourceCode) {
            if depth == 0 {
                out.push(SourceRow::Unit { site: 0 });
            } else {
                balanced_source(depth - 1, out);
                balanced_source(depth - 1, out);
                out.push(SourceRow::Let { site: 0 });
            }
        }
        r = request();
        balanced_source(10, &mut r.source);
        r.source.remove(0);
        r.source.insert(1, SourceRow::Consume { site: 0 });
        assert_eq!(r.source.len(), MAX_CODE_ROWS);
        assert!(encode_request(0, &r).is_ok());
        r.source.insert(3, SourceRow::Consume { site: 0 });
        assert!(encode_request(0, &r).is_err());
        assert!(type_code(&json!(vec![json!([0]); MAX_CODE_ROWS + 1])).is_err());
    }
    #[test]
    fn server_schema_has_no_unknown_or_extra_payloads() {
        for b in [
            br#"[1,"result",0,"validation_error",[["future"]]]"#.as_slice(),
            br#"[1,"result",0,"validation_error",[["rawNodes",1]]]"#,
            br#"[1,"result",0,"check_semantic",[["duplicateUse",0,[0,1]],[1,1,0,0,0]]]"#,
            br#"[1,"result",0,"check_profile",[["typeBound",0,["unknown"]],[1,1,0,0,0]]]"#,
            br#"[1,"result",0,"evaluation",[]]"#,
            br#"[1,"result",0,"future",[]]"#,
            br#"[1,"bye",0,[0,0,0,0,0,0,0],0]"#,
        ] {
            assert!(decode_message(b).is_err());
        }
        let too_large = vec![b' '; RESPONSE_CAP + 1];
        assert!(decode_message(&too_large).is_err());
        assert!(invariant(&json!(["deadHandle", 1, 2])).is_err());
        assert!(refusal(&json!(["capacity", ["future"]])).is_err());
        assert!(invocation_error(&json!(["inputValue", 0, ["future"]])).is_err());
    }
    #[test]
    fn frames_detect_every_truncation_and_cap_before_payload_read() {
        let payload = b"[1]";
        let mut bytes = vec![];
        write_frame(&mut bytes, payload, 3).unwrap();
        assert_eq!(bytes, [3, 0, 0, 0, b'[', b'1', b']']);
        let mut reader = Cursor::new(bytes.clone());
        assert_eq!(read_frame(&mut reader, 3).unwrap(), Some(payload.to_vec()));
        assert_eq!(read_frame(&mut reader, 3).unwrap(), None);
        for len in 1..bytes.len() {
            assert_eq!(
                read_frame(&mut Cursor::new(&bytes[..len]), 3)
                    .unwrap_err()
                    .kind,
                WireErrorKind::Truncated
            );
        }
        struct HeaderOnly {
            header: Cursor<[u8; 4]>,
        }
        impl Read for HeaderOnly {
            fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
                assert!(
                    self.header.position() < 4,
                    "payload allocation/read must not occur"
                );
                self.header.read(b)
            }
        }
        for len in [0u32, 4, u32::MAX] {
            assert_eq!(
                read_frame(
                    &mut HeaderOnly {
                        header: Cursor::new(len.to_le_bytes())
                    },
                    3
                )
                .unwrap_err()
                .kind,
                WireErrorKind::FrameLength
            );
        }
        let mut output = vec![];
        assert!(write_frame(&mut output, b"", 3).is_err());
        assert!(write_frame(&mut output, b"1234", 3).is_err());
        assert!(output.is_empty());
    }
    #[test]
    fn frames_handle_short_and_interrupted_io() {
        struct Slow {
            bytes: Cursor<Vec<u8>>,
            interrupted: bool,
        }
        impl Read for Slow {
            fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
                if !self.interrupted {
                    self.interrupted = true;
                    return Err(io::ErrorKind::Interrupted.into());
                }
                let len = b.len().min(1);
                self.bytes.read(&mut b[..len])
            }
        }
        assert_eq!(
            read_frame(
                &mut Slow {
                    bytes: Cursor::new(vec![1, 0, 0, 0, b'0']),
                    interrupted: false
                },
                1
            )
            .unwrap(),
            Some(vec![b'0'])
        );
        struct Broken;
        impl Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::ErrorKind::Other.into())
            }
        }
        impl Write for Broken {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        assert_eq!(
            read_frame(&mut Broken, 1).unwrap_err().kind,
            WireErrorKind::Io
        );
        assert_eq!(
            write_frame(&mut Broken, b"0", 1).unwrap_err().kind,
            WireErrorKind::Io
        );
    }
}
