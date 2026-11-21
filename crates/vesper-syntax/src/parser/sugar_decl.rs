//! Unexpanded sugar declarations (manuscript §§2.2, 7.5).
use super::*;

impl Parser<'_> {
    pub(super) fn sugar_decl(&mut self) {
        self.trivia();
        self.builder.start_node(SUGAR_DECL);
        if self.eat(K::At) {
            self.sugar_choice(&["lean_notation"], "sugar annotation");
        }
        if self.at_word(0, "pub") {
            self.bump();
        }
        self.sugar_choice(&["sugar"], "declaration keyword");
        self.lower();
        self.expect(K::Colon);
        if self.at_word(0, "infix") {
            self.sugar_infix_sort();
        } else {
            self.sugar_choice(&["stmt", "expr", "pat", "tag", "item"], "sugar sort");
        }
        if self.at_word(0, "emits") {
            self.bump();
            if self.at(K::Eff) {
                self.effects();
            } else {
                self.error("expected an effect row after `emits`");
            }
        }
        self.expect(K::LBrace);
        if !self.at_word(0, "rule") {
            self.error("a sugar declaration requires at least one rule");
        }
        while self.at_word(0, "rule") {
            self.trivia();
            self.builder.start_node(SUGAR_RULE);
            self.bump();
            self.trivia();
            self.builder.start_node(SUGAR_PATTERN);
            self.sugar_decl_group(K::LParen, true, true);
            self.builder.finish_node();
            self.expect(K::FatArrow);
            self.trivia();
            self.builder.start_node(SUGAR_TEMPLATE);
            self.sugar_decl_group(K::LBrace, false, false);
            self.builder.finish_node();
            self.builder.finish_node();
        }
        self.expect(K::RBrace);
        self.builder.finish_node();
    }

    fn sugar_choice(&mut self, choices: &[&str], description: &str) {
        if !choices.contains(&self.text(0)) {
            self.error(format!("expected {description} ({})", choices.join(", ")));
        }
        if self.at(K::Lower) {
            self.bump();
        }
    }

    fn sugar_infix_sort(&mut self) {
        self.bump();
        self.expect(K::LParen);
        self.expect(K::Backtick);
        if let Some((_, end)) = sugar_operator(&self.tokens, self.index(0)) {
            self.consume_sugar_operator(end);
        } else {
            self.error("expected a pipeline or dotted operator");
        }
        self.expect(K::Backtick);
        self.expect(K::Comma);
        self.sugar_choice(
            &[
                "pipeline",
                "logical",
                "compare",
                "additive",
                "multiplicative",
            ],
            "precedence band",
        );
        self.expect(K::Comma);
        self.sugar_choice(&["left", "right", "none"], "associativity");
        self.expect(K::RParen);
    }

    /// Both patterns and templates contain balanced groups. Templates are
    /// token trees: their contents need neither parse as core expressions nor
    /// resolve any of the pattern's metavariables at this stage.
    fn sugar_decl_group(&mut self, open: K, pattern: bool, nonempty: bool) {
        if !self.at(open) {
            self.expect(open);
            return;
        }
        if !self.enter() {
            return;
        }
        self.bump();
        let close = match open {
            K::LParen => K::RParen,
            K::LBracket => K::RBracket,
            K::LBrace => K::RBrace,
            _ => unreachable!("only opening delimiters start a sugar group"),
        };
        if nonempty && self.at(close) {
            self.error("empty sugar pattern or repetition");
        }
        loop {
            let kind = self.kind();
            match kind {
                K::Eof | K::RParen | K::RBracket | K::RBrace => break,
                K::LParen | K::LBracket | K::LBrace => self.sugar_decl_group(kind, pattern, false),
                K::Dollar if pattern => self.sugar_hole(false),
                _ if pattern
                    && (matches!(kind, K::In | K::Inout | K::Sink) || self.at_word(0, "out"))
                    && self.nth(1) == K::Dollar =>
                {
                    self.sugar_hole(true)
                }
                _ => {
                    self.bump();
                }
            }
        }
        self.expect(close);
        self.depth -= 1;
    }

    fn sugar_hole(&mut self, pass: bool) {
        self.trivia();
        self.builder.start_node(SUGAR_HOLE);
        if pass {
            self.bump();
        }
        self.expect(K::Dollar);
        if self.at(K::LParen) {
            if pass {
                self.error("a repetition cannot have a pass mode");
            }
            self.sugar_decl_group(K::LParen, true, true);
            if !matches!(self.kind(), K::Plus | K::Star)
                && matches!(self.nth(1), K::Plus | K::Star)
                && !self.at(K::Eof)
                && self
                    .text(0)
                    .bytes()
                    .all(|b| b.is_ascii_punctuation() && !b"()[]{}\"`".contains(&b))
            {
                self.bump();
            }
            if !(self.eat(K::Star) || self.eat(K::Plus)) {
                self.error("expected `*` or `+` after a sugar repetition");
            }
        } else {
            let sequence = self.eat(K::DotDot);
            if sequence && pass {
                self.error("a sequence hole cannot have a pass mode");
            }
            // Constructor placeholders in the morph example use $Src / $Dst.
            if matches!(self.kind(), K::Lower | K::Upper) {
                self.bump();
            } else {
                self.error("expected a sugar hole name");
            }
            self.expect(K::Colon);
            if sequence {
                self.sugar_choice(
                    &["stmts", "scope", "args", "items", "pats"],
                    "sequence hole sort",
                );
            } else {
                self.sugar_choice(
                    &["ident", "pat", "ty", "expr", "place", "block", "scope"],
                    "hole sort",
                );
            }
        }
        self.builder.finish_node();
    }
}
