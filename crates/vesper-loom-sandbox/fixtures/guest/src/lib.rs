//! Probe component used by the vesper-loom-sandbox integration tests.
//!
//! Each `entry` exercises one authority the host must grant or deny. The
//! component reports what it observed; it never decides policy itself.

use std::fmt::Write as _;

wit_bindgen::generate!({
    path: "../../wit",
    world: "sandboxed-step",
});

use exports::vesper::sandbox::step::Guest;
use vesper::sandbox::types::{
    Invocation, PackedDiagnostic, PackedResult, PackedToken, Severity, Slice, Span, StepKind,
    Token, TokenKind,
};

/// Unpacked guest-side diagnostic; packed into the string table on return.
struct Diagnostic {
    severity: Severity,
    message: String,
    span: Option<Span>,
}

struct Outcome {
    tokens: Vec<Token>,
    diagnostics: Vec<Diagnostic>,
}

#[derive(Default)]
struct Packer {
    out: Option<PackedResult>,
}

impl Packer {
    fn result(&mut self) -> &mut PackedResult {
        self.out.get_or_insert_with(|| PackedResult {
            tokens: Vec::new(),
            diagnostics: Vec::new(),
            text: Vec::new(),
            scopes: Vec::new(),
        })
    }

    fn text(&mut self, s: &str) -> Slice {
        let r = self.result();
        let offset = r.text.len() as u32;
        r.text.extend_from_slice(s.as_bytes());
        Slice {
            offset,
            len: s.len() as u32,
        }
    }

    fn token(&mut self, t: Token) {
        let text = self.text(&t.text);
        let r = self.result();
        let scopes = Slice {
            offset: r.scopes.len() as u32,
            len: t.scopes.len() as u32,
        };
        r.scopes.extend_from_slice(&t.scopes);
        r.tokens.push(PackedToken {
            kind: t.kind,
            text,
            span: t.span,
            scopes,
            pre_resolved: t.pre_resolved,
        });
    }

    fn diag(&mut self, d: Diagnostic) {
        let message = self.text(&d.message);
        self.result().diagnostics.push(PackedDiagnostic {
            severity: d.severity,
            message,
            span: d.span,
        });
    }

    fn finish(mut self) -> PackedResult {
        self.result();
        self.out.take().unwrap()
    }
}

fn pack(r: Result<Outcome, Vec<Diagnostic>>) -> Result<PackedResult, PackedResult> {
    let mut p = Packer::default();
    match r {
        Ok(o) => {
            for t in o.tokens {
                p.token(t);
            }
            for d in o.diagnostics {
                p.diag(d);
            }
            Ok(p.finish())
        }
        Err(ds) => {
            for d in ds {
                p.diag(d);
            }
            Err(p.finish())
        }
    }
}

fn empty_result() -> PackedResult {
    PackedResult {
        tokens: Vec::new(),
        diagnostics: Vec::new(),
        text: Vec::new(),
        scopes: Vec::new(),
    }
}

fn raw_token(text: Slice, scopes: Slice) -> PackedToken {
    PackedToken {
        kind: TokenKind::Ident,
        text,
        span: Span {
            file: 0,
            start: 0,
            end: 0,
        },
        scopes,
        pre_resolved: None,
    }
}

