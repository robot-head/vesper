//! Genuine finite-model observations; synthetic peers are negative transport controls only.
#[path = "../src/bin/resource_oracle42/mod.rs"]
pub mod oracle;

use oracle::process::{self, Config, Session};
use oracle::protocol::{
    self, CapacityError, CheckErrorKind, CheckProfile, Evaluation, Event, EventAction, InputError,
    InvocationError, LedgerEntry, Outcome, Request, ResidualOwner, SourceRow, Stage, Status,
    TypeError, TypeRow, ValidationError, ValueRow,
};
use std::{
    fs,
    io::{self, Cursor, Read, Write},
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

static GENUINE_SESSION: Mutex<()> = Mutex::new(());

const FIXTURES: &[(&str, &[u8])] = &[
    (
        "affine_branch",
        include_bytes!("fixtures/resource_oracle42/affine_branch.json"),
    ),
    (
        "branch_mints",
        include_bytes!("fixtures/resource_oracle42/branch_mints.json"),
    ),
    (
        "branch_ownership",
        include_bytes!("fixtures/resource_oracle42/branch_ownership.json"),
    ),
    (
        "copy_inputs",
        include_bytes!("fixtures/resource_oracle42/copy_inputs.json"),
    ),
    (
        "duplicate_use",
        include_bytes!("fixtures/resource_oracle42/duplicate_use.json"),
    ),
    (
        "dynamic_growth",
        include_bytes!("fixtures/resource_oracle42/dynamic_growth.json"),
    ),
    (
        "field_modality",
        include_bytes!("fixtures/resource_oracle42/field_modality.json"),
    ),
    (
        "literal_out_of_range",
        include_bytes!("fixtures/resource_oracle42/literal_out_of_range.json"),
    ),
    (
        "mint_consume",
        include_bytes!("fixtures/resource_oracle42/mint_consume.json"),
    ),
    (
        "natural",
        include_bytes!("fixtures/resource_oracle42/natural.json"),
    ),
    (
        "return_ticket",
        include_bytes!("fixtures/resource_oracle42/return_ticket.json"),
    ),
    (
        "sequential_resources",
        include_bytes!("fixtures/resource_oracle42/sequential_resources.json"),
    ),
    (
        "type_mismatch",
        include_bytes!("fixtures/resource_oracle42/type_mismatch.json"),
    ),
    (
        "unit",
        include_bytes!("fixtures/resource_oracle42/unit.json"),
    ),
    (
        "unknown_variable",
        include_bytes!("fixtures/resource_oracle42/unknown_variable.json"),
    ),
    (
        "unpack_owners",
        include_bytes!("fixtures/resource_oracle42/unpack_owners.json"),
    ),
    (
        "unpack_values",
        include_bytes!("fixtures/resource_oracle42/unpack_values.json"),
    ),
    (
        "unused_linear",
        include_bytes!("fixtures/resource_oracle42/unused_linear.json"),
    ),
];

fn request(name: &str) -> Request {
    let bytes = FIXTURES.iter().find(|(n, _)| *n == name).unwrap().1;
    let (sequence, request) = protocol::decode_request(bytes).expect("fixed typed request fixture");
    assert_eq!(sequence, 0);
    request
}

fn config(repo_root: PathBuf) -> Config {
    Config {
        lean: std::env::var_os("VESPER_LEAN").map_or_else(|| PathBuf::from("lean"), PathBuf::from),
        repo_root,
        artifact_dir: None,
        limits: Default::default(),
    }
}

struct Matrix {
    session: Session,
    counts: [u64; 7],
}
impl Matrix {
    fn new() -> Self {
        let config = config(PathBuf::from(env!("CARGO_MANIFEST_DIR")));
        Self {
            session: process::start(&config).expect("required genuine pinned Lean worker"),
            counts: [0; 7],
        }
    }
    fn evaluate(&mut self, request: &Request) -> Outcome {
        let response = self
            .session
            .evaluate(request)
            .expect("genuine worker response");
        assert_eq!(response.sequence, self.counts[0]);
        self.counts[0] += 1;
        let index = match response.stage() {
            Stage::ValidationError => 1,
            Stage::CheckSemantic => 2,
            Stage::CheckProfile => 3,
            Stage::InvocationError => 4,
            Stage::EvalAdmissionError => 5,
            Stage::Evaluation => 6,
        };
        self.counts[index] += 1;
        response.outcome
    }
    fn run(&mut self, name: &str) -> Outcome {
        self.evaluate(&request(name))
    }
    fn evaluation(&mut self, request: &Request) -> Evaluation {
        match self.evaluate(request) {
            Outcome::Evaluation(value) => value,
            other => panic!("expected actual evaluation, received {other:?}"),
        }
    }
    fn finish(self) {
        assert_eq!(self.counts[0], self.counts[1..].iter().sum::<u64>());
        assert!(self.counts[1..].iter().all(|count| *count > 0));
        let evidence = self
            .session
            .finish()
            .expect("exact BYE, EOF, successful exit and identities");
        assert!(evidence.completed);
        assert_eq!(evidence.requests, self.counts[0]);
        assert_eq!(evidence.counters, self.counts);
        assert!(evidence.framed_bytes <= protocol::MAX_SESSION_BYTES);
        assert_eq!(evidence.transcript_blake3.len(), 64);
        assert!(!evidence.artifacts.is_empty());
        eprintln!(
            "RESOURCE_ORACLE42_SESSION_EVIDENCE {}",
            serde_json::to_string(&evidence).unwrap()
        );
        eprintln!("RESOURCE_ORACLE42_STAGE_COUNTS {:?}", self.counts);
    }
}

fn completed(value: Vec<ValueRow>) -> Status {
    Status::Completed(value)
}
fn event(action: EventAction, site: u64, id: u64) -> Event {
    Event { action, site, id }
}
fn ledger(mint_site: u64, live: bool) -> LedgerEntry {
    LedgerEntry { mint_site, live }
}
fn owner(id: u64, mint_site: u64) -> ResidualOwner {
    ResidualOwner { id, mint_site }
}
fn assert_execution(
    actual: &Evaluation,
    status: Status,
    transitions: u64,
    events: Vec<Event>,
    residual: Vec<ResidualOwner>,
    ledger: Vec<LedgerEntry>,
) {
    assert_eq!(actual.status, status);
    assert_eq!(actual.transition_count, transitions);
    assert_eq!(actual.events, events);
    assert_eq!(actual.residual, residual);
    assert_eq!(actual.ledger, ledger);
}

#[test]
fn genuine_worker_static_stages_values_ownership_and_limited_prefixes() {
    let _lease = GENUINE_SESSION
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut matrix = Matrix::new();
    let unit = matrix.evaluation(&request("unit"));
    assert_execution(
        &unit,
        completed(vec![ValueRow::Unit]),
        0,
        vec![],
        vec![],
        vec![],
    );
    assert_eq!(unit.checked.result_type, vec![TypeRow::Unit]);
    assert_eq!(unit.checked.source_stats.nodes, 1);
    assert_eq!(unit.checked.source_stats.depth, 1);
    let natural = matrix.evaluation(&request("natural"));
    assert_eq!(
        natural.status,
        completed(vec![ValueRow::Nat(protocol::Natural::from_u64(u64::MAX))])
    );
    assert_eq!(natural.transition_count, 0);

    let mut copy = request("copy_inputs");
    let copied = matrix.evaluation(&copy);
    assert_execution(
        &copied,
        completed(vec![ValueRow::Nat(protocol::Natural::from_u64(7))]),
        2,
        vec![],
        vec![],
        vec![],
    );
    assert_eq!(copied.checked.uses, vec![true, false]);
    copy.fuel = 1;
    let closing_prefix = matrix.evaluation(&copy);
    assert_eq!(closing_prefix.status, Status::FuelExhausted);
    assert_eq!(closing_prefix.transition_count, 1);
    copy.fuel = 2;
    copy.input_values[0] = vec![ValueRow::Nat(protocol::Natural::from_u64(11))];
    let substituted = matrix.evaluation(&copy);
    assert_eq!(
        substituted.status,
        completed(vec![ValueRow::Nat(protocol::Natural::from_u64(11))])
    );
    assert_eq!(copied.checked, substituted.checked);

    let returned = matrix.evaluation(&request("return_ticket"));
    assert_execution(
        &returned,
        completed(vec![ValueRow::Handle(0)]),
        1,
        vec![event(EventAction::Mint, 0, 0)],
        vec![owner(0, 0)],
        vec![ledger(0, true)],
    );
    let mint_consume = request("mint_consume");
    let consumed = matrix.evaluation(&mint_consume);
    assert_execution(
        &consumed,
        completed(vec![ValueRow::Unit]),
        3,
        vec![
            event(EventAction::Mint, 1, 0),
            event(EventAction::Consume, 2, 0),
        ],
        vec![],
        vec![ledger(1, false)],
    );
    let mut short = mint_consume.clone();
    short.fuel = 2;
    assert_execution(
        &matrix.evaluation(&short),
        Status::FuelExhausted,
        2,
        vec![event(EventAction::Mint, 1, 0)],
        vec![owner(0, 1)],
        vec![ledger(1, true)],
    );
    short.fuel = 0;
    assert_execution(
        &matrix.evaluation(&short),
        Status::FuelExhausted,
        0,
        vec![],
        vec![],
        vec![],
    );
    let mut terminal = request("unit");
    terminal.limits[11] = 0;
    terminal.limits[12] = 0;
    terminal.limits[13] = 0;
    assert_execution(
        &matrix.evaluation(&terminal),
        completed(vec![ValueRow::Unit]),
        0,
        vec![],
        vec![],
        vec![],
    );

    let sequential = matrix.evaluation(&request("sequential_resources"));
    assert_execution(
        &sequential,
        completed(vec![ValueRow::Unit]),
        7,
        vec![
            event(EventAction::Mint, 2, 0),
            event(EventAction::Consume, 3, 0),
            event(EventAction::Mint, 6, 1),
            event(EventAction::Consume, 7, 1),
        ],
        vec![],
        vec![ledger(2, false), ledger(6, false)],
    );
    let mut branch = request("branch_mints");
    let zero = matrix.evaluation(&branch);
    assert_execution(
        &zero,
        completed(vec![ValueRow::Handle(0)]),
        3,
        vec![event(EventAction::Mint, 2, 0)],
        vec![owner(0, 2)],
        vec![ledger(2, true)],
    );
    branch.input_values[0] = vec![ValueRow::Nat(protocol::Natural::from_u64(7))];
    let nonzero = matrix.evaluation(&branch);
    assert_execution(
        &nonzero,
        completed(vec![ValueRow::Handle(0)]),
        3,
        vec![event(EventAction::Mint, 3, 0)],
        vec![owner(0, 3)],
        vec![ledger(3, true)],
    );
    assert_eq!(zero.checked, nonzero.checked);
    let unpack = matrix.evaluation(&request("unpack_values"));
    assert_eq!(
        unpack.status,
        completed(vec![
            ValueRow::Nat(protocol::Natural::from_u64(7)),
            ValueRow::Nat(protocol::Natural::from_u64(9)),
            ValueRow::Pair
        ])
    );
    assert_eq!(unpack.transition_count, 2);
    let owners = matrix.evaluation(&request("unpack_owners"));
    assert_execution(
        &owners,
        completed(vec![ValueRow::Unit]),
        9,
        vec![
            event(EventAction::Mint, 1, 0),
            event(EventAction::Mint, 3, 1),
            event(EventAction::Consume, 11, 1),
            event(EventAction::Consume, 13, 0),
        ],
        vec![],
        vec![ledger(1, false), ledger(3, false)],
    );

    for (name, kind, site, slot) in [
        (
            "unknown_variable",
            CheckErrorKind::UnknownVariable,
            0,
            Some(0),
        ),
        (
            "literal_out_of_range",
            CheckErrorKind::LiteralOutOfRange,
            0,
            None,
        ),
        ("unused_linear", CheckErrorKind::UnusedLinear, 0, Some(0)),
        ("duplicate_use", CheckErrorKind::DuplicateUse, 2, Some(0)),
        ("type_mismatch", CheckErrorKind::TypeMismatch, 0, None),
        ("field_modality", CheckErrorKind::FieldModality, 2, None),
        (
            "branch_ownership",
            CheckErrorKind::BranchOwnership,
            2,
            Some(0),
        ),
    ] {
        match matrix.run(name) {
            Outcome::CheckSemantic {
                error,
                source_stats,
            } => {
                assert_eq!((error.kind, error.site, error.slot), (kind, site, slot));
                assert!(source_stats.nodes > 0);
            }
            other => panic!("semantic refusal cannot contain execution: {name}: {other:?}"),
        }
    }
    assert!(matches!(
        matrix.run("affine_branch"),
        Outcome::CheckProfile {
            reason: CheckProfile::AffineBranchDemand(4, 0),
            ..
        }
    ));
    let mut bounded_type = request("unit");
    bounded_type.limits[3] = 0;
    assert!(matches!(
        matrix.evaluate(&bounded_type),
        Outcome::CheckProfile {
            reason: CheckProfile::TypeBound(0, TypeError::Nodes),
            ..
        }
    ));
    let mut invalid_limits = request("unit");
    invalid_limits.limits[0] = 1025;
    assert!(matches!(
        matrix.evaluate(&invalid_limits),
        Outcome::ValidationError(ValidationError::InvalidLimits)
    ));
    let mut raw_nodes = request("unit");
    raw_nodes.limits[0] = 0;
    assert!(matches!(
        matrix.evaluate(&raw_nodes),
        Outcome::ValidationError(ValidationError::RawNodes)
    ));
    let mut duplicated_site = mint_consume.clone();
    duplicated_site.source[0] = SourceRow::Mint { site: 0 };
    assert!(matches!(
        matrix.evaluate(&duplicated_site),
        Outcome::ValidationError(ValidationError::DuplicateSite(0))
    ));
    let mut noncopy = request("branch_mints");
    noncopy.input_types[0] = vec![TypeRow::Ticket];
    assert!(matches!(
        matrix.evaluate(&noncopy),
        Outcome::ValidationError(ValidationError::NoncopyInput(0))
    ));

    for (values, error) in [
        (vec![], InvocationError::InputCount(1, 0)),
        (
            vec![vec![ValueRow::Unit]],
            InvocationError::InputValue(0, InputError::Shape),
        ),
        (
            vec![vec![ValueRow::Handle(0)]],
            InvocationError::InputValue(0, InputError::Handle),
        ),
        (
            vec![vec![ValueRow::Nat(
                protocol::Natural::parse("18446744073709551616").unwrap(),
            )]],
            InvocationError::InputValue(0, InputError::NaturalRange),
        ),
    ] {
        let mut wrong = request("branch_mints");
        wrong.input_values = values;
        match matrix.evaluate(&wrong) {
            Outcome::InvocationError {
                checked,
                error: actual,
            } => {
                assert_eq!(actual, error);
                assert_eq!(checked, zero.checked);
            }
            other => panic!("invocation refusal cannot contain execution: {other:?}"),
        }
    }
    let mut initial_capacity = request("branch_mints");
    initial_capacity.limits[7] = 0;
    assert!(matches!(
        matrix.evaluate(&initial_capacity),
        Outcome::InvocationError {
            error: InvocationError::Capacity(CapacityError::InitialValues),
            ..
        }
    ));
    let mut admission = request("unit");
    admission.fuel = 4;
    admission.limits[13] = 3;
    assert!(matches!(
        matrix.evaluate(&admission),
        Outcome::EvalAdmissionError {
            error: InvocationError::FuelBound(4, 3),
            ..
        }
    ));

    let mut resource_prefix = request("sequential_resources");
    resource_prefix.limits[11] = 1;
    assert_execution(
        &matrix.evaluation(&resource_prefix),
        Status::ResourceLimit(protocol::ResourceRefusal::Resources(2, 1)),
        4,
        vec![
            event(EventAction::Mint, 2, 0),
            event(EventAction::Consume, 3, 0),
        ],
        vec![],
        vec![ledger(2, false)],
    );
    let mut event_prefix = mint_consume;
    event_prefix.limits[12] = 1;
    assert_execution(
        &matrix.evaluation(&event_prefix),
        Status::ResourceLimit(protocol::ResourceRefusal::Events(2, 1)),
        2,
        vec![event(EventAction::Mint, 1, 0)],
        vec![owner(0, 1)],
        vec![ledger(1, true)],
    );
    let mut growth = request("dynamic_growth");
    growth.limits[5] = 4;
    assert_execution(
        &matrix.evaluation(&growth),
        Status::ResourceLimit(protocol::ResourceRefusal::Capacity(
            CapacityError::ValueNodes,
        )),
        3,
        vec![event(EventAction::Mint, 1, 0)],
        vec![owner(0, 1)],
        vec![ledger(1, true)],
    );
    growth.limits[5] = 7;
    assert_execution(
        &matrix.evaluation(&growth),
        completed(vec![ValueRow::Unit]),
        6,
        vec![
            event(EventAction::Mint, 1, 0),
            event(EventAction::Consume, 12, 0),
        ],
        vec![],
        vec![ledger(1, false)],
    );
    matrix.finish();
}

struct ShortReader {
    bytes: Cursor<Vec<u8>>,
    stride: usize,
}
impl Read for ShortReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let n = out.len().min(self.stride);
        self.bytes.read(&mut out[..n])
    }
}
struct ShortWriter {
    bytes: Vec<u8>,
    stride: usize,
}
impl Write for ShortWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let n = bytes.len().min(self.stride);
        self.bytes.extend_from_slice(&bytes[..n]);
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn production_codec_fixed_requests_short_io_and_hostile_lexical_envelope() {
    for (name, bytes) in FIXTURES {
        let (sequence, request) = protocol::decode_request(bytes).unwrap();
        let encoded = protocol::encode_request(sequence, &request).unwrap();
        assert_eq!(
            protocol::decode_request(&encoded).unwrap(),
            (sequence, request),
            "{name}"
        );
    }
    let payload = protocol::encode_request(0, &request("unit")).unwrap();
    for stride in 1..=5 {
        let mut writer = ShortWriter {
            bytes: Vec::new(),
            stride,
        };
        protocol::write_frame(&mut writer, &payload, protocol::REQUEST_CAP).unwrap();
        assert_eq!(
            &writer.bytes[..4],
            &u32::try_from(payload.len()).unwrap().to_le_bytes()
        );
        for read_stride in 1..=5 {
            let mut reader = ShortReader {
                bytes: Cursor::new(writer.bytes.clone()),
                stride: read_stride,
            };
            assert_eq!(
                protocol::read_frame(&mut reader, protocol::REQUEST_CAP).unwrap(),
                Some(payload.clone())
            );
            assert_eq!(
                protocol::read_frame(&mut reader, protocol::REQUEST_CAP).unwrap(),
                None
            );
        }
        for cut in 1..writer.bytes.len() {
            let mut truncated = Cursor::new(writer.bytes[..cut].to_vec());
            assert!(protocol::read_frame(&mut truncated, protocol::REQUEST_CAP).is_err());
        }
    }
    for size in [
        0,
        u32::try_from(protocol::REQUEST_CAP + 1).unwrap(),
        u32::MAX,
    ] {
        let mut bytes = Cursor::new(size.to_le_bytes().to_vec());
        assert!(protocol::read_frame(&mut bytes, protocol::REQUEST_CAP).is_err());
        assert_eq!(bytes.position(), 4);
    }
    for invalid in [
        b"{}".as_slice(),
        b"true",
        b"null",
        b"[-1]",
        b"[1.0]",
        b"[1e2]",
        b"[01]",
        br#"["a\nb"]"#,
        br#"["\u0061"]"#,
        b"[\"\xc3\xa9\"]",
        b"[10000000000]",
        b"[[[[[[[[[0]]]]]]]]]",
        b"[1,\"eval\",0]",
        b"[1,\"unknown\",0]",
    ] {
        assert!(protocol::decode_request(invalid).is_err(), "{invalid:?}");
    }
    for decimal in [
        "",
        "00",
        "01",
        "+1",
        "-1",
        "1.0",
        "1e2",
        "123456789012345678901",
        "１２",
    ] {
        assert!(protocol::Natural::parse(decimal).is_err());
    }
    for decimal in ["0", "18446744073709551616", "99999999999999999999"] {
        assert_eq!(protocol::Natural::parse(decimal).unwrap().as_str(), decimal);
    }
    let mut invalid_tree = request("unit");
    invalid_tree.source = vec![SourceRow::Let { site: 0 }];
    assert!(protocol::encode_request(0, &invalid_tree).is_err());
    invalid_tree.source = vec![SourceRow::Unit { site: 0 }, SourceRow::Unit { site: 1 }];
    assert!(protocol::encode_request(0, &invalid_tree).is_err());
    invalid_tree.source = vec![
        SourceRow::Mint { site: 0 },
        SourceRow::Unit { site: 1 },
        SourceRow::Pair {
            site: 2,
            floor: protocol::Floor::Copy,
            left_mode: protocol::Mode::Linear,
            right_mode: protocol::Mode::Copy,
        },
    ];
    assert!(
        protocol::encode_request(0, &invalid_tree).is_err(),
        "non-atom pair operand"
    );
    invalid_tree.source = vec![SourceRow::Unit { site: 0 }; 2049];
    assert!(protocol::encode_request(0, &invalid_tree).is_err());
    let mut too_many = request("unit");
    too_many.input_values = vec![vec![ValueRow::Unit]; 66];
    assert!(protocol::encode_request(0, &too_many).is_err());
    let unit_payload = String::from_utf8(payload.clone()).unwrap();
    for source in ["[[9,0]]", "[[2,0,-1]]", "[[1,0,\"01\"]]", "[[0,0,1]]"] {
        let hostile = unit_payload.replace("[[0,0]]", source);
        assert_ne!(hostile, unit_payload);
        assert!(protocol::decode_request(hostile.as_bytes()).is_err());
    }
    let mut too_deep = request("unit");
    too_deep.input_values = vec![vec![ValueRow::Unit]];
    for _ in 0..256 {
        too_deep.input_values[0].push(ValueRow::Unit);
        too_deep.input_values[0].push(ValueRow::Pair);
    }
    assert!(
        protocol::encode_request(0, &too_deep).is_err(),
        "bounded constructed depth independent of row cap"
    );
    for payload in [
        br#"[1,"result",0,"validation_error",[["invalidLimits"],[]]]"#.as_slice(),
        br#"[1,"result",0,"validation_error",[["unknown"]]]"#,
        br#"[1,"result",0,"check_semantic",[["unknown",0,[]],[1,1,0,0,0]]]"#,
        br#"[1,"result",0,"evaluation",[]]"#,
        br#"[1,"bye",0,[0,0,0,0,0,0,0],[]]"#,
    ] {
        assert!(
            protocol::decode_message(payload).is_err(),
            "no unknown/extra diagnostic or execution payload"
        );
    }
}

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "vesper-resource-endpoint-acceptance-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        Self(root)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
const SOURCES: &[&str] = &[
    "lean/MetatheoryFull.lean",
    "lean/ResourceModel42/Syntax.lean",
    "lean/ResourceModel42/Checker.lean",
    "lean/ResourceModel42/Evaluator.lean",
    "lean/ResourceOracle42.lean",
];
fn fixed_source_copy(destination: &Path) {
    for relative in SOURCES {
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join(relative);
        assert!(fs::symlink_metadata(&source).unwrap().file_type().is_file());
        let target = destination.join(relative);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::copy(source, target).unwrap();
    }
}

