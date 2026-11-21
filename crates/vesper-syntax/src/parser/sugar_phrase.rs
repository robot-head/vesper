//! Manuscript §2.2. A phrase is identified by its token shape, independently
//! of declarations and imports. Only a SugarGroup admits arbitrary tokens.
use super::*;

fn closing(open: K) -> Option<K> {
    match open {
        K::LParen => Some(K::RParen),
        K::LBracket => Some(K::RBracket),
        K::LBrace => Some(K::RBrace),
        _ => None,
    }
}

/// Calls are ambiguous only until their closing parenthesis. Index matching
/// delimiters once so lookahead through deeply nested calls remains linear.
pub(super) fn group_ends(tokens: &[Token]) -> Vec<usize> {
    let mut ends = vec![usize::MAX; tokens.len()];
    let mut open = Vec::new();
    for (i, token) in tokens.iter().enumerate() {
        if let Some(close) = closing(token.kind) {
            open.push((i, close));
        } else if matches!(token.kind, K::RParen | K::RBracket | K::RBrace)
            && let Some((start, close)) = open.pop()
        {
            if token.kind == close {
                ends[start] = i;
            } else {
                open.clear();
            }
        }
    }
    ends
}

fn head_word(kind: K) -> bool {
    matches!(kind, K::Lower | K::State | K::With | K::Linear | K::Eff)
}

