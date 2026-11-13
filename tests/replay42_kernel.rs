//! Real Lean kernel replay of independently constructed and producer witnesses.
//! MathModelEq is deliberately narrower than a proof of the Rust translation.
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs,
    io::Read,
    path::PathBuf,
    process::{Command, ExitStatus, Stdio},
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use vesper::{
    ast::{Expr, ExprKind, Item, Literal, Type, TypeMap},
    check,
    numeric::IntegerKind,
    optimize::{
        witness::{
            Equality, IntOp, Reason, RewriteCertificate, Substitution, Term, TermNode,
            WitnessLimits, verify_rewrite,
        },
        witness_lean::{
            ExportLimits, PreparedLean, ReplayScope, export_rewrite, model_audit_roots,
            model_source, prepare_rewrite,
        },
    },
    parser,
};
use vesper_lean_syntax::{Argument, Declaration, Ident, Module, Name, Term as LeanTerm};

static SERIAL: Mutex<()> = Mutex::new(());
const CAPTURE: usize = 8 * 1024 * 1024;
const FINAL: &str = "Vesper.ReplayWitness.replay";
const POLICY: &[&str] = &["Quot.sound", "propext"];
const KINDS: [IntegerKind; 4] = [
    IntegerKind::U8,
    IntegerKind::U32,
    IntegerKind::U64,
    IntegerKind::I64,
];
const ROOT_SUFFIXES: [&str; 33] = [
    "r01_add_zero",
    "r02_zero_add",
    "r03_sub_zero",
    "r04_mul_one",
    "r05_one_mul",
    "r06_mul_zero",
    "r07_zero_mul",
    "r08_self_sub",
    "r09_wadd_zero",
    "r10_wsub_zero",
    "r11_wmul_one",
    "r12_wadd_comm",
    "r13_wmul_comm",
    "r14_wadd_assoc",
    "r15_wmul_assoc",
    "r16_wadd_cancel",
    "r17_wmul_zero",
    "r18_wself_sub",
    "r19_checked_add_constants",
    "r20_checked_sub_constants",
    "r21_checked_mul_constants",
    "r22_wrapping_add_constants",
    "r23_wrapping_sub_constants",
    "r24_wrapping_mul_constants",
    "value_fits",
    "literal_exact",
    "value_toInt_injective",
    "literal_roundtrip",
    "total_const",
    "total_var",
    "total_of_eq",
    "congr_checked",
    "congr_wrapping",
];

struct Captured {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}
impl Captured {
    fn diagnostic(&self) -> String {
        format!(
            "{}\n{}",
            String::from_utf8_lossy(&self.stdout),
            String::from_utf8_lossy(&self.stderr)
        )
    }
    fn json(&self) -> Value {
        serde_json::from_slice(&self.stdout)
            .unwrap_or_else(|error| panic!("invalid audit JSON: {error}\n{}", self.diagnostic()))
    }
    fn success(&self) {
        assert!(self.status.success(), "{}", self.diagnostic());
    }
}

/// Drain both pipes even after their capture allowance is exhausted.
fn drain(mut pipe: impl Read, overflow: &AtomicBool) -> Vec<u8> {
    let mut output = Vec::new();
    let mut buffer = [0; 8192];
    loop {
        let count = pipe
            .read(&mut buffer)
            .expect("read bounded subprocess pipe");
        if count == 0 {
            break;
        }
        let room = CAPTURE.saturating_sub(output.len());
        output.extend_from_slice(&buffer[..count.min(room)]);
        if count > room {
            overflow.store(true, Ordering::Relaxed);
        }
    }
    output
}

fn execute(command: &mut Command, seconds: u64) -> Captured {
    let started = Instant::now();
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|error| panic!("required real subprocess {command:?}: {error}"));
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let overflow = AtomicBool::new(false);
    let (status, stdout, stderr, refused) = thread::scope(|scope| {
        let out = scope.spawn(|| drain(stdout, &overflow));
        let err = scope.spawn(|| drain(stderr, &overflow));
        let mut refused = false;
        let status = loop {
            if overflow.load(Ordering::Relaxed) || started.elapsed() > Duration::from_secs(seconds)
            {
                refused = true;
                let _ = child.kill();
                break child.wait().expect("reap refused subprocess");
            }
            if let Some(status) = child.try_wait().expect("poll subprocess") {
                break status;
            }
            thread::sleep(Duration::from_millis(20));
        };
        (status, out.join().unwrap(), err.join().unwrap(), refused)
    });
    let captured = Captured {
        status,
        stdout,
        stderr,
    };
    assert!(
        !refused && !overflow.load(Ordering::Relaxed),
        "real subprocess exceeded {seconds}s or 8MiB/stream: {command:?}\n{}",
        captured.diagnostic()
    );
    assert!(
        !captured.stdout.windows(5).any(|bytes| bytes == b"PANIC")
            && !captured.stderr.windows(5).any(|bytes| bytes == b"PANIC"),
        "Lean panicked: {}",
        captured.diagnostic()
    );
    eprintln!(
        "replay42 subprocess {:.2}s: {command:?}",
        started.elapsed().as_secs_f64()
    );
    captured
}