#[test]
fn changed_missing_nonregular_and_oversize_fixed_sources_never_start_a_model_session() {
    use process::HarnessErrorKind;
    for mutation in ["changed", "missing", "directory", "oversize"] {
        let root = Directory::new();
        fixed_source_copy(&root.0);
        let source = root.0.join("lean/ResourceOracle42.lean");
        match mutation {
            "changed" => {
                let mut bytes = fs::read(&source).unwrap();
                bytes.push(b'\n');
                fs::write(&source, bytes).unwrap();
            }
            "missing" => fs::remove_file(&source).unwrap(),
            "directory" => {
                fs::remove_file(&source).unwrap();
                fs::create_dir(&source).unwrap();
            }
            "oversize" => fs::write(&source, vec![b' '; 4 * 1024 * 1024 + 1]).unwrap(),
            _ => unreachable!(),
        }
        let error = match process::start(&config(root.0.clone())) {
            Ok(_) => panic!("mutated fixed source cannot supply a session: {mutation}"),
            Err(error) => error,
        };
        assert_eq!(
            error.kind,
            HarnessErrorKind::SourceIdentity,
            "{mutation}: {error:?}"
        );
    }
}

#[cfg(unix)]
#[test]
fn symlinked_fixed_source_is_refused_even_with_identical_target_bytes() {
    let root = Directory::new();
    fixed_source_copy(&root.0);
    let source = root.0.join("lean/ResourceOracle42.lean");
    let target = root.0.join("same-worker.lean");
    fs::rename(&source, &target).unwrap();
    std::os::unix::fs::symlink(target, source).unwrap();
    let error = match process::start(&config(root.0.clone())) {
        Ok(_) => panic!("symlinked source cannot supply a session"),
        Err(error) => error,
    };
    assert_eq!(error.kind, process::HarnessErrorKind::SourceIdentity);
}

