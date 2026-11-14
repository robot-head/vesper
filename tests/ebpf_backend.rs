//! Acceptance tests of the compiler eBPF target (§27, §7; lane 26, #349):
//! Vesper source -> `vesper-ebpf` Asm -> verifier model -> interpreter
//! verdicts -> BPF ELF with BTF -> (where `bpf()` is permitted) the running
//! kernel's verifier and `BPF_PROG_TEST_RUN`.
use vesper::ebpf_backend::{Compiled, EbpfBackendError, Options, compile_source, rules};
use vesper_ebpf::interp::{XdpEnv, run_xdp};
use vesper_ebpf::{MapKind, Program, ProgramType, elf};

const XDP_ABORTED: u64 = 0;
const XDP_DROP: u64 = 1;
const XDP_PASS: u64 = 2;
const TC_ACT_OK: u64 = 0;
const TC_ACT_SHOT: u64 = 2;

fn fixture(name: &str) -> String {
    let path = format!(
        "{}/tests/fixtures/ebpf_backend/{name}.ves",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn compile(name: &str) -> Compiled {
    let options = Options {
        file: format!("{name}.ves"),
        ..Options::default()
    };
    compile_source(&fixture(name), &options).unwrap_or_else(|e| panic!("{name}: {e}"))
}

// ------------------------------------------------------------ packets --

fn checksum(bytes: &[u8]) -> u16 {
    let mut sum = 0u32;
    for w in bytes.chunks(2) {
        sum += u32::from(u16::from_be_bytes([w[0], *w.get(1).unwrap_or(&0)]));
    }
    while sum > 0xffff {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

/// Ethernet + IPv4 (no options) + an 8-byte transport header with ports.
fn ipv4(src: [u8; 4], proto: u8, dport: u16) -> Vec<u8> {
    let mut p = vec![0u8; 14];
    p[0..6].copy_from_slice(&[0x02, 0, 0, 0, 0, 1]);
    p[6..12].copy_from_slice(&[0x02, 0, 0, 0, 0, 2]);
    p[12..14].copy_from_slice(&0x0800u16.to_be_bytes());
    let mut ip = [0u8; 20];
    ip[0] = 0x45;
    ip[2..4].copy_from_slice(&(20u16 + 20).to_be_bytes());
    ip[8] = 64;
    ip[9] = proto;
    ip[12..16].copy_from_slice(&src);
    ip[16..20].copy_from_slice(&[192, 0, 2, 1]);
    let c = checksum(&ip);
    ip[10..12].copy_from_slice(&c.to_be_bytes());
    p.extend_from_slice(&ip);
    let mut l4 = [0u8; 20];
    l4[0..2].copy_from_slice(&40000u16.to_be_bytes());
    l4[2..4].copy_from_slice(&dport.to_be_bytes());
    p.extend_from_slice(&l4);
    p
}

fn arp() -> Vec<u8> {
    let mut p = ipv4([10, 0, 0, 1], 6, 80);
    p[12..14].copy_from_slice(&0x0806u16.to_be_bytes());
    p
}

const A: [u8; 4] = [198, 51, 100, 7];
const B: [u8; 4] = [203, 0, 113, 9];

fn key(src: [u8; 4]) -> [u8; 4] {
    // The program reads the address with load_u32be: host order key.
    u32::from_be_bytes(src).to_le_bytes()
}

fn run(prog: &Program, env: &mut XdpEnv, packet: &[u8]) -> u64 {
    run_xdp(prog, env, packet)
        .unwrap_or_else(|e| panic!("interpreter: {e:?}"))
        .ret
}

/// The interpreter models `struct xdp_md`. A tc program reads `data` and
/// `data_end` from `struct __sk_buff` offsets 76 and 80; this test-only
/// shim rewrites those two context loads to the XDP offsets so the same
/// instructions run in the interpreter. It is not used for the verifier
/// model or the kernel, which see the real tc program.
fn tc_as_xdp_for_interpreter(prog: &Program) -> Program {
    let mut p = prog.clone();
    p.prog_type = ProgramType::Xdp;
    for i in &mut p.insns {
        // LDX W r?, [r9 + off]
        if i.code == 0x61 && i.src == 9 {
            i.off = match i.off {
                76 => 0,
                80 => 4,
                o => panic!("unexpected tc context load at offset {o}"),
            };
        }
    }
    p
}

/// (what, packet builder, expected verdict).
type Case = (&'static str, fn() -> Vec<u8>, u64);

const FIREWALL_TRACE: &[Case] = &[
    ("ARP passes", arp, XDP_PASS),
    ("truncated Ethernet passes", || vec![0u8; 10], XDP_PASS),
    (
        "truncated IPv4 aborts",
        || ipv4(A, 6, 80)[..20].to_vec(),
        XDP_ABORTED,
    ),
    ("A to port 80 passes", || ipv4(A, 6, 80), XDP_PASS),
    (
        "A to telnet is dropped and A is blocked",
        || ipv4(A, 6, 23),
        XDP_DROP,
    ),
    ("A to port 80 is now dropped", || ipv4(A, 6, 80), XDP_DROP),
    ("A over UDP is dropped too", || ipv4(A, 17, 53), XDP_DROP),
    ("B to port 80 passes", || ipv4(B, 6, 80), XDP_PASS),
    (
        "B to UDP port 23 passes (not TCP)",
        || ipv4(B, 17, 23),
        XDP_PASS,
    ),
];

// ------------------------------------------------- the verifier model --

#[test]
fn example_programs_pass_the_verifier_model() {
    let fw = compile("firewall");
    assert_eq!(fw.prog_type(), ProgramType::Xdp);
    assert_eq!(fw.program.maps.len(), 2);
    let bl = &fw.program.maps[fw.map_index("Blocklist").unwrap()];
    assert_eq!(
        (
            bl.name.as_str(),
            bl.kind,
            bl.key_size,
            bl.value_size,
            bl.max_entries
        ),
        ("blocklist", MapKind::Hash, 4, 1, 1024)
    );
    let drops = &fw.program.maps[fw.map_index("Drops").unwrap()];
    assert_eq!(
        (drops.kind, drops.key_size, drops.value_size),
        (MapKind::LruHash, 4, 8)
    );
    // Subprograms became BPF-to-BPF calls.
    assert!(
        fw.report.call_graph.functions.len() >= 4,
        "{:?}",
        fw.report.call_graph
    );
    assert!(fw.report.worst_stack <= 512);
    assert!(fw.loops.is_empty());

    let counter = compile("counter");
    assert_eq!(counter.loops, ["header_sum"]);
    let c = &counter.program.maps[0];
    assert_eq!(
        (c.kind, c.key_size, c.value_size, c.max_entries),
        (MapKind::PerCpuArray, 4, 8, 4)
    );
    // The loop's back edge is in the code, and the verifier model walked
    // its iterations.
    assert!(counter.report.insns_processed > counter.program.insns.len() as u64);

    let tc = compile("tc_guard");
    assert_eq!(tc.prog_type(), ProgramType::SchedCls);
    for c in [&fw, &counter, &tc] {
        for (f, bytes) in &c.frames {
            assert!(*bytes <= 512, "{f}: {bytes}");
        }
        // Every function starts with BTF line info.
        assert!(!c.program.lines.is_empty());
        assert_eq!(c.program.lines[0].slot, 0);
    }
}

// ------------------------------------------------------ interpreter --

#[test]
fn firewall_verdicts_in_the_interpreter() {
    let fw = compile("firewall");
    let mut env = XdpEnv::new(&fw.program);
    for (what, packet, want) in FIREWALL_TRACE {
        assert_eq!(run(&fw.program, &mut env, &packet()), *want, "{what}");
    }
    let drops = fw.map_index("Drops").unwrap();
    let count = env.map_get(drops, &key(A)).expect("A was counted");
    assert_eq!(u64::from_le_bytes(count.try_into().unwrap()), 3);
    assert_eq!(env.map_get(drops, &key(B)), None);
    let bl = fw.map_index("Blocklist").unwrap();
    assert_eq!(env.map_get(bl, &key(A)), Some(vec![1]));
    assert_eq!(env.map_get(bl, &key(B)), None);
}

fn counter_trace() -> Vec<(&'static str, Vec<u8>, u64)> {
    let mut bad = ipv4(A, 6, 80);
    bad[24] ^= 0x01; // corrupt the header checksum
    vec![
        ("TCP", ipv4(A, 6, 80), XDP_PASS),
        ("TCP", ipv4(B, 6, 443), XDP_PASS),
        ("UDP", ipv4(A, 17, 53), XDP_PASS),
        ("ICMP", ipv4(A, 1, 0), XDP_PASS),
        ("bad checksum", bad, XDP_DROP),
        ("ARP", arp(), XDP_PASS),
        (
            "truncated IPv4 header",
            ipv4(A, 6, 80)[..30].to_vec(),
            XDP_PASS,
        ),
    ]
}

#[test]
fn counter_verdicts_and_counts_in_the_interpreter() {
    let counter = compile("counter");
    let mut env = XdpEnv::new(&counter.program);
    for (what, packet, want) in counter_trace() {
        assert_eq!(run(&counter.program, &mut env, &packet), want, "{what}");
    }
    let counts: Vec<u64> = (0u32..4)
        .map(|k| {
            u64::from_le_bytes(
                env.map_get(0, &k.to_le_bytes())
                    .expect("array slot")
                    .try_into()
                    .unwrap(),
            )
        })
        .collect();
    assert_eq!(counts, [2, 1, 1, 1]);
}

fn tc_trace() -> Vec<(&'static str, Vec<u8>, u64)> {
    vec![
        ("ARP", arp(), TC_ACT_OK),
        ("DNS from outside", ipv4(A, 17, 53), TC_ACT_SHOT),
        ("DNS from 10/8", ipv4([10, 1, 2, 3], 17, 53), TC_ACT_OK),
        ("TCP 53 from outside", ipv4(A, 6, 53), TC_ACT_OK),
        ("UDP 123 from outside", ipv4(B, 17, 123), TC_ACT_OK),
        (
            "UDP header cut off",
            ipv4(A, 17, 53)[..34].to_vec(),
            TC_ACT_SHOT,
        ),
    ]
}

#[test]
fn tc_verdicts_in_the_interpreter() {
    let tc = compile("tc_guard");
    let prog = tc_as_xdp_for_interpreter(&tc.program);
    let mut env = XdpEnv::new(&prog);
    env.now_ns = 1_000;
    for (what, packet, want) in tc_trace() {
        assert_eq!(run(&prog, &mut env, &packet), want, "{what}");
    }
    let seen = tc.map_index("LastSeen").unwrap();
    assert!(env.map_get(seen, &key(A)).is_some());
    assert!(env.map_get(seen, &key([10, 1, 2, 3])).is_some());
}

// -------------------------------------------------------- ELF + BTF --

fn has(bytes: &[u8], needle: &[u8]) -> bool {
    bytes.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn objects_carry_btf_and_round_trip() {
    for name in ["firewall", "counter", "tc_guard"] {
        let c = compile(name);
        let obj = c.object().unwrap();
        assert!(obj.starts_with(b"\x7fELF"));
        assert!(has(&obj, b".BTF\0"), "{name}: .BTF");
        assert!(has(&obj, b".BTF.ext\0"), "{name}: .BTF.ext");
        assert!(has(&obj, b".maps\0"), "{name}: .maps");
        let back = elf::read_object(&obj).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(back.insns, c.program.insns, "{name}");
        assert_eq!(back.maps, c.program.maps, "{name}");
        assert_eq!(back.prog_type, c.program.prog_type, "{name}");
        // The read-back program is still accepted by the verifier model.
        vesper_ebpf::verify(&back, &Default::default()).unwrap();
    }
}

// ----------------------------------------------------------- kernel --

#[cfg(target_os = "linux")]
mod kernel {
    use super::*;
    use vesper_ebpf::kernel::{self, LoadError};

    fn available() -> bool {
        match kernel::probe() {
            Ok(()) => true,
            Err(e) => {
                eprintln!("kernel BPF evidence unavailable on this host: {e}");
                false
            }
        }
    }

    fn load(c: &Compiled) -> kernel::Loaded {
        match kernel::load(&c.program) {
            Ok(l) => l,
            Err(LoadError::Rejected { log, .. }) => panic!("kernel verifier rejected:\n{log}"),
            Err(e) => panic!("{e}"),
        }
    }

    #[test]
    fn kernel_accepts_the_examples_with_btf_and_line_info() {
        if !available() {
            return;
        }
        for name in ["firewall", "counter", "tc_guard"] {
            let c = compile(name);
            load(&c);
            // Program BTF, func info and line info as emitted for the ELF.
            if let Err(e) = kernel::load_with_ext(&c.program) {
                panic!("{name}: kernel rejected the program with its BTF: {e}");
            }
        }
    }

    #[test]
    fn kernel_verdicts_match_the_interpreter() {
        if !available() {
            return;
        }
        kernel::pin_to_cpu(0).ok();
        // Firewall: same trace, same verdicts, same map contents.
        let fw = compile("firewall");
        let loaded = load(&fw);
        for (what, packet, want) in FIREWALL_TRACE {
            let packet = packet();
            if packet.len() < 14 {
                // BPF_PROG_TEST_RUN refuses XDP input shorter than an
                // Ethernet header (EINVAL); the interpreter covers it.
                continue;
            }
            let r = kernel::test_run(&loaded.prog, &packet).unwrap();
            assert_eq!(u64::from(r.retval), *want, "kernel: {what}");
        }
        let drops = &loaded.maps[fw.map_index("Drops").unwrap()];
        let v = kernel::lookup(drops, &key(A)).unwrap().expect("A counted");
        assert_eq!(u64::from_le_bytes(v[..8].try_into().unwrap()), 3);
        let bl = &loaded.maps[fw.map_index("Blocklist").unwrap()];
        assert_eq!(kernel::lookup(bl, &key(A)).unwrap(), Some(vec![1]));
        assert_eq!(kernel::lookup(bl, &key(B)).unwrap(), None);

        // Counter: per-CPU sums.
        let counter = compile("counter");
        let loaded = load(&counter);
        for (what, packet, want) in counter_trace() {
            let r = kernel::test_run(&loaded.prog, &packet).unwrap();
            assert_eq!(u64::from(r.retval), want, "kernel: {what}");
        }
        let counts: Vec<u64> = (0u32..4)
            .map(|k| {
                let v = kernel::lookup(&loaded.maps[0], &k.to_le_bytes())
                    .unwrap()
                    .expect("array slot");
                v.chunks(8)
                    .map(|c| u64::from_le_bytes(c.try_into().unwrap()))
                    .sum()
            })
            .collect();
        assert_eq!(counts, [2, 1, 1, 1]);

        // tc: the kernel runs the real `__sk_buff` program.
        let tc = compile("tc_guard");
        let loaded = load(&tc);
        for (what, packet, want) in tc_trace() {
            let r =
                kernel::test_run(&loaded.prog, &packet).unwrap_or_else(|e| panic!("{what}: {e}"));
            assert_eq!(u64::from(r.retval), want, "kernel: {what}");
        }
        let seen = &loaded.maps[tc.map_index("LastSeen").unwrap()];
        let t = kernel::lookup(seen, &key(A)).unwrap().expect("A seen");
        assert!(u64::from_le_bytes(t[..8].try_into().unwrap()) > 0);
    }
}

// ------------------------------------------------ out-of-subset input --

const PRELUDE: &str = "copy enum XdpAction { Aborted, Drop, Pass, Tx, Redirect }
copy enum BpfMapKind { Hash, Array, PerCpuHash, PerCpuArray, LruHash }
copy struct BpfMapSpec { kind: BpfMapKind, max_entries: u32 }
effect Packet { fn load_u8(offset: u32) -> Option[u8]; }
";

fn rejected(body: &str) -> EbpfBackendError {
    let src = format!("{PRELUDE}{body}");
    match compile_source(&src, &Options::default()) {
        Ok(_) => panic!("accepted:\n{body}"),
        Err(e) => e,
    }
}

#[test]
fn out_of_subset_programs_get_typed_errors() {
    let cases: &[(&str, &str)] = &[
        // Entry shape.
        (rules::ENTRY, "fn main() -> u64 { 0 }"),
        (rules::ENTRY, "fn main(x: u32) -> XdpAction { XdpAction::Pass }"),
        (rules::ENTRY, "fn other() -> XdpAction { XdpAction::Pass }\nfn helper() -> u64 { 1 }"),
        // Recursion: non-tail self-call and mutual recursion.
        (
            rules::RECURSION,
            "fn depth(n: u32) -> u32 { if n == 0 { 0 } else { wrapping_add(depth(wrapping_sub(n, 1)), 1) } }
             fn main() -> XdpAction { if depth(3) == 3 { XdpAction::Pass } else { XdpAction::Drop } }",
        ),
        (
            rules::RECURSION,
            "fn even(n: u32) -> bool { if n == 0 { true } else { odd(wrapping_sub(n, 1)) } }
             fn odd(n: u32) -> bool { if n == 0 { false } else { even(wrapping_sub(n, 1)) } }
             fn main() -> XdpAction { if even(4) { XdpAction::Pass } else { XdpAction::Drop } }",
        ),
        // Checked arithmetic the checker could not prove total.
        (
            rules::FALLIBLE,
            "fn bump(x: u32) -> u32 eff [Fail[ArithOverflow]] { x + 1 }
             fn main() -> XdpAction eff [Fail[ArithOverflow]] { if bump(1) == 2 { XdpAction::Pass } else { XdpAction::Drop } }",
        ),
        // Closures (function-typed values) and handlers.
        (
            rules::TYPE,
            "fn adder(sink k: u32) -> fn @copy (u32) -> u32 { | v | wrapping_add(v, k) }
             fn main() -> XdpAction { let f = adder(1); if f(1) == 2 { XdpAction::Pass } else { XdpAction::Drop } }",
        ),
        (
            rules::OPERATION,
            "effect Tick { fn tick() -> u32; }
             fn main() -> XdpAction {
                 let n = handle Tick in { Tick::tick() } with { return value => value, tick(resume) => resume(1), };
                 if n == 1 { XdpAction::Pass } else { XdpAction::Drop }
             }",
        ),
        // Struct values at run time.
        (
            rules::TYPE,
            "copy struct Pair { a: u32, b: u32 }
             fn sum(p: Pair) -> u32 { wrapping_add(p.a, p.b) }
             fn main() -> XdpAction { let p = Pair { a: 1, b: 2 }; if sum(p) == 3 { XdpAction::Pass } else { XdpAction::Drop } }",
        ),
        // Unknown effect and malformed recognised effect.
        (
            rules::EFFECT,
            "effect Clock { fn tick() -> u64; fn reset(a: u32, b: u32, c: u32) -> Unit; }
             fn main() -> XdpAction eff [Clock] { if Clock::tick() == 0 { XdpAction::Pass } else { XdpAction::Drop } }",
        ),
        // Map effect without its specification function.
        (
            rules::MAP,
            "effect Seen { fn lookup(key: u32) -> Option[u64]; }
             fn main() -> XdpAction eff [Seen] { match Seen::lookup(1) { Some(_) => XdpAction::Drop, None => XdpAction::Pass } }",
        ),
        // Array map with a byte value for add.
        (
            rules::MAP,
            "effect Hits { fn add(key: u32, delta: u8) -> Unit; }
             fn hits_map() -> BpfMapSpec { BpfMapSpec { kind: BpfMapKind::Array, max_entries: 4 } }
             fn main() -> XdpAction eff [Hits] { Hits::add(0, 1); XdpAction::Pass }",
        ),
        // Signed division.
        (
            rules::OPERATION,
            "fn half(x: i64) -> i64 { x / 2 }
             fn main() -> XdpAction { if half(4) == 2 { XdpAction::Pass } else { XdpAction::Drop } }",
        ),
        // Too many parameters for the BPF calling convention.
        (
            rules::CALL,
            "fn five(a: u32, b: u32, c: u32, d: u32, e: u32) -> u32 { wrapping_add(a, wrapping_add(b, wrapping_add(c, wrapping_add(d, e)))) }
             fn main() -> XdpAction { if five(1, 2, 3, 4, 5) == 15 { XdpAction::Pass } else { XdpAction::Drop } }",
        ),
        // Option parameters are not passed in registers.
        (
            rules::CALL,
            "fn has(x: Option[u32]) -> bool { match x { Some(_) => true, None => false } }
             fn main() -> XdpAction { if has(Some(1)) { XdpAction::Pass } else { XdpAction::Drop } }",
        ),
        // Generic functions.
        (
            rules::GENERIC,
            "fn id[T](sink x: T) -> T { x }
             fn main() -> XdpAction { if id(1) == 1 { XdpAction::Pass } else { XdpAction::Drop } }",
        ),
    ];
    for (rule, body) in cases {
        let e = rejected(body);
        assert_eq!(e.rule(), *rule, "{body}\n-> {e}");
    }

    // Action enum with the wrong variant order.
    let src = "copy enum XdpAction { Drop, Aborted, Pass, Tx, Redirect }
               fn main() -> XdpAction { XdpAction::Pass }";
    let e = compile_source(src, &Options::default()).unwrap_err();
    assert_eq!(e.rule(), rules::ACTION, "{e}");

    // Source errors stay source errors.
    let e = compile_source("fn main( -> XdpAction", &Options::default()).unwrap_err();
    assert_eq!(e.rule(), "source");
}

#[test]
fn unbounded_loops_are_rejected_by_the_verifier_model() {
    // A tail self-call with no bound: lowered to a loop, which the
    // verifier model refuses (it never reaches an exit within the budget).
    let e = rejected(
        "fn spin(i: u32) -> u32 { if i == 4294967295 { i } else { spin(wrapping_add(i, 0)) } }
         fn main() -> XdpAction { if spin(0) == 0 { XdpAction::Pass } else { XdpAction::Drop } }",
    );
    assert!(matches!(e, EbpfBackendError::Verifier(_)), "{e}");
    assert!(
        matches!(e.rule(), "loop.unbounded" | "complexity.too_many_insns"),
        "{e}"
    );
}

#[test]
fn frames_over_the_ebpf_stack_are_rejected() {
    // 70 live u64 locals need 560 bytes in one frame.
    let mut body = String::from("fn big(x: u64) -> u64 {\n");
    for k in 0..70 {
        body.push_str(&format!("    let v{k} = wrapping_add(x, {k});\n"));
    }
    body.push_str("    let s0 = v0;\n");
    for k in 1..70 {
        body.push_str(&format!("    let s{k} = wrapping_add(s{}, v{k});\n", k - 1));
    }
    body.push_str("    s69\n}\nfn main() -> XdpAction { if big(1) == 0 { XdpAction::Drop } else { XdpAction::Pass } }");
    let e = rejected(&body);
    assert_eq!(e.rule(), rules::STACK, "{e}");
}

// -------------------------------------------------------------- CLI --

#[test]
fn build_target_bpf_writes_an_object_the_ebpf_checker_accepts() {
    let dir = std::env::temp_dir().join(format!("vesper-ebpf-backend-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let out = dir.join("firewall.o");
    let src = format!(
        "{}/tests/fixtures/ebpf_backend/firewall.ves",
        env!("CARGO_MANIFEST_DIR")
    );
    let exe = env!("CARGO_BIN_EXE_vesper");
    let r = std::process::Command::new(exe)
        .args(["build", "--target", "bpf", &src, "-o"])
        .arg(&out)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&r.stdout);
    assert!(
        r.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&r.stderr)
    );
    assert!(stdout.contains("verifier model: accepted"), "{stdout}");
    assert!(
        stdout.contains("map blocklist <- effect Blocklist"),
        "{stdout}"
    );
    let r = std::process::Command::new(exe)
        .args(["ebpf", "check"])
        .arg(&out)
        .output()
        .unwrap();
    assert!(
        r.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&r.stdout),
        String::from_utf8_lossy(&r.stderr)
    );

    // Out-of-subset source fails with the rule id.
    let bad = dir.join("bad.ves");
    std::fs::write(
        &bad,
        format!("{PRELUDE}fn main(x: u32) -> XdpAction {{ XdpAction::Pass }}"),
    )
    .unwrap();
    let r = std::process::Command::new(exe)
        .args(["build", "--target", "bpf"])
        .arg(&bad)
        .output()
        .unwrap();
    assert!(!r.status.success());
    assert!(String::from_utf8_lossy(&r.stderr).contains("[ebpf.entry]"));
    std::fs::remove_dir_all(&dir).ok();
}

// ------------------------------------------------------- robustness --

#[test]
fn malformed_and_unusual_inputs_never_panic() {
    for name in ["firewall", "counter", "tc_guard"] {
        let src = fixture(name);
        // Every function as the entry: a typed result either way.
        let checked = vesper::check::check(&vesper::parser::parse(&src).module);
        for f in checked.world.functions.keys() {
            let options = Options {
                entry: f.clone(),
                ..Options::default()
            };
            if let Err(e) = compile_source(&src, &options) {
                assert!(!e.to_string().is_empty());
            }
        }
        // Truncated sources.
        for cut in (0..src.len()).step_by(13) {
            if src.is_char_boundary(cut) {
                let _ = compile_source(&src[..cut], &Options::default());
            }
        }
        // Literals swapped for extreme values.
        for (from, to) in [
            ("2048", "18446744073709551615"),
            ("23", "0"),
            ("1024", "0"),
            ("10", "4294967295"),
        ] {
            let _ = compile_source(&src.replace(from, to), &Options::default());
        }
    }
}

#[test]
fn option_equality_compares_payloads() {
    // Some(a) == Some(b) only when a == b; None == None; Some != None.
    let src = format!(
        "{PRELUDE}fn main() -> XdpAction eff [Packet] {{
             if Packet::load_u8(0) == Packet::load_u8(1) {{ XdpAction::Pass }} else {{ XdpAction::Drop }}
         }}"
    );
    let c = compile_source(&src, &Options::default()).unwrap();
    let mut env = XdpEnv::new(&c.program);
    assert_eq!(run(&c.program, &mut env, &[7, 7, 0]), XDP_PASS);
    assert_eq!(run(&c.program, &mut env, &[1, 2, 0]), XDP_DROP);
    // load_u8(0) is Some, load_u8(1) is None.
    assert_eq!(run(&c.program, &mut env, &[1]), XDP_DROP);
    let ne = src.replace("==", "!=");
    let c = compile_source(&ne, &Options::default()).unwrap();
    let mut env = XdpEnv::new(&c.program);
    assert_eq!(run(&c.program, &mut env, &[7, 7, 0]), XDP_DROP);
    assert_eq!(run(&c.program, &mut env, &[1, 2, 0]), XDP_PASS);
}

#[test]
fn hash_add_inserts_with_noexist_and_retries() {
    // The miss path inserts with BPF_NOEXIST and, if another CPU won the
    // race, re-looks up and adds atomically instead of overwriting.
    let fw = compile("firewall");
    let slots = &fw.program.insns;
    let noexist = slots.windows(2).any(|w| {
        w[0].code == 0xb7 && w[0].dst == 4 && w[0].imm == 1 && w[1].code == 0x85 && w[1].imm == 2
    });
    assert!(noexist, "update with BPF_NOEXIST");
}
