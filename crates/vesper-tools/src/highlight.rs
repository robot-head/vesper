//! `vesper highlight` (design §43): Chroma semantic highlighting.
//!
//! The library crate `vesper-highlight` never depends on this package, so
//! Tier 1 facts are gathered here, read-only, from the parser's AST and the
//! checker's existing outputs (`World::types`, `World::mode`,
//! `World::effects`, `World::functions`) and handed over as a
//! [`SemanticFacts`] DTO. A snippet that does not parse or check is
//! highlighted at Tier 0 with a typed [`FallbackReason`].
use std::{collections::BTreeMap, fs, ops::Range};
use vesper::{
    ast::{
        Expr, ExprKind, Function, Item, Mode, Module, Pass, Pattern, PatternKind, Statement, Type,
        substitute,
    },
    check::World,
    tooling::frontend::{AnalysisFailure, AnalysisLimits, check_source},
};
use vesper_highlight::{
    ChromaPalette, EffectFact, FallbackReason, FieldLeafFact, Highlight, HoverEntry,
    LinearBindingFact, SemanticFacts, ansi, css, highlight_tier1, html, json, tier0_fallback,
};

const HELP: &str = "Usage: vesper highlight [--html|--ansi|--json] [--theme dusk|dawn] [--tier 0|1] [--no-line-numbers] [-o PATH] FILE\n       vesper highlight --css [-o PATH]\n\nChroma semantic highlighting (design §43). Tier 0 classifies the lossless CST of any snippet,\ncomplete or not. Tier 1 (the default) also marks live linear bindings, mutated versus read-only\nFieldTree leaves and linear versus abortable effects when the snippet checks; otherwise it falls\nback to Tier 0 and reports why (in --json, and on stderr).\n\n  --html   Semantic HTML styled by vesper-chroma.css custom properties (copy-exact text)\n  --ansi   24-bit terminal colours (default)\n  --json   The 16-byte ChromaTokenSpanWire records (schema vesper.chroma.spans/1)\n  --css    The vesper-chroma.css sheet with both themes\n";

