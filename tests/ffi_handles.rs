//! Opaque Bridge handles for `.vsys` imports (§16.3, port wave P3b lane 3,
//! #1065): `linear`/`affine extern type` handles that imports create, lend
//! `in`/`inout` and consume `sink`, and byte slices lent to C in place.
//!
//! The worked example is a stateful AEAD sealer (tests/fixtures/ffi_handles):
//! the JIT binds its imports to the Rust functions below on every host; the
//! evaluator runs the same module with a Rust stand-in; on Linux x86-64 the
//! object is linked with the C library `aead.c` and a C host. Each counts
//! finalizer calls, which must equal creations on every path, including the
//! `Fail` exit of a tampered packet.
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
};
use vesper::ffi_lowering::{self, FfiErrorKind, Handler, Imports};
use vesper::runtime::Value;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ffi_handles")
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
fn vsys(target: (&str, &str), digest: &str) -> String {
    fixture("aead.vsys")
        .replace("@TARGET@", target.0)
        .replace("@ABI@", target.1)
        .replace("@DIGEST@", digest)
}
const ZERO: &str = "0000000000000000000000000000000000000000000000000000000000000000";
fn checked(source: &str, target: (&str, &str)) -> ffi_lowering::ForeignChecked {
    let mut imports = Imports::new();
    imports.add(&vsys(target, ZERO), Handler::Direct).unwrap();
    ffi_lowering::check_source(source, &imports).unwrap_or_else(|e| panic!("{e}"))
}
fn word(v: &Value) -> u64 {
    match v {
        Value::Int(n) => *n as u64,
        Value::UInt(n, _) => *n,
        other => panic!("not a word: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// The Rust side of the handle: a toy stateful AEAD. Creating a sealer expands
// the key into a 256-byte schedule (RC4's key scheduling, 4 passes); sealing
// XORs the payload with a keystream drawn from a copy of the schedule and
// writes a 16-byte tag over the ciphertext. It is a stand-in with the cost
// shape of a real AEAD, not a cipher.

const TAG: usize = 16;
struct Schedule {
    s: [u8; 256],
    key: u64,
}
fn expand(key: u64) -> Schedule {
    let mut s = [0u8; 256];
    for (i, b) in s.iter_mut().enumerate() {
        *b = i as u8;
    }
    let k = key.to_le_bytes();
    let mut j: u8 = 0;
    for _ in 0..4 {
        for i in 0..256 {
            j = j.wrapping_add(s[i]).wrapping_add(k[i % 8]);
            s.swap(i, j as usize);
        }
    }
    Schedule { s, key }
}
fn keystream_xor(sch: &Schedule, payload: &mut [u8]) {
    let mut s = sch.s;
    let (mut i, mut j) = (0u8, 0u8);
    for b in payload {
        i = i.wrapping_add(1);
        j = j.wrapping_add(s[i as usize]);
        s.swap(i as usize, j as usize);
        *b ^= s[s[i as usize].wrapping_add(s[j as usize]) as usize];
    }
}
fn tag(sch: &Schedule, ciphertext: &[u8]) -> [u8; TAG] {
    let mut a = 0xcbf2_9ce4_8422_2325u64 ^ sch.key;
    let mut b = 0x9e37_79b9_7f4a_7c15u64;
    for &c in ciphertext {
        a = (a ^ u64::from(c)).wrapping_mul(0x0100_0000_01b3);
        b = b.rotate_left(5) ^ a;
    }
    let mut t = [0u8; TAG];
    t[..8].copy_from_slice(&a.to_le_bytes());
    t[8..].copy_from_slice(&b.to_le_bytes());
    t
}
fn seal(sch: &Schedule, packet: &mut [u8]) -> u32 {
    if packet.len() < TAG {
        return 2;
    }
    let (payload, t) = packet.split_at_mut(packet.len() - TAG);
    keystream_xor(sch, payload);
    t.copy_from_slice(&tag(sch, payload));
    0
}
fn open(sch: &Schedule, packet: &mut [u8]) -> u32 {
    if packet.len() < TAG {
        return 2;
    }
    let (payload, t) = packet.split_at_mut(packet.len() - TAG);
    if tag(sch, payload) != *t {
        return 1;
    }
    keystream_xor(sch, payload);
    0
}

static CREATED: AtomicU64 = AtomicU64::new(0);
static FREED: AtomicU64 = AtomicU64::new(0);
/// The JIT scenarios share the counters, so they run one at a time.
static JIT: Mutex<()> = Mutex::new(());
static PROBE: u64 = 0x5eed;

/// # Safety
/// `ptr` is valid for `len` bytes (the `.vsys` slice contract).
unsafe fn bytes<'a>(ptr: *mut u8, len: usize) -> &'a mut [u8] {
    if len == 0 {
        return &mut [];
    }
    // SAFETY: the caller lends `len` bytes at `ptr` for the call.
    unsafe { std::slice::from_raw_parts_mut(ptr, len) }
}
extern "C" fn aead_sealer_new(key: u64) -> *mut Schedule {
    if key == 0 {
        return std::ptr::null_mut();
    }
    CREATED.fetch_add(1, Ordering::SeqCst);
    Box::into_raw(Box::new(expand(key)))
}
extern "C" fn aead_sealer_must(key: u64) -> *mut Schedule {
    aead_sealer_new(key)
}
extern "C" fn aead_seal(s: *mut Schedule, ptr: *mut u8, len: usize) -> u32 {
    // SAFETY: `s` came from aead_sealer_new and is not yet freed; the
    // packet is lent for the call.
    unsafe { seal(&*s, bytes(ptr, len)) }
}
extern "C" fn aead_open(s: *const Schedule, ptr: *mut u8, len: usize) -> u32 {
    // SAFETY: as in aead_seal.
    unsafe { open(&*s, bytes(ptr, len)) }
}
extern "C" fn aead_sealer_free(s: *mut Schedule) {
    FREED.fetch_add(1, Ordering::SeqCst);
    // SAFETY: the sink import consumes the handle exactly once.
    drop(unsafe { Box::from_raw(s) });
}
extern "C" fn aead_seal_once(key: u64, ptr: *mut u8, len: usize) -> u32 {
    // A CSlice is never NULL, even when empty.
    if ptr.is_null() {
        return 77;
    }
    // SAFETY: the packet is lent for the call.
    unsafe { seal(&expand(key), bytes(ptr, len)) }
}
extern "C" fn aead_probe() -> *const u64 {
    &PROBE
}
extern "C" fn aead_probe_live(p: *const u64) -> u64 {
    // SAFETY: the probe points at the static PROBE.
    assert_eq!(unsafe { *p }, PROBE);
    CREATED.load(Ordering::SeqCst) - FREED.load(Ordering::SeqCst)
}
fn symbols() -> Vec<(&'static str, *const u8)> {
    vec![
        ("aead_sealer_new", aead_sealer_new as *const u8),
        ("aead_sealer_must", aead_sealer_must as *const u8),
        ("aead_seal", aead_seal as *const u8),
        ("aead_open", aead_open as *const u8),
        ("aead_sealer_free", aead_sealer_free as *const u8),
        ("aead_seal_once", aead_seal_once as *const u8),
        ("aead_probe", aead_probe as *const u8),
        ("aead_probe_live", aead_probe_live as *const u8),
    ]
}

/// The expected value of every entry, from the Rust model of the scenario.
fn expected() -> BTreeMap<&'static str, u64> {
    let checksum = |seed: u8| -> u64 {
        // sum_be16 of the 1200 payload bytes of `seed`.
        600 * ((u64::from(seed) << 8) | u64::from(seed))
    };
    let sealed_twenty_thousand = {
        let sch = expand(21);
        let mut p = vec![2u8; 1216];
        for _ in 0..20000 {
            seal(&sch, &mut p);
        }
        p.chunks(2)
            .map(|c| (u64::from(c[0]) << 8) | u64::from(c[1]))
            .sum::<u64>()
    };
    BTreeMap::from([
        ("roundtrip", checksum(3)),
        // The tag check fails with code 1.
        ("tampered", 1001),
        // early(0) = 0, early(11, stop) = 1, early(12) = 2, must(0) = Err
        // raw 0 -> 70, must(13) = 500, probe sees no live sealer.
        ("paths", 10 + 200 + 70 + 500),
        ("churn", 0),
        ("seal_many", sealed_twenty_thousand),
        ("seal_many_stateless", sealed_twenty_thousand),
        ("empty", 2),
    ])
}

#[test]
fn jit_drives_a_stateful_rust_aead_through_linear_handles() {
    let Some(target) = host_target() else {
        eprintln!("no .vsys target for this host; JIT evidence unavailable");
        return;
    };
    let _lock = JIT.lock().unwrap();
    let checked = checked(&fixture("aead.ves"), target);
    let symbols = symbols();
    for (entry, want) in expected() {
        let created = CREATED.load(Ordering::SeqCst);
        let freed = FREED.load(Ordering::SeqCst);
        let got = checked
            .jit(entry, &symbols)
            .unwrap_or_else(|e| panic!("{entry}: {e}"));
        assert_eq!(word(&got), want, "{entry}");
        let made = CREATED.load(Ordering::SeqCst) - created;
        let finalized = FREED.load(Ordering::SeqCst) - freed;
        assert_eq!(made, finalized, "{entry}: every sealer is finalized once");
        let min = match entry {
            "churn" => 1000,
            "paths" => 3,
            "seal_many_stateless" | "empty" => 0,
            _ => 1,
        };
        assert!(made >= min, "{entry}: {made} sealers");
    }
}

#[test]
fn jit_refuses_unbound_foreign_symbols() {
    let Some(target) = host_target() else {
        return;
    };
    let checked = checked(&fixture("aead.ves"), target);
    let mut partial = symbols();
    partial.retain(|(n, _)| *n != "aead_sealer_free");
    let e = checked.jit("roundtrip", &partial).unwrap_err();
    assert_eq!(e.kind, FfiErrorKind::UnknownForeignFunction, "{e}");
    assert!(e.message.contains("aead_sealer_free"), "{e}");
}

/// The evaluator's stand-in host: handles are indices into `sealers`.
#[derive(Default)]
struct EvalHost {
    sealers: Vec<Option<Schedule>>,
    created: u64,
    freed: u64,
}
impl EvalHost {
    fn call(&mut self, call: &mut ffi_lowering::EvalCall<'_, '_>) -> Result<Value, String> {
        let args = call.arguments().to_vec();
        let sealer = |v: &Value| ffi_lowering::handle_word(v).ok_or("not a handle");
        let some = |v: Value| Value::Data {
            ty: "Option".into(),
            constructor: "Some".into(),
            fields: BTreeMap::from([("0".to_string(), v)]),
            resource_identity: None,
        };
        let none = || Value::Data {
            ty: "Option".into(),
            constructor: "None".into(),
            fields: BTreeMap::new(),
            resource_identity: None,
        };
        let result = |ok: bool, v: Value| Value::Data {
            ty: "Result".into(),
            constructor: if ok { "Ok" } else { "Err" }.into(),
            fields: BTreeMap::from([("0".to_string(), v)]),
            resource_identity: None,
        };
        let u32_value = |n: u32| Value::UInt(u64::from(n), 32);
        Ok(match call.symbol.as_str() {
            "aead_sealer_new" | "aead_sealer_must" => {
                let key = word(&args[0]);
                let handle = if key == 0 {
                    None
                } else {
                    self.created += 1;
                    self.sealers.push(Some(expand(key)));
                    Some(ffi_lowering::handle_value(
                        "Sealer",
                        self.sealers.len() as u64,
                    ))
                };
                match (call.symbol.as_str(), handle) {
                    ("aead_sealer_new", Some(h)) => some(h),
                    ("aead_sealer_new", None) => none(),
                    (_, Some(h)) => result(true, h),
                    (_, None) => result(
                        false,
                        Value::Data {
                            ty: "ForeignInvalid".into(),
                            constructor: "ForeignInvalid".into(),
                            fields: BTreeMap::from([("raw".to_string(), Value::UInt(0, 64))]),
                            resource_identity: None,
                        },
                    ),
                }
            }
            "aead_seal" | "aead_open" | "aead_seal_once" => {
                let mut packet = ffi_lowering::slice_bytes(&args[1]).ok_or("not a byte slice")?;
                let code = if call.symbol == "aead_seal_once" {
                    seal(&expand(word(&args[0])), &mut packet)
                } else {
                    let i = sealer(&args[0])? as usize - 1;
                    let sch = self.sealers[i].as_ref().ok_or("use after finalize")?;
                    if call.symbol == "aead_seal" {
                        seal(sch, &mut packet)
                    } else {
                        open(sch, &mut packet)
                    }
                };
                ffi_lowering::write_slice_bytes(&args[1], &packet)?;
                u32_value(code)
            }
            "aead_sealer_free" => {
                let i = sealer(&args[0])? as usize - 1;
                self.sealers[i].take().ok_or("double finalize")?;
                self.freed += 1;
                Value::Tuple(vec![])
            }
            "aead_probe" => some(ffi_lowering::handle_value("Probe", 1)),
            "aead_probe_live" => Value::UInt(self.created - self.freed, 64),
            other => return Err(format!("unknown symbol {other}")),
        })
    }
}

#[test]
fn evaluator_runs_handles_with_a_host_stand_in() {
    // The evaluator never calls C, so any target works.
    let checked = checked(&fixture("aead.ves"), ("x86_64-unknown-linux-gnu", "sysv64"));
    for (entry, want) in expected() {
        if entry.starts_with("seal_many") {
            continue; // 20,000 seals in the evaluator are slow.
        }
        let mut host = EvalHost::default();
        let got = checked
            .evaluate(entry, |call| host.call(call))
            .unwrap_or_else(|e| panic!("{entry}: {e}"));
        assert_eq!(word(&got), want, "{entry}");
        assert_eq!(
            host.created, host.freed,
            "{entry}: every sealer is finalized"
        );
        assert!(host.sealers.iter().all(Option::is_none), "{entry}");
    }
}

// ---------------------------------------------------------------------------
// Misuse: each is a typed diagnostic.

fn misuse(name: &str) -> ffi_lowering::FfiError {
    let mut imports = Imports::new();
    imports
        .add(
            &vsys(("x86_64-unknown-linux-gnu", "sysv64"), ZERO),
            Handler::Direct,
        )
        .unwrap();
    let source = fixture(&format!("misuse/{name}.ves"));
    match ffi_lowering::check_source(&source, &imports) {
        Ok(_) => panic!("{name}: accepted"),
        Err(e) => e,
    }
}
fn codes(e: &ffi_lowering::FfiError) -> Vec<&str> {
    e.diagnostics.iter().map(|d| d.code.as_str()).collect()
}

#[test]
fn handle_misuse_is_a_typed_diagnostic() {
    // Checker diagnostics: (fixture, code).
    for (name, code) in [
        ("leak", "E0302"),
        ("leak_on_branch", "E0309"),
        ("double_finalize", "E0303"),
        ("use_after_finalize", "E0303"),
        ("copy", "E0303"),
        ("compare_linear", "E0302"),
        ("compare_affine", "E0200"),
        ("compare_option_affine", "E0200"),
        ("compare_wrapped_affine", "E0200"),
        ("live_across_fail", "E0402"),
    ] {
        let e = misuse(name);
        assert_eq!(e.kind, FfiErrorKind::Check, "{name}: {e}");
        assert!(codes(&e).contains(&code), "{name}: want {code}, got {e}");
    }
    // Frontend diagnostics.
    for name in ["construct", "destructure", "construct_positional"] {
        let e = misuse(name);
        assert_eq!(e.kind, FfiErrorKind::HandleMisuse, "{name}: {e}");
    }
}

#[test]
fn vsys_handle_declarations_are_validated() {
    let base = |body: &str| {
        format!(
            "sys library L (target = \"x86_64-unknown-linux-gnu\", abi = \"sysv64\", link = \"static\", name = \"l\", artifact = \"blake3:{ZERO}\") {{ {body} }}"
        )
    };
    let parse = |body: &str| vesper_ffi::sys::parse(&base(body), Default::default());
    // A linear type needs a finalizer.
    let e = parse("linear extern type H; fn h_new() -> ForeignVal[Option[H]];").unwrap_err();
    assert!(e.message.contains("no finalizer"), "{e}");
    // Handle parameters need a mode; `out` and `nocapture` are not modes.
    let e = parse("affine extern type H; fn f(h: H) -> ForeignVal[Unit];").unwrap_err();
    assert!(e.message.contains("needs a mode"), "{e}");
    assert!(parse("affine extern type H; fn f(out h: H) -> ForeignVal[Unit];").is_err());
    assert!(parse("affine extern type H; fn f(nocapture in h: H) -> ForeignVal[Unit];").is_err());
    // Capturing pointers stay rejected.
    let e = parse("fn f(in p: u32) -> ForeignVal[Unit];").unwrap_err();
    assert!(e.message.contains("nocapture"), "{e}");
    let e = parse("fn f(in p: CSlice[u8]) -> ForeignVal[Unit];").unwrap_err();
    assert!(e.message.contains("nocapture"), "{e}");
    // `sink` needs an extern type; Option results need a handle.
    assert!(parse("fn f(sink x: u64) -> ForeignVal[Unit];").is_err());
    assert!(parse("fn f() -> ForeignVal[Option[u64]];").is_err());
    // Names are unique across enums, wire structs and extern types.
    assert!(
        parse("affine extern type H; enum H: u8 { A = 1 } fn f() -> ForeignVal[Unit];").is_err()
    );
    assert!(
        parse("affine extern type H; affine extern type H; fn f() -> ForeignVal[Unit];").is_err()
    );
    // A valid library: the handle ABI is one pointer word.
    let d = parse(
        "linear extern type H; affine extern type V;
         fn h_new(seed: u64) -> ForeignVal[Option[H]];
         fn h_use(inout h: H, in v: V, nocapture inout b: CSlice[u8]) -> ForeignVal[u32];
         fn h_free(sink h: H) -> ForeignVal[Unit];",
    )
    .unwrap();
    use vesper_ffi::sys::{AbiType, Linearity, Scalar};
    assert_eq!(d.extern_types().len(), 2);
    assert_eq!(d.extern_type("H").unwrap().linearity(), Linearity::Linear);
    assert_eq!(d.extern_type("V").unwrap().linearity(), Linearity::Affine);
    let abi = d.function("h_use").unwrap().abi();
    assert_eq!(
        abi.parameters,
        vec![
            AbiType::Pointer,
            AbiType::Pointer,
            AbiType::Pointer,
            AbiType::Integer(Scalar::Usize)
        ]
    );
    assert_eq!(
        d.function("h_new").unwrap().abi().result,
        Some(AbiType::Pointer)
    );
    // Extern types are part of a library's canonical identity (libraries
    // without them keep their earlier bytes: the section is appended only
    // when present).
    let plain = parse("fn f() -> ForeignVal[Unit];").unwrap();
    let with = parse("affine extern type H; fn f() -> ForeignVal[Unit];").unwrap();
    assert_ne!(plain.identity(), with.identity());
}

#[test]
fn handle_types_belong_to_one_library() {
    // Two libraries naming the same opaque type would let one library's
    // handle reach the other's imports, so the import set is refused.
    let lib = |name: &str| {
        format!(
            "sys library {name} (target = \"x86_64-unknown-linux-gnu\", abi = \"sysv64\", link = \"static\", name = \"{l}\", artifact = \"blake3:{ZERO}\") {{
                affine extern type H;
                fn {l}_h() -> ForeignVal[Option[H]];
                fn {l}_use(in h: H) -> ForeignVal[u64];
            }}",
            l = name.to_lowercase()
        )
    };
    let mut imports = Imports::new();
    imports.add(&lib("A"), Handler::Direct).unwrap();
    imports.add(&lib("B"), Handler::Direct).unwrap();
    let e = ffi_lowering::check_source("fn f() -> u64 { 0 }", &imports).unwrap_err();
    assert!(e.message.contains("'H'"), "{e}");
}

