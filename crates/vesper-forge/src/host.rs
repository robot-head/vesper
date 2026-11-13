//! The hygiene host: owner of every scope, expansion, splice and symbol.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

use crate::error::{ForgeError, IdentityKind, SpliceFault};
use crate::identity::{
    BinderId, Counter, ExpansionId, IdentityLimits, ScopeId, ScopeSet, SessionId, SpliceHandle,
    Symbol,
};
use crate::lex::lex_generated;
use crate::registry::{DefId, DefKind, DefinitionRegistry, PackageId};
use crate::token::{
    GeneratedToken, HostToken, Origin, Quotation, QuoteItem, QuoteLimits, TokenKind,
};

/// Where a macro was invoked.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum InvocationSite {
    /// Directly in host-lexed source of a package.
    Source(PackageId),
    /// Inside the (finished) output of another expansion.
    Expansion(ExpansionId),
}

/// Host record of one expansion; carried into resolution and audit output.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpansionRecord {
    pub id: ExpansionId,
    pub macro_def: DefId,
    pub macro_package: PackageId,
    pub site: InvocationSite,
    /// Package whose source contains the outermost invocation; its
    /// capability ceiling bounds the whole expansion chain.
    pub root_caller: PackageId,
    /// Fresh introduction scope for tokens this expansion generates.
    pub intro_scope: ScopeId,
}

/// What a caller of [`HygieneHost::begin_expansion`] receives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Expansion {
    pub id: ExpansionId,
    /// One handle per caller argument, in order.
    pub splices: Vec<SpliceHandle>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExpansionStatus {
    Open,
    Finished,
    Failed,
}

#[derive(Clone, Debug)]
struct ExpansionState {
    record: ExpansionRecord,
    arena: BTreeMap<u32, Vec<HostToken>>,
    status: ExpansionStatus,
    typed_alive: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}

#[derive(Clone, Debug)]
pub(crate) struct GlobalBinding {
    pub(crate) symbol: Symbol,
    pub(crate) scopes: ScopeSet,
    pub(crate) def: DefId,
}

/// Owner of all hygiene identities for one compilation session.
///
/// Deliberately not `Clone`: two copies would share a session and counter
/// positions and could mint the same identity with different meanings.
#[derive(Debug)]
pub struct HygieneHost {
    pub(crate) typed_owner: std::sync::Arc<()>,
    session: SessionId,
    pub(crate) registry: DefinitionRegistry,
    pub(crate) quote_limits: QuoteLimits,
    scopes: Counter,
    expansions: Counter,
    splices: Counter,
    binders: Counter,
    symbol_limit: u32,
    symbols: Vec<String>,
    symbol_index: HashMap<String, Symbol>,
    module_scopes: BTreeMap<PackageId, ScopeId>,
    pub(crate) globals: Vec<GlobalBinding>,
    expansion_state: BTreeMap<u32, ExpansionState>,
}

impl HygieneHost {
    pub fn new(session: SessionId, registry: DefinitionRegistry) -> Self {
        Self::with_limits(
            session,
            registry,
            IdentityLimits::default(),
            QuoteLimits::default(),
        )
    }

    pub fn with_limits(
        session: SessionId,
        registry: DefinitionRegistry,
        ids: IdentityLimits,
        quote_limits: QuoteLimits,
    ) -> Self {
        Self {
            typed_owner: std::sync::Arc::new(()),
            session,
            registry,
            quote_limits,
            scopes: Counter::new(IdentityKind::Scope, ids.max_scopes),
            expansions: Counter::new(IdentityKind::Expansion, ids.max_expansions),
            splices: Counter::new(IdentityKind::Splice, ids.max_splices),
            binders: Counter::new(IdentityKind::Binder, ids.max_binders),
            symbol_limit: ids.max_symbols,
            symbols: Vec::new(),
            symbol_index: HashMap::new(),
            module_scopes: BTreeMap::new(),
            globals: Vec::new(),
            expansion_state: BTreeMap::new(),
        }
    }

    pub fn session(&self) -> SessionId {
        self.session
    }

    pub fn registry(&self) -> &DefinitionRegistry {
        &self.registry
    }

    // ----- identities ---------------------------------------------------

    /// Mints a fresh scope that has never been returned before.
    pub fn fresh_scope(&mut self) -> Result<ScopeId, ForgeError> {
        Ok(ScopeId {
            session: self.session,
            index: self.scopes.mint()?,
        })
    }

    pub(crate) fn fresh_binder(&mut self) -> Result<BinderId, ForgeError> {
        Ok(BinderId(self.binders.mint()?))
    }

