//! Layout constraints of the unexpanded Stage 1 CST. The ordinary formatter
//! still lays out blocks, lists and expressions inside the surface syntax.
use super::{Gap, K, Piece, Token};
use std::ops::Range;
use vesper_syntax::parser;

fn comment(k: K) -> bool {
    matches!(k, K::Comment | K::BlockComment)
}

struct Layout<'a, 'src> {
    source: &'src str,
    tokens: &'a [Token],
    // Raw lexer index of each formatter atom, including comments.
    raw: Vec<usize>,
    atoms: &'a mut [Piece<'src>],
}

impl Layout<'_, '_> {
    fn previous_code(&self, i: usize) -> Option<usize> {
        (0..i).rev().find(|&p| !comment(self.atoms[p].kind))
    }

    /// Set a same-line boundary without swallowing a line comment.
    fn join(&mut self, i: usize, gap: Gap) {
        if i > 0 && i < self.atoms.len() && self.atoms[i - 1].kind != K::Comment {
            self.atoms[i].sugar_gap = Some(gap);
        }
    }

    fn clause_boundary(&mut self, i: usize) {
        let Some(brace) = self.previous_code(i) else {
            return;
        };
        if self.atoms[brace].kind != K::RBrace {
            return;
        }
        let between = self.tokens[self.raw[brace]].span.end..self.tokens[self.raw[i]].span.start;
        if self.source[between].contains(['\n', '\r']) {
            self.atoms[i].sugar_gap = Some(Gap::Hard);
        } else {
            // A comment between the brace and head belongs on this line too.
            for j in brace + 1..=i {
                self.join(j, Gap::Space);
            }
        }
    }

    fn operator(&mut self, i: usize) -> Option<usize> {
        let raw = self.raw[i];
        let (tier, raw_end) = parser::sugar_operator(self.tokens, raw)?;
        // Operators cannot contain trivia, so their raw and atom lengths agree.
        let end = i + (raw_end - raw);
        let quoted = i > 0 && self.atoms[i - 1].kind == K::Backtick;
        self.join(
            i,
            if quoted {
                Gap::None
            } else {
                Gap::Operator(tier / 2)
            },
        );
        for j in i + 1..end {
            self.join(j, Gap::None);
        }
        if let Some(next) = self.atoms.get(end) {
            if quoted && next.kind == K::Backtick {
                self.join(end, Gap::None);
            } else if !comment(next.kind) {
                self.join(end, Gap::Space);
            }
        }
        Some(end)
    }

    // The contents of an unexpanded group have no expression AST. Preserve
    // lexical tags, operator glyphs and clause boundaries using the trivia
    // retained by its CST, also inside nested balanced groups.
    fn group(&mut self, range: Range<usize>) {
        let mut parens = Vec::new();
        let mut i = range.start;
        while i < range.end {
            let kind = self.atoms[i].kind;
            let prev = i.checked_sub(1).map(|p| self.atoms[p].kind);
            match kind {
                K::Dot | K::Pipe => {
                    if let Some(end) = self.operator(i) {
                        i = end;
                        continue;
                    }
                    // Introducing adjacency here could create a new operator.
                    if let Some(next) = self.atoms.get(i + 1)
                        && !next.glued
                        && !comment(next.kind)
                        && !matches!(next.kind, K::Lower | K::Upper | K::LBrace)
                    {
                        self.join(i + 1, Gap::Space);
                    }
                }
                K::LBrace | K::String
                    if i > range.start
                        && matches!(
                            prev,
                            Some(K::Lower | K::State | K::With | K::Eff | K::Linear)
                        ) =>
                {
                    let gap = if self.atoms[i].glued {
                        Gap::None
                    } else {
                        Gap::Space
                    };
                    self.join(i, gap);
                }
                K::Lower | K::Else => self.clause_boundary(i),
                // Template repetitions are token trees too. Keep their
                // separator and quantifier with the matching `$(...)`.
                K::LParen => parens.push(
                    self.previous_code(i)
                        .is_some_and(|p| self.atoms[p].kind == K::Dollar),
                ),
                K::RParen if parens.pop() == Some(true) => {
                    let suffix = &self.atoms[i + 1..range.end];
                    if suffix
                        .first()
                        .is_some_and(|t| matches!(t.kind, K::Star | K::Plus))
                    {
                        self.join(i + 1, Gap::None);
                    } else if suffix.len() >= 2
                        && suffix[0].text.bytes().all(|b| b.is_ascii_punctuation())
                        && matches!(suffix[1].kind, K::Star | K::Plus)
                    {
                        self.join(i + 1, Gap::None);
                        self.join(i + 2, Gap::None);
                    }
                }
                _ => {}
            }
            i += 1;
        }
    }
}

pub(super) fn annotate<'src>(
    source: &'src str,
    tokens: &[Token],
    atoms: &mut [Piece<'src>],
    nodes: &[(u16, usize, usize)],
) {
    let raw = tokens
        .iter()
        .enumerate()
        .filter_map(|(i, t)| (!matches!(t.kind, K::Whitespace | K::Eof)).then_some(i))
        .collect();
    let mut layout = Layout {
        source,
        tokens,
        raw,
        atoms,
    };
    let mut covered_group = 0;
    for &(kind, start, end) in nodes {
        if start >= end {
            continue;
        }
        if kind == parser::TAGGED_LITERAL.0 {
            layout.join(start + 1, Gap::None);
        } else if kind == parser::SUGAR_OPERATOR.0 {
            layout.operator(start);
        } else if kind == parser::SUGAR_CONTINUATION.0 {
            layout.clause_boundary(start);
        } else if kind == parser::SUGAR_STMT.0 || kind == parser::SUGAR_ITEM.0 {
            if layout
                .previous_code(start)
                .is_some_and(|p| layout.atoms[p].kind == K::RBrace)
            {
                layout.atoms[start].sugar_gap = Some(Gap::Hard);
            }
        } else if kind == parser::SUGAR_HOLE.0 {
            // In a typed hole the sort is the final token: `$name:sort`.
            if layout.atoms[end - 1].kind == K::Lower
                && end >= 2
                && layout.atoms[end - 2].kind == K::Colon
            {
                layout.join(end - 1, Gap::None);
            }
        } else if kind == parser::SUGAR_DECL.0 {
            if layout.atoms[start].kind == K::At
                && let Some(i) =
                    (start + 1..end).find(|&i| matches!(layout.atoms[i].text, "pub" | "sugar"))
            {
                layout.atoms[i].sugar_gap = Some(Gap::Hard);
            }
        } else if [
            parser::SUGAR_PATTERN,
            parser::SUGAR_TEMPLATE,
            parser::SUGAR_GROUP,
        ]
        .iter()
        .any(|k| k.0 == kind)
        {
            if kind == parser::SUGAR_PATTERN.0 {
                layout.join(start, Gap::Space);
            }
            // CST ranges are in preorder; the outer scan includes nested groups.
            if start >= covered_group {
                layout.group(start..end);
                covered_group = end;
            }
        }
    }
}
