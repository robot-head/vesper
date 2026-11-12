//! Scope-set binder/reference resolution for the lexical fragment.
//!
//! A binding `b` is a candidate for reference `r` when they have the same
//! symbol and `scopes(b) ⊆ scopes(r)`. The candidate with the largest scope
//! set wins; if the maximal candidates disagree the reference is ambiguous
//! and resolution fails. Each `let` mints a fresh binding scope that is added
//! to its binder and to everything in its body. Pre-resolved global claims
//! bypass lexical lookup but are validated against the trusted registry from
//! the point of view of the package that *emitted* the token.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::error::ForgeError;
use crate::host::{ExpansionRecord, HygieneHost, InvocationSite, check_delimiters};
use crate::identity::{BinderId, ExpansionId, ScopeSet, Symbol};
use crate::registry::{DefId, DefKind};
use crate::syntax::{Path, Term, parse};
use crate::token::{HostToken, Origin};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Resolution {
    Local(BinderId),
    Global(DefId),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BinderInfo {
    pub id: BinderId,
    pub token: usize,
    pub name: String,
    pub origin: Origin,
    pub scopes: ScopeSet,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedRef {
    /// Index of the path's first token.
    pub token: usize,
    pub name: String,
    pub origin: Origin,
    pub resolution: Resolution,
    /// Whether the global came from a validated pre-resolved claim.
    pub pre_resolved: bool,
}

/// An operation introduced by the fragment that may need authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum OperationKind {
    /// Any use of a global definition (call or reference).
    Use { def: DefId },
    /// `handle effect with handler { .. }`.
    Handler { effect: DefId, handler: DefId },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmittedOperation {
    pub kind: OperationKind,
    /// Index of the operation's first token.
    pub token: usize,
    /// Every origin that contributed a token to the operation. An operation
    /// is attributed to *each* expansion listed here.
    pub origins: BTreeSet<Origin>,
}

/// Output of [`HygieneHost::resolve`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedFragment {
    pub tokens: Vec<HostToken>,
    pub term: Term,
    pub binders: Vec<BinderInfo>,
    pub references: Vec<ResolvedRef>,
    pub operations: Vec<EmittedOperation>,
    /// Every expansion that contributed tokens, with its ancestors.
    pub expansions: BTreeMap<ExpansionId, ExpansionRecord>,
}

impl ResolvedFragment {
    /// Resolution of the reference starting at token `i`.
    pub fn resolution_at(&self, i: usize) -> Option<Resolution> {
        self.references
            .iter()
            .find(|r| r.token == i)
            .map(|r| r.resolution)
    }

    pub fn binder(&self, id: BinderId) -> Option<&BinderInfo> {
        self.binders.iter().find(|b| b.id == id)
    }
}

struct LocalBinding {
    symbol: Symbol,
    scopes: ScopeSet,
    binder: BinderId,
}

impl HygieneHost {
    /// Parses and resolves a fully expanded token sequence.
    pub fn resolve(&mut self, tokens: Vec<HostToken>) -> Result<ResolvedFragment, ForgeError> {
        for t in &tokens {
            self.validate_token(t)?;
        }
        check_delimiters(&tokens, self.quote_limits.max_depth)?;
        let term = parse(self, &tokens)?;
        let mut expansions = BTreeMap::new();
        for t in &tokens {
            if let Origin::Expansion(mut x) = t.origin {
                loop {
                    if expansions.contains_key(&x) {
                        break;
                    }
                    let rec = self.expansion(x)?.clone();
                    let parent = match rec.site {
                        InvocationSite::Expansion(p) => Some(p),
                        InvocationSite::Source(_) => None,
                    };
                    expansions.insert(x, rec);
                    match parent {
                        Some(p) => x = p,
                        None => break,
                    }
                }
            }
        }
        let mut r = Resolver {
            host: self,
            tokens: &tokens,
            locals: Vec::new(),
            binders: Vec::new(),
            references: Vec::new(),
            operations: Vec::new(),
        };
        r.term(&term, &ScopeSet::new())?;
        let Resolver {
            binders,
            references,
            operations,
            ..
        } = r;
        Ok(ResolvedFragment {
            tokens,
            term,
            binders,
            references,
            operations,
            expansions,
        })
    }
}

struct Resolver<'a> {
    host: &'a mut HygieneHost,
    tokens: &'a [HostToken],
    locals: Vec<LocalBinding>,
    binders: Vec<BinderInfo>,
    references: Vec<ResolvedRef>,
    operations: Vec<EmittedOperation>,
}

