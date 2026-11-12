//! Structured raw-claim adapter. Only the current typed expansion supplies slots.
//! Neither guest output nor a raw cache record contains host identity authority.
use crate::typed_quote::{ExpandedExpr, ExprExpansion, ExprQuoteItem, TypedBudget, TypedError};
use crate::{
    DefId, Delim, GeneratedToken, HostToken, HygieneHost, PackageId, ReportedSpan, TokenKind,
};
use vesper_loom_sandbox::{quote_transport as q, transport as t};
#[derive(Debug)]
pub enum Error {
    Transport(q::Error),
    Typed(TypedError),
    GuestDiagnostic,
    Allocation,
}
impl From<q::Error> for Error {
    fn from(e: q::Error) -> Self {
        Self::Transport(e)
    }
}
impl From<TypedError> for Error {
    fn from(e: TypedError) -> Self {
        Self::Typed(e)
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "structured Expr import {self:?}")
    }
}
impl std::error::Error for Error {}
fn copy(s: &str) -> Result<String, Error> {
    let mut out = String::new();
    out.try_reserve_exact(s.len())
        .map_err(|_| Error::Allocation)?;
    out.push_str(s);
    Ok(out)
}
fn bound(n: usize, max: usize, r: q::Resource) -> Result<(), Error> {
    if n > max {
        Err(q::Error::Budget(r).into())
    } else {
        Ok(())
    }
}
fn add(a: usize, b: usize) -> Result<usize, Error> {
    a.checked_add(b)
        .ok_or(q::Error::Budget(q::Resource::Work).into())
}
/// Exports no scopes, nonces, symbol IDs or DefIds. Full input and encoding
/// bounds are checked before copies; current-host identity is checked first.
pub fn invocation_for_expr(
    host: &HygieneHost,
    expansion: &ExprExpansion,
    kind: q::MacroKind,
    entry: &str,
    args: &[String],
    limits: q::Limits,
    budget: &mut TypedBudget,
) -> Result<q::Invocation, Error> {
    limits.validate()?;
    expansion.validate_host(host)?;
    if entry.is_empty() || entry.len() > 256 || entry.chars().any(char::is_control) {
        return Err(q::Error::Entry.into());
    }
    bound(args.len(), 64, q::Resource::Arguments)?;
    bound(
        expansion.inputs().len(),
        limits.max_slots,
        q::Resource::Slots,
    )?;
    let (mut tokens, mut text, mut scopes, mut encoded) =
        (0, entry.len(), 0, add(21, entry.len())?);
    for arg in args {
        text = add(text, arg.len())?;
        encoded = add(encoded, add(4, arg.len())?)?;
    }
    for input in expansion.inputs() {
        budget.charge_work(1)?;
        tokens = add(tokens, input.tokens().len())?;
        bound(tokens, limits.max_input_tokens, q::Resource::Tokens)?;
        encoded = add(encoded, 5)?;
        for token in input.tokens() {
            budget.charge_work(add(token.scopes().len(), 1)?)?;
            let s = host_text(host, token)?;
            text = add(text, s.len())?;
            bound(text, limits.max_input_text_bytes, q::Resource::Text)?;
            encoded = add(encoded, add(19, s.len())?)?;
            scopes = add(scopes, token.scopes().len())?;
        }
    }
    bound(text, limits.max_input_text_bytes, q::Resource::Text)?;
    bound(
        encoded,
        limits.max_invocation_bytes,
        q::Resource::Invocation,
    )?;
    budget.charge_work(
        encoded
            .checked_mul(4)
            .and_then(|n| n.checked_add(scopes))
            .ok_or(q::Error::Budget(q::Resource::Work))?,
    )?;
    let metadata = tokens
        .checked_mul(std::mem::size_of::<q::InputToken>())
        .and_then(|n| {
            expansion
                .inputs()
                .len()
                .checked_mul(std::mem::size_of::<q::InputSlot>())
                .and_then(|slots| n.checked_add(slots))
        })
        .and_then(|n| {
            args.len()
                .checked_mul(std::mem::size_of::<String>())
                .and_then(|args| n.checked_add(args))
        })
        .and_then(|n| n.checked_add(encoded))
        .ok_or(q::Error::Budget(q::Resource::Work))?;
    budget.charge_metadata(metadata)?;
    if budget.limits().max_depth > limits.max_depth {
        return Err(q::Error::Budget(q::Resource::Depth).into());
    }
    let mut slots = Vec::new();
    slots
        .try_reserve_exact(expansion.inputs().len())
        .map_err(|_| Error::Allocation)?;
    for input in expansion.inputs() {
        let mut tokens = Vec::new();
        tokens
            .try_reserve_exact(input.tokens().len())
            .map_err(|_| Error::Allocation)?;
        for token in input.tokens() {
            tokens.push(q::InputToken {
                kind: wire_kind(token.kind()),
                text: copy(host.text(token.text()))?,
                reported_span: token.reported_span().map(|s| t::Span {
                    file: s.file,
                    start: s.start,
                    end: s.end,
                }),
            });
        }
        slots.push(q::InputSlot {
            category: q::Category::Expr,
            tokens,
        });
    }
    let mut owned_args = Vec::new();
    owned_args
        .try_reserve_exact(args.len())
        .map_err(|_| Error::Allocation)?;
    for a in args {
        owned_args.push(copy(a)?);
    }
    let inv = q::Invocation {
        protocol_version: 1,
        kind,
        entry: copy(entry)?,
        args: owned_args,
        slots,
    };
    limits.check_invocation(&inv)?;
    Ok(inv)
}
fn host_text<'a>(host: &'a HygieneHost, token: &HostToken) -> Result<&'a str, Error> {
    host.validate_token(token).map_err(TypedError::from)?;
    Ok(host.text(token.text()))
}
/// Always closes the supplied expansion, including transport/allocation failure.
/// Raw InputSlot ordinals resolve only against this current sealed expansion.
pub fn finish_expr_outcome(
    host: &mut HygieneHost,
    expansion: ExprExpansion,
    outcome: &q::Outcome,
    limits: q::Limits,
    budget: &mut TypedBudget,
) -> Result<ExpandedExpr, Error> {
    let checked = (|| {
        expansion.validate_host(host)?;
        let mut charge = outcome.items.len();
        for item in &outcome.items {
            if let q::Item::Generated(t) = item {
                charge = add(
                    charge,
                    t.text
                        .len()
                        .checked_mul(4)
                        .ok_or(q::Error::Budget(q::Resource::Work))?,
                )?;
            }
        }
        for d in &outcome.diagnostics {
            charge = add(charge, d.message.len() + 1)?;
        }
        budget.charge_work(charge)?;
        limits.check_outcome(outcome, expansion.inputs().len(), false)?;
        if outcome
            .diagnostics
            .iter()
            .any(|d| d.severity == t::Severity::Error)
        {
            return Err(Error::GuestDiagnostic);
        }
        if budget.limits().max_depth > limits.max_depth {
            return Err(q::Error::Budget(q::Resource::Depth).into());
        }
        let (n, text, scopes) = expanded_stats(host, &expansion, outcome, limits)?;
        budget.charge_work(
            n.checked_mul(128)
                .and_then(|v| text.checked_mul(4).and_then(|t| v.checked_add(t)))
                .and_then(|v| v.checked_add(scopes))
                .ok_or(q::Error::Budget(q::Resource::Work))?,
        )?;
        budget.charge_metadata(
            outcome
                .items
                .len()
                .checked_mul(std::mem::size_of::<ExprQuoteItem>())
                .and_then(|n| n.checked_add(text))
                .ok_or(q::Error::Budget(q::Resource::Work))?,
        )?;
        let mut items = Vec::new();
        items
            .try_reserve_exact(outcome.items.len())
            .map_err(|_| Error::Allocation)?;
        for item in &outcome.items {
            items.push(match item {
                q::Item::InputSlot(n) => ExprQuoteItem::InputSlot(*n),
                q::Item::Generated(t) => ExprQuoteItem::Generated(GeneratedToken {
                    kind: host_kind(t.kind),
                    text: copy(&t.text)?,
                    claimed_def: t.claimed_def.map(|d| DefId {
                        package: PackageId(d.package),
                        item: d.item,
                    }),
                    reported_span: t.reported_span.map(|s| ReportedSpan {
                        file: s.file,
                        start: s.start,
                        end: s.end,
                    }),
                }),
            });
        }
        Ok(items)
    })();
    match checked {
        Ok(items) => host
            .finish_expr(expansion, &items, budget)
            .map_err(Error::Typed),
        Err(error) => {
            let _ = host.abandon_expr(expansion);
            Err(error)
        }
    }
}
fn wire_delim(d: Delim) -> t::Delimiter {
    match d {
        Delim::Paren => t::Delimiter::Paren,
        Delim::Bracket => t::Delimiter::Bracket,
        Delim::Brace => t::Delimiter::Brace,
    }
}
fn host_delim(d: t::Delimiter) -> Delim {
    match d {
        t::Delimiter::Paren => Delim::Paren,
        t::Delimiter::Bracket => Delim::Bracket,
        t::Delimiter::Brace => Delim::Brace,
    }
}
fn wire_kind(k: TokenKind) -> t::TokenKind {
    match k {
        TokenKind::Ident => t::TokenKind::Ident,
        TokenKind::Punct => t::TokenKind::Punct,
        TokenKind::Int => t::TokenKind::IntLiteral,
        TokenKind::Str => t::TokenKind::StringLiteral,
        TokenKind::Open(d) => t::TokenKind::Open(wire_delim(d)),
        TokenKind::Close(d) => t::TokenKind::Close(wire_delim(d)),
    }
}
fn host_kind(k: t::TokenKind) -> TokenKind {
    match k {
        t::TokenKind::Ident => TokenKind::Ident,
        t::TokenKind::Punct => TokenKind::Punct,
        t::TokenKind::IntLiteral => TokenKind::Int,
        t::TokenKind::StringLiteral => TokenKind::Str,
        t::TokenKind::Open(d) => TokenKind::Open(host_delim(d)),
        t::TokenKind::Close(d) => TokenKind::Close(host_delim(d)),
    }
}

