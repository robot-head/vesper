// The AST optimizer's producer and checker (SH3 lane 10, #1249), from
// `modules/optimize`, through `optimize`'s table.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

#[rustfmt::skip]
#[allow(clippy::all)]
#[path = "../../port/optimize.rs"]
mod optimize_binding;

// The AST optimizer swap's input encoder and answer readers (#1249),
// shared with the root package's tests/self_host_optimize*.rs.
#[path = "../../../../tests/common/optimize_words.rs"]
mod optimize_words;

use super::{Registry, Swap, must, on, pass};

/// Expressions the Vesper optimizer proposed for, and proposals it checked.
static OPTIMIZE_PROPOSE_CALLS: AtomicU64 = AtomicU64::new(0);
static OPTIMIZE_VERIFY_CALLS: AtomicU64 = AtomicU64::new(0);

/// `optimize`'s producer from `modules/optimize`: `propose_rewrite` of
/// `@vesper/optimize` on `@vesper/egraph`. The package searches, extracts
/// and proves; the candidate is rebuilt from the returned certificate by
/// `witness::rebuild_candidate`, the builder the Rust producer uses.
fn vesper_propose(
    source: &vesper::ast::Expr,
    types: &vesper::ast::TypeMap,
    limits: vesper::optimize::witness::WitnessLimits,
) -> Result<vesper::optimize::Proposal, vesper::optimize::witness::WitnessError> {
    use vesper::optimize::{Produced, Proposal};
    let types = optimize_words::relevant_types(&[source], types);
    let mut batch = optimize_words::Batch::new(1, true);
    batch.propose(source, &types, limits);
    OPTIMIZE_PROPOSE_CALLS.fetch_add(1, Relaxed);
    let words = optimize_binding::propose_words(&batch.bytes, &batch.names);
    let text = optimize_words::unpack_text(&words);
    let head = text.lines().next().unwrap_or("");
    match head {
        "P B" => Ok(Proposal::Barrier),
        "P U" => Ok(Proposal::Unchanged),
        _ if head.starts_with("P E ") => Err(optimize_words::error_of(
            head[4..].parse().expect("an error code"),
        )),
        _ => {
            let (mut certificate, spent, (terms_cap, proofs_cap)) =
                optimize_words::parse_certificate(&text);
            // The gate charges by the vectors' capacities, as the Rust
            // producer leaves them.
            certificate
                .terms
                .reserve_exact(terms_cap.saturating_sub(certificate.terms.len()));
            certificate
                .proofs
                .reserve_exact(proofs_cap.saturating_sub(certificate.proofs.len()));
            let candidate =
                vesper::optimize::witness::rebuild_candidate(source, &types, &certificate, limits)?;
            Ok(Proposal::Candidate(Box::new(Produced {
                candidate,
                certificate,
                limits,
                spent,
            })))
        }
    }
}

/// `optimize`'s install gate from `modules/optimize`: the certificate
/// checker of `@vesper/optimize`, continuing the producer's budget.
fn vesper_verify(
    source: &vesper::ast::Expr,
    candidate: &vesper::ast::Expr,
    types: &vesper::ast::TypeMap,
    certificate: &vesper::optimize::witness::RewriteCertificate,
    limits: vesper::optimize::witness::WitnessLimits,
    spent: &mut vesper::optimize::witness::Spent,
) -> Result<(), vesper::optimize::witness::WitnessError> {
    let types = optimize_words::relevant_types(&[source, candidate], types);
    let mut batch = optimize_words::Batch::new(1, true);
    batch.verify(
        source,
        candidate,
        &types,
        certificate,
        (certificate.terms.capacity(), certificate.proofs.capacity()),
        limits,
        *spent,
        false,
    );
    OPTIMIZE_VERIFY_CALLS.fetch_add(1, Relaxed);
    let words = optimize_binding::verify_words(&batch.bytes, &batch.names);
    let text = optimize_words::unpack_text(&words);
    let (result, after) =
        optimize_words::parse_verify_line(text.lines().next().expect("an R line"));
    *spent = after;
    result
}

