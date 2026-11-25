//! Deterministic finite source corpus. No expression is evaluated here.
use super::{
    Callee, Case, Code, FailureKind, HarnessError, Input, Intent, Kind, Node, Op, Program, Scope,
    Suite,
};
use serde_json::Value;
use std::fmt::Write;
use std::sync::OnceLock;

const MAX_NODES: usize = 63;
const MAX_DEPTH: usize = 8;
const SOURCE_BYTES: usize = 16384;

fn error(detail: &str) -> HarnessError {
    HarnessError {
        kind: FailureKind::UnsupportedProfile,
        detail: detail.into(),
    }
}
fn allocation() -> HarnessError {
    HarnessError {
        kind: FailureKind::Allocation,
        detail: "corpus allocation failed".into(),
    }
}
fn bounds(kind: Kind) -> (i128, i128) {
    match kind {
        Kind::U8 => (0, u8::MAX as i128),
        Kind::U32 => (0, u32::MAX as i128),
        Kind::U64 => (0, u64::MAX as i128),
        Kind::I64 => (i64::MIN as i128, i64::MAX as i128),
    }
}
fn kind_tag(kind: Kind) -> u8 {
    match kind {
        Kind::U8 => 0,
        Kind::U32 => 1,
        Kind::U64 => 2,
        Kind::I64 => 3,
    }
}
fn op_tag(op: Op) -> u8 {
    match op {
        Op::Add => 0,
        Op::Sub => 1,
        Op::Mul => 2,
    }
}
fn kind_name(kind: Kind) -> &'static str {
    match kind {
        Kind::U8 => "u8",
        Kind::U32 => "u32",
        Kind::U64 => "u64",
        Kind::I64 => "i64",
    }
}
fn op_name(op: Op) -> &'static str {
    match op {
        Op::Add => "+",
        Op::Sub => "-",
        Op::Mul => "*",
    }
}
fn intrinsic(op: Op) -> &'static str {
    match op {
        Op::Add => "wrapping_add",
        Op::Sub => "wrapping_sub",
        Op::Mul => "wrapping_mul",
    }
}
fn variable(scope: Scope, slot: u16) -> String {
    format!(
        "{}{slot}",
        match scope {
            Scope::Input => "x",
            Scope::Local => "t",
        }
    )
}

fn boundary_table(kind: Kind) -> &'static [i128; 256] {
    static TABLES: [OnceLock<[i128; 256]>; 4] = [const { OnceLock::new() }; 4];
    TABLES[kind_tag(kind) as usize].get_or_init(|| make_boundary_table(kind))
}
fn make_boundary_table(kind: Kind) -> [i128; 256] {
    let (min, max) = bounds(kind);
    let mut values = [0; 256];
    let mut len = 0;
    for value in [0, 1, min, max, min + 1, max - 1, -1, -2, 2, 3]
        .into_iter()
        .chain(0..=255)
    {
        if (min..=max).contains(&value) && !values[..len].contains(&value) {
            values[len] = value;
            len += 1;
            if len == 256 {
                break;
            }
        }
    }
    values
}
fn binary(a: Node, b: Node, op: Op, wrapping: bool) -> Code {
    vec![
        a,
        b,
        if wrapping {
            Node::Call(Callee::Intrinsic(op), 2)
        } else {
            Node::Checked(op)
        },
    ]
}
fn input(slot: u16) -> Node {
    Node::Variable(Scope::Input, slot)
}
fn local(slot: u16) -> Node {
    Node::Variable(Scope::Local, slot)
}