struct Fixture {
    root: PathBuf,
    lean: PathBuf,
    prefix: PathBuf,
    started: Instant,
}
impl Fixture {
    fn new() -> Self {
        // Resolve the launcher while the repository's lean-toolchain is visible.
        // Both absent default tools and incorrect explicitly selected tools FAIL.
        let launcher = std::env::var_os("VESPER_LEAN").unwrap_or_else(|| "lean".into());
        let prefix = execute(
            Command::new(launcher)
                .current_dir(env!("CARGO_MANIFEST_DIR"))
                .arg("--print-prefix"),
            30,
        );
        prefix.success();
        let prefix = PathBuf::from(String::from_utf8(prefix.stdout).unwrap().trim())
            .canonicalize()
            .expect("canonical pinned Lean prefix");
        let lean = prefix
            .join("bin")
            .join(format!("lean{}", std::env::consts::EXE_SUFFIX));
        let version = execute(Command::new(&lean).arg("--version"), 10);
        version.success();
        assert!(
            String::from_utf8_lossy(&version.stdout).contains("version 4.24.0,"),
            "Lean4.24.0 is mandatory: {}",
            version.diagnostic()
        );
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "vesper-replay42-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).expect("fresh private regular-file fixture");
        let fixture = Self {
            root,
            lean,
            prefix,
            started: Instant::now(),
        };
        assert!(
            model_source().len() <= 4 * 1024 * 1024,
            "fixed model fixture is bounded"
        );
        fixture.compile("RewriteWitness", model_source()).success();
        fixture
    }
    fn case(&self, name: &str) -> Self {
        assert!(
            name.bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        );
        let root = self.root.join(name);
        fs::create_dir(&root).unwrap();
        for suffix in ["lean", "olean", "olean.server", "olean.private", "ir"] {
            let name = format!("RewriteWitness.{suffix}");
            if self.root.join(&name).is_file() {
                fs::copy(self.root.join(&name), root.join(&name)).unwrap();
            }
        }
        Self {
            root,
            lean: self.lean.clone(),
            prefix: self.prefix.clone(),
            started: Instant::now(),
        }
    }
    fn compile(&self, module: &str, source: &str) -> Captured {
        assert!(source.len() <= 4 * 1024 * 1024, "bounded acceptance source");
        for suffix in ["olean", "olean.server", "olean.private", "ir"] {
            let path = self.root.join(format!("{module}.{suffix}"));
            if path.exists() {
                fs::remove_file(path).unwrap();
            }
        }
        fs::write(self.root.join(format!("{module}.lean")), source).unwrap();
        let mut command = Command::new(&self.lean);
        command
            .env_clear()
            .current_dir(&self.root)
            .env(
                "LEAN_PATH",
                std::env::join_paths([self.root.clone(), self.prefix.join("lib/lean")]).unwrap(),
            )
            .env("LEAN_ABORT_ON_PANIC", "1");
        windows_environment(&mut command);
        execute(
            command
                .args(["-j1", "-M4096", "-o"])
                .arg(format!("{module}.olean"))
                .arg(format!("{module}.lean")),
            300,
        )
    }
    fn audit(&self, theorems: &[String], policy: &[&str]) -> Captured {
        let mut command = Command::new(env!("CARGO_BIN_EXE_vesper"));
        command
            .args(["proof-audit", "ReplayGenerated", "--root"])
            .arg(&self.root)
            .arg("--lean")
            .arg(&self.lean)
            .args(["--timeout", "300", "--json"]);
        for theorem in theorems {
            command.arg("--theorem").arg(theorem);
        }
        for axiom in policy {
            command.args(["--allow-axiom", axiom]);
        }
        execute(&mut command, 360)
    }
    fn accepted(&self, theorems: &[String]) -> Value {
        let before = fs::read(self.root.join("ReplayGenerated.olean")).unwrap();
        let output = self.audit(theorems, POLICY);
        output.success();
        let report = output.json();
        assert_eq!(report["schema_version"], 1);
        assert_eq!(report["status"], "audited");
        assert_eq!(report["accepted"], true);
        assert_eq!(report["kernel_replayed"], true);
        assert_eq!(report["cache_eligible"], false);
        assert_eq!(report["module"], "ReplayGenerated");
        assert_eq!(report["allowed_axioms"], json!(POLICY));
        assert_eq!(report["forbidden_axioms"], json!([]));
        assert!(
            report["lean_version"]
                .as_str()
                .unwrap()
                .contains("version 4.24.0,")
        );
        assert!(report["checked_constants"].as_u64().unwrap() > 0);
        let actual: BTreeSet<_> = report["theorems"]
            .as_array()
            .unwrap()
            .iter()
            .map(|theorem| {
                for axiom in theorem["axioms"].as_array().unwrap() {
                    assert!(POLICY.contains(&axiom.as_str().unwrap()), "{theorem}");
                }
                assert!(!theorem["type_repr"].as_str().unwrap().is_empty());
                theorem["name"].as_str().unwrap().to_owned()
            })
            .collect();
        assert_eq!(actual, theorems.iter().cloned().collect());
        assert_eq!(actual.len(), report["theorems"].as_array().unwrap().len());
        for file in report["artifact_files"].as_array().unwrap() {
            let bytes = fs::read(self.root.join(file["path"].as_str().unwrap())).unwrap();
            assert_eq!(file["bytes"].as_u64().unwrap(), bytes.len() as u64);
            assert_eq!(file["blake3"], blake3::hash(&bytes).to_hex().to_string());
        }
        // The inventory schema hashes struct fields in path/bytes/blake3 order,
        // whereas a generic serde_json::Value map may reorder those keys.
        #[derive(serde::Serialize)]
        struct FileDigest<'a> {
            path: &'a str,
            bytes: u64,
            blake3: &'a str,
        }
        let inventory: Vec<_> = report["artifact_files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|file| FileDigest {
                path: file["path"].as_str().unwrap(),
                bytes: file["bytes"].as_u64().unwrap(),
                blake3: file["blake3"].as_str().unwrap(),
            })
            .collect();
        assert_eq!(
            report["artifact_inventory_blake3"],
            blake3::hash(&serde_json::to_vec(&inventory).unwrap())
                .to_hex()
                .to_string()
        );
        for name in ["RewriteWitness", "ReplayGenerated"] {
            assert!(
                report["imports"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|module| module["name"] == name && module["scope"] == "artifact")
            );
        }
        assert!(
            report["imports"]
                .as_array()
                .unwrap()
                .iter()
                .any(|module| module["scope"] == "toolchain")
        );
        assert_eq!(
            fs::read(self.root.join("ReplayGenerated.olean")).unwrap(),
            before
        );
        report
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        eprintln!(
            "replay42 fixture total {:.2}s",
            self.started.elapsed().as_secs_f64()
        );
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn windows_environment(command: &mut Command) {
    #[cfg(windows)]
    for key in ["SystemRoot", "WINDIR", "TEMP", "TMP"] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    #[cfg(not(windows))]
    let _ = command;
}

