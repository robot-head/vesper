//! Match one validated sugar rule against the unexpanded call. Matching
//! choices share a finite work budget, including fragment reparsing.
use crate::{
    lexer::{self, TokenKind as K},
    parser,
};
use std::{collections::BTreeMap, ops::Range};

pub(super) type Captures = BTreeMap<String, Capture>;

#[derive(Debug, Default, Clone)]
pub(super) struct Capture {
    pub sort: String,
    pub pass: Option<String>,
    pub values: Vec<Captured>,
    /// Iteration counts by enclosing indices, including empty repetitions.
    pub extents: BTreeMap<Vec<usize>, usize>,
}

#[derive(Debug, Clone)]
pub(super) struct Captured {
    pub span: Range<usize>,
    pub from_tail: bool,
    pub text: String,
    pub indices: Vec<usize>,
}

fn spend(work: &mut usize, n: usize) -> Result<(), String> {
    *work = work
        .checked_sub(n)
        .ok_or("sugar pattern matching work limit exceeded")?;
    Ok(())
}

struct Tokens<'a> {
    src: &'a str,
    tokens: Vec<lexer::Token>,
    // Exclusive end of one balanced token tree, or i+1 for an atom.
    end: Vec<usize>,
}

fn close(k: K) -> Option<K> {
    match k {
        K::LParen => Some(K::RParen),
        K::LBracket => Some(K::RBracket),
        K::LBrace => Some(K::RBrace),
        _ => None,
    }
}

impl<'a> Tokens<'a> {
    fn new(src: &'a str) -> Result<Self, String> {
        let lexed =
            lexer::sugar_lex_with_limits(src, Default::default()).map_err(|e| e.to_string())?;
        if !lexed.diagnostics.is_empty() {
            return Err("invalid token in sugar rule or invocation".into());
        }
        let tokens: Vec<_> = lexed
            .tokens
            .into_iter()
            .filter(|t| !t.kind.trivia() && t.kind != K::Eof)
            .collect();
        let mut end: Vec<_> = (1..=tokens.len()).collect();
        let mut stack = Vec::new();
        for (i, t) in tokens.iter().enumerate() {
            if let Some(k) = close(t.kind) {
                stack.push((i, k));
            } else if matches!(t.kind, K::RParen | K::RBracket | K::RBrace) {
                let Some((start, k)) = stack.pop() else {
                    return Err("unmatched sugar delimiter".into());
                };
                if k != t.kind {
                    return Err("mismatched sugar delimiter".into());
                }
                end[start] = i + 1;
            }
        }
        if !stack.is_empty() {
            return Err("unclosed sugar delimiter".into());
        }
        Ok(Self { src, tokens, end })
    }
    fn text(&self, i: usize) -> &'a str {
        self.tokens.get(i).map_or("", |t| &self.src[t.span.clone()])
    }
    fn kind(&self, i: usize) -> K {
        self.tokens.get(i).map_or(K::Eof, |t| t.kind)
    }
    fn slice(&self, r: Range<usize>) -> &'a str {
        if r.is_empty() {
            ""
        } else {
            &self.src[self.tokens[r.start].span.start..self.tokens[r.end - 1].span.end]
        }
    }
}

fn tail_name<'a>(t: &Tokens<'a>) -> Option<&'a str> {
    let i = t.tokens.len().checked_sub(5)?;
    (t.kind(i) == K::Dollar
        && t.kind(i + 1) == K::DotDot
        && t.kind(i + 3) == K::Colon
        && matches!(t.text(i + 4), "stmts" | "scope"))
    .then(|| t.text(i + 2))
}

pub(super) fn captures_tail(pattern: &str) -> bool {
    Tokens::new(pattern).is_ok_and(|t| tail_name(&t).is_some())
}