struct Tape(u64);
impl Tape {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e3779b97f4a7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
        z ^ (z >> 31)
    }
    fn tree(
        &mut self,
        budget: usize,
        depth: usize,
        available: usize,
        table: &[i128],
        out: &mut Code,
    ) {
        if budget == 1 || depth == MAX_DEPTH || self.next().is_multiple_of(4) {
            let scope = self.next() % 3;
            let index = self.next();
            out.push(match scope {
                0 => input((index % 2) as u16),
                1 if available != 0 => local((index % available as u64) as u16),
                1 => input((index % 2) as u16),
                _ => Node::Int(table[(index % 256) as usize]),
            });
            return;
        }
        let operation = self.next() % 6;
        let left = 1 + 2 * (self.next() % ((budget - 1) / 2) as u64) as usize;
        self.tree(left, depth + 1, available, table, out);
        self.tree(budget - 1 - left, depth + 1, available, table, out);
        let op = match operation % 3 {
            0 => Op::Add,
            1 => Op::Sub,
            _ => Op::Mul,
        };
        out.push(if operation < 3 {
            Node::Checked(op)
        } else {
            Node::Call(Callee::Intrinsic(op), 2)
        });
    }
}

/// Return the exact selected case, without evaluating its arithmetic.
pub fn case_at(suite: Suite, seed: u64, id: u64) -> Result<Case, HarnessError> {
    let count = match suite {
        Suite::Ci128 => 128,
        Suite::Full100000 => 100000,
    };
    if id >= count {
        return Err(error("case ID outside suite"));
    }
    let kind = match id % 4 {
        0 => Kind::U8,
        1 => Kind::U32,
        2 => Kind::U64,
        _ => Kind::I64,
    };
    let q = id / 4;
    let rank = match suite {
        Suite::Ci128 if q >= 16 => {
            let j = q - 15;
            256 * j + j % 16
        }
        _ => q,
    };
    let table = boundary_table(kind);
    let (min, max) = bounds(kind);
    let mut p = Program {
        kind,
        inputs: vec![
            Input {
                kind,
                value: table[(rank % 256) as usize],
            },
            Input {
                kind,
                value: table[(rank / 256) as usize],
            },
        ],
        locals: vec![],
        result: vec![],
        allow_fail: true,
    };
    p.result = match rank {
        0 => vec![Node::Int(42)],
        1 => binary(Node::Int(max), Node::Int(1), Op::Add, false),
        2 => binary(Node::Int(min), Node::Int(1), Op::Sub, false),
        3 => binary(Node::Int(max), Node::Int(max), Op::Mul, false),
        4 => binary(Node::Int(max), Node::Int(1), Op::Add, true),
        5 => binary(Node::Int(min), Node::Int(1), Op::Sub, true),
        6 => binary(Node::Int(max), Node::Int(max), Op::Mul, true),
        7..=12 => {
            let op = match (rank - 7) % 3 {
                0 => Op::Add,
                1 => Op::Sub,
                _ => Op::Mul,
            };
            binary(input(0), input(1), op, rank >= 10)
        }
        13 => {
            p.locals = vec![
                binary(input(0), Node::Int(0), Op::Add, true),
                binary(input(1), Node::Int(1), Op::Mul, true),
            ];
            binary(local(0), local(1), Op::Add, false)
        }
        14 => {
            p.locals = vec![binary(Node::Int(max), Node::Int(1), Op::Add, false)];
            vec![Node::Int(42)]
        }
        15 => {
            p.locals = vec![
                vec![Node::Int(20)],
                vec![Node::Int(22)],
                binary(local(0), local(1), Op::Add, false),
            ];
            vec![local(2)]
        }
        _ => {
            let mut tape = Tape(seed ^ (4 * rank + kind_tag(kind) as u64));
            let count = (tape.next() % 4) as usize;
            for i in 0..count {
                let mut code = Vec::with_capacity(15);
                tape.tree(15, 1, i, table, &mut code);
                p.locals.push(code);
            }
            let mut code = Vec::with_capacity(15);
            tape.tree(15, 1, count, table, &mut code);
            code
        }
    };
    validate_program(&p)?;
    Ok(Case {
        id,
        program: p,
        intent: Intent::Valid,
    })
}

