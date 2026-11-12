//! End-to-end tests that execute the committed fixture component
//! (`fixtures/guest`, rebuilt by `fixtures/build.sh`) inside the sandbox.

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use sha2::{Digest, Sha256};
use vesper_loom_sandbox::transport::{
    DefId, Delimiter, Direction, PackedTable, Span, TransportViolation,
};
use vesper_loom_sandbox::{
    ArtifactKind, Invocation, OutputFile, SandboxError, SandboxHost, Severity, StepComponent,
    StepConfig, StepKind, StepReport, Token, TokenKind, TransportLimits, classify,
};

const COMPONENT: &[u8] = include_bytes!("../fixtures/bin/step.component.wasm");
const PREVIEW1: &[u8] = include_bytes!("../fixtures/bin/step.preview1.wasm");
const SUMS: &str = include_str!("../fixtures/bin/SHA256SUMS");

fn host() -> &'static (SandboxHost, StepComponent) {
    static HOST: OnceLock<(SandboxHost, StepComponent)> = OnceLock::new();
    HOST.get_or_init(|| {
        let host = SandboxHost::new().unwrap();
        let step = host.compile(COMPONENT).unwrap();
        (host, step)
    })
}

/// Host layout:
///   root/secret.txt          outside every mount
///   root/proto/schema.txt    mounted read-only at /in/proto
///   root/proto/escape -> ../secret.txt   (symlink, unix only)
///   root/out/                dedicated output, mounted at /out
struct Fixture {
    root: tempfile::TempDir,
}

const SECRET: &str = "TOP-SECRET-HOST-DATA";

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let p = root.path();
        std::fs::write(p.join("secret.txt"), SECRET).unwrap();
        std::fs::create_dir(p.join("proto")).unwrap();
        std::fs::write(p.join("proto/schema.txt"), "id: u64\nname: Str\n").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("../secret.txt", p.join("proto/escape")).unwrap();
        std::fs::create_dir(p.join("out")).unwrap();
        Self { root }
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.root.path().join(rel)
    }

    fn config(&self) -> StepConfig {
        StepConfig::new(self.path("out")).input(self.path("proto"), "/in/proto")
    }

    fn run(&self, entry: &str, args: &[&str]) -> Result<StepReport, SandboxError> {
        self.run_with(&self.config(), entry, args)
    }

    fn run_with(
        &self,
        cfg: &StepConfig,
        entry: &str,
        args: &[&str],
    ) -> Result<StepReport, SandboxError> {
        let (host, step) = host();
        let inv = Invocation::new(StepKind::BuildScript, entry).with_args(args.iter().copied());
        host.run(step, cfg, &inv)
    }
}

fn ok_strings(r: &StepReport) -> Vec<String> {
    match &r.result {
        Ok(o) => o.tokens.iter().map(|t| t.text.clone()).collect(),
        Err(d) => panic!("guest failed: {d:?}"),
    }
}

fn guest_error(r: &StepReport) -> String {
    match &r.result {
        Ok(o) => panic!("expected guest error, got {o:?}"),
        Err(d) => {
            assert!(d.iter().all(|d| d.severity == Severity::Error));
            d.iter()
                .map(|d| d.message.clone())
                .collect::<Vec<_>>()
                .join("\n")
        }
    }
}

