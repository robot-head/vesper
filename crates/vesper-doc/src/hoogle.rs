//! Type-directed signature search over documented functions (§40.1 item 5),
//! shipped as a deterministic static index plus a small self-contained search
//! page. The page's matcher is a direct port of [`search`]; both read the same
//! index, so a query gives the same hits in the CLI and in the browser.
//!
//! Query text (`vesper doc --search`, or the page's input box):
//!
//! ```text
//! query  := params "->" ret [ "eff" "[" effect,* "]" ]   (signature search)
//!         | word                                         (name substring)
//! params := "" | "()" | param ("," param)*
//! param  := [in|inout|sink] [copy|affine|linear] type [ ".{" path,* "}" ]
//! ret    := [copy|affine|linear] type
//! type   := "_" | "()" | Name [ "[" type,* "]" ] | "(" type,* ")"
//! ```
//!
//! An omitted pass or mode is unconstrained, `_` matches any type, an omitted
//! `eff [...]` is an unconstrained ceiling and `eff []` means pure. Matching is
//! variance-aware: a query argument mode must be at most the parameter's mode
//! (`copy <= affine <= linear`, contravariant input), the function's result
//! mode at most the query's (covariant output), every function effect must be
//! inside the ceiling, and a query projection admits functions whose declared
//! footprint lies inside it.
//!
//! Generic functions are matched by instantiation: a type parameter `T` in a
//! signature unifies with any concrete query type, consistently across the
//! whole signature (`fn id[T](sink x: T) -> T` matches `u64 -> u64`, not
//! `u64 -> bool`). The parameter's mode in the index is its declared bound,
//! so the contravariant/covariant mode checks still hold for every
//! instantiation the bound admits. A hit lists the instantiation and is never
//! exact. Row binders, open rows, effect rows that mention a type parameter
//! and function-typed signatures are listed as unsupported rather than
//! matched approximately.
//!
//! A package index ([`build_package_index`]) holds every documented function
//! of every file under a file-qualified path, plus the methods of source
//! `impl` and Pact `model` declarations (`Point as Score::score`,
//! `model ByX::score`), each linking to its file's page.
use crate::{
    FunctionDoc, Limits, Mode, Pass, Selector, Type,
    badges::{Badge, BadgeLevel},
    visual::escape,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const INDEX_FORMAT: &str = "vesper-codex-search";
pub const INDEX_VERSION: u32 = 2;
/// The documentation layer's note for a generic function; search handles
/// non-row generics by instantiation, so this note alone does not exclude.
const GENERIC_NOTE: &str = "generic/row binder instantiation is unsupported by search";
const MAX_QUERY_BYTES: usize = 4096;
const MAX_QUERY_DEPTH: usize = 16;

/// A structural type in the index. Units are the empty tuple.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IndexType {
    Named {
        name: String,
        args: Vec<IndexType>,
    },
    Tuple {
        items: Vec<IndexType>,
    },
    /// A type parameter of the entry, instantiated by matching.
    Var {
        name: String,
    },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    Function,
    ImplMethod,
    ModelMethod,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexGeneric {
    pub name: String,
    pub bound: Mode,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexParam {
    pub name: String,
    pub pass: Pass,
    pub mode: Mode,
    pub ty: IndexType,
    pub text: String,
    /// Declared projection paths; empty is the whole value.
    pub selector: Vec<Vec<String>>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexEntry {
    pub name: String,
    /// Unique key: the name for one file; `file::name` in a package; the
    /// conformance-qualified method for impl and model methods.
    pub path: String,
    pub kind: EntryKind,
    /// Package-relative source file, in a package index.
    pub file: Option<String>,
    /// Page link of the item, in a package index (else `#item-NAME` of the
    /// page the index belongs to).
    pub href: Option<String>,
    pub generics: Vec<IndexGeneric>,
    pub trait_name: Option<String>,
    pub model: Option<String>,
    pub signature: String,
    pub params: Vec<IndexParam>,
    pub result: IndexType,
    pub result_mode: Mode,
    /// Canonical effect texts (`Name` or `Name[args]`), sorted and unique.
    pub effects: Vec<String>,
    pub badge: BadgeLevel,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchIndex {
    pub format: String,
    pub version: u32,
    pub entries: Vec<IndexEntry>,
    /// Functions excluded from the index (and so from every search), with
    /// the reason.
    pub unsupported: Vec<(String, String)>,
}

pub fn type_text(ty: &IndexType) -> String {
    match ty {
        IndexType::Named { name, args } if args.is_empty() => name.clone(),
        IndexType::Named { name, args } => format!(
            "{name}[{}]",
            args.iter().map(type_text).collect::<Vec<_>>().join(", ")
        ),
        IndexType::Tuple { items } => format!(
            "({})",
            items.iter().map(type_text).collect::<Vec<_>>().join(", ")
        ),
        IndexType::Var { name } => name.clone(),
    }
}

fn index_type(
    ty: &Type,
    vars: &BTreeSet<&str>,
    depth: usize,
    nodes: &mut usize,
) -> Result<IndexType, String> {
    *nodes = nodes.checked_sub(1).ok_or("search index type budget")?;
    if depth > 32 {
        return Err("type nesting exceeds the search index depth".into());
    }
    match ty {
        Type::Named(n, a) if n == "Unit" && a.is_empty() => Ok(IndexType::Tuple { items: vec![] }),
        Type::Named(n, a) if a.is_empty() && vars.contains(n.as_str()) => {
            Ok(IndexType::Var { name: n.clone() })
        }
        Type::Named(n, a) => Ok(IndexType::Named {
            name: n.clone(),
            args: a
                .iter()
                .map(|t| index_type(t, vars, depth + 1, nodes))
                .collect::<Result<_, _>>()?,
        }),
        Type::Tuple(ts) => Ok(IndexType::Tuple {
            items: ts
                .iter()
                .map(|t| index_type(t, vars, depth + 1, nodes))
                .collect::<Result<_, _>>()?,
        }),
        Type::Function { .. } => Err("function-typed signature".into()),
        Type::Row(_) => Err("row-typed signature".into()),
        Type::IntLiteral(_) | Type::Infer => Err("unresolved type".into()),
    }
}

fn mentions_var(t: &IndexType) -> bool {
    match t {
        IndexType::Var { .. } => true,
        IndexType::Named { args, .. } => args.iter().any(mentions_var),
        IndexType::Tuple { items } => items.iter().any(mentions_var),
    }
}

fn effect_text(
    e: &crate::Effect,
    vars: &BTreeSet<&str>,
    nodes: &mut usize,
) -> Result<String, String> {
    if e.args.is_empty() {
        return Ok(e.name.clone());
    }
    let args = e
        .args
        .iter()
        .map(|t| index_type(t, vars, 1, nodes))
        .collect::<Result<Vec<_>, _>>()?;
    if args.iter().any(mentions_var) {
        return Err("effect row mentions a type parameter".into());
    }
    let args: Vec<String> = args.iter().map(type_text).collect();
    Ok(format!("{}[{}]", e.name, args.join(", ")))
}

fn pass_text(p: Pass) -> &'static str {
    match p {
        Pass::In => "in",
        Pass::Inout => "inout",
        Pass::Sink => "sink",
    }
}

/// Where an entry comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EntryOrigin {
    pub path: String,
    pub kind: EntryKind,
    pub file: Option<String>,
    pub href: Option<String>,
    pub trait_name: Option<String>,
    pub model: Option<String>,
}
impl EntryOrigin {
    /// A function of a single-file index.
    pub fn local(name: &str) -> Self {
        Self {
            path: name.into(),
            kind: EntryKind::Function,
            file: None,
            href: None,
            trait_name: None,
            model: None,
        }
    }
}

fn entry(
    f: &FunctionDoc,
    origin: EntryOrigin,
    badge: BadgeLevel,
    nodes: &mut usize,
) -> Result<IndexEntry, String> {
    if f.generics.iter().any(|g| g.row) {
        return Err("row binders need instantiation".into());
    }
    if f.effects.tail.is_some() {
        return Err("open effect row".into());
    }
    let notes: Vec<&String> = f
        .unsupported
        .iter()
        .filter(|u| *u != GENERIC_NOTE)
        .collect();
    if !notes.is_empty() {
        return Err(notes
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join("; "));
    }
    let vars: BTreeSet<&str> = f.generics.iter().map(|g| g.name.as_str()).collect();
    if vars.len() != f.generics.len() {
        return Err("duplicate type parameter".into());
    }
    let mut params = Vec::new();
    for p in &f.parameters {
        let ty = index_type(&p.ty, &vars, 0, nodes)?;
        params.push(IndexParam {
            name: p.name.clone(),
            pass: p.pass,
            mode: p.mode,
            text: type_text(&ty),
            ty,
            selector: match &p.selector {
                Selector::All => vec![],
                Selector::Paths(s) => s.clone(),
            },
        });
    }
    let result = index_type(&f.result, &vars, 0, nodes)?;
    let effects: BTreeSet<String> = f
        .effects
        .instances
        .iter()
        .map(|e| effect_text(e, &vars, nodes))
        .collect::<Result<_, _>>()?;
    let effects: Vec<String> = effects.into_iter().collect();
    let generics = if f.generics.is_empty() {
        String::new()
    } else {
        format!(
            "[{}]",
            f.generics
                .iter()
                .map(|g| match g.bound {
                    Mode::Linear => g.name.clone(),
                    Mode::Affine => format!("{}: mod affine", g.name),
                    Mode::Copy => format!("{}: mod copy", g.name),
                })
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    let shown = match (origin.kind, &origin.file) {
        (EntryKind::Function, _) => f.name.clone(),
        (_, Some(file)) => origin
            .path
            .strip_prefix(&format!("{file}::"))
            .unwrap_or(&origin.path)
            .to_string(),
        (_, None) => origin.path.clone(),
    };
    let signature = format!(
        "fn {shown}{generics}({}) -> {}{}",
        params
            .iter()
            .map(|p| {
                let sel = if p.selector.is_empty() {
                    String::new()
                } else {
                    format!(
                        ".{{{}}}",
                        p.selector
                            .iter()
                            .map(|s| s.join("."))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                };
                format!("{} {}{sel}: {}", pass_text(p.pass), p.name, p.text)
            })
            .collect::<Vec<_>>()
            .join(", "),
        type_text(&result),
        if effects.is_empty() {
            String::new()
        } else {
            format!(" eff [{}]", effects.join(", "))
        }
    );
    Ok(IndexEntry {
        name: f.name.clone(),
        path: origin.path,
        kind: origin.kind,
        file: origin.file,
        href: origin.href,
        generics: f
            .generics
            .iter()
            .map(|g| IndexGeneric {
                name: g.name.clone(),
                bound: g.bound,
            })
            .collect(),
        trait_name: origin.trait_name,
        model: origin.model,
        signature,
        params,
        result,
        result_mode: f.result_mode,
        effects,
        badge,
    })
}

/// Build the static index. Badges come from [`crate::badges::badges`]; an
/// item without one is unverified.
pub fn build_index(
    functions: &[FunctionDoc],
    badges: &[Badge],
    limits: Limits,
) -> Result<SearchIndex, String> {
    limits.validate()?;
    if functions.len() > limits.max_items {
        return Err("search index item budget exceeded".into());
    }
    let mut nodes = limits.max_type_nodes;
    let mut index = SearchIndex {
        format: INDEX_FORMAT.into(),
        version: INDEX_VERSION,
        entries: vec![],
        unsupported: vec![],
    };
    let mut seen = BTreeSet::new();
    for f in functions {
        if !seen.insert(&f.name) {
            return Err(format!("duplicate search function identity {}", f.name));
        }
        let badge = badges
            .iter()
            .find(|b| b.kind == "function" && b.item == f.name)
            .map_or(BadgeLevel::Unverified, |b| b.level);
        match entry(f, EntryOrigin::local(&f.name), badge, &mut nodes) {
            Ok(e) => index.entries.push(e),
            Err(reason) if reason == "search index type budget" => return Err(reason),
            Err(reason) => index.unsupported.push((f.name.clone(), reason)),
        }
    }
    index.entries.sort_by(|a, b| a.path.cmp(&b.path));
    index.unsupported.sort();
    Ok(index)
}

/// A method of a source `impl Trait for T` or Pact `model M: Trait for T`
/// declaration. `function` documents the checked generated function.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ImplMethod {
    /// Package-qualified conformance declaration path.
    pub conformance: String,
    pub trait_name: String,
    pub method: String,
    pub self_ty: String,
    pub model: Option<String>,
    pub public: bool,
    pub function: FunctionDoc,
}
impl ImplMethod {
    /// `Point as Score::score`, or `model ByX::score`.
    pub fn path(&self) -> String {
        match &self.model {
            Some(m) => format!("model {m}::{}", self.method),
            None => format!("{} as {}::{}", self.self_ty, self.trait_name, self.method),
        }
    }
}

/// One file of a package index.
pub struct PackageFile<'a> {
    /// Package-relative path with `/` separators.
    pub file: &'a str,
    /// Link of the file's page, relative to the index page.
    pub href: &'a str,
    pub functions: &'a [FunctionDoc],
    pub badges: &'a [Badge],
    pub impls: &'a [ImplMethod],
}

/// Build one index over every file of a package: documented functions as
/// `file::name`, impl and model methods as `file::Self as Trait::method` /
/// `file::model M::method`. Exceeding a budget is an error.
pub fn build_package_index(
    files: &[PackageFile<'_>],
    limits: Limits,
) -> Result<SearchIndex, String> {
    limits.validate()?;
    let total = files.iter().try_fold(0usize, |n, f| {
        n.checked_add(f.functions.len())?.checked_add(f.impls.len())
    });
    if total.is_none_or(|n| n > limits.max_items.saturating_mul(16)) {
        return Err("package search index item budget exceeded".into());
    }
    let mut nodes = limits.max_type_nodes.saturating_mul(16);
    let mut index = SearchIndex {
        format: INDEX_FORMAT.into(),
        version: INDEX_VERSION,
        entries: vec![],
        unsupported: vec![],
    };
    let mut seen = BTreeSet::new();
    for file in files {
        let mut push = |f: &FunctionDoc, origin: EntryOrigin, badge| -> Result<(), String> {
            if !seen.insert(origin.path.clone()) {
                return Err(format!("duplicate search path {}", origin.path));
            }
            let path = origin.path.clone();
            match entry(f, origin, badge, &mut nodes) {
                Ok(e) => index.entries.push(e),
                Err(reason) if reason == "search index type budget" => return Err(reason),
                Err(reason) => index.unsupported.push((path, reason)),
            }
            Ok(())
        };
        for f in file.functions {
            let badge = file
                .badges
                .iter()
                .find(|b| b.kind == "function" && b.item == f.name)
                .map_or(BadgeLevel::Unverified, |b| b.level);
            push(
                f,
                EntryOrigin {
                    path: format!("{}::{}", file.file, f.name),
                    kind: EntryKind::Function,
                    file: Some(file.file.into()),
                    href: Some(format!("{}#item-{}", file.href, f.name)),
                    trait_name: None,
                    model: None,
                },
                badge,
            )?;
        }
        for m in file.impls {
            push(
                &m.function,
                EntryOrigin {
                    path: format!("{}::{}", file.file, m.path()),
                    kind: if m.model.is_some() {
                        EntryKind::ModelMethod
                    } else {
                        EntryKind::ImplMethod
                    },
                    file: Some(file.file.into()),
                    href: Some(format!("{}#codex-impls", file.href)),
                    trait_name: Some(m.trait_name.clone()),
                    model: m.model.clone(),
                },
                BadgeLevel::Unverified,
            )?;
        }
    }
    index.entries.sort_by(|a, b| a.path.cmp(&b.path));
    index.unsupported.sort();
    Ok(index)
}

/// Strict reader for an index written by [`build_index`].
pub fn parse_index(text: &str) -> Result<SearchIndex, String> {
    if text.len() > 8 * 1024 * 1024 {
        return Err("search index exceeds 8 MiB".into());
    }
    let index: SearchIndex =
        serde_json::from_str(text).map_err(|e| format!("invalid search index: {e}"))?;
    if index.format != INDEX_FORMAT || index.version != INDEX_VERSION {
        return Err("unsupported search index format or version".into());
    }
    Ok(index)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TypePattern {
    Any,
    Named {
        name: String,
        args: Vec<TypePattern>,
    },
    Tuple {
        items: Vec<TypePattern>,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParamPattern {
    pub pass: Option<Pass>,
    pub mode: Option<Mode>,
    pub ty: TypePattern,
    /// `None` admits any footprint; `Some(paths)` admits footprints inside it.
    pub selector: Option<Vec<Vec<String>>>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextQuery {
    Name(String),
    Signature {
        params: Vec<ParamPattern>,
        result: TypePattern,
        result_mode: Option<Mode>,
        /// `None` is unconstrained; `Some([])` is pure.
        ceiling: Option<Vec<String>>,
    },
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Word(String),
    Sym(char),
    Arrow,
}

fn tokens(text: &str) -> Result<Vec<Tok>, String> {
    let mut out = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => {}
            '-' if chars.peek() == Some(&'>') => {
                chars.next();
                out.push(Tok::Arrow);
            }
            '[' | ']' | '(' | ')' | ',' | '.' | '{' | '}' => out.push(Tok::Sym(c)),
            c if c.is_ascii_alphanumeric() || c == '_' => {
                let mut word = String::from(c);
                while let Some(&n) = chars.peek() {
                    if n.is_ascii_alphanumeric() || n == '_' {
                        word.push(n);
                        chars.next();
                    } else {
                        break;
                    }
                }
                if word.len() > 128 {
                    return Err("query identifier exceeds 128 bytes".into());
                }
                out.push(Tok::Word(word));
            }
            c => return Err(format!("unexpected character '{c}' in query")),
        }
        if out.len() > 512 {
            return Err("query token limit exceeded".into());
        }
    }
    Ok(out)
}

struct Parser {
    toks: Vec<Tok>,
    at: usize,
}
impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.at)
    }
    fn eat_sym(&mut self, c: char) -> bool {
        if self.peek() == Some(&Tok::Sym(c)) {
            self.at += 1;
            true
        } else {
            false
        }
    }
    fn expect_sym(&mut self, c: char) -> Result<(), String> {
        if self.eat_sym(c) {
            Ok(())
        } else {
            Err(format!("expected '{c}' in query"))
        }
    }
    fn word(&mut self) -> Option<String> {
        if let Some(Tok::Word(w)) = self.peek() {
            let w = w.clone();
            self.at += 1;
            Some(w)
        } else {
            None
        }
    }
    fn peek_word(&self, words: &[&str]) -> Option<String> {
        match self.peek() {
            Some(Tok::Word(w)) if words.contains(&w.as_str()) => Some(w.clone()),
            _ => None,
        }
    }
    fn mode(&mut self) -> Option<Mode> {
        let m = match self.peek_word(&["copy", "affine", "linear"])?.as_str() {
            "copy" => Mode::Copy,
            "affine" => Mode::Affine,
            _ => Mode::Linear,
        };
        // A mode keyword directly followed by a type is a qualifier.
        if matches!(
            self.toks.get(self.at + 1),
            Some(Tok::Word(_)) | Some(Tok::Sym('('))
        ) {
            self.at += 1;
            Some(m)
        } else {
            None
        }
    }
    fn ty(&mut self, depth: usize) -> Result<TypePattern, String> {
        if depth > MAX_QUERY_DEPTH {
            return Err("query type nesting limit".into());
        }
        if self.eat_sym('(') {
            let mut items = Vec::new();
            if !self.eat_sym(')') {
                loop {
                    items.push(self.ty(depth + 1)?);
                    if self.eat_sym(')') {
                        break;
                    }
                    self.expect_sym(',')?;
                }
            }
            return Ok(TypePattern::Tuple { items });
        }
        let name = self.word().ok_or("expected a type in query")?;
        if name == "_" {
            return Ok(TypePattern::Any);
        }
        if name == "Unit" {
            return Ok(TypePattern::Tuple { items: vec![] });
        }
        if !name.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
            return Err(format!("invalid type name '{name}' in query"));
        }
        let mut args = Vec::new();
        if self.eat_sym('[') {
            loop {
                args.push(self.ty(depth + 1)?);
                if self.eat_sym(']') {
                    break;
                }
                self.expect_sym(',')?;
            }
        }
        Ok(TypePattern::Named { name, args })
    }
    fn selector(&mut self) -> Result<Option<Vec<Vec<String>>>, String> {
        if !(self.peek() == Some(&Tok::Sym('.'))
            && self.toks.get(self.at + 1) == Some(&Tok::Sym('{')))
        {
            return Ok(None);
        }
        self.at += 2;
        let mut paths = Vec::new();
        loop {
            let mut path = vec![self.word().ok_or("expected a field name in projection")?];
            while self.eat_sym('.') {
                path.push(self.word().ok_or("expected a field name after '.'")?);
            }
            paths.push(path);
            if paths.len() > 64 {
                return Err("query projection limit".into());
            }
            if self.eat_sym('}') {
                break;
            }
            self.expect_sym(',')?;
        }
        Ok(Some(paths))
    }
    fn param(&mut self) -> Result<ParamPattern, String> {
        let pass = match self.peek_word(&["in", "inout", "sink"]).as_deref() {
            Some(p) => {
                self.at += 1;
                Some(match p {
                    "in" => Pass::In,
                    "inout" => Pass::Inout,
                    _ => Pass::Sink,
                })
            }
            None => None,
        };
        let mode = self.mode();
        let ty = self.ty(0)?;
        let selector = self.selector()?;
        Ok(ParamPattern {
            pass,
            mode,
            ty,
            selector,
        })
    }
}

/// Parse query text. Errors are typed: nothing is guessed.
pub fn parse_text_query(text: &str) -> Result<TextQuery, String> {
    if text.len() > MAX_QUERY_BYTES {
        return Err("query exceeds 4 KiB".into());
    }
    let toks = tokens(text)?;
    if toks.is_empty() {
        return Err("empty query".into());
    }
    if !toks.contains(&Tok::Arrow) {
        return match toks.as_slice() {
            [Tok::Word(w)] => Ok(TextQuery::Name(w.clone())),
            _ => Err("a signature query needs '->'; a name query is one word".into()),
        };
    }
    let mut p = Parser { toks, at: 0 };
    let mut params = Vec::new();
    if p.peek() == Some(&Tok::Sym('('))
        && p.toks.get(1) == Some(&Tok::Sym(')'))
        && p.toks.get(2) == Some(&Tok::Arrow)
    {
        p.at = 2;
    } else if p.peek() != Some(&Tok::Arrow) {
        loop {
            params.push(p.param()?);
            if params.len() > 64 {
                return Err("query parameter limit".into());
            }
            if !p.eat_sym(',') {
                break;
            }
        }
    }
    if p.peek() != Some(&Tok::Arrow) {
        return Err("expected '->' after the query parameters".into());
    }
    p.at += 1;
    let result_mode = p.mode();
    let result = p.ty(0)?;
    let ceiling = if p.peek_word(&["eff"]).is_some() {
        p.at += 1;
        p.expect_sym('[')?;
        let mut effects = Vec::new();
        if !p.eat_sym(']') {
            loop {
                let ty = p.ty(0)?;
                let TypePattern::Named { .. } = ty else {
                    return Err("an effect must be named".into());
                };
                effects.push(ty);
                if p.eat_sym(']') {
                    break;
                }
                p.expect_sym(',')?;
            }
        }
        let mut texts = Vec::new();
        for e in &effects {
            texts.push(pattern_text(e).ok_or("effect arguments cannot be wildcards")?);
        }
        texts.sort();
        texts.dedup();
        Some(texts)
    } else {
        None
    };
    if p.at != p.toks.len() {
        return Err("unexpected trailing query text".into());
    }
    Ok(TextQuery::Signature {
        params,
        result,
        result_mode,
        ceiling,
    })
}

fn pattern_text(p: &TypePattern) -> Option<String> {
    match p {
        TypePattern::Any => None,
        TypePattern::Named { name, args } if args.is_empty() => Some(name.clone()),
        TypePattern::Named { name, args } => Some(format!(
            "{name}[{}]",
            args.iter()
                .map(pattern_text)
                .collect::<Option<Vec<_>>>()?
                .join(", ")
        )),
        TypePattern::Tuple { items } => Some(format!(
            "({})",
            items
                .iter()
                .map(pattern_text)
                .collect::<Option<Vec<_>>>()?
                .join(", ")
        )),
    }
}

fn pattern_display(p: &TypePattern) -> String {
    match p {
        TypePattern::Any => "_".into(),
        TypePattern::Named { name, args } if args.is_empty() => name.clone(),
        TypePattern::Named { name, args } => format!(
            "{name}[{}]",
            args.iter()
                .map(pattern_display)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        TypePattern::Tuple { items } => format!(
            "({})",
            items
                .iter()
                .map(pattern_display)
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// Whether the pattern matches, and whether it did so without a wildcard
/// or an instantiated type parameter. `binds` holds the instantiation: a
/// parameter takes the first non-wildcard pattern it meets, and every later
/// occurrence must be that same pattern.
fn type_match(
    p: &TypePattern,
    t: &IndexType,
    binds: &mut Vec<(String, TypePattern)>,
) -> Option<bool> {
    match (p, t) {
        (TypePattern::Any, _) => Some(false),
        (_, IndexType::Var { name }) => {
            match binds.iter().find(|(n, _)| n == name) {
                Some((_, bound)) if bound != p => return None,
                Some(_) => {}
                None => binds.push((name.clone(), p.clone())),
            }
            Some(false)
        }
        (TypePattern::Named { name, args }, IndexType::Named { name: n, args: a })
            if name == n && args.len() == a.len() =>
        {
            let mut exact = true;
            for (p, t) in args.iter().zip(a) {
                exact &= type_match(p, t, binds)?;
            }
            Some(exact)
        }
        (TypePattern::Tuple { items }, IndexType::Tuple { items: i }) if items.len() == i.len() => {
            let mut exact = true;
            for (p, t) in items.iter().zip(i) {
                exact &= type_match(p, t, binds)?;
            }
            Some(exact)
        }
        _ => None,
    }
}

fn within(inner: &[Vec<String>], outer: &[Vec<String>]) -> bool {
    // An empty footprint is the whole value.
    if outer.is_empty() {
        return true;
    }
    !inner.is_empty() && inner.iter().all(|p| outer.iter().any(|q| p.starts_with(q)))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextHit {
    pub name: String,
    pub path: String,
    pub kind: EntryKind,
    pub href: Option<String>,
    pub signature: String,
    pub score: u32,
    pub exact: bool,
    pub badge: BadgeLevel,
    /// Type parameter bindings of a generic match, in parameter order
    /// (`_` where only a wildcard reached it).
    pub instantiation: Vec<(String, String)>,
}

/// Score: 10 for an exact contract match (no wildcard or instantiation,
/// every pass and mode given and equal, equal footprints and equal effects),
/// plus 5 for a proved and 2 for an audited badge. Ties sort by path. A name
/// query matches a case-insensitive substring of the path (so a trait or
/// model name finds its methods) and is exact when it equals the name.
pub fn search(query: &TextQuery, index: &SearchIndex) -> Vec<TextHit> {
    let mut hits = Vec::new();
    for e in &index.entries {
        let mut binds = Vec::new();
        let exact = match query {
            TextQuery::Name(word) => {
                if !e.path.to_lowercase().contains(&word.to_lowercase()) {
                    continue;
                }
                e.name == *word
            }
            TextQuery::Signature {
                params,
                result,
                result_mode,
                ceiling,
            } => {
                if params.len() != e.params.len() {
                    continue;
                }
                let mut ok = true;
                let mut exact = true;
                // Parameters first, so instantiation follows argument order.
                for (q, f) in params.iter().zip(&e.params) {
                    let Some(ty_exact) = type_match(&q.ty, &f.ty, &mut binds) else {
                        ok = false;
                        break;
                    };
                    if q.pass.is_some_and(|p| p != f.pass)
                        || q.mode.is_some_and(|m| m > f.mode)
                        || q.selector.as_ref().is_some_and(|s| !within(&f.selector, s))
                    {
                        ok = false;
                        break;
                    }
                    exact &= ty_exact
                        && q.pass.is_some()
                        && q.mode == Some(f.mode)
                        && q.selector.as_deref().unwrap_or(&[]) == f.selector.as_slice();
                }
                if !ok {
                    continue;
                }
                let Some(r) = type_match(result, &e.result, &mut binds) else {
                    continue;
                };
                exact &= r;
                if let Some(m) = result_mode {
                    if e.result_mode > *m {
                        continue;
                    }
                    exact &= e.result_mode == *m;
                } else {
                    exact = false;
                }
                if let Some(ceiling) = ceiling {
                    if !e.effects.iter().all(|x| ceiling.contains(x)) {
                        continue;
                    }
                    exact &= e.effects == *ceiling;
                } else {
                    exact = false;
                }
                exact && e.generics.is_empty()
            }
        };
        let score = u32::from(exact) * 10
            + match e.badge {
                BadgeLevel::Proved => 5,
                BadgeLevel::Audited => 2,
                BadgeLevel::Unverified => 0,
            };
        let instantiation = e
            .generics
            .iter()
            .filter_map(|g| {
                binds
                    .iter()
                    .find(|(n, _)| *n == g.name)
                    .map(|(n, p)| (n.clone(), pattern_display(p)))
            })
            .collect();
        hits.push(TextHit {
            name: e.name.clone(),
            path: e.path.clone(),
            kind: e.kind,
            href: e.href.clone(),
            signature: e.signature.clone(),
            score,
            exact,
            badge: e.badge,
            instantiation,
        });
    }
    hits.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.path.cmp(&b.path)));
    hits
}

/// The page script: a line-for-line port of [`parse_text_query`] and
/// [`search`] over the embedded index.
pub const SEARCH_JS: &str = include_str!("search.js");

/// A self-contained search page. The index is embedded as JSON (no fetch, no
/// network, works from `file://`); results link to `index_href#item-NAME`.
pub fn search_page(index: &SearchIndex, index_href: &str) -> Result<String, String> {
    let json = serde_json::to_string(index).map_err(|e| e.to_string())?;
    // `<` never appears raw inside the script element.
    let json = json.replace('<', "\\u003c");
    Ok(format!(
        "<!doctype html><html lang=\"en\"><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline'\"><title>Vesper Codex search</title><style>body{{max-width:1000px;margin:2rem auto;padding:1rem;font:16px system-ui}}input{{width:100%;font:16px ui-monospace,monospace;padding:.4rem}}li{{margin:.4rem 0}}code{{font:14px ui-monospace,monospace}}.b-proved{{color:#2d7a3e}}.b-audited{{color:#a06b00}}#err{{color:#b00020}}</style><h1>Signature search</h1><p>Query like <code>inout Tx.{{total}}, u64 -&gt; ()</code>, <code>sink linear Tx -&gt; u64 eff []</code> or a name. <code>_</code> matches any type; omitted passes, modes and <code>eff [...]</code> are unconstrained. <a href=\"{}\">Back to the API</a>.</p><input id=\"q\" autofocus placeholder=\"_ -&gt; u64\" aria-label=\"Query\"><p id=\"err\" role=\"alert\"></p><ol id=\"hits\"></ol><script type=\"application/json\" id=\"codex-index\">{json}</script><script>const CODEX_HREF={};\n{SEARCH_JS}</script></html>\n",
        escape(index_href),
        serde_json::to_string(index_href)
            .map_err(|e| e.to_string())?
            .replace('<', "\\u003c"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(n: &str) -> IndexType {
        IndexType::Named {
            name: n.into(),
            args: vec![],
        }
    }
    fn param(pass: Pass, mode: Mode, ty: IndexType, selector: &[&str]) -> IndexParam {
        IndexParam {
            name: "p".into(),
            pass,
            mode,
            text: type_text(&ty),
            ty,
            selector: selector
                .iter()
                .map(|s| s.split('.').map(String::from).collect())
                .collect(),
        }
    }
    fn entry(
        name: &str,
        params: Vec<IndexParam>,
        result: IndexType,
        mode: Mode,
        effects: &[&str],
        badge: BadgeLevel,
    ) -> IndexEntry {
        IndexEntry {
            name: name.into(),
            path: name.into(),
            kind: EntryKind::Function,
            file: None,
            href: None,
            generics: vec![],
            trait_name: None,
            model: None,
            signature: name.into(),
            params,
            result,
            result_mode: mode,
            effects: effects.iter().map(|s| s.to_string()).collect(),
            badge,
        }
    }
    fn index() -> SearchIndex {
        let option = IndexType::Named {
            name: "Option".into(),
            args: vec![named("u64")],
        };
        SearchIndex {
            format: INDEX_FORMAT.into(),
            version: INDEX_VERSION,
            entries: vec![
                entry(
                    "commit",
                    vec![param(Pass::Sink, Mode::Linear, named("Txn"), &[])],
                    named("Receipt"),
                    Mode::Copy,
                    &["Storage"],
                    BadgeLevel::Proved,
                ),
                entry(
                    "abort",
                    vec![param(Pass::Sink, Mode::Linear, named("Txn"), &[])],
                    IndexType::Tuple { items: vec![] },
                    Mode::Copy,
                    &[],
                    BadgeLevel::Unverified,
                ),
                entry(
                    "peek",
                    vec![param(Pass::In, Mode::Copy, named("Txn"), &["head.len"])],
                    option,
                    Mode::Copy,
                    &["Alloc"],
                    BadgeLevel::Audited,
                ),
                entry(
                    "open",
                    vec![],
                    named("Txn"),
                    Mode::Linear,
                    &["Fail[IoError]", "Storage"],
                    BadgeLevel::Unverified,
                ),
            ],
            unsupported: vec![],
        }
    }
    fn names(q: &str) -> Vec<String> {
        search(&parse_text_query(q).unwrap(), &index())
            .into_iter()
            .map(|h| h.name)
            .collect()
    }

    #[test]
    fn variance_and_ceilings() {
        // Contravariant input: a copy or affine argument may be passed where
        // the parameter is linear, not the other way round.
        assert_eq!(names("sink copy Txn -> _"), ["commit", "abort"]);
        assert_eq!(names("sink linear Txn -> _"), ["commit", "abort"]);
        // Covariant result: a linear-returning function does not satisfy a
        // query that can only hold an affine result.
        assert_eq!(names("() -> affine Txn"), Vec::<String>::new());
        assert_eq!(names("() -> linear Txn"), ["open"]);
        // Effect ceilings are subsets; eff [] is pure.
        assert_eq!(names("sink Txn -> _ eff []"), ["abort"]);
        assert_eq!(
            names("sink Txn -> _ eff [Storage, Alloc]"),
            ["commit", "abort"]
        );
        assert_eq!(names("-> Txn eff [Storage]"), Vec::<String>::new());
        assert_eq!(names("-> Txn eff [Storage, Fail[IoError]]"), ["open"]);
        // Projections: the function footprint must lie inside the query's.
        assert_eq!(names("in Txn.{head} -> _"), ["peek"]);
        assert_eq!(names("in Txn.{head.len} -> Option[u64]"), ["peek"]);
        assert_eq!(names("in Txn.{tail} -> _"), Vec::<String>::new());
        assert_eq!(names("in Txn -> Option[_]"), ["peek"]);
        assert_eq!(names("in Txn -> Option[bool]"), Vec::<String>::new());
        // Pass must agree when given.
        assert_eq!(names("inout Txn -> _"), Vec::<String>::new());
        assert_eq!(names("Unit -> _"), Vec::<String>::new());
        assert_eq!(names("TXN"), Vec::<String>::new());
        assert_eq!(names("o"), ["commit", "abort", "open"]);
    }

    #[test]
    fn exact_and_proved_rank_first() {
        let hits = search(
            &parse_text_query("sink linear Txn -> copy Receipt eff [Storage]").unwrap(),
            &index(),
        );
        assert_eq!((hits[0].score, hits[0].exact), (15, true));
        let hits = search(&parse_text_query("sink Txn -> _").unwrap(), &index());
        assert_eq!(hits.iter().map(|h| h.score).collect::<Vec<_>>(), [5, 0]);
    }

    #[test]
    fn malformed_queries_are_typed_errors() {
        for bad in [
            "",
            "a b",
            "Txn",
            "-> ",
            "sink -> Txn",
            "in Txn.{} -> _",
            "in Txn.{a,} -> _",
            "-> _ eff [_]",
            "-> _ eff [Fail[_]]",
            "-> _ eff Storage",
            "-> Option[",
            "-> 9x",
            "-> a $",
            "-> (a, b",
            "-> _ trailing",
        ] {
            assert!(parse_text_query(bad).is_err() || bad == "Txn", "{bad}");
        }
        assert_eq!(
            parse_text_query("Txn").unwrap(),
            TextQuery::Name("Txn".into())
        );
        let deep = format!("-> {}u64{}", "Option[".repeat(40), "]".repeat(40));
        assert!(parse_text_query(&deep).unwrap_err().contains("nesting"));
        assert!(parse_text_query(&"a".repeat(5000)).is_err());
        assert!(parse_text_query(&format!("-> {}", "x".repeat(200))).is_err());
    }

    #[test]
    fn generics_instantiate_consistently_within_their_bounds() {
        let var = |n: &str| IndexType::Var { name: n.into() };
        let mut keep = entry(
            "keep",
            vec![param(Pass::Sink, Mode::Linear, var("T"), &[])],
            var("T"),
            Mode::Linear,
            &[],
            BadgeLevel::Unverified,
        );
        keep.generics = vec![IndexGeneric {
            name: "T".into(),
            bound: Mode::Linear,
        }];
        let mut tag = entry(
            "tag",
            vec![
                param(Pass::In, Mode::Copy, named("u64"), &[]),
                param(Pass::In, Mode::Copy, var("T"), &[]),
            ],
            var("T"),
            Mode::Copy,
            &[],
            BadgeLevel::Unverified,
        );
        tag.generics = vec![IndexGeneric {
            name: "T".into(),
            bound: Mode::Copy,
        }];
        let idx = SearchIndex {
            format: INDEX_FORMAT.into(),
            version: INDEX_VERSION,
            entries: vec![keep, tag],
            unsupported: vec![],
        };
        let hits = |q: &str| search(&parse_text_query(q).unwrap(), &idx);
        let keep = hits("sink Option[u64] -> Option[u64]");
        assert_eq!(keep.len(), 1);
        assert_eq!(
            keep[0].instantiation,
            [("T".to_string(), "Option[u64]".to_string())]
        );
        assert!(!keep[0].exact);
        // Inconsistent instantiation and over-bound modes do not match.
        assert!(hits("sink u64 -> bool").is_empty());
        assert_eq!(hits("u64, bool -> bool")[0].name, "tag");
        assert!(hits("u64, linear Txn -> Txn").is_empty());
        // A wildcard never binds: `_` then a concrete type still matches.
        assert_eq!(
            hits("u64, _ -> bool")[0].instantiation,
            [("T".to_string(), "bool".to_string())]
        );
        assert_eq!(
            hits("sink _ -> _")[0].instantiation,
            Vec::<(String, String)>::new()
        );
    }

    #[test]
    fn package_index_qualifies_paths_and_rejects_duplicates() {
        use crate::{Effects, FunctionDoc, Parameter};
        let f = |name: &str| FunctionDoc {
            name: name.into(),
            docs: String::new(),
            span: crate::Span { start: 0, end: 0 },
            generics: vec![],
            parameters: vec![Parameter {
                name: "x".into(),
                pass: Pass::In,
                mode: Mode::Copy,
                ty: Type::named("u64"),
                selector: Selector::All,
            }],
            result: Type::named("u64"),
            declared_result: Type::named("u64"),
            result_mode: Mode::Copy,
            effects: Effects::default(),
            effect_resumptions: vec![],
            unsupported: vec![],
        };
        let a = [f("same")];
        let b = [f("same")];
        let impls = [ImplMethod {
            conformance: "impl p::Show.Point".into(),
            trait_name: "Show".into(),
            method: "show".into(),
            self_ty: "Point".into(),
            model: None,
            public: true,
            function: f("$gen"),
        }];
        let files = [
            PackageFile {
                file: "a.ves",
                href: "files/a.ves.html",
                functions: &a,
                badges: &[],
                impls: &impls,
            },
            PackageFile {
                file: "b.ves",
                href: "files/b.ves.html",
                functions: &b,
                badges: &[],
                impls: &[],
            },
        ];
        let idx = build_package_index(&files, Limits::default()).unwrap();
        let paths: Vec<&str> = idx.entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(
            paths,
            ["a.ves::Point as Show::show", "a.ves::same", "b.ves::same"]
        );
        assert_eq!(idx.entries[0].kind, EntryKind::ImplMethod);
        assert_eq!(
            idx.entries[0].signature,
            "fn Point as Show::show(in x: u64) -> u64"
        );
        let hits = search(&parse_text_query("Show").unwrap(), &idx);
        assert_eq!(hits.len(), 1);
        let dup = [files[0].functions, files[0].functions].concat();
        let files = [PackageFile {
            file: "a.ves",
            href: "a.html",
            functions: &dup,
            badges: &[],
            impls: &[],
        }];
        assert!(build_package_index(&files, Limits::default()).is_err());
    }

    #[test]
    fn index_reader_is_strict() {
        let text = serde_json::to_string(&index()).unwrap();
        assert_eq!(parse_index(&text).unwrap(), index());
        assert!(parse_index(&text.replace(INDEX_FORMAT, "other")).is_err());
        assert!(parse_index("{}").is_err());
    }

    #[test]
    fn search_page_is_self_contained_and_escapes_the_index() {
        let mut idx = index();
        idx.entries[0].signature = "</script><script>alert(1)</script>".into();
        let page = search_page(&idx, "index.html").unwrap();
        assert!(!page.contains("</script><script>alert"));
        assert!(page.contains("script-src 'unsafe-inline'"));
        assert!(page.contains(SEARCH_JS));
    }
}