/// The published isolated static-negative matrix, in exact order.
pub fn negative_cases(first_id: u64) -> Result<Vec<Case>, HarnessError> {
    first_id
        .checked_add(14)
        .ok_or_else(|| error("negative case ID overflow"))?;
    let mut cases = Vec::new();
    cases.try_reserve_exact(15).map_err(|_| allocation())?;
    for n in 0..15 {
        let kind = match n {
            4 => Kind::U32,
            5 => Kind::U64,
            6 | 7 | 14 => Kind::I64,
            _ => Kind::U8,
        };
        let mut p = Program {
            kind,
            inputs: vec![Input { kind, value: 42 }, Input { kind, value: 1 }],
            locals: vec![],
            result: vec![],
            allow_fail: n != 8,
        };
        p.result = match n {
            0 => vec![input(9)],
            1 => {
                p.inputs[1].kind = Kind::U32;
                binary(input(0), input(1), Op::Add, false)
            }
            2 => vec![Node::Int(256)],
            3 => vec![Node::Int(-1)],
            4 => vec![Node::Int(4294967296)],
            5 => vec![Node::Int(18446744073709551616)],
            6 => vec![Node::Int(9223372036854775808)],
            7 => vec![Node::Int(-9223372036854775809)],
            8 => binary(input(0), Node::Int(1), Op::Add, false),
            9 => vec![Node::Int(1), Node::Call(Callee::Intrinsic(Op::Add), 1)],
            10 => binary(Node::Bool(true), Node::Int(1), Op::Add, true),
            11 => vec![
                Node::Int(1),
                Node::Int(2),
                Node::Call(Callee::Variable(Scope::Input, 0), 2),
            ],
            12 => vec![local(0)],
            13 => {
                p.locals = vec![vec![local(1)], vec![Node::Int(0)]];
                vec![local(0)]
            }
            _ => vec![Node::Int(-18446744073709551616)],
        };
        validate_program(&p)?;
        cases.push(Case {
            id: first_id + n,
            program: p,
            intent: Intent::StaticNegative,
        });
    }
    Ok(cases)
}

fn arity(node: &Node) -> Result<usize, HarnessError> {
    Ok(match node {
        Node::Checked(_) => 2,
        Node::Call(_, n) if *n <= 3 => *n as usize,
        Node::Call(_, _) => return Err(error("call arity exceeds profile")),
        _ => 0,
    })
}
fn validate_program(p: &Program) -> Result<(), HarnessError> {
    if p.inputs.len() > 3 || p.locals.len() > 3 {
        return Err(error("input/local profile limit"));
    }
    for i in &p.inputs {
        let (min, max) = bounds(i.kind);
        if !(min..=max).contains(&i.value) {
            return Err(error("input value outside declared kind"));
        }
    }
    let mut total = 0usize;
    for code in p.locals.iter().chain(std::iter::once(&p.result)) {
        total = total
            .checked_add(code.len())
            .ok_or_else(|| error("node count overflow"))?;
        if code.is_empty() || total > MAX_NODES {
            return Err(error("postfix node profile limit"));
        }
        let mut stack = [0usize; MAX_NODES];
        let mut len = 0;
        for node in code {
            let n = arity(node)?;
            if len < n {
                return Err(error("postfix stack underflow"));
            }
            let depth = if n == 0 {
                1
            } else {
                1 + stack[len - n..len].iter().copied().max().unwrap_or(0)
            };
            if depth > MAX_DEPTH {
                return Err(error("postfix depth profile limit"));
            }
            len -= n;
            stack[len] = depth;
            len += 1;
        }
        if len != 1 {
            return Err(error("postfix has extra roots"));
        }
    }
    Ok(())
}

fn render_code(code: &Code) -> Result<String, HarnessError> {
    let mut stack: Vec<String> = Vec::new();
    stack
        .try_reserve_exact(code.len())
        .map_err(|_| allocation())?;
    for node in code {
        let n = arity(node)?;
        let start = stack
            .len()
            .checked_sub(n)
            .ok_or_else(|| error("postfix render underflow"))?;
        let rendered = match node {
            Node::Int(v) => v.to_string(),
            Node::Bool(v) => v.to_string(),
            Node::Variable(scope, slot) => variable(*scope, *slot),
            Node::Checked(op) => {
                format!("({} {} {})", stack[start], op_name(*op), stack[start + 1])
            }
            Node::Call(callee, _) => {
                let name = match callee {
                    Callee::Intrinsic(op) => intrinsic(*op).into(),
                    Callee::Variable(scope, slot) => variable(*scope, *slot),
                };
                format!("{name}({})", stack[start..].join(", "))
            }
        };
        stack.truncate(start);
        stack.push(rendered);
    }
    stack.pop().ok_or_else(|| error("empty rendered code"))
}