/// Largest input file (the highlighter's default byte budget).
const MAX_BYTES: usize = vesper_highlight::DEFAULT_MAX_SOURCE_BYTES;
/// Tier 1 analysis budget: the guarded front end's hard maxima.
const ANALYSIS_LIMITS: AnalysisLimits = AnalysisLimits {
    max_source_bytes: 262_144,
    max_ast_nodes: 8192,
    max_type_nodes: 32768,
    max_depth: 64,
    max_identifier_bytes: 1024,
    max_printed_type_bytes: 8192,
    max_diagnostics: 256,
    max_diagnostic_bytes: 262_144,
    max_metadata_bytes: 2_097_152,
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Format {
    Html,
    Ansi,
    Json,
    Css,
}

pub fn run(args: Vec<String>) -> Result<(), String> {
    let mut format = None;
    let mut theme = "dusk";
    let mut tier = 1u8;
    let mut line_numbers = true;
    let mut output = None;
    let mut input = None;
    let mut it = args.into_iter();
    let set = |f: &mut Option<Format>, v: Format| {
        if f.replace(v).is_some_and(|old| old != v) {
            Err("choose one of --html, --ansi, --json, --css".to_string())
        } else {
            Ok(())
        }
    };
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{HELP}");
                return Ok(());
            }
            "--html" => set(&mut format, Format::Html)?,
            "--ansi" => set(&mut format, Format::Ansi)?,
            "--json" => set(&mut format, Format::Json)?,
            "--css" => set(&mut format, Format::Css)?,
            "--no-line-numbers" => line_numbers = false,
            "--theme" => {
                theme = match it.next().as_deref() {
                    Some("dusk" | "vesper-dusk") => "dusk",
                    Some("dawn" | "vesper-dawn") => "dawn",
                    _ => return Err("--theme takes dusk or dawn".into()),
                }
            }
            "--tier" => {
                tier = match it.next().as_deref() {
                    Some("0") => 0,
                    Some("1") => 1,
                    _ => return Err("--tier takes 0 or 1".into()),
                }
            }
            "-o" => output = Some(it.next().ok_or("-o takes a path")?),
            a if a.starts_with('-') => return Err(format!("unknown highlight option {a}")),
            _ if input.is_some() => return Err("highlight takes one FILE".into()),
            _ => input = Some(arg),
        }
    }
    let format = format.unwrap_or(Format::Ansi);
    if format == Format::Css {
        if input.is_some() {
            return Err("--css takes no FILE".into());
        }
        return write(output, css::stylesheet().as_bytes());
    }
    let input = input.ok_or("missing source file; see vesper highlight --help")?;
    let meta = fs::metadata(&input).map_err(|e| format!("{input}: {e}"))?;
    if meta.len() > MAX_BYTES as u64 {
        return Err(format!(
            "{input}: highlight input exceeds {MAX_BYTES} bytes"
        ));
    }
    let bytes = fs::read(&input).map_err(|e| format!("{input}: {e}"))?;
    let source = String::from_utf8(bytes).map_err(|_| format!("{input}: not UTF-8"))?;
    let highlight = highlight_source(&source, tier).map_err(|e| format!("{input}: {e}"))?;
    if let Some(reason) = &highlight.fallback
        && tier == 1
    {
        eprintln!(
            "vesper highlight: Tier 1 unavailable ({}), using Tier 0",
            describe(reason)
        );
    }
    let palette = ChromaPalette::by_name(theme).ok_or("unknown theme")?;
    let text = match format {
        Format::Html => html::render(
            &source,
            &highlight,
            &html::HtmlOptions {
                theme: Some(theme),
                line_numbers,
            },
        ),
        Format::Ansi => ansi::render(&source, &highlight, &palette),
        Format::Json => {
            json::render(&highlight, palette.name.as_str()).map_err(|e| e.to_string())?
        }
        Format::Css => unreachable!("handled above"),
    };
    write(output, text.as_bytes())
}

fn describe(reason: &FallbackReason) -> String {
    match reason {
        FallbackReason::Syntax { code, message, .. } => format!("syntax error {code}: {message}"),
        FallbackReason::Check { code, message, .. } => format!("check error {code}: {message}"),
        FallbackReason::Budget { message } => format!("analysis budget exceeded: {message}"),
        FallbackReason::NotRequested => "not requested".into(),
    }
}

fn write(output: Option<String>, bytes: &[u8]) -> Result<(), String> {
    match output {
        Some(path) => fs::write(&path, bytes).map_err(|e| format!("{path}: {e}")),
        None => {
            use std::io::Write;
            std::io::stdout()
                .write_all(bytes)
                .map_err(|e| e.to_string())
        }
    }
}

/// Tier 1 when requested and the snippet checks, else Tier 0 with a reason.
pub fn highlight_source(
    source: &str,
    tier: u8,
) -> Result<Highlight, vesper_highlight::ChromaError> {
    if tier == 0 {
        return tier0_fallback(source, FallbackReason::NotRequested);
    }
    match facts(source) {
        Ok(facts) => highlight_tier1(source, &facts),
        Err(reason) => tier0_fallback(source, reason),
    }
}

