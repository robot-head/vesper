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
//! footprint lies inside it. Generic, open-row and function-typed signatures
//! are listed as unsupported rather than matched approximately.
use crate::{
    FunctionDoc, Limits, Mode, Pass, Selector, Type,
    badges::{Badge, BadgeLevel},
    visual::escape,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const INDEX_FORMAT: &str = "vesper-codex-search";
pub const INDEX_VERSION: u32 = 1;
const MAX_QUERY_BYTES: usize = 4096;
const MAX_QUERY_DEPTH: usize = 16;

/// A structural type in the index. Units are the empty tuple.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IndexType {
    Named { name: String, args: Vec<IndexType> },
    Tuple { items: Vec<IndexType> },
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
    }
}

fn index_type(ty: &Type, depth: usize, nodes: &mut usize) -> Result<IndexType, String> {
    *nodes = nodes.checked_sub(1).ok_or("search index type budget")?;
    if depth > 32 {
        return Err("type nesting exceeds the search index depth".into());
    }
    match ty {
        Type::Named(n, a) if n == "Unit" && a.is_empty() => Ok(IndexType::Tuple { items: vec![] }),
        Type::Named(n, a) => Ok(IndexType::Named {
            name: n.clone(),
            args: a
                .iter()
                .map(|t| index_type(t, depth + 1, nodes))
                .collect::<Result<_, _>>()?,
        }),
        Type::Tuple(ts) => Ok(IndexType::Tuple {
            items: ts
                .iter()
                .map(|t| index_type(t, depth + 1, nodes))
                .collect::<Result<_, _>>()?,
        }),
        Type::Function { .. } => Err("function-typed signature".into()),
        Type::Row(_) => Err("row-typed signature".into()),
        Type::IntLiteral(_) | Type::Infer => Err("unresolved type".into()),
    }
}

fn effect_text(e: &crate::Effect, nodes: &mut usize) -> Result<String, String> {
    if e.args.is_empty() {
        return Ok(e.name.clone());
    }
    let args = e
        .args
        .iter()
        .map(|t| index_type(t, 1, nodes).map(|t| type_text(&t)))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(format!("{}[{}]", e.name, args.join(", ")))
}

fn pass_text(p: Pass) -> &'static str {
    match p {
        Pass::In => "in",
        Pass::Inout => "inout",
        Pass::Sink => "sink",
    }
}

fn entry(f: &FunctionDoc, badge: BadgeLevel, nodes: &mut usize) -> Result<IndexEntry, String> {
    if !f.generics.is_empty() {
        return Err("generic/row binders need instantiation".into());
    }
    if f.effects.tail.is_some() {
        return Err("open effect row".into());
    }
    if !f.unsupported.is_empty() {
        return Err(f.unsupported.join("; "));
    }
    let mut params = Vec::new();
    for p in &f.parameters {
        let ty = index_type(&p.ty, 0, nodes)?;
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
    let result = index_type(&f.result, 0, nodes)?;
    let effects: BTreeSet<String> = f
        .effects
        .instances
        .iter()
        .map(|e| effect_text(e, nodes))
        .collect::<Result<_, _>>()?;
    let effects: Vec<String> = effects.into_iter().collect();
    let signature = format!(
        "fn {}({}) -> {}{}",
        f.name,
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
        match entry(f, badge, &mut nodes) {
            Ok(e) => index.entries.push(e),
            Err(reason) if reason == "search index type budget" => return Err(reason),
            Err(reason) => index.unsupported.push((f.name.clone(), reason)),
        }
    }
    index.entries.sort_by(|a, b| a.name.cmp(&b.name));
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

/// Whether the pattern matches, and whether it did so without a wildcard.
fn type_match(p: &TypePattern, t: &IndexType) -> Option<bool> {
    match (p, t) {
        (TypePattern::Any, _) => Some(false),
        (TypePattern::Named { name, args }, IndexType::Named { name: n, args: a })
            if name == n && args.len() == a.len() =>
        {
            args.iter()
                .zip(a)
                .try_fold(true, |exact, (p, t)| Some(exact & type_match(p, t)?))
        }
        (TypePattern::Tuple { items }, IndexType::Tuple { items: i }) if items.len() == i.len() => {
            items
                .iter()
                .zip(i)
                .try_fold(true, |exact, (p, t)| Some(exact & type_match(p, t)?))
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
    pub signature: String,
    pub score: u32,
    pub exact: bool,
    pub badge: BadgeLevel,
}

/// Score: 10 for an exact contract match (no wildcard, every pass and mode
/// given and equal, equal footprints and equal effects), plus 5 for a proved
/// and 2 for an audited badge. Ties sort by name.
pub fn search(query: &TextQuery, index: &SearchIndex) -> Vec<TextHit> {
    let mut hits = Vec::new();
    for e in &index.entries {
        let exact = match query {
            TextQuery::Name(word) => {
                if !e.name.to_lowercase().contains(&word.to_lowercase()) {
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
                let Some(mut exact) = type_match(result, &e.result) else {
                    continue;
                };
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
                let mut ok = true;
                for (q, f) in params.iter().zip(&e.params) {
                    let Some(ty_exact) = type_match(&q.ty, &f.ty) else {
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
                exact
            }
        };
        let score = u32::from(exact) * 10
            + match e.badge {
                BadgeLevel::Proved => 5,
                BadgeLevel::Audited => 2,
                BadgeLevel::Unverified => 0,
            };
        hits.push(TextHit {
            name: e.name.clone(),
            signature: e.signature.clone(),
            score,
            exact,
            badge: e.badge,
        });
    }
    hits.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.name.cmp(&b.name)));
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
