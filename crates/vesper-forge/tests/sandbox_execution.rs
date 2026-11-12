#![cfg(feature = "sandbox-transport")]

mod common;

use common::*;
use std::sync::OnceLock;
use vesper_forge::*;
use vesper_loom_sandbox::{
    Invocation, SandboxHost, StepComponent, StepConfig, StepKind, TransportLimits, transport as t,
};

fn sandbox() -> &'static (SandboxHost, StepComponent) {
    static HOST: OnceLock<(SandboxHost, StepComponent)> = OnceLock::new();
    HOST.get_or_init(|| {
        let host = SandboxHost::new().unwrap();
        let component = host
            .compile(include_bytes!(
                "../../vesper-loom-sandbox/fixtures/bin/step.component.wasm"
            ))
            .unwrap();
        (host, component)
    })
}

/// The real component echoes adversarial wire tokens, including forged caller scope/span
/// claims. The host must treat the returned flat bytes as macro emission, not splice evidence.
fn run_guest(source: &str, caller_scope: u32) -> t::Outcome {
    let parent = tempfile::tempdir().unwrap();
    let output = parent.path().join("out");
    std::fs::create_dir(&output).unwrap();
    let mut invocation = Invocation::new(StepKind::ProcMacro, "echo");
    invocation.tokens = lex_fragment(source)
        .into_iter()
        .map(|token| t::Token {
            kind: match token.kind {
                TokenKind::Ident => t::TokenKind::Ident,
                TokenKind::Punct => t::TokenKind::Punct,
                TokenKind::Int => t::TokenKind::IntLiteral,
                TokenKind::Str => t::TokenKind::StringLiteral,
                TokenKind::Open(Delim::Paren) => t::TokenKind::Open(t::Delimiter::Paren),
                TokenKind::Close(Delim::Paren) => t::TokenKind::Close(t::Delimiter::Paren),
                _ => panic!("unexpected delimiter in test fixture"),
            },
            text: token.text,
            scopes: vec![caller_scope],
            span: t::Span {
                file: 4,
                start: 12,
                end: 14,
            },
            pre_resolved: None,
        })
        .collect();
    let (host, component) = sandbox();
    let report = host
        .run(component, &StepConfig::new(output), &invocation)
        .unwrap();
    assert!(report.fuel_consumed > 0);
    let outcome = report.result.unwrap();
    assert_eq!(outcome.tokens, invocation.tokens);
    assert!(
        outcome
            .diagnostics
            .iter()
            .any(|d| d.message == "echo as proc-macro")
    );
    outcome
}

#[test]
fn real_wasi_output_cannot_claim_caller_provenance_or_bypass_authority() {
    for (source, privileged) in [("hash_into(1)", false), ("phone_home()", true)] {
        let mut host = host();
        let caller_scope = host.module_scope(APP).unwrap();
        let expansion = host
            .begin_expansion(DERIVE_MACRO, InvocationSite::Source(APP), vec![])
            .unwrap();
        let outcome = run_guest(source, caller_scope.index());
        let tokens = host
            .finish_flat_outcome(expansion.id, &outcome, &TransportLimits::default())
            .unwrap();
        assert!(
            tokens
                .iter()
                .all(|token| token.origin() == Origin::Expansion(expansion.id))
        );
        assert!(
            tokens
                .iter()
                .all(|token| !token.scopes().contains(caller_scope))
        );
        let fragment = host.resolve(tokens).unwrap();
        let rejected = check_authority(
            host.session(),
            fragment.clone(),
            &graph(),
            &policies(vec![]),
        );
        if privileged {
            let rejection = rejected.unwrap_err();
            assert!(
                rejection
                    .violations
                    .iter()
                    .any(|violation| matches!(violation,
                Violation::Exceeds { requirement, .. } if *requirement == net()))
            );
            let accepted = check_authority(
                host.session(),
                fragment,
                &graph(),
                &policies(vec![exact(net())]),
            )
            .unwrap();
            assert!(
                accepted
                    .audit
                    .operations
                    .iter()
                    .any(|operation| operation.requirements.contains(&net()))
            );
        } else {
            assert!(
                rejected
                    .unwrap()
                    .audit
                    .operations
                    .iter()
                    .all(|operation| operation.requirements.is_empty())
            );
        }
    }
}

#[test]
fn real_wasi_echo_does_not_turn_caller_locals_into_authenticated_splices() {
    let mut host = host();
    let caller_scope = host.module_scope(APP).unwrap();
    let mut program = host.lex_source(APP, "let x = 1;").unwrap();
    let expansion = host
        .begin_expansion(DERIVE_MACRO, InvocationSite::Source(APP), vec![])
        .unwrap();
    let outcome = run_guest("x", caller_scope.index());
    program.extend(
        host.finish_flat_outcome(expansion.id, &outcome, &TransportLimits::default())
            .unwrap(),
    );
    assert!(matches!(host.resolve(program), Err(ForgeError::Unbound { name, .. }) if name == "x"));
}