pub fn render_case(case: &Case) -> Result<String, HarnessError> {
    let p = &case.program;
    validate_program(p)?;
    // 63 nodes, each <=40 decimal bytes or a bounded operator/name, plus the
    // fixed wrapper fit this preflight envelope before source construction.
    let estimate = 1024usize + 80 * (p.locals.iter().map(Vec::len).sum::<usize>() + p.result.len());
    if estimate > SOURCE_BYTES {
        return Err(error("source preflight limit"));
    }
    let mut out = String::new();
    out.try_reserve_exact(estimate).map_err(|_| allocation())?;
    out.push_str("fn f(");
    for (i, input) in p.inputs.iter().enumerate() {
        if i != 0 {
            out.push_str(", ");
        }
        write!(out, "x{i}: {}", kind_name(input.kind)).map_err(|_| error("source formatting"))?;
    }
    write!(
        out,
        ") -> {}{} {{ ",
        kind_name(p.kind),
        if p.allow_fail {
            " eff [Fail[ArithOverflow]]"
        } else {
            ""
        }
    )
    .map_err(|_| error("source formatting"))?;
    for (i, code) in p.locals.iter().enumerate() {
        write!(
            out,
            "let t{i}: {} = {}; ",
            kind_name(p.kind),
            render_code(code)?
        )
        .map_err(|_| error("source formatting"))?;
    }
    write!(
        out,
        "{} }} fn main() -> (bool, {}) {{ handle Fail[ArithOverflow] in {{ (false, f(",
        render_code(&p.result)?,
        kind_name(p.kind)
    )
    .map_err(|_| error("source formatting"))?;
    for (i, input) in p.inputs.iter().enumerate() {
        if i != 0 {
            out.push_str(", ");
        }
        write!(out, "{}", input.value).map_err(|_| error("source formatting"))?;
    }
    out.push_str(")) } with { return v => v, raise(e, k) => (true, 0), } }");
    if out.len() > SOURCE_BYTES || out.len() > estimate {
        return Err(error("source bound invariant"));
    }
    Ok(out)
}

// The parsed module is the compiler's own `emit --emit hir --no-opt` JSON
// (externally tagged enums, `kind`/`span` expression records). Only the
// fields named here are read; spans are ignored.