enum Op<'a> {
    Literal(&'a str, Option<bool>),
    Hole {
        name: &'a str,
        sequence: bool,
    },
    Tail(&'a str),
    Repeat {
        end: usize,
        minimum: usize,
        names: Vec<&'a str>,
    },
    Next {
        begin: usize,
        separator: Option<&'a str>,
    },
}

fn compile<'a>(
    t: &Tokens<'a>,
    captures: &mut Captures,
    work: &mut usize,
) -> Result<Vec<Op<'a>>, String> {
    let mut ops = Vec::new();
    let mut groups = Vec::new();
    let mut i = 0;
    let mut symbol_end = 0;
    while i < t.tokens.len() {
        if groups.last().is_some_and(|&(_, end)| i == end) {
            let (begin, _) = groups.pop().unwrap();
            i += 1;
            let sep = if matches!(t.kind(i), K::Star | K::Plus) {
                None
            } else {
                let s = t.text(i);
                i += 1;
                Some(s)
            };
            let minimum = match t.kind(i) {
                K::Plus => 1,
                K::Star => 0,
                _ => return Err("invalid sugar repetition".into()),
            };
            let end = ops.len();
            let Op::Repeat {
                end: target,
                minimum: min,
                ..
            } = &mut ops[begin]
            else {
                unreachable!()
            };
            *target = end;
            *min = minimum;
            ops.push(Op::Next {
                begin,
                separator: sep,
            });
            i += 1;
            continue;
        }
        if t.kind(i) != K::Dollar {
            let joint = if i < symbol_end {
                Some(true)
            } else {
                if let Some((_, end)) = parser::sugar_operator(&t.tokens, i) {
                    symbol_end = end;
                }
                if i > 0
                    && matches!(ops.last(), Some(Op::Literal(_, _)))
                    && matches!(
                        t.kind(i - 1),
                        K::Lower | K::With | K::Eff | K::Linear | K::State
                    )
                    && matches!(t.kind(i), K::LBrace | K::String)
                {
                    Some(t.tokens[i - 1].span.end == t.tokens[i].span.start)
                } else {
                    None
                }
            };
            ops.push(Op::Literal(t.text(i), joint));
            i += 1;
            continue;
        }
        let start = i;
        i += 1;
        if t.kind(i) == K::LParen {
            if groups.len() >= parser::HARD_PARSE_DEPTH {
                return Err("sugar repetition nesting limit exceeded".into());
            }
            groups.push((ops.len(), t.end[i] - 1));
            ops.push(Op::Repeat {
                end: 0,
                minimum: 0,
                names: Vec::new(),
            });
            i += 1;
            continue;
        }
        let sequence = t.kind(i) == K::DotDot;
        if sequence {
            i += 1
        }
        if !matches!(t.kind(i), K::Lower | K::Upper)
            || t.kind(i + 1) != K::Colon
            || t.kind(i + 2) != K::Lower
        {
            return Err("malformed sugar capture".into());
        }
        let name = t.text(i);
        let sort = t.text(i + 2);
        let pass = start
            .checked_sub(1)
            .map(|j| t.text(j))
            .filter(|s| matches!(*s, "in" | "inout" | "out" | "sink"))
            .map(str::to_owned);
        if captures
            .insert(
                name.to_owned(),
                Capture {
                    sort: sort.to_owned(),
                    pass,
                    ..Default::default()
                },
            )
            .is_some()
        {
            return Err(format!("duplicate sugar capture `${name}`"));
        }
        spend(work, groups.len())?;
        for &(begin, _) in &groups {
            let Op::Repeat { names, .. } = &mut ops[begin] else {
                unreachable!()
            };
            names.push(name);
        }
        i += 3;
        if sequence && i == t.tokens.len() && matches!(sort, "scope" | "stmts") {
            ops.push(Op::Tail(name));
        } else {
            ops.push(Op::Hole { name, sequence });
        }
    }
    if !groups.is_empty() {
        return Err("unclosed sugar repetition".into());
    }
    Ok(ops)
}

