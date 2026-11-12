//! Pure checked-source lint preparation; performs no filesystem or subprocess I/O.
use crate::{
    ast::{self, Effect, Pass, Type},
    check, interface, ir,
    lexer::{self, Token, TokenKind as K},
    parser,
};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
pub use vesper_lint::policy::{Policy, PolicyLimits, PolicyUsage, RuleStatus, Severity};
use vesper_lint::{
    Action, CheckedProgram, Finding, FindingKind, FunctionFacts, LintReport, ParameterFacts,
    SourceEdit,
};
const MAX_BYTES: usize = 262144;
#[derive(Debug, Serialize)]
pub struct ReportedFinding {
    #[serde(flatten)]
    pub finding: Finding,
    pub fixable: bool,
    pub severity: Severity,
}
#[derive(Debug, Serialize)]
pub struct SourceReport {
    pub schema_version: u32,
    pub status: &'static str,
    pub diagnostics: Vec<ReportedFinding>,
    pub skipped: Vec<vesper_lint::Skipped>,
    pub changed: bool,
    pub applied_edits: usize,
    pub semver: Option<vesper_semver::Report>,
    pub work: usize,
    pub policy: PolicyReport,
    pub outcome: PolicyOutcome,
}
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConfigProvenance {
    Default,
    Explicit {
        path: String,
        bytes: usize,
        blake3: String,
    },
}
#[derive(Debug, Serialize)]
pub struct PolicyReport {
    pub provenance: ConfigProvenance,
    pub rules: Vec<RuleStatus>,
    pub unavailable: Vec<RuleStatus>,
    pub plugin_packs_supported: bool,
    pub usage: PolicyUsage,
}
#[derive(Debug, Serialize, Default)]
pub struct PolicyOutcome {
    pub remaining_denied: usize,
    pub remaining_warnings: usize,
    pub suppressed: usize,
    pub excluded_generated_bodies: usize,
    pub unavailable_denied_rules: Vec<vesper_lint::policy::LintName>,
}
pub struct PreparedLint {
    pub report: SourceReport,
    pub candidate: Option<String>,
}
struct SourceFacts {
    checked: check::Checked,
    facts: CheckedProgram,
    tokens: Vec<Token>,
}
fn trivia(kind: K) -> bool {
    matches!(kind, K::Whitespace | K::Comment | K::BlockComment)
}
fn preflight(source: &str) -> Result<Vec<Token>, String> {
    if source.len() > MAX_BYTES {
        return Err("lint source exceeds 256 KiB".into());
    }
    let (tokens, errors) = lexer::lex(source);
    if !errors.is_empty() {
        return Err(format!("lexical errors: {errors:?}"));
    }
    if tokens.len() > 8192 {
        return Err("lint token limit (8192)".into());
    }
    let mut depth = 0usize;
    let mut run = 0usize;
    let mut branches = 0usize;
    let mut recursive_tokens = 0usize;
    for token in &tokens {
        if trivia(token.kind) {
            continue;
        }
        run += 1;
        if matches!(
            token.kind,
            K::LParen
                | K::Dot
                | K::Question
                | K::Arrow
                | K::Eq
                | K::PlusEq
                | K::MinusEq
                | K::StarEq
                | K::SlashEq
                | K::PercentEq
                | K::Plus
                | K::Minus
                | K::Star
                | K::Slash
                | K::Percent
                | K::EqEq
                | K::Ne
                | K::Lt
                | K::Le
                | K::Gt
                | K::Ge
                | K::And
                | K::Or
                | K::Bang
                | K::Pipe
        ) {
            recursive_tokens += 1;
            if recursive_tokens > 256 {
                return Err("lint recursive-expression token budget (256)".into());
            }
        }
        if matches!(token.kind, K::Upper | K::Lower) && token.span.len() > 128 {
            return Err("lint identifier byte limit (128)".into());
        }
        if token.kind == K::If {
            branches += 1;
            if branches > 64 {
                return Err("lint conditional limit (64), including else-if recursion".into());
            }
        }
        match token.kind {
            K::LParen | K::LBracket | K::LBrace => {
                depth += 1;
                if depth > 32 {
                    return Err("lint nesting limit (32)".into());
                }
            }
            K::RParen | K::RBracket | K::RBrace => depth = depth.saturating_sub(1),
            _ => (),
        }
        if matches!(token.kind, K::Semi | K::LBrace | K::RBrace) {
            run = 0;
        }
        if run > 256 {
            return Err("lint expression/signature token-run limit (256)".into());
        }
    }
    Ok(tokens)
}
// Bound nominal structural-mode expansion before invoking the unchanged checker.
// Repeated sibling occurrences are charged, never memoized as one AST node.
// Recursive nominal contracts are conservatively unsupported here: the core
// mode solver's key also contains generic argument modes, not just nominal name.
fn surface_expansion(module: &ast::Module) -> Result<(), String> {
    if module.items.len() > 256 {
        return Err("lint source item limit (256)".into());
    }
    // Substitution clones an inferred actual type at each formal occurrence.
    // At most one generic occurrence in a result/field prevents compact
    // dup(dup(...)) or field-projection chains from doubling inferred trees.
    fn occurrences(ty: &Type, variables: &BTreeSet<String>) -> Result<usize, String> {
        let mut count = 0usize;
        let mut stack = vec![ty];
        while let Some(ty) = stack.pop() {
            match ty {
                Type::Infer | Type::IntLiteral(_) => {
                    return Err("lint unresolved result/type inference unsupported".into());
                }
                Type::Named(name, args) => {
                    if name == ast::PROJECTION_TYPE {
                        return Err("lint associated type expansion unsupported".into());
                    }
                    if variables.contains(name) {
                        count += 1;
                    }
                    stack.extend(args);
                }
                Type::Tuple(types) => stack.extend(types),
                Type::Function {
                    params,
                    result,
                    effects,
                    ..
                } => {
                    stack.extend(params.iter().map(|(_, ty)| ty));
                    stack.push(result);
                    for e in &effects.instances {
                        stack.extend(&e.args);
                    }
                    if effects
                        .tail
                        .as_ref()
                        .is_some_and(|name| variables.contains(name))
                    {
                        count += 1;
                    }
                }
                Type::Row(row) => {
                    for e in &row.instances {
                        stack.extend(&e.args);
                    }
                    if row
                        .tail
                        .as_ref()
                        .is_some_and(|name| variables.contains(name))
                    {
                        count += 1;
                    }
                }
            }
            if count > 1 {
                return Err("lint duplicating generic result/field inference unsupported".into());
            }
        }
        Ok(count)
    }
    for item in &module.items {
        if let ast::Item::Adt(a) = item {
            if a.generics.len() > 1 {
                return Err("lint multi-parameter generic ADT inference unsupported".into());
            }
            let variables = a.generics.iter().map(|g| g.name.clone()).collect();
            for field in a.variants.iter().flat_map(|v| &v.fields) {
                occurrences(&field.ty, &variables)?;
            }
        }
    }
    let adts: BTreeMap<_, _> = module
        .items
        .iter()
        .filter_map(|item| match item {
            ast::Item::Adt(a) => Some((a.name.as_str(), a)),
            _ => None,
        })
        .collect();
    struct Expansion<'a> {
        adts: BTreeMap<&'a str, &'a ast::Adt>,
        remaining: usize,
    }
    impl Expansion<'_> {
        fn visit(&mut self, ty: &Type, path: &mut Vec<String>, depth: usize) -> Result<(), String> {
            self.remaining = self
                .remaining
                .checked_sub(1)
                .ok_or("lint nominal expansion budget (8192)")?;
            if depth > 32 {
                return Err("lint nominal expansion depth (32)".into());
            }
            match ty {
                Type::Named(name, args) => {
                    for ty in args {
                        self.visit(ty, path, depth + 1)?;
                    }
                    if let Some(adt) = self.adts.get(name.as_str()).copied() {
                        if path.contains(name) {
                            return Err("lint recursive nominal expansion unsupported".into());
                        }
                        path.push(name.clone());
                        for field in adt.variants.iter().flat_map(|v| &v.fields) {
                            self.visit(&field.ty, path, depth + 1)?;
                        }
                        path.pop();
                    }
                }
                Type::Tuple(types) => {
                    for ty in types {
                        self.visit(ty, path, depth + 1)?;
                    }
                }
                Type::Function {
                    params,
                    result,
                    effects,
                    ..
                } => {
                    for (_, ty) in params {
                        self.visit(ty, path, depth + 1)?;
                    }
                    self.visit(result, path, depth + 1)?;
                    for ty in effects.instances.iter().flat_map(|e| &e.args) {
                        self.visit(ty, path, depth + 1)?;
                    }
                }
                Type::Row(row) => {
                    for ty in row.instances.iter().flat_map(|e| &e.args) {
                        self.visit(ty, path, depth + 1)?;
                    }
                }
                Type::Infer | Type::IntLiteral(_) => (),
            }
            Ok(())
        }
        fn ty(&mut self, ty: &Type) -> Result<(), String> {
            self.visit(ty, &mut vec![], 0)
        }
        fn function(&mut self, f: &ast::Function, outer: &[ast::Generic]) -> Result<(), String> {
            let mut variables: BTreeSet<String> = outer
                .iter()
                .chain(&f.generics)
                .map(|g| g.name.clone())
                .collect();
            variables.insert("Self".into());
            occurrences(&f.result, &variables)?;
            self.ty(&f.result)?;
            for p in &f.params {
                self.ty(&p.ty)?;
            }
            for ty in f.effects.instances.iter().flat_map(|e| &e.args) {
                self.ty(ty)?;
            }
            let mut stack: Vec<&ast::Expr> = f.body.iter().collect();
            while let Some(e) = stack.pop() {
                match &e.kind {
                    ast::ExprKind::Tuple(es) => {
                        if es.len() > 1
                            && es
                                .iter()
                                .any(|e| !matches!(e.kind, ast::ExprKind::Literal(_)))
                        {
                            return Err(
                                "lint inferred composite tuple construction unsupported".into()
                            );
                        }
                        stack.extend(es);
                    }
                    ast::ExprKind::Struct { fields, .. } => {
                        stack.extend(fields.iter().map(|(_, e)| e))
                    }
                    ast::ExprKind::Block { statements, tail } => {
                        for statement in statements {
                            match statement {
                                ast::Statement::Let { ty, value, .. } => {
                                    self.ty(ty)?;
                                    stack.push(value);
                                }
                                ast::Statement::Expr(e) => stack.push(e),
                            }
                        }
                        stack.extend(tail.iter().map(|e| e.as_ref()));
                    }
                    ast::ExprKind::Call {
                        callee,
                        types,
                        args,
                    } => {
                        if matches!(&callee.kind, ast::ExprKind::Name(name) if matches!(name.as_str(), "Ok" | "Err" | "Result::Ok" | "Result::Err"))
                            && (types.len() != 2
                                || types
                                    .iter()
                                    .any(|ty| occurrences(ty, &variables).ok() != Some(0)))
                        {
                            return Err(
                                "lint Result construction requires explicit closed type arguments"
                                    .into(),
                            );
                        }
                        stack.push(callee);
                        for ty in types {
                            self.ty(ty)?;
                        }
                        stack.extend(args.iter().map(|a| &a.expr));
                    }
                    ast::ExprKind::Field(e, _)
                    | ast::ExprKind::Try(e)
                    | ast::ExprKind::Unary(_, e) => stack.push(e),
                    ast::ExprKind::Index(a, b)
                    | ast::ExprKind::Binary(_, a, b)
                    | ast::ExprKind::Assign(_, a, b) => {
                        stack.push(a);
                        stack.push(b);
                    }
                    ast::ExprKind::If {
                        condition,
                        then,
                        otherwise,
                    } => {
                        stack.push(condition);
                        stack.push(then);
                        stack.extend(otherwise.iter().map(|e| e.as_ref()));
                    }
                    ast::ExprKind::Match { value, arms } => {
                        stack.push(value);
                        stack.extend(arms.iter().map(|(_, e)| e));
                    }
                    ast::ExprKind::Closure {
                        params,
                        result,
                        effects,
                        body,
                        ..
                    } => {
                        for p in params {
                            self.ty(&p.ty)?;
                        }
                        self.ty(result)?;
                        for ty in effects.instances.iter().flat_map(|e| &e.args) {
                            self.ty(ty)?;
                        }
                        stack.push(body);
                    }
                    ast::ExprKind::Handle {
                        effect,
                        state,
                        body,
                        ret,
                        ops,
                    } => {
                        for ty in &effect.args {
                            self.ty(ty)?;
                        }
                        if let Some((_, initial)) = state {
                            stack.push(initial);
                        }
                        stack.push(body);
                        stack.push(&ret.body);
                        stack.extend(ops.iter().map(|c| &c.body));
                    }
                    ast::ExprKind::Literal(_) | ast::ExprKind::Name(_) => (),
                }
            }
            Ok(())
        }
    }
    let mut expansion = Expansion {
        adts,
        remaining: 8192,
    };
    for name in expansion.adts.keys().copied().collect::<Vec<_>>() {
        expansion.ty(&Type::named(name))?;
    }
    for item in &module.items {
        match item {
            ast::Item::Function(f) => expansion.function(f, &[])?,
            ast::Item::Effect(e) => {
                for op in &e.ops {
                    expansion.function(op, &e.generics)?;
                }
            }
            ast::Item::Impl {
                ty,
                methods,
                generics,
            } => {
                if !generics.is_empty() {
                    return Err("lint generic inherent impl inference unsupported".into());
                }
                expansion.ty(ty)?;
                for f in methods {
                    expansion.function(f, generics)?;
                }
            }
            ast::Item::Trait(trait_decl) => {
                if !trait_decl.assoc.is_empty() {
                    return Err("lint associated type expansion unsupported".into());
                }
                for f in &trait_decl.methods {
                    expansion.function(f, &trait_decl.generics)?;
                }
            }
            ast::Item::Conformance(c) => {
                if !c.generics.is_empty() || !c.assoc.is_empty() {
                    return Err("lint generic conformance/associated inference unsupported".into());
                }
                expansion.ty(&c.self_ty)?;
                for ty in &c.trait_ref.args {
                    expansion.ty(ty)?;
                }
                for clause in &c.where_clauses {
                    expansion.ty(&clause.ty)?;
                    for ty in &clause.trait_ref.args {
                        expansion.ty(ty)?;
                    }
                }
                for f in &c.methods {
                    expansion.function(f, &c.generics)?;
                }
            }
            _ => (),
        }
    }
    Ok(())
}