    /// Interns `text`.
    pub fn intern(&mut self, text: &str) -> Result<Symbol, ForgeError> {
        if let Some(&s) = self.symbol_index.get(text) {
            return Ok(s);
        }
        let n = u32::try_from(self.symbols.len()).unwrap_or(u32::MAX);
        if n >= self.symbol_limit {
            return Err(ForgeError::IdentityExhausted {
                kind: IdentityKind::Symbol,
                limit: self.symbol_limit,
            });
        }
        let s = Symbol(n);
        self.symbols.push(text.to_owned());
        self.symbol_index.insert(text.to_owned(), s);
        Ok(s)
    }

    /// Text of an interned symbol. Unknown symbols (forged by
    /// deserialization) print as `<?sym N>`; [`Self::validate_token`]
    /// rejects them before resolution.
    pub fn text(&self, s: Symbol) -> &str {
        self.symbols
            .get(s.0 as usize)
            .map(String::as_str)
            .unwrap_or("<?sym>")
    }

    /// The definition-site scope of `package`'s top-level environment.
    pub fn module_scope(&mut self, package: PackageId) -> Result<ScopeId, ForgeError> {
        if self.registry.package(package).is_none() {
            return Err(ForgeError::UnknownPackage(package));
        }
        if let Some(&s) = self.module_scopes.get(&package) {
            return Ok(s);
        }
        let s = self.fresh_scope()?;
        self.module_scopes.insert(package, s);
        Ok(s)
    }

    /// Checks that a scope was minted by this host.
    pub fn validate_scope(&self, s: ScopeId) -> Result<(), ForgeError> {
        if s.session != self.session {
            return Err(ForgeError::ForeignScope(s));
        }
        if !self.scopes.minted(s.index) {
            return Err(ForgeError::ForgedScope(s));
        }
        Ok(())
    }

    /// Checks every host-owned identity carried by a token.
    pub fn validate_token(&self, t: &HostToken) -> Result<(), ForgeError> {
        for s in t.scopes.iter() {
            self.validate_scope(s)?;
        }
        if t.text.0 as usize >= self.symbols.len() {
            return Err(ForgeError::ForgedSymbol(t.text.0));
        }
        match t.origin {
            Origin::Source { package } => {
                if self.registry.package(package).is_none() {
                    return Err(ForgeError::UnknownPackage(package));
                }
            }
            Origin::Expansion(x) => {
                let st = self.state(x)?;
                if st.status != ExpansionStatus::Finished {
                    return Err(ForgeError::ExpansionNotFinished(x));
                }
            }
        }
        Ok(())
    }

    fn state(&self, x: ExpansionId) -> Result<&ExpansionState, ForgeError> {
        if x.session != self.session {
            return Err(ForgeError::ForeignExpansion(x));
        }
        self.expansion_state
            .get(&x.index)
            .ok_or(ForgeError::UnknownExpansion(x))
    }

    pub fn expansion(&self, x: ExpansionId) -> Result<&ExpansionRecord, ForgeError> {
        Ok(&self.state(x)?.record)
    }

    /// Package that emitted tokens of origin `o`: the source package, or the
    /// macro package for generated tokens.
    pub fn emitting_package(&self, o: Origin) -> Result<PackageId, ForgeError> {
        match o {
            Origin::Source { package } => Ok(package),
            Origin::Expansion(x) => Ok(self.expansion(x)?.macro_package),
        }
    }

    // ----- environments ---------------------------------------------------

    /// Brings `def` into `package`'s top-level environment as `name`.
    /// The definition must be visible from `package`.
    pub fn declare_global(
        &mut self,
        package: PackageId,
        name: &str,
        def: DefId,
    ) -> Result<(), ForgeError> {
        self.registry.visible_from(package, def)?;
        let symbol = self.intern(name)?;
        let scope = self.module_scope(package)?;
        self.globals.push(GlobalBinding {
            symbol,
            scopes: ScopeSet::single(scope),
            def,
        });
        Ok(())
    }

    /// Lexes trusted source text of `package` (fixture/host lexer). Never use
    /// this for guest output: the tokens receive source origin.
    pub fn lex_source(
        &mut self,
        package: PackageId,
        src: &str,
    ) -> Result<Vec<HostToken>, ForgeError> {
        let scope = self.module_scope(package)?;
        lex_generated(src)
            .into_iter()
            .map(|g| {
                Ok(HostToken {
                    kind: g.kind,
                    text: self.intern(&g.text)?,
                    scopes: ScopeSet::single(scope),
                    origin: Origin::Source { package },
                    claimed_def: None,
                    reported_span: None,
                })
            })
            .collect()
    }

