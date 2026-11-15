//! Bridge in the compiler (§13, wave-eight lane 20): `.vsys` imports,
//! `Ffi[Lib]` capabilities, native foreign calls and `isolated(mpk)`
//! trampolines. The runtime tests build real C libraries with `cc`, link
//! them with the emitted Vesper object and a C host, and run the result.
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};
use vesper::ffi_lowering::{self, FfiErrorKind, Handler, Imports};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "vesper-ffi-lowering-{}-{}",
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
fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ffi_lowering")
}
fn fixture(name: &str) -> String {
    fs::read_to_string(fixtures().join(name)).unwrap()
}
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
/// Whether this host can build and run the Linux x86-64 fixtures.
fn native_host() -> bool {
    if !(cfg!(target_os = "linux") && cfg!(target_arch = "x86_64")) {
        eprintln!("Bridge runtime fixtures need a Linux x86-64 host; runtime evidence unavailable");
        return false;
    }
    for tool in ["cc", "ar"] {
        if Command::new(tool).arg("--version").output().is_err() {
            eprintln!("Bridge runtime fixtures unavailable: missing {tool}");
            return false;
        }
    }
    true
}
/// Compile `source` into the static archive `lib<name>.a`; returns its path.
fn archive(temp: &Temp, source: &str, name: &str) -> PathBuf {
    let object = temp.0.join(format!("{name}.o"));
    run(Command::new("cc")
        .args([
            "-std=c11", "-Wall", "-Wextra", "-Werror", "-O2", "-fPIC", "-c",
        ])
        .arg(fixtures().join(source))
        .arg("-o")
        .arg(&object));
    let archive = temp.0.join(format!("lib{name}.a"));
    run(Command::new("ar").arg("crs").arg(&archive).arg(&object));
    archive
}
fn vsys(name: &str, archive: &Path) -> String {
    let digest = blake3::hash(&fs::read(archive).unwrap()).to_hex();
    fixture(name).replace("@DIGEST@", digest.as_str())
}
/// A placeholder digest for frontend-only tests.
fn vsys_unlinked(name: &str) -> String {
    fixture(name).replace("@DIGEST@", &"0".repeat(64))
}
fn bridge_imports(handler: Handler) -> Imports {
    let mut imports = Imports::new();
    imports.add(&vsys_unlinked("bridge.vsys"), handler).unwrap();
    imports
}
fn error_kind(source: &str, imports: &Imports) -> ffi_lowering::FfiError {
    match ffi_lowering::check_source(source, imports) {
        Ok(_) => panic!("accepted:\n{source}"),
        Err(e) => e,
    }
}

#[test]
fn foreign_calls_require_the_library_capability() {
    let imports = bridge_imports(Handler::Direct);
    let e = error_kind("fn f() -> u32 { Bridge::bridge_sum(1, 2) }", &imports);
    assert_eq!(e.kind, FfiErrorKind::Check, "{e}");
    assert!(
        e.diagnostics
            .iter()
            .any(|d| d.message.contains("Ffi[Bridge]")),
        "{e}"
    );
    // A pure caller of a foreign-capability function is rejected too.
    let e = error_kind(
        "fn f() -> u32 eff [Ffi[Bridge]] { Bridge::bridge_sum(1, 2) } fn g() -> u32 { f() }",
        &imports,
    );
    assert_eq!(e.kind, FfiErrorKind::Check, "{e}");
    assert!(
        e.diagnostics
            .iter()
            .any(|d| d.message.contains("Ffi[Bridge]")),
        "{e}"
    );
    let ok = ffi_lowering::check_source(
        "fn f() -> u32 eff [Ffi[Bridge]] { Bridge::bridge_sum(1, 2) }",
        &imports,
    )
    .unwrap();
    assert_eq!(ok.calls().len(), 1);
    assert_eq!(ok.calls()[0].symbol, "bridge_sum");
    assert_eq!(ok.calls()[0].handler, Handler::Direct);
}