fn tree(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

// ---- artifact classification ----------------------------------------------

#[test]
fn fixtures_match_recorded_checksums() {
    for (name, bytes) in [
        ("step.component.wasm", COMPONENT),
        ("step.preview1.wasm", PREVIEW1),
    ] {
        let digest: String = Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let line = format!("{digest}  {name}");
        assert!(
            SUMS.lines().any(|l| l == line),
            "{name} does not match SHA256SUMS; rerun fixtures/build.sh"
        );
    }
}

#[test]
fn preview1_module_is_distinguished_and_rejected() {
    assert_eq!(classify(COMPONENT), ArtifactKind::Component);
    assert_eq!(classify(PREVIEW1), ArtifactKind::Preview1Module);
    let (host, _) = host();
    assert_eq!(
        host.compile(PREVIEW1).err(),
        Some(SandboxError::NotAComponent {
            detected: ArtifactKind::Preview1Module
        })
    );
}

#[test]
fn non_wasi_core_module_and_garbage_rejected() {
    let (host, _) = host();
    let core = b"\0asm\x01\0\0\0";
    assert_eq!(
        host.compile(core).err(),
        Some(SandboxError::NotAComponent {
            detected: ArtifactKind::CoreModule
        })
    );
    assert_eq!(
        host.compile(b"not wasm").err(),
        Some(SandboxError::NotAComponent {
            detected: ArtifactKind::NotWasm
        })
    );
}

#[test]
fn component_without_step_export_fails_to_link() {
    let (host, _) = host();
    let empty = wasmtime_component_bytes("(component)");
    let step = host.compile(&empty).unwrap();
    let fx = Fixture::new();
    let err = fx_run_component(&fx, &step, "echo").unwrap_err();
    match err {
        SandboxError::Link { message } => {
            assert!(message.contains("vesper:sandbox/step@0.1.0"), "{message}")
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn shared_memory_threads_are_not_enabled() {
    let (host, _) = host();
    let threaded = wasmtime_component_bytes("(component (core module (memory 1 1 shared)))");
    assert!(matches!(
        host.compile(&threaded),
        Err(SandboxError::Compile { .. })
    ));
}

fn wasmtime_component_bytes(wat: &str) -> Vec<u8> {
    wat::parse_str(wat).unwrap()
}

fn fx_run_component(
    fx: &Fixture,
    step: &StepComponent,
    entry: &str,
) -> Result<StepReport, SandboxError> {
    let (host, _) = host();
    host.run(
        step,
        &fx.config(),
        &Invocation::new(StepKind::ProcMacro, entry),
    )
}

// ---- allowed reads and outputs --------------------------------------------

#[test]
fn build_step_reads_input_and_writes_output() {
    let fx = Fixture::new();
    let r = fx
        .run("codegen", &["/in/proto/schema.txt", "/out/generated.vsp"])
        .unwrap();
    assert_eq!(ok_strings(&r), ["/out/generated.vsp"]);
    let generated = std::fs::read_to_string(fx.path("out/generated.vsp")).unwrap();
    assert_eq!(
        generated,
        "// generated by vesper-sandbox-fixture\nstruct Generated {\n    id: u64,\n    name: Str,\n}\n"
    );
    assert_eq!(
        r.outputs,
        [OutputFile {
            path: "generated.vsp".into(),
            bytes: generated.len() as u64
        }]
    );
    assert!(r.fuel_consumed > 0);
    assert!(r.peak_memory_bytes > 0);
}

#[test]
fn input_mount_lists_only_its_own_contents() {
    let fx = Fixture::new();
    let r = fx.run("list-dir", &["/in/proto"]).unwrap();
    let mut want = vec!["schema.txt".to_string()];
    if cfg!(unix) {
        want.insert(0, "escape".into());
    }
    assert_eq!(ok_strings(&r), want);
}

#[test]
fn output_scan_reports_relative_paths() {
    let fx = Fixture::new();
    let r = fx.run("write", &["/out/b.txt", "bb"]).unwrap();
    assert_eq!(r.outputs[0].path, "b.txt");
    assert_eq!(r.outputs[0].bytes, 2);
}

// ---- filesystem denials ----------------------------------------------------

#[test]
fn traversal_out_of_input_mount_is_denied() {
    let fx = Fixture::new();
    for path in [
        "/in/proto/../secret.txt",
        "/in/proto/../../secret.txt",
        "/in/proto/../../../../../../etc/passwd",
        "/out/../secret.txt",
        "/out/../proto/schema.txt",
    ] {
        let r = fx.run("read", &[path]).unwrap();
        let msg = guest_error(&r);
        assert!(!msg.contains(SECRET), "{path}: leaked");
        assert!(msg.contains("read"), "{path}: {msg}");
    }
}

#[cfg(unix)]
#[test]
fn symlink_escape_from_input_is_denied() {
    let fx = Fixture::new();
    // The host can follow the link; the guest must not.
    assert_eq!(
        std::fs::read_to_string(fx.path("proto/escape")).unwrap(),
        SECRET
    );
    let r = fx.run("read", &["/in/proto/escape"]).unwrap();
    let msg = guest_error(&r);
    assert!(!msg.contains(SECRET));
}

#[test]
fn unmounted_absolute_paths_are_denied() {
    let fx = Fixture::new();
    let secret = fx.path("secret.txt");
    for path in ["/etc/passwd", secret.to_str().unwrap(), "secret.txt", "/"] {
        let r = fx.run("read", &[path]).unwrap();
        let msg = guest_error(&r);
        assert!(!msg.contains(SECRET), "{path}");
    }
    let r = fx.run("list-dir", &["/"]).unwrap();
    guest_error(&r);
}

#[test]
fn writes_to_inputs_and_unmounted_paths_are_denied() {
    let fx = Fixture::new();
    let before = std::fs::read(fx.path("proto/schema.txt")).unwrap();
    for path in [
        "/in/proto/schema.txt",
        "/in/proto/new.txt",
        "/tmp/vesper-sandbox-escape",
        "/out/../escape.txt",
        "/out/../proto/new.txt",
    ] {
        let r = fx.run("write", &[path, "pwned"]).unwrap();
        let msg = guest_error(&r);
        assert!(msg.contains("write"), "{path}: {msg}");
        assert!(r.outputs.is_empty(), "{path}");
    }
    assert_eq!(std::fs::read(fx.path("proto/schema.txt")).unwrap(), before);
    assert_eq!(
        tree(&fx.path("proto")).len(),
        if cfg!(unix) { 2 } else { 1 }
    );
    assert_eq!(tree(fx.root.path()), ["out", "proto", "secret.txt"]);
    assert!(!Path::new("/tmp/vesper-sandbox-escape").exists());
}

#[test]
fn output_directory_must_start_empty() {
    let fx = Fixture::new();
    std::fs::write(fx.path("out/stale"), b"x").unwrap();
    assert!(matches!(
        fx.run("echo", &[]),
        Err(SandboxError::Config(
            vesper_loom_sandbox::ConfigError::OutputNotEmpty { .. }
        ))
    ));
}

#[test]
fn output_byte_limit_is_enforced_after_run() {
    let fx = Fixture::new();
    let mut cfg = fx.config();
    cfg.limits.max_output_bytes = 4;
    let err = fx
        .run_with(&cfg, "write", &["/out/big", "0123456789"])
        .unwrap_err();
    assert!(matches!(
        err,
        SandboxError::Output(vesper_loom_sandbox::OutputViolation::TooManyBytes {
            bytes: 10,
            limit: 4
        })
    ));
}

// ---- environment, network, clocks, randomness ------------------------------

#[test]
fn no_ambient_environment_arguments_or_extra_preopens() {
    // Set a host variable that a leaking host would forward.
    // SAFETY: tests in this file do not read the environment concurrently.
    unsafe { std::env::set_var("VESPER_SANDBOX_HOST_SECRET", SECRET) };
    let fx = Fixture::new();
    let r = fx.run("authority", &[]).unwrap();
    let got = ok_strings(&r);
    assert_eq!(
        got,
        [
            "cwd:None",
            "preopen:/in/proto",
            "preopen:/out",
            "std-env-count:0"
        ]
    );
}

#[test]
fn tcp_connect_to_listening_localhost_is_denied() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let fx = Fixture::new();
    let r = fx.run("tcp-connect", &[&addr]).unwrap();
    let msg = guest_error(&r);
    assert!(msg.contains("access-denied"), "{msg}");
    std::thread::sleep(Duration::from_millis(50));
    assert!(listener.accept().is_err(), "a connection reached the host");
}

#[test]
fn udp_bind_is_denied() {
    let fx = Fixture::new();
    let msg = guest_error(&fx.run("udp-bind", &[]).unwrap());
    assert!(msg.contains("access-denied"), "{msg}");
}

#[test]
fn dns_lookup_is_denied() {
    let fx = Fixture::new();
    let msg = guest_error(&fx.run("dns", &["localhost"]).unwrap());
    // wasmtime-wasi reports a disabled resolver as a permanent failure.
    assert!(msg.contains("permanent-resolver-failure"), "{msg}");
}

#[test]
fn clocks_and_randomness_are_deterministic() {
    let fx1 = Fixture::new();
    let fx2 = Fixture::new();
    let a = fx1.run("clock-random", &[]).unwrap();
    let b = fx2.run("clock-random", &[]).unwrap();
    let av = ok_strings(&a);
    assert_eq!(av, ok_strings(&b));
    assert_eq!(a.fuel_consumed, b.fuel_consumed);
    assert_eq!(av[0], "wall:0.000000000");
    assert_eq!(av[1], "std-wall-ns:0");
    assert_eq!(av[3], "mono:0");

    let mut cfg = fx1.config();
    cfg.determinism.random_seed = 7;
    cfg.determinism.wall_clock_ns = 1_700_000_000_123_456_789;
    let c = fx1.run_with(&cfg, "clock-random", &[]).unwrap();
    let cv = ok_strings(&c);
    assert_eq!(cv[0], "wall:1700000000.123456789");
    for i in [5, 6, 7, 8] {
        assert_ne!(av[i], cv[i], "seed change did not change {}", av[i]);
    }
}

#[test]
fn full_step_report_is_reproducible() {
    let run = || {
        let fx = Fixture::new();
        fx.run("codegen", &["/in/proto/schema.txt", "/out/g.vsp"])
            .unwrap()
    };
    assert_eq!(run(), run());
}

// ---- resource limits ------------------------------------------------------

#[test]
fn infinite_loop_exhausts_fuel() {
    let fx = Fixture::new();
    let mut cfg = fx.config();
    cfg.limits.fuel = 5_000_000;
    assert_eq!(
        fx.run_with(&cfg, "spin", &[]).unwrap_err(),
        SandboxError::FuelExhausted { limit: 5_000_000 }
    );
}

#[test]
fn small_fuel_budget_stops_even_a_trivial_step() {
    // Wasmtime checks fuel at function entries and loop headers, so a budget
    // only slightly below the measured cost may still complete; half of it
    // must not.
    let fx = Fixture::new();
    let full = fx.run("echo", &[]).unwrap();
    let mut cfg = fx.config();
    cfg.limits.fuel = full.fuel_consumed / 2;
    let half = fx.run_with(&cfg, "echo", &[]);
    assert!(
        matches!(half, Err(SandboxError::FuelExhausted { .. })),
        "full run consumed {} fuel; half budget gave {half:?}",
        full.fuel_consumed
    );
    cfg.limits.fuel = full.fuel_consumed;
    assert!(fx.run_with(&cfg, "echo", &[]).is_ok());
}

#[test]
fn unbounded_allocation_hits_memory_limit() {
    let fx = Fixture::new();
    let mut cfg = fx.config();
    cfg.limits.max_memory_bytes = 16 << 20;
    cfg.limits.fuel = u64::MAX / 2;
    match fx.run_with(&cfg, "alloc", &[]).unwrap_err() {
        SandboxError::MemoryLimitExceeded { limit, requested } => {
            assert_eq!(limit, 16 << 20);
            assert!(requested > limit);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn memory_limit_below_initial_size_fails_instantiation() {
    let fx = Fixture::new();
    let mut cfg = fx.config();
    cfg.limits.max_memory_bytes = 4096;
    assert!(matches!(
        fx.run_with(&cfg, "echo", &[]),
        Err(SandboxError::MemoryLimitExceeded { limit: 4096, .. })
    ));
}

#[test]
fn blocking_sleep_hits_wall_clock_backstop() {
    let fx = Fixture::new();
    let mut cfg = fx.config();
    cfg.limits.wall_clock_backstop = Duration::from_millis(300);
    assert_eq!(
        fx.run_with(&cfg, "sleep", &["3600"]).unwrap_err(),
        SandboxError::WallClockTimeout {
            limit: Duration::from_millis(300)
        }
    );
}

#[test]
fn guest_panic_is_a_trap() {
    let fx = Fixture::new();
    match fx.run("panic", &[]) {
        Err(SandboxError::Trap { message }) => {
            assert!(message.contains("unreachable"), "{message}")
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn stdout_is_captured_and_bounded() {
    let fx = Fixture::new();
    let mut cfg = fx.config();
    cfg.limits.max_stdio_bytes = 8 << 10;
    // 4 MiB into an 8 KiB pipe: std's `println!` fails once the pipe is
    // full and the guest panics, which surfaces as a trap rather than as
    // unbounded host memory.
    assert!(matches!(
        fx.run_with(&cfg, "stdout-flood", &[]),
        Err(SandboxError::Trap { .. })
    ));
    cfg.limits.max_stdio_bytes = 8 << 20;
    let r = fx.run_with(&cfg, "stdout-flood", &[]).unwrap();
    assert_eq!(r.stdout.len(), 4096 * 1024);
    assert!(r.stdout.starts_with(b"xxx"));
    assert!(r.stderr.is_empty());
}

// ---- token transport ----------------------------------------------------

fn token(kind: TokenKind, text: &str, scopes: Vec<u32>) -> Token {
    Token {
        kind,
        text: text.into(),
        span: Span {
            file: 3,
            start: 10,
            end: 10 + text.len() as u32,
        },
        scopes,
        pre_resolved: None,
    }
}

#[test]
fn tokens_round_trip_with_spans_scopes_and_def_ids() {
    let mut path = token(TokenKind::Ident, "hash_into", vec![]);
    path.pre_resolved = Some(DefId {
        package: 42,
        item: 7,
    });
    let tokens = vec![
        token(TokenKind::Open(Delimiter::Brace), "{", vec![1]),
        token(TokenKind::Ident, "h", vec![1, 5]),
        path,
        token(TokenKind::StringLiteral, "\"s\"", vec![]),
        token(TokenKind::Close(Delimiter::Brace), "}", vec![1]),
    ];
    let (host, step) = host();
    let fx = Fixture::new();
    let inv = Invocation::new(StepKind::DeriveMacro, "echo").with_tokens(tokens.clone());
    let r = host.run(step, &fx.config(), &inv).unwrap();
    let out = r.result.unwrap();
    assert_eq!(out.tokens, tokens);
    assert_eq!(out.diagnostics[0].message, "echo as derive-macro");
    assert_eq!(out.diagnostics[0].severity, Severity::Note);
}

#[test]
fn unbalanced_input_tokens_rejected_before_execution() {
    let (host, step) = host();
    let fx = Fixture::new();
    let inv = Invocation::new(StepKind::ProcMacro, "echo").with_tokens(vec![token(
        TokenKind::Open(Delimiter::Paren),
        "(",
        vec![],
    )]);
    assert!(matches!(
        host.run(step, &fx.config(), &inv),
        Err(SandboxError::Transport {
            direction: vesper_loom_sandbox::transport::Direction::Input,
            ..
        })
    ));
}

#[test]
fn oversized_guest_output_is_rejected() {
    let (host, step) = host();
    let fx = Fixture::new();
    let mut cfg = fx.config();
    cfg.limits.transport.max_tokens = 1;
    let r = host.run(
        step,
        &cfg,
        &Invocation::new(StepKind::BuildScript, "list-dir").with_args(["/in/proto"]),
    );
    if cfg!(unix) {
        assert!(matches!(
            r,
            Err(SandboxError::Transport {
                direction: vesper_loom_sandbox::transport::Direction::Output,
                ..
            })
        ));
    }
}

#[test]
fn payload_protocol_version_is_checked_by_guest() {
    let (host, step) = host();
    let fx = Fixture::new();
    let mut inv = Invocation::new(StepKind::ProcMacro, "echo");
    inv.protocol_version = 99;
    let r = host.run(step, &fx.config(), &inv).unwrap();
    assert!(guest_error(&r).contains("protocol 99 unsupported"));
}

// ---- hostile packed results -----------------------------------------------

fn output_violation(entry: &str, cfg: Option<StepConfig>) -> TransportViolation {
    let fx = Fixture::new();
    let cfg = cfg.unwrap_or_else(|| fx.config());
    match fx.run_with(&cfg, entry, &[]) {
        Err(SandboxError::Transport {
            direction: Direction::Output,
            violation,
        }) => violation,
        other => panic!("{entry}: {other:?}"),
    }
}

#[test]
fn aliased_text_slices_cannot_amplify_host_allocation() {
    // 100k tokens x 1 MiB each would be ~100 GiB if copied before checking.
    assert_eq!(
        output_violation("alias-bomb", None),
        TransportViolation::TooMuchText {
            bytes: 100_000 << 20,
            limit: TransportLimits::default().max_text_bytes
        }
    );
    assert!(matches!(
        output_violation("alias-bomb-diagnostics", None),
        TransportViolation::TooManyDiagnostics { count: 100_000, .. }
    ));
    let fx = Fixture::new();
    let mut cfg = fx.config();
    cfg.limits.transport.max_diagnostics = usize::MAX;
    assert!(matches!(
        output_violation("alias-bomb-diagnostics", Some(cfg)),
        TransportViolation::TooMuchText { .. }
    ));
}

#[test]
fn aliased_scope_slices_are_bounded() {
    assert_eq!(
        output_violation("scope-bomb", None),
        TransportViolation::TooManyScopes {
            token: 0,
            count: 1 << 16,
            limit: 256
        }
    );
    let fx = Fixture::new();
    let mut cfg = fx.config();
    cfg.limits.transport.max_scopes_per_token = 1 << 16;
    assert!(matches!(
        output_violation("scope-bomb", Some(cfg)),
        TransportViolation::TooManyScopesTotal { .. }
    ));
}

#[test]
fn malformed_packed_results_are_rejected() {
    assert_eq!(
        output_violation("bad-slice", None),
        TransportViolation::SliceOutOfBounds {
            table: PackedTable::TokenText,
            index: 0
        }
    );
    assert_eq!(
        output_violation("bad-utf8", None),
        TransportViolation::InvalidUtf8 {
            table: PackedTable::TokenText,
            index: 0
        }
    );
    assert_eq!(
        output_violation("error-with-tokens", None),
        TransportViolation::TokensInFailure { count: 1 }
    );
}

// ---- filesystem metadata determinism ---------------------------------------

fn metadata_run(fx: &Fixture, cfg: &StepConfig) -> Vec<String> {
    ok_strings(&fx.run_with(cfg, "fs-metadata", &[]).unwrap())
}

#[test]
fn filesystem_metadata_is_normalized() {
    use std::time::{Duration, SystemTime};
    // Two layouts with the same contents but different host timestamps and
    // creation order.
    let a = Fixture::new();
    let b = Fixture::new();
    for name in ["c.txt", "a.txt", "b.txt"] {
        std::fs::write(a.path("proto").join(name), name).unwrap();
    }
    for name in ["b.txt", "c.txt", "a.txt"] {
        std::fs::write(b.path("proto").join(name), name).unwrap();
    }
    let old = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
    std::fs::File::options()
        .write(true)
        .open(b.path("proto/schema.txt"))
        .unwrap()
        .set_modified(old)
        .unwrap();
    std::thread::sleep(Duration::from_millis(20));

    let ra = metadata_run(&a, &a.config());
    let rb = metadata_run(&b, &b.config());
    assert_eq!(ra, rb);
    let schema = &ra[0];
    assert!(
        schema.contains("mtime=0.000000000 atime=0.000000000"),
        "{schema}"
    );
    assert!(
        ra[1].starts_with("/out/probe.txt len=5 mtime=0.000000000"),
        "{}",
        ra[1]
    );
    // Identities are renumbered, stable within a run, and distinct per object.
    let hash = |key: &str| {
        ra.iter()
            .find_map(|l| l.strip_prefix(key))
            .unwrap()
            .to_string()
    };
    assert_eq!(hash("hash:schema="), "0:1");
    assert_eq!(hash("hash:schema-again="), "0:1");
    assert_eq!(hash("hash:probe="), "0:2");
    let mut order = "order=a.txt,b.txt,c.txt,schema.txt".to_string();
    if cfg!(unix) {
        order = "order=a.txt,b.txt,c.txt,escape,schema.txt".into();
    }
    assert_eq!(ra.last().unwrap(), &order);

    // The normalized timestamp follows the configured wall clock.
    let c = Fixture::new();
    let mut cfg = c.config();
    cfg.determinism.wall_clock_ns = 5_000_000_007;
    let rc = metadata_run(&c, &cfg);
    assert!(rc[0].contains("mtime=5.000000007"), "{}", rc[0]);
}