    /// Adds a minted scope to every token (Flatt's add-scope, e.g. for a
    /// compiler-chosen use-site scope).
    pub fn add_scope(&self, tokens: &mut [HostToken], scope: ScopeId) -> Result<(), ForgeError> {
        self.validate_scope(scope)?;
        for t in tokens {
            t.scopes.insert(scope);
        }
        Ok(())
    }

    // ----- expansions -----------------------------------------------------

    /// Starts expanding macro `macro_def` at `site`. Each caller argument is
    /// stored in a host arena and a fresh splice handle is returned for it.
    pub fn begin_expansion(
        &mut self,
        macro_def: DefId,
        site: InvocationSite,
        caller_args: Vec<Vec<HostToken>>,
    ) -> Result<Expansion, ForgeError> {
        let info = self
            .registry
            .definition(macro_def)
            .ok_or(ForgeError::UnknownDefinition(macro_def))?;
        if info.kind != DefKind::Macro {
            return Err(ForgeError::NotAMacro(macro_def));
        }
        let (root_caller, invoking_package) = match site {
            InvocationSite::Source(p) => {
                if self.registry.package(p).is_none() {
                    return Err(ForgeError::UnknownPackage(p));
                }
                (p, p)
            }
            InvocationSite::Expansion(parent) => {
                let st = self.state(parent)?;
                if st.status != ExpansionStatus::Finished {
                    return Err(ForgeError::ExpansionNotFinished(parent));
                }
                (st.record.root_caller, st.record.macro_package)
            }
        };
        // The invoking code must be able to name the macro.
        self.registry.visible_from(invoking_package, macro_def)?;
        for arg in &caller_args {
            for t in arg {
                self.validate_token(t)?;
            }
        }
        let macro_package = macro_def.package;
        self.module_scope(macro_package)?;
        let id = ExpansionId {
            session: self.session,
            index: self.expansions.mint()?,
        };
        let intro_scope = self.fresh_scope()?;
        let mut arena = BTreeMap::new();
        let mut splices = Vec::with_capacity(caller_args.len());
        for arg in caller_args {
            let nonce = self.splices.mint()?;
            arena.insert(nonce, arg);
            splices.push(SpliceHandle {
                session: self.session,
                expansion: id,
                nonce,
            });
        }
        self.expansion_state.insert(
            id.index,
            ExpansionState {
                record: ExpansionRecord {
                    id,
                    macro_def,
                    macro_package,
                    site,
                    root_caller,
                    intro_scope,
                },
                arena,
                status: ExpansionStatus::Open,
                typed_alive: None,
            },
        );
        Ok(Expansion { id, splices })
    }

    /// Finishes an expansion from a structured quotation. Generated tokens
    /// receive the macro's definition-site scope plus the fresh introduction
    /// scope and macro origin; antiquotes are replaced by the host's stored
    /// caller syntax, which keeps its own origin and scopes.
    ///
    /// The expansion is closed whether or not this succeeds.
    pub fn finish_quotation(
        &mut self,
        id: ExpansionId,
        q: &Quotation,
    ) -> Result<Vec<HostToken>, ForgeError> {
        self.open(id)?;
        let result = self.expand_items(id, &q.items);
        self.close(id, result.is_ok());
        result
    }

    /// Finishes an expansion whose macro produced flat generated tokens with
    /// no splice evidence: every token is attributed to the macro.
    pub fn finish_generated(
        &mut self,
        id: ExpansionId,
        tokens: Vec<GeneratedToken>,
    ) -> Result<Vec<HostToken>, ForgeError> {
        let items: Vec<QuoteItem> = tokens.into_iter().map(QuoteItem::Token).collect();
        self.finish_quotation(id, &Quotation { items })
    }

    /// Closes an open expansion as failed (e.g. the guest reported errors).
    pub fn abandon(&mut self, id: ExpansionId) -> Result<(), ForgeError> {
        self.open(id)?;
        self.close(id, false);
        Ok(())
    }