fn expanded_stats(
    host: &HygieneHost,
    expansion: &ExprExpansion,
    out: &q::Outcome,
    limits: q::Limits,
) -> Result<(usize, usize, usize), Error> {
    let (mut n, mut text, mut scopes) = (0usize, 0usize, 0usize);
    for item in &out.items {
        match item {
            q::Item::Generated(t) => {
                n = add(n, 1)?;
                text = add(text, t.text.len())?;
                scopes = add(scopes, 2)?;
            }
            q::Item::InputSlot(i) => {
                let input = expansion.inputs().get(*i as usize).ok_or(q::Error::Slot)?;
                n = add(n, add(input.tokens().len(), 2)?)?;
                bound(n, limits.max_expanded_tokens, q::Resource::Tokens)?;
                text = add(text, 2)?;
                scopes = add(scopes, 4)?;
                for token in input.tokens() {
                    text = add(text, host.text(token.text()).len())?;
                    scopes = add(scopes, token.scopes().len())?;
                    bound(text, limits.max_expanded_text_bytes, q::Resource::Text)?;
                    bound(scopes, limits.max_expanded_scopes, q::Resource::Scopes)?;
                }
            }
        }
        bound(n, limits.max_expanded_tokens, q::Resource::Tokens)?;
        bound(text, limits.max_expanded_text_bytes, q::Resource::Text)?;
        bound(scopes, limits.max_expanded_scopes, q::Resource::Scopes)?;
    }
    Ok((n, text, scopes))
}