impl Parser<'_> {
    pub(super) fn at_sugar_phrase(&self) -> bool {
        self.phrase_entry(false)
    }

    fn after_phrase_group(&self, open: usize) -> Option<usize> {
        let close = self.group_ends[open];
        (close != usize::MAX).then(|| {
            close
                + 1
                + self.tokens[close + 1..]
                    .iter()
                    .take_while(|t| t.kind.trivia())
                    .count()
        })
    }

    fn phrase_entry(&self, continuation: bool) -> bool {
        if self.no_struct || !head_word(self.kind()) || self.at_tagged_literal() {
            return false;
        }
        // Contextual core forms have priority at statement and expression starts.
        if (self.at_word(0, "for") && self.for_header())
            || (self.at_word(0, "loop") && self.nth(1) == K::LBrace)
            || (self.at_word(0, "region")
                && self.nth(1) == K::Lower
                && matches!(self.nth(2), K::LParen | K::LBrace))
            || (self.at_word(0, "break") && !self.bound_values.contains("break"))
            || self.at_continue()
        {
            return false;
        }

        let ghost = usize::from(self.at_word(0, "ghost") && head_word(self.nth(1)));
        let word = self.index(ghost);
        let next = self.index(ghost + 1);
        let (last, next) = if self.tokens[next].kind == K::LParen {
            let Some(after) = self.after_phrase_group(next) else {
                return false;
            };
            (self.group_ends[next], after)
        } else {
            (word, next)
        };
        let token = &self.tokens[next];
        if self.before_handler_in && token.kind == K::In {
            return false;
        }
        match token.kind {
            // Adjacent names introduce tags. A call immediately before a string
            // cannot introduce a tag, but a brace after a call must be spaced.
            K::LBrace => {
                self.tokens[last].span.end < token.span.start || (continuation && last != word)
            }
            K::String => last != word || self.tokens[last].span.end < token.span.start,
            K::Lower
            | K::Upper
            | K::Int
            | K::Float
            | K::True
            | K::False
            | K::Let
            | K::Else
            | K::In
            | K::Inout
            | K::Sink
            | K::With
            | K::State
            | K::Eff
            | K::Linear => true,
            _ => false,
        }
    }

    /// A lowercase core pattern is already complete after its identifier.
    /// Additional pattern operands, an argument list, or a spaced token group
    /// select a surface pattern; delimiters such as `in` or `=>` belong to
    /// the surrounding loop or arm.
    pub(super) fn at_pattern_phrase(&self) -> bool {
        if self.no_struct || !head_word(self.kind()) || self.at_word(0, "_") {
            return false;
        }
        match self.nth(1) {
            K::LBrace | K::String => {
                self.tokens[self.index(0)].span.end < self.tokens[self.index(1)].span.start
            }
            k => {
                head_word(k)
                    || matches!(
                        k,
                        K::Upper | K::LParen | K::Int | K::Float | K::True | K::False
                    )
            }
        }
    }

    pub(super) fn pattern_phrase(&mut self) {
        self.trivia();
        self.builder.start_node(SUGAR_PAT);
        if self.template_scopes {
            self.error("nested pattern sugar requires pattern expansion before binding analysis");
        }
        self.bump();
        loop {
            match self.kind() {
                K::LBrace => {
                    self.sugar_group();
                    break;
                }
                K::LParen => {
                    self.bump();
                    self.pattern_list(K::RParen);
                    self.expect(K::RParen);
                }
                k if head_word(k)
                    || matches!(
                        k,
                        K::Upper | K::Int | K::Float | K::String | K::True | K::False | K::Minus
                    ) =>
                {
                    let outer = self.no_struct;
                    self.no_struct = true;
                    self.pattern();
                    self.no_struct = outer;
                }
                _ => break,
            }
        }
        self.builder.finish_node();
    }

    pub(super) fn sugar_group(&mut self) {
        let Some(end) = closing(self.kind()) else {
            self.error("expected a sugar token group");
            return;
        };
        if !self.enter() {
            return;
        }
        self.trivia();
        self.builder.start_node(SUGAR_GROUP);
        self.bump();
        while !self.at(end) && !self.at(K::Eof) {
            if closing(self.kind()).is_some() {
                self.sugar_group();
            } else if matches!(self.kind(), K::RParen | K::RBracket | K::RBrace) {
                break;
            } else {
                self.bump();
            }
        }
        self.expect(end);
        self.builder.finish_node();
        self.depth -= 1;
    }

    pub(super) fn sugar_phrase(&mut self, slot: SyntaxKind) -> Expr {
        let start = self.start();
        self.trivia();
        self.builder.start_node(slot);
        if self.at_word(0, "ghost") && head_word(self.nth(1)) {
            self.bump();
        }
        self.bump(); // the head word
        let mut operands = Vec::new();
        self.sugar_heads(&mut operands);
        if self.at(K::LBrace) {
            self.phrase_body(&mut operands);
            while slot != SUGAR_ITEM
                && !self.source[self.end()..self.start()].contains(['\n', '\r'])
                && (self.at(K::Else) || self.phrase_entry(true))
            {
                self.trivia();
                self.builder.start_node(SUGAR_CONTINUATION);
                self.bump();
                self.sugar_heads(&mut operands);
                self.phrase_body(&mut operands);
                self.builder.finish_node();
            }
            if slot == SUGAR_STMT {
                self.eat(K::Semi);
            }
        } else if slot == SUGAR_STMT {
            self.expect(K::Semi);
        } else {
            self.error("expected a braced group after the sugar phrase");
        }
        self.builder.finish_node();
        // This value is only scratch space for the surrounding core parser.
        // The public surface API returns the CST and diagnostics alone.
        Expr {
            kind: if self.template_scopes {
                ExprKind::Block {
                    statements: operands,
                    tail: None,
                }
            } else {
                ExprKind::Tuple(Vec::new())
            },
            span: start..self.end(),
        }
    }

    // A word may be followed by one or more bracketed token-tree heads,
    // e.g. `scratch tmp[256]`. If a postfix or infix follows the brackets
    // they belong to a header expression instead, e.g. `guard table[i] > 0`.
    fn at_bracketed_head_word(&self) -> bool {
        if !head_word(self.kind()) || self.nth(1) != K::LBracket {
            return false;
        }
        let mut next = self.index(1);
        while self.tokens[next].kind == K::LBracket {
            let Some(after) = self.after_phrase_group(next) else {
                return false;
            };
            next = after;
        }
        let kind = self.tokens[next].kind;
        !matches!(kind, K::LParen | K::Dot | K::Question)
            && sugar_ops::core_operator(kind).is_none()
    }

    fn phrase_body(&mut self, operands: &mut Vec<Statement>) {
        if self.template_scopes {
            operands.push(Statement::Expr(self.block()));
        } else {
            self.sugar_group();
        }
    }

    fn sugar_heads(&mut self, operands: &mut Vec<Statement>) {
        while !matches!(
            self.kind(),
            K::LBrace | K::Semi | K::Eof | K::RBrace | K::RParen | K::RBracket | K::Comma
        ) {
            let before = self.pos;
            match self.kind() {
                K::Let => {
                    self.bump();
                    let pat = self.pattern();
                    self.expect(K::Eq);
                    let value = self.restricted();
                    if self.template_scopes {
                        operands.push(Statement::Let {
                            pat,
                            ty: Type::Infer,
                            value,
                            mutable: false,
                        });
                    }
                }
                kind if matches!(kind, K::In | K::Inout | K::Sink) || self.at_word(0, "out") => {
                    self.bump();
                    let old = self.no_struct;
                    self.no_struct = true;
                    let expr = self.postfix(false);
                    if self.template_scopes {
                        operands.push(Statement::Expr(expr));
                    }
                    self.no_struct = old;
                }
                K::LParen => {
                    self.bump();
                    let args = self.args(false);
                    if self.template_scopes {
                        operands.extend(args.into_iter().map(|a| Statement::Expr(a.expr)));
                    }
                    self.expect(K::RParen);
                }
                K::LBracket => {
                    if self.template_scopes {
                        self.error("raw phrase headers require typed hole matching before binding analysis");
                    }
                    self.sugar_group();
                }
                _ if self.at(K::Else) || self.at_bracketed_head_word() => {
                    self.bump();
                }
                _ => {
                    let expr = self.restricted();
                    if self.template_scopes {
                        operands.push(Statement::Expr(expr));
                    }
                }
            }
            if self.pos == before {
                break;
            }
        }
    }
}
