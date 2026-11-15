//! Tier 1: decorating Tier 0 spans with facts the checker reported.
//!
//! [`SemanticFacts`] is a small transport DTO. The `vesper` CLI fills it from
//! the checker's existing outputs (`src/highlight_commands.rs`); this crate
//! never depends on the root package and never runs the checker itself.
use crate::{ChromaError, HighlightSpan, VesperTokenClass as C, flags, highlight_tier0};
use smol_str::SmolStr;
use std::ops::Range;

/// A `Mode::Linear` binding: minted at `mint`, used at `occurrences`, and
/// consumed (discharged) at `discharge`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinearBindingFact {
    pub name: SmolStr,
    /// The binding identifier at its minting site (a `let` pattern or a
    /// `sink` parameter).
    pub mint: Range<usize>,
    /// Identifier ranges of later uses, in source order.
    pub occurrences: Vec<Range<usize>>,
    /// The consuming use, if the checker saw one in this snippet.
    pub discharge: Option<Range<usize>>,
    /// Index into [`SemanticFacts::hovers`].
    pub hover: Option<u32>,
}

/// A FieldTree leaf identifier: `mutated` when written through an `inout`
/// projection, otherwise a read-only (shared `in`) leaf.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldLeafFact {
    pub span: Range<usize>,
    pub mutated: bool,
}

/// An effect the checked World declares: `linear` for `linear effect`
/// capabilities, otherwise abortable (`Fail`, `Cancel`, plain `effect`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EffectFact {
    pub name: SmolStr,
    pub linear: bool,
}

/// One hover-table row: an inferred type and mode for the tokens in `span`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HoverEntry {
    pub span: Range<usize>,
    pub text: SmolStr,
}

/// Checker facts for one snippet.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SemanticFacts {
    pub linear_bindings: Vec<LinearBindingFact>,
    pub field_leaves: Vec<FieldLeafFact>,
    pub effects: Vec<EffectFact>,
    pub hovers: Vec<HoverEntry>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
    /// Lossless CST classification only.
    Lossless0,
    /// CST classification decorated with checker facts.
    Semantic1,
}

/// Why Tier 1 was not available, so the output is Tier 0.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FallbackReason {
    /// The snippet has syntax errors (first diagnostic).
    Syntax {
        code: SmolStr,
        message: SmolStr,
        span: Range<usize>,
    },
    /// The snippet parses but does not check (first diagnostic).
    Check {
        code: SmolStr,
        message: SmolStr,
        span: Range<usize>,
    },
    /// The checker's analysis budget (AST size, nesting, type growth) was
    /// exceeded, so the snippet was not checked.
    Budget { message: SmolStr },
    /// Tier 1 was not requested.
    NotRequested,
}

impl FallbackReason {
    pub fn code(&self) -> &str {
        match self {
            Self::Syntax { code, .. } | Self::Check { code, .. } => code,
            Self::Budget { .. } | Self::NotRequested => "",
        }
    }
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Syntax { .. } => "syntax",
            Self::Check { .. } => "check",
            Self::Budget { .. } => "budget",
            Self::NotRequested => "not-requested",
        }
    }
}

/// Highlighted source: lossless spans, the hover table they index, the tier
/// that produced them and, for Tier 0 output, why Tier 1 was not used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Highlight {
    pub spans: Vec<HighlightSpan>,
    pub hovers: Vec<HoverEntry>,
    pub tier: Tier,
    pub fallback: Option<FallbackReason>,
}

/// Tier 0 output carrying the typed reason Tier 1 was unavailable.
pub fn tier0_fallback(src: &str, reason: FallbackReason) -> Result<Highlight, ChromaError> {
    Ok(Highlight {
        spans: highlight_tier0(src)?,
        hovers: vec![],
        tier: Tier::Lossless0,
        fallback: Some(reason),
    })
}

