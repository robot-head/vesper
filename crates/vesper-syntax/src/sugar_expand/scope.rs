//! Reify a scope's exits, run the rule's cleanup, then dispatch its resume.
//! Only caller captures are rewritten. Explicit exits inside closures and
//! breaks targeting loops within the capture retain their lexical targets.
//!
//! The supported protocol binds one scope to a local, executes cleanup, and
//! resumes that local exactly once as the block tail. A concrete Fail row is
//! relayed with its payload. Open effect rows, labelled Fail capabilities,
//! other potentially abortive user effects, and inferred closure rows require
//! control or type information not present at this expansion phase and are
//! diagnosed. In a function returning Option with a Fail row, an explicit
//! match disambiguates Option's early return from Result's abort. The same
//! restriction applies to a question mark inside a nested effect handler.
use super::{matcher::Captures, template::Rendered};
use crate::{
    ast::{Effects, ExprKind, Item, Span, Type},
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

fn outermost(mut spans: Vec<Span>) -> Vec<Span> {
    spans.sort_by_key(|r| (r.start, std::cmp::Reverse(r.end)));
    let mut out = Vec::new();
    for r in spans {
        if out.last().is_none_or(|prev| !covers(prev, &r)) {
            out.push(r);
        }
    }
    out
}
fn enclosed(spans: &[Span], r: &Span) -> bool {
    let end = spans.partition_point(|s| s.start <= r.start);
    end > 0 && covers(&spans[end - 1], r)
}

struct Control {
    jumps: Vec<Jump>,
    questions: Vec<(Span, Span)>,
    crosses_loop: bool,
    question_in_handler: bool,
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
    let closures = outermost(closures);
    let loops = outermost(loops);
    let handlers = outermost(handlers);
    let forbidden = outermost(forbidden);
    jumps.retain(|j| {
        !enclosed(&closures, &j.span) && (j.exit == Exit::Return || !enclosed(&loops, &j.span))
    });
    if jumps.iter().any(|j| {
        enclosed(&forbidden, &j.span) || j.exit != Exit::Return && enclosed(&handlers, &j.span)
    }) {
        return Err("invalid control transfer across a handler or region boundary".into());
    }
    tries.retain(|(t, _)| !enclosed(&closures, t));
    let question_in_handler = tries.iter().any(|(t, _)| enclosed(&handlers, t));
    let crosses_loop = jumps.iter().any(|j| {
        j.exit == Exit::Return && (enclosed(&handlers, &j.span) || enclosed(&loops, &j.span))
    });
    Ok(Control {
        jumps,
        questions: tries,
        crosses_loop,
        question_in_handler,
    })
}

/// Select the actual function, closure and handler bodies enclosing this
/// call. Handler clauses execute outside their own handler; closures start
/// a fresh effect and return boundary.
fn ambient(source: &str, call: Span) -> Result<(Vec<String>, Type), String> {
    let mut parsed = parser::parse_expansion_scopes(source, Default::default(), true)
        .map_err(|e| e.to_string())?;
    if !parsed.diagnostics.is_empty() {
        return Err("could not determine effects at scope capture".into());
    }
    let linear: std::collections::BTreeSet<_> = parsed
        .module
        .items
        .iter()
        .filter_map(|i| match i {
            Item::Effect(e) if e.linear => Some(e.name.clone()),
            _ => None,
        })
        .collect();
    let mut functions = parsed.module.items.iter_mut().flat_map(|i| match i {
        Item::Function(f) => std::slice::from_mut(f.as_mut()),
        Item::Impl { methods, .. } => methods.as_mut_slice(),
        Item::Conformance(c) => c.methods.as_mut_slice(),
        _ => &mut [],
    });
    let f = functions
        .find(|f| f.body.as_ref().is_some_and(|b| covers(&b.span, &call)))
        .ok_or("scope capture requires a function body")?;
    let mut result = f.result.clone();
    let mut effects = f.effects.clone();
    let mut frames = Vec::new();
    parser::walk_exprs(f.body.as_mut().unwrap(), &mut |e| match &e.kind {
        ExprKind::Handle { effect, body, .. } if covers(&body.span, &call) => {
            frames.push((
                body.span.len(),
                None,
                Effects {
                    instances: vec![effect.clone()],
                    tail: None,
                },
                false,
            ));
        }
        ExprKind::Closure {
            result,
            effects,
            body,
            infer_effects,
            ..
        } if covers(&body.span, &call) => {
            frames.push((
                body.span.len(),
                Some(result.clone()),
                effects.clone(),
                *infer_effects,
            ));
        }
        _ => {}
    });
    frames.sort_by_key(|(len, ..)| std::cmp::Reverse(*len));
    for (_, returns, frame, inferred) in frames {
        if let Some(returns) = returns {
            if inferred {
                return Err(
                    "exit-aware scope inside a closure requires an explicit effect row".into(),
                );
            }
            result = returns;
            effects = frame;
        } else {
            effects.instances.extend(frame.instances);
        }
    }
    if effects.tail.is_some() {
        return Err("an exit-aware scope requires concrete effect rows".into());
    }
    let mut rows = Vec::new();
    for e in effects.instances {
        match e.name.as_str() {
            "Fail" => {
                if e.label.is_some() {
                    return Err(
                        "exit-aware scope cannot intercept a labelled Fail capability".into(),
                    );
                }
                let [arg] = e.args.as_slice() else {
                    return Err("expected Fail with one error type".into());
                };
                let row = format!("Fail[{}]", source_type(arg)?);
                if !rows.contains(&row) {
                    rows.push(row);
                }
            }
            "ConstTime" | "Realtime" | "ConstDenorm" | "DetFloat" | parser::REGION_EFFECT => {}
            n if linear.contains(n) => {}
            n => {
                return Err(format!(
                    "exit-aware scope cannot relay potentially abortive effect `{n}`"
                ));
            }
        }
    }
    Ok((rows, result))
}

// Display for function and row types is intended for diagnostics, not source
// generation. Never print it back as an approximation of the caller's type.
fn source_type(t: &Type) -> Result<String, String> {
    match t {
        Type::Named(_, args) | Type::Tuple(args) if args.iter().all(|a| source_type(a).is_ok()) => {
            Ok(t.to_string())
        }
        _ => Err("scope exit requires an explicit named or tuple payload type".into()),
    }
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
        map[map.partition_point(|(src, _)| src.end <= old.start)..]
            .iter()
            .take_while(|(src, _)| src.start < old.end)
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
    let option_try = !control.questions.is_empty() && result_ty.name() == "Option";
    if option_try && (!fails.is_empty() || control.question_in_handler) {
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
        head.push_str(&format!(
            "handle {label}: {effect}[{}] in {{ ",
            source_type(&result_ty)?
        ));
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
        result = format!(
            "match {value} {{ Result::Ok({value}) => {result}, Result::Err({err}) => return {err}, }}"
        );
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
