//! Import-free operator tiers and tagged literals for the surface CST.
use super::*;

pub const SUGAR_OPERATOR: SyntaxKind = SyntaxKind(220);
pub const PIPE_EXPR: SyntaxKind = SyntaxKind(221);
pub const DOTTED_EXPR: SyntaxKind = SyntaxKind(222);
pub const TAGGED_LITERAL: SyntaxKind = SyntaxKind(223);

/// The core Pratt tiers leave one level between assignment and logical OR
/// for the two pipeline operators. All other infix operators associate left.
pub(super) fn core_operator(k: K) -> Option<(u8, bool)> {
    let level = match k {
        K::Eq | K::PlusEq | K::MinusEq | K::StarEq | K::SlashEq | K::PercentEq => 2,
        K::Or => 4,
        K::And => 6,
        K::EqEq | K::Ne => 8,
        K::Lt | K::Le | K::Gt | K::Ge => 10,
        K::Pipe => 12,
        K::Caret => 14,
        K::Amp => 16,
        K::Shl | K::Shr => 18,
        K::Plus | K::Minus => 20,
        K::Star | K::Slash | K::Percent => 22,
        _ => return None,
    };
    Some((level, level == 2))
}

/// Recognize an unbroken operator spelling starting at a raw lexer token.
/// Returns its Pratt precedence and the exclusive end token index. Trivia
/// interrupts a symbol, so `x . + y` can never become `x .+ y`.
pub fn sugar_operator(tokens: &[Token], start: usize) -> Option<(u8, usize)> {
    let head = tokens.get(start)?;
    let next = tokens.get(start + 1)?;
    if head.span.end != next.span.start {
        return None;
    }
    if head.kind == K::Pipe {
        return match next.kind {
            K::Gt => Some((3, start + 2)),
            K::Question => {
                let last = tokens.get(start + 2)?;
                (last.kind == K::Gt && next.span.end == last.span.start).then_some((3, start + 3))
            }
            _ => None,
        };
    }
    if head.kind != K::Dot {
        return None;
    }

    // Longest-match lexer tokens can include a symbolic tail (e.g. `.+=`
    // has a PlusEq token). It is the leading glyph that chooses the tier.
    let base = match next.kind {
        K::EqEq | K::Ne => K::EqEq,
        K::Lt | K::Le | K::Shl | K::Gt | K::Ge | K::Shr => K::Lt,
        K::Pipe | K::PipeEq | K::Or => K::Pipe,
        K::Caret => K::Caret,
        K::Amp | K::AmpEq | K::And => K::Amp,
        K::Plus | K::Minus | K::PlusEq | K::MinusEq | K::Arrow => K::Plus,
        K::Star | K::Slash | K::Percent | K::StarEq | K::SlashEq | K::PercentEq => K::Star,
        _ => return None,
    };
    let mut end = start + 2;
    while let Some(token) = tokens.get(end) {
        let symbol = matches!(
            token.kind,
            K::Dot
                | K::DotDot
                | K::DotDotEq
                | K::Question
                | K::Tilde
                | K::Bang
                | K::FatArrow
                | K::Arrow
                | K::PipeEq
                | K::AmpEq
        ) || core_operator(token.kind).is_some();
        if !symbol || tokens[end - 1].span.end != token.span.start {
            break;
        }
        end += 1;
    }
    Some((core_operator(base)?.0, end))
}

impl Parser<'_> {
    // A brace inside a call argument, tuple or index cannot close an outer
    // header. Restore header mode at the delimiter so the following block
    // still belongs to the surrounding if, match, for or phrase.
    pub(super) fn delimited_expr(&mut self) -> Expr {
        let header = self.no_struct;
        if self.surface {
            self.no_struct = false;
        }
        let expr = self.expr();
        self.no_struct = header;
        expr
    }

    pub(super) fn surface_operator(&self) -> Option<(u8, usize)> {
        self.surface
            .then(|| sugar_operator(&self.tokens, self.index(0)))
            .flatten()
    }

    pub(super) fn consume_sugar_operator(&mut self, end: usize) -> String {
        self.trivia();
        let start = self.start();
        self.builder.start_node(SUGAR_OPERATOR);
        while self.pos < end {
            self.bump();
        }
        self.builder.finish_node();
        self.source[start..self.end()].to_owned()
    }

    pub(super) fn at_tagged_literal(&self) -> bool {
        if !self.surface || !(self.at(K::Lower) || self.at_contextual_name()) {
            return false;
        }
        let word = self.index(0);
        self.tokens.get(word + 1).is_some_and(|delimiter| {
            delimiter.span.start == self.tokens[word].span.end
                && (delimiter.kind == K::String || delimiter.kind == K::LBrace && !self.no_struct)
        })
    }

    pub(super) fn pipeline_placeholder(&mut self) -> Expr {
        let span = self.span();
        self.builder.start_node(EXPR);
        let kind = ExprKind::Name(self.bump());
        self.builder.finish_node();
        Expr { kind, span }
    }

    pub(super) fn tagged_literal(&mut self) -> ExprKind {
        self.trivia();
        self.builder.start_node(TAGGED_LITERAL);
        self.bump();
        let mut operands = Vec::new();
        if self.at(K::String) {
            self.bump(); // The tag decides how to interpret its payload.
        } else if self.template_scopes {
            operands.push(self.block());
        } else {
            self.sugar_group();
        }
        self.builder.finish_node();
        ExprKind::Tuple(operands)
    }
}