fn checked_island(source: &str) -> (Expr, TypeMap) {
    let parsed = parser::parse(source);
    assert!(
        parsed.diagnostics.is_empty(),
        "{source}\n{:?}",
        parsed.diagnostics
    );
    let checked = check::check(&parsed.module);
    assert!(
        checked.diagnostics.is_empty(),
        "{source}\n{:?}",
        checked.diagnostics
    );
    let Item::Function(function) = &parsed.module.items[0] else {
        panic!("function fixture")
    };
    let ExprKind::Block {
        tail: Some(tail), ..
    } = &function.body.as_ref().unwrap().kind
    else {
        panic!("tail fixture")
    };
    ((**tail).clone(), checked.expression_types)
}
fn kind_name(kind: IntegerKind) -> &'static str {
    match kind {
        IntegerKind::U8 => "u8",
        IntegerKind::U32 => "u32",
        IntegerKind::U64 => "u64",
        IntegerKind::I64 => "i64",
    }
}
fn canonical_candidate(source: &Expr) -> Expr {
    fn spans(value: &mut Expr, root: &std::ops::Range<usize>) {
        value.span = root.clone();
        match &mut value.kind {
            ExprKind::Binary(_, a, b) => {
                spans(a, root);
                spans(b, root);
            }
            ExprKind::Call { args, .. } => {
                for arg in args {
                    spans(&mut arg.expr, root);
                }
            }
            _ => {}
        }
    }
    let mut candidate = source.clone();
    spans(&mut candidate, &source.span);
    candidate
}

struct Catalog {
    kind: IntegerKind,
    terms: Vec<Term>,
    proofs: Vec<Equality>,
}
impl Catalog {
    fn term(&mut self, node: TermNode) -> u32 {
        if let Some(id) = self.terms.iter().position(|term| term.node == node) {
            return id as u32;
        }
        let id = self.terms.len() as u32;
        self.terms.push(Term {
            kind: self.kind,
            node,
        });
        id
    }
    fn bin(&mut self, wrapping: bool, op: IntOp, a: u32, b: u32) -> u32 {
        self.term(if wrapping {
            TermNode::Wrapping {
                op,
                children: [a, b],
            }
        } else {
            TermNode::Checked {
                op,
                children: [a, b],
            }
        })
    }
    fn proof(&mut self, from: u32, to: u32, reason: Reason) -> u32 {
        let id = self.proofs.len() as u32;
        self.proofs.push(Equality { from, to, reason });
        id
    }
    fn rule(
        &mut self,
        rule: u16,
        from: u32,
        to: u32,
        variables: &[u32],
        guard: Option<u32>,
    ) -> u32 {
        let mut substitution = Substitution {
            bindings: [0; 3],
            len: variables.len() as u8,
        };
        substitution.bindings[..variables.len()].copy_from_slice(variables);
        self.proof(
            from,
            to,
            Reason::Rule {
                rule,
                substitution,
                guard,
            },
        )
    }
}

/// Every record is Rust-checked and emitted, including rules unused by conclusion.
fn aggregate(kind: IntegerKind) -> (Expr, Expr, TypeMap, RewriteCertificate) {
    let ty = kind_name(kind);
    let (source, types) = checked_island(&format!(
        "fn f(x:{ty},y:{ty},z:{ty})->{ty} eff [Fail[ArithOverflow]]{{x+(y+z)}}"
    ));
    let candidate = canonical_candidate(&source);
    let mut b = Catalog {
        kind,
        terms: Vec::new(),
        proofs: Vec::new(),
    };
    let x = b.term(TermNode::InputVar(0));
    let y = b.term(TermNode::InputVar(1));
    let z = b.term(TermNode::InputVar(2));
    let zero = b.term(TermNode::Const(0));
    let one = b.term(TermNode::Const(1));
    for rule in 1..=18 {
        let (from, to, variables) = match rule {
            1 => (b.bin(false, IntOp::Add, x, zero), x, vec![x]),
            2 => (b.bin(false, IntOp::Add, zero, x), x, vec![x]),
            3 => (b.bin(false, IntOp::Sub, x, zero), x, vec![x]),
            4 => (b.bin(false, IntOp::Mul, x, one), x, vec![x]),
            5 => (b.bin(false, IntOp::Mul, one, x), x, vec![x]),
            6 => (b.bin(false, IntOp::Mul, x, zero), zero, vec![x]),
            7 => (b.bin(false, IntOp::Mul, zero, x), zero, vec![x]),
            8 => (b.bin(false, IntOp::Sub, x, x), zero, vec![x]),
            9 => (b.bin(true, IntOp::Add, x, zero), x, vec![x]),
            10 => (b.bin(true, IntOp::Sub, x, zero), x, vec![x]),
            11 => (b.bin(true, IntOp::Mul, x, one), x, vec![x]),
            12 | 13 => {
                let op = if rule == 12 { IntOp::Add } else { IntOp::Mul };
                (b.bin(true, op, x, y), b.bin(true, op, y, x), vec![x, y])
            }
            14 | 15 => {
                let op = if rule == 14 { IntOp::Add } else { IntOp::Mul };
                let xy = b.bin(true, op, x, y);
                let yz = b.bin(true, op, y, z);
                (
                    b.bin(true, op, xy, z),
                    b.bin(true, op, x, yz),
                    vec![x, y, z],
                )
            }
            16 => {
                let sum = b.bin(true, IntOp::Add, x, y);
                (b.bin(true, IntOp::Sub, sum, y), x, vec![x, y])
            }
            17 => (b.bin(true, IntOp::Mul, x, zero), zero, vec![x]),
            18 => (b.bin(true, IntOp::Sub, x, x), zero, vec![x]),
            _ => unreachable!(),
        };
        b.rule(rule, from, to, &variables, None);
    }
    for rule in 19..=24 {
        let (op, a, c, result) = match rule {
            19 => (IntOp::Add, 2, 3, 5),
            20 => (IntOp::Sub, 9, 4, 5),
            21 => (IntOp::Mul, 2, 3, 6),
            22 => (IntOp::Add, kind.max(), 1, kind.wrap(kind.max() + 1)),
            23 => (IntOp::Sub, kind.min(), 1, kind.wrap(kind.min() - 1)),
            24 => (
                IntOp::Mul,
                kind.max(),
                kind.max(),
                kind.wrap(kind.max().wrapping_mul(kind.max())),
            ),
            _ => unreachable!(),
        };
        let a = b.term(TermNode::Const(a));
        let c = b.term(TermNode::Const(c));
        let to = b.term(TermNode::Const(result));
        let from = b.bin(rule >= 22, op, a, c);
        b.rule(rule, from, to, &[a, c], None);
    }
    let identity = b.bin(false, IntOp::Add, x, zero);
    let reverse = b.proof(x, identity, Reason::Symmetry { proof: 0 });
    b.proof(
        identity,
        identity,
        Reason::Transitivity {
            first: 0,
            second: reverse,
        },
    );
    let refl = b.proof(y, y, Reason::Refl);
    for wrapping in [false, true] {
        let from = b.bin(wrapping, IntOp::Add, identity, y);
        let to = b.bin(wrapping, IntOp::Add, x, y);
        b.proof(
            from,
            to,
            Reason::Congruence {
                children: [0, refl],
            },
        );
    }
    for guard in [0, reverse] {
        let erased = b.bin(false, IntOp::Mul, identity, zero);
        b.rule(6, erased, zero, &[identity], Some(guard));
    }
    // Reuse one proof twice: actual DAG sharing, not copied proof trees.
    let shared_from = b.bin(true, IntOp::Mul, identity, identity);
    let shared_to = b.bin(true, IntOp::Mul, x, x);
    b.proof(
        shared_from,
        shared_to,
        Reason::Congruence { children: [0, 0] },
    );
    let yz = b.bin(false, IntOp::Add, y, z);
    let input = b.bin(false, IntOp::Add, x, yz);
    let conclusion = b.proof(input, input, Reason::Refl);
    let cert = RewriteCertificate {
        schema_version: 1,
        arithmetic_version: 1,
        terms: b.terms,
        proofs: b.proofs,
        input,
        output: input,
        conclusion,
    };
    assert_eq!(
        verify_rewrite(&source, &candidate, &types, &cert, WitnessLimits::default()),
        Ok(())
    );
    (source, candidate, types, cert)
}