    pub(crate) fn typed_bind_expansion(
        &mut self,
        id: ExpansionId,
        alive: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<(), ForgeError> {
        self.open(id)?;
        self.expansion_state
            .get_mut(&id.index)
            .expect("validated expansion")
            .typed_alive = Some(alive);
        Ok(())
    }
    pub(crate) fn typed_retire_count(&self) -> usize {
        self.expansion_state.len()
    }
    pub(crate) fn retire_typed_expansions(&mut self) {
        for state in self.expansion_state.values_mut() {
            if state.status == ExpansionStatus::Open
                && state
                    .typed_alive
                    .as_ref()
                    .is_some_and(|alive| !alive.load(std::sync::atomic::Ordering::Relaxed))
            {
                state.status = ExpansionStatus::Failed;
                state.arena.clear();
            }
        }
    }
    fn open(&self, id: ExpansionId) -> Result<(), ForgeError> {
        let state = self.state(id)?;
        if state
            .typed_alive
            .as_ref()
            .is_some_and(|alive| !alive.load(std::sync::atomic::Ordering::Relaxed))
        {
            return Err(ForgeError::ExpansionClosed(id));
        }
        match state.status {
            ExpansionStatus::Open => Ok(()),
            _ => Err(ForgeError::ExpansionClosed(id)),
        }
    }

    fn close(&mut self, id: ExpansionId, ok: bool) {
        if let Some(st) = self.expansion_state.get_mut(&id.index) {
            if let Some(alive) = &st.typed_alive {
                alive.store(false, std::sync::atomic::Ordering::Relaxed);
            }
            st.status = if ok {
                ExpansionStatus::Finished
            } else {
                ExpansionStatus::Failed
            };
            st.arena.clear();
        }
    }

    fn expand_items(
        &mut self,
        id: ExpansionId,
        items: &[QuoteItem],
    ) -> Result<Vec<HostToken>, ForgeError> {
        let lim = self.quote_limits;
        if items.len() > lim.max_items {
            return Err(ForgeError::QuoteLimit {
                what: "quotation items",
                limit: lim.max_items,
            });
        }
        let rec = self.state(id)?.record.clone();
        let mut gen_scopes = ScopeSet::single(self.module_scope(rec.macro_package)?);
        gen_scopes.insert(rec.intro_scope);
        let mut out: Vec<HostToken> = Vec::new();
        let mut text_bytes = 0usize;
        let mut splices = 0usize;
        for item in items {
            match item {
                QuoteItem::Token(g) => {
                    text_bytes = text_bytes.saturating_add(g.text.len());
                    out.push(HostToken {
                        kind: g.kind,
                        text: self.intern(&g.text)?,
                        scopes: gen_scopes.clone(),
                        origin: Origin::Expansion(id),
                        claimed_def: g.claimed_def,
                        reported_span: g.reported_span,
                    });
                }
                QuoteItem::Splice(h) => {
                    splices += 1;
                    if splices > lim.max_splices {
                        return Err(ForgeError::QuoteLimit {
                            what: "splices",
                            limit: lim.max_splices,
                        });
                    }
                    let stored = self.lookup_splice(id, *h)?;
                    for t in stored {
                        text_bytes = text_bytes.saturating_add(self.text(t.text).len());
                    }
                    out.extend(stored.iter().cloned());
                }
            }
            if out.len() > lim.max_output_tokens {
                return Err(ForgeError::QuoteLimit {
                    what: "output tokens",
                    limit: lim.max_output_tokens,
                });
            }
            if text_bytes > lim.max_text_bytes {
                return Err(ForgeError::QuoteLimit {
                    what: "text bytes",
                    limit: lim.max_text_bytes,
                });
            }
        }
        check_delimiters(&out, lim.max_depth)?;
        Ok(out)
    }

    fn lookup_splice(&self, id: ExpansionId, h: SpliceHandle) -> Result<&[HostToken], ForgeError> {
        if h.session != self.session || h.expansion.session != self.session {
            return Err(ForgeError::Splice {
                fault: SpliceFault::Foreign,
                handle: h,
            });
        }
        if h.expansion != id {
            let fault = if self.expansion_state.contains_key(&h.expansion.index) {
                SpliceFault::Stale
            } else {
                SpliceFault::Forged
            };
            return Err(ForgeError::Splice { fault, handle: h });
        }
        self.expansion_state[&id.index]
            .arena
            .get(&h.nonce)
            .map(Vec::as_slice)
            .ok_or(ForgeError::Splice {
                fault: SpliceFault::Forged,
                handle: h,
            })
    }
}

pub(crate) fn check_delimiters(tokens: &[HostToken], max_depth: usize) -> Result<(), ForgeError> {
    let mut stack = Vec::new();
    for (i, t) in tokens.iter().enumerate() {
        match t.kind {
            TokenKind::Open(d) => {
                stack.push(d);
                if stack.len() > max_depth {
                    return Err(ForgeError::QuoteLimit {
                        what: "nesting depth",
                        limit: max_depth,
                    });
                }
            }
            TokenKind::Close(d) if stack.pop() != Some(d) => {
                return Err(ForgeError::UnbalancedDelimiters { token: i });
            }
            _ => {}
        }
    }
    if !stack.is_empty() {
        return Err(ForgeError::UnbalancedDelimiters {
            token: tokens.len(),
        });
    }
    Ok(())
}
