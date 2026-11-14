//! Checker rules for region operations (design §15.2). This file is a child
//! module of `check.rs` so that it types operands with the checker's own
//! ownership and capability machinery; the rules are documented on
//! [`crate::regions`].
use super::*;
use crate::regions::{self, REGION_EFFECT};

impl Checker<'_> {
    /// The built-in region capability a source label names, if any.
    pub(super) fn region_capability(&self, label: &str) -> Option<Effect> {
        if !crate::regions::active(self.world) {
            return None;
        }
        let target = self
            .cap_aliases
            .get(label)
            .map(String::as_str)
            .unwrap_or(label);
        self.caps
            .iter()
            .rev()
            .chain(self.allowed.instances.iter())
            .find(|e| e.label.as_deref() == Some(target))
            .filter(|e| e.name == REGION_EFFECT)
            .cloned()
    }

    /// Rewrites source region labels in an annotation to capabilities.
    pub(super) fn region_annotation(&self, ty: &Type) -> Type {
        if !crate::regions::active(self.world) {
            return ty.clone();
        }
        regions::normalize(ty, &self.cap_aliases)
    }

    /// [T-RegionAlloc] and the access rules: types `r.op(args)`.
    pub(super) fn region_op(
        &mut self,
        effect: &Effect,
        op: &str,
        args: &[Arg],
        env: &mut Env,
        span: Span,
        expected: &Type,
    ) -> Type {
        let Some(cap) = effect.label.clone() else {
            return Type::Infer;
        };
        let arity = match op {
            "checkpoint" | "used" => 0,
            "box" | "get" | "take" | "rewind" | "commit" => 1,
            "set" => 2,
            _ => usize::MAX,
        };
        if args.len() != arity {
            let message = if arity == usize::MAX {
                format!("unknown region operation '{op}'")
            } else {
                format!(
                    "region operation '{op}' expects {arity} argument{}, found {}",
                    if arity == 1 { "" } else { "s" },
                    args.len()
                )
            };
            self.diagnostics
                .push(Diagnostic::new("E0790", message, span.clone()).with_help(
                    "region operations: box, get, set, take, checkpoint, rewind, commit, used",
                ));
            for a in args {
                self.expr(&a.expr, env, Usage::Own);
            }
            return Type::Infer;
        }
        for a in args {
            if a.explicit && a.pass == Pass::Inout {
                self.error(
                    "E0308",
                    "region operations do not take inout arguments",
                    a.expr.span.clone(),
                );
            }
        }
        let result = match op {
            "box" => {
                let hint = regions::box_parts(expected)
                    .map(|(_, p)| p.clone())
                    .unwrap_or(Type::Infer);
                let actual = self.expr_expected(&args[0].expr, env, Usage::Own, &hint);
                if hint != Type::Infer {
                    self.expect_type(&hint, &actual, args[0].expr.span.clone());
                }
                let payload = if hint == Type::Infer {
                    crate::numeric::default_type(&actual)
                } else {
                    refine_annotation(&hint, &actual)
                };
                if unresolved(&payload) || payload.name() == "Never" {
                    self.error(
                        "E0208",
                        "cannot infer the type of a region payload; add a type annotation",
                        args[0].expr.span.clone(),
                    );
                }
                regions::box_type(&cap, payload)
            }
            "get" | "set" | "take" => {
                let usage = if op == "take" {
                    Usage::Own
                } else {
                    Usage::Borrow(Pass::In)
                };
                let boxed = self.expr(&args[0].expr, env, usage);
                let Some((label, payload)) = regions::box_parts(&boxed) else {
                    if boxed != Type::Infer {
                        self.error(
                            "E0798",
                            format!(
                                "region operation '{op}' expects a RegionBox, found {}",
                                regions::display(&boxed)
                            ),
                            args[0].expr.span.clone(),
                        );
                    }
                    for a in &args[1..] {
                        self.expr(&a.expr, env, Usage::Own);
                    }
                    self.require_effect(effect, env, span);
                    return Type::Infer;
                };
                let payload = payload.clone();
                if label != cap {
                    self.diagnostics.push(
                        Diagnostic::new(
                            "E0792",
                            format!(
                                "this box belongs to {}, not to the region whose capability performs '{op}'",
                                regions::display(&regions::region_row(label))
                            ),
                            args[0].expr.span.clone(),
                        )
                        .with_help("access a box through the capability of the region that allocated it"),
                    );
                }
                let mode = self.mode(&payload);
                match op {
                    "get" | "set" if mode != Mode::Copy => {
                        self.diagnostics.push(
                            Diagnostic::new(
                                "E0793",
                                format!(
                                    "'{op}' needs a copy payload, but {} is {mode:?}",
                                    regions::display(&payload)
                                ),
                                args[0].expr.span.clone(),
                            )
                            .with_help("move an affine or linear payload out with take"),
                        );
                    }
                    "take" if mode == Mode::Copy => {
                        self.diagnostics.push(
                            Diagnostic::new(
                                "E0794",
                                format!(
                                    "'take' moves out of a unique box, but {} is copy and its box may be aliased",
                                    regions::display(&payload)
                                ),
                                args[0].expr.span.clone(),
                            )
                            .with_help("read a copy payload with get"),
                        );
                    }
                    _ => {}
                }
                match op {
                    "set" => {
                        let value = self.expr_expected(&args[1].expr, env, Usage::Own, &payload);
                        self.expect_type(&payload, &value, args[1].expr.span.clone());
                        Type::unit()
                    }
                    _ => payload,
                }
            }
            "checkpoint" => regions::watermark_type(&cap),
            "rewind" | "commit" => {
                let mark = self.expr(&args[0].expr, env, Usage::Own);
                match regions::watermark_label(&mark) {
                    Some(l) if l == cap => {}
                    Some(l) => self.diagnostics.push(Diagnostic::new(
                        "E0792",
                        format!(
                            "this watermark belongs to {}, not to the region performing '{op}'",
                            regions::display(&regions::region_row(l))
                        ),
                        args[0].expr.span.clone(),
                    )),
                    None if mark != Type::Infer => self.error(
                        "E0798",
                        format!(
                            "region operation '{op}' expects a Watermark, found {}",
                            regions::display(&mark)
                        ),
                        args[0].expr.span.clone(),
                    ),
                    None => {}
                }
                if op == "rewind" {
                    let live = env
                        .bindings
                        .iter()
                        .filter(|(_, b)| !b.moved.iter().any(Vec::is_empty))
                        .filter_map(|(n, b)| {
                            let (l, p) = regions::box_parts(&b.ty)?;
                            (l == cap && self.mode(p) == Mode::Linear)
                                .then(|| (n.clone(), b.span.clone()))
                        })
                        .collect::<Vec<_>>();
                    for (name, at) in live {
                        self.diagnostics.push(
                            Diagnostic::new(
                                "E0795",
                                format!(
                                    "cannot rewind while the linear region payload '{name}' is live"
                                ),
                                span.clone(),
                            )
                            .with_related(at, "linear box bound here")
                            .with_help("take and consume the linear payload before rewinding"),
                        );
                    }
                }
                Type::unit()
            }
            _ => Type::named("u64"),
        };
        self.require_effect(effect, env, span);
        result
    }

    /// Region-block handling inside [`Checker::handle`]: `Some(capacity)`
    /// for a parser-produced region block, an error for a hand-written
    /// handler of the built-in effect.
    pub(super) fn region_block(
        &mut self,
        effect: &Effect,
        ret: &Clause,
        span: Span,
    ) -> Result<Option<usize>, ()> {
        if effect.name != REGION_EFFECT || !crate::regions::active(self.world) {
            return Ok(None);
        }
        match regions::block_capacity(effect, ret) {
            Some(capacity) => Ok(Some(capacity)),
            None => {
                self.diagnostics.push(
                    Diagnostic::new(
                        "E0797",
                        "the built-in Region effect cannot be handled by hand",
                        span,
                    )
                    .with_help("open a lexical region with `region r { ... }`"),
                );
                Err(())
            }
        }
    }
}