/// Runs the parser and checker and extracts the Tier 1 facts.
pub fn facts(source: &str) -> Result<SemanticFacts, FallbackReason> {
    if source.len() > MAX_BYTES {
        return Err(FallbackReason::Syntax {
            code: "E0003".into(),
            message: "source exceeds the highlighter budget".into(),
            span: 0..0,
        });
    }
    // The guarded front end bounds AST size, nesting and type growth, so a
    // small input such as repeated `let x = (x, x);` cannot exhaust memory.
    let checked = match check_source(source, ANALYSIS_LIMITS) {
        Ok(c) => c,
        Err(AnalysisFailure::Parse { diagnostics }) => {
            return Err(diagnostics.first().map_or_else(
                || FallbackReason::Budget {
                    message: "parse failed".into(),
                },
                |d| FallbackReason::Syntax {
                    code: d.code.as_str().into(),
                    message: d.message.as_str().into(),
                    span: d.span.clone(),
                },
            ));
        }
        Err(AnalysisFailure::Semantic { diagnostics }) => {
            return Err(diagnostics.first().map_or_else(
                || FallbackReason::Budget {
                    message: "check failed".into(),
                },
                |d| FallbackReason::Check {
                    code: d.code.as_str().into(),
                    message: d.message.as_str().into(),
                    span: d.span.clone(),
                },
            ));
        }
        Err(AnalysisFailure::Resource { reason } | AnalysisFailure::Unsupported { reason }) => {
            return Err(FallbackReason::Budget {
                message: reason.as_str().into(),
            });
        }
    };
    let parsed = checked.parsed();
    let mut collector = Collector {
        src: source,
        world: checked.world(),
        facts: SemanticFacts::default(),
        done: vec![],
    };
    collector.module(&parsed.module);
    let mut facts = collector.facts;
    facts.effects = checked
        .world()
        .effects
        .values()
        .map(|e| EffectFact {
            name: e.name.as_str().into(),
            linear: e.linear,
        })
        .collect();
    Ok(facts)
}

struct Binding {
    name: String,
    mint: Range<usize>,
    linear: bool,
    occurrences: Vec<Range<usize>>,
    hover: Option<u32>,
}

struct Collector<'a> {
    src: &'a str,
    world: &'a World,
    facts: SemanticFacts,
    /// Bindings of the current function whose lexical scope has ended.
    done: Vec<Binding>,
}

fn mode_name(mode: Mode) -> &'static str {
    match mode {
        Mode::Copy => "copy",
        Mode::Affine => "affine",
        Mode::Linear => "linear",
    }
}

fn pass_name(pass: Pass) -> &'static str {
    match pass {
        Pass::In => "in",
        Pass::Inout => "inout",
        Pass::Sink => "sink",
    }
}

/// The first whole-word occurrence of `word` in `src[range]`.
fn find_word(src: &str, range: &Range<usize>, word: &str) -> Option<Range<usize>> {
    let hay = src.get(range.clone())?;
    let is_ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut from = 0;
    while let Some(i) = hay[from..].find(word) {
        let start = from + i;
        let end = start + word.len();
        let before = start == 0 || !is_ident(hay.as_bytes()[start - 1]);
        let after = end == hay.len() || !is_ident(hay.as_bytes()[end]);
        if before && after {
            return Some(range.start + start..range.start + end);
        }
        from = start + 1;
    }
    None
}

impl<'a> Collector<'a> {
    fn hover(&mut self, span: Range<usize>, text: String) -> u32 {
        let id = self.facts.hovers.len() as u32;
        self.facts.hovers.push(HoverEntry {
            span,
            text: text.into(),
        });
        id
    }

    fn module(&mut self, module: &Module) {
        for item in &module.items {
            match item {
                Item::Function(f) => self.function(f, None),
                Item::Impl { ty, methods, .. } => {
                    methods.iter().for_each(|f| self.function(f, Some(ty)))
                }
                Item::Conformance(c) => c
                    .methods
                    .iter()
                    .for_each(|f| self.function(f, Some(&c.self_ty))),
                _ => {}
            }
        }
    }

