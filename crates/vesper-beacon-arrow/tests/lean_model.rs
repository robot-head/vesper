//! `lean/BeaconRedaction.lean`: source hygiene always; with `VESPER_LEAN` set
//! (as in CI) compile it, audit every theorem's axioms, and compare the Lean
//! `exportAttr` model under the `redact` policy with the Rust exporter on
//! generated attribute sets, through a full OTAP encode and decode.
//!
//! The comparison is over concrete inputs. The general claims are the Lean
//! theorems, which are about the Lean models, not about this Rust code.

use std::io::Write;
use std::path::{Path, PathBuf};

use vesper_beacon::*;
use vesper_beacon_arrow::otap::*;

const MODEL_ID: &str = "vesper.beacon.redaction-model/1";
const MODEL_CLAIM: &str =
    "attribute redaction and dictionary export stream models; not a proof about the Rust implementation";
const STANDARD_AXIOMS: [&str; 3] = ["propext", "Classical.choice", "Quot.sound"];
const THEOREMS: [&str; 7] = [
    "export_attr_noninterference",
    "redact_only_low",
    "export_span_noninterference",
    "export_batch_noninterference",
    "stream_noninterference",
    "emitted_strings_exported",
    "pii_never_emitted",
];

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../lean")
}

fn strip_comments(source: &str) -> String {
    let mut out = String::new();
    let mut depth = 0usize;
    let mut chars = source.chars().peekable();
    while let Some(c) = chars.next() {
        match (c, chars.peek()) {
            ('/', Some('-')) => {
                chars.next();
                depth += 1;
            }
            ('-', Some('/')) if depth > 0 => {
                chars.next();
                depth -= 1;
            }
            ('-', Some('-')) if depth == 0 => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

#[test]
fn lean_source_has_no_escape_hatches_and_audits_every_theorem() {
    let source = std::fs::read_to_string(root().join("BeaconRedaction.lean")).expect("read model");
    let code = strip_comments(&source);
    for banned in [
        "sorry",
        "admit",
        "axiom ",
        "unsafe",
        "partial ",
        "@[implemented_by",
        "@[extern",
    ] {
        assert!(
            !code.contains(banned),
            "BeaconRedaction.lean contains {banned:?}"
        );
    }
    assert!(code.contains(&format!("def modelId : String := \"{MODEL_ID}\"")));
    for theorem in THEOREMS {
        assert!(
            code.contains(&format!("theorem {theorem}")),
            "{theorem} missing"
        );
        assert!(
            code.contains(&format!("#print axioms Vesper.BeaconRedaction.{theorem}\n")),
            "{theorem} not audited"
        );
    }
}

struct Case {
    /// (public?, key, value)
    attrs: Vec<(bool, &'static str, String)>,
}

fn generate(cases: usize) -> Vec<Case> {
    let mut state = 0x6265_6163_6f6e_0001u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    const KEYS: [&str; 8] = [
        "user.email",
        "user.id",
        "route",
        "auth",
        "ip",
        "tenant",
        "card",
        "session",
    ];
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789@.-_";
    (0..cases)
        .map(|_| {
            let mut keys: Vec<&'static str> = KEYS.to_vec();
            let n = (next() % 6) as usize;
            let attrs = (0..n)
                .map(|_| {
                    let key = keys.remove((next() % keys.len() as u64) as usize);
                    let len = 1 + (next() % 24) as usize;
                    let value: String = (0..len)
                        .map(|_| ALPHABET[(next() % ALPHABET.len() as u64) as usize] as char)
                        .collect();
                    (next() % 2 == 0, key, value)
                })
                .collect();
            Case { attrs }
        })
        .collect()
}

/// Rust result in the fixture's output format: encode with the real
/// exporter, decode the OTAP batch, render the attributes.
fn rust_render(case: &Case, encoder: &mut TraceEncoder, decoder: &mut OtapDecoder) -> String {
    let pool = SpanPool::<1>::new(1, core::num::NonZeroU64::new(1).unwrap(), 0).unwrap();
    let mut ring = TelemetryRing::<1>::default();
    pool.start_span(None, SpanSchema::Request, 0, 0)
        .unwrap()
        .finish_ok(&mut ring, 1, PrimaryAttribute::None)
        .unwrap();
    let mut span = OtapSpan::from(ring.records().next().unwrap());
    for (public, key, value) in &case.attrs {
        if *public {
            // Static vocabulary for the test only.
            let value: &'static str = Box::leak(value.clone().into_boxed_str());
            span.attributes.insert(key, value).unwrap();
        } else {
            span.attributes
                .insert_pii(key, Pii::new(value.clone()), &PiiPolicy::Redact)
                .unwrap();
        }
    }
    let batch = encoder.encode_spans(&[span]).unwrap();
    for (public, _, value) in &case.attrs {
        if !*public && value.len() >= 6 {
            let wire = prost::Message::encode_to_vec(&batch);
            assert!(
                !wire.windows(value.len()).any(|w| w == value.as_bytes())
                    || case
                        .attrs
                        .iter()
                        .any(|(p, k, v)| (*p && v.contains(value.as_str()))
                            || k.contains(value.as_str())),
                "PII value {value} reached the wire"
            );
        }
    }
    let decoded = decoder.decode(&batch).unwrap();
    decoded.spans[0]
        .attributes
        .iter()
        .map(|(k, v)| match v {
            DecodedValue::Str(s) => format!("{k}={s}"),
            other => panic!("unexpected value {other:?}"),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn lean_path(lean: &str, dir: &Path) -> std::ffi::OsString {
    let mut paths = vec![dir.to_path_buf()];
    if let Ok(out) = std::process::Command::new(lean)
        .arg("--print-libdir")
        .output()
    {
        if out.status.success() {
            let lib = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !lib.is_empty() {
                paths.push(PathBuf::from(lib));
            }
        }
    }
    std::env::join_paths(paths).expect("LEAN_PATH")
}

#[test]
fn lean_model_compiles_and_matches_rust_redaction() {
    let Ok(lean) = std::env::var("VESPER_LEAN") else {
        eprintln!("VESPER_LEAN unset; Lean compilation and differential skipped");
        return;
    };
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("vesper-beacon-{}-{nanos}", std::process::id()));
    std::fs::create_dir(&dir).expect("private Lean output directory");
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(dir.clone());

    let compiled = std::process::Command::new(&lean)
        .current_dir(root())
        .arg("-o")
        .arg(dir.join("BeaconRedaction.olean"))
        .arg("BeaconRedaction.lean")
        .output()
        .expect("spawn Lean");
    let report = String::from_utf8(compiled.stdout).expect("UTF-8");
    assert!(
        compiled.status.success(),
        "Lean model failed: {}\n{report}\n{}",
        compiled.status,
        String::from_utf8_lossy(&compiled.stderr)
    );
    let mut audited = 0;
    for line in report.lines() {
        if line.ends_with("does not depend on any axioms") {
            audited += 1;
            continue;
        }
        let axioms = line
            .split_once(" depends on axioms: [")
            .and_then(|(_, rest)| rest.strip_suffix(']'))
            .unwrap_or_else(|| panic!("unexpected Lean output: {line}"));
        for axiom in axioms.split(", ") {
            assert!(
                STANDARD_AXIOMS.contains(&axiom),
                "unapproved axiom {axiom} in {line}"
            );
        }
        audited += 1;
    }
    assert_eq!(audited, THEOREMS.len(), "axiom report lines");

    let cases = generate(500);
    let input: String = cases
        .iter()
        .filter(|c| !c.attrs.is_empty())
        .map(|c| {
            c.attrs
                .iter()
                .map(|(p, k, v)| format!("{}:{k}:{v}", if *p { "pub" } else { "pii" }))
                .collect::<Vec<_>>()
                .join(" ")
                + "\n"
        })
        .collect();
    let mut child = std::process::Command::new(&lean)
        .env("LEAN_PATH", lean_path(&lean, &dir))
        .arg("--run")
        .arg(root().join("BeaconRedaction/Fixture.lean"))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn Lean");
    let mut stdin = child.stdin.take().expect("stdin");
    let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()));
    let output = child.wait_with_output().expect("wait for Lean");
    writer.join().expect("writer").expect("write stdin");
    let stdout = String::from_utf8(output.stdout).expect("UTF-8");
    assert!(
        output.status.success(),
        "Lean fixture failed: {}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let mut lines = stdout.lines();
    assert_eq!(
        lines.next(),
        Some(format!("{MODEL_ID} | {MODEL_CLAIM}").as_str())
    );
    // One encoder and decoder across all cases: dictionaries grow by deltas.
    let mut encoder = TraceEncoder::new(EncoderConfig::default());
    let mut decoder = OtapDecoder::default();
    let mut compared = 0;
    for case in cases.iter().filter(|c| !c.attrs.is_empty()) {
        let lean_line = lines.next().expect("one Lean line per case");
        assert_eq!(rust_render(case, &mut encoder, &mut decoder), lean_line);
        compared += 1;
    }
    assert!(lines.next().is_none());
    assert!(compared > 300, "compared {compared} cases");
}
