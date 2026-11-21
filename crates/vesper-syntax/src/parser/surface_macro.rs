//! CST boundaries for fixed-profile expression macros in the surface grammar.
//! The shared macro parser validates their quoted payloads and parameters.
use super::*;

pub const SURFACE_MACRO_DECL: SyntaxKind = SyntaxKind(224);
pub const SURFACE_MACRO_CALL: SyntaxKind = SyntaxKind(225);
pub const SURFACE_MACRO_ARG: SyntaxKind = SyntaxKind(226);

impl Parser<'_> {
    fn macro_delimited_tokens(&mut self, open: K) {
        if !self.at(open) {
            self.expect(open);
            return;
        }
        let close = self.group_ends[self.index(0)];
        if close == usize::MAX {
            self.error("unclosed macro delimiter");
            self.bump();
            return;
        }
        while self.pos <= close {
            self.bump();
        }
    }

    pub(super) fn surface_macro_decl(&mut self) {
        self.trivia();
        self.builder.start_node(SURFACE_MACRO_DECL);
        self.bump();
        self.expect(K::Lower);
        self.macro_delimited_tokens(K::LParen);
        self.expect(K::Arrow);
        self.expect(K::Lower);
        self.macro_delimited_tokens(K::LBrace);
        self.builder.finish_node();
    }

    pub(super) fn surface_macro_call(&mut self) -> ExprKind {
        self.trivia();
        self.builder.start_node(SURFACE_MACRO_CALL);
        self.bump();
        self.expect(K::Bang);
        self.expect(K::LParen);
        let mut args = Vec::new();
        while !self.at(K::RParen) && !self.at(K::Eof) {
            self.trivia();
            self.builder.start_node(SURFACE_MACRO_ARG);
            args.push(self.delimited_expr());
            self.builder.finish_node();
            if !self.eat(K::Comma) {
                break;
            }
            if self.at(K::RParen) {
                self.error("expression macro arguments cannot end with a trailing comma");
            }
        }
        self.expect(K::RParen);
        self.builder.finish_node();
        ExprKind::Tuple(args)
    }
}