/// Hostile results that a correct host must reject without amplifying.
fn hostile(entry: &str) -> Option<Result<PackedResult, PackedResult>> {
    let empty = Slice { offset: 0, len: 0 };
    let mut r = empty_result();
    match entry {
        // 1 MiB of text referenced by 100k tokens: 100 GiB if copied naively.
        "alias-bomb" => {
            r.text = vec![b'a'; 1 << 20];
            let all = Slice {
                offset: 0,
                len: 1 << 20,
            };
            r.tokens = (0..100_000).map(|_| raw_token(all, empty)).collect();
        }
        "alias-bomb-diagnostics" => {
            r.text = vec![b'a'; 1 << 20];
            let all = Slice {
                offset: 0,
                len: 1 << 20,
            };
            r.diagnostics = (0..100_000)
                .map(|_| PackedDiagnostic {
                    severity: Severity::Error,
                    message: all,
                    span: None,
                })
                .collect();
            return Some(Err(r));
        }
        "scope-bomb" => {
            r.scopes = vec![1; 1 << 16];
            let all = Slice {
                offset: 0,
                len: 1 << 16,
            };
            r.tokens = (0..100_000).map(|_| raw_token(empty, all)).collect();
        }
        "bad-slice" => {
            r.text = b"abc".to_vec();
            r.tokens = vec![raw_token(
                Slice {
                    offset: 2,
                    len: u32::MAX,
                },
                empty,
            )];
        }
        "bad-utf8" => {
            r.text = vec![0xff, 0xfe];
            r.tokens = vec![raw_token(Slice { offset: 0, len: 2 }, empty)];
        }
        "error-with-tokens" => {
            r.text = b"x".to_vec();
            r.tokens = vec![raw_token(Slice { offset: 0, len: 1 }, empty)];
            return Some(Err(r));
        }
        _ => return None,
    }
    Some(Ok(r))
}

const PROTOCOL_VERSION: u32 = 1;

struct Fixture;

fn diag(message: impl Into<String>) -> Diagnostic {
    Diagnostic {
        severity: Severity::Error,
        message: message.into(),
        span: None,
    }
}

fn text_token(kind: TokenKind, text: impl Into<String>) -> Token {
    Token {
        kind,
        text: text.into(),
        span: Span {
            file: 0,
            start: 0,
            end: 0,
        },
        scopes: Vec::new(),
        pre_resolved: None,
    }
}

fn strings(values: impl IntoIterator<Item = String>) -> Outcome {
    Outcome {
        tokens: values
            .into_iter()
            .map(|v| text_token(TokenKind::StringLiteral, v))
            .collect(),
        diagnostics: Vec::new(),
    }
}

fn arg(inv: &Invocation, i: usize) -> Result<&str, Vec<Diagnostic>> {
    inv.args
        .get(i)
        .map(String::as_str)
        .ok_or_else(|| vec![diag(format!("missing argument {i}"))])
}

fn io_err(op: &str, path: &str, e: std::io::Error) -> Vec<Diagnostic> {
    vec![diag(format!("{op} {path}: {:?}: {e}", e.kind()))]
}

impl Guest for Fixture {
    fn protocol_version() -> u32 {
        PROTOCOL_VERSION
    }

    fn run(inv: Invocation) -> Result<PackedResult, PackedResult> {
        if let Some(r) = hostile(&inv.entry) {
            return r;
        }
        pack(run(inv))
    }
}

fn run(inv: Invocation) -> Result<Outcome, Vec<Diagnostic>> {
    {
        if inv.protocol_version != PROTOCOL_VERSION {
            return Err(vec![diag(format!(
                "protocol {} unsupported",
                inv.protocol_version
            ))]);
        }
        match inv.entry.as_str() {
            "echo" => echo(inv),
            "codegen" => codegen(&inv),
            "read" => {
                let path = arg(&inv, 0)?;
                let body = std::fs::read_to_string(path).map_err(|e| io_err("read", path, e))?;
                Ok(strings([body]))
            }
            "write" => {
                let path = arg(&inv, 0)?;
                let body = arg(&inv, 1)?;
                std::fs::write(path, body).map_err(|e| io_err("write", path, e))?;
                Ok(strings([]))
            }
            "list-dir" => {
                let path = arg(&inv, 0)?;
                let mut names: Vec<String> = std::fs::read_dir(path)
                    .map_err(|e| io_err("read_dir", path, e))?
                    .map(|e| e.map(|e| e.file_name().to_string_lossy().into_owned()))
                    .collect::<Result<_, _>>()
                    .map_err(|e| io_err("read_dir", path, e))?;
                names.sort();
                Ok(strings(names))
            }
            "authority" => Ok(authority()),
            "tcp-connect" => tcp_connect(arg(&inv, 0)?),
            "udp-bind" => udp_bind(),
            "dns" => dns(arg(&inv, 0)?),
            "clock-random" => Ok(clock_random()),
            "spin" => spin(),
            "alloc" => alloc(),
            "sleep" => {
                let secs: u64 = arg(&inv, 0)?
                    .parse()
                    .map_err(|_| vec![diag("bad seconds")])?;
                std::thread::sleep(std::time::Duration::from_secs(secs));
                Ok(strings(["woke".to_string()]))
            }
            "panic" => panic!("fixture panic requested"),
            "fs-metadata" => fs_metadata(),
            "stdout-flood" => {
                let line = "x".repeat(1023);
                for _ in 0..4096 {
                    println!("{line}");
                }
                Ok(strings([]))
            }
            other => Err(vec![diag(format!("unknown entry `{other}`"))]),
        }
    }
}

