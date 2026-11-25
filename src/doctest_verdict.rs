//! The decisions `vesper test --doc` makes about one file's examples without
//! running them, behind a swap point (self-hosting SH5 lane 14, #1337):
//! the private wrapper names of the examples, and per example where its
//! diagnostics point in the documented file and whether checking alone
//! settles it (a `compile_fail` fence matched or not, a positive example
//! that failed checking or whose entry the snippet does not define).
//!
//! The Rust implementation (moved here from `prepare` of src/doctest.rs) is
//! the default [`DocTestVerdictTable`]. `@vesper/test-runner`
//! (packages/vesper-test-runner/src/verdict.ves) decides the same;
//! tests/self_host_doctest_verdict.rs runs both and requires equal results.

/// The names one example adds to the reserved set: its `entry=` and the
/// identifier tokens of its code.
#[derive(Clone, Copy, Debug)]
pub struct ExampleNames<'a> {
    pub entry: Option<&'a str>,
    /// The example's code lines.
    pub lines: &'a [&'a str],
}

/// One checker diagnostic of a generated module.
#[derive(Clone, Copy, Debug)]
pub struct CheckError<'a> {
    pub code: &'a str,
    pub message: &'a str,
    pub start: usize,
    pub end: usize,
}

/// What [`verdict`] needs about one checked example.
#[derive(Clone, Copy, Debug)]
pub struct VerdictInput<'a> {
    /// The documented file.
    pub source: &'a str,
    /// The byte offset of the fence's opening line.
    pub offset: usize,
    /// Each code line's offset in `source` and length.
    pub lines: &'a [(usize, usize)],
    /// The generated module: where the snippet and its code start, and
    /// each code line's range there.
    pub snippet_start: usize,
    pub code_start: usize,
    pub segments: &'a [(usize, usize)],
    /// The checker's diagnostics of the generated module.
    pub errors: &'a [CheckError<'a>],
    /// The `compile_fail(...)` code.
    pub expected: Option<&'a str>,
    /// Whether the snippet itself defines the entry function.
    pub own_entry: bool,
}

/// A diagnostic placed in the documented file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Located {
    pub code: String,
    pub message: String,
    pub byte_start: usize,
    pub byte_end: usize,
    pub line: usize,
    pub column: usize,
}

/// The example's position, its placed diagnostics and, when checking
/// settles it, whether it passed and why (`None`: it runs).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verdict {
    pub line: usize,
    pub column: usize,
    pub diagnostics: Vec<Located>,
    pub decided: Option<(bool, String)>,
}