fn kind_tag(kind: IntegerKind) -> u8 {
    match kind {
        IntegerKind::U8 => 0,
        IntegerKind::U32 => 1,
        IntegerKind::U64 => 2,
        IntegerKind::I64 => 3,
    }
}
fn op_tag(op: IntOp) -> u8 {
    match op {
        IntOp::Add => 0,
        IntOp::Sub => 1,
        IntOp::Mul => 2,
    }
}
fn certificate_digest(cert: &RewriteCertificate) -> [u8; 32] {
    let mut bytes = b"vesper.replay.certificate/1\0".to_vec();
    bytes.extend(cert.schema_version.to_le_bytes());
    bytes.extend(cert.arithmetic_version.to_le_bytes());
    bytes.extend((cert.terms.len() as u64).to_le_bytes());
    bytes.extend((cert.proofs.len() as u64).to_le_bytes());
    for id in [cert.input, cert.output, cert.conclusion] {
        bytes.extend(id.to_le_bytes());
    }
    for term in &cert.terms {
        bytes.push(kind_tag(term.kind));
        match term.node {
            TermNode::Const(value) => {
                bytes.push(0);
                bytes.extend(value.to_le_bytes());
            }
            TermNode::InputVar(id) => {
                bytes.push(1);
                bytes.extend(id.to_le_bytes());
            }
            TermNode::Checked { op, children } | TermNode::Wrapping { op, children } => {
                bytes.push(if matches!(term.node, TermNode::Checked { .. }) {
                    2
                } else {
                    3
                });
                bytes.push(op_tag(op));
                for child in children {
                    bytes.extend(child.to_le_bytes());
                }
            }
        }
    }
    for proof in &cert.proofs {
        bytes.extend(proof.from.to_le_bytes());
        bytes.extend(proof.to.to_le_bytes());
        match proof.reason {
            Reason::Refl => bytes.push(0),
            Reason::Rule {
                rule,
                substitution,
                guard,
            } => {
                bytes.push(1);
                bytes.extend(rule.to_le_bytes());
                bytes.push(substitution.len);
                for id in substitution.bindings {
                    bytes.extend(id.to_le_bytes());
                }
                match guard {
                    None => bytes.push(0),
                    Some(id) => {
                        bytes.push(1);
                        bytes.extend(id.to_le_bytes());
                    }
                }
            }
            Reason::Congruence { children } => {
                bytes.push(2);
                for id in children {
                    bytes.extend(id.to_le_bytes());
                }
            }
            Reason::Symmetry { proof } => {
                bytes.push(3);
                bytes.extend(proof.to_le_bytes());
            }
            Reason::Transitivity { first, second } => {
                bytes.push(4);
                bytes.extend(first.to_le_bytes());
                bytes.extend(second.to_le_bytes());
            }
        }
    }
    *blake3::hash(&bytes).as_bytes()
}