    fn function(&mut self, f: &Function, self_ty: Option<&Type>) {
        let bounds: BTreeMap<String, Mode> = f
            .generics
            .iter()
            .map(|g| (g.name.clone(), g.bound))
            .collect();
        let mut bindings = vec![];
        for p in &f.params {
            let ty = match self_ty {
                Some(t) if p.name == "self" && p.ty == Type::Infer => t,
                _ => &p.ty,
            };
            let mode = self.world.mode(ty, &bounds);
            let word = if p.name == "self" {
                "self"
            } else {
                p.name.as_str()
            };
            // Skip the pass keyword before searching for the name.
            let search = p.span.start + pass_name(p.pass).len().min(p.span.len())..p.span.end;
            let Some(mint) =
                find_word(self.src, &search, word).or_else(|| find_word(self.src, &p.span, word))
            else {
                continue;
            };
            let hover = self.hover(
                mint.clone(),
                format!(
                    "{} {}: {ty} (mod {})",
                    pass_name(p.pass),
                    p.name,
                    mode_name(mode)
                ),
            );
            bindings.push(Binding {
                name: p.name.clone(),
                mint,
                linear: p.pass == Pass::Sink && mode == Mode::Linear,
                occurrences: vec![],
                hover: Some(hover),
            });
        }
        if let Some(body) = &f.body {
            self.expr(body, &bounds, &mut bindings, false);
        }
        let mut all = std::mem::take(&mut self.done);
        all.extend(bindings);
        all.sort_by_key(|b| b.mint.start);
        for b in all.into_iter().filter(|b| b.linear) {
            let discharge = b.occurrences.last().cloned();
            self.facts.linear_bindings.push(LinearBindingFact {
                name: b.name.as_str().into(),
                mint: b.mint,
                occurrences: b.occurrences,
                discharge,
                hover: b.hover,
            });
        }
    }

    fn bind(
        &mut self,
        name: &str,
        span: &Range<usize>,
        ty: &Type,
        bounds: &BTreeMap<String, Mode>,
        bindings: &mut Vec<Binding>,
    ) {
        let Some(mint) = find_word(self.src, span, name) else {
            return;
        };
        let mode = self.world.mode(ty, bounds);
        // A binding whose type the checker did not record still shadows
        // outer names, but gets no hover row and is never linear.
        let known = *ty != Type::Infer;
        let hover = known.then(|| {
            self.hover(
                mint.clone(),
                format!("{name} : {ty} (mod {})", mode_name(mode)),
            )
        });
        bindings.push(Binding {
            name: name.into(),
            mint,
            linear: known && mode == Mode::Linear,
            occurrences: vec![],
            hover,
        });
    }

    /// Binds every name a pattern introduces, with its component type, as
    /// the checker's `bind_mode` does (tuples by position, constructor
    /// fields by name with the ADT's generics substituted).
    fn bind_pattern(
        &mut self,
        p: &Pattern,
        ty: &Type,
        bounds: &BTreeMap<String, Mode>,
        bindings: &mut Vec<Binding>,
    ) {
        match &p.kind {
            PatternKind::Bind(name) => self.bind(name, &p.span, ty, bounds, bindings),
            PatternKind::Wild | PatternKind::Literal(_) => {}
            PatternKind::Tuple(ps) => {
                for (i, q) in ps.iter().enumerate() {
                    let t = match ty {
                        Type::Tuple(ts) => ts.get(i).cloned().unwrap_or(Type::Infer),
                        _ => Type::Infer,
                    };
                    self.bind_pattern(q, &t, bounds, bindings);
                }
            }
            PatternKind::Constructor { name, fields, .. } => {
                let found = self.world.constructor(name);
                let vars: BTreeMap<String, Type> = match (found, ty) {
                    (Some((adt, _)), Type::Named(n, args)) if *n == adt.name => adt
                        .generics
                        .iter()
                        .zip(args)
                        .map(|(g, t)| (g.name.clone(), t.clone()))
                        .collect(),
                    _ => BTreeMap::new(),
                };
                for (field, q) in fields {
                    let t = found
                        .and_then(|(_, v)| v.fields.iter().find(|f| &f.name == field))
                        .map(|f| substitute(&f.ty, &vars))
                        .unwrap_or(Type::Infer);
                    self.bind_pattern(q, &t, bounds, bindings);
                }
            }
        }
    }

