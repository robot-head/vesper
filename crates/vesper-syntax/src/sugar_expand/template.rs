//! Instantiate a rule from lossless tokens. Captures remain opaque to template
//! renaming and are emitted only after repetition and scope analysis.
use super::matcher::{Capture, Captured, Captures};
use crate::{
    ast::{Span, SugarPass},
    lexer::{self, Token, TokenKind as K},
};
use std::collections::{BTreeMap, BTreeSet};
mod flow;

#[derive(Default, Debug)]
pub(super) struct Rendered {
    pub text: String,
    pub holes: Vec<Span>,
    pub origins: Vec<(Span, Span, bool)>,
    pub places: Vec<(Span, SugarPass)>,
}

struct Hole<'a> {
    cap: &'a Capture,
    value: &'a Captured,
    marker: String,
    operand: Span,
}

struct Slot {
    id: usize,
    span: Span,
    memo: Option<(String, bool)>,
}

enum Template<'a> {
    Text(&'a str),
    Hole(&'a str, bool),
    Repeat {
        body: Vec<Self>,
        names: BTreeSet<&'a str>,
        separator: &'a str,
        plus: bool,
    },
}

fn next(tokens: &[Token], mut i: usize) -> usize {
    while tokens.get(i).is_some_and(|t| t.kind.trivia()) {
        i += 1
    }
    i
}

fn parse<'a>(
    s: &'a str,
    tokens: &[Token],
    range: Span,
    depth: usize,
) -> Result<Vec<Template<'a>>, String> {
    if depth >= 64 {
        return Err("template repetition nesting limit exceeded".into());
    }
    let mut parts = Vec::new();
    let mut i = range.start;
    while i < range.end {
        let t = &tokens[i];
        if t.kind != K::Dollar {
            parts.push(Template::Text(&s[t.span.clone()]));
            i += 1;
            continue;
        }
        i = next(tokens, i + 1);
        if tokens.get(i).is_some_and(|t| t.kind == K::LParen) {
            let begin = i + 1;
            let mut level = 1;
            i = begin;
            while i < range.end {
                match tokens[i].kind {
                    K::LParen => level += 1,
                    K::RParen => level -= 1,
                    _ => {}
                }
                if level == 0 {
                    break;
                }
                i += 1;
            }
            if i == range.end {
                return Err("unclosed template repetition".into());
            }
            let body = parse(s, tokens, begin..i, depth + 1)?;
            let mut names = BTreeSet::new();
            for p in &body {
                match p {
                    Template::Hole(n, _) => {
                        names.insert(*n);
                    }
                    Template::Repeat { names: inner, .. } => names.extend(inner),
                    _ => {}
                }
            }
            i = next(tokens, i + 1);
            let separator = if tokens
                .get(i)
                .is_some_and(|t| matches!(t.kind, K::Plus | K::Star))
            {
                ""
            } else {
                let t = tokens
                    .get(i)
                    .filter(|_| i < range.end)
                    .ok_or("missing template repetition quantifier")?;
                let sep = &s[t.span.clone()];
                if !sep.bytes().all(|b| b.is_ascii_punctuation()) {
                    return Err("invalid template repetition separator".into());
                }
                i = next(tokens, i + 1);
                sep
            };
            let quantifier = tokens
                .get(i)
                .filter(|_| i < range.end)
                .ok_or("missing template repetition quantifier")?
                .kind;
            if !matches!(quantifier, K::Star | K::Plus) {
                return Err("expected * or + after template repetition".into());
            }
            parts.push(Template::Repeat {
                body,
                names,
                separator,
                plus: quantifier == K::Plus,
            });
        } else {
            let seq = tokens.get(i).is_some_and(|t| t.kind == K::DotDot);
            if seq {
                i = next(tokens, i + 1);
            }
            let t = tokens
                .get(i)
                .filter(|t| {
                    i < range.end
                        && matches!(
                            t.kind,
                            K::Lower | K::Upper | K::With | K::State | K::Eff | K::Linear
                        )
                })
                .ok_or("expected a template hole name")?;
            parts.push(Template::Hole(&s[t.span.clone()], seq));
        }
        i += 1;
    }
    Ok(parts)
}

fn bounded(out: &mut String, s: &str, max: usize) -> Result<(), String> {
    if s.len() > max.saturating_sub(out.len()) {
        return Err("sugar expansion byte limit exceeded".into());
    }
    out.push_str(s);
    Ok(())
}

struct Skeleton<'a> {
    source: String,
    holes: Vec<Hole<'a>>,
    ids: BTreeMap<(&'a str, usize), usize>,
    slots: Vec<Slot>,
    mark: &'a str,
    max: usize,
    steps: usize,
}