fn binding_digest(source: &Expr, candidate: &Expr, types: &TypeMap) -> [u8; 32] {
    fn string(bytes: &mut Vec<u8>, value: &str) {
        bytes.extend((value.len() as u64).to_le_bytes());
        bytes.extend(value.as_bytes());
    }
    fn count(value: &Expr) -> u64 {
        match &value.kind {
            ExprKind::Literal(_) | ExprKind::Name(_) => 1,
            ExprKind::Binary(_, a, b) => 1 + count(a) + count(b),
            ExprKind::Call { args, .. } => 2 + args.iter().map(|arg| count(&arg.expr)).sum::<u64>(),
            _ => panic!("bounded admitted fixture"),
        }
    }
    fn encode(value: &Expr, types: &TypeMap, bytes: &mut Vec<u8>) {
        let tag = match &value.kind {
            ExprKind::Literal(Literal::Int(_)) => 0,
            ExprKind::Name(_) => 1,
            ExprKind::Binary(op, _, _) => match op.as_str() {
                "+" => 2,
                "-" => 3,
                "*" => 4,
                _ => panic!("admitted operator"),
            },
            ExprKind::Call { callee, .. } => {
                let ExprKind::Name(name) = &callee.kind else {
                    panic!("intrinsic name")
                };
                match name.as_str() {
                    "wrapping_add" => 5,
                    "wrapping_sub" => 6,
                    "wrapping_mul" => 7,
                    _ => panic!("intrinsic"),
                }
            }
            _ => panic!("admitted value"),
        };
        bytes.push(tag);
        bytes.extend((value.span.start as u64).to_le_bytes());
        bytes.extend((value.span.end as u64).to_le_bytes());
        match types.get(&(value.span.start, value.span.end)).unwrap() {
            Type::Named(name, args) if args.is_empty() => bytes.push(match name.as_str() {
                "u8" => 1,
                "u32" => 2,
                "u64" => 3,
                "i64" => 4,
                _ => panic!("closed integer type"),
            }),
            Type::IntLiteral(value) => {
                bytes.push(5);
                bytes.extend(value.to_le_bytes());
            }
            _ => panic!("admitted actual lookup"),
        }
        match &value.kind {
            ExprKind::Literal(Literal::Int(value)) => bytes.extend(value.to_le_bytes()),
            ExprKind::Name(name) => string(bytes, name),
            ExprKind::Binary(_, a, b) => {
                encode(a, types, bytes);
                encode(b, types, bytes);
            }
            ExprKind::Call { callee, args, .. } => {
                let ExprKind::Name(name) = &callee.kind else {
                    unreachable!()
                };
                string(bytes, name);
                bytes.extend((callee.span.start as u64).to_le_bytes());
                bytes.extend((callee.span.end as u64).to_le_bytes());
                assert!(!types.contains_key(&(callee.span.start, callee.span.end)));
                bytes.push(0);
                for arg in args {
                    encode(&arg.expr, types, bytes);
                }
            }
            _ => unreachable!(),
        }
    }
    let mut bytes = b"vesper.replay.binding/1\0".to_vec();
    bytes.extend(1u16.to_le_bytes());
    bytes.extend(1u16.to_le_bytes());
    for (role, expression) in [(0, source), (1, candidate)] {
        bytes.push(role);
        bytes.extend(count(expression).to_le_bytes());
        encode(expression, types, &mut bytes);
    }
    *blake3::hash(&bytes).as_bytes()
}

fn declarations(module: &Module) -> &[Declaration] {
    let [Declaration::Namespace { name, declarations }] = module.declarations.as_slice() else {
        panic!("one generated namespace envelope")
    };
    assert_eq!(
        name.components()
            .iter()
            .map(Ident::as_str)
            .collect::<Vec<_>>(),
        ["Vesper", "ReplayWitness"]
    );
    declarations
}
fn declarations_mut(module: &mut Module) -> &mut [Declaration] {
    let [Declaration::Namespace { declarations, .. }] = module.declarations.as_mut_slice() else {
        panic!("one generated namespace envelope")
    };
    declarations
}
fn generated_references(term: &LeanTerm, prefix: char) -> Vec<u32> {
    fn walk(term: &LeanTerm, prefix: char, result: &mut Vec<u32>) {
        match term {
            LeanTerm::Ref(name) => {
                let component = name.components().last().unwrap().as_str();
                if let Some(digits) = component.strip_prefix(prefix)
                    && !digits.is_empty()
                    && digits.bytes().all(|byte| byte.is_ascii_digit())
                {
                    result.push(digits.parse().unwrap());
                }
            }
            LeanTerm::App {
                function,
                arguments,
            } => {
                walk(function, prefix, result);
                for argument in arguments {
                    match argument {
                        Argument::Positional(value) | Argument::Named { value, .. } => {
                            walk(value, prefix, result)
                        }
                    }
                }
            }
            LeanTerm::Ascribe { value, ty } => {
                walk(value, prefix, result);
                walk(ty, prefix, result);
            }
            LeanTerm::Unary { value, .. } => walk(value, prefix, result),
            LeanTerm::Binary { left, right, .. } => {
                walk(left, prefix, result);
                walk(right, prefix, result);
            }
            _ => {}
        }
    }
    let mut result = Vec::new();
    walk(term, prefix, &mut result);
    result
}
fn inspect(
    prepared: &PreparedLean,
    source: &Expr,
    candidate: &Expr,
    types: &TypeMap,
    cert: &RewriteCertificate,
) {
    let manifest = prepared.manifest();
    assert_eq!(manifest.format_version, 1);
    assert_eq!(manifest.model_version, 1);
    assert_eq!(manifest.schema_version, cert.schema_version);
    assert_eq!(manifest.arithmetic_version, cert.arithmetic_version);
    assert_eq!(manifest.module_name, "ReplayGenerated");
    assert_eq!(manifest.theorem, FINAL);
    assert_eq!(manifest.scope, ReplayScope::MathModelEq);
    assert_eq!(manifest.kind, cert.terms[cert.input as usize].kind);
    assert_eq!(
        (manifest.input, manifest.output, manifest.conclusion),
        (cert.input, cert.output, cert.conclusion)
    );
    assert_eq!(
        (manifest.term_count, manifest.proof_count),
        (cert.terms.len(), cert.proofs.len())
    );
    assert_eq!(
        manifest.declaration_count,
        cert.terms.len() + cert.proofs.len() + 2
    );
    assert!(manifest.ast_node_count > manifest.declaration_count);
    assert!(manifest.ast_depth > 0 && manifest.ast_depth <= ExportLimits::default().max_ast_depth);
    assert_eq!(
        manifest.model_blake3,
        *blake3::hash(model_source().as_bytes()).as_bytes()
    );
    assert_eq!(manifest.certificate_blake3, certificate_digest(cert));
    assert_eq!(
        manifest.binding_blake3,
        binding_digest(source, candidate, types)
    );
    let declarations = declarations(prepared.module());
    assert_eq!(declarations.len() + 1, manifest.declaration_count);
    for (id, declaration) in declarations[..cert.terms.len()].iter().enumerate() {
        let Declaration::Definition(definition) = declaration else {
            panic!("named semantic definition")
        };
        assert_eq!(
            definition.name.components().last().unwrap().as_str(),
            format!("n{id}")
        );
        assert!(definition.binders.is_empty());
        let children = match cert.terms[id].node {
            TermNode::Checked { children, .. } | TermNode::Wrapping { children, .. } => {
                children.to_vec()
            }
            _ => Vec::new(),
        };
        assert_eq!(
            generated_references(&definition.body, 'n'),
            children,
            "term edges remain direct named DAG references"
        );
    }
    for (id, declaration) in declarations[cert.terms.len()..cert.terms.len() + cert.proofs.len()]
        .iter()
        .enumerate()
    {
        let Declaration::Theorem {
            name,
            proposition,
            proof,
            ..
        } = declaration
        else {
            panic!("named proof")
        };
        assert_eq!(name.components().last().unwrap().as_str(), format!("p{id}"));
        assert_eq!(
            generated_references(proposition, 'n'),
            [cert.proofs[id].from, cert.proofs[id].to]
        );
        let dependencies = match cert.proofs[id].reason {
            Reason::Refl => vec![],
            Reason::Rule { guard, .. } => guard.into_iter().collect(),
            Reason::Congruence { children } => children.to_vec(),
            Reason::Symmetry { proof } => vec![proof],
            Reason::Transitivity { first, second } => vec![first, second],
        };
        assert_eq!(
            generated_references(proof, 'p'),
            dependencies,
            "proof sharing and guard references are not flattened"
        );
    }
    let Declaration::Theorem { name, proof, .. } = declarations.last().unwrap() else {
        panic!("final theorem")
    };
    assert_eq!(name.components().last().unwrap().as_str(), "replay");
    let LeanTerm::Ref(name) = proof else {
        panic!("final proof remains a DAG reference")
    };
    assert_eq!(
        name.components().last().unwrap().as_str(),
        format!("p{}", cert.conclusion)
    );
}

