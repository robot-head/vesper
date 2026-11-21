//! Resolve sugar imports over the package graph before parsing executable bodies.
mod forge;
use super::*;
use crate::parser;
use std::sync::Arc;
use vesper_syntax::{
    lexer::TokenKind as K,
    sugar_expand::{ExpandedSugar, expand_sugar_with_scopes_for_execution},
};

#[derive(Default)]
pub(super) struct ModuleSugar {
    pub fresh_prefix: String,
    /// Functions validated inside Forge's closed authority graph.
    pub audited: Option<BTreeSet<String>>,
    pub declarations: BTreeMap<String, Declaration>,
    surface: bool,
    tests: bool,
    imports: Vec<Span>,
    /// For each expanded region, the module and original declaration span.
    pub origins: Vec<(ModuleKey, Span)>,
    /// Original text, kept separately when executable bytes change.
    pub original: Option<usize>,
}

#[derive(Clone)]
pub(super) struct Declaration {
    pub public: bool,
    text: Arc<str>,
    span: Span,
}

impl ModuleSugar {
    pub fn discover(text: &str, core: &mut parser::Parse, tests: bool) -> Self {
        let mut result = Self {
            tests,
            ..Self::default()
        };
        if core.diagnostics.is_empty() {
            return result;
        }
        let Ok(surface) = parser::parse_surface_with_limits(
            text,
            parser::ParseLimits {
                max_source_bytes: crate::lexer::HARD_SOURCE_BYTES,
                max_tokens: crate::lexer::HARD_TOKENS,
                ..Default::default()
            },
        ) else {
            return result;
        };
        if !surface.diagnostics.is_empty() {
            return result;
        }
        let tree = surface.syntax();
        result.surface = tree.descendants().any(|n| parser::is_sugar_node(n.kind()));
        if !result.surface {
            return result;
        }
        // Recovery of a core AST may have missed imports following a declaration
        // it did not recognize. Read the complete import items from the surface CST.
        core.module.uses.clear();
        for item in tree.descendants().filter(|n| n.kind() == parser::ITEM) {
            let top = item.parent().is_some_and(|p| p.kind() == parser::MODULE);
            if !top && !tests {
                continue;
            }
            let decl = item.children().find(|n| n.kind() == parser::SUGAR_DECL);
            if let Some(decl) = decl {
                let span =
                    usize::from(decl.text_range().start())..usize::from(decl.text_range().end());
                let mut public = false;
                let mut words = decl
                    .descendants_with_tokens()
                    .filter_map(|e| e.into_token())
                    .filter(|t| is_significant(t.kind().0));
                for token in words.by_ref() {
                    if token.text() == "sugar" {
                        break;
                    }
                    public |= token.text() == "pub";
                }
                if let Some(name) = words.next() {
                    result.declarations.insert(
                        name.text().to_owned(),
                        Declaration {
                            public,
                            text: Arc::from(&text[span.clone()]),
                            span,
                        },
                    );
                }
                continue;
            }
            let mut words = item
                .descendants_with_tokens()
                .filter_map(|e| e.into_token())
                .filter(|t| is_significant(t.kind().0));
            let first = words.next();
            let head = if first.as_ref().is_some_and(|t| t.text() == "pub") {
                words.next()
            } else {
                first
            };
            if !head.is_some_and(|t| t.text() == "use") {
                continue;
            }
            let start: usize = item.text_range().start().into();
            let end: usize = item.text_range().end().into();
            result.imports.push(start..end);
            for mut import in parser::parse(&text[start..end]).module.uses {
                import.span = start + import.span.start..start + import.span.end;
                for n in &mut import.names {
                    n.span = start + n.span.start..start + n.span.end;
                }
                core.module.uses.push(import);
            }
        }
        result
    }
}

fn is_significant(kind: u16) -> bool {
    ![K::Whitespace, K::Comment, K::BlockComment]
        .iter()
        .any(|k| *k as u16 == kind)
}

struct Imported {
    alias: String,
    module: ModuleKey,
    decl: Declaration,
}

#[derive(Default)]
struct Plan {
    imports: Vec<Imported>,
    /// Each declaration shares its defining module's lexical environment.
    scopes: BTreeMap<ModuleKey, Vec<(String, usize)>>,
    visible: Vec<usize>,
}

#[derive(Default)]
struct SugarGraph {
    decls: BTreeMap<ItemKey, Declaration>,
    uses: BTreeMap<ModuleKey, BTreeMap<String, ItemKey>>,
    names: BTreeMap<ModuleKey, BTreeMap<String, ItemKey>>,
}

