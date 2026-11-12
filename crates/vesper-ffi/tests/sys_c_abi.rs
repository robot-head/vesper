//! Real native C ABI evidence. The library remains entirely safe Rust; only
//! the controlled separate fixture host crosses its explicitly unsafe ABI.
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};
use vesper_ffi::sys::*;
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Temp(PathBuf);
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn run(command: &mut Command) -> Output {
    let output = command.output().expect("run native fixture tool");
    assert!(
        output.status.success(),
        "command {command:?}\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}
#[test]
fn actual_static_c_calls_match_lowered_signature_and_foreign_validation() {
    if !Target::LinuxX86_64.is_host() {
        eprintln!(
            "C ABI fixture requires the exact Linux x86_64 GNU host target; runtime evidence unavailable on this host"
        );
        return;
    }
    for tool in ["cc", "ar", "rustc"] {
        if Command::new(tool).arg("--version").output().is_err() {
            eprintln!("C ABI fixture runtime unavailable: missing {tool}");
            return;
        }
    }
    let temp = Temp(std::env::temp_dir().join(format!(
        "vesper-bridge-c-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )));
    fs::create_dir(&temp.0).unwrap();
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let object = temp.0.join("fixture.o");
    let archive = temp.0.join("libbridge_fixture.a");
    let executable = temp.0.join("host");
    run(Command::new("cc")
        .args(["-std=c11", "-Wall", "-Wextra", "-Werror", "-O2", "-c"])
        .arg(fixtures.join("sys_fixture.c"))
        .arg("-o")
        .arg(&object));
    run(Command::new("ar").arg("crs").arg(&archive).arg(&object));
    let digest = blake3::hash(&fs::read(&archive).unwrap()).to_hex();
    let source = format!(
        r#"sys library Fixture (
        target = "x86_64-unknown-linux-gnu", abi = "sysv64",
        link = "static", name = "bridge_fixture", artifact = "blake3:{digest}"
    ) {{
        enum State: u32 {{ Ready = 1, Busy = 7 }}
        fn bridge_sum(a: u32, b: u32) -> ForeignVal[u32];
        fn bridge_negative() -> ForeignVal[i16];
        fn bridge_bool() -> ForeignVal[bool];
        fn bridge_state() -> ForeignVal[State];
        fn bridge_address() -> ForeignVal[Ptr[u32]];
        fn bridge_copy(nocapture in src: CSlice[u8], nocapture inout dst: CSlice[u8]) -> ForeignVal[usize];
    }}"#
    );
    let library = parse(&source, Limits::default()).unwrap();
    let archive_bytes = fs::read(&archive).unwrap();
    assert_eq!(
        library.link().artifact_blake3(),
        blake3::hash(&archive_bytes).as_bytes()
    );
    library.link().verify_artifact(&archive_bytes).unwrap();
    assert!(
        library
            .link()
            .verify_artifact(b"different foreign library")
            .is_err()
    );
    assert_eq!(
        library.function("bridge_copy").unwrap().abi(),
        AbiSignature {
            convention: CallingConvention::SysV64,
            parameters: vec![
                AbiType::Pointer,
                AbiType::Integer(Scalar::Usize),
                AbiType::Pointer,
                AbiType::Integer(Scalar::Usize)
            ],
            result: Some(AbiType::Integer(Scalar::Usize)),
        }
    );
    assert_eq!(
        library.function("bridge_bool").unwrap().abi().result,
        Some(AbiType::Integer(Scalar::U8))
    );
    run(Command::new("rustc")
        .arg("--edition=2024")
        .arg(fixtures.join("sys_fixture_host.rs"))
        .arg("-L")
        .arg(format!("native={}", temp.0.display()))
        .arg("-l")
        .arg("static=bridge_fixture")
        .arg("-o")
        .arg(&executable));
    let output = run(&mut Command::new(&executable));
    let text = String::from_utf8(output.stdout).unwrap();
    let mut lines = text.lines();
    let numbers: Vec<u64> = lines
        .next()
        .unwrap()
        .split_whitespace()
        .map(|n| n.parse().unwrap())
        .collect();
    assert_eq!(numbers.len(), 6);
    assert_eq!(&numbers[..5], &[42, 65519, 2, 9, 4]);
    assert_eq!(lines.next(), Some("91 90 165 218"));
    for (symbol, bits, expected) in [
        ("bridge_sum", numbers[0], ValidatedValue::Unsigned(42)),
        ("bridge_negative", numbers[1], ValidatedValue::Signed(-17)),
        ("bridge_copy", numbers[4], ValidatedValue::Unsigned(4)),
    ] {
        assert_eq!(
            library
                .function(symbol)
                .unwrap()
                .capture_return(bits, ())
                .validate()
                .unwrap()
                .value(),
            &expected
        );
    }
    let failure = library
        .function("bridge_bool")
        .unwrap()
        .capture_return(numbers[2], vec![1u8, 2, 3])
        .validate()
        .unwrap_err();
    let (error, raw) = failure.into_parts();
    assert_eq!(error, ValidationError::InvalidBool);
    assert_eq!(raw.into_parts(), (2, vec![1, 2, 3]));
    assert!(matches!(
        library
            .function("bridge_state")
            .unwrap()
            .capture_return(numbers[3], ())
            .validate()
            .unwrap_err()
            .error(),
        ValidationError::InvalidEnum { .. }
    ));
    assert!(
        library
            .function("bridge_address")
            .unwrap()
            .capture_return(numbers[5], ())
            .validate()
            .is_ok()
    );
    eprintln!(
        "executed real Linux x86_64 System V C/Rust ABI fixture; descriptor {}",
        library.identity()
    );
}
