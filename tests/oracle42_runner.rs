//! Runner invariants; the independent acceptance target owns the full real Lean matrix.
#[path = "../src/bin/arithmetic_oracle42/mod.rs"]
pub mod oracle;

use oracle::process::testing::{ProbeKind, ProbeLimits, probe};
use oracle::*;
use std::io::{self, Cursor, Read, Write};
use std::process::Command;

struct ShortReader {
    bytes: Cursor<Vec<u8>>,
    limit: usize,
}
impl Read for ShortReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let n = out.len().min(self.limit);
        self.bytes.read(&mut out[..n])
    }
}
struct ShortWriter {
    bytes: Vec<u8>,
    limit: usize,
    flushes: usize,
}
impl Write for ShortWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let n = bytes.len().min(self.limit);
        self.bytes.extend_from_slice(&bytes[..n]);
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.flushes += 1;
        Ok(())
    }
}

#[test]
fn canonical_i128_worker_data_never_passes_through_json_numbers() {
    for value in [
        i128::MIN,
        -18_446_744_073_709_551_616,
        -9_223_372_036_854_775_809,
        0,
        i128::from(u64::MAX) + 1,
        i128::MAX,
    ] {
        let node = Node::Int(value);
        let bytes = serde_json::to_vec(&node).unwrap();
        let text = String::from_utf8(bytes.clone()).unwrap();
        assert!(text.contains(&format!("\"{value}\"")));
        assert_eq!(serde_json::from_slice::<Node>(&bytes).unwrap(), node);
        let input = Input {
            kind: Kind::I64,
            value,
        };
        let bytes = serde_json::to_vec(&input).unwrap();
        assert_eq!(serde_json::from_slice::<Input>(&bytes).unwrap(), input);
        let outcome = Outcome::Value(value);
        let bytes = serde_json::to_vec(&outcome).unwrap();
        assert_eq!(serde_json::from_slice::<Outcome>(&bytes).unwrap(), outcome);
    }
    for bad in [
        r#"{"int":18446744073709551616}"#,
        r#"{"int":"01"}"#,
        r#"{"int":"-0"}"#,
        r#"{"int":"+1"}"#,
        r#"{"int":"1e2"}"#,
        r#"{"int":"170141183460469231731687303715884105728"}"#,
    ] {
        assert!(serde_json::from_str::<Node>(bad).is_err(), "{bad}");
    }
}

#[test]
fn blocking_codec_handles_every_short_header_and_payload_boundary() {
    let payload = br#"[1,"overflow",0,3]"#;
    for stride in 1..=5 {
        let mut writer = ShortWriter {
            bytes: Vec::new(),
            limit: stride,
            flushes: 0,
        };
        protocol::write_frame(&mut writer, payload, 512).unwrap();
        assert_eq!(writer.flushes, 1);
        let expected = writer.bytes.clone();
        for limit in 1..=5 {
            let mut reader = ShortReader {
                bytes: Cursor::new(expected.clone()),
                limit,
            };
            assert_eq!(
                protocol::read_frame(&mut reader, 512).unwrap().unwrap(),
                payload
            );
            assert!(protocol::read_frame(&mut reader, 512).unwrap().is_none());
        }
        for cut in 1..expected.len() {
            let mut reader = Cursor::new(expected[..cut].to_vec());
            assert!(protocol::read_frame(&mut reader, 512).is_err());
        }
    }
    let mut output = ShortWriter {
        bytes: vec![9],
        limit: 1,
        flushes: 0,
    };
    assert!(protocol::write_frame(&mut output, payload, 1).is_err());
    assert_eq!(output.bytes, [9]);
    let mut header = Cursor::new([513u32.to_le_bytes().as_slice(), b"unchanged payload"].concat());
    assert!(protocol::read_frame(&mut header, 512).is_err());
    assert_eq!(header.position(), 4);
}

#[test]
fn shape_validation_preserves_language_negatives_but_refuses_bad_postfix() {
    let mut p = Program {
        kind: Kind::U8,
        inputs: vec![],
        locals: vec![],
        result: vec![Node::Variable(Scope::Local, u16::MAX)],
        allow_fail: true,
    };
    assert!(p.validate().is_ok());
    p.result = vec![Node::Call(Callee::Intrinsic(Op::Add), 0)];
    assert!(p.validate().is_ok());
    p.result = vec![Node::Int(0), Node::Int(1)];
    assert!(p.validate().is_err());
    p.result = vec![Node::Checked(Op::Add)];
    assert!(p.validate().is_err());
    p.result = vec![Node::Call(Callee::Intrinsic(Op::Add), 4)];
    assert!(p.validate().is_err());
    p.result = vec![Node::Int(0)];
    for _ in 0..8 {
        p.result.extend([Node::Int(0), Node::Checked(Op::Add)]);
    }
    assert!(p.validate().is_err());
    p.result = vec![Node::Int(0)];
    p.inputs.push(Input {
        kind: Kind::U8,
        value: 256,
    });
    assert!(p.validate().is_err());
}