impl SugarGraph {
    fn new(packages: &[Package], root: usize) -> Self {
        let mut graph = Self::default();
        let mut resolver = Resolver::new(packages, root);
        resolver.reexports();
        for (p, package) in packages.iter().enumerate() {
            for m in &package.modules {
                let module = (p, m.path.clone());
                let mut imported = BTreeMap::new();
                for u in &m.parsed.uses {
                    let Ok((tp, path, shown)) = resolver.use_module(p, &m.path, u) else {
                        continue;
                    };
                    for n in &u.names {
                        if let Ok((key, Kind::Sugar)) = resolver.lookup(
                            tp,
                            &path,
                            &n.name,
                            tp == p && path == m.path,
                            &shown,
                            n.span.clone(),
                        ) {
                            imported.insert(n.local().to_owned(), key);
                        }
                    }
                }
                let mut names = imported.clone();
                for (name, decl) in &m.sugar.declarations {
                    let key = (p, m.path.clone(), name.clone());
                    names.insert(name.clone(), key.clone());
                    graph.decls.insert(key, decl.clone());
                }
                graph.uses.insert(module.clone(), imported);
                graph.names.insert(module, names);
            }
        }
        graph
    }

    /// Follow private dependencies in the rule author's namespace, keeping
    /// only the caller's explicit imports visible at the original call site.
    fn plan(&self, caller: &ModuleKey) -> Plan {
        let mut plan = Plan::default();
        let mut bound = BTreeMap::new();
        let mut bind = |imports: &mut Vec<Imported>, alias: &String, key: &ItemKey| {
            *bound
                .entry((alias.clone(), key.clone()))
                .or_insert_with(|| {
                    let index = imports.len();
                    imports.push(Imported {
                        alias: alias.clone(),
                        module: (key.0, key.1.clone()),
                        decl: self.decls[key].clone(),
                    });
                    index
                })
        };
        for (alias, key) in &self.uses[caller] {
            plan.visible.push(bind(&mut plan.imports, alias, key));
        }
        let mut next = 0;
        while let Some(import) = plan.imports.get(next) {
            let module = import.module.clone();
            next += 1;
            plan.scopes.entry(module.clone()).or_insert_with(|| {
                self.names[&module]
                    .iter()
                    .map(|(name, key)| (name.clone(), bind(&mut plan.imports, name, key)))
                    .collect()
            });
        }
        plan
    }
}

impl Plan {
    fn expand(
        &self,
        source: &str,
        pkg: usize,
        m: &mut ModuleSource,
    ) -> Result<(String, parser::Parse), Diagnostic> {
        let imports = &self.imports;
        let scopes: Vec<_> = imports
            .iter()
            .map(|i| self.scopes[&i.module].as_slice())
            .collect();
        let bindings: Vec<_> = imports
            .iter()
            .map(|i| (i.alias.as_str(), i.decl.text.as_ref()))
            .collect();
        let Some(mut expanded) = expand_sugar_with_scopes_for_execution(
            source,
            &bindings,
            &scopes,
            &self.visible,
            m.sugar.tests,
        )?
        else {
            return Err(Diagnostic::new(
                "E0142",
                "could not resolve source sugar",
                0..0,
            ));
        };
        m.sugar.fresh_prefix = expanded.fresh_prefix.clone();
        // Save definition coordinates before a mixed macro can project
        // regions. Map by definition and call span rather than index.
        let origin_map: Vec<_> = expanded
            .regions
            .iter()
            .zip(&expanded.import_origins)
            .map(|(r, idx)| {
                let (key, span) = if let Some(i) = idx {
                    let origin = &imports[*i];
                    (
                        origin.module.clone(),
                        origin.decl.span.start + r.rule.start..origin.decl.span.start + r.rule.end,
                    )
                } else {
                    ((pkg, m.path.clone()), r.rule.clone())
                };
                ((r.call.clone(), r.rule.clone()), (key, span))
            })
            .collect();
        let mixed_imports = mask_macro_imports(&mut expanded, &m.sugar.imports);
        let (text, mut parsed) = if mixed_imports {
            forge::parse(expanded, m.sugar.tests, &mut m.sugar.audited)?
        } else {
            crate::forge_frontend::parse_expanded_sugar(expanded, m.sugar.tests)?
        };
        if mixed_imports {
            parsed.module.uses = m.parsed.uses.clone();
        }
        let mut origins = origin_map.into_iter();
        for r in &parsed.module.sugar_origins {
            // Macro projection may discard regions. The surviving
            // origins retain their source order.
            let (_, origin) = origins
                .find(|((call, rule), _)| *call == r.call && *rule == r.rule)
                .ok_or_else(|| {
                    Diagnostic::new(
                        "E0141",
                        "cannot locate the definition of an expanded package sugar",
                        r.call.clone(),
                    )
                })?;
            m.sugar.origins.push(origin);
        }
        Ok((text, parsed))
    }
}

