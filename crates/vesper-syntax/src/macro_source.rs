//! Strict actual-source Tier1 expression macro syntax. No checking or execution.
use crate::{
    ast::{Effects, Generic, Module, Param, Span, Type},
    lexer::{self, Token, TokenKind as K},
};
use std::{fmt, mem::size_of, sync::Arc};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub max_source_bytes: usize,
    pub max_tokens: usize,
    pub max_macros: usize,
    pub max_functions: usize,
    pub max_invocations: usize,
    /// Macro slots and plain parameters of macro-bearing functions. Ordinary
    /// support-function parameters retain their existing source/token bounds.
    pub max_parameters: usize,
    pub max_depth: usize,
    pub max_metadata_bytes: usize,
    pub max_work: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_source_bytes: 128 * 1024,
            max_tokens: 16384,
            max_macros: 32,
            max_functions: 128,
            max_invocations: 256,
            max_parameters: 8,
            max_depth: 48,
            max_metadata_bytes: 1024 * 1024,
            max_work: 16 * 1024 * 1024,
        }
    }
}
impl Limits {
    pub const HARD: Self = Self {
        max_source_bytes: 1024 * 1024,
        max_tokens: 65536,
        max_macros: 128,
        max_functions: 1024,
        max_invocations: 4096,
        max_parameters: 32,
        max_depth: 128,
        max_metadata_bytes: 16 * 1024 * 1024,
        max_work: 128 * 1024 * 1024,
    };
    pub fn validate(self) -> Result<(), Error> {
        let h = Self::HARD;
        let a = [
            self.max_source_bytes,
            self.max_tokens,
            self.max_macros,
            self.max_functions,
            self.max_invocations,
            self.max_parameters,
            self.max_depth,
            self.max_metadata_bytes,
            self.max_work,
        ];
        let b = [
            h.max_source_bytes,
            h.max_tokens,
            h.max_macros,
            h.max_functions,
            h.max_invocations,
            h.max_parameters,
            h.max_depth,
            h.max_metadata_bytes,
            h.max_work,
        ];
        if self.max_tokens == 0 || self.max_depth == 0 || a.into_iter().zip(b).any(|(a, b)| a > b) {
            Err(Error::new(ErrorKind::InvalidLimits, None))
        } else {
            Ok(())
        }
    }
    pub fn preflight(self, source: &str) -> Result<(), Error> {
        self.validate()?;
        bound(source.len(), self.max_source_bytes, Resource::Source)?;
        bound(source.len(), self.max_work, Resource::Work)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resource {
    Source,
    Tokens,
    Macros,
    Functions,
    Invocations,
    Parameters,
    Depth,
    Metadata,
    Work,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    InvalidLimits,
    Budget(Resource),
    Lexical,
    Syntax,
    Unsupported,
    UnknownMacro,
    Arity,
    DuplicateName,
    ForeignId,
    Allocation,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    pub kind: ErrorKind,
    pub span: Option<Span>,
}
impl Error {
    pub(crate) fn new(kind: ErrorKind, span: Option<Span>) -> Self {
        Self { kind, span }
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self.kind {
            ErrorKind::InvalidLimits => "invalid source macro limits",
            ErrorKind::Budget(_) => "source macro budget exceeded",
            ErrorKind::Lexical => "invalid source macro lexical syntax",
            ErrorKind::Syntax => "invalid source macro syntax",
            ErrorKind::Unsupported => "unsupported source macro category or context",
            ErrorKind::UnknownMacro => "unknown source macro",
            ErrorKind::Arity => "source macro arity mismatch",
            ErrorKind::DuplicateName => "duplicate source macro name or parameter",
            ErrorKind::ForeignId => "foreign source macro identity",
            ErrorKind::Allocation => "source macro allocation failed",
        })
    }
}
impl std::error::Error for Error {}
macro_rules! identity {
    ($name:ident) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub struct $name {
            source_hash: [u8; 32],
            index: usize,
        }
        impl $name {
            pub fn index(self) -> usize {
                self.index
            }
            pub fn source_hash(self) -> [u8; 32] {
                self.source_hash
            }
        }
    };
}
identity!(MacroId);
identity!(FunctionId);
identity!(InvocationId);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    Expr,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MacroParameter {
    pub name_span: Span,
    pub span: Span,
    pub category: Category,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MacroDefinition {
    pub id: MacroId,
    pub name_span: Span,
    pub parameters: Vec<MacroParameter>,
    pub quote_span: Span,
    pub span: Span,
}
/// A real separately parsed header, never a Function with a placeholder body.
#[derive(Clone, Debug, PartialEq)]
pub struct FunctionHeader {
    pub name: String,
    pub name_span: Span,
    pub generics: Vec<Generic>,
    pub params: Vec<Param>,
    pub result: Type,
    pub effects: Effects,
    pub span: Span,
}
#[derive(Clone, Debug, PartialEq)]
pub struct SourceFunction {
    pub id: FunctionId,
    pub header: FunctionHeader,
    pub body_span: Span,
    pub span: Span,
    pub invocations: Vec<InvocationId>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invocation {
    pub id: InvocationId,
    pub definition: MacroId,
    pub function: FunctionId,
    pub name_span: Span,
    pub span: Span,
    pub arguments: Vec<Span>,
}
/// Privately parsed actual source/tokens and syntax-only identities. Public record
/// copies cannot alter this snapshot or authorize a checked program.
/// ```compile_fail
/// use vesper_syntax::macro_source::MacroSource;
/// let fabricated = MacroSource {};
/// ```
#[derive(Debug)]
pub struct MacroSource {
    source: Arc<str>,
    source_hash: [u8; 32],
    tokens: Vec<Token>,
    definitions: Vec<MacroDefinition>,
    functions: Vec<SourceFunction>,
    parameter_names: Vec<Vec<Span>>,
    invocations: Vec<Invocation>,
    support_items: Module,
}
impl MacroSource {
    pub fn source(&self) -> &str {
        &self.source
    }
    pub fn source_hash(&self) -> [u8; 32] {
        self.source_hash
    }
    pub fn tokens(&self) -> &[Token] {
        &self.tokens
    }
    pub fn definitions(&self) -> &[MacroDefinition] {
        &self.definitions
    }
    pub fn functions(&self) -> &[SourceFunction] {
        &self.functions
    }
    pub fn invocations(&self) -> &[Invocation] {
        &self.invocations
    }
    pub fn support_items(&self) -> &Module {
        &self.support_items
    }
    pub fn definition(&self, id: MacroId) -> Result<&MacroDefinition, Error> {
        if id.source_hash != self.source_hash {
            return Err(Error::new(ErrorKind::ForeignId, None));
        }
        self.definitions
            .get(id.index)
            .ok_or_else(|| Error::new(ErrorKind::ForeignId, None))
    }
    pub fn function(&self, id: FunctionId) -> Result<&SourceFunction, Error> {
        if id.source_hash != self.source_hash {
            return Err(Error::new(ErrorKind::ForeignId, None));
        }
        self.functions
            .get(id.index)
            .ok_or_else(|| Error::new(ErrorKind::ForeignId, None))
    }
    /// Actual header identifier tokens, in parameter order. The inventory is
    /// sealed with this source; changing a public header clone cannot change it.
    pub fn parameter_name_spans(&self, id: FunctionId) -> Result<&[Span], Error> {
        self.function(id)?;
        self.parameter_names
            .get(id.index)
            .map(Vec::as_slice)
            .ok_or_else(|| Error::new(ErrorKind::ForeignId, None))
    }
    pub fn invocation(&self, id: InvocationId) -> Result<&Invocation, Error> {
        if id.source_hash != self.source_hash {
            return Err(Error::new(ErrorKind::ForeignId, None));
        }
        self.invocations
            .get(id.index)
            .ok_or_else(|| Error::new(ErrorKind::ForeignId, None))
    }
}
fn bound(n: usize, cap: usize, r: Resource) -> Result<(), Error> {
    if n > cap {
        Err(Error::new(ErrorKind::Budget(r), None))
    } else {
        Ok(())
    }
}
fn add(a: usize, b: usize, r: Resource) -> Result<usize, Error> {
    a.checked_add(b)
        .ok_or_else(|| Error::new(ErrorKind::Budget(r), None))
}
fn mul(a: usize, b: usize, r: Resource) -> Result<usize, Error> {
    a.checked_mul(b)
        .ok_or_else(|| Error::new(ErrorKind::Budget(r), None))
}
struct Budget {
    work: usize,
    metadata: usize,
    limits: Limits,
}
impl Budget {
    fn work(&mut self, n: usize) -> Result<(), Error> {
        self.work = add(self.work, n, Resource::Work)?;
        bound(self.work, self.limits.max_work, Resource::Work)
    }
    fn metadata(&mut self, n: usize) -> Result<(), Error> {
        self.metadata = add(self.metadata, n, Resource::Metadata)?;
        bound(
            self.metadata,
            self.limits.max_metadata_bytes,
            Resource::Metadata,
        )
    }
    fn stage(&mut self, tokens: usize, bytes: usize) -> Result<(), Error> {
        let depth = self.limits.max_depth;
        self.metadata(add(
            mul(
                mul(tokens, 64, Resource::Metadata)?,
                depth,
                Resource::Metadata,
            )?,
            mul(
                mul(bytes, 2, Resource::Metadata)?,
                depth,
                Resource::Metadata,
            )?,
            Resource::Metadata,
        )?)?;
        self.work(add(
            mul(mul(tokens, 64, Resource::Work)?, depth, Resource::Work)?,
            mul(bytes, 8, Resource::Work)?,
            Resource::Work,
        )?)
    }
}
#[derive(Clone, Copy)]
enum ExprMode {
    Quote(usize),
    Caller(usize),
    Argument,
}
struct SourceParser<'a> {
    source: &'a str,
    hash: [u8; 32],
    tokens: Vec<Token>,
    significant: Vec<usize>,
    pos: usize,
    definitions: Vec<MacroDefinition>,
    functions: Vec<SourceFunction>,
    parameter_names: Vec<Vec<Span>>,
    invocations: Vec<Invocation>,
    support_items: Module,
    budget: Budget,
}
impl SourceParser<'_> {
    fn token(&self) -> &Token {
        &self.tokens[self.significant[self.pos]]
    }
    fn kind(&self) -> K {
        self.token().kind
    }
    fn span(&self) -> Span {
        self.token().span.clone()
    }
    fn text(&self) -> &str {
        &self.source[self.span()]
    }
    fn fail(&self, kind: ErrorKind) -> Error {
        Error::new(kind, Some(self.span()))
    }
    fn word(&self, s: &str) -> bool {
        self.kind() == K::Lower && self.text() == s
    }
    fn bump(&mut self) -> Result<Span, Error> {
        self.budget.work(1)?;
        let span = self.span();
        if self.kind() != K::Eof {
            self.pos += 1;
        }
        Ok(span)
    }
    fn expect(&mut self, k: K) -> Result<Span, Error> {
        if self.kind() != k {
            return Err(self.fail(ErrorKind::Syntax));
        }
        self.bump()
    }
    fn word_expect(&mut self, s: &str) -> Result<Span, Error> {
        if !self.word(s) {
            return Err(self.fail(ErrorKind::Syntax));
        }
        self.bump()
    }
    fn end(&self) -> usize {
        self.tokens[self.significant[self.pos.saturating_sub(1)]]
            .span
            .end
    }
    fn push_def(&mut self) -> Result<(), Error> {
        bound(
            self.definitions.len() + 1,
            self.budget.limits.max_macros,
            Resource::Macros,
        )?;
        self.budget.metadata(size_of::<MacroDefinition>())?;
        let start = self.word_expect("macro")?.start;
        let name_span = self.expect(K::Lower)?;
        for definition in &self.definitions {
            self.budget.work(add(
                definition.name_span.len(),
                name_span.len(),
                Resource::Work,
            )?)?;
            if self.source[definition.name_span.clone()] == self.source[name_span.clone()] {
                return Err(Error::new(ErrorKind::DuplicateName, Some(name_span)));
            }
        }
        self.expect(K::LParen)?;
        let mut parameters: Vec<MacroParameter> = Vec::new();
        while self.kind() != K::RParen {
            bound(
                parameters.len() + 1,
                self.budget.limits.max_parameters,
                Resource::Parameters,
            )?;
            self.budget.metadata(size_of::<MacroParameter>())?;
            let start = self.expect(K::Dollar)?.start;
            let name_span = self.expect(K::Lower)?;
            self.expect(K::Colon)?;
            let end = self.word_expect("expr")?.end;
            for parameter in &parameters {
                self.budget.work(add(
                    parameter.name_span.len(),
                    name_span.len(),
                    Resource::Work,
                )?)?;
                if self.source[parameter.name_span.clone()] == self.source[name_span.clone()] {
                    return Err(Error::new(ErrorKind::DuplicateName, Some(name_span)));
                }
            }
            parameters
                .try_reserve(1)
                .map_err(|_| self.fail(ErrorKind::Allocation))?;
            parameters.push(MacroParameter {
                name_span,
                span: start..end,
                category: Category::Expr,
            });
            if self.kind() != K::Comma {
                break;
            }
            self.bump()?;
            if self.kind() == K::RParen {
                return Err(self.fail(ErrorKind::Syntax));
            }
        }
        self.expect(K::RParen)?;
        self.expect(K::Arrow)?;
        self.word_expect("expr")?;
        self.expect(K::LBrace)?;
        self.word_expect("quote")?;
        let inner_start = self.expect(K::LBrace)?.end;
        let index = self.definitions.len();
        self.definitions
            .try_reserve(1)
            .map_err(|_| self.fail(ErrorKind::Allocation))?;
        self.definitions.push(MacroDefinition {
            id: MacroId {
                source_hash: self.hash,
                index,
            },
            name_span,
            parameters,
            quote_span: inner_start..inner_start,
            span: start..start,
        });
        self.expr(ExprMode::Quote(index), 1)?;
        let close = self.expect(K::RBrace)?;
        self.definitions[index].quote_span = inner_start..close.start;
        let end = self.expect(K::RBrace)?.end;
        self.definitions[index].span = start..end;
        Ok(())
    }
    fn matching(&mut self, start: usize) -> Result<usize, Error> {
        let mut stack = Vec::new();
        self.budget.metadata(mul(
            self.budget.limits.max_depth,
            size_of::<K>(),
            Resource::Metadata,
        )?)?;
        stack
            .try_reserve_exact(self.budget.limits.max_depth)
            .map_err(|_| self.fail(ErrorKind::Allocation))?;
        for at in start..self.significant.len() {
            self.budget.work(1)?;
            let k = self.tokens[self.significant[at]].kind;
            match k {
                K::LParen | K::LBrace | K::LBracket => {
                    bound(
                        stack.len() + 1,
                        self.budget.limits.max_depth,
                        Resource::Depth,
                    )?;
                    stack.push(k)
                }
                K::RParen | K::RBrace | K::RBracket => {
                    let open = stack.pop().ok_or_else(|| self.fail(ErrorKind::Syntax))?;
                    if !matches!(
                        (open, k),
                        (K::LParen, K::RParen)
                            | (K::LBrace, K::RBrace)
                            | (K::LBracket, K::RBracket)
                    ) {
                        return Err(self.fail(ErrorKind::Syntax));
                    }
                    if stack.is_empty() {
                        return Ok(at);
                    }
                }
                K::Eof => break,
                _ => {}
            }
        }
        Err(self.fail(ErrorKind::Syntax))
    }
    fn function(&mut self) -> Result<(), Error> {
        bound(
            self.functions.len() + 1,
            self.budget.limits.max_functions,
            Resource::Functions,
        )?;
        self.budget.metadata(add(
            size_of::<SourceFunction>(),
            size_of::<Vec<Span>>(),
            Resource::Metadata,
        )?)?;
        let begin = self.pos;
        let mut body = self.pos + 1;
        while body < self.significant.len() {
            self.budget.work(1)?;
            match self.tokens[self.significant[body]].kind {
                K::LParen | K::LBracket => {
                    body = self.matching(body)? + 1;
                }
                K::LBrace => break,
                K::Eof | K::Semi => return Err(self.fail(ErrorKind::Syntax)),
                _ => body += 1,
            }
        }
        if body >= self.significant.len() {
            return Err(self.fail(ErrorKind::Syntax));
        }
        let close = self.matching(body)?;
        let start_byte = self.tokens[self.significant[begin]].span.start;
        let body_start = self.tokens[self.significant[body]].span.start;
        let end = self.tokens[self.significant[close]].span.end;
        let first = self.significant[begin];
        let header_end = self.significant[body] + 1;
        self.budget
            .stage(header_end - first, body_start - start_byte)?;
        let header = crate::parser::parse_source_header(
            self.source,
            &self.tokens,
            first..header_end,
            self.budget.limits,
        )?;
        let has_macro = (body..close).any(|i| {
            self.tokens[self.significant[i]].kind == K::Bang
                && i > body
                && self.tokens[self.significant[i - 1]].kind == K::Lower
        });
        let function_index = self.functions.len();
        let invocation_start = self.invocations.len();
        if has_macro {
            self.budget.work(header.params.len())?;
            if !header.generics.is_empty()
                || header.params.iter().any(|p| !p.projections.is_empty())
            {
                return Err(self.fail(ErrorKind::Unsupported));
            }
            bound(
                header.params.len(),
                self.budget.limits.max_parameters,
                Resource::Parameters,
            )?;
            // Reject original duplicate names before any later alpha-renaming
            // could turn an invalid header into distinct valid bindings.
            for (i, param) in header.params.iter().enumerate() {
                for previous in &header.params[..i] {
                    self.budget.work(add(
                        param.name.len(),
                        previous.name.len(),
                        Resource::Work,
                    )?)?;
                    if param.name == previous.name {
                        return Err(Error::new(
                            ErrorKind::DuplicateName,
                            Some(param.span.clone()),
                        ));
                    }
                }
            }
        }
        let parameter_names = self.parameter_names(&header, begin..body)?;
        if has_macro {
            self.pos = body;
            self.expr(ExprMode::Caller(function_index), 1)?;
            if self.pos != close + 1 {
                return Err(self.fail(ErrorKind::Syntax));
            }
        } else {
            self.budget
                .stage(self.significant[close] + 1 - first, end - start_byte)?;
            let module = crate::parser::parse_source_item(
                self.source,
                &self.tokens,
                first..self.significant[close] + 1,
                self.budget.limits,
            )?;
            self.support_items.items.extend(module.items);
            self.support_items.model_uses.extend(module.model_uses);
            self.support_items.trait_bounds.extend(module.trait_bounds);
            self.pos = close + 1;
        }
        let mut ids = Vec::new();
        let count = self.invocations.len() - invocation_start;
        self.budget
            .metadata(mul(count, size_of::<InvocationId>(), Resource::Metadata)?)?;
        ids.try_reserve_exact(count)
            .map_err(|_| self.fail(ErrorKind::Allocation))?;
        ids.extend(self.invocations[invocation_start..].iter().map(|i| i.id));
        self.functions
            .try_reserve(1)
            .map_err(|_| self.fail(ErrorKind::Allocation))?;
        self.budget.work(mul(
            add(self.parameter_names.len(), 1, Resource::Work)?,
            size_of::<Vec<Span>>(),
            Resource::Work,
        )?)?;
        self.parameter_names
            .try_reserve_exact(1)
            .map_err(|_| self.fail(ErrorKind::Allocation))?;
        self.parameter_names.push(parameter_names);
        self.functions.push(SourceFunction {
            id: FunctionId {
                source_hash: self.hash,
                index: function_index,
            },
            header,
            body_span: body_start..end,
            span: start_byte..end,
            invocations: ids,
        });
        Ok(())
    }
    fn parameter_names(
        &mut self,
        header: &FunctionHeader,
        significant_header: Span,
    ) -> Result<Vec<Span>, Error> {
        // One monotone pass over this bounded header, plus exact name checks.
        // Reserve the entire scan and retained-span envelope before traversing
        // tokens or allocating the inventory. Parsed header storage was already
        // covered by stage() before invoking the actual header parser.
        self.budget.work(add(
            significant_header.len(),
            mul(header.params.len(), 4, Resource::Work)?,
            Resource::Work,
        )?)?;
        self.budget.metadata(mul(
            header.params.len(),
            size_of::<Span>(),
            Resource::Metadata,
        )?)?;
        let mut names = Vec::new();
        names
            .try_reserve_exact(header.params.len())
            .map_err(|_| self.fail(ErrorKind::Allocation))?;
        let mut cursor = significant_header.start;
        let mut previous_end = header.span.start;
        for param in &header.params {
            if param.span.start < previous_end
                || param.span.end > header.span.end
                || self.source.get(param.span.clone()).is_none()
            {
                return Err(Error::new(ErrorKind::Syntax, Some(param.span.clone())));
            }
            previous_end = param.span.end;
            while cursor < significant_header.end
                && self.tokens[self.significant[cursor]].span.start < param.span.start
            {
                cursor += 1;
            }
            if cursor < significant_header.end
                && matches!(
                    self.tokens[self.significant[cursor]].kind,
                    K::In | K::Inout | K::Sink
                )
            {
                cursor += 1;
            }
            let token = (cursor < significant_header.end)
                .then(|| &self.tokens[self.significant[cursor]])
                .ok_or_else(|| Error::new(ErrorKind::Syntax, Some(param.span.clone())))?;
            self.budget
                .work(add(token.span.len(), param.name.len(), Resource::Work)?)?;
            if !matches!(token.kind, K::Lower | K::SelfKw)
                || token.span.start < param.span.start
                || token.span.end > param.span.end
                || self.source.get(token.span.clone()) != Some(param.name.as_str())
            {
                return Err(Error::new(ErrorKind::Syntax, Some(param.span.clone())));
            }
            names.push(token.span.clone());
            cursor += 1;
        }
        Ok(names)
    }
    fn support(&mut self) -> Result<(), Error> {
        if matches!(self.kind(), K::Copy | K::Affine | K::Linear)
            && !matches!(
                self.tokens[self.significant[self.pos + 1]].kind,
                K::Struct | K::Enum | K::Effect
            )
        {
            return Err(self.fail(ErrorKind::Unsupported));
        }
        let begin = self.pos;
        let mut at = self.pos;
        while at < self.significant.len() {
            self.budget.work(1)?;
            match self.tokens[self.significant[at]].kind {
                K::LParen | K::LBracket => at = self.matching(at)? + 1,
                K::LBrace => {
                    let end = self.matching(at)?;
                    let first = self.significant[begin];
                    let last = self.significant[end] + 1;
                    let bytes = self.tokens[last - 1].span.end - self.tokens[first].span.start;
                    self.budget.stage(last - first, bytes)?;
                    let module = crate::parser::parse_source_item(
                        self.source,
                        &self.tokens,
                        first..last,
                        self.budget.limits,
                    )?;
                    self.support_items.items.extend(module.items);
                    self.support_items.model_uses.extend(module.model_uses);
                    self.support_items.trait_bounds.extend(module.trait_bounds);
                    self.pos = end + 1;
                    return Ok(());
                }
                K::Eof | K::Semi => return Err(self.fail(ErrorKind::Unsupported)),
                _ => at += 1,
            }
        }
        Err(self.fail(ErrorKind::Syntax))
    }
    fn expr(&mut self, mode: ExprMode, depth: usize) -> Result<(), Error> {
        bound(depth, self.budget.limits.max_depth, Resource::Depth)?;
        self.budget.work(1)?;
        let name_start = self.pos;
        let mut simple_name = false;
        match self.kind() {
            K::Lower | K::Upper | K::SelfKw => {
                self.bump()?;
                simple_name = true;
                while self.kind() == K::ColonColon {
                    simple_name = false;
                    self.bump()?;
                    if !matches!(self.kind(), K::Lower | K::Upper | K::SelfKw) {
                        return Err(self.fail(ErrorKind::Syntax));
                    }
                    self.bump()?;
                }
            }
            K::Int | K::String | K::True | K::False => {
                self.bump()?;
            }
            K::Dollar => {
                let ExprMode::Quote(definition) = mode else {
                    return Err(self.fail(ErrorKind::Unsupported));
                };
                self.bump()?;
                let name = self.expect(K::Lower)?;
                let mut found = false;
                for parameter in &self.definitions[definition].parameters {
                    self.budget.work(add(
                        parameter.name_span.len(),
                        name.len(),
                        Resource::Work,
                    )?)?;
                    if self.source[parameter.name_span.clone()] == self.source[name.clone()] {
                        found = true;
                        break;
                    }
                }
                if !found {
                    return Err(Error::new(ErrorKind::Syntax, Some(name)));
                }
            }
            K::LParen => {
                self.bump()?;
                // Stream the actual values without constructing a second AST
                // or element vector. Tuple arity is bounded by the existing
                // source/token/work limits, independently of macro slot count.
                // An empty list is unit; one value without a comma is grouping.
                if self.kind() != K::RParen {
                    self.expr(mode, depth + 1)?;
                    while self.kind() == K::Comma {
                        self.bump()?;
                        if self.kind() == K::RParen {
                            break;
                        }
                        self.expr(mode, depth + 1)?;
                    }
                }
                self.expect(K::RParen)?;
            }
            K::LBrace => {
                self.bump()?;
                while self.kind() != K::RBrace {
                    if self.kind() == K::Let {
                        self.bump()?;
                        self.expect(K::Lower)?;
                        self.expect(K::Eq)?;
                        self.expr(mode, depth + 1)?;
                        self.expect(K::Semi)?;
                    } else {
                        self.expr(mode, depth + 1)?;
                        if self.kind() == K::Semi {
                            self.bump()?;
                        } else {
                            break;
                        }
                    }
                }
                self.expect(K::RBrace)?;
            }
            _ => return Err(self.fail(ErrorKind::Unsupported)),
        }
        if self.kind() == K::Bang {
            let ExprMode::Caller(function) = mode else {
                return Err(self.fail(ErrorKind::Unsupported));
            };
            if !simple_name {
                return Err(self.fail(ErrorKind::Unsupported));
            }
            bound(
                self.invocations.len() + 1,
                self.budget.limits.max_invocations,
                Resource::Invocations,
            )?;
            self.budget.metadata(size_of::<Invocation>())?;
            let name_span = self.tokens[self.significant[name_start]].span.clone();
            self.bump()?;
            self.expect(K::LParen)?;
            let mut arguments = Vec::new();
            while self.kind() != K::RParen {
                bound(
                    arguments.len() + 1,
                    self.budget.limits.max_parameters,
                    Resource::Parameters,
                )?;
                self.budget.metadata(size_of::<Span>())?;
                let start = self.span().start;
                self.expr(ExprMode::Argument, depth + 1)?;
                let end = self.end();
                arguments
                    .try_reserve(1)
                    .map_err(|_| self.fail(ErrorKind::Allocation))?;
                arguments.push(start..end);
                if self.kind() != K::Comma {
                    break;
                }
                self.bump()?;
                if self.kind() == K::RParen {
                    return Err(self.fail(ErrorKind::Syntax));
                }
            }
            let end = self.expect(K::RParen)?.end;
            let index = self.invocations.len();
            self.invocations
                .try_reserve(1)
                .map_err(|_| self.fail(ErrorKind::Allocation))?;
            self.invocations.push(Invocation {
                id: InvocationId {
                    source_hash: self.hash,
                    index,
                },
                definition: MacroId {
                    source_hash: self.hash,
                    index: usize::MAX,
                },
                function: FunctionId {
                    source_hash: self.hash,
                    index: function,
                },
                span: name_span.start..end,
                name_span,
                arguments,
            });
        }
        while self.kind() == K::LParen {
            self.bump()?;
            let mut count = 0;
            while self.kind() != K::RParen {
                count += 1;
                bound(
                    count,
                    self.budget.limits.max_parameters,
                    Resource::Parameters,
                )?;
                self.expr(mode, depth + 1)?;
                if self.kind() != K::Comma {
                    break;
                }
                self.bump()?;
                if self.kind() == K::RParen {
                    return Err(self.fail(ErrorKind::Syntax));
                }
            }
            self.expect(K::RParen)?;
        }
        Ok(())
    }
}
/// Parse actual original bytes. No macro expansion, checker, World, I/O or CST
/// claim is made; complete support AST items retain their original absolute spans.
pub fn parse(source: &str, limits: Limits) -> Result<MacroSource, Error> {
    limits.preflight(source)?;
    let mut budget = Budget {
        work: 0,
        metadata: 0,
        limits,
    };
    budget.work(mul(source.len(), 4, Resource::Work)?)?;
    let potential = limits
        .max_tokens
        .min(add(source.len(), 1, Resource::Tokens)?);
    budget.metadata(add(
        source.len(),
        mul(
            potential,
            size_of::<Token>() + size_of::<usize>(),
            Resource::Metadata,
        )?,
        Resource::Metadata,
    )?)?;
    let lexed = lexer::lex_with_limits(
        source,
        lexer::LexLimits {
            max_source_bytes: limits.max_source_bytes,
            max_tokens: limits.max_tokens,
            max_diagnostics: 64,
        },
    )
    .map_err(|e| {
        Error::new(
            match e.kind {
                lexer::LexLimitKind::SourceBytes => ErrorKind::Budget(Resource::Source),
                lexer::LexLimitKind::Tokens => ErrorKind::Budget(Resource::Tokens),
                _ => ErrorKind::Lexical,
            },
            None,
        )
    })?;
    if lexed.limit.is_some() {
        return Err(Error::new(ErrorKind::Budget(Resource::Tokens), None));
    }
    if let Some(d) = lexed.diagnostics.first() {
        return Err(Error::new(ErrorKind::Lexical, Some(d.span.clone())));
    }
    let mut significant = Vec::new();
    significant
        .try_reserve_exact(lexed.tokens.len())
        .map_err(|_| Error::new(ErrorKind::Allocation, None))?;
    significant.extend(
        lexed
            .tokens
            .iter()
            .enumerate()
            .filter_map(|(i, t)| (!t.kind.trivia()).then_some(i)),
    );
    let hash = *blake3::hash(source.as_bytes()).as_bytes();
    let mut p = SourceParser {
        source,
        hash,
        tokens: lexed.tokens,
        significant,
        pos: 0,
        definitions: vec![],
        functions: vec![],
        parameter_names: vec![],
        invocations: vec![],
        support_items: Module::default(),
        budget,
    };
    while p.kind() != K::Eof {
        if p.word("macro") {
            p.push_def()?;
        } else {
            match p.kind() {
                K::Fn => p.function()?,
                K::Copy | K::Affine | K::Linear | K::Struct | K::Enum | K::Effect => p.support()?,
                _ => return Err(p.fail(ErrorKind::Unsupported)),
            }
        }
    }
    for invocation in &mut p.invocations {
        let mut found = None;
        for (index, definition) in p.definitions.iter().enumerate() {
            p.budget.work(add(
                definition.name_span.len(),
                invocation.name_span.len(),
                Resource::Work,
            )?)?;
            if p.source[definition.name_span.clone()] == p.source[invocation.name_span.clone()] {
                found = Some((index, definition.parameters.len()));
                break;
            }
        }
        let Some((index, arity)) = found else {
            return Err(Error::new(
                ErrorKind::UnknownMacro,
                Some(invocation.name_span.clone()),
            ));
        };
        if invocation.arguments.len() != arity {
            return Err(Error::new(ErrorKind::Arity, Some(invocation.span.clone())));
        }
        invocation.definition.index = index;
    }
    Ok(MacroSource {
        source: Arc::from(source),
        source_hash: hash,
        tokens: p.tokens,
        definitions: p.definitions,
        functions: p.functions,
        parameter_names: p.parameter_names,
        invocations: p.invocations,
        support_items: p.support_items,
    })
}