impl Resolver<'_> {
    fn term(&mut self, t: &Term, ctx: &ScopeSet) -> Result<(), ForgeError> {
        match t {
            Term::Lit(_) => Ok(()),
            Term::Ref(p) => {
                let res = self.path(p, ctx)?;
                if let Resolution::Global(def) = res {
                    let origins = self.path_origins(p);
                    self.operations.push(EmittedOperation {
                        kind: OperationKind::Use { def },
                        token: p.tokens[0],
                        origins,
                    });
                }
                Ok(())
            }
            Term::Let {
                binder,
                value,
                body,
            } => {
                self.term(value, ctx)?;
                let tok = &self.tokens[*binder];
                if let Some(def) = tok.claimed_def {
                    return Err(ForgeError::PreResolvedBinder {
                        token: *binder,
                        def,
                    });
                }
                let scope = self.host.fresh_scope()?;
                let mut inner = ctx.clone();
                inner.insert(scope);
                let scopes = tok.scopes.union(&inner);
                let id = self.host.fresh_binder()?;
                self.locals.push(LocalBinding {
                    symbol: tok.text,
                    scopes: scopes.clone(),
                    binder: id,
                });
                self.binders.push(BinderInfo {
                    id,
                    token: *binder,
                    name: self.host.text(tok.text).to_owned(),
                    origin: tok.origin,
                    scopes,
                });
                self.term(body, &inner)
            }
            Term::Call { callee, args, .. } => {
                self.term(callee, ctx)?;
                for a in args {
                    self.term(a, ctx)?;
                }
                Ok(())
            }
            Term::Block(items) => {
                for i in items {
                    self.term(i, ctx)?;
                }
                Ok(())
            }
            Term::Handle {
                keyword,
                effect,
                handler,
                body,
            } => {
                let effect_def = self.global_of_kind(effect, ctx, DefKind::Effect)?;
                let handler_def = self.global_of_kind(handler, ctx, DefKind::Handler)?;
                let mut origins = self.path_origins(effect);
                origins.extend(self.path_origins(handler));
                origins.insert(self.tokens[*keyword].origin);
                self.operations.push(EmittedOperation {
                    kind: OperationKind::Handler {
                        effect: effect_def,
                        handler: handler_def,
                    },
                    token: *keyword,
                    origins,
                });
                self.term(body, ctx)
            }
        }
    }

    fn path_origins(&self, p: &Path) -> BTreeSet<Origin> {
        p.tokens.iter().map(|&i| self.tokens[i].origin).collect()
    }

    fn global_of_kind(
        &mut self,
        p: &Path,
        ctx: &ScopeSet,
        kind: DefKind,
    ) -> Result<DefId, ForgeError> {
        match self.path(p, ctx)? {
            Resolution::Global(def) => {
                let found = self.host.registry.definition(def).map(|d| d.kind);
                if found != Some(kind) {
                    return Err(ForgeError::WrongDefinitionKind {
                        token: p.tokens[0],
                        def,
                        expected: kind,
                    });
                }
                Ok(def)
            }
            Resolution::Local(_) => Err(ForgeError::ExpectedGlobal { token: p.tokens[0] }),
        }
    }

    fn path(&mut self, p: &Path, ctx: &ScopeSet) -> Result<Resolution, ForgeError> {
        let first = &self.tokens[p.tokens[0]];
        let name = self.host.text(p.symbol).to_owned();
        let mut claims = BTreeSet::new();
        for &i in &p.tokens {
            let t = &self.tokens[i];
            if t.scopes != first.scopes || t.origin != first.origin {
                return Err(ForgeError::MixedPath { token: i });
            }
            if let Some(d) = t.claimed_def {
                claims.insert(d);
            }
        }
        if claims.len() > 1 {
            return Err(ForgeError::MixedPath { token: p.tokens[0] });
        }
        let origin = first.origin;
        let (resolution, pre_resolved) = if let Some(&def) = claims.iter().next() {
            let from = self.host.emitting_package(origin)?;
            self.host.registry.validate_claim(from, &name, def)?;
            (Resolution::Global(def), true)
        } else {
            let scopes = first.scopes.union(ctx);
            (self.lookup(p.symbol, &scopes, &name, p.tokens[0])?, false)
        };
        self.references.push(ResolvedRef {
            token: p.tokens[0],
            name,
            origin,
            resolution,
            pre_resolved,
        });
        Ok(resolution)
    }

    fn lookup(
        &self,
        symbol: Symbol,
        scopes: &ScopeSet,
        name: &str,
        token: usize,
    ) -> Result<Resolution, ForgeError> {
        let mut cands: Vec<(&ScopeSet, Resolution)> = Vec::new();
        for l in &self.locals {
            if l.symbol == symbol && l.scopes.is_subset(scopes) {
                cands.push((&l.scopes, Resolution::Local(l.binder)));
            }
        }
        for g in &self.host.globals {
            if g.symbol == symbol && g.scopes.is_subset(scopes) {
                cands.push((&g.scopes, Resolution::Global(g.def)));
            }
        }
        if cands.is_empty() {
            return Err(ForgeError::Unbound {
                token,
                name: name.to_owned(),
            });
        }
        // Maximal candidates: no other candidate strictly contains them.
        let maximal: BTreeSet<Resolution> = cands
            .iter()
            .filter(|(s, _)| {
                !cands
                    .iter()
                    .any(|(o, _)| s.is_subset(o) && s.len() < o.len())
            })
            .map(|&(_, r)| r)
            .collect();
        if maximal.len() == 1 {
            return Ok(*maximal.iter().next().expect("one candidate"));
        }
        Err(ForgeError::Ambiguous {
            token,
            name: name.to_owned(),
            candidates: maximal.into_iter().collect(),
        })
    }
}
