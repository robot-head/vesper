//! A host kernel ported to source vectors (§14, lane 28): the encoded size of
//! a JSON string from vesper-serde's encoder, as a 128-bit (Baseline128) and
//! a 256-bit (Avx2) kernel behind a checked `Isa::supported` dispatch. The
//! evaluator and the native JIT must both equal the host encoder on every
//! input, on whichever path this host's code generator selects, and the
//! 128-bit path is also run directly.
use vesper::{check, native, parser, runtime::Runtime};

const KERNEL: &str = include_str!("fixtures/vector/json_escape.ves");

fn host_size(text: &str) -> u64 {
    vesper_serde::encoded_size(
        &vesper_serde::Value::String(text),
        vesper_serde::EncodeLimits::default(),
    )
    .unwrap() as u64
}

fn run(main: &str) -> (String, String) {
    let source = format!("{KERNEL}\n{main}");
    let p = parser::parse(&source);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    let c = check::check(&p.module);
    assert!(c.diagnostics.is_empty(), "{:?}", c.diagnostics);
    let interpreted = Runtime::new(&c.world)
        .run("main", vec![])
        .unwrap()
        .to_string();
    let jit = native::jit(&c.world, "main").unwrap().to_string();
    for target in ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"] {
        native::object_for_target(&c.world, "main", Some(target)).unwrap();
    }
    (interpreted, jit)
}

fn inputs() -> Vec<String> {
    let mut out: Vec<String> = vec![
        String::new(),
        "a".into(),
        "plain ascii text".into(),
        "\"quoted\" and \\escaped\\".into(),
        "line\nbreak\r\ttab\u{8}backspace\u{c}feed".into(),
        (0u8..32).map(char::from).collect(),
        "héllo wörld ✓ — multibyte UTF-8 stays one cost per byte".into(),
        "x".repeat(15),
        "y".repeat(16),
        "z".repeat(17),
        "\"".repeat(31),
        "\\".repeat(32),
        "\u{1}".repeat(33),
    ];
    // Lengths around every chunk boundary, with escapes at the edges.
    for len in [47, 48, 49, 63, 64, 65, 100, 127, 128, 129] {
        let s: String = (0..len)
            .map(|i| match (i * 7 + len) % 11 {
                0 => '"',
                1 => '\\',
                2 => '\n',
                3 => '\u{1f}',
                4 => '\u{0}',
                _ => char::from(b'a' + (i % 26) as u8),
            })
            .collect();
        out.push(s);
    }
    out
}

#[test]
fn dispatched_kernel_matches_the_host_encoder() {
    for text in inputs() {
        let literal = serde_json::to_string(&text).unwrap();
        let (interpreted, jit) = run(&format!(
            "fn main() -> u64 {{ json_string_size({literal}) }}"
        ));
        let expected = host_size(&text).to_string();
        assert_eq!(interpreted, expected, "evaluator on {literal}");
        assert_eq!(jit, expected, "native on {literal}");
    }
}

#[test]
fn baseline_kernel_matches_the_host_encoder() {
    for text in inputs() {
        let literal = serde_json::to_string(&text).unwrap();
        let (interpreted, jit) = run(&format!(
            "fn main() -> u64 {{ if Isa::supported[Baseline128]() {{ wrapping_add(cost_from16({literal}, 0, 0), 2) }} else {{ 0 }} }}"
        ));
        let expected = host_size(&text).to_string();
        assert_eq!(interpreted, expected, "evaluator on {literal}");
        assert_eq!(jit, expected, "native on {literal}");
    }
}

#[test]
fn kernel_requires_its_profile() {
    let p = parser::parse(&format!(
        "{KERNEL}\nfn main() -> u64 {{ cost_from32(\"abc\", 0, 0) }}"
    ));
    let c = check::check(&p.module);
    assert!(
        c.diagnostics
            .iter()
            .any(|d| d.code == "E1403" && d.message.contains("Isa[Avx2]")),
        "{:?}",
        c.diagnostics
    );
}