#[test]
fn source_slices_only_for_packed_elements() {
    let vsys = format!(
        "sys library W (target = \"x86_64-unknown-linux-gnu\", abi = \"sysv64\", link = \"static\", name = \"w\", artifact = \"blake3:{ZERO}\") {{
            fn w_sum(nocapture in s: CSlice[u16]) -> ForeignVal[u64];
            fn w_len(nocapture in s: CSlice[u8]) -> ForeignVal[u64];
        }}"
    );
    let mut imports = Imports::new();
    imports.add(&vsys, Handler::Direct).unwrap();
    let ok = "fn f(in s: Slice[u8]) -> u64 eff [Ffi[W]] { W::w_len(in s) }";
    let checked = ffi_lowering::check_source(ok, &imports).unwrap();
    assert_eq!(checked.calls()[0].slice_lengths, vec![usize::MAX]);
    // u16 elements are 8-byte words in native slices, not C's 2 bytes.
    let bad = "fn f(in s: Slice[u16]) -> u64 eff [Ffi[W]] { W::w_sum(in s) }";
    let e = ffi_lowering::check_source(bad, &imports).unwrap_err();
    assert_eq!(e.kind, FfiErrorKind::SliceArgument, "{e}");
}

// ---------------------------------------------------------------------------
// Objects: linked with a C library and a C host (Linux x86-64).