#[test]
fn imports_are_resolved_with_typed_errors() {
    let imports = bridge_imports(Handler::Direct);
    let cases: &[(&str, FfiErrorKind)] = &[
        (
            "fn f() -> u32 eff [Ffi[Zlib]] { 1 }",
            FfiErrorKind::UnknownLibrary,
        ),
        (
            "fn f() -> u32 eff [Ffi[Bridge]] { Bridge::nope(1) }",
            FfiErrorKind::UnknownForeignFunction,
        ),
        (
            "fn f() -> u64 eff [Ffi[Bridge]] { let g = Bridge::bridge_tick; 1 }",
            FfiErrorKind::ForeignValue,
        ),
        (
            "fn f() -> u64 eff [Realtime, Ffi[Bridge]] { Bridge::bridge_tick() }",
            FfiErrorKind::Realtime,
        ),
        (
            "fn f() -> u64 eff [Ffi[Bridge, Bridge]] { 1 }",
            FfiErrorKind::MalformedCapability,
        ),
        (
            "fn f() -> u64 { handle Ffi[Bridge] in { 1 } with { return x => x } }",
            FfiErrorKind::ForeignHandler,
        ),
        (
            "copy struct Pair { a: u64 } fn f() -> u64 { 1 }",
            FfiErrorKind::NameCollision,
        ),
        (
            "fn f() -> u64 eff [Ffi[Bridge]] { let x: u64 = 3; Bridge::bridge_opt_len(in x) }",
            FfiErrorKind::SliceArgument,
        ),
        (
            "fn f() -> u64 eff [Ffi[Bridge]] { let x: (u8, u8) = (1, 2); let y: u64 = 4; Bridge::bridge_copy(in x, inout y) }",
            FfiErrorKind::SliceArgument,
        ),
    ];
    for (source, kind) in cases {
        let e = error_kind(source, &imports);
        assert_eq!(e.kind, *kind, "{source}\n{e}");
    }
    // A slice argument must have the slice's element type.
    let e = error_kind(
        "fn f() -> u64 eff [Ffi[Bridge]] { let x: (u32, u32) = (1, 2); let y: (u8, u8) = (0, 0); Bridge::bridge_copy(in x, inout y) }",
        &imports,
    );
    assert_eq!(e.kind, FfiErrorKind::Check, "{e}");
    // Unsupported .vsys forms are rejected where they are called.
    let mut narrow = Imports::new();
    narrow
        .add(
            r#"sys library Narrow (target = "x86_64-unknown-linux-gnu", abi = "sysv64", link = "static", name = "n", artifact = "blake3:0000000000000000000000000000000000000000000000000000000000000000") {
                fn takes_i32(x: i32) -> ForeignVal[i32];
                fn gives_ptr() -> ForeignVal[Ptr[u32]];
                fn fine() -> ForeignVal[u64];
            }"#,
            Handler::Direct,
        )
        .unwrap();
    for call in ["Narrow::takes_i32(1)", "Narrow::gives_ptr()"] {
        let e = error_kind(
            &format!("fn f() -> u64 eff [Ffi[Narrow]] {{ {call}; 1 }}"),
            &narrow,
        );
        assert_eq!(e.kind, FfiErrorKind::UnsupportedType, "{call}: {e}");
    }
    ffi_lowering::check_source(
        "fn f() -> u64 eff [Ffi[Narrow]] { Narrow::fine() }",
        &narrow,
    )
    .unwrap();
    // A library is imported once.
    assert_eq!(
        narrow
            .add(&vsys_unlinked("bridge.vsys"), Handler::Direct)
            .map(|_| ())
            .and_then(|_| narrow
                .add(&vsys_unlinked("bridge.vsys"), Handler::Direct)
                .map(|_| ()))
            .unwrap_err()
            .kind,
        FfiErrorKind::DuplicateLibrary
    );
    assert_eq!(
        Imports::new()
            .add("sys library", Handler::Direct)
            .unwrap_err()
            .kind,
        FfiErrorKind::Vsys
    );
}

#[test]
fn slice_arity_is_fixed_per_call_site() {
    let imports = bridge_imports(Handler::Direct);
    let checked = ffi_lowering::check_source(&fixture("bridge.ves"), &imports).unwrap();
    let lengths = |symbol: &str| -> Vec<Vec<usize>> {
        checked
            .calls()
            .iter()
            .filter(|c| c.symbol == symbol)
            .map(|c| c.slice_lengths.clone())
            .collect()
    };
    assert_eq!(lengths("bridge_copy"), vec![vec![4, 3]]);
    assert_eq!(lengths("bridge_crc_update"), vec![vec![4], vec![2]]);
    assert_eq!(lengths("bridge_rec_total"), vec![vec![3], vec![2]]);
    assert_eq!(lengths("bridge_opt_len"), vec![vec![3], vec![3]]);
    assert!(lengths("bridge_sum").iter().all(Vec::is_empty));
    // One operation per distinct arity; no open types reach the World.
    let effect = &checked.world().effects["Ffi$Bridge"];
    let names: Vec<&str> = effect.ops.iter().map(|o| o.name.as_str()).collect();
    assert!(names.contains(&"bridge_crc_update$4"), "{names:?}");
    assert!(names.contains(&"bridge_crc_update$2"), "{names:?}");
    assert!(names.contains(&"bridge_copy$4_3"), "{names:?}");
}

