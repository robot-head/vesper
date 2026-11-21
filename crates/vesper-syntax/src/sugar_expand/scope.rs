//! Reify a scope's exits, run the rule's cleanup, then dispatch its resume.
//! Only caller captures are rewritten. Explicit exits inside closures and
//! breaks targeting loops within the capture retain their lexical targets.
//!
//! The supported protocol binds one scope to a local, executes cleanup, and
//! resumes that local exactly once as the block tail. A concrete Fail row is
//! relayed with its payload. Open effect rows, labelled Fail capabilities,
//! other potentially abortive user effects, and an enclosing closure require
//! control or type information not present at this expansion phase and are
//! diagnosed. In a function returning Option with a Fail row, an explicit
//! match disambiguates Option's early return from Result's abort.
use super::{matcher::Captures, template::Rendered};
use crate::{
    ast::{ExprKind, Item, Span},
    lexer::{self, Token, TokenKind as K},
    parser,
};

fn significant(source: &str) -> Result<Vec<Token>, String> {
    let l = lexer::sugar_lex_with_limits(source, Default::default()).map_err(|e| e.to_string())?;
    if !l.diagnostics.is_empty() {
        return Err("invalid tokens in scope capture".into());
    }
    Ok(l.tokens
        .into_iter()
        .filter(|t| !t.kind.trivia() && t.kind != K::Eof)
        .collect())
}

