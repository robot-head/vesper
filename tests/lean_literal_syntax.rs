use std::{
    fs,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};
use vesper::{check, lean, parser};

struct Scratch(std::path::PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn extracted_scalar_literals_preserve_control_characters_and_ieee_bits() {
    let text = "\0\u{8}\u{c}\n\r\t\u{1f}\u{7f}\u{85}\"\\λ😀";
    let floats = [0.0f64, 1.2345678901234567, 5e-324, f64::MAX];
    let mut source = format!(
        "fn text() -> String {{ {} }}\n",
        serde_json::to_string(text).unwrap()
    );
    for (index, value) in floats.iter().enumerate() {
        source.push_str(&format!("fn float{index}() -> f64 {{ {value:?} }}\n"));
    }
    source.push_str("fn minimum() -> i64 { -9223372036854775808 }\nfn maximum() -> u64 { 18446744073709551615 }\nfn truth() -> bool { true }\n");
    let parsed = parser::parse(&source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let checked = check::check(&parsed.module);
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
    let mut output = lean::extract(&checked.world).unwrap();
    assert!(!output.contains("\\b"));
    assert!(!output.contains("\\f"));
    assert!(output.contains("\\x08") && output.contains("\\x0c"));
    output.push_str("\n#eval Vesper.«text».toList.map Char.toNat\n");
    for index in 0..floats.len() {
        output.push_str(&format!("#eval Float.toBits Vesper.«float{index}»\n"));
    }
    output.push_str("#eval Vesper.«minimum»\n#eval Vesper.«maximum»\n#eval Vesper.«truth»\n");
    let compiler = std::env::var("VESPER_LEAN").unwrap_or_else(|_| "lean".into());
    if std::env::var_os("VESPER_LEAN").is_none()
        && Command::new(&compiler).arg("--version").output().is_err()
    {
        eprintln!("Lean test skipped: install pinned Lean or set VESPER_LEAN");
        return;
    }
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "vesper-lean-literal-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let builder = &mut fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(&path).unwrap();
    let scratch = Scratch(path);
    let file = scratch.0.join("Model.lean");
    fs::write(&file, &output).unwrap();
    let result = Command::new(compiler).arg(file).output().unwrap();
    assert!(
        result.status.success(),
        "{}\n{}\n{output}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    let expected = std::iter::once(format!(
        "{:?}",
        text.chars().map(u32::from).collect::<Vec<_>>()
    ))
    .chain(floats.iter().map(|n| n.to_bits().to_string()))
    .chain([i64::MIN.to_string(), u64::MAX.to_string(), "true".into()])
    .collect::<Vec<_>>()
    .join("\n");
    assert_eq!(String::from_utf8(result.stdout).unwrap().trim(), expected);
}