#[test]
fn changed_executable_object_is_refused_before_any_model_response_or_evidence() {
    let _lease = GENUINE_SESSION
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut session = process::start(&config(PathBuf::from(env!("CARGO_MANIFEST_DIR")))).unwrap();
    let object = session.artifact_root().join("ResourceOracle42.olean");
    let mut bytes = fs::read(&object).unwrap();
    bytes.push(0);
    // Production seals compiled objects read-only. Deliberately make only
    // this owned fixture writable before corrupting its actual bytes.
    let mut permissions = fs::metadata(&object).unwrap().permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        permissions.set_mode(permissions.mode() | 0o200);
    }
    #[cfg(windows)]
    #[expect(
        clippy::permissions_set_readonly_false,
        reason = "On Windows this clears only the read-only attribute; ACL permissions are unchanged."
    )]
    permissions.set_readonly(false);
    fs::set_permissions(&object, permissions).unwrap();
    fs::write(object, bytes).unwrap();
    let error = session.evaluate(&request("unit")).unwrap_err();
    assert_eq!(error.kind, process::HarnessErrorKind::ArtifactIdentity);
    assert!(
        session.finish().is_err(),
        "failed identity never retires as evidence"
    );
}

fn negative_peer() -> (std::ffi::OsString, PathBuf) {
    let interpreter = std::env::var_os("PYTHON").unwrap_or_else(|| {
        if cfg!(windows) {
            "python".into()
        } else {
            "python3".into()
        }
    });
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/resource_oracle42/fault_peer.py");
    assert!(script.is_file(), "fixed negative transport fixture exists");
    (interpreter, script)
}

