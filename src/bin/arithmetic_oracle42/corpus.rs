//! Deterministic finite source corpus. No expression is evaluated here.
use super::{
    Callee, Case, Code, FailureKind, HarnessError, Input, Intent, Kind, Node, Op, Program, Scope,
    Suite,
};
use std::fmt::Write;
use vesper::ast::{self, Expr, ExprKind, Item, Literal, Pass, PatternKind, Statement, Type};

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

fn boundary_table(kind: Kind) -> Result<Vec<i128>, HarnessError> {
    let (min, max) = bounds(kind);
    let mut values = Vec::new();
    values.try_reserve_exact(256).map_err(|_| allocation())?;
    for value in [0, 1, min, max, min + 1, max - 1, -1, -2, 2, 3]
        .into_iter()
        .chain(0..=255)
    {
        if value >= min && value <= max && !values.contains(&value) {
            values.push(value);
            if values.len() == 256 {
                break;
            }
        }
    }
    if values.len() != 256 {
        return Err(error("boundary table cardinality"));
    }
    Ok(values)
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
        if budget == 1 || depth == MAX_DEPTH || self.next() % 4 == 0 {
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
    let table = boundary_table(kind)?;
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
                tape.tree(15, 1, i, &table, &mut code);
                p.locals.push(code);
            }
            let mut code = Vec::with_capacity(15);
            tape.tree(15, 1, count, &table, &mut code);
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
        if i.value < min || i.value > max {
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

fn typed(kind: Kind, ty: &Type) -> bool {
    matches!(ty, Type::Named(n, args) if n == kind_name(kind) && args.is_empty())
}
fn fail_effect(effect: &ast::Effect) -> bool {
    effect.label.is_none() && effect.name == "Fail" && effect.args == [Type::named("ArithOverflow")]
}
fn binding(p: &ast::Pattern, name: &str) -> bool {
    matches!(&p.kind, PatternKind::Bind(n) if n == name)
}
fn plain_block(e: &Expr) -> Result<&Expr, HarnessError> {
    match &e.kind {
        ExprKind::Block {
            statements,
            tail: Some(tail),
        } if statements.is_empty() => Ok(tail),
        _ => Err(error("expected exact statement-free wrapper block")),
    }
}
fn named(e: &Expr, name: &str) -> bool {
    matches!(&e.kind, ExprKind::Name(n) if n == name)
}
fn int(e: &Expr, value: i128) -> bool {
    matches!(&e.kind, ExprKind::Literal(Literal::Int(n)) if *n == value)
}
fn boolean(e: &Expr, value: bool) -> bool {
    matches!(&e.kind, ExprKind::Literal(Literal::Bool(n)) if *n == value)
}
fn args_plain(args: &[ast::Arg]) -> bool {
    args.iter().all(|a| a.pass == Pass::In && !a.explicit)
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
    e: &Expr,
    depth: usize,
    out: &mut Code,
    total: &mut usize,
) -> Result<(), HarnessError> {
    if depth > MAX_DEPTH || *total >= MAX_NODES {
        return Err(error("actual source shape limit"));
    }
    *total += 1;
    let node = match &e.kind {
        ExprKind::Literal(Literal::Int(v)) => Node::Int(*v),
        ExprKind::Literal(Literal::Bool(v)) => Node::Bool(*v),
        ExprKind::Name(n) => {
            let (scope, slot) = parse_variable(n)?;
            Node::Variable(scope, slot)
        }
        ExprKind::Binary(op, a, b) => {
            let op = parsed_op(op)?;
            normalize(a, depth + 1, out, total)?;
            normalize(b, depth + 1, out, total)?;
            Node::Checked(op)
        }
        ExprKind::Call {
            callee,
            types,
            args,
        } if types.is_empty() && args.len() <= 3 && args_plain(args) => {
            let ExprKind::Name(n) = &callee.kind else {
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
                normalize(&a.expr, depth + 1, out, total)?;
            }
            Node::Call(callee, args.len() as u8)
        }
        _ => return Err(error("unexpected source expression")),
    };
    out.push(node);
    Ok(())
}

/// Independently normalize the actual parser output, including its wrapper.
pub fn validate_rendered(
    case: &Case,
    source: &str,
    module: &ast::Module,
) -> Result<(), HarnessError> {
    let p = &case.program;
    validate_program(p)?;
    if source.len() > SOURCE_BYTES
        || !source.is_ascii()
        || !module.model_uses.is_empty()
        || !module.trait_bounds.is_empty()
    {
        return Err(error("source/module profile"));
    }
    let [Item::Function(f), Item::Function(main)] = module.items.as_slice() else {
        return Err(error("expected only f and main"));
    };
    if f.name != "f"
        || main.name != "main"
        || !f.generics.is_empty()
        || !main.generics.is_empty()
        || !main.params.is_empty()
        || f.params.len() != p.inputs.len()
        || !typed(p.kind, &f.result)
        || main.result != Type::Tuple(vec![Type::named("bool"), Type::named(kind_name(p.kind))])
        || main.effects != ast::Effects::default()
        || f.effects.tail.is_some()
    {
        return Err(error("function declarations differ"));
    }
    if (p.allow_fail && (f.effects.instances.len() != 1 || !fail_effect(&f.effects.instances[0])))
        || (!p.allow_fail && !f.effects.instances.is_empty())
    {
        return Err(error("failure row differs"));
    }
    for (i, (param, input)) in f.params.iter().zip(&p.inputs).enumerate() {
        if param.name != format!("x{i}")
            || param.pass != Pass::In
            || !param.projections.is_empty()
            || !typed(input.kind, &param.ty)
        {
            return Err(error("input declaration differs"));
        }
    }
    let Some(fbody) = &f.body else {
        return Err(error("missing f body"));
    };
    let ExprKind::Block {
        statements,
        tail: Some(result),
    } = &fbody.kind
    else {
        return Err(error("f body is not a strict block"));
    };
    if statements.len() != p.locals.len() {
        return Err(error("local count differs"));
    }
    let mut total = 0;
    for (i, (stmt, expected)) in statements.iter().zip(&p.locals).enumerate() {
        let Statement::Let { pat, ty, value } = stmt else {
            return Err(error("extra source statement"));
        };
        if !binding(pat, &format!("t{i}")) || !typed(p.kind, ty) {
            return Err(error("local declaration differs"));
        }
        let mut code = Vec::with_capacity(expected.len());
        normalize(value, 1, &mut code, &mut total)?;
        if code != *expected {
            return Err(error("local source code differs"));
        }
    }
    let mut code = Vec::with_capacity(p.result.len());
    normalize(result, 1, &mut code, &mut total)?;
    if code != p.result {
        return Err(error("result source code differs"));
    }
    let mainbody = plain_block(
        main.body
            .as_ref()
            .ok_or_else(|| error("missing main body"))?,
    )?;
    let ExprKind::Handle {
        effect,
        state: None,
        body,
        ret,
        ops,
    } = &mainbody.kind
    else {
        return Err(error("main handler differs"));
    };
    if !fail_effect(effect)
        || ret.name != "return"
        || ret.pats.len() != 1
        || !binding(&ret.pats[0], "v")
        || !named(&ret.body, "v")
        || ops.len() != 1
    {
        return Err(error("handler return differs"));
    }
    let op = &ops[0];
    if op.name != "raise"
        || op.pats.len() != 2
        || !binding(&op.pats[0], "e")
        || !binding(&op.pats[1], "k")
    {
        return Err(error("handler failure clause differs"));
    }
    let ExprKind::Tuple(failure) = &op.body.kind else {
        return Err(error("failure tuple differs"));
    };
    if failure.len() != 2 || !boolean(&failure[0], true) || !int(&failure[1], 0) {
        return Err(error("failure tag/value differs"));
    }
    let ExprKind::Tuple(success) = &plain_block(body)?.kind else {
        return Err(error("success tuple differs"));
    };
    if success.len() != 2 || !boolean(&success[0], false) {
        return Err(error("success tag differs"));
    }
    let ExprKind::Call {
        callee,
        types,
        args,
    } = &success[1].kind
    else {
        return Err(error("entry call differs"));
    };
    if !named(callee, "f")
        || !types.is_empty()
        || args.len() != p.inputs.len()
        || !args_plain(args)
        || !args
            .iter()
            .zip(&p.inputs)
            .all(|(a, i)| int(&a.expr, i.value))
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
    if out.len() == 64 || p == original.program || out.iter().any(|c| c.program == p) {
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