impl<'a> Skeleton<'a> {
    fn emit(
        &mut self,
        parts: &[Template<'a>],
        caps: &'a Captures,
        indices: &mut Vec<usize>,
    ) -> Result<(), String> {
        for p in parts {
            self.steps = self
                .steps
                .checked_sub(1)
                .ok_or("sugar template work limit exceeded")?;
            match p {
                Template::Text(text) => bounded(&mut self.source, text, self.max)?,
                Template::Hole(name, sequence) => {
                    let cap = caps
                        .get(*name)
                        .ok_or_else(|| format!("undefined template hole ${name}"))?;
                    let seq_sort = matches!(cap.sort.as_str(), "stmts" | "args" | "pats" | "items");
                    if seq_sort && !sequence || *sequence && !seq_sort && cap.sort != "scope" {
                        return Err(format!("wrong splice syntax for ${name}:{}", cap.sort));
                    }
                    let mut matched = indices.as_slice();
                    let (i, val) = loop {
                        if let Ok(i) = cap
                            .values
                            .binary_search_by(|v| v.indices.as_slice().cmp(matched))
                        {
                            break (i, &cap.values[i]);
                        }
                        if cap.extents.contains_key(matched) || matched.is_empty() {
                            return Err(format!("unbound repetition depth for ${name}"));
                        }
                        matched = &matched[..matched.len() - 1];
                    };
                    let id = *self.ids.entry((name, i)).or_insert_with(|| {
                        let id = self.holes.len();
                        self.holes.push(Hole {
                            cap,
                            value: val,
                            operand: operand_span(&val.text),
                            marker: format!("{}{id}", self.mark),
                        });
                        id
                    });
                    let start = self.source.len();
                    bounded(&mut self.source, &self.holes[id].marker, self.max)?;
                    self.slots.push(Slot {
                        id,
                        span: start..self.source.len(),
                        memo: None,
                    });
                }
                Template::Repeat {
                    body,
                    names,
                    separator,
                    plus,
                } => {
                    let mut count = None;
                    for n in names {
                        let c = caps
                            .get(*n)
                            .ok_or_else(|| format!("undefined template hole ${n}"))?;
                        if let Some(n) = c.extents.get(indices.as_slice())
                            && count.replace(*n).is_some_and(|prev| prev != *n)
                        {
                            return Err("incompatible sugar repetition lengths".into());
                        }
                    }
                    let count = count
                        .ok_or("template repetition has no captured sequence at this depth")?;
                    if *plus && count == 0 {
                        return Err("nonempty template repetition matched zero elements".into());
                    }
                    for k in 0..count {
                        if k > 0 {
                            bounded(&mut self.source, separator, self.max)?;
                        }
                        indices.push(k);
                        self.emit(body, caps, indices)?;
                        indices.pop();
                    }
                }
            }
        }
        Ok(())
    }
}

// The AST drops grouping parentheses. Use the corresponding interval for
// ownership checks, while the source map retains every byte of the capture.
fn operand_span(text: &str) -> Span {
    let (tokens, _) = lexer::lex(text);
    let tokens: Vec<_> = tokens
        .iter()
        .filter(|t| !t.kind.trivia() && t.kind != K::Eof)
        .collect();
    let mut open = Vec::new();
    let mut last = vec![None; tokens.len()];
    for (i, t) in tokens.iter().enumerate() {
        match t.kind {
            K::LParen => open.push((i, false)),
            K::Comma => {
                if let Some((_, tuple)) = open.last_mut() {
                    *tuple = true;
                }
            }
            K::RParen => {
                if let Some((j, false)) = open.pop() {
                    last[j] = Some(i);
                }
            }
            _ => {}
        }
    }
    let (mut a, mut b) = (0, tokens.len());
    while b > a + 2 && last[a] == Some(b - 1) {
        a += 1;
        b -= 1;
    }
    if b > a {
        tokens[a].span.start..tokens[b - 1].span.end
    } else {
        0..text.len()
    }
}

fn permission(hole: &Hole<'_>) -> Result<Option<SugarPass>, String> {
    match hole.cap.sort.as_str() {
        "expr" => Ok(Some(SugarPass::In)),
        "place" => Ok(Some(
            hole.cap
                .pass
                .as_deref()
                .unwrap_or("in")
                .parse()
                .map_err(|()| "invalid place pass mode".to_owned())?,
        )),
        _ => Ok(None),
    }
}

impl Rendered {
    fn hole(&mut self, h: &Hole<'_>, max: usize) -> Result<(), String> {
        let wrapping = match h.cap.sort.as_str() {
            "expr" => ("(", ")"),
            "scope" if h.value.from_tail => ("{", "}"),
            _ => ("", ""),
        };
        let outer_start = self.text.len();
        bounded(&mut self.text, wrapping.0, max)?;
        let start = self.text.len();
        bounded(&mut self.text, &h.value.text, max)?;
        let span = start..self.text.len();
        self.origins
            .push((span.clone(), h.value.span.clone(), h.value.from_tail));
        if let Some(p) = permission(h)? {
            self.places
                .push((start + h.operand.start..start + h.operand.end, p));
        }
        bounded(&mut self.text, wrapping.1, max)?;
        self.holes.push(outer_start..self.text.len());
        Ok(())
    }
}

pub(super) fn render(
    template: &str,
    captures: &Captures,
    mark: &str,
    max_bytes: usize,
) -> Result<Rendered, String> {
    let lexed =
        lexer::sugar_lex_with_limits(template, Default::default()).map_err(|e| e.to_string())?;
    if !lexed.diagnostics.is_empty() {
        return Err("invalid token in sugar template".into());
    }
    let parts = parse(template, &lexed.tokens, 0..lexed.tokens.len() - 1, 0)?;
    let mut sk = Skeleton {
        source: String::new(),
        holes: vec![],
        ids: BTreeMap::new(),
        slots: vec![],
        mark,
        max: max_bytes,
        steps: max_bytes.saturating_mul(4),
    };
    sk.emit(&parts, captures, &mut Vec::new())?;
    flow::render(sk)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repeated(values: &[(&[usize], &str)], extents: &[(&[usize], usize)]) -> Capture {
        Capture {
            sort: "expr".into(),
            pass: None,
            values: values
                .iter()
                .map(|(path, text)| Captured {
                    text: (*text).to_owned(),
                    indices: path.to_vec(),
                    span: 0..text.len(),
                    from_tail: false,
                })
                .collect(),
            extents: extents
                .iter()
                .map(|(path, n)| (path.to_vec(), *n))
                .collect(),
        }
    }

    #[test]
    fn nested_repetitions_keep_empty_rows_and_zip_only_equal_cardinalities() {
        let xs = repeated(
            &[(&[1, 0], "2"), (&[1, 1], "3")],
            &[(&[], 2), (&[0], 0), (&[1], 2)],
        );
        let caps = BTreeMap::from([("x".into(), xs)]);
        let result = render("rows($(row($($x),*)),*)", &caps, "fresh_", 4096).unwrap();
        assert_eq!(result.text, "rows(row(),row((2),(3)))");
        let mut caps = caps;
        caps.insert("y".into(), repeated(&[(&[0], "7")], &[(&[], 1)]));
        assert!(
            render("$(row($y,$($x),*)),*", &caps, "fresh_", 4096)
                .unwrap_err()
                .contains("length")
        );
    }

    #[test]
    fn nested_sugar_operands_and_generated_functions_keep_lexical_bindings() {
        let caps = BTreeMap::from([("x".into(), repeated(&[(&[], "caller()")], &[]))]);
        let body = render("{ let t=$x; inner t { t } }", &caps, "fresh_", 4096)
            .unwrap()
            .text;
        assert_eq!(
            body,
            "{ let fresh_local1=(caller()); inner fresh_local1 { fresh_local1 } }"
        );
        let item = render("fn made(t:i64)->i64{let y=$x; y+t}", &caps, "fresh_", 4096)
            .unwrap()
            .text;
        assert_eq!(
            item,
            "fn made(fresh_param1:i64)->i64{let fresh_local2=(caller()); fresh_local2+fresh_param1}"
        );
        let checked = render(
            "{ let t=$x; inner t { let t = 7; t } }",
            &caps,
            "fresh_",
            4096,
        )
        .unwrap()
        .text;
        assert_eq!(
            checked,
            "{ let fresh_local1=(caller()); inner fresh_local1 { let fresh_local2 = 7; fresh_local2 } }"
        );
    }

    #[test]
    fn or_pattern_alternatives_and_fields_share_the_correct_local_name() {
        let src =
            "match value { Either::Left(x) | Either::Right(x) => Pair { x: /*same field*/ x }, }";
        let out = render(src, &Captures::new(), "fresh_", 4096).unwrap();
        assert_eq!(
            out.text,
            src.replace("(x)", "(fresh_local1)")
                .replace("field*/ x", "field*/ fresh_local1")
        );
    }

    #[test]
    fn visibility_and_spans_follow_the_source_operand_through_grouping() {
        let mut cap = repeated(&[(&[], "((obj.field))")], &[]);
        cap.sort = "place".into();
        let caps = BTreeMap::from([("p".into(), cap)]);
        assert!(render("write(inout $p)", &caps, "fresh_", 4096).is_err());
        assert!(
            render("{let y=1; inner inout $p {y}}", &caps, "fresh_", 4096)
                .unwrap_err()
                .contains("upgrades")
        );
        let out = render("read(in $p)", &caps, "fresh_", 4096).unwrap();
        assert_eq!(&out.text[out.places[0].0.clone()], "obj.field");
        assert_eq!(&out.text[out.origins[0].0.clone()], "((obj.field))");
    }
}
