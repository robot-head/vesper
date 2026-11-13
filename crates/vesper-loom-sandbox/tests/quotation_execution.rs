use sha2::{Digest, Sha256};
use std::sync::OnceLock;
use vesper_loom_sandbox::{
    SandboxError, SandboxHost, StepComponent, StepConfig, quote_host::Error, quote_transport as q,
    transport::TokenKind as K,
};
const BYTES: &[u8] = include_bytes!("../fixtures/quotation/bin/quotation.component.wasm");
fn runtime() -> &'static (SandboxHost, StepComponent) {
    static HOST: OnceLock<(SandboxHost, StepComponent)> = OnceLock::new();
    HOST.get_or_init(|| {
        let host = SandboxHost::new().unwrap();
        let component = host.compile(BYTES).unwrap();
        (host, component)
    })
}
fn inv(entry: &str) -> q::Invocation {
    q::Invocation {
        protocol_version: 1,
        kind: q::MacroKind::ProcMacro,
        entry: entry.into(),
        args: vec![],
        slots: vec![q::InputSlot {
            category: q::Category::Expr,
            tokens: vec![q::InputToken {
                kind: K::Ident,
                text: "x".into(),
                reported_span: None,
            }],
        }],
    }
}
fn run(entry: &str) -> Result<vesper_loom_sandbox::quote_host::QuotationReport, Error> {
    let dir = tempfile::tempdir().unwrap();
    let cfg = StepConfig::new(dir.path());
    let (h, c) = runtime();
    h.run_quotation(c, &cfg, &inv(entry), q::Limits::default())
}
#[test]
fn committed_fixture_checksum_and_actual_structured_run() {
    let expected = include_str!("../fixtures/quotation/bin/SHA256SUMS")
        .split_whitespace()
        .next()
        .unwrap();
    let actual = format!("{:x}", Sha256::digest(BYTES));
    assert_eq!(actual, expected);
    assert!(BYTES.len() > 1000);
    let report = run("identity").unwrap();
    assert!(report.fuel_consumed > 0);
    assert!(report.peak_memory_bytes > 0);
    assert!(report.outputs.is_empty());
    let out = report.result.unwrap();
    assert_eq!(out.items, vec![q::Item::InputSlot(0)]);
    assert_eq!(out.diagnostics[0].message, "actual structured guest");
    assert!(matches!(
        run("duplicate").unwrap().result.unwrap().items[1],
        q::Item::InputSlot(0)
    ));
}
#[test]
fn actual_hostile_wire_returns_fail_before_typed_approval() {
    for (entry, kind) in [
        ("forged-slot", q::Error::Slot),
        ("bad-kind", q::Error::Token),
        ("bad-utf8", q::Error::Utf8),
        ("bad-slice", q::Error::Slice),
        ("alias-bomb", q::Error::Budget(q::Resource::Text)),
        ("slot-bomb", q::Error::Budget(q::Resource::References)),
        ("failure-items", q::Error::ItemsInFailure),
    ] {
        assert!(
            matches!(run(entry),Err(Error::Transport(e))if e==kind),
            "{entry}"
        );
    }
    assert!(run("fail").unwrap().result.is_err());
    let out = run("claim").unwrap().result.unwrap();
    let q::Item::Generated(token) = &out.items[0] else {
        panic!()
    };
    assert_eq!(token.claimed_def.unwrap().package, 999);
}
#[test]
fn actual_sandbox_controls_and_determinism_are_shared() {
    let a = run("random").unwrap().result.unwrap();
    let b = run("random").unwrap().result.unwrap();
    assert_eq!(a, b);
    let clock = run("clock").unwrap().result.unwrap();
    assert!(matches!(&clock.items[0],q::Item::Generated(t)if t.text=="0"));
    let denied = run("authority-denied").unwrap().result.unwrap();
    assert!(matches!(&denied.items[0],q::Item::Generated(t)if t.text=="1"));
    let report = run("write-out").unwrap();
    assert_eq!(report.outputs.len(), 1);
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = StepConfig::new(dir.path());
    let (h, c) = runtime();
    cfg.limits.fuel = 100_000;
    assert!(matches!(
        h.run_quotation(c, &cfg, &inv("spin"), q::Limits::default()),
        Err(Error::Sandbox(SandboxError::FuelExhausted { .. }))
    ));
    cfg.limits.fuel = 100_000_000;
    cfg.limits.max_memory_bytes = 64 << 20;
    assert!(matches!(
        h.run_quotation(c, &cfg, &inv("memory"), q::Limits::default()),
        Err(Error::Sandbox(SandboxError::MemoryLimitExceeded { .. }))
    ));
    cfg.limits.max_stdio_bytes = 1024;
    assert!(
        h.run_quotation(c, &cfg, &inv("stdio"), q::Limits::default())
            .is_err()
    );
}
#[test]
fn legacy_component_cannot_link_as_quotation() {
    let dir = tempfile::tempdir().unwrap();
    let (h, _) = runtime();
    let old = h
        .compile(include_bytes!("../fixtures/bin/step.component.wasm"))
        .unwrap();
    assert!(matches!(
        h.run_quotation(
            &old,
            &StepConfig::new(dir.path()),
            &inv("identity"),
            q::Limits::default()
        ),
        Err(Error::Sandbox(SandboxError::Link { .. }))
    ));
}