/// Macro-shaped step: returns its input tokens, preserving spans and marks,
/// and reports the step kind it was invoked as.
fn echo(inv: Invocation) -> Result<Outcome, Vec<Diagnostic>> {
    let kind = match inv.step {
        StepKind::BuildScript => "build-script",
        StepKind::ProcMacro => "proc-macro",
        StepKind::DeriveMacro => "derive-macro",
    };
    Ok(Outcome {
        tokens: inv.tokens,
        diagnostics: vec![Diagnostic {
            severity: Severity::Note,
            message: format!("echo as {kind}"),
            span: None,
        }],
    })
}

/// Build-step shaped: reads a `name: type` schema and writes generated source.
fn codegen(inv: &Invocation) -> Result<Outcome, Vec<Diagnostic>> {
    let input = arg(inv, 0)?;
    let output = arg(inv, 1)?;
    let schema = std::fs::read_to_string(input).map_err(|e| io_err("read", input, e))?;
    let mut generated =
        String::from("// generated by vesper-sandbox-fixture\nstruct Generated {\n");
    for line in schema.lines().filter(|l| !l.trim().is_empty()) {
        let (name, ty) = line
            .split_once(':')
            .ok_or_else(|| vec![diag(format!("bad schema line `{line}`"))])?;
        let _ = writeln!(generated, "    {}: {},", name.trim(), ty.trim());
    }
    generated.push_str("}\n");
    std::fs::write(output, &generated).map_err(|e| io_err("write", output, e))?;
    Ok(Outcome {
        tokens: vec![text_token(TokenKind::StringLiteral, output)],
        diagnostics: vec![Diagnostic {
            severity: Severity::Note,
            message: format!("wrote {} bytes", generated.len()),
            span: None,
        }],
    })
}

/// Reports environment, arguments, cwd and preopens as `key=value` strings.
#[cfg(target_env = "p2")]
fn authority() -> Outcome {
    use wasip2::cli::environment;
    use wasip2::filesystem::preopens;
    let mut out = Vec::new();
    for (k, v) in environment::get_environment() {
        out.push(format!("env:{k}={v}"));
    }
    for a in environment::get_arguments() {
        out.push(format!("arg:{a}"));
    }
    out.push(format!("cwd:{:?}", environment::initial_cwd()));
    let mut pre: Vec<String> = preopens::get_directories()
        .into_iter()
        .map(|(_, p)| p)
        .collect();
    pre.sort();
    for p in pre {
        out.push(format!("preopen:{p}"));
    }
    out.push(format!("std-env-count:{}", std::env::vars_os().count()));
    strings(out)
}

