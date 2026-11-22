//! Validate original macro spans in a parsed Stage 1 module. Complete sugar
//! arguments remain available to enforestation; the caller validates their
//! emitted expressions with the ordinary macro compiler after lowering.
use super::*;
use crate::parser::{self, SURFACE_MACRO_ARG, SURFACE_MACRO_CALL, SURFACE_MACRO_DECL, SyntaxNode};

fn bytes(node: &SyntaxNode) -> Span {
    node.text_range().start().into()..node.text_range().end().into()
}

impl SourceParser<'_> {
    fn significant_range(&self, bytes: Span) -> Span {
        let at = |offset| {
            self.significant
                .partition_point(|&i| self.tokens[i].span.start < offset)
        };
        at(bytes.start)..at(bytes.end)
    }

    fn check_arity(&mut self, name: Span, arity: Option<usize>) -> Result<(), Error> {
        for def in &self.definitions {
            self.budget
                .work(add(name.len(), def.name_span.len(), Resource::Work)?)?;
            if self.source[name.clone()] == self.source[def.name_span.clone()] {
                if arity.is_some_and(|n| n != def.parameters.len()) {
                    return Err(Error::new(ErrorKind::Arity, Some(name)));
                }
                return Ok(());
            }
        }
        Err(Error::new(ErrorKind::UnknownMacro, Some(name)))
    }

    fn contains_call(&self, range: Span) -> bool {
        self.significant[range].windows(2).any(|w| {
            matches!(self.tokens[w[0]].kind, K::Lower | K::Upper)
                && self.tokens[w[1]].kind == K::Bang
        })
    }

    // A template is a token tree, so an argument can be a metavariable or
    // an argument-sequence splice. Resolve the spelling and all statically
    // known arities before allowing an expression macro through this boundary.
    fn template_calls(&mut self, range: Span, count: &mut usize) -> Result<(), Error> {
        self.pos = range.start;
        while self.pos + 1 < range.end {
            if self.kind() != K::Lower
                || self.tokens[self.significant[self.pos + 1]].kind != K::Bang
                || (self.pos > range.start
                    && self.tokens[self.significant[self.pos - 1]].kind == K::Dollar)
            {
                self.bump()?;
                continue;
            }
            *count += 1;
            bound(
                *count,
                self.budget.limits.max_invocations,
                Resource::Invocations,
            )?;
            let name = self.bump()?;
            self.expect(K::Bang)?;
            self.expect(K::LParen)?;
            let mut arity = 0;
            let mut splice = false;
            while self.kind() != K::RParen {
                let start = self.pos;
                while self.pos < range.end && !matches!(self.kind(), K::Comma | K::RParen) {
                    if self.kind() == K::Dollar
                        && matches!(
                            self.tokens[self.significant[self.pos + 1]].kind,
                            K::DotDot | K::LParen
                        )
                    {
                        splice = true;
                    }
                    if matches!(self.kind(), K::LParen | K::LBrace | K::LBracket) {
                        self.pos = self.matching(self.pos)? + 1;
                    } else {
                        self.bump()?;
                    }
                }
                if self.pos == start || self.pos >= range.end || self.contains_call(start..self.pos)
                {
                    return Err(self.fail(ErrorKind::Unsupported));
                }
                arity += 1;
                bound(
                    arity,
                    self.budget.limits.max_parameters,
                    Resource::Parameters,
                )?;
                if self.kind() != K::Comma {
                    break;
                }
                self.bump()?;
                if self.kind() == K::RParen {
                    return Err(self.fail(ErrorKind::Syntax));
                }
            }
            self.expect(K::RParen)?;
            self.check_arity(name, (!splice).then_some(arity))?;
        }
        Ok(())
    }

    fn surface_call(&mut self, node: &SyntaxNode) -> Result<(), Error> {
        let start = self.significant_range(bytes(node)).start;
        let name = self.tokens[self.significant[start]].span.clone();
        let mut arity = 0;
        for arg in node.children().filter(|n| n.kind() == SURFACE_MACRO_ARG) {
            arity += 1;
            bound(
                arity,
                self.budget.limits.max_parameters,
                Resource::Parameters,
            )?;
            let range = self.significant_range(bytes(&arg));
            self.budget.work(range.len())?;
            if self.contains_call(range.clone()) {
                return Err(Error::new(ErrorKind::Unsupported, Some(bytes(&arg))));
            }
            if !arg.descendants().any(|n| parser::is_sugar_node(n.kind())) {
                self.pos = range.start;
                self.expr(ExprMode::Argument, 1)?;
                if self.pos != range.end {
                    return Err(Error::new(ErrorKind::Unsupported, Some(bytes(&arg))));
                }
            }
        }
        self.check_arity(name, Some(arity))
    }
}

pub(crate) fn validate_surface(
    source: &str,
    tokens: Vec<Token>,
    syntax: &SyntaxNode,
    limits: Limits,
) -> Result<(), Error> {
    let macro_node = |n: &SyntaxNode| matches!(n.kind(), SURFACE_MACRO_DECL | SURFACE_MACRO_CALL);
    if !syntax.descendants().any(|n| macro_node(&n)) {
        return Ok(());
    }
    limits.preflight(source)?;
    bound(tokens.len(), limits.max_tokens, Resource::Tokens)?;
    let mut budget = Budget {
        work: 0,
        metadata: 0,
        limits,
    };
    budget.work(source.len())?;
    budget.metadata(mul(tokens.len(), size_of::<usize>(), Resource::Metadata)?)?;
    let mut p = SourceParser::new(source, tokens, budget)?;
    // A forward invocation resolves just like one after its declaration.
    for node in syntax
        .descendants()
        .filter(|n| n.kind() == SURFACE_MACRO_DECL)
    {
        let range = p.significant_range(bytes(&node));
        p.pos = range.start;
        p.push_def()?;
        if p.pos != range.end {
            return Err(p.fail(ErrorKind::Syntax));
        }
    }
    let mut count = 0;
    for node in syntax.descendants() {
        if node.kind() == SURFACE_MACRO_CALL {
            count += 1;
            bound(count, limits.max_invocations, Resource::Invocations)?;
            p.surface_call(&node)?;
        } else if node.kind() == parser::SUGAR_TEMPLATE && !p.definitions.is_empty() {
            p.template_calls(p.significant_range(bytes(&node)), &mut count)?;
        }
    }
    Ok(())
}