fn check_range(src: &str, r: &Range<usize>) -> Result<(), ChromaError> {
    if r.start > r.end
        || r.end > src.len()
        || !src.is_char_boundary(r.start)
        || !src.is_char_boundary(r.end)
    {
        return Err(ChromaError::InvalidFact {
            start: r.start,
            end: r.end,
        });
    }
    Ok(())
}

/// Index of the span that exactly covers `r`, if one does.
fn exact(spans: &[HighlightSpan], r: &Range<usize>) -> Option<usize> {
    let i = spans.partition_point(|s| s.start < r.start);
    spans
        .get(i)
        .filter(|s| s.start == r.start && s.end == r.end)
        .map(|_| i)
}

/// Indices of the spans lying inside `r`.
fn inside(spans: &[HighlightSpan], r: &Range<usize>) -> Range<usize> {
    let a = spans.partition_point(|s| s.start < r.start);
    let b = spans.partition_point(|s| s.end <= r.end);
    a..b.max(a)
}

/// Tier 1: Tier 0 spans decorated with `facts`. Facts whose ranges do not
/// match a token are ignored (the CST is the source of truth for token
/// boundaries); facts outside the source are a typed error.
pub fn highlight_tier1(src: &str, facts: &SemanticFacts) -> Result<Highlight, ChromaError> {
    let mut spans = highlight_tier0(src)?;
    for b in &facts.linear_bindings {
        check_range(src, &b.mint)?;
        for o in &b.occurrences {
            check_range(src, o)?;
        }
        if let Some(d) = &b.discharge {
            check_range(src, d)?;
        }
    }
    for leaf in &facts.field_leaves {
        check_range(src, &leaf.span)?;
    }
    for h in &facts.hovers {
        check_range(src, &h.span)?;
    }
    let hover_len = u32::try_from(facts.hovers.len())
        .ok()
        .filter(|n| *n < u32::MAX)
        .ok_or(ChromaError::HoverTableOverflow {
            len: facts.hovers.len(),
        })?;
    let valid_hover = |h: Option<u32>| h.filter(|h| *h < hover_len);

    // Hovers first, so the more specific facts below override their ids.
    for (id, h) in facts.hovers.iter().enumerate() {
        for i in inside(&spans, &h.span) {
            if spans[i].class != C::Whitespace && spans[i].class != C::CommentTrivia {
                spans[i].hover_sym_id = Some(id as u32);
            }
        }
    }
    let effects: std::collections::BTreeMap<&str, bool> = facts
        .effects
        .iter()
        .map(|e| (e.name.as_str(), e.linear))
        .collect();
    for s in spans.iter_mut() {
        if matches!(
            s.class,
            C::TypeNominal | C::EffectLinear | C::EffectAbortable
        ) && let Some(&linear) = effects.get(&src[s.start..s.end])
        {
            s.class = if linear {
                C::EffectLinear
            } else {
                C::EffectAbortable
            };
        }
    }
    for leaf in &facts.field_leaves {
        if let Some(i) = exact(&spans, &leaf.span) {
            let s = &mut spans[i];
            if leaf.mutated {
                s.class = C::FieldLeafMut;
                s.flags |= flags::MUTATED_LEAF;
            } else {
                s.class = C::FieldLeafRead;
            }
        }
    }
    for b in &facts.linear_bindings {
        // Live from minting to the consuming use (or to the last use seen).
        let live_end = b
            .discharge
            .as_ref()
            .map(|d| d.end)
            .or_else(|| b.occurrences.iter().map(|o| o.end).max())
            .unwrap_or(b.mint.end);
        for r in std::iter::once(&b.mint).chain(&b.occurrences) {
            if let Some(i) = exact(&spans, r) {
                let s = &mut spans[i];
                s.class = C::LinearBinding;
                s.is_live_linear = r.end <= live_end;
                if let Some(h) = valid_hover(b.hover) {
                    s.hover_sym_id = Some(h);
                }
            }
        }
    }
    crate::verify_lossless_spans(src.len(), &spans)?;
    Ok(Highlight {
        spans,
        hovers: facts.hovers.clone(),
        tier: Tier::Semantic1,
        fallback: None,
    })
}
