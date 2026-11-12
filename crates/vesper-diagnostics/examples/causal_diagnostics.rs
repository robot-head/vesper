use std::sync::Arc;
use vesper_core::ast::Diagnostic;
use vesper_diagnostics::{Limits, prepare};
use vesper_span::LineIndex;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let source = "let 名 = 😀;\r\nconsume(名);\r\n";
    let index = LineIndex::new(Arc::from(source), Limits::default().span)?;
    let first = source.find('名').unwrap();
    let next = source.rfind('名').unwrap();
    let diagnostics = [
        Diagnostic::new("E0300", "value already consumed", next..next + 3)
            .with_related(first..first + 3, "binding declared here")
            .with_help("Consume each linear value exactly once."),
    ];
    let prepared = prepare(&index, &diagnostics, Limits::default())?;
    let lsp = prepared.to_lsp("untitled:unicode")?;
    assert_eq!(lsp[0]["range"]["start"]["line"], 1);
    assert_eq!(
        lsp[0]["relatedInformation"][0]["location"]["range"]["start"]["character"],
        4
    );
    assert_eq!(
        lsp[0]["data"]["help"],
        "Consume each linear value exactly once."
    );
    let terminal = String::from_utf8(prepared.terminal("unicode.vsp")?)?;
    assert!(terminal.contains("E0300"));
    assert!(terminal.contains("binding declared here"));
    assert!(terminal.contains("Consume each linear value exactly once."));
    assert!(!terminal.contains('\x1b'));
    let end = source.len();
    let eof = [Diagnostic::new("E0001", "expected item", end..end)];
    let prepared = prepare(&index, &eof, Limits::default())?;
    let out = String::from_utf8(prepared.terminal("unicode.vsp")?)?;
    assert!(out.contains("unicode.vsp:3:1"), "{out}");
    println!("{terminal}bounded terminal/LSP causal diagnostics: ok");
    Ok(())
}