const VESPER_OPTIMIZE: vesper::optimize::OptimizeTable = vesper::optimize::OptimizeTable {
    verify: vesper_verify,
    propose: Some(vesper_propose),
};

#[cfg(test)]
mod tests {
    use vesper::ast::{Arg, Expr, ExprKind, Literal, Pass, Type, TypeMap};
    use vesper::optimize::witness::WitnessLimits;

    use super::{optimize_binding, optimize_words};

    fn expr(kind: ExprKind, at: usize, len: usize) -> Expr {
        Expr {
            kind,
            span: at..at + len,
        }
    }

    fn typed(e: &Expr, t: &str, types: &mut TypeMap) {
        types.insert((e.span.start, e.span.end), Type::Named(t.into(), vec![]));
    }

    /// A split batch of several cases reads each case's names after the
    /// previous case's, so it answers as the cases do one by one.
    #[test]
    fn split_batches_keep_the_names_cursor() {
        let mut types = TypeMap::new();
        // x + 0
        let x = expr(ExprKind::Name("x".into()), 0, 1);
        let zero = expr(ExprKind::Literal(Literal::Int(0)), 4, 1);
        typed(&x, "u32", &mut types);
        typed(&zero, "u32", &mut types);
        let first = expr(
            ExprKind::Binary("+".into(), Box::new(x), Box::new(zero)),
            0,
            5,
        );
        typed(&first, "u32", &mut types);
        // wrapping_add(y, 0)
        let callee = expr(ExprKind::Name("wrapping_add".into()), 10, 12);
        let y = expr(ExprKind::Name("y".into()), 23, 1);
        let zero = expr(ExprKind::Literal(Literal::Int(0)), 26, 1);
        typed(&y, "u32", &mut types);
        typed(&zero, "u32", &mut types);
        let arg = |expr| Arg {
            pass: Pass::In,
            explicit: false,
            expr,
        };
        let second = expr(
            ExprKind::Call {
                callee: Box::new(callee),
                types: vec![],
                args: vec![arg(y), arg(zero)],
            },
            10,
            18,
        );
        typed(&second, "u32", &mut types);
        let limits = WitnessLimits::default();
        let propose = |cases: &[&Expr]| {
            let mut batch = optimize_words::Batch::new(cases.len(), true);
            for e in cases {
                batch.propose(e, &optimize_words::relevant_types(&[e], &types), limits);
            }
            optimize_words::unpack_text(&optimize_binding::propose_words(
                &batch.bytes,
                &batch.names,
            ))
        };
        let one_by_one = propose(&[&first]) + &propose(&[&second]);
        assert_eq!(propose(&[&first, &second]), one_by_one);
        assert!(!one_by_one.contains("P E "), "{one_by_one}");
    }
}

/// The flip-gate rows of this file's swaps (`swaps::on`).
pub(super) const SWAPS: &[Swap] = &[pass("optimize", "optimize", "#1501")];

pub(super) fn register(_r: &mut Registry) {
    // `VESPER_HYBRID_RUST_PRODUCER=1` keeps the Rust producer behind the
    // Vesper gate: the differential's check of the optimizer's accepted
    // equal-cost ties (tests/differential.rs).
    let table = if std::env::var_os("VESPER_HYBRID_RUST_PRODUCER").is_some_and(|v| v == "1") {
        vesper::optimize::OptimizeTable {
            propose: None,
            ..VESPER_OPTIMIZE
        }
    } else {
        VESPER_OPTIMIZE
    };
    if on("optimize") {
        must(vesper::optimize::install_optimize(table));
    }
}

pub(super) fn report() {
    eprintln!(
        "vesper-hybrid: optimize proposed for {} expressions and checked {} proposals from Vesper",
        OPTIMIZE_PROPOSE_CALLS.load(Relaxed),
        OPTIMIZE_VERIFY_CALLS.load(Relaxed)
    );
}