    /// Ends a lexical scope opened when `bindings` had `mark` entries.
    fn close(&mut self, bindings: &mut Vec<Binding>, mark: usize) {
        self.done.extend(bindings.drain(mark..));
    }

    fn expr_type(&self, e: &Expr) -> Type {
        self.world
            .types
            .get(&(e.span.start, e.span.end))
            .cloned()
            .unwrap_or(Type::Infer)
    }

    /// Field-access chain rooted at a name: marks each field identifier as a
    /// leaf, mutated when `write`.
    fn leaves(&mut self, e: &Expr, write: bool) -> bool {
        match &e.kind {
            ExprKind::Field(base, name) => {
                if !self.leaves(base, write) {
                    return false;
                }
                let start = e.span.end.saturating_sub(name.len());
                if self.src.get(start..e.span.end) == Some(name.as_str()) {
                    self.facts.field_leaves.push(FieldLeafFact {
                        span: start..e.span.end,
                        mutated: write,
                    });
                }
                true
            }
            ExprKind::Name(_) => true,
            _ => false,
        }
    }

    /// Whether the method the checker resolves for `receiver.method(..)`
    /// takes its receiver `inout` or `sink`: `Type::method` for the
    /// receiver's recorded type. Unresolved calls count as reads.
    /// The function a call resolves to, and the index of the parameter its
    /// first argument binds (1 for a method call, whose receiver is param 0).
    fn callee(&self, callee: &Expr) -> Option<(&'a Function, usize)> {
        match &callee.kind {
            ExprKind::Name(n) => self.world.functions.get(n).map(|f| (f, 0)),
            ExprKind::Field(receiver, method) => {
                let Type::Named(type_name, _) = self.expr_type(receiver) else {
                    return None;
                };
                self.world
                    .functions
                    .get(&format!("{type_name}::{method}"))
                    .map(|f| (f, 1))
            }
            _ => None,
        }
    }

