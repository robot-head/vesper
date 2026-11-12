use serde_json::Value;
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "vesper-grammar-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn file(&self, name: &str, bytes: impl AsRef<[u8]>) -> PathBuf {
        let path = self.0.join(name);
        fs::write(&path, bytes).unwrap();
        path
    }
    fn schema(&self) -> PathBuf {
        self.file("schema.json", schema())
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn schema() -> &'static str {
    r#"{"profile":"vesper.protobuf.explicit-presence/1","root":0,"messages":[{"fields":[{"number":1,"kind":"u64"},{"number":2,"kind":"string"}]}]}"#
}
fn path(path: &Path) -> &str {
    path.to_str().unwrap()
}
fn invoke(args: &[&str]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_vesper"))
        .arg("grammar")
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let out = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).unwrap();
        bytes
    });
    let err = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).unwrap();
        bytes
    });
    let start = Instant::now();
    let mut timeout = false;
    while child.try_wait().unwrap().is_none() {
        if start.elapsed() > Duration::from_secs(20) {
            timeout = true;
            child.kill().unwrap();
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let status = child.wait().unwrap();
    let output = Output {
        status,
        stdout: out.join().unwrap(),
        stderr: err.join().unwrap(),
    };
    assert!(!timeout, "grammar command exceeded its test deadline");
    output
}
fn report(output: &Output, success: bool) -> Value {
    assert_eq!(
        output.status.success(),
        success,
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let data: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "{e}: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    assert_eq!(data["schema"], "vesper.grammar/1");
    assert_eq!(data["status"], if success { "valid" } else { "invalid" });
    data
}

#[test]
fn protobuf_actual_values_presence_hashes_and_transactional_read_only_validation() {
    let f = Fixture::new();
    let schema = f.schema();
    let input = f.file("data.pb", [8, 42]);
    let args = [
        "check",
        "protobuf",
        path(&input),
        "--schema",
        path(&schema),
        "--json",
    ];
    let data = report(&invoke(&args), true);
    assert_eq!(data["consumed_bytes"], 2);
    assert_eq!(
        data["input_blake3"],
        blake3::hash(&[8, 42]).to_hex().to_string()
    );
    assert_eq!(
        data["result"]["profile"],
        "vesper.protobuf.explicit-presence/1"
    );
    assert_eq!(data["result"]["roundtrip_bytes_equal"], true);
    assert_eq!(
        data["result"]["projection"]["fields"][0]["value"]["value"],
        "42"
    );
    fs::write(&input, []).unwrap();
    let absent = report(&invoke(&args), true);
    fs::write(&input, [8, 0]).unwrap();
    let present = report(&invoke(&args), true);
    assert_eq!(absent["result"]["projection"]["field_occurrences"], 0);
    assert_eq!(present["result"]["projection"]["field_occurrences"], 1);
    assert_ne!(absent["input_blake3"], present["input_blake3"]);
    for bytes in [
        vec![8, 0xaa, 0],
        vec![8, 1, 8, 2],
        vec![24, 1],
        vec![8, 0x80],
    ] {
        fs::write(&input, &bytes).unwrap();
        report(&invoke(&args), false);
        assert_eq!(fs::read(&input).unwrap(), bytes);
    }
    assert_eq!(fs::read_to_string(&schema).unwrap(), self::schema());
}

#[test]
fn protobuf_schema_rejects_decoded_duplicates_unknowns_inactive_nulls_cycles_and_deep_subtrees() {
    let f = Fixture::new();
    let input = f.file("data.pb", [8, 42]);
    let schema = f.schema();
    let args = [
        "check",
        "protobuf",
        path(&input),
        "--schema",
        path(&schema),
        "--json",
    ];
    let mut invalid = vec![
        self::schema().replace("\"number\":1", "\"number\":1,\"\\u006eumber\":1"),
        self::schema().replace("\"kind\":\"u64\"", "\"kind\":\"u64\",\"ignored\":true"),
        self::schema().replace("\"kind\":\"u64\"", "\"kind\":\"u64\",\"message\":null"),
        self::schema().replace("\"kind\":\"u64\"", "\"kind\":\"u64\",\"message\":0"),
        self::schema().replace("\"kind\":\"u64\"", "\"kind\":\"message\",\"message\":0"),
        self::schema().replace("\"root\":0", "\"root\":0,\"r\\u006fot\":0"),
        self::schema().replace("\"root\":0", "\"root\":1"),
    ];
    invalid.push(format!(
        "{{\"profile\":\"wrong\",\"root\":0,\"messages\":[],\"unknown\":{}0{}}}",
        "[".repeat(80),
        "]".repeat(80)
    ));
    for value in invalid {
        fs::write(&schema, &value).unwrap();
        report(&invoke(&args), false);
        assert_eq!(fs::read_to_string(&schema).unwrap(), value);
        assert_eq!(fs::read(&input).unwrap(), [8, 42]);
    }
    fs::write(&schema, vec![b' '; 64 * 1024 + 1]).unwrap();
    report(&invoke(&args), false);
}

#[test]
fn previews_never_claim_to_be_complete_when_payloads_are_elided() {
    let f = Fixture::new();
    let schema = f.schema();
    let mut bytes = vec![18, 240, 1];
    bytes.extend_from_slice(&[b'a'; 240]);
    let input = f.file("long.pb", &bytes);
    let data = report(
        &invoke(&[
            "check",
            "protobuf",
            path(&input),
            "--schema",
            path(&schema),
            "--json",
        ]),
        true,
    );
    assert_eq!(data["result"]["projection_truncated"], true);
    assert_eq!(
        data["result"]["projection"]["fields"][0]["value"]["value"]["truncated"],
        true
    );
    assert_eq!(fs::read(input).unwrap(), bytes);
}

#[test]
fn der_actual_values_and_typed_set_of_require_canonical_reencoding() {
    let f = Fixture::new();
    let input = f.file("data.der", [0x30, 6, 2, 1, 42, 1, 1, 0xff]);
    let data = report(&invoke(&["check", "der", path(&input), "--json"]), true);
    assert_eq!(data["result"]["nodes"], 3);
    assert_eq!(data["result"]["roundtrip_bytes_equal"], true);
    assert_eq!(
        data["result"]["projection"]["value"]["children"][0]["value"]["value_i64"],
        "42"
    );
    fs::write(&input, [0x31, 6, 2, 1, 1, 2, 1, 2]).unwrap();
    report(&invoke(&["check", "der", path(&input), "--json"]), false);
    let data = report(
        &invoke(&["check", "der", path(&input), "--set-of", "--json"]),
        true,
    );
    assert_eq!(data["result"]["root_set_of"], true);
    for bytes in [
        vec![0x31, 6, 2, 1, 2, 2, 1, 1],
        vec![2, 2, 0, 42],
        vec![2, 0x81, 1, 42],
        vec![1, 1, 1],
        vec![2, 1, 42, 0],
    ] {
        fs::write(&input, &bytes).unwrap();
        report(
            &invoke(&["check", "der", path(&input), "--set-of", "--json"]),
            false,
        );
        report(&invoke(&["check", "der", path(&input), "--json"]), false);
        assert_eq!(fs::read(&input).unwrap(), bytes);
    }
}

#[test]
fn actual_arrow_reference_streams_keep_nulls_versions_alignment_and_overflow_honest() {
    let f = Fixture::new();
    let fixtures =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("crates/vesper-grammar/tests/fixtures/ipc");
    for (name, version) in [("modern-v5.arrow", "V5"), ("legacy-v4.arrow", "V4")] {
        let bytes = fs::read(fixtures.join(name)).unwrap();
        let input = f.file(name, &bytes);
        let data = report(
            &invoke(&["check", "arrow-ipc", path(&input), "--json"]),
            true,
        );
        let result = &data["result"];
        assert_eq!(result["metadata_version"], version);
        assert_eq!(result["canonical_serialization_claimed"], false);
        assert_eq!(result["field_count"], 2);
        assert_eq!(result["batch_count"], 3);
        assert_eq!(result["total_rows"], 13);
        assert_eq!(result["batches"][0]["columns"][0]["nulls"], 2);
        assert_eq!(result["batches"][0]["columns"][0]["sum"], "11");
        assert_eq!(result["batches"][0]["columns"][1]["sum"], "450");
        assert!(result["batches"][2]["columns"][0]["sum"].is_null());
        assert!(result["batches"][2]["columns"][0]["sum_unavailable_reason"].is_null());
        assert_eq!(result["batches"][2]["columns"][1]["sum"], "12");
        let strict = report(
            &invoke(&[
                "check",
                "arrow-ipc",
                path(&input),
                "--simd-alignment",
                "--json",
            ]),
            false,
        );
        assert!(strict["error"].as_str().unwrap().contains("Alignment"));
        assert_eq!(fs::read(&input).unwrap(), bytes);
    }
    let input = f.file(
        "overflow.arrow",
        fs::read(fixtures.join("sum-overflow.arrow")).unwrap(),
    );
    let data = report(
        &invoke(&["check", "arrow-ipc", path(&input), "--json"]),
        true,
    );
    let column = &data["result"]["batches"][0]["columns"][0];
    assert!(column["sum"].is_null());
    assert!(
        column["sum_unavailable_reason"]
            .as_str()
            .unwrap()
            .contains("Overflow")
    );
    for name in [
        "compressed.arrow",
        "dictionary.arrow",
        "int32.arrow",
        "unsigned.arrow",
        "nested.arrow",
        "custom-metadata.arrow",
    ] {
        let input = f.file(name, fs::read(fixtures.join(name)).unwrap());
        report(
            &invoke(&["check", "arrow-ipc", path(&input), "--json"]),
            false,
        );
    }
}

#[test]
fn invalid_arguments_files_and_non_ipc_bytes_emit_one_error_without_writes() {
    let f = Fixture::new();
    let input = f.file("invalid.arrow", b"not an Arrow stream");
    for args in [
        vec!["--json"],
        vec!["check", "unknown", path(&input), "--json"],
        vec!["check", "protobuf", path(&input), "--json"],
        vec![
            "check",
            "der",
            path(&input),
            "--schema",
            "missing.json",
            "--json",
        ],
        vec!["check", "der", path(&input), "--simd-alignment", "--json"],
        vec!["check", "arrow-ipc", path(&input), "--json"],
        vec!["check", "der", path(&f.0), "--json"],
    ] {
        report(&invoke(&args), false);
    }
    assert_eq!(fs::read(&input).unwrap(), b"not an Arrow stream");
    assert!(invoke(&["--help"]).status.success());
    #[cfg(unix)]
    {
        let link = f.0.join("alias.arrow");
        std::os::unix::fs::symlink(&input, &link).unwrap();
        report(
            &invoke(&["check", "arrow-ipc", path(&link), "--json"]),
            false,
        );
    }
}