fn valid_fragment(
    sort: &str,
    sequence: bool,
    t: &Tokens<'_>,
    r: Range<usize>,
    work: &mut usize,
) -> Result<bool, String> {
    spend(work, r.len())?;
    if !sequence && r.is_empty() {
        return Ok(false);
    }
    let one_tree = !r.is_empty() && t.end[r.start] == r.end;
    match sort {
        "ident" => {
            return Ok(r.len() == 1
                && matches!(
                    t.kind(r.start),
                    K::Lower | K::Upper | K::State | K::With | K::Linear | K::Eff
                ));
        }
        "place" => {
            if !matches!(
                t.kind(r.start),
                K::Lower | K::SelfKw | K::State | K::With | K::Linear | K::Eff
            ) {
                return Ok(false);
            }
            let mut i = r.start + 1;
            while i < r.end {
                match t.kind(i) {
                    K::Dot
                        if matches!(
                            t.kind(i + 1),
                            K::Lower | K::State | K::With | K::Linear | K::Eff
                        ) =>
                    {
                        i += 2
                    }
                    K::LBracket if t.end[i] <= r.end && t.end[i] > i + 2 => i = t.end[i],
                    _ => return Ok(false),
                }
            }
            if i != r.end {
                return Ok(false);
            }
        }
        "block" | "scope" if !sequence && (!one_tree || t.kind(r.start) != K::LBrace) => {
            return Ok(false);
        }
        _ => {}
    }
    let (prefix, suffix) = match sort {
        "expr" | "place" | "block" | "scope" if !sequence => ("fn __f(){let __x = (", ");}"),
        "ty" => ("fn __f(__x: ", "){}"),
        "pat" => ("fn __f(){let ", " = __x;}"),
        "args" => ("fn __f(){__call(", ");}"),
        "pats" => ("fn __f(){let (", ") = __x;}"),
        "stmts" | "scope" => ("fn __f(){", "\n}"),
        "items" => ("", ""),
        _ => return Err(format!("unknown sugar hole sort `{sort}`")),
    };
    if !sequence {
        let mut i = r.start;
        while i < r.end {
            if matches!(t.kind(i), K::Semi | K::Comma) {
                return Ok(false);
            }
            i = t.end[i];
        }
    }
    let source = t.slice(r);
    spend(work, prefix.len() + source.len() + suffix.len())?;
    let wrapped = format!("{prefix}{source}{suffix}");
    let parsed = parser::parse_sugar_fragment(&wrapped).map_err(|e| e.to_string())?;
    Ok(parsed.diagnostics.is_empty())
}

#[derive(Clone)]
struct Frame {
    begin: usize,
    index: usize,
    start: usize,
}

#[derive(Clone, Default)]
struct State {
    pc: usize,
    pos: usize,
    frames: Vec<Frame>,
    log: Option<usize>,
    // Validate a variable-width candidate only if this branch is explored.
    pending: Option<(usize, Range<usize>)>,
}

enum Event<'a> {
    Value(&'a str, Range<usize>, bool),
    Extent(usize, usize),
}

struct Record<'a> {
    prev: Option<usize>,
    indices: Vec<usize>,
    event: Event<'a>,
}

impl State {
    fn record<'a>(
        &mut self,
        event: Event<'a>,
        records: &mut Vec<Record<'a>>,
        work: &mut usize,
    ) -> Result<(), String> {
        spend(work, 1 + self.frames.len())?;
        let prev = self.log.replace(records.len());
        records.push(Record {
            prev,
            indices: self.frames.iter().map(|f| f.index).collect(),
            event,
        });
        Ok(())
    }
    fn fork(&self, work: &mut usize) -> Result<Self, String> {
        spend(work, 1 + self.frames.len())?;
        Ok(self.clone())
    }
}

