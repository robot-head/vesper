//! Root-free documentation metadata. The facade supplies actual checked source;
//! this crate validates bounded DTOs, searches complete structural contracts and
//! renders deterministic escaped local documents. It never authorizes a proof
//! badge, compiler correctness claim or all-path ownership conclusion.
#![forbid(unsafe_code)]
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    io::Write,
};
pub use vesper_core::ast::{
    Adt, ConformanceInfo, Effect, Effects, Generic, Mode, Pass, Span, TraitDecl, Type,
};
use vesper_syntax::lexer::{self, TokenKind as K};

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_source_bytes: usize,
    pub max_items: usize,
    pub max_type_nodes: usize,
    pub max_output_bytes: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_source_bytes: 131_072,
            max_items: 256,
            max_type_nodes: 8192,
            max_output_bytes: 2_097_152,
        }
    }
}
impl Limits {
    pub fn validate(self) -> Result<(), String> {
        if self.max_source_bytes == 0
            || self.max_source_bytes > 262_144
            || self.max_items == 0
            || self.max_items > 1024
            || self.max_type_nodes == 0
            || self.max_type_nodes > 65_536
            || self.max_output_bytes == 0
            || self.max_output_bytes > 8_388_608
        {
            return Err("documentation limit configuration unsupported".into());
        }
        Ok(())
    }
}
/// Bounded lexical preflight before the facade invokes the existing parser.
/// Conservative expression runs are limited as well as delimiter nesting.
pub fn source_preflight(source: &str, limits: Limits) -> Result<(), String> {
    limits.validate()?;
    if source.len() > limits.max_source_bytes {
        return Err("source byte limit exceeded".into());
    }
    let (tokens, errors) = lexer::lex(source);
    if !errors.is_empty() {
        return Err(format!("source lexing failed: {}", errors[0].message));
    }
    let mut depth = 0usize;
    // Each frame tracks unfinished cost and the peak of completed statements.
    // The peak propagates too: a semicolon cannot hide a deep child AST.
    let mut segments = vec![(0usize, 0usize)];
    let mut functions = 0usize;
    let mut conditionals = 0usize;
    if tokens.len() > 16_384 {
        return Err("source token limit exceeded".into());
    }
    for token in tokens {
        if token.kind.trivia() {
            continue;
        }
        match token.kind {
            K::LBrace | K::LParen | K::LBracket => {
                depth += 1;
                if depth > 48 {
                    return Err("source nesting limit exceeded".into());
                }
            }
            K::RBrace | K::RParen | K::RBracket => depth = depth.saturating_sub(1),
            _ => (),
        }
        if token.kind == K::If {
            conditionals += 1;
            if conditionals > 64 {
                return Err("source conditional-chain limit exceeded".into());
            }
        }
        if token.kind == K::Fn {
            functions += 1;
            if functions > 128 {
                return Err("source function-token limit exceeded".into());
            }
        }
        match token.kind {
            K::LBrace | K::LParen | K::LBracket => segments.push((0, 0)),
            K::RBrace | K::RParen | K::RBracket => {
                if segments.len() > 1 {
                    let child = segments.pop().ok_or("invalid segment stack")?;
                    let parent = segments.last_mut().ok_or("invalid segment stack")?;
                    parent.0 = parent
                        .0
                        .checked_add(child.0.max(child.1) + 1)
                        .ok_or("source segment overflow")?;
                    parent.1 = parent.1.max(parent.0);
                }
                if token.kind == K::RBrace && segments.len() == 1 {
                    segments[0] = (0, 0);
                }
            }
            K::Semi => segments.last_mut().ok_or("invalid segment stack")?.0 = 0,
            _ => {
                let current = segments.last_mut().ok_or("invalid segment stack")?;
                current.0 += 1;
                current.1 = current.1.max(current.0);
            }
        }
        if segments.last().copied().unwrap_or((0, 0)).1 > 256 {
            return Err("source weighted expression/signature limit exceeded".into());
        }
    }
    Ok(())
}
/// Attached `///` comments are found using actual lexer trivia tokens. A code
/// token, ordinary comment, or blank line breaks attachment. No text in string
/// literals can masquerade as a doc comment. Module docs retain source order.
pub fn comments(source: &str, item_start: usize) -> (String, String) {
    let (tokens, _) = lexer::lex(source);
    let mut module = Vec::new();
    let mut pending = Vec::new();
    let mut leading = true;
    for token in tokens {
        if token.span.start >= item_start {
            break;
        }
        let text = &source[token.span.clone()];
        match token.kind {
            K::Comment if text.starts_with("//!") => {
                if leading {
                    module.push(text[3..].trim_start().to_owned());
                }
                pending.clear();
            }
            K::Comment if text.starts_with("///") && !text.starts_with("////") => {
                pending.push(text[3..].strip_prefix(' ').unwrap_or(&text[3..]).to_owned())
            }
            K::Whitespace => {
                if text.bytes().filter(|b| *b == b'\n').count() > 1 {
                    pending.clear();
                }
            }
            _ => {
                pending.clear();
                if !token.kind.trivia() {
                    leading = false;
                }
            }
        }
    }
    (module.join("\n"), pending.join("\n"))
}