/// The single variant `{ "<tag>": payload }` of an enum value.
fn variant(v: &Value) -> Option<(&str, &Value)> {
    let o = v.as_object()?;
    (o.len() == 1).then(|| o.iter().next().map(|(k, v)| (k.as_str(), v)))?
}
fn tagged<'a>(v: &'a Value, tag: &str) -> Option<&'a Value> {
    variant(v).and_then(|(t, p)| (t == tag).then_some(p))
}
fn field<'a>(v: &'a Value, name: &str) -> Result<&'a Value, HarnessError> {
    v.get(name)
        .ok_or_else(|| error("parsed module lacks an expected field"))
}
fn list<'a>(v: &'a Value, name: &str) -> Result<&'a [Value], HarnessError> {
    field(v, name)?
        .as_array()
        .map(Vec::as_slice)
        .ok_or_else(|| error("parsed module field is not a list"))
}
fn text<'a>(v: &'a Value, name: &str) -> Option<&'a str> {
    v.get(name).and_then(Value::as_str)
}
fn empty(v: &Value, name: &str) -> bool {
    v.get(name)
        .is_none_or(|f| f.is_null() || f.as_array().is_some_and(Vec::is_empty))
}
/// An expression's kind (`{ "kind": ..., "span": ... }`).
fn kind_of(e: &Value) -> Result<(&str, &Value), HarnessError> {
    e.get("kind")
        .and_then(variant)
        .ok_or_else(|| error("parsed expression has no kind"))
}
/// `Named(name, [])`.
fn named_type(ty: &Value, name: &str) -> bool {
    tagged(ty, "Named")
        .and_then(Value::as_array)
        .is_some_and(|p| p.len() == 2 && p[0] == name && p[1].as_array().is_some_and(Vec::is_empty))
}
fn typed(kind: Kind, ty: &Value) -> bool {
    named_type(ty, kind_name(kind))
}
fn fail_effect(effect: &Value) -> bool {
    effect.get("label").is_some_and(Value::is_null)
        && text(effect, "name") == Some("Fail")
        && effect
            .get("args")
            .and_then(Value::as_array)
            .is_some_and(|a| a.len() == 1 && named_type(&a[0], "ArithOverflow"))
}
fn no_effects(effects: &Value) -> bool {
    empty(effects, "instances") && effects.get("tail").is_none_or(Value::is_null)
}
fn binding(p: &Value, name: &str) -> bool {
    p.get("kind")
        .and_then(|k| tagged(k, "Bind"))
        .is_some_and(|n| n == name)
}
fn plain_block(e: &Value) -> Result<&Value, HarnessError> {
    match kind_of(e)? {
        ("Block", b) if empty(b, "statements") && !empty(b, "tail") => field(b, "tail"),
        _ => Err(error("expected exact statement-free wrapper block")),
    }
}
fn named(e: &Value, name: &str) -> bool {
    matches!(kind_of(e), Ok(("Name", n)) if n == name)
}
fn literal_int(e: &Value) -> Option<i128> {
    let ("Literal", l) = kind_of(e).ok()? else {
        return None;
    };
    let n = tagged(l, "Int")?;
    n.as_i64()
        .map(i128::from)
        .or_else(|| n.as_u64().map(i128::from))
}
fn int(e: &Value, value: i128) -> bool {
    literal_int(e) == Some(value)
}
fn boolean(e: &Value, value: bool) -> bool {
    matches!(kind_of(e), Ok(("Literal", l)) if tagged(l, "Bool") == Some(&Value::Bool(value)))
}
fn args_plain(args: &[Value]) -> bool {
    args.iter()
        .all(|a| text(a, "pass") == Some("In") && a.get("explicit") == Some(&Value::Bool(false)))
}
fn parse_variable(name: &str) -> Result<(Scope, u16), HarnessError> {
    let (scope, digits) = if let Some(s) = name.strip_prefix('x') {
        (Scope::Input, s)
    } else if let Some(s) = name.strip_prefix('t') {
        (Scope::Local, s)
    } else {
        return Err(error("unexpected source variable"));
    };
    let slot = digits
        .parse::<u16>()
        .map_err(|_| error("source variable index"))?;
    if digits != slot.to_string() {
        return Err(error("noncanonical source variable"));
    }
    Ok((scope, slot))
}
fn parsed_op(name: &str) -> Result<Op, HarnessError> {
    match name {
        "+" => Ok(Op::Add),
        "-" => Ok(Op::Sub),
        "*" => Ok(Op::Mul),
        _ => Err(error("unsupported source operator")),
    }
}
fn normalize(
    e: &Value,
    depth: usize,
    out: &mut Code,
    total: &mut usize,
) -> Result<(), HarnessError> {
    if depth > MAX_DEPTH || *total >= MAX_NODES {
        return Err(error("actual source shape limit"));
    }
    *total += 1;
    let node = match kind_of(e)? {
        ("Literal", l) => match variant(l) {
            Some(("Int", _)) => {
                Node::Int(literal_int(e).ok_or_else(|| error("source literal range"))?)
            }
            Some(("Bool", Value::Bool(v))) => Node::Bool(*v),
            _ => return Err(error("unexpected source literal")),
        },
        ("Name", Value::String(n)) => {
            let (scope, slot) = parse_variable(n)?;
            Node::Variable(scope, slot)
        }
        ("Binary", Value::Array(parts)) if parts.len() == 3 => {
            let op = parsed_op(parts[0].as_str().ok_or_else(|| error("source operator"))?)?;
            normalize(&parts[1], depth + 1, out, total)?;
            normalize(&parts[2], depth + 1, out, total)?;
            Node::Checked(op)
        }
        ("Call", call) => {
            let args = list(call, "args")?;
            if !empty(call, "types") || args.len() > 3 || !args_plain(args) {
                return Err(error("unexpected source call"));
            }
            let ("Name", Value::String(n)) = kind_of(field(call, "callee")?)? else {
                return Err(error("non-name source callee"));
            };
            let callee = match n.as_str() {
                "wrapping_add" => Callee::Intrinsic(Op::Add),
                "wrapping_sub" => Callee::Intrinsic(Op::Sub),
                "wrapping_mul" => Callee::Intrinsic(Op::Mul),
                _ => {
                    let (scope, slot) = parse_variable(n)?;
                    Callee::Variable(scope, slot)
                }
            };
            for a in args {
                normalize(field(a, "expr")?, depth + 1, out, total)?;
            }
            Node::Call(callee, args.len() as u8)
        }
        _ => return Err(error("unexpected source expression")),
    };
    out.push(node);
    Ok(())
}