#[test]
fn objects_check_targets_and_handler_tiers() {
    let source = "fn entry() -> u64 eff [Ffi[Bridge]] { Bridge::bridge_tick() }";
    let direct = ffi_lowering::check_source(source, &bridge_imports(Handler::Direct)).unwrap();
    let e = direct
        .object("entry", Some("aarch64-unknown-linux-gnu"))
        .unwrap_err();
    assert_eq!(e.kind, FfiErrorKind::TargetMismatch, "{e}");
    let object = direct
        .object("entry", Some("x86_64-unknown-linux-gnu"))
        .unwrap();
    assert_eq!(&object.bytes[..4], b"\x7fELF");
    assert!(
        object.bytes.windows(11).any(|w| w == b"bridge_tick"),
        "the object imports the C symbol"
    );
    assert!(object.isolation_symbols.is_empty());
    assert_eq!(object.link.len(), 1);
    assert!(object.manifest().starts_with(ffi_lowering::SCHEMA));
    assert_eq!(
        object
            .verify_artifact("Bridge", b"not it")
            .unwrap_err()
            .kind,
        FfiErrorKind::ArtifactMismatch
    );
    let isolated =
        ffi_lowering::check_source(source, &bridge_imports(Handler::IsolatedMpk)).unwrap();
    let object = isolated
        .object("entry", Some("x86_64-unknown-linux-gnu"))
        .unwrap();
    assert_eq!(
        object.isolation_symbols,
        vec![ffi_lowering::isolation_symbol("Bridge")]
    );
    assert!(object.clif.contains("trapnz"), "{}", object.clif);
    // A foreign import never binds to a Vesper function of the same name.
    let clash = ffi_lowering::check_source(
        "fn bridge_tick() -> u64 eff [Ffi[Bridge]] { Bridge::bridge_tick() }",
        &bridge_imports(Handler::Direct),
    )
    .unwrap();
    let e = clash
        .object("bridge_tick", Some("x86_64-unknown-linux-gnu"))
        .unwrap_err();
    assert_eq!(e.kind, FfiErrorKind::Codegen, "{e}");
    assert!(e.message.contains("collides"), "{e}");
    // Only libraries the entry reaches are linked and target-checked.
    let mut imports = bridge_imports(Handler::Direct);
    imports
        .add(
            r#"sys library Arm (target = "aarch64-unknown-linux-gnu", abi = "aapcs64", link = "static", name = "arm", artifact = "blake3:0000000000000000000000000000000000000000000000000000000000000000") {
                fn arm_only() -> ForeignVal[u64];
            }"#,
            Handler::IsolatedMpk,
        )
        .unwrap();
    let mixed = ffi_lowering::check_source(
        "fn entry() -> u64 eff [Ffi[Bridge]] { Bridge::bridge_tick() } fn unused() -> u64 eff [Ffi[Arm]] { Arm::arm_only() } fn other() -> u64 eff [Ffi[Arm]] { unused() }",
        &imports,
    )
    .unwrap();
    let object = mixed
        .object("entry", Some("x86_64-unknown-linux-gnu"))
        .unwrap();
    assert_eq!(object.link.len(), 1);
    assert_eq!(object.link[0].library, "Bridge");
    assert!(object.isolation_symbols.is_empty());
    let e = mixed
        .object("other", Some("x86_64-unknown-linux-gnu"))
        .unwrap_err();
    assert_eq!(e.kind, FfiErrorKind::TargetMismatch, "{e}");
    let e = mixed
        .object("other", Some("aarch64-unknown-linux-gnu"))
        .unwrap_err();
    assert_eq!(e.kind, FfiErrorKind::IsolationUnavailable, "{e}");
    // The native backend refuses foreign effects outside this frontend.
    assert!(vesper::native::object(direct.world(), "entry").is_err());
    assert_eq!(ffi_lowering::pkru_revoke_mask(1).unwrap(), 0b1100);
    assert!(ffi_lowering::pkru_revoke_mask(0).is_err());
    assert!(ffi_lowering::pkru_revoke_mask(16).is_err());
}