/// Every actual /// or //! token, including comments on fields/variants whose
/// current core AST has no dedicated field span. Attachment is not guessed.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DocComment {
    pub kind: String,
    pub span: Span,
    pub text: String,
}
pub fn doc_comments(source: &str) -> Vec<DocComment> {
    let (tokens, _) = lexer::lex(source);
    tokens
        .into_iter()
        .filter_map(|token| {
            if token.kind != K::Comment {
                return None;
            }
            let text = &source[token.span.clone()];
            let kind = if text.starts_with("//!") {
                "module"
            } else if text.starts_with("///") && !text.starts_with("////") {
                "item"
            } else {
                return None;
            };
            Some(DocComment {
                kind: kind.into(),
                span: token.span,
                text: text[3..].strip_prefix(' ').unwrap_or(&text[3..]).to_owned(),
            })
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "paths", deny_unknown_fields)]
pub enum Selector {
    All,
    Paths(Vec<Vec<String>>),
}
impl Selector {
    pub fn from_paths(paths: &[Vec<String>]) -> Self {
        if paths.is_empty() {
            Self::All
        } else {
            Self::Paths(paths.to_vec())
        }
    }
    fn valid(&self) -> bool {
        match self {
            Self::All => true,
            Self::Paths(p) => {
                !p.is_empty()
                    && p.len() <= 64
                    && p.iter().all(|x| {
                        !x.is_empty()
                            && x.len() <= 32
                            && x.iter().all(|s| !s.is_empty() && s.len() <= 128)
                    })
            }
        }
    }
    fn within(&self, allowed: &Self) -> bool {
        match (self, allowed) {
            (_, Self::All) => true,
            (Self::All, _) => false,
            (Self::Paths(a), Self::Paths(b)) => {
                a.iter().all(|p| b.iter().any(|q| p.starts_with(q)))
            }
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Parameter {
    pub name: String,
    pub pass: Pass,
    pub mode: Mode,
    pub ty: Type,
    pub selector: Selector,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FunctionDoc {
    pub name: String,
    pub docs: String,
    pub span: Span,
    pub generics: Vec<Generic>,
    pub parameters: Vec<Parameter>,
    pub result: Type,
    pub declared_result: Type,
    pub result_mode: Mode,
    pub effects: Effects,
    pub effect_resumptions: Vec<Option<Mode>>,
    pub unsupported: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NativeField {
    pub name: String,
    pub ty: Type,
    pub offset: i32,
    pub bytes: u32,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NativeVariant {
    pub name: String,
    pub fields: Vec<NativeField>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "status")]
pub enum NativeLayout {
    Available {
        alignment: u32,
        bytes: u32,
        kind: String,
        variants: Vec<NativeVariant>,
    },
    Unsupported {
        reason: String,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TypeDoc {
    pub declaration: Adt,
    /// Actual checked kind for a nongeneric type; generic instances need arguments.
    pub concrete_mode: Option<Mode>,
    pub docs: String,
    pub layout: NativeLayout,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EffectDoc {
    pub name: String,
    pub docs: String,
    pub generics: Vec<Generic>,
    pub linear: bool,
    pub operations: Vec<FunctionDoc>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TraitDoc {
    pub declaration: TraitDecl,
    pub docs: String,
    pub limitation: String,
}
/// Full typed source conformance head, without private implementation bodies.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConformanceDeclaration {
    pub kind: vesper_core::ast::ConformanceKind,
    pub generics: Vec<Generic>,
    pub self_type: Type,
    pub trait_reference: vesper_core::ast::TraitRef,
    pub associated: Vec<vesper_core::ast::AssocTypeBinding>,
    pub where_clauses: Vec<vesper_core::ast::WhereClause>,
    pub docs: String,
    pub span: Span,
    pub unsupported: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct State {
    pub identity: String,
    pub ty: Type,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Transition {
    pub from: String,
    pub function: String,
    pub parameter: usize,
    pub pass: Pass,
    pub to: Option<String>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TypestateGraph {
    pub states: Vec<State>,
    pub edges: Vec<Transition>,
    pub omitted: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StateReachability {
    pub identity: String,
    pub terminal_path_exists: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Document {
    pub schema_version: u32,
    pub source_bytes: usize,
    pub module_docs: String,
    pub doc_comments: Vec<DocComment>,
    pub functions: Vec<FunctionDoc>,
    pub types: Vec<TypeDoc>,
    pub effects: Vec<EffectDoc>,
    pub traits: Vec<TraitDoc>,
    pub conformances: Vec<ConformanceInfo>,
    pub source_conformances: Vec<ConformanceDeclaration>,
    pub graph: TypestateGraph,
    pub reachability: Vec<StateReachability>,
    pub limitations: Vec<String>,
}
pub fn type_identity(ty: &Type) -> Result<String, String> {
    let mut unsupported = vec![];
    type_walk(ty, 0, &mut 0, 65_536, &mut unsupported)?;
    if !unsupported.is_empty() {
        return Err("unresolved typestate identity".into());
    }
    let mut output = Bounded {
        bytes: vec![],
        limit: 8_388_608,
    };
    serde_json::to_writer(&mut output, ty).map_err(|e| e.to_string())?;
    String::from_utf8(output.bytes).map_err(|e| e.to_string())
}
fn effect_walk(
    effects: &Effects,
    depth: usize,
    nodes: &mut usize,
    limit: usize,
    unsupported: &mut Vec<String>,
) -> Result<(), String> {
    if effects.instances.len() > 64
        || effects
            .tail
            .as_ref()
            .is_some_and(|s| s.is_empty() || s.len() > 128)
    {
        return Err("effect instance/tail budget exceeded".into());
    }
    *nodes = nodes
        .checked_add(effects.instances.len())
        .ok_or("effect contract node overflow")?;
    if *nodes > limit {
        return Err("type/effect node budget exceeded".into());
    }
    for effect in &effects.instances {
        if effect.name.is_empty()
            || effect.name.len() > 256
            || effect
                .label
                .as_ref()
                .is_some_and(|s| s.is_empty() || s.len() > 128)
            || effect.args.len() > 64
        {
            return Err("effect name/label/argument budget exceeded".into());
        }
        for arg in &effect.args {
            type_walk(arg, depth + 1, nodes, limit, unsupported)?;
        }
    }
    Ok(())
}
fn type_walk(
    ty: &Type,
    depth: usize,
    nodes: &mut usize,
    limit: usize,
    unsupported: &mut Vec<String>,
) -> Result<(), String> {
    *nodes = nodes.checked_add(1).ok_or("type budget overflow")?;
    if *nodes > limit || depth > 48 {
        return Err("type node/depth budget exceeded".into());
    }
    match ty {
        Type::Infer | Type::IntLiteral(_) => unsupported.push("unresolved type".into()),
        Type::Row(effects) => {
            unsupported.push("row-valued type requires a richer scoped contract".into());
            effect_walk(effects, depth, nodes, limit, unsupported)?;
        }
        Type::Named(n, args) => {
            if n.len() > 256 {
                return Err("type name limit exceeded".into());
            }
            for a in args {
                type_walk(a, depth + 1, nodes, limit, unsupported)?;
            }
        }
        Type::Tuple(args) => {
            for a in args {
                type_walk(a, depth + 1, nodes, limit, unsupported)?;
            }
        }
        Type::Function {
            params,
            result,
            effects,
            ..
        } => {
            if !effects.instances.is_empty() || effects.tail.is_some() {
                unsupported.push("effectful arrow binder matching unsupported".into());
            }
            if params.len() > 64 {
                return Err("arrow parameter budget exceeded".into());
            }
            for (_, t) in params {
                type_walk(t, depth + 1, nodes, limit, unsupported)?;
            }
            type_walk(result, depth + 1, nodes, limit, unsupported)?;
            effect_walk(effects, depth, nodes, limit, unsupported)?;
        }
    }
    Ok(())
}
fn check_function(
    f: &FunctionDoc,
    nodes: &mut usize,
    limits: Limits,
) -> Result<Vec<String>, String> {
    if f.unsupported.len() > 64
        || f.unsupported.iter().any(|s| s.len() > 512)
        || f.docs.len() > limits.max_source_bytes
        || f.generics
            .iter()
            .any(|g| g.name.is_empty() || g.name.len() > 128)
    {
        return Err("function metadata text budget exceeded".into());
    }
    let mut unsupported = f.unsupported.clone();
    if !f.generics.is_empty() {
        unsupported.push("generic/row binder instantiation is unsupported by search".into());
    }
    if f.effects.tail.is_some() {
        unsupported.push("open effect ceiling is unsupported by search".into());
    }
    if f.effect_resumptions.iter().any(Option::is_none) {
        unsupported.push("unknown effect resumption contract".into());
    }
    if f.name.is_empty()
        || f.name.len() > 256
        || f.parameters.len() > 64
        || f.generics.len() > 32
        || f.effects.instances.len() > 64
        || f.effect_resumptions.len() != f.effects.instances.len()
    {
        return Err("malformed function documentation contract".into());
    }
    for p in &f.parameters {
        if p.name.is_empty() || p.name.len() > 128 || !p.selector.valid() {
            return Err("invalid parameter name/field selector".into());
        }
        type_walk(&p.ty, 0, nodes, limits.max_type_nodes, &mut unsupported)?;
    }
    type_walk(&f.result, 0, nodes, limits.max_type_nodes, &mut unsupported)?;
    type_walk(
        &f.declared_result,
        0,
        nodes,
        limits.max_type_nodes,
        &mut vec![],
    )?;
    effect_walk(
        &f.effects,
        0,
        nodes,
        limits.max_type_nodes,
        &mut unsupported,
    )?;
    unsupported.sort();
    unsupported.dedup();
    Ok(unsupported)
}
impl Document {
    pub fn validate(&mut self, limits: Limits) -> Result<(), String> {
        limits.validate()?;
        if self.schema_version != 1 || self.source_bytes > limits.max_source_bytes {
            return Err("invalid documentation schema/source budget".into());
        }
        let mut count = 0usize;
        let mut charge = |n: usize| -> Result<(), String> {
            count = count
                .checked_add(n)
                .ok_or("documentation item count overflow")?;
            if count > limits.max_items {
                return Err("documentation item budget exceeded".into());
            }
            Ok(())
        };
        for n in [
            self.functions.len(),
            self.types.len(),
            self.effects.len(),
            self.traits.len(),
            self.conformances.len(),
            self.source_conformances.len(),
        ] {
            charge(n)?;
        }
        for ty in &self.types {
            charge(ty.declaration.variants.len())?;
            for variant in &ty.declaration.variants {
                charge(variant.fields.len())?;
            }
        }
        for effect in &self.effects {
            charge(effect.operations.len())?;
        }
        for tr in &self.traits {
            charge(tr.declaration.methods.len())?;
            charge(tr.declaration.assoc.len())?;
        }
        if self.doc_comments.len() > 4096 {
            return Err("documentation comment count exceeded".into());
        }
        let mut comment_bytes = 0usize;
        for c in &self.doc_comments {
            comment_bytes = comment_bytes
                .checked_add(c.text.len())
                .ok_or("comment byte overflow")?;
            if c.span.start > c.span.end
                || c.span.end > self.source_bytes
                || comment_bytes > limits.max_source_bytes
            {
                return Err("invalid documentation comment span/budget".into());
            }
        }
        let mut names = BTreeSet::new();
        let mut nodes = 0usize;
        self.functions.sort_by(|a, b| a.name.cmp(&b.name));
        self.types
            .sort_by(|a, b| a.declaration.name.cmp(&b.declaration.name));
        self.effects.sort_by(|a, b| a.name.cmp(&b.name));
        self.traits
            .sort_by(|a, b| a.declaration.name.cmp(&b.declaration.name));
        self.conformances.sort_by(|a, b| a.path.cmp(&b.path));
        for f in &mut self.functions {
            if !names.insert(f.name.clone()) {
                return Err("duplicate function identity".into());
            }
            f.unsupported = check_function(f, &mut nodes, limits)?;
        }
        for ty in &self.types {
            for v in &ty.declaration.variants {
                for f in &v.fields {
                    type_walk(&f.ty, 0, &mut nodes, limits.max_type_nodes, &mut vec![])?;
                }
            }
        }
        for e in &mut self.effects {
            e.operations.sort_by(|a, b| a.name.cmp(&b.name));
            for f in &mut e.operations {
                f.unsupported = check_function(f, &mut nodes, limits)?;
            }
        }
        for tr in &self.traits {
            for method in &tr.declaration.methods {
                type_walk(
                    &method.result,
                    0,
                    &mut nodes,
                    limits.max_type_nodes,
                    &mut vec![],
                )?;
                for p in &method.params {
                    type_walk(&p.ty, 0, &mut nodes, limits.max_type_nodes, &mut vec![])?;
                }
                effect_walk(
                    &method.effects,
                    0,
                    &mut nodes,
                    limits.max_type_nodes,
                    &mut vec![],
                )?;
            }
        }
        for conformance in &self.source_conformances {
            type_walk(
                &conformance.self_type,
                0,
                &mut nodes,
                limits.max_type_nodes,
                &mut vec![],
            )?;
            for t in &conformance.trait_reference.args {
                type_walk(t, 0, &mut nodes, limits.max_type_nodes, &mut vec![])?;
            }
            for assoc in &conformance.associated {
                type_walk(&assoc.ty, 0, &mut nodes, limits.max_type_nodes, &mut vec![])?;
            }
            for clause in &conformance.where_clauses {
                type_walk(
                    &clause.ty,
                    0,
                    &mut nodes,
                    limits.max_type_nodes,
                    &mut vec![],
                )?;
                for t in &clause.trait_ref.args {
                    type_walk(t, 0, &mut nodes, limits.max_type_nodes, &mut vec![])?;
                }
            }
        }
        self.reachability = self.graph.reachability(&self.functions, limits)?;
        Ok(())
    }
    pub fn json(&self, limits: Limits) -> Result<Vec<u8>, String> {
        limits.validate()?;
        to_json(self, limits.max_output_bytes)
    }
    pub fn html(&self, limits: Limits) -> Result<Vec<u8>, String> {
        render_html(self, limits)
    }
}
pub fn render_html(value: &impl Serialize, limits: Limits) -> Result<Vec<u8>, String> {
    limits.validate()?;
    let json = to_json(value, limits.max_output_bytes)?;
    let mut output = Bounded {
        bytes: Vec::new(),
        limit: limits.max_output_bytes,
    };
    output.write_all(b"<!doctype html><html lang=\"en\"><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; style-src 'unsafe-inline'\"><title>Vesper checked API documentation</title><style>body{max-width:1000px;margin:2rem auto;padding:1rem;font:16px system-ui}pre{white-space:pre-wrap;overflow-wrap:anywhere;background:#f4f5f7;padding:1rem}h1{font-size:1.7rem}</style><h1>Checked source API</h1><p>No audited artifact-bound proof evidence supplied: unverified. Layouts describe the native eight-byte-aligned ABI, not wire formats. Terminal-path existence is not an all-path linearity proof.</p>").map_err(|_|"output byte budget exceeded")?;
    // A readable item overview accompanies the complete typed metadata. Deep
    // DTO JSON can exceed serde's reader recursion cap; the complete escaped
    // metadata remains available even when that optional overview is omitted.
    if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&json) {
        let doc = value.get("document").unwrap_or(&value);
        for (field, title) in [
            ("functions", "Functions"),
            ("types", "Types"),
            ("effects", "Effects"),
            ("traits", "Traits"),
        ] {
            if let Some(items) = doc.get(field).and_then(|v| v.as_array()) {
                output
                    .write_all(b"<section><h2>")
                    .map_err(|_| "output byte budget exceeded")?;
                escape_html(&mut output, title.as_bytes())?;
                output
                    .write_all(b"</h2>")
                    .map_err(|_| "output byte budget exceeded")?;
                for item in items {
                    let name = item
                        .get("name")
                        .or_else(|| item.get("declaration").and_then(|v| v.get("name")))
                        .and_then(|v| v.as_str())
                        .unwrap_or("item");
                    output
                        .write_all(b"<article><h3>")
                        .map_err(|_| "output byte budget exceeded")?;
                    escape_html(&mut output, name.as_bytes())?;
                    output
                        .write_all(b"</h3><pre>")
                        .map_err(|_| "output byte budget exceeded")?;
                    escape_html(
                        &mut output,
                        item.get("docs")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .as_bytes(),
                    )?;
                    output
                        .write_all(b"</pre></article>")
                        .map_err(|_| "output byte budget exceeded")?;
                }
                output
                    .write_all(b"</section>")
                    .map_err(|_| "output byte budget exceeded")?;
            }
        }
    }
    output
        .write_all(b"<h2>Complete typed metadata</h2><pre>")
        .map_err(|_| "output byte budget exceeded")?;
    escape_html(&mut output, &json)?;
    output
        .write_all(b"</pre></html>\n")
        .map_err(|_| "output byte budget exceeded")?;
    Ok(output.bytes)
}
fn escape_html(output: &mut Bounded, bytes: &[u8]) -> Result<(), String> {
    for byte in bytes {
        let escaped: &[u8] = match byte {
            b'&' => b"&amp;",
            b'<' => b"&lt;",
            b'>' => b"&gt;",
            b'"' => b"&quot;",
            b'\'' => b"&#39;",
            _ => std::slice::from_ref(byte),
        };
        output
            .write_all(escaped)
            .map_err(|_| "output byte budget exceeded")?;
    }
    Ok(())
}
struct Bounded {
    bytes: Vec<u8>,
    limit: usize,
}
impl Write for Bounded {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self
            .bytes
            .len()
            .checked_add(bytes.len())
            .is_none_or(|n| n > self.limit)
        {
            return Err(std::io::Error::other("output byte budget exceeded"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
pub fn to_json(value: &impl Serialize, max_bytes: usize) -> Result<Vec<u8>, String> {
    let mut output = Bounded {
        bytes: Vec::new(),
        limit: max_bytes,
    };
    serde_json::to_writer_pretty(&mut output, value)
        .map_err(|e| format!("bounded JSON output: {e}"))?;
    output.write_all(b"\n").map_err(|e| e.to_string())?;
    Ok(output.bytes)
}

/// Query types use the complete core Type JSON representation. Parameters need
/// no names; selectors are allowed footprints. No wildcard/generic inference.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueryParameter {
    pub pass: Pass,
    pub mode: Mode,
    pub ty: Type,
    pub selector: Selector,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Query {
    pub parameters: Vec<QueryParameter>,
    pub result: Type,
    pub result_max_mode: Mode,
    pub effect_ceiling: Effects,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SearchHit {
    pub function: String,
    pub score: u32,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SearchResults {
    pub matches: Vec<SearchHit>,
    pub unsupported_functions: Vec<String>,
}
fn canonical_type(ty: &Type) -> Type {
    match ty {
        Type::Named(n, args) if n == "Unit" && args.is_empty() => Type::unit(),
        Type::Named(n, args) => Type::Named(n.clone(), args.iter().map(canonical_type).collect()),
        Type::Tuple(args) => Type::Tuple(args.iter().map(canonical_type).collect()),
        Type::Function {
            params,
            result,
            effects,
            mode,
        } => Type::Function {
            params: params
                .iter()
                .map(|(p, t)| (*p, canonical_type(t)))
                .collect(),
            result: Box::new(canonical_type(result)),
            effects: canonical_effects(effects),
            mode: *mode,
        },
        Type::Row(e) => Type::Row(canonical_effects(e)),
        _ => ty.clone(),
    }
}
fn canonical_effects(effects: &Effects) -> Effects {
    let mut instances: Vec<_> = effects
        .instances
        .iter()
        .map(|e| Effect {
            label: e.label.clone(),
            name: e.name.clone(),
            args: e.args.iter().map(canonical_type).collect(),
        })
        .collect();
    instances.sort_by_key(|e| serde_json::to_string(e).expect("effect serialization"));
    instances.dedup();
    Effects {
        instances,
        tail: effects.tail.clone(),
    }
}
pub fn search(
    query: &Query,
    index: &[FunctionDoc],
    limits: Limits,
) -> Result<SearchResults, String> {
    limits.validate()?;
    if index.len() > limits.max_items || query.parameters.len() > 64 {
        return Err("search item/parameter budget exceeded".into());
    }
    if query.effect_ceiling.tail.is_some() {
        return Err("open query effect ceilings are unsupported".into());
    }
    let mut nodes = 0;
    let mut unsupported = Vec::new();
    type_walk(
        &query.result,
        0,
        &mut nodes,
        limits.max_type_nodes,
        &mut unsupported,
    )?;
    for p in &query.parameters {
        if !p.selector.valid() {
            return Err("invalid query field selector".into());
        }
        type_walk(
            &p.ty,
            0,
            &mut nodes,
            limits.max_type_nodes,
            &mut unsupported,
        )?;
    }
    effect_walk(
        &query.effect_ceiling,
        0,
        &mut nodes,
        limits.max_type_nodes,
        &mut unsupported,
    )?;
    if !unsupported.is_empty() {
        return Err(format!(
            "unsupported query contract: {}",
            unsupported.join("; ")
        ));
    }
    let ceiling = canonical_effects(&query.effect_ceiling);
    let mut result = SearchResults {
        matches: vec![],
        unsupported_functions: vec![],
    };
    let mut seen = BTreeSet::new();
    for function in index {
        if !seen.insert(&function.name) {
            return Err("duplicate search function identity".into());
        }
        let issues = check_function(function, &mut nodes, limits)?;
        if !issues.is_empty() {
            result.unsupported_functions.push(function.name.clone());
            continue;
        }
        if function.parameters.len() != query.parameters.len()
            || function.result_mode > query.result_max_mode
            || canonical_type(&function.result) != canonical_type(&query.result)
        {
            continue;
        }
        let effects = canonical_effects(&function.effects);
        if effects
            .instances
            .iter()
            .any(|e| !ceiling.instances.contains(e))
        {
            continue;
        }
        if function
            .parameters
            .iter()
            .zip(&query.parameters)
            .any(|(f, q)| {
                f.pass != q.pass
                    || q.mode > f.mode
                    || canonical_type(&f.ty) != canonical_type(&q.ty)
                    || !f.selector.within(&q.selector)
            })
        {
            continue;
        }
        let exact = function.result_mode == query.result_max_mode
            && effects == ceiling
            && function
                .parameters
                .iter()
                .zip(&query.parameters)
                .all(|(f, q)| {
                    f.mode == q.mode
                        && f.selector.within(&q.selector)
                        && q.selector.within(&f.selector)
                });
        result.matches.push(SearchHit {
            function: function.name.clone(),
            score: if exact { 10 } else { 0 },
        });
    }
    result.matches.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| a.function.cmp(&b.function))
    });
    result.unsupported_functions.sort();
    Ok(result)
}
impl TypestateGraph {
    pub fn from_functions(functions: &[FunctionDoc], limits: Limits) -> Result<Self, String> {
        limits.validate()?;
        if functions.len() > limits.max_items {
            return Err("typestate function budget exceeded".into());
        }
        let mut graph = Self::default();
        let mut states = BTreeMap::new();
        for f in functions {
            if !check_function(f, &mut 0, limits)?.is_empty() {
                continue;
            }
            for (ty, mode) in f
                .parameters
                .iter()
                .map(|p| (&p.ty, p.mode))
                .chain(std::iter::once((&f.result, f.result_mode)))
            {
                if mode == Mode::Linear && matches!(ty, Type::Named(..)) {
                    states
                        .entry(type_identity(ty)?)
                        .or_insert_with(|| ty.clone());
                }
            }
        }
        graph.states = states
            .iter()
            .map(|(identity, ty)| State {
                identity: identity.clone(),
                ty: ty.clone(),
            })
            .collect();
        for f in functions {
            if !check_function(f, &mut 0, limits)?.is_empty() {
                graph
                    .omitted
                    .push(format!("{}: unsupported contract", f.name));
                continue;
            }
            let owned = f
                .parameters
                .iter()
                .filter(|p| p.mode == Mode::Linear && p.pass == Pass::Sink)
                .count();
            for (i, p) in f.parameters.iter().enumerate() {
                if p.mode != Mode::Linear || p.pass == Pass::In {
                    continue;
                }
                let from = type_identity(&p.ty)?;
                if !states.contains_key(&from) {
                    graph.omitted.push(format!(
                        "{} parameter {i}: non-nominal linear state",
                        f.name
                    ));
                    continue;
                }
                let to = if p.pass == Pass::Inout {
                    Some(from.clone())
                } else if owned != 1 {
                    graph.omitted.push(format!(
                        "{}: multiple owned linear inputs make attribution ambiguous",
                        f.name
                    ));
                    continue;
                } else if f.result_mode != Mode::Linear {
                    None
                } else {
                    let id = type_identity(&f.result)?;
                    if !states.contains_key(&id) {
                        graph
                            .omitted
                            .push(format!("{}: non-nominal linear result", f.name));
                        continue;
                    }
                    Some(id)
                };
                graph.edges.push(Transition {
                    from,
                    function: f.name.clone(),
                    parameter: i,
                    pass: p.pass,
                    to,
                });
            }
        }
        graph.omitted.sort();
        graph.omitted.dedup();
        graph.validate(functions, limits)?;
        Ok(graph)
    }
    pub fn validate(&self, functions: &[FunctionDoc], limits: Limits) -> Result<(), String> {
        limits.validate()?;
        if self.states.len() > limits.max_items || self.edges.len() > limits.max_items * 64 {
            return Err("typestate graph budget exceeded".into());
        }
        let mut states = BTreeMap::new();
        let mut nodes = 0usize;
        for state in &self.states {
            let mut unsupported = vec![];
            type_walk(
                &state.ty,
                0,
                &mut nodes,
                limits.max_type_nodes,
                &mut unsupported,
            )?;
            if !unsupported.is_empty() {
                return Err("unresolved typestate identity".into());
            }
            if state.identity.len() > limits.max_output_bytes
                || state.identity != type_identity(&state.ty)?
                || !matches!(state.ty, Type::Named(..))
                || states.insert(&state.identity, &state.ty).is_some()
            {
                return Err("invalid/duplicate typestate identity".into());
            }
        }
        let function_count = functions.len();
        let functions: BTreeMap<_, _> = functions.iter().map(|f| (&f.name, f)).collect();
        if functions.len() != function_count || function_count > limits.max_items {
            return Err("invalid function index identity/budget".into());
        }
        let mut seen = BTreeSet::new();
        for edge in &self.edges {
            if !seen.insert((&edge.from, &edge.function, edge.parameter)) {
                return Err("duplicate typestate edge".into());
            }
            let from = states
                .get(&edge.from)
                .ok_or("unknown typestate edge source")?;
            let f = functions
                .get(&edge.function)
                .ok_or("unknown typestate function identity")?;
            if !check_function(f, &mut 0, limits)?.is_empty() {
                return Err("unsupported typestate function contract".into());
            }
            let p = f
                .parameters
                .get(edge.parameter)
                .ok_or("unknown typestate parameter")?;
            if &p.ty != *from || p.mode != Mode::Linear || p.pass != edge.pass || p.pass == Pass::In
            {
                return Err("typestate receiver mismatch".into());
            }
            match &edge.to {
                None => {
                    if edge.pass != Pass::Sink || f.result_mode == Mode::Linear {
                        return Err("invalid terminal edge".into());
                    }
                }
                Some(to) => {
                    let target = states.get(to).ok_or("unknown typestate edge target")?;
                    if edge.pass == Pass::Inout {
                        if to != &edge.from {
                            return Err("inout cannot discharge/change nominal state".into());
                        }
                    } else if f.result_mode != Mode::Linear || &f.result != *target {
                        return Err("typestate result mismatch".into());
                    }
                }
            }
            if edge.pass == Pass::Sink
                && f.parameters
                    .iter()
                    .filter(|p| p.mode == Mode::Linear && p.pass == Pass::Sink)
                    .count()
                    != 1
            {
                return Err("ambiguous owned transition".into());
            }
        }
        Ok(())
    }
    /// Existence of an API path only. Cycles may coexist with a terminal path;
    /// this never establishes universal termination or all-path linearity.
    pub fn reachability(
        &self,
        functions: &[FunctionDoc],
        limits: Limits,
    ) -> Result<Vec<StateReachability>, String> {
        self.validate(functions, limits)?;
        let mut reverse: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        let mut found = BTreeSet::new();
        let mut queue = VecDeque::new();
        for edge in &self.edges {
            match &edge.to {
                None => {
                    if found.insert(edge.from.as_str()) {
                        queue.push_back(edge.from.as_str());
                    }
                }
                Some(to) => reverse.entry(to).or_default().push(&edge.from),
            }
        }
        while let Some(state) = queue.pop_front() {
            if let Some(preds) = reverse.get(state) {
                for &pred in preds {
                    if found.insert(pred) {
                        queue.push_back(pred);
                    }
                }
            }
        }
        Ok(self
            .states
            .iter()
            .map(|s| StateReachability {
                identity: s.identity.clone(),
                terminal_path_exists: found.contains(s.identity.as_str()),
            })
            .collect())
    }
}

/// Strict structural-query JSON reader. The core AST serde types intentionally
/// permit some unknown fields for compiler compatibility; query contracts must
/// reject those fields instead of silently discarding an unfamiliar constraint.
pub fn parse_query_json(text: &str, limits: Limits) -> Result<Query, String> {
    limits.validate()?;
    if text.len() > 32_768 {
        return Err("query byte limit exceeded".into());
    }
    let value = unique_json(text)?;
    fn keys(value: &serde_json::Value, allowed: &[&str]) -> Result<(), String> {
        let object = value.as_object().ok_or("query contract object expected")?;
        for key in object.keys() {
            if !allowed.contains(&key.as_str()) {
                return Err(format!("unsupported query contract field '{key}'"));
            }
        }
        Ok(())
    }
    fn effects(
        value: &serde_json::Value,
        depth: usize,
        remaining: &mut usize,
    ) -> Result<(), String> {
        keys(value, &["instances", "tail"])?;
        if let Some(instances) = value.get("instances").and_then(|v| v.as_array()) {
            if instances.len() > 64 {
                return Err("query effect limit exceeded".into());
            }
            for effect in instances {
                keys(effect, &["label", "name", "args"])?;
                if let Some(args) = effect.get("args").and_then(|v| v.as_array()) {
                    for arg in args {
                        ty(arg, depth + 1, remaining)?;
                    }
                }
            }
        }
        Ok(())
    }
    fn ty(value: &serde_json::Value, depth: usize, remaining: &mut usize) -> Result<(), String> {
        *remaining = remaining
            .checked_sub(1)
            .ok_or("query type budget exceeded")?;
        if depth > 48 {
            return Err("query type depth exceeded".into());
        }
        if value.as_str() == Some("Infer") {
            return Ok(());
        }
        let object = value.as_object().ok_or("structural type object expected")?;
        if object.len() != 1 {
            return Err("one structural type variant expected".into());
        }
        let (tag, payload) = object.iter().next().ok_or("type variant expected")?;
        match tag.as_str() {
            "Named" => {
                if let Some(parts) = payload.as_array()
                    && let Some(args) = parts.get(1).and_then(|v| v.as_array())
                {
                    for arg in args {
                        ty(arg, depth + 1, remaining)?;
                    }
                }
            }
            "Tuple" => {
                if let Some(args) = payload.as_array() {
                    for arg in args {
                        ty(arg, depth + 1, remaining)?;
                    }
                }
            }
            "Function" => {
                keys(payload, &["params", "result", "effects", "mode"])?;
                if let Some(params) = payload.get("params").and_then(|v| v.as_array()) {
                    for param in params {
                        if let Some(t) = param.as_array().and_then(|p| p.get(1)) {
                            ty(t, depth + 1, remaining)?;
                        }
                    }
                }
                if let Some(t) = payload.get("result") {
                    ty(t, depth + 1, remaining)?;
                }
                if let Some(e) = payload.get("effects") {
                    effects(e, depth + 1, remaining)?;
                }
            }
            "Row" => effects(payload, depth + 1, remaining)?,
            "IntLiteral" => (),
            _ => return Err(format!("unsupported structural type variant '{tag}'")),
        }
        Ok(())
    }
    keys(
        &value,
        &["parameters", "result", "result_max_mode", "effect_ceiling"],
    )?;
    let mut remaining = limits.max_type_nodes;
    if let Some(parameters) = value.get("parameters").and_then(|v| v.as_array()) {
        if parameters.len() > 64 {
            return Err("query parameter limit exceeded".into());
        }
        for parameter in parameters {
            keys(parameter, &["pass", "mode", "ty", "selector"])?;
            if let Some(t) = parameter.get("ty") {
                ty(t, 0, &mut remaining)?;
            }
        }
    }
    if let Some(t) = value.get("result") {
        ty(t, 0, &mut remaining)?;
    }
    if let Some(e) = value.get("effect_ceiling") {
        effects(e, 0, &mut remaining)?;
    }
    serde_json::from_value(value).map_err(|e| e.to_string())
}

/// Reject duplicate decoded JSON keys before any constraint can be overwritten.
fn unique_json(text: &str) -> Result<serde_json::Value, String> {
    struct Unique(serde_json::Value);
    impl<'de> Deserialize<'de> for Unique {
        fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            struct Visitor;
            impl<'de> serde::de::Visitor<'de> for Visitor {
                type Value = Unique;
                fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    f.write_str("JSON without duplicate decoded keys")
                }
                fn visit_bool<E: serde::de::Error>(self, v: bool) -> Result<Unique, E> {
                    Ok(Unique(v.into()))
                }
                fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Unique, E> {
                    Ok(Unique(v.into()))
                }
                fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Unique, E> {
                    Ok(Unique(v.into()))
                }
                fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<Unique, E> {
                    serde_json::Number::from_f64(v)
                        .map(|n| Unique(n.into()))
                        .ok_or_else(|| E::custom("nonfinite JSON number"))
                }
                fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Unique, E> {
                    Ok(Unique(v.into()))
                }
                fn visit_string<E: serde::de::Error>(self, v: String) -> Result<Unique, E> {
                    Ok(Unique(v.into()))
                }
                fn visit_unit<E: serde::de::Error>(self) -> Result<Unique, E> {
                    Ok(Unique(serde_json::Value::Null))
                }
                fn visit_none<E: serde::de::Error>(self) -> Result<Unique, E> {
                    self.visit_unit()
                }
                fn visit_seq<A: serde::de::SeqAccess<'de>>(
                    self,
                    mut seq: A,
                ) -> Result<Unique, A::Error> {
                    let mut values = vec![];
                    while let Some(v) = seq.next_element::<Unique>()? {
                        values.push(v.0);
                    }
                    Ok(Unique(values.into()))
                }
                fn visit_map<A: serde::de::MapAccess<'de>>(
                    self,
                    mut map: A,
                ) -> Result<Unique, A::Error> {
                    let mut values = serde_json::Map::new();
                    while let Some(key) = map.next_key::<String>()? {
                        if values.contains_key(&key) {
                            return Err(serde::de::Error::custom("duplicate decoded query key"));
                        }
                        values.insert(key, map.next_value::<Unique>()?.0);
                    }
                    Ok(Unique(values.into()))
                }
            }
            deserializer.deserialize_any(Visitor)
        }
    }
    let mut deserializer = serde_json::Deserializer::from_str(text);
    let value = Unique::deserialize(&mut deserializer).map_err(|e| e.to_string())?;
    deserializer.end().map_err(|e| e.to_string())?;
    Ok(value.0)
}

/// Iterative audit before the facade's unchanged checker clones/traverses ASTs.
/// Lexical weighted segments keep rejected AST teardown shallow as well.
pub fn ast_preflight(module: &vesper_core::ast::Module) -> Result<(), String> {
    use vesper_core::ast::{
        Clause, Expr, ExprKind as E, Function, Item, Pattern, PatternKind as P, Statement,
    };
    enum Node<'a> {
        Item(&'a Item),
        Function(&'a Function),
        Type(&'a Type),
        Effects(&'a Effects),
        Expr(&'a Expr),
        Pattern(&'a Pattern),
        Clause(&'a Clause),
    }
    let mut generic_values = BTreeSet::new();
    let mut generic_constructors: BTreeSet<String> = [
        "Some",
        "None",
        "Ok",
        "Err",
        "Option::Some",
        "Option::None",
        "Result::Ok",
        "Result::Err",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    for item in &module.items {
        match item {
            Item::Function(f) if !f.generics.is_empty() => {
                generic_values.insert(f.name.clone());
            }
            Item::Impl {
                ty,
                generics,
                methods,
            } => {
                for f in methods {
                    if !generics.is_empty() || !f.generics.is_empty() {
                        generic_values.insert(f.name.clone());
                        generic_values.insert(format!("{}::{}", ty.name(), f.name));
                    }
                }
            }
            Item::Adt(a) if !a.generics.is_empty() => {
                generic_constructors.insert(a.name.clone());
                for v in &a.variants {
                    generic_constructors.insert(v.name.clone());
                    generic_constructors.insert(format!("{}::{}", a.name, v.name));
                }
            }
            Item::Conformance(c) if !c.generics.is_empty() => {
                return Err("generic conformance inference is unsupported before checking".into());
            }
            _ => (),
        }
    }
    let mut allowed_closures = BTreeSet::new();
    let mut stack: Vec<_> = module
        .items
        .iter()
        .map(|item| (Node::Item(item), 0usize))
        .collect();
    for bound in &module.trait_bounds {
        for arg in &bound.trait_ref.args {
            stack.push((Node::Type(arg), 0));
        }
    }
    let mut count = 0;
    while let Some((node, depth)) = stack.pop() {
        count += 1;
        if count > 16_384 || depth > 128 {
            return Err("parsed AST node/depth budget exceeded".into());
        }
        let next = depth + 1;
        match node {
            Node::Item(item) => match item {
                Item::Adt(a) => {
                    for f in a.variants.iter().flat_map(|v| &v.fields) {
                        stack.push((Node::Type(&f.ty), next));
                    }
                }
                Item::Function(f) => stack.push((Node::Function(f), next)),
                Item::Effect(e) => {
                    for f in &e.ops {
                        stack.push((Node::Function(f), next));
                    }
                }
                Item::Impl { ty, methods, .. } => {
                    stack.push((Node::Type(ty), next));
                    for f in methods {
                        stack.push((Node::Function(f), next));
                    }
                }
                Item::Trait(t) => {
                    for f in &t.methods {
                        stack.push((Node::Function(f), next));
                    }
                }
                Item::Conformance(c) => {
                    stack.push((Node::Type(&c.self_ty), next));
                    for t in &c.trait_ref.args {
                        stack.push((Node::Type(t), next));
                    }
                    for b in &c.assoc {
                        stack.push((Node::Type(&b.ty), next));
                    }
                    for w in &c.where_clauses {
                        stack.push((Node::Type(&w.ty), next));
                        for t in &w.trait_ref.args {
                            stack.push((Node::Type(t), next));
                        }
                    }
                    for f in &c.methods {
                        stack.push((Node::Function(f), next));
                    }
                }
            },
            Node::Function(f) => {
                if matches!(f.result, Type::Function { .. })
                    && let Some(Expr {
                        kind:
                            E::Block {
                                tail: Some(tail), ..
                            },
                        ..
                    }) = &f.body
                    && matches!(tail.kind, E::Closure { .. })
                {
                    allowed_closures.insert((tail.span.start, tail.span.end));
                }
                for p in &f.params {
                    stack.push((Node::Type(&p.ty), next));
                }
                stack.push((Node::Type(&f.result), next));
                stack.push((Node::Effects(&f.effects), next));
                if let Some(e) = &f.body {
                    stack.push((Node::Expr(e), next));
                }
            }
            Node::Type(t) => match t {
                Type::Named(_, ts) | Type::Tuple(ts) => {
                    for t in ts {
                        stack.push((Node::Type(t), next));
                    }
                }
                Type::Function {
                    params,
                    result,
                    effects,
                    ..
                } => {
                    for (_, t) in params {
                        stack.push((Node::Type(t), next));
                    }
                    stack.push((Node::Type(result), next));
                    stack.push((Node::Effects(effects), next));
                }
                Type::Row(e) => stack.push((Node::Effects(e), next)),
                Type::Infer | Type::IntLiteral(_) => (),
            },
            Node::Effects(e) => {
                for effect in &e.instances {
                    for t in &effect.args {
                        stack.push((Node::Type(t), next));
                    }
                }
            }
            Node::Pattern(p) => match &p.kind {
                P::Tuple(ps) => {
                    for p in ps {
                        stack.push((Node::Pattern(p), next));
                    }
                }
                P::Constructor { fields, .. } => {
                    for (_, p) in fields {
                        stack.push((Node::Pattern(p), next));
                    }
                }
                _ => (),
            },
            Node::Clause(c) => {
                for p in &c.pats {
                    stack.push((Node::Pattern(p), next));
                }
                stack.push((Node::Expr(&c.body), next));
            }
            Node::Expr(e) => match &e.kind {
                E::Literal(_) => (),
                E::Name(name) => {
                    if generic_values.contains(name) || generic_constructors.contains(name) {
                        return Err(
                            "generic value/constructor inference is unsupported before checking"
                                .into(),
                        );
                    }
                }
                E::Tuple(es) => {
                    if !es.is_empty() {
                        return Err(
                            "inferred tuple aggregate construction is unsupported before checking"
                                .into(),
                        );
                    }
                    for e in es {
                        stack.push((Node::Expr(e), next));
                    }
                }
                E::Struct { name, fields, .. } => {
                    if generic_constructors.contains(name) {
                        return Err(
                            "generic aggregate construction is unsupported before checking".into(),
                        );
                    }
                    for (_, e) in fields {
                        stack.push((Node::Expr(e), next));
                    }
                }
                E::Block { statements, tail } => {
                    for s in statements {
                        match s {
                            Statement::Let { pat, ty, value } => {
                                if matches!(ty, Type::Function { .. })
                                    && matches!(value.kind, E::Closure { .. })
                                {
                                    allowed_closures.insert((value.span.start, value.span.end));
                                }
                                stack.push((Node::Pattern(pat), next));
                                stack.push((Node::Type(ty), next));
                                stack.push((Node::Expr(value), next));
                            }
                            Statement::Expr(e) => stack.push((Node::Expr(e), next)),
                        }
                    }
                    if let Some(e) = tail {
                        stack.push((Node::Expr(e), next));
                    }
                }
                E::Call {
                    callee,
                    types,
                    args,
                } => {
                    stack.push((Node::Expr(callee), next));
                    for t in types {
                        stack.push((Node::Type(t), next));
                    }
                    for a in args {
                        stack.push((Node::Expr(&a.expr), next));
                    }
                }
                E::Field(_, name) if generic_values.contains(name) => {
                    return Err("generic method inference is unsupported before checking".into());
                }
                E::Field(e, _) | E::Try(e) | E::Unary(_, e) => stack.push((Node::Expr(e), next)),
                E::Index(a, b) | E::Binary(_, a, b) | E::Assign(_, a, b) => {
                    stack.push((Node::Expr(a), next));
                    stack.push((Node::Expr(b), next));
                }
                E::If {
                    condition,
                    then,
                    otherwise,
                } => {
                    stack.push((Node::Expr(condition), next));
                    stack.push((Node::Expr(then), next));
                    if let Some(e) = otherwise {
                        stack.push((Node::Expr(e), next));
                    }
                }
                E::Match { value, arms } => {
                    stack.push((Node::Expr(value), next));
                    for (p, e) in arms {
                        stack.push((Node::Pattern(p), next));
                        stack.push((Node::Expr(e), next));
                    }
                }
                E::Closure {
                    params,
                    result,
                    effects,
                    body,
                    ..
                } => {
                    if !allowed_closures.contains(&(e.span.start, e.span.end)) {
                        return Err("inferred closure growth is unsupported without a direct complete arrow annotation".into());
                    }
                    for p in params {
                        stack.push((Node::Type(&p.ty), next));
                    }
                    stack.push((Node::Type(result), next));
                    stack.push((Node::Effects(effects), next));
                    stack.push((Node::Expr(body), next));
                }
                E::Handle {
                    effect,
                    state,
                    body,
                    ret,
                    ops,
                } => {
                    for t in &effect.args {
                        stack.push((Node::Type(t), next));
                    }
                    if let Some((p, e)) = state {
                        stack.push((Node::Pattern(p), next));
                        stack.push((Node::Expr(e), next));
                    }
                    stack.push((Node::Expr(body), next));
                    stack.push((Node::Clause(ret), next));
                    for c in ops {
                        stack.push((Node::Clause(c), next));
                    }
                }
            },
        }
    }
    Ok(())
}