fn run(command: &mut Command) -> Output {
    let output = command.output().expect("run native tool");
    assert!(
        output.status.success(),
        "command {command:?}\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "vesper-ffi-handles-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn objects_link_handles_with_a_c_library() {
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
    let temp = Temp::new();
    let lib_object = temp.0.join("aead.o");
    run(Command::new("cc")
        .args([
            "-std=c11", "-Wall", "-Wextra", "-Werror", "-O2", "-fPIC", "-c",
        ])
        .arg(fixtures().join("aead.c"))
        .arg("-o")
        .arg(&lib_object));
    let archive = temp.0.join("libvesper_aead.a");
    run(Command::new("ar").arg("crs").arg(&archive).arg(&lib_object));
    let digest = blake3::hash(&fs::read(&archive).unwrap()).to_hex();
    let mut imports = Imports::new();
    imports
        .add(
            &vsys(("x86_64-unknown-linux-gnu", "sysv64"), digest.as_str()),
            Handler::Direct,
        )
        .unwrap();
    let checked = ffi_lowering::check_source(&fixture("aead.ves"), &imports).unwrap();
    let mut objects = vec![];
    for entry in [
        "roundtrip",
        "tampered",
        "paths",
        "churn",
        "seal_many",
        "empty",
    ] {
        let object = checked
            .object(entry, Some("x86_64-unknown-linux-gnu"))
            .unwrap_or_else(|e| panic!("{entry}: {e}"));
        object
            .verify_artifact("Aead", &fs::read(&archive).unwrap())
            .unwrap();
        assert_eq!(object.link.len(), 1);
        let path = temp.0.join(format!("{entry}.o"));
        fs::write(&path, &object.bytes).unwrap();
        objects.push(path);
    }
    // Each object exports its entry and its own copies of the helpers it
    // reaches, so link one object per program.
    let want = expected();
    for (entry, object) in [
        "roundtrip",
        "tampered",
        "paths",
        "churn",
        "seal_many",
        "empty",
    ]
    .iter()
    .zip(&objects)
    {
        let exe = temp.0.join(entry);
        run(Command::new("cc")
            .arg(format!("-DENTRY={entry}"))
            .arg(fixtures().join("host.c"))
            .arg(object)
            .arg(&archive)
            .arg("-o")
            .arg(&exe));
        let out = run(&mut Command::new(&exe));
        let stdout = String::from_utf8_lossy(&out.stdout);
        let fields: Vec<u64> = stdout
            .split_whitespace()
            .map(|f| f.parse().unwrap())
            .collect();
        let [value, created, freed] = fields[..] else {
            panic!("{entry}: unexpected output {stdout}");
        };
        assert_eq!(value, want[entry], "{entry}");
        assert_eq!(created, freed, "{entry}: every sealer is finalized once");
        assert!(created >= 1 || *entry == "empty", "{entry}");
    }
}

/// A library built as `vesper build --lib` builds it (the port path:
/// `// @export vsys direct rust=<shim>`). Handles stay inside Vesper.
fn export(body: &str) -> Result<String, String> {
    use vesper::native::export::{BindingOptions, LibraryOptions, compile_source, rust_binding};
    let mut src = String::from("// @export vsys direct rust=crate::shim\n");
    for line in fixture("aead.vsys").lines() {
        src.push_str("// | ");
        src.push_str(&line.replace("@DIGEST@", ZERO));
        src.push('\n');
    }
    src.push_str(body);
    let options = LibraryOptions {
        library: "t",
        target: Some("x86_64-unknown-linux-gnu"),
        optimize: true,
    };
    compile_source("t.ves", &src, &options)
        .map(|l| rust_binding(&l.interface, &BindingOptions::default()))
        .map_err(|e| e.to_string())
}

#[test]
fn export_libraries_keep_handles_inside() {
    // Internal handles and in-place slices compile into a port library.
    let binding = export(
        "pub fn seal_packet(key: u64, inout p: Slice[u8]) -> u32 eff [Ffi[Aead]] {
            match Aead::aead_sealer_new(key) {
                Some(s) => {
                    let mut s = s;
                    let code = Aead::aead_seal(inout s, inout p);
                    Aead::aead_sealer_free(s);
                    code
                },
                None => 1,
            }
        }\n",
    )
    .unwrap();
    assert!(binding.contains("seal_packet"), "{binding}");
    assert!(!binding.contains("$handle"), "{binding}");
    // A handle in an exported signature is a typed refusal.
    for body in [
        "pub fn take(inout s: Sealer) -> u32 eff [Ffi[Aead]] { 0 }\n",
        "pub fn make() -> Option[Sealer] eff [Ffi[Aead]] { Aead::aead_sealer_new(1) }\n",
    ] {
        let e = export(body).unwrap_err();
        assert!(e.contains("opaque .vsys extern-type handle"), "{e}");
    }
}

/// The measurement in docs/parallel-results/port-p3b-bridge-handles.md:
/// 20,000 seals of a 1,216-byte packet (1,200 payload bytes) through one
/// handle against 20,000 stateless seals that re-derive the key schedule,
/// from JIT code, next to the same loops in Rust. Run it with
/// `cargo test --release --test ffi_handles -- --ignored --nocapture`.
#[test]
#[ignore = "timing; run by hand in release"]
fn measure_seal_through_handle() {
    let Some(target) = host_target() else {
        return;
    };
    let _lock = JIT.lock().unwrap();
    let checked = checked(&fixture("aead.ves"), target);
    let symbols = symbols();
    let best = |f: &mut dyn FnMut()| {
        (0..7)
            .map(|_| {
                let t = std::time::Instant::now();
                f();
                t.elapsed()
            })
            .min()
            .unwrap()
    };
    let seals = 20_000u32;
    let per = |d: std::time::Duration| d.as_nanos() as f64 / f64::from(seals);
    // Each JIT run compiles its entry first: subtract a run of the same code
    // with one seal.
    let jit = |entry: &str| {
        best(&mut || {
            checked.jit(entry, &symbols).unwrap();
        })
    };
    let (handle, handle_one) = (jit("seal_many"), jit("seal_one"));
    let (stateless, stateless_one) = (jit("seal_many_stateless"), jit("seal_one_stateless"));
    let per_jit = |many: std::time::Duration, one: std::time::Duration| {
        many.saturating_sub(one).as_nanos() as f64 / f64::from(seals - 1)
    };
    let rust_handle = best(&mut || {
        let sch = expand(21);
        let mut p = vec![2u8; 1216];
        for _ in 0..seals {
            std::hint::black_box(seal(&sch, std::hint::black_box(&mut p)));
        }
    });
    let rust_stateless = best(&mut || {
        let mut p = vec![2u8; 1216];
        for _ in 0..seals {
            std::hint::black_box(seal(&expand(21), std::hint::black_box(&mut p)));
        }
    });
    println!(
        "Vesper JIT, seal through a handle: {handle:?} (one seal: {handle_one:?}), {:.0} ns/seal",
        per_jit(handle, handle_one)
    );
    println!(
        "Vesper JIT, stateless seal (re-derive key): {stateless:?} (one seal: {stateless_one:?}), {:.0} ns/seal",
        per_jit(stateless, stateless_one)
    );
    println!(
        "Rust, seal with one schedule: {:.0} ns/seal",
        per(rust_handle)
    );
    println!("Rust, stateless seal: {:.0} ns/seal", per(rust_stateless));
}

#[test]
fn verified_backend_and_wasm_refuse_foreign_handles() {
    let checked = checked(&fixture("aead.ves"), ("x86_64-unknown-linux-gnu", "sysv64"));
    let verified = vesper::verified_codegen::compile(
        checked.world(),
        "paths",
        &vesper::verified_codegen::Options::new(vesper::verified_codegen::Target::X86_64_LINUX),
    );
    assert!(
        verified.is_err(),
        "the verified backend has no foreign calls"
    );
    let wasm = vesper::wasm_backend::compile(
        checked.world(),
        "paths",
        &vesper::wasm_backend::WasmOptions::default(),
    );
    assert!(wasm.is_err(), "WebAssembly has no foreign calls");
}