/// The signature of [`wrappers`].
pub type WrappersFn = fn(&[&str], &[ExampleNames<'_>]) -> Result<Vec<String>, String>;

/// The swappable functions.
#[derive(Clone, Copy)]
pub struct DocTestVerdictTable {
    /// [`wrappers`].
    pub wrappers: WrappersFn,
    /// [`verdict`].
    pub verdict: fn(VerdictInput<'_>) -> Verdict,
}

/// The Rust implementation, the default [`DocTestVerdictTable`].
pub const RUST_DOCTEST_VERDICT: DocTestVerdictTable = DocTestVerdictTable {
    wrappers: rust_wrappers,
    verdict: rust_verdict,
};

static TABLE: std::sync::OnceLock<DocTestVerdictTable> = std::sync::OnceLock::new();

/// Replace [`RUST_DOCTEST_VERDICT`] for the rest of the process. Only the
/// first call takes effect; a later one returns its table back.
pub fn install_doctest_verdict(table: DocTestVerdictTable) -> Result<(), DocTestVerdictTable> {
    TABLE.set(table)
}

fn table() -> &'static DocTestVerdictTable {
    TABLE.get().unwrap_or(&RUST_DOCTEST_VERDICT)
}

/// The private wrapper name of each example: `__vesper_doctest_{index}`,
/// lengthened with `_` until it collides with no base function, no
/// `entry=`, no identifier of any example and no earlier wrapper.
pub fn wrappers(functions: &[&str], examples: &[ExampleNames<'_>]) -> Result<Vec<String>, String> {
    (table().wrappers)(functions, examples)
}

/// The verdict of one checked example.
pub fn verdict(input: VerdictInput<'_>) -> Verdict {
    (table().verdict)(input)
}

/// The 1-based line and column (in characters) of byte `offset`.
pub fn location(source: &str, offset: usize) -> (usize, usize) {
    let offset = offset.min(source.len());
    let prefix = &source[..source.floor_char_boundary(offset)];
    (
        prefix.bytes().filter(|b| *b == b'\n').count() + 1,
        prefix.rsplit('\n').next().unwrap_or("").chars().count() + 1,
    )
}

/// The Rust implementation of [`wrappers`].
pub fn rust_wrappers(
    functions: &[&str],
    examples: &[ExampleNames<'_>],
) -> Result<Vec<String>, String> {
    use crate::lexer::{self, TokenKind as K};
    let mut reserved: std::collections::BTreeSet<String> =
        functions.iter().map(|f| f.to_string()).collect();
    // Private wrappers must not collide with identifiers authored in any other
    // example either; all snippet bytes are already bounded by extraction.
    for example in examples {
        if let Some(entry) = example.entry {
            reserved.insert(entry.to_string());
        }
        let raw: String = example.lines.iter().map(|l| format!("{l}\n")).collect();
        for token in lexer::lex(&raw).0 {
            if matches!(token.kind, K::Lower | K::Upper) {
                reserved.insert(raw[token.span].to_string());
            }
        }
    }
    let mut out = Vec::with_capacity(examples.len());
    for index in 0..examples.len() {
        let mut wrapper = format!("__vesper_doctest_{index}");
        while !reserved.insert(wrapper.clone()) {
            wrapper.push('_');
            if wrapper.len() > 128 {
                return Err("doctest private wrapper name limit".into());
            }
        }
        out.push(wrapper);
    }
    Ok(out)
}

/// Where generated offset `offset` lies in the documented file: on a code
/// line, its place there; before the code, the first line; after it, the
/// end of the last line.
fn map_offset(input: &VerdictInput<'_>, offset: usize) -> usize {
    for (&(start, end), &(original, _)) in input.segments.iter().zip(input.lines) {
        if offset >= start && offset <= end {
            return original + offset - start;
        }
    }
    if offset < input.code_start {
        input.lines.first().map_or(input.offset, |l| l.0)
    } else {
        input.lines.last().map_or(input.offset, |l| l.0 + l.1)
    }
}

/// The Rust implementation of [`verdict`].
pub fn rust_verdict(input: VerdictInput<'_>) -> Verdict {
    let (line, column) = location(input.source, input.offset);
    let local_only = input.errors.iter().all(|d| {
        input
            .segments
            .iter()
            .any(|(start, end)| d.start >= *start && d.start <= *end)
    });
    let placed = |d: &CheckError<'_>, start: usize, end: usize| {
        let (line, column) = location(input.source, start);
        Located {
            code: d.code.to_string(),
            message: d.message.to_string(),
            byte_start: start,
            byte_end: end,
            line,
            column,
        }
    };
    let diagnostics = input
        .errors
        .iter()
        .map(|d| {
            if d.start < input.snippet_start {
                placed(d, d.start, d.end)
            } else {
                placed(d, map_offset(&input, d.start), map_offset(&input, d.end))
            }
        })
        .collect();
    let decided = if let Some(expected) = input.expected {
        let passed = !input.errors.is_empty()
            && local_only
            && input.errors.iter().all(|d| d.code == expected);
        Some((
            passed,
            if passed {
                "matched exact diagnostic code"
            } else {
                "expected exact snippet diagnostic code; unrelated/base errors cannot satisfy compile_fail"
            }
            .to_string(),
        ))
    } else if !input.errors.is_empty() {
        Some((false, "positive example failed full checking".to_string()))
    } else if !input.own_entry {
        Some((
            false,
            "positive entry must be defined by this snippet, not inherited from the base package"
                .to_string(),
        ))
    } else {
        None
    };
    Verdict {
        line,
        column,
        diagnostics,
        decided,
    }
}
