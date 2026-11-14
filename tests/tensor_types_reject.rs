//! Every rule of the source tensor types has a rejected fixture under
//! `tests/fixtures/tensor_types/reject`. Each file starts with
//!
//! ```text
//! // expect: <code>
//! // at: <source text the diagnostic span must cover>
//! // message: <substring of the message>
//! ```
use std::path::Path;

fn header<'a>(src: &'a str, key: &str) -> &'a str {
    src.lines()
        .find_map(|l| l.strip_prefix(&format!("// {key}: ")))
        .unwrap_or_else(|| panic!("missing '// {key}:' header"))
}

fn check(path: &Path) -> Result<(), String> {
    let src = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let (code, at, message) = (
        header(&src, "expect"),
        header(&src, "at"),
        header(&src, "message"),
    );
    let parsed = vesper::parser::parse(&src);
    if !parsed.diagnostics.is_empty() {
        return Err(format!("parse errors: {:?}", parsed.diagnostics));
    }
    // Every fixture uses tensor syntax, so the tensor checker runs.
    let checked = vesper::tensor_types::check(&parsed.module)
        .ok_or("the module does not activate the tensor checker")?;
    let found = checked.diagnostics.iter().any(|d| {
        d.code == code
            && d.message.contains(message)
            && src
                .get(d.span.clone())
                .is_some_and(|text| text.starts_with(at) || text.contains(at))
    });
    if found {
        Ok(())
    } else {
        Err(format!(
            "expected {code} at {at:?} containing {message:?}; found:\n{}",
            checked
                .diagnostics
                .iter()
                .map(|d| format!(
                    "  {} {:?} at {:?}",
                    d.code,
                    d.message,
                    src.get(d.span.clone()).unwrap_or("<out of range>")
                ))
                .collect::<Vec<_>>()
                .join("\n")
        ))
    }
}

#[test]
fn every_rejected_fixture_reports_its_rule_with_a_span() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tensor_types/reject");
    let mut paths: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "ves"))
        .collect();
    paths.sort();
    assert!(paths.len() >= 30, "{} fixtures", paths.len());
    let failures: Vec<String> = paths
        .iter()
        .filter_map(|p| check(p).err().map(|e| format!("{}: {e}", p.display())))
        .collect();
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

#[test]
fn every_diagnostic_code_has_a_fixture() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tensor_types/reject");
    let mut seen = std::collections::BTreeSet::new();
    for e in std::fs::read_dir(&dir).unwrap() {
        let src = std::fs::read_to_string(e.unwrap().path()).unwrap();
        seen.insert(header(&src, "expect").to_string());
    }
    use vesper::tensor_types::codes::*;
    for code in [
        KIND,
        DIM,
        SHAPE,
        INFER,
        OBLIGATION,
        DTYPE,
        WIRE,
        OPAQUE,
        HANDLER,
        UNSUPPORTED,
        POOL,
    ] {
        assert!(seen.contains(code), "no fixture for {code}");
    }
}

#[test]
fn the_core_check_entry_routes_tensor_modules() {
    // `check::check` (used by the CLI) runs the tensor checker first.
    let src = include_str!("fixtures/tensor_types/reject/matmul_inner.ves");
    let parsed = vesper::parser::parse(src);
    let checked = vesper::check::check(&parsed.module);
    assert!(
        checked
            .diagnostics
            .iter()
            .any(|d| d.code == header(src, "expect")),
        "{:?}",
        checked.diagnostics
    );
    let ok = vesper::parser::parse(include_str!(
        "../tests/fixtures/tensor_types/projection.ves"
    ));
    let checked = vesper::check::check(&ok.module);
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
}