fn mask_macro_imports(expanded: &mut ExpandedSugar, imports: &[Span]) -> bool {
    // Forge accepts a closed module. Its parser need not see
    // package directives that the resolver has already handled.
    let mixed_imports = !imports.is_empty()
        && expanded.source.contains("macro")
        && crate::lexer::lex(&expanded.source)
            .0
            .iter()
            .any(|t| t.kind == K::Lower && &expanded.source[t.span.clone()] == "macro");
    if mixed_imports {
        for mapping in expanded.mappings.iter().filter(|m| m.copied) {
            let first = imports.partition_point(|span| span.end <= mapping.original.start);
            for import in imports[first..]
                .iter()
                .take_while(|span| span.start < mapping.original.end)
            {
                let lo = import.start.max(mapping.original.start);
                let hi = import.end.min(mapping.original.end);
                if lo >= hi {
                    continue;
                }
                let start = mapping.expanded.start + (lo - mapping.original.start);
                let span = start..start + (hi - lo);
                let blank: String = expanded.source[span.clone()]
                    .bytes()
                    .map(|b| if b == b'\n' { '\n' } else { ' ' })
                    .collect();
                expanded.source.replace_range(span, &blank);
            }
        }
    }
    mixed_imports
}

impl Loader<'_> {
    pub(super) fn expand_sugars(
        &mut self,
        root: usize,
        mut prepare: Option<&mut PreparedParser<'_>>,
    ) -> Result<(), String> {
        let graph = self
            .packages
            .iter()
            .flat_map(|p| &p.modules)
            .any(|m| m.sugar.surface)
            .then(|| SugarGraph::new(&self.packages, root));
        if graph.is_none() && prepare.is_none() {
            return Ok(());
        }
        let mut originals = Vec::new();
        for (pkg, package) in self.packages.iter_mut().enumerate() {
            for m in &mut package.modules {
                let file = &mut self.sources.files[m.file];
                if m.sugar.surface {
                    let plan = graph
                        .as_ref()
                        .expect("discovered sugar graph")
                        .plan(&(pkg, m.path.clone()));
                    self.diagnostics.retain(|d| d.file != Some(m.file));
                    let expansion = plan.expand(&file.text, pkg, m);
                    match expansion {
                        Ok((text, parsed)) => {
                            let old = std::mem::replace(&mut file.text, text);
                            originals.push((m.file, file.display.clone(), old));
                            m.parsed = parsed.module;
                            self.diagnostics.extend(parsed.diagnostics.into_iter().map(
                                |diagnostic| PackageDiagnostic {
                                    file: Some(m.file),
                                    diagnostic,
                                },
                            ));
                        }
                        Err(diagnostic) => {
                            self.diagnostics.push(PackageDiagnostic {
                                file: Some(m.file),
                                diagnostic,
                            });
                            continue;
                        }
                    }
                }
                if pkg == root
                    && let Some(prepare) = prepare.as_deref_mut()
                {
                    let mut prepared = prepare(&m.disk_path, &mut file.text)?;
                    if m.sugar.surface && prepared.module.uses.is_empty() {
                        prepared.module.uses = std::mem::take(&mut m.parsed.uses);
                    }
                    prepared.module.sugar_origins = std::mem::take(&mut m.parsed.sugar_origins);
                    m.parsed = prepared.module;
                    self.diagnostics.retain(|d| d.file != Some(m.file));
                    self.diagnostics
                        .extend(prepared.diagnostics.into_iter().map(|diagnostic| {
                            PackageDiagnostic {
                                file: Some(m.file),
                                diagnostic,
                            }
                        }));
                }
            }
        }
        // Source spans in the executable AST refer to generated bytes, while
        // call-site and rule labels refer to the user's original source bytes.
        for (original, display, text) in originals {
            let i = self.sources.files.len();
            self.sources.files.push(SourceFile {
                display,
                text,
                base: 0,
            });
            let module = self
                .packages
                .iter_mut()
                .flat_map(|p| &mut p.modules)
                .find(|m| m.file == original)
                .expect("sugar source");
            module.sugar.original = Some(i);
        }
        let mut base = 0;
        for file in &mut self.sources.files {
            file.base = base;
            base += file.text.len() + 1;
        }
        Ok(())
    }
}

impl Resolver<'_> {
    /// Look up free template names at the defining module, including its
    /// private items and its own import aliases.
    pub(super) fn sugar_name(&self, scope: &ModuleKey, name: &str, ns: Kind) -> Option<String> {
        if self.tables[scope]
            .items
            .get(name)
            .is_some_and(|kind| kind.has(ns))
        {
            return Some(self.name_of(&(scope.0, scope.1.clone(), name.to_owned())));
        }
        let module = self.packages[scope.0]
            .modules
            .iter()
            .find(|m| m.path == scope.1)?;
        for import in &module.parsed.uses {
            if let Some(n) = import.names.iter().find(|n| n.local() == name) {
                let (p, path, shown) = self.use_module(scope.0, &scope.1, import).ok()?;
                let (key, kind) = self
                    .lookup(
                        p,
                        &path,
                        &n.name,
                        p == scope.0 && path == scope.1,
                        &shown,
                        n.span.clone(),
                    )
                    .ok()?;
                return kind.has(ns).then(|| self.name_of(&key));
            }
        }
        None
    }
}
