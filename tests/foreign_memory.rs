//! Foreign memory viewed in place (§16.3, Rust-exit gap wave 1 lane 8,
//! #1505): `Pinned[T]` buffers whose address C keeps until the program
//! releases them, and `linear extern mapping` memory that C owns and Vesper
//! reads and writes in place through `.data`.
//!
//! The worked example is an AF_XDP-shaped receive path
//! (tests/fixtures/foreign_memory): the socket keeps the UMEM's address (a
//! pinned buffer) and writes frames into it during later calls, and the
//! receive ring is a mapping the socket owns, with the producer and
//! consumer indices and the descriptors in it. The JIT binds the imports to
//! the Rust stand-in below on every host.
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
};
use vesper::ffi_lowering::{self, FfiErrorKind, Handler, Imports};
use vesper::runtime::Value;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/foreign_memory")
}
fn fixture(name: &str) -> String {
    fs::read_to_string(fixtures().join(name)).unwrap()
}
/// The host's `.vsys` target and calling convention, if `vesper-ffi` has one.
fn host_target() -> Option<(&'static str, &'static str)> {
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some(("x86_64-unknown-linux-gnu", "sysv64"))
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        Some(("aarch64-unknown-linux-gnu", "aapcs64"))
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some(("aarch64-apple-darwin", "darwin64"))
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        Some(("x86_64-pc-windows-msvc", "win64"))
    } else {
        None
    }
}
const ZERO: &str = "0000000000000000000000000000000000000000000000000000000000000000";
const LINUX: (&str, &str) = ("x86_64-unknown-linux-gnu", "sysv64");
fn vsys(target: (&str, &str)) -> String {
    fixture("xsk.vsys")
        .replace("@TARGET@", target.0)
        .replace("@ABI@", target.1)
        .replace("@DIGEST@", ZERO)
}
fn imports(target: (&str, &str)) -> Imports {
    let mut imports = Imports::new();
    imports.add(&vsys(target), Handler::Direct).unwrap();
    imports
}
fn checked(source: &str, target: (&str, &str)) -> ffi_lowering::ForeignChecked {
    ffi_lowering::check_source(source, &imports(target)).unwrap_or_else(|e| panic!("{e}"))
}
fn word(v: &Value) -> u64 {
    match v {
        Value::Int(n) => *n as u64,
        Value::UInt(n, _) => *n,
        other => panic!("not a word: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// The stand-in socket and kernel.

const FRAME: usize = 2048;
const FRAMES: usize = 16;
const ENTRIES: u32 = 8;
const RING_BYTES: usize = 64 + 16 * ENTRIES as usize;

struct Socket {
    /// The UMEM the socket was given: it writes frames here later.
    umem: *mut u8,
    umem_len: usize,
    frame_size: u32,
    ring: *mut u8,
    /// Packets received so far: the payload seed and the next frame.
    received: u32,
}

/// Live sockets plus live mappings, which every scenario must end at zero.
static LIVE: AtomicU64 = AtomicU64::new(0);
static JIT: Mutex<()> = Mutex::new(());

fn ring_layout() -> std::alloc::Layout {
    std::alloc::Layout::from_size_align(RING_BYTES, 64).unwrap()
}
/// The ring's initial bytes: zero indices, then a fixed pattern.
fn ring_pattern(i: usize) -> u8 {
    if i < 8 { 0 } else { (i * 7 % 251) as u8 }
}
/// The length of packet `k` and its payload byte at offset `i`.
fn packet_len(k: u32) -> usize {
    64 + (k as usize % 7) * 100
}
fn payload(k: u32, i: usize) -> u8 {
    (k as usize * 31 + i) as u8
}

extern "C" fn xsk_socket_new(umem: *mut u8, umem_len: usize, frame_size: u32) -> *mut Socket {
    if umem.is_null() || umem_len < FRAME * FRAMES {
        return std::ptr::null_mut();
    }
    LIVE.fetch_add(1, Ordering::SeqCst);
    Box::into_raw(Box::new(Socket {
        umem,
        umem_len,
        frame_size,
        ring: std::ptr::null_mut(),
        received: 0,
    }))
}
extern "C" fn xsk_ring_map(s: *mut Socket, bytes: usize) -> *mut u8 {
    // MAP_FAILED, not NULL, as mmap reports it.
    if bytes != RING_BYTES {
        return usize::MAX as *mut u8;
    }
    // SAFETY: a fresh allocation of the ring's layout, initialized below.
    let ring = unsafe { std::alloc::alloc(ring_layout()) };
    for i in 0..RING_BYTES {
        // SAFETY: `i` is inside the allocation.
        unsafe { *ring.add(i) = ring_pattern(i) };
    }
    // SAFETY: `s` is a live socket lent `in`.
    unsafe { (*s).ring = ring };
    LIVE.fetch_add(1, Ordering::SeqCst);
    ring
}
extern "C" fn xsk_kernel_rx(s: *mut Socket, packets: u32) -> u32 {
    // SAFETY: `s` is a live socket lent `inout`; its UMEM is the pinned
    // buffer it was given, still unreleased, and its ring is mapped.
    let s = unsafe { &mut *s };
    let (umem, ring) = unsafe {
        (
            std::slice::from_raw_parts_mut(s.umem, s.umem_len),
            std::slice::from_raw_parts_mut(s.ring, RING_BYTES),
        )
    };
    let read = |ring: &[u8], at: usize| u32::from_le_bytes(ring[at..at + 4].try_into().unwrap());
    let mut got = 0;
    for _ in 0..packets {
        let producer = read(ring, 0);
        let consumer = read(ring, 4);
        if producer.wrapping_sub(consumer) == ENTRIES {
            break;
        }
        let k = s.received;
        let addr = (k as usize % FRAMES) * s.frame_size as usize;
        let len = packet_len(k);
        for i in 0..len {
            umem[addr + i] = payload(k, i);
        }
        let at = 64 + 16 * (producer % ENTRIES) as usize;
        ring[at..at + 8].copy_from_slice(&(addr as u64).to_le_bytes());
        ring[at + 8..at + 12].copy_from_slice(&(len as u32).to_le_bytes());
        ring[0..4].copy_from_slice(&producer.wrapping_add(1).to_le_bytes());
        s.received += 1;
        got += 1;
    }
    got
}
extern "C" fn xsk_ring_unmap(ring: *mut u8, len: usize) -> i32 {
    assert_eq!(len, RING_BYTES, "C receives the mapping's length");
    LIVE.fetch_sub(1, Ordering::SeqCst);
    // SAFETY: the ring came from xsk_ring_map; `sink` releases it once.
    unsafe { std::alloc::dealloc(ring, ring_layout()) };
    0
}
extern "C" fn xsk_socket_free(s: *mut Socket) {
    LIVE.fetch_sub(1, Ordering::SeqCst);
    // SAFETY: the sink import consumes the socket exactly once.
    drop(unsafe { Box::from_raw(s) });
}
extern "C" fn xsk_live() -> u64 {
    LIVE.load(Ordering::SeqCst)
}
fn ring_of(s: *const Socket) -> &'static mut [u8] {
    // SAFETY: `s` is a live socket lent to the import, its ring mapped.
    unsafe { std::slice::from_raw_parts_mut((*s).ring, RING_BYTES) }
}
fn ring_u32(s: *const Socket, at: usize) -> u32 {
    u32::from_le_bytes(ring_of(s)[at..at + 4].try_into().unwrap())
}
extern "C" fn xsk_rx_producer(s: *const Socket) -> u32 {
    ring_u32(s, 0)
}
extern "C" fn xsk_rx_consumer(s: *const Socket) -> u32 {
    ring_u32(s, 4)
}
extern "C" fn xsk_rx_desc_addr(s: *const Socket, i: u32) -> u64 {
    let at = 64 + 16 * (i % ENTRIES) as usize;
    u64::from_le_bytes(ring_of(s)[at..at + 8].try_into().unwrap())
}
extern "C" fn xsk_rx_desc_len(s: *const Socket, i: u32) -> u32 {
    ring_u32(s, 64 + 16 * (i % ENTRIES) as usize + 8)
}
extern "C" fn xsk_rx_release(s: *mut Socket, consumer: u32) {
    ring_of(s)[4..8].copy_from_slice(&consumer.to_le_bytes());
}
extern "C" fn xsk_copy_out(s: *const Socket, addr: u64, dst: *mut u8, len: usize) -> u32 {
    // SAFETY: the UMEM is the socket's pinned buffer; `dst` is lent for
    // the call.
    unsafe {
        let s = &*s;
        let addr = addr as usize;
        if addr + len > s.umem_len {
            return 1;
        }
        std::ptr::copy_nonoverlapping(s.umem.add(addr), dst, len);
    }
    0
}
fn symbols() -> Vec<(&'static str, *const u8)> {
    vec![
        ("xsk_socket_new", xsk_socket_new as *const u8),
        ("xsk_ring_map", xsk_ring_map as *const u8),
        ("xsk_kernel_rx", xsk_kernel_rx as *const u8),
        ("xsk_ring_unmap", xsk_ring_unmap as *const u8),
        ("xsk_socket_free", xsk_socket_free as *const u8),
        ("xsk_live", xsk_live as *const u8),
        ("xsk_rx_producer", xsk_rx_producer as *const u8),
        ("xsk_rx_consumer", xsk_rx_consumer as *const u8),
        ("xsk_rx_desc_addr", xsk_rx_desc_addr as *const u8),
        ("xsk_rx_desc_len", xsk_rx_desc_len as *const u8),
        ("xsk_rx_release", xsk_rx_release as *const u8),
        ("xsk_copy_out", xsk_copy_out as *const u8),
    ]
}

/// RFC 1071-style big-endian word sum, as `Slice[u8]::sum_be16`.
fn sum_be16(bytes: &[u8]) -> u64 {
    bytes
        .chunks(2)
        .map(|c| (u64::from(c[0]) << 8) | c.get(1).map_or(0, |b| u64::from(*b)))
        .sum()
}
fn expected_rx(packets: u32) -> u64 {
    (0..packets)
        .map(|k| {
            let bytes: Vec<u8> = (0..packet_len(k)).map(|i| payload(k, i)).collect();
            sum_be16(&bytes)
        })
        .fold(0u64, u64::wrapping_add)
}

#[test]
fn jit_receives_into_a_pinned_umem_through_a_mapped_ring() {
    let Some(target) = host_target() else {
        eprintln!("no .vsys target for this host; JIT evidence unavailable");
        return;
    };
    let _lock = JIT.lock().unwrap();
    let checked = checked(&fixture("xsk.ves"), target);
    let symbols = symbols();
    let ring: Vec<u8> = (0..RING_BYTES).map(ring_pattern).collect();
    // 37 packets in batches of 5 through an 8-entry ring: the kernel
    // writes into the UMEM whose address it kept, Vesper reads the frames
    // and the ring in place and writes the consumer index back.
    for (entry, want) in [
        ("rx_37", expected_rx(37)),
        ("rx_3", expected_rx(3)),
        ("rx_copy_37", expected_rx(37)),
        ("refused", 70),
        ("view", RING_BYTES as u64 + sum_be16(&ring)),
    ] {
        let got = checked
            .jit(entry, &symbols)
            .unwrap_or_else(|e| panic!("{entry}: {e}"));
        assert_eq!(word(&got), want, "{entry}");
        assert_eq!(LIVE.load(Ordering::SeqCst), 0, "{entry}: all released");
    }
}

// ---------------------------------------------------------------------------
// Misuse: escapes, use after release, and anything that would move the
// memory C holds are refused.

const PRELUDE: &str = "
fn umem() -> Pinned[u8] {
    let mut b = ChunkSeq::with_capacity[u8](32768);
    b.resize(32768, 0);
    Pinned { data: b }
}
fn release(sink p: Pinned[u8]) {
    let Pinned { data } = p;
}
fn grow(inout s: ChunkSeq[u8]) {
    s.push(0);
}
";
fn refused(body: &str) -> ffi_lowering::FfiError {
    let source = format!("{PRELUDE}\n{body}");
    match ffi_lowering::check_source(&source, &imports(LINUX)) {
        Ok(_) => panic!("accepted:\n{body}"),
        Err(e) => e,
    }
}
fn codes(e: &ffi_lowering::FfiError) -> Vec<&str> {
    e.diagnostics.iter().map(|d| d.code.as_str()).collect()
}

#[test]
fn foreign_memory_never_moves_while_c_may_hold_it() {
    for body in [
        // Growing reallocates; shrinking and clearing change the length.
        "fn f(inout r: Ring) { r.data.push(1); }",
        "fn f(inout r: Ring) { r.data.truncate(0); }",
        "fn f(inout r: Ring) { r.data.clear(); }",
        "fn f(inout r: Ring) { r.data.reserve(64); }",
        "fn f(inout p: Pinned[u8]) { p.data.push(1); }",
        "fn f(inout p: Pinned[u8]) { p.data.resize(1, 0); }",
        // Lending the whole sequence `inout` lets the callee grow it.
        "fn f(inout r: Ring) { grow(inout r.data); }",
        "fn f(inout p: Pinned[u8]) { grow(inout p.data); }",
        // Replacing or exchanging it.
        "fn f(inout r: Ring) { r.data = ChunkSeq::new[u8](); }",
        "fn f(inout r: Ring, inout s: ChunkSeq[u8]) { swap(inout r.data, inout s); }",
        // Through a field path, a match binding, a `let` and a generic.
        "linear struct Pair { r: Ring, n: u64 }\nfn f(inout p: Pair) { p.r.data.push(1); }",
        "fn f(sink o: Option[Ring]) -> u64 eff [Ffi[Xsk]] { match o { Some(r) => { let mut r2 = r; r2.data.push(1); Xsk::xsk_ring_unmap(r2); 1 }, None => 0 } }",
        "fn g[T](inout p: Pinned[T], inout s: ChunkSeq[T]) { swap(inout p.data, inout s); }",
        // Through a handler's return clause and an operation clause.
        "effect Ask { fn ask() -> u64; }\nfn f(sink r: Ring) -> u64 eff [Ffi[Xsk]] { handle Ask in { r } with { return m => { let mut m2 = m; m2.data.push(1); Xsk::xsk_ring_unmap(m2); 1 }, ask(resume) => resume(0), } }",
        // A view held across a foreign call, which may write the memory:
        // bound, directly or through a function that calls into C, or lent
        // to such a function.
        "fn f(inout s: Socket, in r: Ring) -> u64 eff [Ffi[Xsk]] { let v = r.data.as_slice(); Xsk::xsk_kernel_rx(inout s, 1); v.len() }",
        "fn f(inout s: Socket, in p: Pinned[u8]) -> u64 eff [Ffi[Xsk]] { let v = p.data[0..64]; Xsk::xsk_kernel_rx(inout s, 1); v.len() }",
        "fn pump(inout s: Socket) -> u32 eff [Ffi[Xsk]] { Xsk::xsk_kernel_rx(inout s, 1) }\nfn f(inout s: Socket, in r: Ring) -> u64 eff [Ffi[Xsk]] { let v = r.data.as_slice(); let n = v.len(); pump(inout s); n }",
        "fn pump(inout s: Socket, in v: Slice[u8]) -> u64 eff [Ffi[Xsk]] { Xsk::xsk_kernel_rx(inout s, 1); v.len() }\nfn f(inout s: Socket, in r: Ring) -> u64 eff [Ffi[Xsk]] { pump(inout s, in r.data[0..8]) }",
        "fn pump(inout s: Socket, in v: Slice[u8]) -> u64 eff [Ffi[Xsk]] { Xsk::xsk_kernel_rx(inout s, 1); v.len() }\nfn f(inout s: Socket, in p: Pinned[u8]) -> u64 eff [Ffi[Xsk]] { pump(inout s, in p.data.as_slice()) }",
        "effect Give { fn give(sink r: Ring) -> u64; }\nfn f(sink r: Ring) -> u64 eff [Ffi[Xsk]] { handle Give in { Give::give(r) } with { return v => v, give(m, resume) => { let mut m2 = m; m2.data.push(1); Xsk::xsk_ring_unmap(m2); resume(1) }, } }",
    ] {
        let e = refused(body);
        assert_eq!(e.kind, FfiErrorKind::BufferMisuse, "{body}: {e}");
    }
    // Moving it out is refused, by the checker or by the buffer rule.
    for body in [
        "fn f(sink p: Pinned[u8]) -> ChunkSeq[u8] { let s = p.data; release(p); s }",
        "fn f(sink r: Ring) -> u64 eff [Ffi[Xsk]] { let s = r.data; Xsk::xsk_ring_unmap(r); s.len() }",
    ] {
        let e = refused(body);
        assert!(
            matches!(e.kind, FfiErrorKind::BufferMisuse | FfiErrorKind::Check),
            "{body}: {e}"
        );
    }
    // Reading, writing and lending views in place are accepted.
    let ok = format!(
        "{PRELUDE}
        fn sum(in s: Slice[u8]) -> u64 {{ s.sum_be16() }}
        fn zero(inout s: Slice[u8]) {{ s.fill(0); }}
        fn f(inout r: Ring, in p: Pinned[u8]) -> u64 eff [Fail[RuntimeError]] {{
            r.data.try_write_u32_le(4, 7);
            r.data.set(8, 1);
            zero(inout r.data[16..32]);
            let n = r.data.len();
            let a = sum(in p.data[0..64]);
            let b = sum(in r.data.as_slice());
            let v = p.data.as_slice();
            wrapping_add(wrapping_add(n, a), wrapping_add(b, v.len()))
        }}"
    );
    ffi_lowering::check_source(&ok, &imports(LINUX)).unwrap_or_else(|e| panic!("{e}"));
    // A view that ends before the foreign call, and element reads after
    // it, are accepted.
    let ok = format!(
        "{PRELUDE}
        fn f(inout s: Socket, in r: Ring) -> u64 eff [Ffi[Xsk]] {{
            let n = {{
                let v = r.data.as_slice();
                v.len()
            }};
            Xsk::xsk_kernel_rx(inout s, 1);
            let m = match r.data.try_read_u32_le(0) {{
                Some(p) => widen_u64(p),
                None => 0,
            }};
            wrapping_add(n, m)
        }}"
    );
    ffi_lowering::check_source(&ok, &imports(LINUX)).unwrap_or_else(|e| panic!("{e}"));
    // A bound view of a `let mut` owner or an `inout` parameter is
    // accepted; the owner is written and released after the view's block.
    let ok = format!(
        "{PRELUDE}
        fn g(inout r: Ring) -> u64 {{
            let v = r.data.as_slice();
            v.len()
        }}
        pub fn f() -> u64 {{
            let mut u = umem();
            let n = {{
                let v = u.data.as_slice();
                v.len()
            }};
            u.data.try_write_u32_le(0, 1);
            release(u);
            n
        }}"
    );
    ffi_lowering::check_source(&ok, &imports(LINUX)).unwrap_or_else(|e| panic!("{e}"));
}

#[test]
fn mappings_and_pinned_buffers_are_linear_and_views_cannot_escape() {
    let ring = "match Xsk::xsk_ring_map(in s, 192) { Some(r) => r, None => { Xsk::xsk_socket_free(s); release(u); return 0; } }";
    let socket = "let mut u = umem(); let s = match Xsk::xsk_socket_new(inout u, 2048) { Some(s) => s, None => { release(u); return 0; } };";
    let cases = [
        // Never released.
        (
            format!("pub fn f() -> u64 eff [Ffi[Xsk]] {{ {socket} let r = {ring}; Xsk::xsk_socket_free(s); release(u); 1 }}"),
            "E0302",
        ),
        ("pub fn f() -> u64 { let u = umem(); 1 }".to_owned(), "E0302"),
        // Used after its release.
        (
            format!("pub fn f() -> u64 eff [Ffi[Xsk]] {{ {socket} let r = {ring}; Xsk::xsk_ring_unmap(r); let n = r.data.len(); Xsk::xsk_socket_free(s); release(u); n }}"),
            "E0303",
        ),
        (
            "pub fn f() -> u64 { let u = umem(); release(u); u.data.len() }".to_owned(),
            "E0303",
        ),
        // Released twice.
        (
            format!("pub fn f() -> u64 eff [Ffi[Xsk]] {{ {socket} let r = {ring}; Xsk::xsk_ring_unmap(r); Xsk::xsk_ring_unmap(r); Xsk::xsk_socket_free(s); release(u); 1 }}"),
            "E0303",
        ),
        // A bound view keeps its mapping or buffer from being released.
        (
            format!("pub fn f() -> u64 eff [Ffi[Xsk]] {{ {socket} let r = {ring}; let v = r.data.as_slice(); Xsk::xsk_ring_unmap(r); Xsk::xsk_socket_free(s); release(u); v.len() }}"),
            "E1531",
        ),
        (
            "pub fn f() -> u64 { let u = umem(); let v = u.data.as_slice(); release(u); v.len() }"
                .to_owned(),
            "E1531",
        ),
        // A view is never returned or stored.
        ("fn f(in r: Ring) -> Slice[u8] { r.data.as_slice() }".to_owned(), "E1520"),
        (
            "struct Holder { v: Slice[u8] }\nfn f(in r: Ring) -> u64 { let h = Holder { v: r.data.as_slice() }; 0 }"
                .to_owned(),
            "E1520",
        ),
        // A bound view of a `let mut` mapping freezes it while in scope:
        // no release and no write through it.
        (
            format!("pub fn f() -> u64 eff [Ffi[Xsk]] {{ {socket} let mut r = {ring}; let v = r.data.as_slice(); let n = v.len(); Xsk::xsk_ring_unmap(r); Xsk::xsk_socket_free(s); release(u); n }}"),
            "E1531",
        ),
        (
            "pub fn f() -> u64 { let mut u = umem(); let v = u.data.as_slice(); u.data.try_write_u32_le(0, 1); let n = v.len(); release(u); n }"
                .to_owned(),
            "E1531",
        ),
        // A mutable binding of a view stays refused.
        (
            "pub fn f() -> u64 { let u = umem(); let mut v = u.data.as_slice(); let n = v.len(); release(u); n }"
                .to_owned(),
            "E1521",
        ),
        // A live mapping may not cross an abortable effect.
        (
            format!("copy struct E {{ c: u32 }}\npub fn f() -> u64 eff [Ffi[Xsk], Fail[E]] {{ {socket} let r = {ring}; Fail::raise(E {{ c: 1 }}); Xsk::xsk_ring_unmap(r); Xsk::xsk_socket_free(s); release(u); 1 }}"),
            "E0402",
        ),
    ];
    for (body, code) in &cases {
        let e = refused(body);
        assert_eq!(e.kind, FfiErrorKind::Check, "{body}: {e}");
        assert!(codes(&e).contains(code), "{body}: want {code}, got {e}");
    }
    // Only the library creates and releases a mapping.
    for body in [
        "fn f() -> Ring { Ring { data: ChunkSeq::new[u8]() } }",
        "fn f(sink r: Ring) -> ChunkSeq[u8] { let Ring { data } = r; data }",
    ] {
        let e = refused(body);
        assert_eq!(e.kind, FfiErrorKind::HandleMisuse, "{body}: {e}");
    }
}

#[test]
fn vsys_foreign_memory_declarations_are_validated() {
    let base = |body: &str| {
        format!(
            "sys library L (target = \"x86_64-unknown-linux-gnu\", abi = \"sysv64\", link = \"static\", name = \"l\", artifact = \"blake3:{ZERO}\") {{ {body} }}"
        )
    };
    let parse = |body: &str| vesper_ffi::sys::parse(&base(body), Default::default());
    let unmap = "fn m_unmap(sink m: M) -> ForeignVal[i32];";
    // A mapping is linear, of an in-place element type, and released.
    let e = parse(&format!("affine extern mapping M[u8]; {unmap}")).unwrap_err();
    assert!(e.message.contains("linear extern mapping"), "{e}");
    let e = parse(&format!("linear extern mapping M[u16]; {unmap}")).unwrap_err();
    assert!(e.message.contains("u8, u32"), "{e}");
    let e = parse("linear extern mapping M[u8]; fn m_map(n: u64) -> ForeignVal[Option[M[n]]];")
        .unwrap_err();
    assert!(e.message.contains("no finalizer"), "{e}");
    // A mapping result names its length parameter, which is an unsigned
    // by-value integer; only a mapping names one.
    for (result, wanted) in [
        ("ForeignVal[Option[M]]", "Option[M[len]]"),
        ("ForeignVal[M]", "Option[M[len]]"),
        ("ForeignVal[NonNull[M]]", "Option[M[len]]"),
        ("ForeignVal[Option[M[k]]]", "by-value u32, u64 or usize"),
        ("ForeignVal[Option[M[s]]]", "by-value u32, u64 or usize"),
    ] {
        let e = parse(&format!(
            "linear extern mapping M[u8]; fn m_map(n: u64, s: i64) -> {result}; {unmap}"
        ))
        .unwrap_err();
        assert!(e.message.contains(wanted), "{result}: {e}");
    }
    let e = parse("affine extern type H; fn h(n: u64) -> ForeignVal[Option[H[n]]];").unwrap_err();
    assert!(e.message.contains("only a mapping"), "{e}");
    // Pinned buffers are lent `in` or `inout`, of in-place elements.
    assert!(parse("fn f(out b: Pinned[u8]) -> ForeignVal[Unit];").is_err());
    assert!(parse("fn f(in b: Pinned[u16]) -> ForeignVal[Unit];").is_err());
    assert!(parse("affine extern type Pinned; fn f() -> ForeignVal[Unit];").is_err());
    // C receives a mapping and a pinned buffer as `(data, len)`.
    let lib = parse(&format!(
        "linear extern mapping M[u32]; fn m_map(n: u64) -> ForeignVal[Option[M[n]]] errno; {unmap} fn use_both(in m: M, inout b: Pinned[u64]) -> ForeignVal[u32];"
    ))
    .unwrap();
    let f = lib.function("use_both").unwrap();
    assert_eq!(f.abi().parameters.len(), 4);
    assert!(f.uses_foreign_memory());
    assert_eq!(lib.function("m_map").unwrap().mapping_length(), Some(0));
    assert_eq!(
        lib.extern_type("M").unwrap().mapping(),
        Some(vesper_ffi::sys::Scalar::U32)
    );
    // Libraries without mappings or pinned buffers keep their identities:
    // the new forms are appended to the canonical bytes only when used.
    let plain = parse("linear extern type H; fn h_new() -> ForeignVal[Option[H]]; fn h_free(sink h: H) -> ForeignVal[Unit];").unwrap();
    assert!(!String::from_utf8_lossy(plain.canonical_bytes()).contains("mapping"));
}

#[test]
fn the_evaluator_refuses_foreign_memory() {
    let checked = checked(&fixture("xsk.ves"), LINUX);
    let e = checked
        .evaluate("rx_3", |_| Err("no host".into()))
        .unwrap_err();
    assert!(e.to_string().contains("foreign memory"), "{e}");
}

// ---------------------------------------------------------------------------
// Objects: the same program linked with a C library whose ring is a real
// anonymous `mmap`, released by `munmap(data, len)`.

fn run(command: &mut std::process::Command) -> std::process::Output {
    let output = command.output().expect("run native tool");
    assert!(
        output.status.success(),
        "command {command:?}\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

#[test]
fn objects_receive_through_a_c_library_and_munmap() {
    use std::process::Command;
    if !(cfg!(target_os = "linux") && cfg!(target_arch = "x86_64")) {
        eprintln!("object fixtures need a Linux x86-64 host; object evidence unavailable");
        return;
    }
    for tool in ["cc", "ar"] {
        if Command::new(tool).arg("--version").output().is_err() {
            eprintln!("object fixtures unavailable: missing {tool}");
            return;
        }
    }
    let dir = std::env::temp_dir().join(format!("vesper-foreign-memory-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let lib_object = dir.join("xsk.o");
    run(Command::new("cc")
        .args([
            "-std=c11",
            "-D_DEFAULT_SOURCE",
            "-Wall",
            "-Wextra",
            "-Werror",
            "-O2",
            "-fPIC",
            "-c",
        ])
        .arg(fixtures().join("xsk.c"))
        .arg("-o")
        .arg(&lib_object));
    let archive = dir.join("libvesper_xsk.a");
    run(Command::new("ar").arg("crs").arg(&archive).arg(&lib_object));
    let digest = blake3::hash(&fs::read(&archive).unwrap()).to_hex();
    let text = fixture("xsk.vsys")
        .replace("@TARGET@", LINUX.0)
        .replace("@ABI@", LINUX.1)
        .replace("@DIGEST@", digest.as_str());
    let mut imports = Imports::new();
    imports.add(&text, Handler::Direct).unwrap();
    let checked = ffi_lowering::check_source(&fixture("xsk.ves"), &imports).unwrap();
    let ring: Vec<u8> = (0..RING_BYTES).map(ring_pattern).collect();
    for (entry, want) in [
        ("rx_37", expected_rx(37)),
        ("refused", 70),
        ("view", RING_BYTES as u64 + sum_be16(&ring)),
    ] {
        let object = checked
            .object(entry, Some(LINUX.0))
            .unwrap_or_else(|e| panic!("{entry}: {e}"));
        let path = dir.join(format!("{entry}.o"));
        fs::write(&path, &object.bytes).unwrap();
        let exe = dir.join(entry);
        run(Command::new("cc")
            .arg(format!("-DENTRY={entry}"))
            .arg(fixtures().join("host.c"))
            .arg(&path)
            .arg(&archive)
            .arg("-o")
            .arg(&exe));
        let out = run(&mut Command::new(&exe));
        let stdout = String::from_utf8_lossy(&out.stdout);
        let fields: Vec<u64> = stdout
            .split_whitespace()
            .map(|f| f.parse().unwrap())
            .collect();
        assert_eq!(fields, vec![want, 0], "{entry}: value and live count");
    }
    let _ = fs::remove_dir_all(&dir);
}

/// In place against the copy workaround, 200,000 packets each, ABBA:
/// `cargo test --release --test foreign_memory measure -- --ignored --nocapture`.
#[test]
#[ignore]
fn measure_in_place_against_copies() {
    let Some(target) = host_target() else {
        return;
    };
    let _lock = JIT.lock().unwrap();
    let checked = checked(&fixture("xsk.ves"), target);
    let symbols = symbols();
    let want = expected_rx(200_000);
    let time = |entry: &str| {
        let start = std::time::Instant::now();
        let got = checked.jit(entry, &symbols).unwrap();
        let ns = start.elapsed().as_nanos() as f64 / 200_000.0;
        assert_eq!(word(&got), want, "{entry}");
        ns
    };
    let mut in_place = vec![];
    let mut copies = vec![];
    for round in 0..3 {
        // A B B A, then B A A B, then A B B A.
        let order = if round % 2 == 0 {
            [0, 1, 1, 0]
        } else {
            [1, 0, 0, 1]
        };
        for which in order {
            if which == 0 {
                in_place.push(time("rx_200000"));
            } else {
                copies.push(time("rx_copy_200000"));
            }
        }
    }
    let median = |v: &mut Vec<f64>| {
        v.sort_by(f64::total_cmp);
        v[v.len() / 2]
    };
    let (a, b) = (median(&mut in_place), median(&mut copies));
    eprintln!(
        "ns/packet (JIT, includes compile and the stand-in kernel): in place {a:.1}, copies {b:.1}, copies/in place {:.2}x",
        b / a
    );
}

#[test]
fn a_derived_pinned_view_ends_at_its_outer_block() {
    let view = "let all = p.data.as_slice(); let tail = all[1..2]; tail.len()";
    let header = "fn f(inout s: Socket, in p: Pinned[u8]) -> u64 eff [Ffi[Xsk]]";
    let call = "Xsk::xsk_kernel_rx(inout s, 1);";
    let ok = format!("{PRELUDE}\n{header} {{ let n = {{ {view} }}; {call} n }}");
    ffi_lowering::check_source(&ok, &imports(LINUX)).unwrap_or_else(|e| panic!("{e}"));

    let body = format!(
        "{header} {{ let all = p.data.as_slice(); let n = {{ let tail = all[1..2]; tail.len() }}; {call} n }}"
    );
    assert_eq!(refused(&body).kind, FfiErrorKind::BufferMisuse);
}