#[cfg(target_env = "p2")]
fn tcp_connect(addr: &str) -> Result<Outcome, Vec<Diagnostic>> {
    use wasip2::sockets::network::{IpAddressFamily, IpSocketAddress, Ipv4SocketAddress};
    use wasip2::sockets::{instance_network, tcp_create_socket};
    let sa: std::net::SocketAddrV4 = addr.parse().map_err(|_| vec![diag("bad address")])?;
    let net = instance_network::instance_network();
    let sock = tcp_create_socket::create_tcp_socket(IpAddressFamily::Ipv4)
        .map_err(|e| vec![diag(format!("tcp create: {e:?}"))])?;
    let o = sa.ip().octets();
    let remote = IpSocketAddress::Ipv4(Ipv4SocketAddress {
        port: sa.port(),
        address: (o[0], o[1], o[2], o[3]),
    });
    sock.start_connect(&net, remote)
        .map_err(|e| vec![diag(format!("tcp start-connect: {e:?}"))])?;
    sock.subscribe().block();
    sock.finish_connect()
        .map_err(|e| vec![diag(format!("tcp finish-connect: {e:?}"))])?;
    Ok(strings(["connected".to_string()]))
}

#[cfg(target_env = "p2")]
fn udp_bind() -> Result<Outcome, Vec<Diagnostic>> {
    use wasip2::sockets::network::{IpAddressFamily, IpSocketAddress, Ipv4SocketAddress};
    use wasip2::sockets::{instance_network, udp_create_socket};
    let net = instance_network::instance_network();
    let sock = udp_create_socket::create_udp_socket(IpAddressFamily::Ipv4)
        .map_err(|e| vec![diag(format!("udp create: {e:?}"))])?;
    let local = IpSocketAddress::Ipv4(Ipv4SocketAddress {
        port: 0,
        address: (127, 0, 0, 1),
    });
    sock.start_bind(&net, local)
        .map_err(|e| vec![diag(format!("udp start-bind: {e:?}"))])?;
    sock.subscribe().block();
    sock.finish_bind()
        .map_err(|e| vec![diag(format!("udp finish-bind: {e:?}"))])?;
    Ok(strings(["bound".to_string()]))
}

#[cfg(target_env = "p2")]
fn dns(name: &str) -> Result<Outcome, Vec<Diagnostic>> {
    use wasip2::sockets::{instance_network, ip_name_lookup};
    let net = instance_network::instance_network();
    let stream = ip_name_lookup::resolve_addresses(&net, name)
        .map_err(|e| vec![diag(format!("dns resolve: {e:?}"))])?;
    let mut found = Vec::new();
    loop {
        match stream.resolve_next_address() {
            Ok(Some(addr)) => found.push(format!("{addr:?}")),
            Ok(None) => break,
            Err(wasip2::sockets::network::ErrorCode::WouldBlock) => stream.subscribe().block(),
            Err(e) => return Err(vec![diag(format!("dns next: {e:?}"))]),
        }
    }
    Ok(strings(found))
}

#[cfg(target_env = "p2")]
fn clock_random() -> Outcome {
    use wasip2::clocks::{monotonic_clock, wall_clock};
    use wasip2::random::{insecure, insecure_seed, random};
    let hex = |b: Vec<u8>| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
    let wall = wall_clock::now();
    let since_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(u128::MAX);
    let (s0, s1) = insecure_seed::insecure_seed();
    // Exercise std's own randomness path as well (HashMap seeding).
    let mut probe = std::collections::HashMap::new();
    probe.insert(1u8, 1u8);
    strings([
        format!("wall:{}.{:09}", wall.seconds, wall.nanoseconds),
        format!("std-wall-ns:{since_epoch}"),
        format!("wall-res:{:?}", wall_clock::resolution()),
        format!("mono:{}", monotonic_clock::now()),
        format!("mono-res:{}", monotonic_clock::resolution()),
        format!("random:{}", hex(random::get_random_bytes(16))),
        format!("random-u64:{}", random::get_random_u64()),
        format!("insecure:{}", hex(insecure::get_insecure_random_bytes(16))),
        format!("seed:{s0:016x}{s1:016x}"),
    ])
}

#[cfg(not(target_env = "p2"))]
fn authority() -> Outcome {
    strings(std::env::vars().map(|(k, v)| format!("env:{k}={v}")))
}