fn render_checked(prepared: PreparedLean) -> String {
    let bound = prepared.manifest().source_bytes_upper_bound;
    let rendered = prepared.render().expect("one consuming AST serialization");
    assert!(
        rendered.source.len() <= bound
            && rendered.source.len() <= ExportLimits::default().max_source_bytes
    );
    assert_eq!(
        rendered.source_blake3,
        *blake3::hash(rendered.source.as_bytes()).as_bytes()
    );
    rendered.source
}

fn catalog_matrix(base: &Fixture) {
    let expected: Vec<_> = ROOT_SUFFIXES
        .iter()
        .map(|suffix| format!("Vesper.RewriteWitness.{suffix}"))
        .collect();
    assert_eq!(
        model_audit_roots(),
        expected.iter().map(String::as_str).collect::<Vec<_>>()
    );
    for kind in KINDS {
        let fixture = base.case(&format!("catalog-{}", kind_name(kind)));
        let (source, candidate, types, cert) = aggregate(kind);
        let rules: BTreeSet<_> = cert
            .proofs
            .iter()
            .filter_map(|proof| match proof.reason {
                Reason::Rule { rule, .. } => Some(rule),
                _ => None,
            })
            .collect();
        assert_eq!(rules, (1..=24).collect());
        let prepared = export_rewrite(
            &source,
            &candidate,
            &types,
            &cert,
            WitnessLimits::default(),
            ExportLimits::default(),
        )
        .unwrap();
        inspect(&prepared, &source, &candidate, &types, &cert);
        let lean = render_checked(prepared);
        fixture.compile("ReplayGenerated", &lean).success();
        if kind == IntegerKind::U64 {
            let mut roots = expected.clone();
            roots.push(FINAL.into());
            let accepted = fixture.accepted(&roots);
            let denied = fixture.audit(&roots, &[]);
            assert!(!denied.status.success());
            let denied = denied.json();
            assert_eq!(denied["status"], "audited");
            assert_eq!(denied["accepted"], false);
            assert_eq!(denied["kernel_replayed"], true);
            assert_eq!(denied["cache_eligible"], false);
            let closure: BTreeSet<_> = accepted["theorems"]
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|theorem| theorem["axioms"].as_array().unwrap())
                .map(|axiom| axiom.as_str().unwrap())
                .collect();
            assert!(
                !closure.is_empty(),
                "the audited catalog uses the explicitly allowed standard axioms"
            );
            assert_eq!(denied["forbidden_axioms"], json!(closure));
        }
    }
}

fn producer_case(
    base: &Fixture,
    label: &str,
    text: &str,
    audit: bool,
) -> (String, [u8; 32], Option<Value>) {
    let fixture = base.case(label);
    let (source, types) = checked_island(text);
    let result = prepare_rewrite(
        &source,
        &types,
        WitnessLimits::default(),
        ExportLimits::default(),
    )
    .expect("actual producer and cumulative Rust gate")
    .expect("genuine simplification");
    assert_eq!(
        verify_rewrite(
            &source,
            &result.candidate,
            &types,
            &result.certificate,
            WitnessLimits::default()
        ),
        Ok(())
    );
    inspect(
        &result.lean,
        &source,
        &result.candidate,
        &types,
        &result.certificate,
    );
    let digest = result.lean.manifest().binding_blake3;
    let repeated = export_rewrite(
        &source,
        &result.candidate,
        &types,
        &result.certificate,
        WitnessLimits::default(),
        ExportLimits::default(),
    )
    .unwrap();
    assert_eq!(result.lean.manifest(), repeated.manifest());
    let lean = render_checked(result.lean);
    assert_eq!(
        lean,
        render_checked(repeated),
        "deterministic independent prepared exports"
    );
    fixture.compile("ReplayGenerated", &lean).success();
    let report = audit.then(|| fixture.accepted(&[FINAL.into()]));
    (lean, digest, report)
}