/// Independently normalize the actual parser output, including its wrapper.
/// `module` is the compiler's parsed module (`emit --emit hir --no-opt`).
pub fn validate_rendered(case: &Case, source: &str, module: &Value) -> Result<(), HarnessError> {
    let p = &case.program;
    validate_program(p)?;
    let only_items = module.as_object().is_some_and(|m| {
        m.iter()
            .all(|(k, v)| k == "items" || empty(module, k) || v.is_null())
    });
    if source.len() > SOURCE_BYTES || !source.is_ascii() || !only_items {
        return Err(error("source/module profile"));
    }
    let items = list(module, "items")?;
    let [f, main] = items else {
        return Err(error("expected only f and main"));
    };
    let (Some(f), Some(main)) = (tagged(f, "Function"), tagged(main, "Function")) else {
        return Err(error("expected only f and main"));
    };
    let main_result = tagged(field(main, "result")?, "Tuple")
        .and_then(Value::as_array)
        .is_some_and(|t| t.len() == 2 && named_type(&t[0], "bool") && typed(p.kind, &t[1]));
    let f_params = list(f, "params")?;
    if text(f, "name") != Some("f")
        || text(main, "name") != Some("main")
        || !empty(f, "generics")
        || !empty(main, "generics")
        || !empty(main, "params")
        || f_params.len() != p.inputs.len()
        || !typed(p.kind, field(f, "result")?)
        || !main_result
        || !no_effects(field(main, "effects")?)
        || !field(f, "effects")?.get("tail").is_none_or(Value::is_null)
    {
        return Err(error("function declarations differ"));
    }
    let instances = list(field(f, "effects")?, "instances")?;
    if (p.allow_fail && (instances.len() != 1 || !fail_effect(&instances[0])))
        || (!p.allow_fail && !instances.is_empty())
    {
        return Err(error("failure row differs"));
    }
    for (i, (param, input)) in f_params.iter().zip(&p.inputs).enumerate() {
        if text(param, "name") != Some(format!("x{i}").as_str())
            || text(param, "pass") != Some("In")
            || !empty(param, "projections")
            || !typed(input.kind, field(param, "ty")?)
        {
            return Err(error("input declaration differs"));
        }
    }
    let fbody = f
        .get("body")
        .filter(|b| !b.is_null())
        .ok_or_else(|| error("missing f body"))?;
    let ("Block", block) = kind_of(fbody)? else {
        return Err(error("f body is not a strict block"));
    };
    let statements = list(block, "statements")?;
    if empty(block, "tail") {
        return Err(error("f body is not a strict block"));
    }
    let result = field(block, "tail")?;
    if statements.len() != p.locals.len() {
        return Err(error("local count differs"));
    }
    let mut total = 0;
    for (i, (stmt, expected)) in statements.iter().zip(&p.locals).enumerate() {
        let Some(binding_let) = tagged(stmt, "Let") else {
            return Err(error("extra source statement"));
        };
        if !binding(field(binding_let, "pat")?, &format!("t{i}"))
            || !typed(p.kind, field(binding_let, "ty")?)
        {
            return Err(error("local declaration differs"));
        }
        let mut code = Vec::with_capacity(expected.len());
        normalize(field(binding_let, "value")?, 1, &mut code, &mut total)?;
        if code != *expected {
            return Err(error("local source code differs"));
        }
    }
    let mut code = Vec::with_capacity(p.result.len());
    normalize(result, 1, &mut code, &mut total)?;
    if code != p.result {
        return Err(error("result source code differs"));
    }
    let main_body = main
        .get("body")
        .filter(|b| !b.is_null())
        .ok_or_else(|| error("missing main body"))?;
    let ("Handle", handle) = kind_of(plain_block(main_body)?)? else {
        return Err(error("main handler differs"));
    };
    let ret = field(handle, "ret")?;
    let ret_pats = list(ret, "pats")?;
    let ops = list(handle, "ops")?;
    if !fail_effect(field(handle, "effect")?)
        || !handle.get("state").is_none_or(Value::is_null)
        || text(ret, "name") != Some("return")
        || ret_pats.len() != 1
        || !binding(&ret_pats[0], "v")
        || !named(field(ret, "body")?, "v")
        || ops.len() != 1
    {
        return Err(error("handler return differs"));
    }
    let op = &ops[0];
    let op_pats = list(op, "pats")?;
    if text(op, "name") != Some("raise")
        || op_pats.len() != 2
        || !binding(&op_pats[0], "e")
        || !binding(&op_pats[1], "k")
    {
        return Err(error("handler failure clause differs"));
    }
    let ("Tuple", Value::Array(failure)) = kind_of(field(op, "body")?)? else {
        return Err(error("failure tuple differs"));
    };
    if failure.len() != 2 || !boolean(&failure[0], true) || !int(&failure[1], 0) {
        return Err(error("failure tag/value differs"));
    }
    let ("Tuple", Value::Array(success)) = kind_of(plain_block(field(handle, "body")?)?)? else {
        return Err(error("success tuple differs"));
    };
    if success.len() != 2 || !boolean(&success[0], false) {
        return Err(error("success tag differs"));
    }
    let ("Call", call) = kind_of(&success[1])? else {
        return Err(error("entry call differs"));
    };
    let args = list(call, "args")?;
    if !named(field(call, "callee")?, "f")
        || !empty(call, "types")
        || args.len() != p.inputs.len()
        || !args_plain(args)
        || !args
            .iter()
            .zip(&p.inputs)
            .all(|(a, i)| a.get("expr").is_some_and(|e| int(e, i.value)))
    {
        return Err(error("actual input argument values differ"));
    }
    Ok(())
}