#[test]
fn request_tags_and_decimal_literals_bind_exact_data() {
    let p = Program {
        kind: Kind::U64,
        inputs: vec![Input {
            kind: Kind::U64,
            value: u64::MAX.into(),
        }],
        locals: vec![vec![Node::Int(i128::from(u64::MAX) + 1)]],
        result: vec![
            Node::Variable(Scope::Input, 0),
            Node::Variable(Scope::Local, 0),
            Node::Call(Callee::Intrinsic(Op::Mul), 2),
        ],
        allow_fail: true,
    };
    let bytes = protocol::encode_request(73, &p, 128, 16384).unwrap();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        value,
        serde_json::json!([
            1,
            73,
            2,
            128,
            1,
            [[2, "18446744073709551615"]],
            [[[0, "18446744073709551616"]]],
            [[1, 0], [2, 0], [4, 0, 2, 2]]
        ])
    );
    assert!(protocol::encode_request(0, &p, 128, bytes.len() - 1).is_err());
    assert!(protocol::encode_request(SEQUENCE_CAP + 1, &p, 128, 16384).is_err());
}

#[test]
fn strict_response_decoder_rejects_wrong_sequence_shape_and_lexical_extensions() {
    assert_eq!(
        protocol::decode_answer(0, br#"[1,"value",0,"18446744073709551615",1]"#).unwrap(),
        OracleAnswer::Outcome {
            outcome: Outcome::Value(u64::MAX.into()),
            steps: 1
        }
    );
    assert_eq!(
        protocol::decode_answer(
            8,
            br#"[1,"diagnostic",8,"E0002","literal_parse_range",[2,0,0]]"#
        )
        .unwrap(),
        OracleAnswer::Diagnostic {
            class: DiagnosticClass::LiteralParseRange,
            blame: Blame {
                scope: 2,
                slot: 0,
                node: 0
            }
        }
    );
    for bad in [
        br#"[1,"value",1,"1",1]"#.as_slice(),
        br#"[1,"value",0,1,1]"#,
        br#"[1,"value",0,"-0",1]"#,
        br#"[1,"value",0,"1",1,9]"#,
        br#"[1,"value",0,"1",1e0]"#,
        br#"[1,"value",0,"\u0031",1]"#,
        br#"[1,"diagnostic",0,"E0201","literal_range",[2,0,0]]"#,
        br#"[1,"diagnostic",0,"E0201","unknown_variable",[2,1,0]]"#,
        br#"{"response":[1,"overflow",0,2]}"#,
    ] {
        assert!(protocol::decode_answer(0, bad).is_err(), "{:?}", bad);
    }
    assert_eq!(
        protocol::decode_answer(0, br#"[1,"fuel_exhausted",0,0]"#)
            .unwrap_err()
            .kind,
        FailureKind::Fuel
    );
    assert_eq!(
        protocol::decode_answer(0, br#"[1,"invariant_failure",0]"#)
            .unwrap_err()
            .kind,
        FailureKind::Invariant
    );
}

#[test]
fn ready_and_terminal_checks_require_exact_profile_and_totals() {
    protocol::ready(br#"[1,"ready",1,"4.24.0","vesper.arith.checked/1",63,8,3,3,4096]"#).unwrap();
    assert!(
        protocol::ready(br#"[1,"ready",1,"4.25.0","vesper.arith.checked/1",63,8,3,3,4096]"#)
            .is_err()
    );
    let mut counters = protocol::Counters::default();
    counters
        .record(&OracleAnswer::Outcome {
            outcome: Outcome::Overflow,
            steps: 3,
        })
        .unwrap();
    counters
        .record(&OracleAnswer::Diagnostic {
            class: DiagnosticClass::UnknownVariable,
            blame: Blame {
                scope: 2,
                slot: 0,
                node: 0,
            },
        })
        .unwrap();
    protocol::bye(br#"[1,"bye",2,1,1,0,0,1,0]"#, counters).unwrap();
    assert_eq!(
        protocol::bye(br#"[1,"bye",2,1,1,0,1,0,0]"#, counters)
            .unwrap_err()
            .kind,
        FailureKind::CounterMismatch
    );
}

#[test]
fn invalid_run_limits_fail_before_any_child_is_spawned() {
    let mut config = RunConfig {
        suite: Suite::Ci128,
        seed: 0,
        jobs: 1,
        lean: "nonexistent-lean-oracle42".into(),
        worker_exe: "nonexistent-worker-oracle42".into(),
        artifacts: None,
        limits: HarnessLimits::default(),
    };
    config.limits.worker_cases = 0;
    let failure = run(&config).unwrap_err();
    assert_eq!(failure.error.kind, FailureKind::InvalidConfig);
    assert_eq!(failure.partial.lean_spawns, 0);
    assert_eq!(failure.partial.rust_worker_spawns, 0);
    assert_eq!(failure.partial.committed_valid, 0);
    config.limits = HarnessLimits::default();
    config.jobs = 5;
    assert_eq!(
        run(&config).unwrap_err().error.kind,
        FailureKind::InvalidConfig
    );
}

#[test]
fn configured_source_cap_refuses_before_oracle_request_or_compiler_worker() {
    let limits = HarnessLimits {
        source_bytes: 1,
        ..HarnessLimits::default()
    };
    let config = RunConfig {
        suite: Suite::Ci128,
        seed: 0,
        jobs: 1,
        lean: std::env::var_os("VESPER_LEAN")
            .unwrap_or_else(|| "lean".into())
            .into(),
        worker_exe: env!("CARGO_BIN_EXE_arithmetic_oracle42").into(),
        artifacts: None,
        limits,
    };
    // This is a genuine Lean handshake/BYE, not a fake successful reference.
    let failure = run(&config).unwrap_err();
    assert_eq!(failure.error.kind, FailureKind::UnsupportedProfile);
    assert_eq!(failure.case_id, Some(0));
    assert_eq!(failure.partial.lean_spawns, 1);
    assert_eq!(failure.partial.lean_requests, 0);
    assert_eq!(failure.partial.rust_worker_spawns, 0);
    assert_eq!(failure.partial.parser_calls, 0);
}

#[test]
fn real_peer_deadline_stderr_and_truncation_reap_and_join() {
    let binary = process::resolve_executable(std::path::Path::new(env!(
        "CARGO_BIN_EXE_arithmetic_oracle42"
    )))
    .unwrap();
    for (mode, kind, expected) in [
        ("stall", ProbeKind::Stall, FailureKind::Timeout),
        (
            "stderr_flood",
            ProbeKind::StderrFlood,
            FailureKind::Protocol,
        ),
        (
            "truncated_payload",
            ProbeKind::TruncatedPayload,
            FailureKind::Protocol,
        ),
        ("crash", ProbeKind::Crash, FailureKind::Crash),
    ] {
        let mut command = Command::new(&binary);
        command.args(["--fault-peer", mode]);
        let report = probe(
            command,
            kind,
            ProbeLimits {
                frame_bytes: 512,
                stderr_bytes: 1024,
                timeout_ms: 300,
                shutdown_timeout_ms: 1000,
            },
        )
        .unwrap();
        assert_eq!(report.observed, Some(expected), "{mode}");
        assert!(
            report.child_reaped
                && report.reader_joined
                && report.writer_joined
                && report.stderr_joined
        );
        assert!(report.max_frame_bytes <= 512);
        assert!(report.stderr_bytes <= 1024);
    }
}

#[test]
fn real_peer_shortwrites_are_transport_only() {
    let binary = process::resolve_executable(std::path::Path::new(env!(
        "CARGO_BIN_EXE_arithmetic_oracle42"
    )))
    .unwrap();
    let mut command = Command::new(binary);
    command.args(["--fault-peer", "short_writes"]);
    let report = probe(
        command,
        ProbeKind::ShortWrites,
        ProbeLimits {
            frame_bytes: 512,
            stderr_bytes: 1024,
            timeout_ms: 1000,
            shutdown_timeout_ms: 1000,
        },
    )
    .unwrap();
    assert_eq!(report.observed, None);
    assert!(
        report.child_reaped && report.reader_joined && report.writer_joined && report.stderr_joined
    );
    assert_eq!(report.max_frame_bytes, b"oracle42-transport-marker".len());
}