fn covers(outer: &Span, inner: &Span) -> bool {
    outer.start <= inner.start && outer.end >= inner.end
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Exit {
    Normal,
    Return,
    Break,
    Continue,
}
struct Jump {
    exit: Exit,
    span: Span,
    value: Option<Span>,
}

struct Control {
    jumps: Vec<Jump>,
    questions: Vec<(Span, Span)>,
    crosses_loop: bool,
}
fn jumps(body: &str) -> Result<Control, String> {
    let prefix = "fn __scope_probe() ";
    let mut parsed = parser::rust_parse(&format!("{prefix}{body}"));
    if !parsed.diagnostics.is_empty() {
        return Err("expand nested surface forms before capturing a scope".into());
    }
    let Some(Item::Function(f)) = parsed.module.items.first_mut() else {
        return Err("invalid scope body".into());
    };
    let mut closures = Vec::new();
    let mut loops = Vec::new();
    let mut handlers = Vec::new();
    let mut forbidden = Vec::new();
    let mut jumps = Vec::new();
    let mut tries = Vec::new();
    let relative = |r: &Span| r.start - prefix.len()..r.end - prefix.len();
    parser::walk_exprs(f.body.as_mut().unwrap(), &mut |e| {
        let (exit, value) = match &e.kind {
            ExprKind::Closure { body, .. } => {
                closures.push(relative(&body.span));
                return;
            }
            ExprKind::Loop(body) | ExprKind::For { body, .. } => {
                loops.push(relative(&body.span));
                return;
            }
            ExprKind::Handle {
                ret, ops, effect, ..
            } => {
                handlers.push(relative(&e.span));
                forbidden.push(relative(&ret.body.span));
                forbidden.extend(ops.iter().map(|c| relative(&c.body.span)));
                if effect.name == parser::REGION_EFFECT {
                    forbidden.push(relative(&e.span));
                }
                return;
            }
            ExprKind::Try(v) => {
                tries.push((relative(&e.span), relative(&v.span)));
                return;
            }
            ExprKind::Break(v) => (Exit::Break, v.as_deref().map(|v| relative(&v.span))),
            ExprKind::Return(v) => (Exit::Return, v.as_deref().map(|v| relative(&v.span))),
            ExprKind::Continue => (Exit::Continue, None),
            _ => return,
        };
        jumps.push(Jump {
            exit,
            value,
            span: relative(&e.span),
        });
    });
    jumps.retain(|j| {
        !closures.iter().any(|r| covers(r, &j.span))
            && (j.exit == Exit::Return || !loops.iter().any(|r| covers(r, &j.span)))
    });
    if jumps.iter().any(|j| {
        forbidden.iter().any(|r| covers(r, &j.span))
            || j.exit != Exit::Return && handlers.iter().any(|r| covers(r, &j.span))
    }) {
        return Err("invalid control transfer across a handler or region boundary".into());
    }
    tries.retain(|(t, _)| !closures.iter().any(|r| covers(r, t)));
    let crosses_loop = jumps.iter().any(|j| {
        j.exit == Exit::Return && handlers.iter().chain(&loops).any(|r| covers(r, &j.span))
    });
    Ok(Control {
        jumps,
        questions: tries,
        crosses_loop,
    })
}

/// Collect the concrete Fail rows active at the call, retaining the source
/// spelling of type arguments (including nested generic types).
fn ambient(source: &str, call: Span) -> Result<(Vec<String>, String), String> {
    let p =
        parser::parse_surface_with_limits(source, Default::default()).map_err(|e| e.to_string())?;
    let root = p.syntax();
    let mut total_effects = std::collections::BTreeSet::from([
        "ConstTime".to_owned(),
        "Realtime".to_owned(),
        "ConstDenorm".to_owned(),
        "DetFloat".to_owned(),
    ]);
    let all = significant(source)?;
    for decl in all.windows(3) {
        if decl[0].kind == K::Linear && decl[1].kind == K::Effect && decl[2].kind == K::Upper {
            total_effects.insert(source[decl[2].span.clone()].to_owned());
        }
    }
    let check_effect = |name: &str| {
        if name == "Fail" || total_effects.contains(name) {
            Ok(())
        } else {
            Err(format!(
                "exit-aware scope cannot relay potentially abortive effect `{name}`"
            ))
        }
    };
    let mut rows = Vec::new();
    let mut result_ty = "()".to_owned();
    for node in root
        .descendants()
        .filter(|n| matches!(n.kind(), parser::ITEM | parser::EXPR))
    {
        let r = node.text_range();
        if usize::from(r.start()) > call.start || usize::from(r.end()) < call.end {
            continue;
        }
        let s = &source[usize::from(r.start())..call.start];
        let ts = significant(s)?;
        let word = |i: usize| ts.get(i).map_or("", |t| &s[t.span.clone()]);
        let kind = |i: usize| ts.get(i).map(|t| t.kind);
        let mut ranges = Vec::new();
        if node.kind() == parser::ITEM {
            // Only the signature's top-level effect row is ambient.
            let mut depth = 0usize;
            for (i, t) in ts.iter().enumerate() {
                match t.kind {
                    K::LBrace if depth == 0 => break,
                    K::Arrow if depth == 0 => {
                        let mut j = i + 1;
                        let mut brackets = 0usize;
                        while j < ts.len() {
                            if brackets == 0
                                && (matches!(ts[j].kind, K::Eff | K::LBrace)
                                    || matches!(word(j), "where" | "ensures"))
                            {
                                break;
                            }
                            match ts[j].kind {
                                K::LParen | K::LBracket => brackets += 1,
                                K::RParen | K::RBracket => brackets = brackets.saturating_sub(1),
                                _ => {}
                            }
                            j += 1;
                        }
                        if j > i + 1 {
                            result_ty = s[ts[i + 1].span.start..ts[j - 1].span.end].to_owned();
                        }
                    }
                    K::Eff if depth == 0 && kind(i + 1) == Some(K::LBracket) => {
                        let mut j = i + 2;
                        let mut nesting = 0;
                        while j < ts.len() {
                            if nesting == 0 && ts[j].kind == K::RBracket {
                                break;
                            }
                            if nesting == 0 && ts[j].kind == K::Upper {
                                check_effect(word(j))?;
                            }
                            if nesting == 0
                                && (ts[j].kind == K::Pipe
                                    || (ts[j].kind == K::Lower && kind(j + 1) != Some(K::Colon)))
                            {
                                return Err(
                                    "an exit-aware scope requires concrete effect rows".into()
                                );
                            }
                            if ts[j].kind == K::LBracket {
                                nesting += 1
                            }
                            if ts[j].kind == K::RBracket {
                                nesting -= 1
                            }
                            j += 1;
                        }
                        ranges.push(i + 2..j);
                    }
                    K::LParen | K::LBracket | K::LBrace => depth += 1,
                    K::RParen | K::RBracket | K::RBrace => depth = depth.saturating_sub(1),
                    _ => {}
                }
            }
        } else if kind(0) == Some(K::Handle) {
            // A handle's clauses execute outside that handle.
            let mut depth = 0usize;
            let mut in_clauses = false;
            for t in &ts {
                match t.kind {
                    K::With if depth == 0 => in_clauses = true,
                    K::LBrace | K::LParen | K::LBracket => depth += 1,
                    K::RBrace | K::RParen | K::RBracket => depth = depth.saturating_sub(1),
                    _ => {}
                }
            }
            if !in_clauses {
                let name = if kind(2) == Some(K::Colon) { 3 } else { 1 };
                if kind(name) == Some(K::Upper) {
                    check_effect(word(name))?;
                }
                ranges.push(1..ts.len().min(4));
            }
        } else if matches!(kind(0), Some(K::Pipe | K::Or)) {
            return Err(
                "scope capture inside a closure requires an explicit control boundary".into(),
            );
        }
        for indices in ranges {
            for i in indices {
                if word(i) != "Fail" || kind(i + 1) != Some(K::LBracket) {
                    continue;
                }
                if i > 0 && kind(i - 1) == Some(K::Colon) {
                    return Err(
                        "exit-aware scope cannot intercept a labelled Fail capability".into(),
                    );
                }
                let mut j = i + 2;
                let mut n = 1;
                while j < ts.len() {
                    match ts[j].kind {
                        K::LBracket => n += 1,
                        K::RBracket => n -= 1,
                        _ => {}
                    }
                    if n == 0 {
                        break;
                    }
                    j += 1;
                }
                if j == ts.len() {
                    return Err("incomplete Fail type in scope context".into());
                }
                let row = s[ts[i].span.start..ts[j].span.end].to_owned();
                if !rows.contains(&row) {
                    rows.push(row)
                }
            }
        }
    }
    Ok((rows, result_ty))
}

struct Change {
    range: Span,
    text: String,
    forwarded: bool,
}

fn encode(i: usize, total: usize, leaf: &str) -> String {
    let mut text = if i + 1 == total {
        leaf.into()
    } else {
        format!("Result::Ok({leaf})")
    };
    for _ in 0..i {
        text = format!("Result::Err({text})");
    }
    text
}

fn rewrite(r: &mut Rendered, mut edits: Vec<Change>, max: usize) -> Result<(), String> {
    edits.sort_by_key(|e| (e.range.start, e.range.end));
    let mut text = String::new();
    let mut pos = 0;
    let mut map = Vec::new();
    let mut forwarding = Vec::new();
    for e in edits {
        if e.range.start < pos {
            return Err("overlapping scope exits".into());
        }
        if text.len() + e.range.start - pos + e.text.len() > max {
            return Err("scope expansion exceeds output budget".into());
        }
        let offset = text.len();
        text.push_str(&r.text[pos..e.range.start]);
        map.push((pos..e.range.start, offset));
        let start = text.len();
        text.push_str(&e.text);
        if e.forwarded {
            forwarding.push(start..text.len());
        }
        pos = e.range.end;
    }
    if text.len() + r.text.len() - pos > max {
        return Err("scope expansion exceeds output budget".into());
    }
    map.push((pos..r.text.len(), text.len()));
    text.push_str(&r.text[pos..]);
    let parts = |old: &Span| {
        map.iter()
            .filter_map(|(src, at)| {
                let a = src.start.max(old.start);
                let b = src.end.min(old.end);
                (a < b).then(|| (a..b, at + (a - src.start)..at + (b - src.start)))
            })
            .collect::<Vec<_>>()
    };
    r.holes = std::mem::take(&mut r.holes)
        .iter()
        .flat_map(|h| parts(h).into_iter().map(|(_, v)| v))
        .chain(forwarding)
        .collect();
    r.origins = std::mem::take(&mut r.origins)
        .into_iter()
        .flat_map(|(region, old, tail)| {
            parts(&region).into_iter().map(move |(a, b)| {
                (
                    b,
                    old.start + (a.start - region.start)..old.start + (a.end - region.start),
                    tail,
                )
            })
        })
        .collect();
    r.places = std::mem::take(&mut r.places)
        .into_iter()
        .flat_map(|(region, p)| parts(&region).into_iter().map(move |(_, b)| (b, p)))
        .collect();
    r.text = text;
    Ok(())
}

pub(super) fn lower(
    r: &mut Rendered,
    captures: &Captures,
    source: &str,
    call: Span,
    mark: &str,
    max: usize,
) -> Result<String, String> {
    let mut scopes = captures
        .values()
        .filter(|v| v.sort == "scope")
        .flat_map(|v| &v.values);
    let Some(scope) = scopes.next() else {
        return Ok(String::new());
    };
    if scopes.next().is_some() {
        return Err("a rule can resume only one captured scope".into());
    }
    let origins: Vec<_> = r
        .origins
        .iter()
        .filter(|(_, span, tail)| *span == scope.span && *tail == scope.from_tail)
        .collect();
    let [origin] = origins.as_slice() else {
        return Err("a captured scope must be evaluated exactly once".into());
    };
    let mut body = origin.0.clone();
    if scope.from_tail {
        body.start -= 1;
        body.end += 1;
    }
    let ts = significant(&r.text)?;
    let first = ts.partition_point(|t| t.span.start < body.start);
    let after = ts.partition_point(|t| t.span.start < body.end);
    let word = |i: usize| ts.get(i).map_or("", |t| &r.text[t.span.clone()]);
    if first < 3 || word(first - 3) != "let" || word(first - 1) != "=" || word(after) != ";" {
        return Err("bind the scope exit with `let exit = ...;` before resuming it".into());
    }
    let binder = word(first - 2);
    let uses: Vec<_> = (after + 1..ts.len())
        .filter(|&i| word(i) == binder)
        .collect();
    let [resume] = uses.as_slice() else {
        return Err("a captured exit must be resumed exactly once".into());
    };
    let resume = *resume;
    if [
        word(resume + 1),
        word(resume + 2),
        word(resume + 3),
        word(resume + 4),
    ] != [".", "resume", "(", ")"]
        || !(resume + 5..ts.len()).all(|i| word(i) == "}")
    {
        return Err("a scope exit must be resumed as the tail of its cleanup block".into());
    }
    let control = jumps(&r.text[body.clone()])?;
    let jumps = control.jumps;
    let (fails, result_ty) = ambient(source, call)?;
    let option_try =
        !control.questions.is_empty() && result_ty.split(['[', ' ']).next() == Some("Option");
    if option_try && !fails.is_empty() {
        return Err(
            "use an explicit match for `?` when both Option and Fail exits are active in a scope"
                .into(),
        );
    }
    let relay_return = control.crosses_loop || option_try;
    let effect = format!("VesperScope{mark}");
    let label = format!("{mark}leave");
    let mut kinds = vec![Exit::Normal];
    for j in &jumps {
        if !(relay_return && j.exit == Exit::Return) && !kinds.contains(&j.exit) {
            kinds.push(j.exit)
        }
    }
    let mut edits = Vec::new();
    let value = format!("{mark}value");
    let err = format!("{mark}error");
    let mut head = String::new();
    let mut foot = String::new();
    if relay_return {
        head.push_str(&format!("handle {label}: {effect}[{result_ty}] in {{ "));
    }
    for f in &fails {
        head.push_str(&format!("handle {f} in {{ "));
    }
    if kinds.len() > 1 {
        head.push_str(&format!("loop {{ let {value} = "));
        foot.push_str(&format!("; break {}; }}", encode(0, kinds.len(), &value)));
    }
    for _ in &fails {
        foot.push_str(&format!(
            " }} with {{ return {value} => Result::Ok({value}), raise({err}, _) => Result::Err({err}), }}"
        ));
    }
    if relay_return {
        foot.push_str(&format!(
            " }} with {{ return {value} => Result::Ok({value}), leave({err}, _) => Result::Err({err}), }}"
        ));
    }
    edits.push(Change {
        range: body.start..body.start,
        text: head,
        forwarded: false,
    });
    for j in &jumps {
        let jump = if relay_return && j.exit == Exit::Return {
            format!("{label}.leave(#)")
        } else {
            let index = kinds.iter().position(|k| *k == j.exit).unwrap();
            format!("break {}", encode(index, kinds.len(), "#"))
        };
        let (left, right) = jump
            .split_once('#')
            .map(|(a, b)| (a.to_owned(), b.to_owned()))
            .unwrap();
        let a = body.start + j.span.start;
        let b = body.start + j.span.end;
        if let Some(v) = &j.value {
            edits.push(Change {
                range: a..body.start + v.start,
                text: left,
                forwarded: false,
            });
            edits.push(Change {
                range: b..b,
                text: right,
                forwarded: false,
            });
        } else {
            edits.push(Change {
                range: a..b,
                text: format!("{left}(){right}"),
                forwarded: false,
            });
        }
    }
    if option_try {
        for (whole, operand) in control.questions {
            // Parenthesize the match: callers can project or call its result.
            let start = body.start + operand.start;
            edits.push(Change {
                range: start..start,
                text: "(match (".into(),
                forwarded: false,
            });
            let q = body.start + whole.end - 1;
            edits.push(Change{range:q..q+1,text:format!(") {{ Option::Some({value}) => {value}, Option::None => {label}.leave(Option::None), }})"),forwarded:true});
        }
    }
    edits.push(Change {
        range: body.end..body.end,
        text: foot,
        forwarded: false,
    });
    let dispatch = |exit: Exit| match exit {
        Exit::Normal => value.clone(),
        Exit::Return => format!("return {value}"),
        Exit::Continue => "continue".into(),
        Exit::Break
            if jumps
                .iter()
                .any(|j| j.exit == Exit::Break && j.value.is_some()) =>
        {
            format!("break {value}")
        }
        Exit::Break => "break".into(),
    };
    let mut result = dispatch(*kinds.last().unwrap());
    for &k in kinds[..kinds.len() - 1].iter().rev() {
        result = format!(
            "match {value} {{ Result::Ok({value}) => {}, Result::Err({value}) => {result}, }}",
            dispatch(k)
        );
    }
    for _ in &fails {
        result = format!(
            "match {value} {{ Result::Ok({value}) => {result}, Result::Err({err}) => Fail::raise({err}), }}"
        );
    }
    if relay_return {
        result =
            format!("match {value} {{ Result::Ok({value}) => {result}, Result::Err({err}) => return {err}, }}");
    }
    edits.push(Change {
        range: ts[resume].span.start..ts[resume + 4].span.end,
        text: format!("{{ let {value} = {binder}; {result} }}"),
        forwarded: true,
    });
    // The wrapper only relays control belonging to the caller. In particular
    // its private return effect and its forwarded Fail operations introduce
    // no capability that the sugar may otherwise exercise.
    let body_size = edits
        .iter()
        .filter(|e| covers(&body, &e.range))
        .fold(body.len(), |len, e| len + e.text.len() - e.range.len());
    let forwarded_body = body.start..body.start + body_size;
    rewrite(r, edits, max)?;
    r.holes.push(forwarded_body);
    Ok(if relay_return {
        format!("\neffect {effect}[T] {{ fn leave(sink value:T) -> Never; }}\n")
    } else {
        String::new()
    })
}

#[cfg(test)]
mod tests {
    use crate::sugar_expand::expand_sugar;

    const DEFER: &str =
        "sugar defer:stmt { rule(defer $b:block $..r:scope)=>{{let exit=$..r; $b; exit.resume()}}}";

    #[test]
    fn opaque_abort_channels_are_diagnosed_before_execution() {
        for text in [
            "effect Stop {fn stop()->Never;} fn f()->i64 eff [Stop]{defer {} Stop::stop()}",
            "fn f()->Option[i64] eff [Fail[RuntimeError]]{defer {} let x=Some(1)?;Some(x)}",
            "fn f()->i64 {handle stop:Fail[i64] in {defer {} stop.raise(3)} with {return v=>v,raise(e,_)=>e,}}",
        ] {
            assert!(expand_sugar(&format!("{DEFER} {text}")).is_err(), "{text}");
        }
    }
}