/// Canonical semantic hash; never hashes a calculated expression result.
pub fn semantic_fingerprint(case: &Case) -> Result<[u8; 32], HarnessError> {
    let p = &case.program;
    validate_program(p)?;
    let mut h = blake3::Hasher::new();
    h.update(b"vesper.oracle42.program\0");
    h.update(&1u16.to_le_bytes());
    h.update(&[kind_tag(p.kind), p.inputs.len() as u8]);
    for input in &p.inputs {
        h.update(&[kind_tag(input.kind)]);
        h.update(&input.value.to_le_bytes());
    }
    h.update(&[u8::from(p.allow_fail), p.locals.len() as u8]);
    for code in p.locals.iter().chain(std::iter::once(&p.result)) {
        h.update(&(code.len() as u16).to_le_bytes());
        for node in code {
            match node {
                Node::Int(v) => {
                    h.update(&[0]);
                    h.update(&v.to_le_bytes());
                }
                Node::Bool(v) => {
                    h.update(&[5, u8::from(*v)]);
                }
                Node::Variable(scope, slot) => {
                    h.update(&[match scope {
                        Scope::Input => 1,
                        Scope::Local => 2,
                    }]);
                    h.update(&slot.to_le_bytes());
                }
                Node::Checked(op) => {
                    h.update(&[3, op_tag(*op)]);
                }
                Node::Call(callee, n) => {
                    let (target, selector) = match callee {
                        Callee::Intrinsic(op) => (0, op_tag(*op) as u16),
                        Callee::Variable(Scope::Input, slot) => (1, *slot),
                        Callee::Variable(Scope::Local, slot) => (2, *slot),
                    };
                    h.update(&[4, target]);
                    h.update(&selector.to_le_bytes());
                    h.update(&[*n]);
                }
            }
        }
    }
    Ok(*h.finalize().as_bytes())
}