#[test]
fn c_host_calls_vesper_which_calls_c_libraries() {
    if !native_host() {
        return;
    }
    let temp = Temp::new();
    let lib = archive(&temp, "bridge.c", "vesper_bridge");
    let mut imports = Imports::new();
    imports
        .add(&vsys("bridge.vsys", &lib), Handler::Direct)
        .unwrap();
    let checked = ffi_lowering::check_source(&fixture("bridge.ves"), &imports).unwrap();
    let object = checked.object("bridge_main", None).unwrap();
    object
        .verify_artifact("Bridge", &fs::read(&lib).unwrap())
        .unwrap();
    let object_path = temp.0.join("bridge_main.o");
    fs::write(&object_path, &object.bytes).unwrap();
    let exe = temp.0.join("bridge_host");
    run(Command::new("cc")
        .arg(fixtures().join("host.c"))
        .arg(&object_path)
        .arg(&lib)
        .arg("-o")
        .arg(&exe));
    let output = run(&mut Command::new(&exe));
    let stdout = String::from_utf8(output.stdout).unwrap();
    let expected = "\
1=42
2=-17
3=65535
4=-5
scenario 1 -> 12
10=10
11=202
12=2
13=30
14=200
15=9000
scenario 2 -> 9444
20=91
21=88
22=89
23=6496457
24=1948128169
scenario 3 -> 3
30=5
31=18
32=1
33=16910071
34=16909061
35=6
36=254
37=3
38=2000
scenario 4 -> 16911326
40=13
41=30
42=-12
43=3
44=42
scenario 5 -> 0
50=3
51=999
52=41
53=4294967295
54=42
55=2
scenario 6 -> 0
60=3
60=2
60=1
scenario 7 -> 0
scenario 8 -> 3
";
    assert_eq!(stdout, expected);
}

/// Builds the isolated(mpk) host and runs it in `mode`.
fn mpk_run(temp: &Temp, mode: &str) -> Output {
    let exe = temp.0.join("mpk_host");
    if !exe.exists() {
        let lib = archive(temp, "sandbox.c", "vesper_sandbox");
        let mut imports = Imports::new();
        imports
            .add(&vsys("sandbox.vsys", &lib), Handler::IsolatedMpk)
            .unwrap();
        let checked = ffi_lowering::check_source(&fixture("sandbox.ves"), &imports).unwrap();
        let object = checked.object("sandbox_main", None).unwrap();
        assert_eq!(
            object.isolation_symbols,
            vec!["vesper_bridge_pkru_revoke_Sandbox".to_owned()]
        );
        let object_path = temp.0.join("sandbox_main.o");
        fs::write(&object_path, &object.bytes).unwrap();
        run(Command::new("cc")
            .arg(fixtures().join("mpk_host.c"))
            .arg(&object_path)
            .arg(&lib)
            .arg("-o")
            .arg(&exe));
    }
    Command::new(&exe).arg(mode).output().unwrap()
}

#[test]
fn isolated_callee_writing_protected_memory_faults() {
    if !native_host() {
        return;
    }
    let temp = Temp::new();
    let ok = mpk_run(&temp, "ok");
    let text = String::from_utf8_lossy(&ok.stdout).into_owned();
    if ok.status.code() == Some(77) {
        eprintln!("isolated(mpk) runtime evidence unavailable: pkey_alloc failed ({text})");
        return;
    }
    eprintln!("isolated(mpk) exercised on this host (PKU present)");
    assert!(ok.status.success(), "{text}");
    assert_eq!(text, "result 48\npage 8\nrevoked after return 0\n");
    let fault = mpk_run(&temp, "fault");
    let text = String::from_utf8_lossy(&fault.stdout).into_owned();
    assert_eq!(fault.status.code(), Some(42), "{text}");
    assert_eq!(text, "before\npkey fault\n");
    // Without a configured key the trampoline traps before calling C.
    let unset = mpk_run(&temp, "unset");
    assert!(!unset.status.success());
    assert_eq!(unset.status.code(), None, "terminated by a signal");
}
