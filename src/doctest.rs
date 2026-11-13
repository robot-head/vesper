//! Checked documentation fences with finite native execution, and hermetic
//! `sim(...)` fences on the reference evaluator under a seeded SimPulse kernel
//! (§40.2). No ambient IO, source arena or hard-timeout/sandbox claim is made.
use crate::{
    ast::Diagnostic,
    check,
    lexer::{self, TokenKind as K},
    native::batch::{BatchLimits, BatchStats, NativeBatch},
    parser,
};
use serde::Serialize;
use vesper_doc::sim::{self, SimHost, SimLimits, SimOutcome, SimSpec};

#[derive(Clone, Copy, Debug)]
pub struct DocTestLimits {
    pub max_source_bytes: usize,
    pub max_snippet_bytes: usize,
    pub max_combined_bytes: usize,
    pub max_total_check_bytes: usize,
    pub max_tokens: usize,
    pub max_nesting: usize,
    pub max_segment_tokens: usize,
    pub max_declarations: usize,
    pub max_examples: usize,
    pub max_nominal_expansion: usize,
    pub runs_per_example: usize,
    pub batch: BatchLimits,
}
impl Default for DocTestLimits {
    fn default() -> Self {
        Self {
            max_source_bytes: 128 * 1024,
            max_snippet_bytes: 16 * 1024,
            max_combined_bytes: 512 * 1024,
            max_total_check_bytes: 8 * 1024 * 1024,
            max_tokens: 8192,
            max_nesting: 32,
            max_segment_tokens: 128,
            max_declarations: 128,
            max_examples: 64,
            max_nominal_expansion: 8192,
            runs_per_example: 2,
            batch: BatchLimits::default(),
        }
    }
}
impl DocTestLimits {
    fn validate(self) -> Result<(), String> {
        if self.max_source_bytes == 0
            || self.max_source_bytes > 512 * 1024
            || self.max_snippet_bytes == 0
            || self.max_snippet_bytes > 64 * 1024
            || self.max_combined_bytes == 0
            || self.max_combined_bytes > 2 * 1024 * 1024
            || self.max_total_check_bytes == 0
            || self.max_total_check_bytes > 32 * 1024 * 1024
            || self.max_tokens == 0
            || self.max_tokens > 16384
            || self.max_nesting == 0
            || self.max_nesting > 32
            || self.max_segment_tokens == 0
            || self.max_segment_tokens > 128
            || self.max_declarations == 0
            || self.max_declarations > 256
            || self.max_nominal_expansion == 0
            || self.max_nominal_expansion > 32768
            || self.max_examples == 0
            || self.max_examples > 128
            || self.runs_per_example == 0
            || self.runs_per_example > 100
        {
            return Err("doctest limit configuration".into());
        }
        Ok(())
    }
}
#[derive(Debug, Serialize)]
pub struct LocatedDiagnostic {
    pub code: String,
    pub message: String,
    pub byte_start: usize,
    pub byte_end: usize,
    pub line: usize,
    pub column: usize,
}
#[derive(Debug, Serialize)]
pub struct ExampleReport {
    pub index: usize,
    pub line: usize,
    pub column: usize,
    pub byte_start: usize,
    pub passed: bool,
    pub expected_code: Option<String>,
    pub entry: Option<String>,
    pub result: Option<String>,
    pub message: String,
    pub diagnostics: Vec<LocatedDiagnostic>,
    /// Present for `sim(...)` fences that reached execution.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub simulation: Option<SimulationReport>,
}
/// A simulated doctest ran twice: a recording run and a replay of its
/// SimPulse journal. Both must succeed with identical results and traces.
#[derive(Debug, Serialize)]
pub struct SimulationReport {
    pub spec: SimSpec,
    pub runs: usize,
    pub replayed: bool,
    pub outcome: SimOutcome,
}
#[derive(Debug, Serialize)]
pub struct BatchReport {
    pub module_count: usize,
    pub compilation_count: usize,
    pub defined_functions: usize,
    pub entry_count: usize,
    pub runs: u64,
    pub max_entry_work: usize,
}
impl From<BatchStats> for BatchReport {
    fn from(s: BatchStats) -> Self {
        Self {
            module_count: s.module_count,
            compilation_count: s.compilation_count,
            defined_functions: s.defined_functions,
            entry_count: s.entry_count,
            runs: s.runs,
            max_entry_work: s.max_entry_work,
        }
    }
}
#[derive(Debug, Serialize)]
pub struct DocTestReport {
    pub passed: bool,
    pub base_diagnostics: Vec<LocatedDiagnostic>,
    pub examples: Vec<ExampleReport>,
    pub batch: Option<BatchReport>,
}
struct Line {
    text: String,
    offset: usize,
}
struct Example {
    lines: Vec<Line>,
    offset: usize,
    expected: Option<String>,
    entry: Option<String>,
    sim: Option<SimSpec>,
}
fn location(source: &str, offset: usize) -> (usize, usize) {
    let offset = offset.min(source.len());
    let prefix = &source[..source.floor_char_boundary(offset)];
    (
        prefix.bytes().filter(|b| *b == b'\n').count() + 1,
        prefix.rsplit('\n').next().unwrap_or("").chars().count() + 1,
    )
}
fn diagnostic(source: &str, d: &Diagnostic, start: usize, end: usize) -> LocatedDiagnostic {
    let (line, column) = location(source, start);
    LocatedDiagnostic {
        code: d.code.clone(),
        message: d.message.clone(),
        byte_start: start,
        byte_end: end,
        line,
        column,
    }
}
/// Lexer/token/declaration/nested-segment limits are checked BEFORE recursive
/// parsing. Segment weight includes unfinished outer groups, preventing long
/// Pratt/operator spines hidden inside nested blocks from evading the guard.
pub fn preflight(source: &str, limits: DocTestLimits) -> Result<(), String> {
    limits.validate()?;
    if source.len() > limits.max_combined_bytes {
        return Err("doctest combined source byte limit".into());
    }
    let (tokens, _) = lexer::lex(source);
    let mut count = 0usize;
    let mut declarations = 0usize;
    // (unfinished outer segment, maximum completed statement, opener).
    let mut stack: Vec<(usize, usize, K)> = Vec::new();
    let mut segment = 0usize;
    let mut top_item = false;
    for token in tokens {
        if matches!(
            token.kind,
            K::Whitespace | K::Comment | K::BlockComment | K::Eof
        ) {
            continue;
        }
        count = count.checked_add(1).ok_or("token count overflow")?;
        if matches!(
            token.kind,
            K::Fn | K::Struct | K::Enum | K::Effect | K::Impl
        ) {
            declarations += 1;
        }
        if count > limits.max_tokens || declarations > limits.max_declarations {
            return Err("doctest token/declaration limit".into());
        }
        if stack.is_empty()
            && (matches!(
                token.kind,
                K::Fn | K::Struct | K::Enum | K::Effect | K::Impl | K::Copy | K::Affine | K::Linear
            ) || token.kind == K::Lower
                && matches!(&source[token.span.clone()], "trait" | "model" | "pub"))
        {
            top_item = true;
        }
        let mut completed_item = false;
        match token.kind {
            K::LParen | K::LBrace | K::LBracket => {
                stack.push((segment, 0, token.kind));
                segment = 0;
            }
            K::RParen | K::RBrace | K::RBracket => {
                let (outer, peak, opener) = stack.pop().unwrap_or((0, 0, token.kind));
                segment = outer
                    .checked_add(segment.max(peak))
                    .and_then(|n| n.checked_add(1))
                    .ok_or("segment overflow")?;
                completed_item =
                    stack.is_empty() && top_item && opener == K::LBrace && token.kind == K::RBrace;
            }
            K::Semi => {
                if let Some((_, peak, _)) = stack.last_mut() {
                    *peak = (*peak).max(segment);
                }
                segment = 0;
                if stack.is_empty() {
                    top_item = false;
                }
            }
            _ => segment = segment.checked_add(1).ok_or("segment overflow")?,
        }
        let weighted = stack.iter().try_fold(segment, |n, (outer, _, _)| {
            n.checked_add(*outer).ok_or("segment overflow")
        })?;
        if stack.len() > limits.max_nesting || weighted > limits.max_segment_tokens {
            return Err("doctest nesting/expression-segment limit".into());
        }
        if completed_item {
            segment = 0;
            top_item = false;
        }
    }
    Ok(())
}
fn fence(text: &str) -> Option<(char, usize, &str)> {
    let text = text.trim();
    let first = text.chars().next()?;
    if first != '`' && first != '~' {
        return None;
    }
    let count = text.chars().take_while(|c| *c == first).count();
    (count >= 3).then_some((first, count, text[count..].trim()))
}
/// Split fence info on commas outside parentheses.
fn attribute_parts(info: &str) -> Result<Vec<&str>, String> {
    let mut parts = Vec::new();
    let (mut depth, mut start) = (0usize, 0usize);
    for (i, c) in info.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth = depth
                    .checked_sub(1)
                    .ok_or("unbalanced doctest fence attribute")?
            }
            ',' if depth == 0 => {
                parts.push(info[start..i].trim());
                start = i + 1;
            }
            _ => (),
        }
    }
    if depth != 0 {
        return Err("unbalanced doctest fence attribute".into());
    }
    parts.push(info[start..].trim());
    Ok(parts)
}
type Attributes = (Option<String>, Option<String>, Option<SimSpec>);
fn attributes(info: &str) -> Result<Attributes, String> {
    let mut expected = None;
    let mut entry = None;
    let mut simulated = None;
    for part in attribute_parts(info)?.into_iter().skip(1) {
        if let Some(code) = part
            .strip_prefix("compile_fail(")
            .and_then(|s| s.strip_suffix(')'))
        {
            if expected.is_some()
                || code.len() != 5
                || !code.starts_with('E')
                || !code[1..].bytes().all(|b| b.is_ascii_digit())
            {
                return Err("invalid doctest diagnostic code".into());
            }
            expected = Some(code.to_string());
        } else if let Some(name) = part.strip_prefix("entry=") {
            if entry.is_some()
                || name.is_empty()
                || name.len() > 128
                || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                || name.as_bytes()[0].is_ascii_digit()
            {
                return Err("invalid doctest entry".into());
            }
            entry = Some(name.to_string());
        } else if let Some(inner) = part.strip_prefix("sim(").and_then(|s| s.strip_suffix(')')) {
            if simulated.is_some() {
                return Err("duplicate sim doctest attribute".into());
            }
            simulated = Some(SimSpec::parse(inner).map_err(|e| e.to_string())?);
        } else {
            return Err(format!(
                "unsupported Vesper doctest fence attribute '{part}'"
            ));
        }
    }
    if expected.is_some() && entry.is_some() {
        return Err("compile_fail doctest cannot request execution entry".into());
    }
    if expected.is_some() && simulated.is_some() {
        return Err("compile_fail doctest cannot request simulated execution".into());
    }
    Ok((expected, entry, simulated))
}
fn extract(source: &str, limits: DocTestLimits) -> Result<Vec<Example>, String> {
    let (tokens, _) = lexer::lex(source);
    let mut blocks: Vec<Vec<Line>> = Vec::new();
    let mut previous = None;
    for t in tokens {
        if t.kind != K::Comment {
            continue;
        }
        let raw = &source[t.span.clone()];
        if !(raw.starts_with("///") && !raw.starts_with("////") || raw.starts_with("//!")) {
            previous = None;
            continue;
        }
        let offset = t.span.start + 3 + usize::from(raw[3..].starts_with(' '));
        if previous.is_none_or(|end| !source[end..t.span.start].chars().all(char::is_whitespace)) {
            blocks.push(Vec::new());
        }
        blocks.last_mut().expect("new doc block").push(Line {
            text: source[offset..t.span.end].to_string(),
            offset,
        });
        previous = Some(t.span.end);
    }
    let mut examples = Vec::new();
    for block in blocks {
        let mut active: Option<(char, usize, Option<Example>)> = None;
        let mut code_bytes = 0usize;
        for line in block {
            if let Some((kind, width, example)) = &mut active {
                if fence(&line.text)
                    .is_some_and(|(c, n, info)| c == *kind && n >= *width && info.is_empty())
                {
                    if let Some(e) = example.take() {
                        examples.push(e);
                        if examples.len() > limits.max_examples {
                            return Err("doctest example limit".into());
                        }
                    }
                    active = None;
                    continue;
                }
                if let Some(e) = example {
                    code_bytes = code_bytes
                        .checked_add(line.text.len())
                        .and_then(|n| n.checked_add(1))
                        .ok_or("snippet byte overflow")?;
                    if code_bytes > limits.max_snippet_bytes {
                        return Err("doctest snippet byte limit".into());
                    }
                    e.lines.push(line);
                }
            } else if let Some((kind, width, info)) = fence(&line.text) {
                let language = info.split(',').next().unwrap_or("").trim();
                if language != "vesper" && language.split_whitespace().next() == Some("vesper") {
                    return Err("unsupported Vesper doctest fence metadata (use comma-separated attributes)".into());
                }
                let example = if language == "vesper" {
                    let (expected, entry, sim) = attributes(info)?;
                    Some(Example {
                        lines: Vec::new(),
                        offset: line.offset,
                        expected,
                        entry,
                        sim,
                    })
                } else {
                    None
                };
                code_bytes = 0;
                active = Some((kind, width, example));
            }
        }
        if active.is_some_and(|(_, _, e)| e.is_some()) {
            return Err("unterminated Vesper doctest fence".into());
        }
    }
    Ok(examples)
}
struct Generated {
    source: String,
    snippet_start: usize,
    code_start: usize,
    segments: Vec<(usize, usize, usize)>,
    entry: Option<String>,
}
fn generate(
    base: &str,
    e: &Example,
    wrapper: &str,
    limits: DocTestLimits,
    used: &mut usize,
) -> Result<Generated, String> {
    let mut source = format!("{base}\n");
    let snippet_start = source.len();
    let raw: String = e.lines.iter().map(|l| format!("{}\n", l.text)).collect();
    *used = used
        .checked_add(raw.len())
        .ok_or("doctest check-byte overflow")?;
    if *used > limits.max_total_check_bytes {
        return Err("doctest total check-byte limit".into());
    }
    preflight(&raw, limits)?;
    let (tokens, _) = lexer::lex(&raw);
    // Try the actual module grammar as well as recognizing malformed declaration
    // prefixes. Contextual trait/model/pub and leading use-model declarations
    // must never acquire artificial block-parser errors from a wrapper.
    let raw_module = parser::parse(&raw);
    let declaration = raw_module.diagnostics.is_empty()
        && (!raw_module.module.items.is_empty() || !raw_module.module.model_uses.is_empty())
        || tokens
            .iter()
            .find(|t| {
                !matches!(
                    t.kind,
                    K::Whitespace | K::Comment | K::BlockComment | K::Eof
                )
            })
            .is_some_and(|t| {
                matches!(
                    t.kind,
                    K::Fn
                        | K::Copy
                        | K::Affine
                        | K::Linear
                        | K::Struct
                        | K::Enum
                        | K::Effect
                        | K::Impl
                ) || t.kind == K::Lower && matches!(&raw[t.span.clone()], "trait" | "model" | "pub")
            });
    let entry = if e.expected.is_some() {
        None
    } else if declaration {
        Some(e.entry.clone().unwrap_or_else(|| "main".into()))
    } else {
        Some(e.entry.clone().unwrap_or_else(|| wrapper.to_string()))
    };
    if !declaration {
        // A simulated body fence may use exactly the devices it declared.
        let row = e.sim.as_ref().map_or(String::new(), |spec| {
            let names: Vec<_> = spec.devices.iter().map(|d| d.effect()).collect();
            format!(" eff [{}]", names.join(", "))
        });
        source.push_str(&format!(
            "fn {}(){row} {{\n",
            entry.as_deref().unwrap_or(wrapper)
        ));
    }
    let code_start = source.len();
    let mut segments = Vec::new();
    for line in &e.lines {
        let start = source.len();
        source.push_str(&line.text);
        segments.push((start, source.len(), line.offset));
        source.push('\n');
    }
    if !declaration {
        source.push_str("}\n");
    }
    Ok(Generated {
        source,
        snippet_start,
        code_start,
        segments,
        entry,
    })
}