fn producer_matrix(base: &Fixture) {
    let mut original_report = None;
    let mut original_binding = None;
    let mut original_lean = None;
    // Exactly24 genuine fold programs: six operators, four integer kinds.
    // Unlike aggregate DTOs these all pass through the actual Runner/Extractor.
    for kind in KINDS {
        let ty = kind_name(kind);
        let expressions = [
            "2+3".to_owned(),
            "9-4".to_owned(),
            "2*3".to_owned(),
            format!("wrapping_add({},1)", kind.max()),
            format!("wrapping_sub({},1)", kind.min()),
            format!("wrapping_mul({0},{0})", kind.max()),
        ];
        for (op, expression) in expressions.iter().enumerate() {
            let text = format!("fn f()->{ty} eff [Fail[ArithOverflow]]{{{expression}}}");
            let audit = kind == IntegerKind::U64 && op == 0;
            let (lean, binding, report) =
                producer_case(base, &format!("producer-{ty}-{op}"), &text, audit);
            if audit {
                original_report = report;
                original_binding = Some(binding);
                original_lean = Some(lean);
            }
        }
        producer_case(
            base,
            &format!("identity-{ty}"),
            &format!("fn f(x:{ty})->{ty} eff [Fail[ArithOverflow]]{{x+0}}"),
            false,
        );
        let (source, types) = checked_island(&format!(
            "fn f()->{ty} eff [Fail[ArithOverflow]]{{({}+1)*0}}",
            kind.max()
        ));
        assert!(
            prepare_rewrite(
                &source,
                &types,
                WitnessLimits::default(),
                ExportLimits::default()
            )
            .expect("checked overflow island refusal")
            .is_none(),
            "overflow cannot be erased"
        );
    }
    for (label, text) in [
        (
            "prior-totality",
            "fn f(x:i64)->i64 eff [Fail[ArithOverflow]]{(x+0)*0}",
        ),
        (
            "signed-min",
            "fn f()->i64{wrapping_sub(-9223372036854775808,1)}",
        ),
        (
            "callee-origin",
            "fn f(π:u64,y:u64)->u64 eff [Fail[ArithOverflow]]{wrapping_add(π+0,y)}",
        ),
    ] {
        let fixture = base.case(label);
        let (source, types) = checked_island(text);
        let prepared = prepare_rewrite(
            &source,
            &types,
            WitnessLimits::default(),
            ExportLimits::default(),
        )
        .unwrap()
        .expect("actual producer result");
        assert_eq!(
            verify_rewrite(
                &source,
                &prepared.candidate,
                &types,
                &prepared.certificate,
                WitnessLimits::default()
            ),
            Ok(())
        );
        if label == "prior-totality" {
            assert!(
                prepared
                    .certificate
                    .proofs
                    .iter()
                    .any(|proof| matches!(proof.reason, Reason::Rule { guard: Some(_), .. })),
                "actual earlier equality supports guarded erasure"
            );
        }
        if label == "callee-origin" {
            let ExprKind::Call { callee: before, .. } = &source.kind else {
                panic!("source intrinsic")
            };
            let ExprKind::Call { callee: after, .. } = &prepared.candidate.kind else {
                panic!("retained intrinsic")
            };
            assert!(before.span.start > 0);
            assert_eq!(before.span, after.span);
            assert_eq!(before.kind, after.kind);
            assert_eq!(&text[after.span.clone()], "wrapping_add");
        }
        inspect(
            &prepared.lean,
            &source,
            &prepared.candidate,
            &types,
            &prepared.certificate,
        );
        fixture
            .compile("ReplayGenerated", &render_checked(prepared.lean))
            .success();
    }
    let (lean, binding, report) =
        producer_case(base, "valid-changed-artifact", "fn f()->u64{2+4}", true);
    assert_ne!(Some(binding), original_binding);
    assert_ne!(Some(lean), original_lean);
    assert_ne!(
        report.unwrap()["artifact_inventory_blake3"],
        original_report.unwrap()["artifact_inventory_blake3"],
        "a distinct valid equality may pass audit while its actual inventory changes"
    );
}

fn name(path: &str) -> Name {
    Name::qualified(path.split('.').map(|part| Ident::new(part).unwrap()))
        .unwrap()
        .rooted()
}
fn reference(path: &str) -> LeanTerm {
    LeanTerm::Ref(name(path))
}

/// The export's closed syntax subset; generated terms/proofs keep DAG links.
fn mutate_term(term: &mut LeanTerm, change: &mut impl FnMut(&mut LeanTerm) -> bool) -> bool {
    if change(term) {
        return true;
    }
    match term {
        LeanTerm::App {
            function,
            arguments,
        } => {
            if mutate_term(function, change) {
                return true;
            }
            arguments.iter_mut().any(|argument| match argument {
                Argument::Positional(value) | Argument::Named { value, .. } => {
                    mutate_term(value, change)
                }
            })
        }
        LeanTerm::Ascribe { value, ty } => mutate_term(value, change) || mutate_term(ty, change),
        LeanTerm::Unary { value, .. } => mutate_term(value, change),
        LeanTerm::Binary { left, right, .. } => {
            mutate_term(left, change) || mutate_term(right, change)
        }
        _ => false,
    }
}
fn output_definition(module: &mut Module, output: u32) -> &mut vesper_lean_syntax::Definition {
    declarations_mut(module)
        .iter_mut()
        .find_map(|declaration| match declaration {
            Declaration::Definition(definition)
                if definition.name.components().last().unwrap().as_str()
                    == format!("n{output}") =>
            {
                Some(definition)
            }
            _ => None,
        })
        .unwrap()
}
fn reject_ast(base: &Fixture, label: &str, module: Module) {
    let fixture = base.case(label);
    let source = module
        .render()
        .expect("one adversarial caller AST serialization");
    let output = fixture.compile("ReplayGenerated", &source);
    assert!(
        !output.status.success(),
        "mutated proof must fail real Lean: {label}"
    );
    assert!(
        !fixture.root.join("ReplayGenerated.olean").exists(),
        "failed compile creates no accepted artifact"
    );
    let text = output.diagnostic();
    assert!(
        text.contains("error:"),
        "actual elaboration/kernel error, not a launch failure: {text}"
    );
}
fn audit_error(output: Captured) {
    assert!(!output.status.success(), "{}", output.diagnostic());
    let report = output.json();
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["status"], "error");
    assert_eq!(report["error"]["kind"], "audit_failed");
    assert!(report.get("accepted").is_none());
    let message = report["error"]["message"].as_str().unwrap();
    assert!(
        !message.is_empty() && !message.contains("timed out"),
        "real rejection cause: {message}"
    );
}