#[cfg(not(target_env = "p2"))]
fn tcp_connect(_: &str) -> Result<Outcome, Vec<Diagnostic>> {
    Err(vec![diag("sockets unavailable on preview1")])
}

#[cfg(not(target_env = "p2"))]
fn udp_bind() -> Result<Outcome, Vec<Diagnostic>> {
    Err(vec![diag("sockets unavailable on preview1")])
}

#[cfg(not(target_env = "p2"))]
fn dns(_: &str) -> Result<Outcome, Vec<Diagnostic>> {
    Err(vec![diag("sockets unavailable on preview1")])
}

#[cfg(not(target_env = "p2"))]
fn clock_random() -> Outcome {
    strings([])
}

fn spin() -> ! {
    let mut x: u64 = 0;
    loop {
        x = std::hint::black_box(x.wrapping_mul(6364136223846793005).wrapping_add(1));
    }
}

fn alloc() -> Result<Outcome, Vec<Diagnostic>> {
    let mut chunks: Vec<Vec<u8>> = Vec::new();
    loop {
        // Touch every page so the allocation is real, not just reserved.
        let chunk = vec![0xA5u8; 1 << 20];
        std::hint::black_box(&chunk);
        chunks.push(chunk);
        if chunks.len() > 1 << 16 {
            return Ok(strings(["allocated 64 GiB?".to_string()]));
        }
    }
}

export!(Fixture);

/// Reports metadata a host filesystem would leak: timestamps, inode-like
/// identities, and raw (unsorted) directory order.
#[cfg(target_env = "p2")]
fn metadata_hash(dir: &str, name: &str) -> String {
    use wasip2::filesystem::{preopens, types::PathFlags};
    for (d, path) in preopens::get_directories() {
        if path == dir {
            return match d.metadata_hash_at(PathFlags::empty(), name) {
                Ok(h) => format!("{:x}:{:x}", h.upper, h.lower),
                Err(e) => format!("err:{e:?}"),
            };
        }
    }
    "no-preopen".into()
}

#[cfg(not(target_env = "p2"))]
fn metadata_hash(_: &str, _: &str) -> String {
    "unsupported".into()
}

fn fs_metadata() -> Result<Outcome, Vec<Diagnostic>> {
    let secs = |t: std::io::Result<std::time::SystemTime>| match t {
        Ok(t) => match t.duration_since(std::time::UNIX_EPOCH) {
            Ok(d) => format!("{}.{:09}", d.as_secs(), d.subsec_nanos()),
            Err(_) => "before-epoch".into(),
        },
        Err(e) => format!("err:{:?}", e.kind()),
    };
    std::fs::write("/out/probe.txt", "probe").map_err(|e| io_err("write", "/out/probe.txt", e))?;
    let mut out = Vec::new();
    for path in ["/in/proto/schema.txt", "/out/probe.txt", "/in/proto"] {
        let m = std::fs::metadata(path).map_err(|e| io_err("stat", path, e))?;
        out.push(format!(
            "{path} len={} mtime={} atime={}",
            if m.is_dir() { 0 } else { m.len() },
            secs(m.modified()),
            secs(m.accessed()),
        ));
    }
    out.push(format!(
        "hash:schema={}",
        metadata_hash("/in/proto", "schema.txt")
    ));
    out.push(format!("hash:probe={}", metadata_hash("/out", "probe.txt")));
    out.push(format!(
        "hash:schema-again={}",
        metadata_hash("/in/proto", "schema.txt")
    ));
    let order: Vec<String> = std::fs::read_dir("/in/proto")
        .map_err(|e| io_err("read_dir", "/in/proto", e))?
        .map(|e| e.map(|e| e.file_name().to_string_lossy().into_owned()))
        .collect::<Result<_, _>>()
        .map_err(|e| io_err("read_dir", "/in/proto", e))?;
    out.push(format!("order={}", order.join(",")));
    Ok(strings(out))
}