fn map_offset(e: &Example, generated: &Generated, offset: usize) -> usize {
    for &(start, end, original) in &generated.segments {
        if offset >= start && offset <= end {
            return original + offset - start;
        }
    }
    if offset < generated.code_start {
        e.lines.first().map_or(e.offset, |l| l.offset)
    } else {
        e.lines.last().map_or(e.offset, |l| l.offset + l.text.len())
    }
}
/// Conservative expansion guard BEFORE World::mode/checking. Repeated nominal
/// fields unfold independently against one global budget. Recursive nominal
/// types are explicitly unsupported: a name-only cycle shortcut would not bound
/// the checker's mode-keyed generic cycle exploration.
fn nominal_expansion(module: &crate::ast::Module, limit: usize) -> Result<(), String> {
    use crate::ast::{Adt, Item, Type};
    use std::collections::BTreeMap;
    let adts: BTreeMap<_, _> = module
        .items
        .iter()
        .filter_map(|i| {
            if let Item::Adt(a) = i {
                Some((a.name.as_str(), a))
            } else {
                None
            }
        })
        .collect();
    fn visit<'a>(
        ty: &Type,
        adts: &BTreeMap<&'a str, &'a Adt>,
        path: &mut Vec<String>,
        remaining: &mut usize,
        depth: usize,
    ) -> Result<(), String> {
        *remaining = remaining
            .checked_sub(1)
            .ok_or("doctest nominal expansion limit")?;
        if depth > 48 {
            return Err("doctest nominal expansion depth limit".into());
        }
        match ty {
            Type::Named(name, args) => {
                for ty in args {
                    visit(ty, adts, path, remaining, depth + 1)?;
                }
                if let Some(adt) = adts.get(name.as_str()) {
                    if path.contains(name) {
                        return Err(
                            "recursive nominal doctest types are unsupported before checking"
                                .into(),
                        );
                    }
                    path.push(name.clone());
                    for field in adt.variants.iter().flat_map(|v| &v.fields) {
                        visit(&field.ty, adts, path, remaining, depth + 1)?;
                    }
                    path.pop();
                }
            }
            Type::Tuple(types) => {
                for ty in types {
                    visit(ty, adts, path, remaining, depth + 1)?;
                }
            }
            Type::Function { params, result, .. } => {
                for (_, ty) in params {
                    visit(ty, adts, path, remaining, depth + 1)?;
                }
                visit(result, adts, path, remaining, depth + 1)?;
            }
            _ => {}
        }
        Ok(())
    }
    let mut remaining = limit;
    for name in adts.keys() {
        visit(
            &Type::named(name),
            &adts,
            &mut Vec::new(),
            &mut remaining,
            0,
        )?;
    }
    Ok(())
}
/// Fail-closed source profile, called after parsing and BEFORE checker inference.
/// This guard never invokes the checker. Inferred composite growth is not modeled:
/// nonempty tuples, generic producers/applications, closures and handlers are
/// unsupported. Closed nongeneric nominal data (including flat linear cleanup)
/// remains available under the separate nominal expansion bound.
pub fn source_check_guard(
    module: &crate::ast::Module,
    limits: DocTestLimits,
) -> Result<(), String> {
    use crate::ast::*;
    limits.validate()?;
    enum Node<'a> {
        Expr(&'a Expr, usize),
        Ty(&'a Type, usize, bool),
        Pat(&'a Pattern, usize),
    }
    fn function<'a>(
        f: &'a Function,
        implicit_self: bool,
        todo: &mut Vec<Node<'a>>,
    ) -> Result<(), String> {
        if !f.generics.is_empty() {
            return Err(
                "doctest inferred composite growth: generic functions unsupported before checking"
                    .into(),
            );
        }
        todo.push(Node::Ty(&f.result, 1, false));
        for p in &f.params {
            if implicit_self && p.name == "self" && p.ty == Type::Infer {
                continue;
            }
            todo.push(Node::Ty(&p.ty, 1, false));
        }
        for effect in &f.effects.instances {
            for ty in &effect.args {
                todo.push(Node::Ty(ty, 1, false));
            }
        }
        if let Some(body) = &f.body {
            todo.push(Node::Expr(body, 1));
        }
        Ok(())
    }
    fn builtin_constructor(name: &str) -> bool {
        matches!(
            name.rsplit("::").next(),
            Some("Some" | "None" | "Ok" | "Err" | "Option" | "Result")
        )
    }
    if module.items.len() > limits.max_declarations {
        return Err("doctest parsed module item limit".into());
    }
    let mut todo = Vec::new();
    for item in &module.items {
        match item {
            Item::Function(f) => function(f, false, &mut todo)?,
            Item::Adt(a) => {
                if !a.generics.is_empty() {
                    return Err("doctest inferred composite growth: generic nominal types unsupported before checking".into());
                }
                for field in a.variants.iter().flat_map(|v| &v.fields) {
                    todo.push(Node::Ty(&field.ty, 1, false));
                }
            }
            Item::Effect(e) => {
                if !e.generics.is_empty() {
                    return Err(
                        "doctest generic effect declarations unsupported before checking".into(),
                    );
                }
                for f in &e.ops {
                    function(f, false, &mut todo)?;
                }
            }
            Item::Impl {
                ty,
                generics,
                methods,
            } => {
                if !generics.is_empty() {
                    return Err(
                        "doctest generic implementations unsupported before checking".into(),
                    );
                }
                todo.push(Node::Ty(ty, 1, false));
                for f in methods {
                    function(f, true, &mut todo)?;
                }
            }
            Item::Trait(_) | Item::Conformance(_) => {
                return Err("doctest trait/model inference unsupported before checking".into());
            }
        }
        if todo.len() > 16384 {
            return Err("doctest parsed AST volume limit".into());
        }
    }
    let mut count = 0usize;
    while let Some(node) = todo.pop() {
        count = count.checked_add(1).ok_or("parsed AST count overflow")?;
        if count > 16384 {
            return Err("doctest parsed AST volume limit".into());
        }
        let depth = match &node {
            Node::Expr(_, d) | Node::Ty(_, d, _) | Node::Pat(_, d) => *d,
        };
        if depth > 64 {
            return Err("doctest parsed AST depth limit".into());
        }
        let next = depth.checked_add(1).ok_or("parsed AST depth overflow")?;
        match node {
            Node::Ty(ty,_,allow_infer) => match ty {
                Type::Tuple(ts) if !ts.is_empty() => return Err("doctest inferred composite growth: nonempty tuple types unsupported before checking".into()),
                Type::Named(name,args) if !args.is_empty() || matches!(name.as_str(), "Option" | "Result") => return Err("doctest inferred composite growth: generic type applications unsupported before checking".into()),
                Type::Infer if !allow_infer => return Err("doctest inferred function/field signatures unsupported before checking".into()),
                Type::Function { params, result, effects, .. } => {
                    for (_,t) in params { todo.push(Node::Ty(t,next,false)); }
                    todo.push(Node::Ty(result,next,false));
                    for e in &effects.instances { for ty in &e.args { todo.push(Node::Ty(ty,next,false)); } }
                }
                Type::Row(effects) => for e in &effects.instances { for ty in &e.args { todo.push(Node::Ty(ty,next,false)); } },
                _ => {}
            },
            Node::Pat(p,_) => match &p.kind {
                PatternKind::Tuple(ps) if !ps.is_empty() => return Err("doctest inferred composite growth: tuple patterns unsupported before checking".into()),
                PatternKind::Constructor { name,fields,.. } => {
                    if builtin_constructor(name) { return Err("doctest inferred composite growth: builtin generic constructors unsupported before checking".into()); }
                    for (_,p) in fields { todo.push(Node::Pat(p,next)); }
                }
                _ => {}
            },
            Node::Expr(expr,_) => match &expr.kind {
                ExprKind::Tuple(es) if !es.is_empty() => return Err("doctest inferred composite growth: nonempty tuples unsupported before checking".into()),
                ExprKind::Closure { .. } | ExprKind::Handle { .. } => return Err("doctest inferred composite growth: closures/handlers unsupported before checking".into()),
                ExprKind::Name(name) if builtin_constructor(name) => return Err("doctest inferred composite growth: builtin generic constructors unsupported before checking".into()),
                ExprKind::Struct { name, fields } => {
                    if builtin_constructor(name) { return Err("doctest builtin generic constructors unsupported before checking".into()); }
                    for (_,e) in fields { todo.push(Node::Expr(e,next)); }
                }
                ExprKind::Block { statements,tail } => {
                    for statement in statements { match statement {
                        Statement::Let {pat,ty,value} => { todo.push(Node::Pat(pat,next)); todo.push(Node::Ty(ty,next,true)); todo.push(Node::Expr(value,next)); }
                        Statement::Expr(e) => todo.push(Node::Expr(e,next)),
                    } }
                    if let Some(e) = tail { todo.push(Node::Expr(e,next)); }
                }
                ExprKind::Call { callee,args,types } => {
                    if !types.is_empty() { return Err("doctest inferred composite growth: generic calls unsupported before checking".into()); }
                    todo.push(Node::Expr(callee,next));
                    for a in args { todo.push(Node::Expr(&a.expr,next)); }
                }
                ExprKind::Field(_,name) if builtin_constructor(name) => return Err("doctest inferred composite growth: builtin generic constructors unsupported before checking".into()),
                ExprKind::Unary(_,e) | ExprKind::Field(e,_) | ExprKind::Try(e) => todo.push(Node::Expr(e,next)),
                ExprKind::Binary(_,a,b) | ExprKind::Assign(_,a,b) | ExprKind::Index(a,b) => { todo.push(Node::Expr(a,next)); todo.push(Node::Expr(b,next)); }
                ExprKind::If { condition,then,otherwise } => { todo.push(Node::Expr(condition,next)); todo.push(Node::Expr(then,next)); if let Some(e) = otherwise { todo.push(Node::Expr(e,next)); } }
                ExprKind::Match { value,arms } => { todo.push(Node::Expr(value,next)); for (p,e) in arms { todo.push(Node::Pat(p,next)); todo.push(Node::Expr(e,next)); } }
                _ => {}
            },
        }
        if count
            .checked_add(todo.len())
            .ok_or("parsed AST count overflow")?
            > 16384
        {
            return Err("doctest parsed AST volume limit".into());
        }
    }
    nominal_expansion(module, limits.max_nominal_expansion)
}
fn checked(
    source: &str,
    limits: DocTestLimits,
    used: &mut usize,
) -> Result<(check::Checked, Vec<Diagnostic>), String> {
    *used = used
        .checked_add(source.len())
        .ok_or("doctest check-byte overflow")?;
    if *used > limits.max_total_check_bytes {
        return Err("doctest total check-byte limit".into());
    }
    preflight(source, limits)?;
    let parsed = parser::parse(source);
    if !parsed.diagnostics.is_empty() {
        // A default checked empty world is not used for execution on this path.
        return Ok((check::check(&parser::parse("").module), parsed.diagnostics));
    }
    source_check_guard(&parsed.module, limits)?;
    let checked = check::check(&parsed.module);
    let diagnostics = checked.diagnostics.clone();
    Ok((checked, diagnostics))
}
/// Every effect of the entry must be a declared simulated device whose source
/// declaration matches the Codex operation contract exactly.
fn sim_contract(world: &check::World, entry: &str, spec: &SimSpec) -> Result<(), String> {
    use crate::ast::Type;
    let function = world
        .functions
        .get(entry)
        .ok_or("missing simulated entry")?;
    if function.effects.tail.is_some() {
        return Err("simulated entry requires a closed effect row".into());
    }
    let u64_type = |t: &Type| matches!(t, Type::Named(n, a) if n == "u64" && a.is_empty());
    for instance in &function.effects.instances {
        let device = sim::Device::from_effect(&instance.name)
            .filter(|d| spec.devices.contains(d))
            .ok_or_else(|| {
                format!(
                    "effect {} is not a simulated device declared by this doctest",
                    instance.name
                )
            })?;
        let declaration = &world.effects[&instance.name];
        if !declaration.linear || !declaration.generics.is_empty() || !instance.args.is_empty() {
            return Err(format!(
                "simulated device {} must be a non-generic linear effect",
                instance.name
            ));
        }
        for op in &declaration.ops {
            let arity = sim::operation_arity(device, &op.name).ok_or_else(|| {
                format!(
                    "{}::{} is not a simulated operation",
                    instance.name, op.name
                )
            })?;
            if op.params.len() != arity
                || !op
                    .params
                    .iter()
                    .all(|p| u64_type(&p.ty) && p.pass == crate::ast::Pass::In)
                || !u64_type(&op.result)
                || !op.effects.instances.is_empty()
                || op.effects.tail.is_some()
            {
                return Err(format!(
                    "{}::{} must take {arity} u64 argument(s) and return u64",
                    instance.name, op.name
                ));
            }
        }
    }
    Ok(())
}
fn simulate_once(
    world: &check::World,
    entry: &str,
    mut host: SimHost,
) -> Result<(String, SimOutcome), String> {
    use crate::runtime::{Execution, Runtime, Value};
    let mut runtime = Runtime::new(world);
    runtime.fuel = 1_000_000;
    let mut state = runtime.begin_external(entry, vec![])?;
    loop {
        match state {
            Execution::Complete(value) => {
                return Ok((value.to_string(), host.finish().map_err(|e| e.to_string())?));
            }
            Execution::Suspended(suspension) => {
                let request = suspension.request();
                let arguments = request
                    .arguments
                    .iter()
                    .map(|v| match v {
                        Value::UInt(n, 64) => Ok(*n),
                        _ => Err("simulated operations take u64 arguments".to_string()),
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let effect = request.effect.name.clone();
                let operation = request.operation.clone();
                let value = host
                    .call(&effect, &operation, &arguments)
                    .map_err(|e| e.to_string())?;
                state = suspension.resume(Value::UInt(value, 64))?;
            }
        }
    }
}
/// Record one run, then replay it against its own SimPulse journal.
fn simulate(
    world: &check::World,
    entry: &str,
    spec: &SimSpec,
    limits: DocTestLimits,
) -> Result<(String, SimulationReport), String> {
    sim_contract(world, entry, spec)?;
    let host_limits = SimLimits::default();
    let host = SimHost::new(spec.clone(), host_limits).map_err(|e| e.to_string())?;
    let (value, outcome) = simulate_once(world, entry, host)?;
    let runs = limits.runs_per_example.max(2);
    for _ in 1..runs {
        let replay = SimHost::replay(spec.clone(), host_limits, outcome.journal.clone())
            .map_err(|e| e.to_string())?;
        let (again, replayed) = simulate_once(world, entry, replay)?;
        if again != value || replayed != outcome {
            return Err("replayed simulation diverged from the recording".into());
        }
    }
    Ok((
        value,
        SimulationReport {
            spec: spec.clone(),
            runs,
            replayed: true,
            outcome,
        },
    ))
}
/// Whether `source` holds at least one Vesper doc-comment fence. Package runs
/// use this to skip files without examples before any checking; malformed
/// fences are still errors.
pub fn has_examples(source: &str, limits: DocTestLimits) -> Result<bool, String> {
    limits.validate()?;
    if source.len() > limits.max_source_bytes {
        return Err("doctest source byte limit".into());
    }
    preflight(source, limits)?;
    Ok(!extract(source, limits)?.is_empty())
}
/// Body fences are Unit blocks. Declaration fences use explicit `entry=name`
/// or `main` by default; positive declaration names must be unique in the merged
/// package. Unsupported syntax/execution is an explicit failure, never a skip.
pub fn run(source: &str, limits: DocTestLimits) -> Result<DocTestReport, String> {
    limits.validate()?;
    if source.len() > limits.max_source_bytes {
        return Err("doctest source byte limit".into());
    }
    let mut checked_bytes = 0usize;
    let (base_checked, base_errors) = checked(source, limits, &mut checked_bytes)?;
    let mut report = DocTestReport {
        passed: false,
        base_diagnostics: base_errors
            .iter()
            .map(|d| diagnostic(source, d, d.span.start, d.span.end))
            .collect(),
        examples: Vec::new(),
        batch: None,
    };
    if !base_errors.is_empty() {
        return Ok(report);
    }
    let examples = extract(source, limits)?;
    if examples.is_empty() {
        return Err("no Vesper documentation examples found".into());
    }
    let mut merged = source.to_string();
    let mut runnable = Vec::new();
    let mut reserved: std::collections::BTreeSet<String> =
        base_checked.world.functions.keys().cloned().collect();
    // Private wrappers must not collide with identifiers authored in any other
    // example either; all snippet bytes are already bounded by extraction.
    for example in &examples {
        if let Some(entry) = &example.entry {
            reserved.insert(entry.clone());
        }
        let raw: String = example
            .lines
            .iter()
            .map(|l| format!("{}\n", l.text))
            .collect();
        for token in lexer::lex(&raw).0 {
            if matches!(token.kind, K::Lower | K::Upper) {
                reserved.insert(raw[token.span].to_string());
            }
        }
    }
    for (index, example) in examples.iter().enumerate() {
        let mut wrapper = format!("__vesper_doctest_{index}");
        while !reserved.insert(wrapper.clone()) {
            wrapper.push('_');
            if wrapper.len() > 128 {
                return Err("doctest private wrapper name limit".into());
            }
        }
        let generated = generate(source, example, &wrapper, limits, &mut checked_bytes)?;
        let (snippet_checked, errors) = checked(&generated.source, limits, &mut checked_bytes)?;
        let own_entry = generated.entry.as_ref().is_some_and(|entry| {
            snippet_checked.world.functions.get(entry).is_some_and(|f| {
                f.span.start >= generated.snippet_start && f.span.end <= generated.source.len()
            })
        });
        let (line, column) = location(source, example.offset);
        let local_only = errors.iter().all(|d| {
            generated
                .segments
                .iter()
                .any(|(start, end, _)| d.span.start >= *start && d.span.start <= *end)
        });
        let diagnostics = errors
            .iter()
            .map(|d| {
                if d.span.start < generated.snippet_start {
                    diagnostic(source, d, d.span.start, d.span.end)
                } else {
                    diagnostic(
                        source,
                        d,
                        map_offset(example, &generated, d.span.start),
                        map_offset(example, &generated, d.span.end),
                    )
                }
            })
            .collect();
        let mut result = ExampleReport {
            index,
            line,
            column,
            byte_start: example.offset,
            passed: false,
            expected_code: example.expected.clone(),
            entry: generated.entry.clone(),
            result: None,
            message: String::new(),
            diagnostics,
            simulation: None,
        };
        if let Some(expected) = &example.expected {
            result.passed =
                !errors.is_empty() && local_only && errors.iter().all(|d| d.code == *expected);
            result.message = if result.passed { "matched exact diagnostic code" } else { "expected exact snippet diagnostic code; unrelated/base errors cannot satisfy compile_fail" }.into();
        } else if !errors.is_empty() {
            result.message = "positive example failed full checking".into();
        } else if !own_entry {
            result.message = "positive entry must be defined by this snippet, not inherited from the base package".into();
        } else if let Some(spec) = &example.sim {
            let entry = generated.entry.as_deref().expect("positive entry");
            match simulate(&snippet_checked.world, entry, spec, limits) {
                Ok((value, simulation)) => {
                    result.passed = true;
                    result.result = Some(value);
                    result.message =
                        "simulated twice under SimPulse; journal replay matched".into();
                    result.simulation = Some(simulation);
                }
                Err(error) => result.message = format!("simulated execution failed: {error}"),
            }
        } else {
            let code = &generated.source[generated.snippet_start..];
            let size = merged
                .len()
                .checked_add(code.len())
                .and_then(|n| n.checked_add(1))
                .ok_or("merged byte overflow")?;
            if size > limits.max_combined_bytes {
                return Err("doctest merged source byte limit".into());
            }
            merged.push('\n');
            let fragment_start = merged.len();
            merged.push_str(code);
            runnable.push((
                index,
                generated.entry.expect("positive entry"),
                fragment_start..merged.len(),
            ));
            result.message = "checked; awaiting shared native execution".into();
        }
        report.examples.push(result);
    }
    if !runnable.is_empty() {
        let (checked, errors) = checked(&merged, limits, &mut checked_bytes)?;
        let valid_origins = runnable.iter().all(|(_, entry, range)| {
            checked
                .world
                .functions
                .get(entry)
                .is_some_and(|f| f.span.start >= range.start && f.span.end <= range.end)
        });
        if !errors.is_empty() || !valid_origins {
            for (index, _, _) in &runnable {
                report.examples[*index].message = format!(
                    "merged positive module check/provenance failed: {}",
                    errors
                        .iter()
                        .map(|d| d.code.as_str())
                        .collect::<Vec<_>>()
                        .join(",")
                );
            }
        } else {
            let entries: Vec<_> = runnable.iter().map(|(_, n, _)| n.as_str()).collect();
            match NativeBatch::compile(&checked.world, &entries, limits.batch) {
                Err(error) => {
                    for (index, _, _) in &runnable {
                        report.examples[*index].message =
                            format!("unsupported native execution: {error}");
                    }
                }
                Ok(mut batch) => {
                    for (index, entry, _) in &runnable {
                        let mut first = None;
                        let mut failed = None;
                        for _ in 0..limits.runs_per_example {
                            match batch.run(entry) {
                                Ok(value) => {
                                    let value = value.to_string();
                                    if first.as_ref().is_some_and(|x| x != &value) {
                                        failed = Some("repeated native results differ".into());
                                        break;
                                    }
                                    first = Some(value);
                                }
                                Err(error) => {
                                    failed = Some(error);
                                    break;
                                }
                            }
                        }
                        let result = &mut report.examples[*index];
                        result.passed = failed.is_none();
                        result.result = first;
                        result.message = failed
                            .unwrap_or_else(|| "executed repeatedly in shared JIT module".into());
                    }
                    report.batch = Some(batch.stats().into());
                }
            }
        }
    }
    report.passed = report.examples.iter().all(|e| e.passed);
    Ok(report)
}