    fn expr(
        &mut self,
        e: &Expr,
        bounds: &BTreeMap<String, Mode>,
        bindings: &mut Vec<Binding>,
        write: bool,
    ) {
        match &e.kind {
            ExprKind::Name(n) => {
                if let Some(b) = bindings
                    .iter_mut()
                    .rev()
                    .find(|b| &b.name == n && b.mint.start <= e.span.start)
                    && self.src.get(e.span.clone()) == Some(n.as_str())
                {
                    b.occurrences.push(e.span.clone());
                }
            }
            ExprKind::Field(base, _) => {
                self.leaves(e, write);
                let mut root = base.as_ref();
                while let ExprKind::Field(b, _) = &root.kind {
                    root = b;
                }
                // The root name occurrence, or a computed base such as `f().x`.
                self.expr(root, bounds, bindings, false);
            }
            ExprKind::Literal(_) => {}
            ExprKind::Tuple(items) => {
                for i in items {
                    self.expr(i, bounds, bindings, false);
                }
            }
            ExprKind::Struct { fields, .. } => {
                for (_, v) in fields {
                    self.expr(v, bounds, bindings, false);
                }
            }
            ExprKind::Block { statements, tail } => {
                let mark = bindings.len();
                for s in statements {
                    match s {
                        Statement::Let { pat, ty, value, .. } => {
                            self.expr(value, bounds, bindings, false);
                            let ty = if *ty != Type::Infer {
                                ty.clone()
                            } else {
                                self.expr_type(value)
                            };
                            self.bind_pattern(pat, &ty, bounds, bindings);
                        }
                        Statement::Expr(x) => self.expr(x, bounds, bindings, false),
                    }
                }
                if let Some(t) = tail {
                    self.expr(t, bounds, bindings, false);
                }
                self.close(bindings, mark);
            }
            ExprKind::Call { callee, args, .. } => {
                let resolved = self.callee(callee);
                if let ExprKind::Field(receiver, _) = &callee.kind {
                    let writes = resolved
                        .and_then(|(f, _)| f.params.first())
                        .is_some_and(|p| p.pass != Pass::In);
                    self.expr(receiver, bounds, bindings, writes);
                } else {
                    self.expr(callee, bounds, bindings, false);
                }
                for (i, a) in args.iter().enumerate() {
                    // An argument without a mode keyword takes its parameter's mode.
                    let pass = if a.explicit {
                        Some(a.pass)
                    } else {
                        resolved
                            .and_then(|(f, off)| f.params.get(off + i))
                            .map(|p| p.pass)
                    };
                    let w = matches!(pass, Some(Pass::Inout | Pass::Sink));
                    self.expr(&a.expr, bounds, bindings, w);
                }
            }
            ExprKind::Index(a, b) => {
                self.expr(a, bounds, bindings, write);
                self.expr(b, bounds, bindings, false);
            }
            ExprKind::Try(x) | ExprKind::Unary(_, x) => self.expr(x, bounds, bindings, false),
            ExprKind::Binary(_, a, b) => {
                self.expr(a, bounds, bindings, false);
                self.expr(b, bounds, bindings, false);
            }
            ExprKind::Assign(_, lhs, rhs) => {
                self.expr(rhs, bounds, bindings, false);
                self.expr(lhs, bounds, bindings, true);
            }
            ExprKind::If {
                condition,
                then,
                otherwise,
            } => {
                self.expr(condition, bounds, bindings, false);
                self.expr(then, bounds, bindings, false);
                if let Some(o) = otherwise {
                    self.expr(o, bounds, bindings, false);
                }
            }
            ExprKind::Match { value, arms } => {
                self.expr(value, bounds, bindings, false);
                let ty = self.expr_type(value);
                for (pat, arm) in arms {
                    let mark = bindings.len();
                    self.bind_pattern(pat, &ty, bounds, bindings);
                    self.expr(arm, bounds, bindings, false);
                    self.close(bindings, mark);
                }
            }
            ExprKind::For { pat, iter, body } => {
                self.expr(iter, bounds, bindings, false);
                let ty = match &iter.kind {
                    ExprKind::Range { start, .. } => self.expr_type(start),
                    _ => Type::Infer,
                };
                let mark = bindings.len();
                self.bind_pattern(pat, &ty, bounds, bindings);
                self.expr(body, bounds, bindings, false);
                self.close(bindings, mark);
            }
            k @ (ExprKind::Loop(_)
            | ExprKind::Range { .. }
            | ExprKind::Break(_)
            | ExprKind::Continue
            | ExprKind::Return(_)) => {
                for c in k.control_children().into_iter().flatten() {
                    self.expr(c, bounds, bindings, false);
                }
            }
            ExprKind::Closure { params, body, .. } => {
                let mark = bindings.len();
                for p in params {
                    self.bind(&p.name, &p.span, &p.ty, bounds, bindings);
                    // Only an owned (`sink`) parameter carries an obligation.
                    if p.pass != Pass::Sink
                        && let Some(b) = bindings.last_mut()
                    {
                        b.linear = false;
                    }
                }
                self.expr(body, bounds, bindings, false);
                self.close(bindings, mark);
            }
            ExprKind::Handle {
                state,
                body,
                ret,
                ops,
                ..
            } => {
                let outer = bindings.len();
                if let Some((pat, init)) = state {
                    self.expr(init, bounds, bindings, false);
                    let ty = self.expr_type(init);
                    self.bind_pattern(pat, &ty, bounds, bindings);
                }
                self.expr(body, bounds, bindings, false);
                for clause in std::iter::once(ret.as_ref()).chain(ops) {
                    let mark = bindings.len();
                    for pat in &clause.pats {
                        // Clause parameter types are not in the checker's
                        // expression map: shadow only.
                        self.bind_pattern(pat, &Type::Infer, bounds, bindings);
                    }
                    self.expr(&clause.body, bounds, bindings, false);
                    self.close(bindings, mark);
                }
                self.close(bindings, outer);
            }
        }
    }
}