#[test]
fn hostile_peers_use_production_process_and_cannot_return_mathematical_evidence() {
    use process::{HarnessErrorKind, HarnessLimits};
    use std::{
        process::Command,
        time::{Duration, Instant},
    };
    let (interpreter, peer) = negative_peer();
    for (kind, expected, requests) in [
        ("wrong_ready", HarnessErrorKind::Protocol, vec![]),
        ("stdout_noise", HarnessErrorKind::Protocol, vec![]),
        (
            "malformed",
            HarnessErrorKind::Protocol,
            vec![request("unit")],
        ),
        (
            "unknown_constructor",
            HarnessErrorKind::Protocol,
            vec![request("unit")],
        ),
        (
            "reordered",
            HarnessErrorKind::Sequence,
            vec![request("unit")],
        ),
        (
            "oversize",
            HarnessErrorKind::Protocol,
            vec![request("unit")],
        ),
        (
            "zero_length",
            HarnessErrorKind::Protocol,
            vec![request("unit")],
        ),
        (
            "truncated_header",
            HarnessErrorKind::Protocol,
            vec![request("unit")],
        ),
        (
            "truncated_payload",
            HarnessErrorKind::Protocol,
            vec![request("unit")],
        ),
        ("stall", HarnessErrorKind::Timeout, vec![request("unit")]),
        ("flood", HarnessErrorKind::Quota, vec![request("unit")]),
        ("crash", HarnessErrorKind::Crash, vec![request("unit")]),
        ("bad_bye", HarnessErrorKind::CounterMismatch, vec![]),
        ("trailing", HarnessErrorKind::Protocol, vec![]),
        ("exit_failure", HarnessErrorKind::Crash, vec![]),
        ("no_bye", HarnessErrorKind::Protocol, vec![]),
    ] {
        let mut command = Command::new(&interpreter);
        command.arg(&peer).arg(kind);
        let limits = HarnessLimits {
            // Allow interpreter startup for semantic faults; retain the short
            // request clock for the intentional stall probe.
            request_timeout_ms: if kind == "stall" { 300 } else { 2_000 },
            shutdown_timeout_ms: 300,
            stderr_bytes: 1024,
            ..HarnessLimits::default()
        };
        let started = Instant::now();
        let error = process::test_transport_failure(command, &requests, limits);
        assert_eq!(error.kind, expected, "{kind}: {error:?}");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "bounded cleanup for {kind}"
        );
    }
    #[cfg(unix)]
    {
        let script = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/resource_oracle42/bye_eof_stall.sh");
        let mut command = Command::new("sh");
        command.arg(script).arg(&interpreter).arg(&peer);
        let started = Instant::now();
        let cleanup_failures = process::cleanup_failures();
        let error = process::test_transport_failure(
            command,
            &[],
            HarnessLimits {
                request_timeout_ms: 300,
                shutdown_timeout_ms: 300,
                ..HarnessLimits::default()
            },
        );
        assert_eq!(
            error.kind,
            HarnessErrorKind::Timeout,
            "BYE and stdout EOF do not prove process retirement"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "shutdown retains finite kill/reap/join allowance"
        );
        assert_eq!(process::cleanup_failures(), cleanup_failures);
    }
    let mut blocked = request("unit");
    // Sixty-four independent balanced trees use 131,008 rows, below the
    // 131,072-row aggregate cap, and exceed ordinary OS pipe capacity.
    fn value_tree(depth: usize, rows: &mut Vec<ValueRow>) {
        if depth == 0 {
            rows.push(ValueRow::Unit);
        } else {
            value_tree(depth - 1, rows);
            value_tree(depth - 1, rows);
            rows.push(ValueRow::Pair);
        }
    }
    let mut tree = Vec::new();
    value_tree(10, &mut tree);
    assert_eq!(tree.len(), 2047);
    blocked.input_values = vec![tree; 64];
    assert!(protocol::encode_request(0, &blocked).unwrap().len() > 262_144);
    let mut command = Command::new(interpreter);
    command.arg(peer).arg("blocked_stdin");
    let error = process::test_transport_failure(
        command,
        &[blocked],
        HarnessLimits {
            request_timeout_ms: 300,
            shutdown_timeout_ms: 300,
            ..HarnessLimits::default()
        },
    );
    assert_eq!(error.kind, HarnessErrorKind::Timeout);
}