fn negative_matrix(base: &Fixture) {
    let (source, types) = checked_island("fn f()->u64{2+3}");
    let prepared = prepare_rewrite(
        &source,
        &types,
        WitnessLimits::default(),
        ExportLimits::default(),
    )
    .unwrap()
    .expect("genuine five-fold artifact");
    assert!(matches!(
        prepared.candidate.kind,
        ExprKind::Literal(Literal::Int(5))
    ));
    let output = prepared.certificate.output;
    let mut wrong_constant = prepared.lean.module().clone();
    let definition = output_definition(&mut wrong_constant, output);
    assert!(mutate_term(&mut definition.body, &mut |term| {
        if matches!(term, LeanTerm::Nat(n) if n.as_str() == "5") {
            *term = LeanTerm::Nat(6u64.into());
            true
        } else {
            false
        }
    }));
    reject_ast(base, "wrong-output-constant", wrong_constant);

    let mut wrong_kind = prepared.lean.module().clone();
    assert!(mutate_term(
        &mut output_definition(&mut wrong_kind, output).result,
        &mut |term| {
            if matches!(term, LeanTerm::Ref(name) if name.components().last().unwrap().as_str() == "u64")
            {
                *term = reference("Vesper.RewriteWitness.Kind.i64");
                true
            } else {
                false
            }
        }
    ));
    reject_ast(base, "wrong-nominal-kind", wrong_kind);

    let mut wrong_reference = prepared.lean.module().clone();
    let Declaration::Theorem { proof, .. } =
        declarations_mut(&mut wrong_reference).last_mut().unwrap()
    else {
        panic!("final theorem")
    };
    *proof = reference("Vesper.ReplayWitness.p_missing");
    reject_ast(base, "wrong-proof-reference", wrong_reference);

    let (source, candidate, types, certificate) = aggregate(IntegerKind::I64);
    let guard_id = certificate
        .proofs
        .iter()
        .position(|proof| matches!(proof.reason, Reason::Rule { guard: Some(_), .. }))
        .unwrap();
    let aggregate = export_rewrite(
        &source,
        &candidate,
        &types,
        &certificate,
        WitnessLimits::default(),
        ExportLimits::default(),
    )
    .unwrap();
    let mut bad_guard = aggregate.module().clone();
    let Declaration::Theorem { proof, .. } =
        &mut declarations_mut(&mut bad_guard)[certificate.terms.len() + guard_id]
    else {
        panic!("guarded theorem")
    };
    assert!(mutate_term(proof, &mut |term| {
        if matches!(term, LeanTerm::Ref(name) if name.components().last().unwrap().as_str() == "total_of_eq")
        {
            *term = reference("True.intro");
            true
        } else {
            false
        }
    }));
    reject_ast(base, "wrong-totality-evidence", bad_guard);

    let fixture = base.case("corrupt-compiled-artifact");
    let lean = render_checked(prepared.lean);
    fixture.compile("ReplayGenerated", &lean).success();
    let artifact = fixture.root.join("ReplayGenerated.olean");
    let bytes = fs::read(&artifact).unwrap();
    assert!(bytes.len() > 64);
    fs::write(&artifact, &bytes[..11]).unwrap();
    audit_error(fixture.audit(&[FINAL.into()], POLICY));

    // Missing/wrong explicit tool selections are errors, never skipped success.
    for lean in [
        fixture.root.join("nonexistent-lean"),
        PathBuf::from(env!("CARGO_BIN_EXE_vesper")),
    ] {
        let output = execute(
            Command::new(env!("CARGO_BIN_EXE_vesper"))
                .args(["proof-audit", "ReplayGenerated", "--root"])
                .arg(&fixture.root)
                .args(["--theorem", FINAL, "--json", "--lean"])
                .arg(lean),
            45,
        );
        audit_error(output);
    }
    let option = execute(
        Command::new(env!("CARGO_BIN_EXE_vesper"))
            .args(["proof-audit", "ReplayGenerated", "--root"])
            .arg(&fixture.root)
            .args(["--theorem", FINAL, "--allow-axiom", "sorryAx", "--json"]),
        10,
    );
    assert!(!option.status.success());
    assert!(option.diagnostic().contains("sorryAx cannot be allowed"));
    assert!(
        option.stdout.is_empty(),
        "CLI preflight failure has no invented audit report"
    );
}

fn policy_matrix(base: &Fixture) {
    let fixture = base.case("classical-choice-policy");
    // Entire hostile theorem is ordinary syntax AST; no raw axiom/tactic text.
    let module = Module {
        imports: vec![vesper_lean_syntax::ModuleName::new("RewriteWitness").unwrap()],
        declarations: vec![Declaration::Namespace {
            name: Name::qualified([
                Ident::new("Vesper").unwrap(),
                Ident::new("ReplayWitness").unwrap(),
            ])
            .unwrap(),
            declarations: vec![Declaration::Theorem {
                name: Name::single("replay").unwrap(),
                binders: vec![],
                proposition: reference("True"),
                proof: LeanTerm::app(
                    reference("Classical.choice"),
                    [LeanTerm::app(
                        reference("Nonempty.intro"),
                        [reference("True.intro")],
                    )],
                ),
            }],
        }],
    };
    fixture
        .compile("ReplayGenerated", &module.render().unwrap())
        .success();
    for policy in [&[][..], POLICY] {
        let denied = fixture.audit(&[FINAL.into()], policy);
        assert!(!denied.status.success());
        let report = denied.json();
        assert_eq!(report["status"], "audited");
        assert_eq!(report["accepted"], false);
        assert_eq!(report["kernel_replayed"], true);
        assert_eq!(report["cache_eligible"], false);
        assert_eq!(report["forbidden_axioms"], json!(["Classical.choice"]));
        assert_eq!(report["theorems"][0]["axioms"], json!(["Classical.choice"]));
    }
    // Deliberately broaden ONLY this hostile fixture policy to demonstrate
    // policy enforcement independently of kernel validity. This is not an
    // accepted rewrite artifact or the production model policy.
    let explicit = fixture.audit(&[FINAL.into()], &["Classical.choice"]);
    explicit.success();
    let report = explicit.json();
    assert_eq!(report["accepted"], true);
    assert_eq!(report["kernel_replayed"], true);
    assert_eq!(report["cache_eligible"], false);
    assert_eq!(report["allowed_axioms"], json!(["Classical.choice"]));
}

#[test]
fn real_kernel_replay_catalog_producer_and_hostile_artifacts() {
    let _serial = SERIAL.lock().unwrap_or_else(|poison| poison.into_inner());
    let started = Instant::now();
    let base = Fixture::new(); // Exactly one static model compilation per target.
    catalog_matrix(&base);
    producer_matrix(&base);
    negative_matrix(&base);
    policy_matrix(&base);
    eprintln!(
        "replay42 complete serial target wall time {:.2}s",
        started.elapsed().as_secs_f64()
    );
}
