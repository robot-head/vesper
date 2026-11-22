//! Enforce the capabilities of an instantiated sugar rule on resolved core
//! operations. Caller expressions keep their original lexical permissions.
use super::*;

struct Entry<'a> {
    region: &'a SugarEffectRegion,
    max_end: usize,
}

#[derive(Default)]
pub(super) struct Ceilings<'a>(Vec<Entry<'a>>);

fn contains(outer: &Span, inner: &Span) -> bool {
    outer.start <= inner.start && inner.end <= outer.end
}

impl<'a> Ceilings<'a> {
    pub(super) fn new(regions: &'a [SugarEffectRegion]) -> Self {
        let mut entries: Vec<_> = regions
            .iter()
            .map(|region| Entry { region, max_end: 0 })
            .collect();
        entries.sort_unstable_by_key(|e| e.region.expanded.start);
        let mut end = 0;
        for entry in &mut entries {
            end = end.max(entry.region.expanded.end);
            entry.max_end = end;
        }
        Self(entries)
    }

    pub(super) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    fn enclosing(&self, site: &Span) -> impl Iterator<Item = &SugarEffectRegion> {
        let last = self
            .0
            .partition_point(|entry| entry.region.expanded.start <= site.start);
        self.0[..last]
            .iter()
            .rev()
            .take_while(|entry| site.end <= entry.max_end)
            .map(|entry| entry.region)
            .filter(move |r| contains(&r.expanded, site))
    }

    fn check_effect(
        &self,
        site: &Span,
        name: &str,
        permitted: impl Fn(&Effects) -> bool,
    ) -> Option<Diagnostic> {
        let region = self.enclosing(site).find(|region| {
            !region.holes.iter().any(|hole| contains(hole, site)) && !permitted(&region.emits)
        })?;
        Some(
            Diagnostic::new(
                "E0144",
                format!("sugar rule introduces effect '{name}' outside its declared emits row"),
                region.call.clone(),
            )
            .with_related(region.rule.clone(), "effect introduced by this rule"),
        )
    }

    pub(super) fn effect(&self, site: &Span, effect: &Effect) -> Option<Diagnostic> {
        self.check_effect(site, &effect.name, |row| {
            row.instances.iter().any(|allowed| {
                allowed.name == effect.name
                    && (allowed.args.is_empty() || allowed.args == effect.args)
                    && (allowed.label.is_none() || allowed.label == effect.label)
            })
        })
    }

    pub(super) fn open_row(&self, site: &Span, name: &str) -> Option<Diagnostic> {
        // A sugar declaration has no effect-row parameters to bind an open row.
        self.check_effect(site, name, |_| false)
    }

    /// A primitive can access a place directly, without a resolved function
    /// signature. Operations nested inside a larger captured expression belong
    /// to the caller. Appending a field to the captured place still borrows it.
    pub(super) fn captured_access(&self, site: &Span, requested: SugarPass) -> Option<Diagnostic> {
        for region in self.enclosing(site) {
            if let Some((_, pass)) = region
                .places
                .iter()
                .find(|(p, _)| p.start == site.start && p.end <= site.end)
                && !Self::permits(*pass, requested)
            {
                return Some(Self::access_error(region, *pass, requested));
            }
        }
        None
    }

    fn permits(floor: SugarPass, requested: SugarPass) -> bool {
        // On an initialized capture, the same core assignment replaces its value.
        floor.permits(if requested == SugarPass::Out && floor != SugarPass::Out {
            SugarPass::Inout
        } else {
            requested
        })
    }

    fn access_error(
        region: &SugarEffectRegion,
        floor: SugarPass,
        requested: SugarPass,
    ) -> Diagnostic {
        Diagnostic::new(
            "E0143",
            format!(
                "sugar rule requests {requested:?} access to a caller place passed as {floor:?}"
            ),
            region.call.clone(),
        )
        .with_related(region.rule.clone(), "access introduced by this rule")
    }

    pub(super) fn access(
        &self,
        operation: &Span,
        arg: &Expr,
        requested: SugarPass,
    ) -> Option<Diagnostic> {
        if self.0.is_empty()
            || !matches!(
                arg.kind,
                ExprKind::Name(_) | ExprKind::Field(..) | ExprKind::Index(..)
            )
        {
            return None;
        }
        for region in self.enclosing(operation) {
            // An operation inside a caller-supplied expression is the caller's.
            if region.holes.iter().any(|h| contains(h, operation)) {
                continue;
            }
            let mut base = arg;
            loop {
                if let Some((_, floor)) = region
                    .places
                    .iter()
                    .find(|(hole, _)| contains(hole, &base.span))
                    && !Self::permits(*floor, requested)
                {
                    return Some(Self::access_error(region, *floor, requested));
                }
                match &base.kind {
                    ExprKind::Field(parent, _) | ExprKind::Index(parent, _) => base = parent,
                    _ => break,
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forge_frontend::parse_compiler_source;

    fn diagnostics(source: &str) -> Vec<Diagnostic> {
        let (_, parsed) = parse_compiler_source(source, false).unwrap();
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        check(&parsed.module).diagnostics
    }

    #[test]
    fn effect_ceiling_distinguishes_introduced_calls_from_caller_arguments() {
        let base = "effect Log { fn event(); } fn helper() eff [Log] { Log::event(); }";
        let source = format!(
            "{base} sugar emit:expr {{ rule (emit $b:block) => {{ helper() }} }} fn main() eff [Log] {{ emit {{}} }}"
        );
        let errors = diagnostics(&source);
        let error = errors.iter().find(|d| d.code == "E0144").unwrap();
        assert_eq!(&source[error.span.clone()], "emit {}");
        assert!(source[error.related[0].span.clone()].starts_with("rule"));
        let allowed = source.replace("emit:expr", "emit:expr emits eff [Log]");
        assert!(diagnostics(&allowed).is_empty());
        let captured = source
            .replace("=> { helper() }", "=> { $b }")
            .replace("emit {} }", "emit { helper() } }");
        assert!(diagnostics(&captured).is_empty());
    }

    #[test]
    fn resolved_parameter_modes_cannot_upgrade_a_place_capture() {
        let source = "fn change(inout n:i64){n=1;} sugar alter:expr {rule(alter $p:place $b:block)=>{change($p)}} fn main(){let mut n=0;alter n {} }";
        let denied = diagnostics(source);
        assert!(denied.iter().any(|d| d.code == "E0143"), "{denied:?}");
        let allowed = source
            .replace("alter $p:place", "alter inout $p:place")
            .replace("alter n {}", "alter inout n {}");
        let errors = diagnostics(&allowed);
        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn builtin_sequence_methods_respect_capture_modes() {
        let source = "sugar clear_seq:expr {rule(clear_seq $p:place $b:block)=>{$p.clear()}} fn main(){let mut xs = ChunkSeq::new[i64](); clear_seq xs {}}";
        let denied = diagnostics(source);
        assert!(denied.iter().any(|d| d.code == "E0143"), "{denied:?}");
        let allowed = source
            .replace("clear_seq $p:place", "clear_seq inout $p:place")
            .replace("clear_seq xs {}", "clear_seq inout xs {}");
        let errors = diagnostics(&allowed);
        assert!(errors.is_empty(), "{errors:?}");
    }
}