fn checked_source(source: &str) -> Result<SourceFacts, String> {
    let tokens = preflight(source)?;
    let parsed = parser::parse(source);
    if !parsed.diagnostics.is_empty() {
        return Err(format!("syntax errors: {:?}", parsed.diagnostics));
    }
    surface_expansion(&parsed.module)?;
    let checked = check::check(&parsed.module);
    if !checked.diagnostics.is_empty() {
        return Err(format!("check errors: {:?}", checked.diagnostics));
    }
    let program = ir::lower(&checked);
    let world = &checked.world;
    let mut facts = CheckedProgram {
        functions: BTreeMap::new(),
    };
    for (name, f) in &world.functions {
        let mut actions = vec![];
        if let Some(body) = program.functions.get(name) {
            for instruction in body.blocks.iter().flat_map(|b| &b.instructions) {
                let key = (instruction.span.start, instruction.span.end);
                if world.overflow_ops.contains(&key) {
                    actions.push(Action::Perform(Effect {
                        label: None,
                        name: "Fail".into(),
                        args: vec![Type::named("ArithOverflow")],
                    }));
                }
                if world.partial_ops.contains(&key) {
                    actions.push(Action::Perform(Effect {
                        label: None,
                        name: "Fail".into(),
                        args: vec![Type::named("RuntimeError")],
                    }));
                }
                match &instruction.op {
                    ir::Op::Call {
                        callee, capability, ..
                    } => {
                        let vars = world.calls.get(&key).cloned().unwrap_or_default();
                        if world.functions.contains_key(callee) {
                            actions.push(Action::Call {
                                callee: callee.clone(),
                                substitutions: vars,
                            });
                        } else if let Some((effect, operation)) = callee.split_once("::") {
                            if let Some(declaration) = world.effects.get(effect) {
                                if let Some(op) =
                                    declaration.ops.iter().find(|f| f.name == operation)
                                {
                                    if !op.generics.is_empty()
                                        || op.body.is_some()
                                        || op.effects.tail.is_some()
                                        || !op.effects.instances.is_empty()
                                    {
                                        actions.push(Action::Opaque(
                                            "operation-local generics/outgoing effects".into(),
                                        ));
                                    } else {
                                        actions.push(Action::Perform(Effect {
                                            label: capability.clone(),
                                            name: effect.into(),
                                            args: declaration
                                                .generics
                                                .iter()
                                                .map(|g| {
                                                    vars.get(&g.name)
                                                        .cloned()
                                                        .unwrap_or(Type::Infer)
                                                })
                                                .collect(),
                                        }));
                                    }
                                } else {
                                    actions
                                        .push(Action::Opaque("unresolved effect operation".into()));
                                }
                            } else {
                                actions.push(Action::Opaque("unresolved qualified callee".into()));
                            }
                        } else {
                            actions.push(Action::Opaque("unresolved direct callee".into()));
                        }
                    }
                    ir::Op::Read {
                        root,
                        path,
                        pass,
                        projections,
                    } => {
                        let moves_noncopy = *pass == Pass::Sink
                            && world.mode(&instruction.ty, &body.bounds) != ast::Mode::Copy;
                        if projections.is_empty() {
                            actions.push(Action::Access {
                                root: root.clone(),
                                path: path.clone(),
                                pass: *pass,
                                moves_noncopy,
                            });
                        } else {
                            for projection in projections {
                                let mut selected = path.clone();
                                selected.extend(projection.clone());
                                actions.push(Action::Access {
                                    root: root.clone(),
                                    path: selected,
                                    pass: *pass,
                                    moves_noncopy,
                                });
                            }
                        }
                    }
                    ir::Op::Write { place, .. } => {
                        let mut pieces = place.split('.');
                        let root = pieces.next().unwrap_or("");
                        if root.is_empty() {
                            actions.push(Action::Opaque("unresolved assignment place".into()));
                        } else {
                            actions.push(Action::Access {
                                root: root.into(),
                                path: pieces.map(str::to_owned).collect(),
                                pass: Pass::Inout,
                                moves_noncopy: false,
                            });
                        }
                    }
                    ir::Op::Try(value) => match body.values.get(value) {
                        Some(Type::Named(name, arguments))
                            if name == "Result" && arguments.len() == 2 =>
                        {
                            actions.push(Action::Perform(Effect {
                                label: None,
                                name: "Fail".into(),
                                args: vec![arguments[1].clone()],
                            }));
                        }
                        _ => actions.push(Action::Opaque(
                            "unresolved Result error type for '?'".into(),
                        )),
                    },
                    ir::Op::Indirect { .. } => actions.push(Action::Opaque("indirect call".into())),
                    ir::Op::Closure { .. } => {
                        actions.push(Action::Opaque("closure capture/effect scope".into()))
                    }
                    ir::Op::Handle { .. } => {
                        actions.push(Action::Opaque("handler capability scope".into()))
                    }
                    _ => (),
                }
            }
        } else if f.body.is_some() {
            actions.push(Action::Opaque("checked body missing from IR".into()));
        }
        let parameters = f
            .params
            .iter()
            .map(|p| ParameterFacts {
                parameter: p.clone(),
                struct_fields: world
                    .adts
                    .get(p.ty.name())
                    .filter(|a| !a.is_enum)
                    .map(|a| {
                        a.variants
                            .iter()
                            .flat_map(|v| &v.fields)
                            .map(|f| f.name.clone())
                            .collect()
                    })
                    .unwrap_or_default(),
            })
            .collect();
        facts.functions.insert(
            name.clone(),
            FunctionFacts {
                declaration: f.clone(),
                actions,
                parameters,
                source_owned: !world.elaboration.generated.contains_key(name),
            },
        );
    }
    Ok(SourceFacts {
        checked,
        facts,
        tokens,
    })
}
fn row_tokens<'a>(f: &ast::Function, tokens: &'a [Token]) -> Result<Vec<&'a Token>, String> {
    let end = f.body.as_ref().map_or(f.span.end, |body| body.span.start);
    let header: Vec<_> = tokens
        .iter()
        .filter(|t| {
            t.span.start >= f.span.start && t.span.end <= end && !trivia(t.kind) && t.kind != K::Eof
        })
        .collect();
    let mut depth = 0usize;
    let mut position = None;
    for (i, t) in header.iter().enumerate() {
        if t.kind == K::Eff && depth == 0 {
            position = Some(i);
        }
        match t.kind {
            K::LParen | K::LBracket | K::LBrace => depth += 1,
            K::RParen | K::RBracket | K::RBrace => depth = depth.saturating_sub(1),
            _ => (),
        }
    }
    let position = position.ok_or("explicit source effect row unavailable")?;
    if header
        .get(position + 1)
        .is_none_or(|t| t.kind != K::LBracket)
    {
        return Err("source effect row delimiter unavailable".into());
    }
    let mut out = vec![];
    depth = 1;
    for token in header.into_iter().skip(position + 2) {
        match token.kind {
            K::LBracket => depth += 1,
            K::RBracket => {
                depth -= 1;
                if depth == 0 {
                    return Ok(out);
                }
            }
            _ => (),
        }
        out.push(token);
    }
    Err("unterminated source effect row".into())
}
struct EffectRow<'a> {
    entries: Vec<Vec<&'a Token>>,
    commas: Vec<&'a Token>,
    spans: BTreeMap<usize, ast::Span>,
}
// Map checked effects to source tokens first. Edit normalization only runs after
// policy and selected-range filtering; mapping never builds unselected edits.
fn effect_row<'a>(function: &ast::Function, tokens: &'a [Token]) -> Result<EffectRow<'a>, String> {
    if function.effects.tail.is_some() {
        return Err("open source effect row".into());
    }
    let row = row_tokens(function, tokens)?;
    let mut entries: Vec<Vec<&Token>> = vec![vec![]];
    let mut commas = vec![];
    let mut depth = 0usize;
    for token in row {
        match token.kind {
            K::LBracket | K::LParen => depth += 1,
            K::RBracket | K::RParen => depth = depth.saturating_sub(1),
            K::Comma if depth == 0 => {
                commas.push(token);
                entries.push(vec![]);
                continue;
            }
            _ => (),
        }
        entries.last_mut().expect("entry exists").push(token);
    }
    if entries.last().is_some_and(Vec::is_empty) {
        entries.pop();
    }
    if entries.len() != function.effects.instances.len() || entries.iter().any(Vec::is_empty) {
        return Err("effect row/source mapping differs from checked declaration".into());
    }
    let spans = entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            (
                index,
                entry[0].span.start..entry.last().expect("nonempty").span.end,
            )
        })
        .collect();
    Ok(EffectRow {
        entries,
        commas,
        spans,
    })
}
fn edits_for(
    source: &str,
    function: &ast::Function,
    tokens: &[Token],
    unused: &BTreeSet<usize>,
) -> Result<(Vec<SourceEdit>, BTreeMap<usize, ast::Span>), String> {
    let EffectRow {
        entries,
        commas,
        spans,
    } = effect_row(function, tokens)?;
    let mut edits = vec![];
    let mut remove = |token: &Token| {
        edits.push(SourceEdit {
            span: token.span.clone(),
            expected: source[token.span.clone()].into(),
            replacement: String::new(),
        });
    };
    for (index, entry) in entries.iter().enumerate() {
        if unused.contains(&index) {
            for token in entry {
                remove(token);
            }
        }
    }
    for (index, comma) in commas.iter().enumerate() {
        if unused.contains(&index) || !(index + 1..entries.len()).any(|i| !unused.contains(&i)) {
            remove(comma);
        }
    }
    Ok((edits, spans))
}
/// Builds a fully checked candidate but never performs filesystem writes.
pub fn prepare_source(source: &str, fix: bool, semver_safe: bool) -> Result<PreparedLint, String> {
    prepare_source_with_policy(
        source,
        fix,
        semver_safe,
        &Policy::default(),
        ConfigProvenance::Default,
    )
}
/// Apply a validated policy before selecting edits. The candidate is fully
/// rechecked by the same unchanged compiler and interface gates as the default.
pub fn prepare_source_with_policy(
    source: &str,
    fix: bool,
    semver_safe: bool,
    policy: &Policy,
    provenance: ConfigProvenance,
) -> Result<PreparedLint, String> {
    prepare(source, fix, semver_safe, policy, provenance, None)
}
/// Prepare a range-scoped quick fix from fresh compiler/lint evidence. Client
/// diagnostics are never accepted. A caret selects a containing finding with an
/// exclusive end; a nonempty range selects intersecting finding spans. Findings
/// elsewhere remain reported but cannot authorize edits. Explicit fix-all uses
/// `prepare_source_with_policy` instead. All candidate checks and SemVer gates
/// still inspect the complete document.
pub fn prepare_source_selected_with_policy(
    source: &str,
    selected: ast::Span,
    semver_safe: bool,
    policy: &Policy,
    provenance: ConfigProvenance,
) -> Result<PreparedLint, String> {
    if source.get(selected.clone()).is_none() {
        return Err("invalid selected source range".into());
    }
    // The new editor entry point has its own explicit checked-snapshot profile.
    // Legacy CLI prepare APIs retain the existing independent lint profile.
    super::frontend::check_source(source, super::frontend::AnalysisLimits::default())
        .map_err(|e| e.to_string())?;
    prepare(
        source,
        true,
        semver_safe,
        policy,
        provenance,
        Some(&selected),
    )
}
fn intersects(selected: &ast::Span, finding: &ast::Span) -> bool {
    if selected.is_empty() {
        (finding.start <= selected.start && selected.start < finding.end)
            || (finding.is_empty() && finding.start == selected.start)
    } else {
        selected.start < finding.end && finding.start < selected.end
    }
}
fn prepare(
    source: &str,
    fix: bool,
    semver_safe: bool,
    policy: &Policy,
    provenance: ConfigProvenance,
    selected: Option<&ast::Span>,
) -> Result<PreparedLint, String> {
    if semver_safe && !fix {
        return Err("--semver-safe requires --fix".into());
    }
    let before = checked_source(source)?;
    let mut analysis = vesper_lint::analyze(&before.facts, vesper_lint::Limits::default())
        .map_err(|e| e.to_string())?;
    let suppressed = apply_policy(&mut analysis, policy);
    let mut groups: BTreeMap<String, BTreeSet<usize>> = BTreeMap::new();
    for finding in &analysis.findings {
        if let FindingKind::UnusedEffectCapability { effect_index, .. } = &finding.detail {
            groups
                .entry(finding.function.clone())
                .or_default()
                .insert(*effect_index);
        }
    }
    let mut edits = vec![];
    let mut fixable = BTreeSet::new();
    for name in groups.keys() {
        let f = &before.checked.world.functions[name];
        match effect_row(f, &before.tokens) {
            Ok(EffectRow { spans, .. }) => {
                let mut chosen = BTreeSet::new();
                for finding in &mut analysis.findings {
                    if finding.function == *name
                        && let FindingKind::UnusedEffectCapability { effect_index, .. } =
                            &finding.detail
                    {
                        finding.span = spans[effect_index].clone();
                        fixable.insert((name.clone(), *effect_index));
                        if selected.is_none_or(|range| intersects(range, &finding.span)) {
                            chosen.insert(*effect_index);
                        }
                    }
                }
                if !chosen.is_empty() {
                    let (mut function_edits, _) = edits_for(source, f, &before.tokens, &chosen)?;
                    edits.append(&mut function_edits);
                }
            }
            Err(reason) => analysis.skipped.push(vesper_lint::Skipped {
                function: name.clone(),
                reason,
            }),
        }
    }
    let diagnostics = analysis.findings.iter().cloned().map(|finding| {
        let can_fix=matches!(&finding.detail,FindingKind::UnusedEffectCapability{effect_index,..} if fixable.contains(&(finding.function.clone(),*effect_index)));
        let severity=policy.severity_for(&finding.detail);
        ReportedFinding{finding,fixable:can_fix,severity}
    }).collect();
    let mut report = SourceReport {
        schema_version: 1,
        status: "checked",
        diagnostics,
        skipped: analysis.skipped.clone(),
        changed: false,
        applied_edits: 0,
        semver: None,
        work: analysis.work,
        policy: PolicyReport {
            provenance,
            rules: policy.rules(),
            unavailable: policy.unavailable(),
            plugin_packs_supported: false,
            usage: policy.usage(),
        },
        outcome: policy_outcome(&analysis, &before.facts, policy, suppressed),
    };
    set_policy_status(&mut report);
    if !fix {
        return Ok(PreparedLint {
            report,
            candidate: None,
        });
    }
    if !edits.is_empty() && !analysis.skipped.is_empty() {
        return Err(format!(
            "autofix refused: conservative unsupported evidence: {:?}",
            analysis.skipped
        ));
    }
    let candidate =
        vesper_lint::apply_edits(source, &edits, MAX_BYTES).map_err(|e| e.to_string())?;
    let after = checked_source(&candidate)?;
    let mut after_report = vesper_lint::analyze(&after.facts, vesper_lint::Limits::default())
        .map_err(|e| e.to_string())?;
    let after_suppressed = apply_policy(&mut after_report, policy);
    if let Some(range) = selected {
        validate_selected_delta(&analysis, &after_report, range)?;
    } else {
        validate_lint_delta(&analysis, &after_report)?;
    }
    if semver_safe {
        let compatibility = compatible_interfaces(&before.checked.world, &after.checked.world)?;
        report.semver = Some(compatibility);
    }
    if !edits.is_empty() {
        if selected.is_none()
            && after_report
                .findings
                .iter()
                .any(|f| matches!(f.detail, FindingKind::UnusedEffectCapability { .. }))
        {
            return Err("autofix did not remove all offered unused effects".into());
        }
        report.changed = candidate != source;
        report.applied_edits = edits.len();
    }
    report.outcome = policy_outcome(&after_report, &after.facts, policy, after_suppressed);
    set_policy_status(&mut report);
    Ok(PreparedLint {
        candidate: report.changed.then_some(candidate),
        report,
    })
}
fn apply_policy(report: &mut LintReport, policy: &Policy) -> usize {
    let before = report.findings.len();
    report
        .findings
        .retain(|finding| policy.severity_for(&finding.detail) != Severity::Allow);
    before - report.findings.len()
}
fn policy_outcome(
    report: &LintReport,
    program: &CheckedProgram,
    policy: &Policy,
    suppressed: usize,
) -> PolicyOutcome {
    // Trait-implementation bodies are checked but elaborated into generated
    // functions. The unchanged analyzer excludes them from source diagnostics.
    // An explicit deny cannot claim complete coverage of those bodies.
    let excluded_generated_bodies = program
        .functions
        .values()
        .filter(|facts| !facts.source_owned && facts.declaration.body.is_some())
        .count();
    PolicyOutcome {
        remaining_denied: report
            .findings
            .iter()
            .filter(|finding| policy.severity_for(&finding.detail) == Severity::Deny)
            .count(),
        remaining_warnings: report
            .findings
            .iter()
            .filter(|finding| policy.severity_for(&finding.detail) == Severity::Warn)
            .count(),
        suppressed,
        excluded_generated_bodies,
        unavailable_denied_rules: if report.skipped.is_empty() && excluded_generated_bodies == 0 {
            vec![]
        } else {
            policy
                .rules()
                .into_iter()
                .filter(|rule| rule.severity == Severity::Deny)
                .map(|rule| rule.lint)
                .collect()
        },
    }
}
fn set_policy_status(report: &mut SourceReport) {
    report.status = if report.outcome.remaining_denied > 0
        || !report.outcome.unavailable_denied_rules.is_empty()
    {
        "denied"
    } else {
        "checked"
    };
}
fn compatible_interfaces(
    before: &check::World,
    after: &check::World,
) -> Result<vesper_semver::Report, String> {
    let baseline = interface::export(before, "@vesper/lint")?;
    let candidate = interface::export(after, "@vesper/lint")?;
    let report = vesper_semver::compare(&baseline, &candidate);
    if !report.is_compatible() {
        return Err(format!("semver-safe autofix refused: {:?}", report.issues));
    }
    Ok(report)
}