pub(super) fn match_rule(
    pattern: &str,
    invocation: &str,
    tail: Option<&str>,
    work: &mut usize,
) -> Result<Option<Captures>, String> {
    spend(work, pattern.len() + invocation.len())?;
    let p = Tokens::new(pattern)?;
    let t = Tokens::new(invocation)?;
    let mut captures = Captures::new();
    let ops = compile(&p, &mut captures, work)?;
    let mut limit = t.tokens.len();
    // A statement ending in a brace may carry an optional semicolon.
    let last_literal = ops.iter().rev().find(|op| !matches!(op, Op::Tail(_)));
    if limit >= 2
        && t.kind(limit - 1) == K::Semi
        && t.kind(limit - 2) == K::RBrace
        && !matches!(last_literal, Some(Op::Literal(";", _)))
    {
        limit -= 1;
    }
    let mut todo = vec![State::default()];
    let mut records = Vec::new();
    while let Some(mut s) = todo.pop() {
        if let Some((pc, r)) = s.pending.take() {
            let Op::Hole { name, sequence } = ops[pc] else {
                unreachable!()
            };
            if !valid_fragment(&captures[name].sort, sequence, &t, r.clone(), work)? {
                continue;
            }
            let bytes = if r.is_empty() {
                let pos = t
                    .tokens
                    .get(r.start)
                    .map_or(invocation.len(), |tok| tok.span.start);
                pos..pos
            } else {
                t.tokens[r.start].span.start..t.tokens[r.end - 1].span.end
            };
            s.record(Event::Value(name, bytes, false), &mut records, work)?;
        }
        loop {
            spend(work, 1)?;
            let Some(op) = ops.get(s.pc) else {
                if s.pos != limit {
                    break;
                }
                let mut log = s.log;
                while let Some(i) = log {
                    let rec = &records[i];
                    match &rec.event {
                        Event::Value(name, span, from_tail) => {
                            let src = if *from_tail {
                                tail.unwrap_or("")
                            } else {
                                invocation
                            };
                            spend(work, span.len() + rec.indices.len())?;
                            captures.get_mut(*name).unwrap().values.push(Captured {
                                text: src[span.clone()].to_owned(),
                                indices: rec.indices.clone(),
                                span: span.clone(),
                                from_tail: *from_tail,
                            });
                        }
                        Event::Extent(begin, count) => {
                            let Op::Repeat { names, .. } = &ops[*begin] else {
                                unreachable!()
                            };
                            for name in names {
                                spend(work, 1 + rec.indices.len())?;
                                captures
                                    .get_mut(*name)
                                    .unwrap()
                                    .extents
                                    .insert(rec.indices.clone(), *count);
                            }
                        }
                    }
                    log = rec.prev;
                }
                for cap in captures.values_mut() {
                    cap.values.reverse();
                }
                return Ok(Some(captures));
            };
            match op {
                Op::Literal(lit, joint) => {
                    if s.pos == limit
                        || *lit != t.text(s.pos)
                        || joint.is_some_and(|a| {
                            s.pos == 0
                                || (t.tokens[s.pos - 1].span.end == t.tokens[s.pos].span.start) != a
                        })
                    {
                        break;
                    }
                    s.pos += 1;
                    s.pc += 1;
                }
                Op::Tail(name) => {
                    let Some(src) = tail else { break };
                    if s.pos != limit {
                        break;
                    }
                    s.record(Event::Value(name, 0..src.len(), true), &mut records, work)?;
                    s.pc += 1;
                }
                Op::Hole { name, sequence } => {
                    let start = s.pos;
                    let mut end = start;
                    let scalar = &captures[*name].sort;
                    loop {
                        spend(work, 1)?;
                        let length_ok = (*sequence || end > start)
                            && match scalar.as_str() {
                                "ident" => end == start + 1,
                                "block" | "scope" if !sequence => {
                                    t.kind(start) == K::LBrace && t.end.get(start) == Some(&end)
                                }
                                _ => true,
                            };
                        let next_ok = match ops.get(s.pc + 1) {
                            None | Some(Op::Tail(_)) => end == limit,
                            Some(Op::Literal(lit, _)) => end < limit && *lit == t.text(end),
                            _ => true,
                        };
                        if length_ok && next_ok {
                            let mut branch = s.fork(work)?;
                            branch.pending = Some((s.pc, start..end));
                            branch.pos = end;
                            branch.pc += 1;
                            todo.push(branch);
                        }
                        if end == limit
                            || matches!(t.kind(end), K::RBrace | K::RParen | K::RBracket)
                        {
                            break;
                        }
                        if !sequence && matches!(t.kind(end), K::Comma | K::Semi) {
                            break;
                        }
                        if end > start
                            && (scalar == "ident"
                                || (!sequence && matches!(scalar.as_str(), "block" | "scope")))
                        {
                            break;
                        }
                        end = t.end[end];
                    }
                    break;
                }
                Op::Repeat { end, minimum, .. } => {
                    if *minimum == 0 {
                        let mut skip = s.fork(work)?;
                        skip.record(Event::Extent(s.pc, 0), &mut records, work)?;
                        skip.pc = end + 1;
                        todo.push(skip);
                    }
                    s.frames.push(Frame {
                        begin: s.pc,
                        index: 0,
                        start: s.pos,
                    });
                    s.pc += 1;
                }
                Op::Next { begin, separator } => {
                    let frame = s.frames.last().unwrap();
                    debug_assert_eq!(frame.begin, *begin);
                    if s.pos == frame.start {
                        break;
                    }
                    let count = frame.index + 1;
                    let mut done = s.fork(work)?;
                    done.frames.pop();
                    done.record(Event::Extent(*begin, count), &mut records, work)?;
                    done.pc += 1;
                    todo.push(done);
                    if let Some(sep) = separator {
                        if s.pos == limit || t.text(s.pos) != *sep {
                            break;
                        }
                        s.pos += 1;
                    }
                    if s.pos == limit {
                        break;
                    }
                    let frame = s.frames.last_mut().unwrap();
                    frame.index = count;
                    frame.start = s.pos;
                    s.pc = begin + 1;
                }
            }
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn capture(p: &str, c: &str) -> Captures {
        match_rule(p, c, None, &mut 100_000).unwrap().unwrap()
    }
    #[test]
    fn typed_places_and_alias_heads_preserve_the_callers_text() {
        let caps = capture(
            "while $cond:expr invariant $ok:expr $b:block",
            "while a < n invariant a <= n { step(); }",
        );
        assert_eq!(caps["cond"].values[0].text, "a < n");
        let caps = capture(
            "swap inout $a:place with inout $b:place ;",
            "swap inout a[i] with inout s.y;",
        );
        assert_eq!(caps["a"].values[0].text, "a[i]");
        assert_eq!(caps["b"].pass.as_deref(), Some("inout"));
        assert!(
            match_rule(
                "swap inout $x:place ;",
                "swap inout get();",
                None,
                &mut 10000
            )
            .unwrap()
            .is_none()
        );
    }
    #[test]
    fn nested_star_repeats_record_empty_rows_and_tree_boundaries() {
        let c = capture(
            "rows { $( [ $( $v:expr ),* ] );* }",
            "rows { []; [f(1,2), 3] }",
        );
        let cap = &c["v"];
        assert_eq!(cap.extents[&vec![]], 2);
        assert_eq!(cap.extents[&vec![0]], 0);
        assert_eq!(cap.values[0].indices, [1, 0]);
        assert_eq!(cap.values[0].text, "f(1,2)");
        assert_eq!(cap.values[1].indices, [1, 1]);
    }
    #[test]
    fn sequence_and_tail_capture_distinct_source_ranges() {
        let c = capture(
            "$lhs:expr |?> $callee:ident ( $..args:args )",
            "frame |?> decode(x, in s)",
        );
        assert_eq!(c["args"].values[0].text, "x, in s");
        let c = match_rule(
            "defer $b:block $..rest:scope",
            "defer { tidy(); };",
            Some(" next(); value"),
            &mut 10000,
        )
        .unwrap()
        .unwrap();
        assert!(c["rest"].values[0].from_tail);
        assert_eq!(c["rest"].values[0].text, " next(); value");
    }
    #[test]
    fn tagged_and_symbolic_pattern_literals_keep_their_adjacency() {
        assert!(
            match_rule("tag{ x }", "tag { x }", None, &mut 1000)
                .unwrap()
                .is_none()
        );
        assert!(
            match_rule("x .+ y", "x . + y", None, &mut 1000)
                .unwrap()
                .is_none()
        );
        assert!(
            match_rule("tag{ x }", "tag{x}", None, &mut 1000)
                .unwrap()
                .is_some()
        );
    }
    #[test]
    fn ambiguous_matches_are_bounded() {
        let result = match_rule(
            "$( $x:expr )* z",
            "a ".repeat(150).as_str(),
            None,
            &mut 1000,
        );
        assert!(result.is_err());
    }
}
