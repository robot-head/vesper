//! Checked-source adapter for the root-free typed lint engine.
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};
use vesper::{
    ast::{self, Effect, Pass, Type},
    check, interface, ir,
    lexer::{self, Token, TokenKind as K},
    parser,
};
use vesper_lint::{
    Action, CheckedProgram, Finding, FindingKind, FunctionFacts, LintReport, ParameterFacts,
    SourceEdit,
};

pub const HELP: &str = "Usage: vesper lint FILE [--json] [--fix] [--semver-safe]\n\nCheck actual resolved effects and supported inout field accesses.\nUnused effects (L0102) may be removed only after complete candidate reparse/recheck.\nProjection narrowing (L0101) is diagnostic-only: caller/signature changes need review.\n--semver-safe requires --fix and validates exported supported interface compatibility.\nHandlers, closures, indirect/open-row/recursive calls remain conservative.\nInputs: regular UTF-8 files <=256 KiB; token/nesting/work budgets also apply.\nNo arena escape, mode-relaxation, FIP, secret/Cancel or plugin-pack analysis.\n";
const MAX_BYTES: usize = 262144;
#[derive(Debug, Serialize)]
pub struct ReportedFinding {
    #[serde(flatten)]
    pub finding: Finding,
    pub fixable: bool,
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
fn edits_for(
    source: &str,
    function: &ast::Function,
    tokens: &[Token],
    unused: &BTreeSet<usize>,
) -> Result<(Vec<SourceEdit>, BTreeMap<usize, ast::Span>), String> {
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
    let mut edits = vec![];
    let mut spans = BTreeMap::new();
    let mut remove = |token: &Token| {
        edits.push(SourceEdit {
            span: token.span.clone(),
            expected: source[token.span.clone()].into(),
            replacement: String::new(),
        });
    };
    for (index, entry) in entries.iter().enumerate() {
        spans.insert(
            index,
            entry[0].span.start..entry.last().expect("nonempty").span.end,
        );
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
    if semver_safe && !fix {
        return Err("--semver-safe requires --fix".into());
    }
    let before = checked_source(source)?;
    let mut analysis = vesper_lint::analyze(&before.facts, vesper_lint::Limits::default())
        .map_err(|e| e.to_string())?;
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
    for (name, indices) in &groups {
        let f = &before.checked.world.functions[name];
        match edits_for(source, f, &before.tokens, indices) {
            Ok((mut function_edits, spans)) => {
                for finding in &mut analysis.findings {
                    if finding.function == *name
                        && let FindingKind::UnusedEffectCapability { effect_index, .. } =
                            &finding.detail
                    {
                        finding.span = spans[effect_index].clone();
                        fixable.insert((name.clone(), *effect_index));
                    }
                }
                edits.append(&mut function_edits);
            }
            Err(reason) => analysis.skipped.push(vesper_lint::Skipped {
                function: name.clone(),
                reason,
            }),
        }
    }
    let diagnostics=analysis.findings.iter().cloned().map(|finding|{let can_fix=matches!(&finding.detail,FindingKind::UnusedEffectCapability{effect_index,..} if fixable.contains(&(finding.function.clone(),*effect_index)));ReportedFinding{finding,fixable:can_fix}}).collect();
    let mut report = SourceReport {
        schema_version: 1,
        status: "checked",
        diagnostics,
        skipped: analysis.skipped.clone(),
        changed: false,
        applied_edits: 0,
        semver: None,
        work: analysis.work,
    };
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
    let after_report = vesper_lint::analyze(&after.facts, vesper_lint::Limits::default())
        .map_err(|e| e.to_string())?;
    validate_lint_delta(&analysis, &after_report)?;
    if semver_safe {
        let compatibility = compatible_interfaces(&before.checked.world, &after.checked.world)?;
        report.semver = Some(compatibility);
    }
    if !edits.is_empty() {
        if after_report
            .findings
            .iter()
            .any(|f| matches!(f.detail, FindingKind::UnusedEffectCapability { .. }))
        {
            return Err("autofix did not remove all offered unused effects".into());
        }
        report.changed = candidate != source;
        report.applied_edits = edits.len();
    }
    Ok(PreparedLint {
        candidate: report.changed.then_some(candidate),
        report,
    })
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
fn read_source(path: &Path) -> Result<(String, fs::Permissions), String> {
    let metadata = fs::symlink_metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if !metadata.is_file() {
        return Err("lint input must be a regular file without final symlinks".into());
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x0020_0000);
    }
    let file = options.open(path).map_err(|e| e.to_string())?;
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.len() > MAX_BYTES as u64 {
        return Err("lint input is not a bounded regular file (256 KiB)".into());
    }
    let mut bytes = vec![];
    file.take(MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > MAX_BYTES {
        return Err("lint source exceeds 256 KiB".into());
    }
    Ok((
        String::from_utf8(bytes).map_err(|_| "lint input must be UTF-8")?,
        metadata.permissions(),
    ))
}
struct Staging(PathBuf);
impl Drop for Staging {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
fn replace_source(
    path: &Path,
    original: &str,
    candidate: &str,
    permissions: fs::Permissions,
) -> Result<(), String> {
    if permissions.readonly() {
        return Err("lint refuses to replace a readonly source".into());
    }
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let temporary_path = parent.join(format!(
        ".vesper-lint-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    replace_at(path, original, candidate, permissions, temporary_path)
}
fn replace_at(
    path: &Path,
    original: &str,
    candidate: &str,
    permissions: fs::Permissions,
    temporary_path: PathBuf,
) -> Result<(), String> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary_path).map_err(|e| e.to_string())?;
    let temporary = Staging(temporary_path);
    file.write_all(candidate.as_bytes())
        .map_err(|e| e.to_string())?;
    file.set_permissions(permissions)
        .map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    drop(file);
    let (current, _) = read_source(path)?;
    if current != original {
        return Err(
            "source changed while lint was validating; original path left untouched".into(),
        );
    }
    fs::rename(&temporary.0, path).map_err(|e| e.to_string())
}
pub fn run(args: Vec<String>) -> Result<(), String> {
    let json = args.iter().any(|a| a == "--json");
    let result = (|| {
        let mut path = None;
        let mut fix = false;
        let mut semver_safe = false;
        for arg in args {
            match arg.as_str() {
                "--help" | "-h" => {
                    print!("{HELP}");
                    return Ok(None);
                }
                "--json" => (),
                "--fix" => fix = true,
                "--semver-safe" => semver_safe = true,
                _ if arg.starts_with('-') => return Err(format!("unknown lint option '{arg}'")),
                _ => {
                    if path.replace(PathBuf::from(arg)).is_some() {
                        return Err("lint accepts one source file".into());
                    }
                }
            }
        }
        let path = path.ok_or("lint requires a source file")?;
        let (source, permissions) = read_source(&path)?;
        let prepared = prepare_source(&source, fix, semver_safe)?;
        let output = serde_json::to_string_pretty(&prepared.report).map_err(|e| e.to_string())?;
        if let Some(candidate) = prepared.candidate {
            replace_source(&path, &source, &candidate, permissions)?;
        }
        Ok(Some((prepared.report, output)))
    })();
    match result {
        Ok(None) => Ok(()),
        Ok(Some((report, output))) => {
            if json {
                println!("{output}");
            } else {
                for finding in &report.diagnostics {
                    println!(
                        "{} {}:{}..{}: {}",
                        finding.finding.code,
                        finding.finding.function,
                        finding.finding.span.start,
                        finding.finding.span.end,
                        finding.finding.message
                    );
                }
                println!(
                    "lint: {} diagnostic(s), {} conservative skip(s), changed={}",
                    report.diagnostics.len(),
                    report.skipped.len(),
                    report.changed
                );
            }
            Ok(())
        }
        Err(error) => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({"schema_version":1,"status":"invalid","error":error})
                );
            }
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (PathBuf, PathBuf) {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "vesper-lint-transaction-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&dir).unwrap();
        let file = dir.join("source.ves");
        fs::write(&file, "current").unwrap();
        (dir, file)
    }
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
    fn stale_contents_and_private_staging_collisions_preserve_files() {
        let (dir, path) = fixture();
        let permissions = fs::metadata(&path).unwrap().permissions();
        let temporary = dir.join("stage");
        assert!(
            replace_at(
                &path,
                "outdated",
                "candidate",
                permissions.clone(),
                temporary.clone()
            )
            .is_err()
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "current");
        assert!(!temporary.exists());
        fs::write(&temporary, "preexisting staging owner").unwrap();
        assert!(
            replace_at(
                &path,
                "current",
                "candidate",
                permissions,
                temporary.clone()
            )
            .is_err()
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "current");
        assert_eq!(
            fs::read_to_string(&temporary).unwrap(),
            "preexisting staging owner"
        );
        fs::remove_dir_all(dir).unwrap();
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