fn same_finding(left: &Finding, right: &Finding) -> bool {
    left.function == right.function
        && match (&left.detail, &right.detail) {
            (
                FindingKind::UnusedEffectCapability { effect: a, .. },
                FindingKind::UnusedEffectCapability { effect: b, .. },
            ) => a == b,
            (a, b) => a == b,
        }
}
fn validate_selected_delta(
    before: &LintReport,
    after: &LintReport,
    selected: &ast::Span,
) -> Result<(), String> {
    for finding in &before.findings {
        if !matches!(finding.detail, FindingKind::UnusedEffectCapability { .. }) {
            continue;
        }
        let expected = before
            .findings
            .iter()
            .filter(|old| same_finding(old, finding) && !intersects(selected, &old.span))
            .count();
        let actual = after
            .findings
            .iter()
            .filter(|new| same_finding(new, finding))
            .count();
        if expected != actual {
            return Err(
                "selected autofix did not remove exactly the selected unused effects".into(),
            );
        }
    }
    for finding in &after.findings {
        if !before.findings.iter().any(|old| same_finding(old, finding)) {
            return Err("selected autofix introduced a new lint diagnostic".into());
        }
    }
    for skipped in &after.skipped {
        if !before.skipped.iter().any(|old| old == skipped) {
            return Err("selected autofix introduced unsupported evidence".into());
        }
    }
    Ok(())
}