fn add_shrink(out: &mut Vec<Case>, original: &Case, p: Program) -> Result<(), HarnessError> {
    fn size(p: &Program) -> (usize, usize, u128) {
        let nodes = p.locals.iter().map(Vec::len).sum::<usize>() + p.result.len();
        let values = p.inputs.iter().map(|i| i.value).chain(
            p.locals
                .iter()
                .chain(std::iter::once(&p.result))
                .flatten()
                .filter_map(|n| match n {
                    Node::Int(v) => Some(*v),
                    _ => None,
                }),
        );
        let magnitude = values.fold(0u128, |sum, n| sum.saturating_add(n.unsigned_abs()));
        (nodes, p.locals.len(), magnitude)
    }
    if out.len() == 64 || size(&p) >= size(&original.program) || out.iter().any(|c| c.program == p)
    {
        return Ok(());
    }
    validate_program(&p)?;
    out.push(Case {
        id: original.id,
        intent: original.intent,
        program: p,
    });
    Ok(())
}

/// Bounded deterministic suggestions only; the runner rechecks each mismatch.
pub fn shrink_candidates(case: &Case) -> Result<Vec<Case>, HarnessError> {
    validate_program(&case.program)?;
    let mut out = Vec::new();
    out.try_reserve_exact(64).map_err(|_| allocation())?;
    // Inputs and literals are reduced without evaluating any operator.
    for i in 0..case.program.inputs.len() {
        for v in [0, 1] {
            let mut p = case.program.clone();
            p.inputs[i].value = v;
            add_shrink(&mut out, case, p)?;
        }
    }
    for which in 0..=case.program.locals.len() {
        let original = if which == case.program.locals.len() {
            &case.program.result
        } else {
            &case.program.locals[which]
        };
        let mut spans: Vec<(usize, usize)> = Vec::with_capacity(original.len());
        for (i, node) in original.iter().enumerate() {
            let n = arity(node)?;
            let start = if n == 0 { i } else { spans[spans.len() - n].0 };
            if n != 0 {
                for child in &spans[spans.len() - n..] {
                    if out.len() == 64 {
                        return Ok(out);
                    }
                    let mut p = case.program.clone();
                    let code = if which == p.locals.len() {
                        &mut p.result
                    } else {
                        &mut p.locals[which]
                    };
                    code.splice(start..=i, original[child.0..=child.1].iter().cloned());
                    add_shrink(&mut out, case, p)?;
                }
            }
            if let Node::Int(value) = node {
                for v in [0, 1] {
                    if *value != v {
                        let mut p = case.program.clone();
                        let code = if which == p.locals.len() {
                            &mut p.result
                        } else {
                            &mut p.locals[which]
                        };
                        code[i] = Node::Int(v);
                        add_shrink(&mut out, case, p)?;
                    }
                }
            }
            spans.truncate(spans.len() - n);
            spans.push((start, i));
        }
    }
    // Delete only unused strict locals, adjusting every subsequent local index.
    for removed in (0..case.program.locals.len()).rev() {
        if out.len() == 64 {
            break;
        }
        let used = case.program.locals.iter().skip(removed+1).chain(std::iter::once(&case.program.result)).flatten().any(|n| matches!(n, Node::Variable(Scope::Local, x) | Node::Call(Callee::Variable(Scope::Local, x), _) if *x as usize == removed));
        if used {
            continue;
        }
        let mut p = case.program.clone();
        p.locals.remove(removed);
        for n in p
            .locals
            .iter_mut()
            .chain(std::iter::once(&mut p.result))
            .flatten()
        {
            match n {
                Node::Variable(Scope::Local, x)
                | Node::Call(Callee::Variable(Scope::Local, x), _)
                    if *x as usize > removed =>
                {
                    *x -= 1
                }
                _ => {}
            }
        }
        add_shrink(&mut out, case, p)?;
    }
    Ok(out)
}