#[test]
fn retirement_reserve_is_prepaid_and_unconfirmed_retirement_has_no_evidence() {
    let _lease = GENUINE_SESSION
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let request = request("unit");
    let ready = serde_json::to_vec(&serde_json::json!([
        1,
        "ready",
        protocol::PROTOCOL_ID,
        protocol::LEAN_VERSION,
        protocol::LEAN_COMMIT,
        protocol::BOUNDS
    ]))
    .unwrap();
    let request_bytes = protocol::encode_request(0, &request).unwrap();
    let threshold = 4 + ready.len() + 4 + request_bytes.len() + 4 + protocol::RESPONSE_CAP + 512;
    let mut configuration = config(PathBuf::from(env!("CARGO_MANIFEST_DIR")));
    configuration.limits.session_wire_bytes = u64::try_from(threshold - 1).unwrap();
    let mut session =
        process::start(&configuration).expect("genuine READY below bounded wire quota");
    let error = session.evaluate(&request).unwrap_err();
    assert_eq!(error.kind, process::HarnessErrorKind::Quota);
    assert!(
        session.finish().is_err(),
        "a refused epoch cannot supply a successful mathematical report"
    );
}

#[test]
fn dropping_genuine_session_records_abandonment_and_never_completed_evidence() {
    let _lease = GENUINE_SESSION
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let before = process::abandoned_sessions();
    let session = process::start(&config(PathBuf::from(env!("CARGO_MANIFEST_DIR")))).unwrap();
    let private_root = session.artifact_root().to_path_buf();
    assert!(private_root.is_dir());
    drop(session);
    assert_eq!(process::abandoned_sessions(), before + 1);
    assert!(
        !private_root.exists(),
        "abandoned private epoch removed after bounded cleanup"
    );
}