fn validate_lint_delta(before: &LintReport, after: &LintReport) -> Result<(), String> {
    for finding in &after.findings {
        if !before
            .findings
            .iter()
            .any(|old| old.function == finding.function && old.detail == finding.detail)
        {
            return Err("autofix introduced a new lint diagnostic".into());
        }
    }
    for skipped in &after.skipped {
        if !before.skipped.iter().any(|old| old == skipped) {
            return Err("autofix introduced unsupported evidence".into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inferred_growth_is_refused_without_invoking_core_checker() {
        let mut tuple = "fn main(){let x=0;".to_owned();
        for _ in 0..25 {
            tuple.push_str("let x=(x,x);");
        }
        tuple.push('}');
        let nested = format!(
            "fn dup[T:mod copy](in x:T)->(T,T){{(x,x)}} fn main(){{{}0{};}}",
            "dup(".repeat(25),
            ")".repeat(25)
        );
        let pair = "copy struct Pair[T:mod copy,U:mod copy]{a:T,b:U} fn main(){}";
        let field = "copy struct D[T:mod copy]{payload:(T,T)} fn main(){}";
        for source in [tuple.as_str(), nested.as_str(), pair, field] {
            preflight(source).unwrap();
            let parsed = parser::parse(source);
            assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
            assert!(surface_expansion(&parsed.module).is_err());
            // Deliberately do not call check::check on an expanding input.
        }
    }

    #[test]
    fn actual_checked_supported_interface_change_is_rejected() {
        let before = checked_source("fn value(x:u64)->u64{x}").unwrap();
        let after = checked_source("fn value(x:u32)->u32{x}").unwrap();
        assert!(compatible_interfaces(&before.checked.world, &after.checked.world).is_err());
        let same = compatible_interfaces(&before.checked.world, &before.checked.world).unwrap();
        assert!(same.is_compatible());
    }
    #[test]
    fn candidate_check_rejects_effect_and_linear_cleanup_regressions() {
        let before = "effect E{fn act();} fn value() eff[E]{E::act();}";
        checked_source(before).unwrap();
        let start = before.find("E]{").unwrap();
        let edited = vesper_lint::apply_edits(
            before,
            &[SourceEdit {
                span: start..start + 1,
                expected: "E".into(),
                replacement: String::new(),
            }],
            MAX_BYTES,
        )
        .unwrap();
        assert!(checked_source(&edited).is_err());
        checked_source("linear struct Ticket{x:u64} fn done(sink t:Ticket){let Ticket{x}=t;}")
            .unwrap();
        assert!(checked_source("linear struct Ticket{x:u64} fn done(sink t:Ticket){}").is_err());
    }
}

#[cfg(test)]
mod selected_tests {
    use super::*;
    const SOURCE: &str =
        "effect E{fn e();} effect F{fn f();} fn first() eff[E,F]{} fn second() eff[E,F]{}";
    #[test]
    fn selected_fix_edits_one_fresh_finding_and_preserves_other_functions() {
        let at = SOURCE.find("eff[E,F]").unwrap() + 4;
        let prepared = prepare_source_selected_with_policy(
            SOURCE,
            at..at + 1,
            true,
            &Policy::default(),
            ConfigProvenance::Default,
        )
        .unwrap();
        let candidate = prepared.candidate.unwrap();
        assert!(candidate.contains("fn first() eff[F]{}"), "{candidate}");
        assert!(candidate.ends_with("fn second() eff[E,F]{}"));
        assert!(prepared.report.semver.unwrap().is_compatible());
        assert_eq!(prepared.report.diagnostics.len(), 4);
        assert_eq!(
            prepare_source(&candidate, false, false)
                .unwrap()
                .report
                .diagnostics
                .len(),
            3
        );
    }
    #[test]
    fn caret_exclusive_end_no_match_and_invalid_utf8_range() {
        let at = SOURCE.find("eff[E,F]").unwrap() + 4;
        let fixed = prepare_source_selected_with_policy(
            SOURCE,
            at..at,
            true,
            &Policy::default(),
            ConfigProvenance::Default,
        )
        .unwrap();
        assert!(fixed.candidate.unwrap().contains("fn first() eff[F]{}"));
        let no = prepare_source_selected_with_policy(
            SOURCE,
            at + 1..at + 1,
            true,
            &Policy::default(),
            ConfigProvenance::Default,
        )
        .unwrap();
        assert!(no.candidate.is_none());
        assert!(
            prepare_source_selected_with_policy(
                SOURCE,
                ast::Span { start: 9, end: 2 },
                true,
                &Policy::default(),
                ConfigProvenance::Default
            )
            .is_err()
        );
        let utf8 = format!("// 🙂\n{SOURCE}");
        assert!(
            prepare_source_selected_with_policy(
                &utf8,
                4..5,
                true,
                &Policy::default(),
                ConfigProvenance::Default
            )
            .is_err()
        );
    }
    #[test]
    fn selected_all_is_distinct_and_allow_policy_cannot_authorize_fix() {
        let policy = Policy::parse(
            "[lints.vesper]\nunused_effect_capability = \"allow\"\n",
            PolicyLimits::default(),
        )
        .unwrap();
        let no = prepare_source_selected_with_policy(
            SOURCE,
            0..SOURCE.len(),
            true,
            &policy,
            ConfigProvenance::Default,
        )
        .unwrap();
        assert!(no.candidate.is_none());
        assert!(no.report.diagnostics.is_empty());
        let all = prepare_source(SOURCE, true, true)
            .unwrap()
            .candidate
            .unwrap();
        assert_eq!(
            prepare_source(&all, false, false)
                .unwrap()
                .report
                .diagnostics
                .len(),
            0
        );
        // Fresh invalid source is refused even when the selected span looks valid.
        assert!(
            prepare_source_selected_with_policy(
                "fn broken(){unknown();}",
                0..2,
                true,
                &Policy::default(),
                ConfigProvenance::Default
            )
            .is_err()
        );
    }
}
