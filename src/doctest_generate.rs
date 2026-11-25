//! The generated module of one doctest example (the snippet appended to the
//! file's base source, wrapped in a function unless it declares items)
//! behind a swap point (self-hosting SH5 lane 14, #1337).
//!
//! The Rust implementation (moved here from `generate` of src/doctest.rs;
//! the check-byte budget and the token preflight stay there) is the
//! default [`DocTestGenerateTable`]. `@vesper/test-runner`
//! (packages/vesper-test-runner/src/generate.ves) builds the same module;
//! tests/self_host_doctest_generate.rs runs both on the repository's
//! corpus and requires equal results.

use crate::lexer::{self, TokenKind as K};
use crate::parser;

/// One example as [`generate`] needs it.
#[derive(Clone, Copy, Debug)]
pub struct GenerateInput<'a> {
    /// The file's source without its doctest fences' examples.
    pub base: &'a str,
    /// The example's code lines.
    pub lines: &'a [&'a str],
    /// The name of the wrapper function of a body example.
    pub wrapper: &'a str,
    /// `entry=name`.
    pub entry: Option<&'a str>,
    /// Whether the fence is `compile_fail(...)`.
    pub expected: bool,
    /// The device effects of a `sim(...)` fence.
    pub sim_effects: Option<&'a [&'a str]>,
}

/// The generated module: the source, where the snippet and its code start,
/// each code line's range in the source, and the entry function.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Generated {
    pub source: String,
    pub snippet_start: usize,
    pub code_start: usize,
    pub segments: Vec<(usize, usize)>,
    pub entry: Option<String>,
}

/// The swappable function.
#[derive(Clone, Copy)]
pub struct DocTestGenerateTable {
    /// [`generate`].
    pub generate: fn(GenerateInput<'_>) -> Generated,
}

/// The Rust implementation, the default [`DocTestGenerateTable`].
pub const RUST_DOCTEST_GENERATE: DocTestGenerateTable = DocTestGenerateTable {
    generate: rust_generate,
};

static TABLE: std::sync::OnceLock<DocTestGenerateTable> = std::sync::OnceLock::new();

/// Replace [`RUST_DOCTEST_GENERATE`] for the rest of the process. Only the
/// first call takes effect; a later one returns its table back.
pub fn install_doctest_generate(table: DocTestGenerateTable) -> Result<(), DocTestGenerateTable> {
    TABLE.set(table)
}

/// The generated module of `input`.
pub fn generate(input: GenerateInput<'_>) -> Generated {
    (TABLE.get().unwrap_or(&RUST_DOCTEST_GENERATE).generate)(input)
}

/// Whether the snippet declares items: it parses as a module with items
/// or model uses, or its first token starts a declaration (so malformed
/// declarations and contextual trait/model/pub never acquire wrapper
/// errors).
pub fn declaration(raw: &str) -> bool {
    let (tokens, _) = lexer::lex(raw);
    let raw_module = parser::parse(raw);
    raw_module.diagnostics.is_empty()
        && (!raw_module.module.items.is_empty() || !raw_module.module.model_uses.is_empty())
        || tokens
            .iter()
            .find(|t| {
                !matches!(
                    t.kind,
                    K::Whitespace | K::Comment | K::BlockComment | K::Eof
                )
            })
            .is_some_and(|t| {
                matches!(
                    t.kind,
                    K::Fn
                        | K::Copy
                        | K::Affine
                        | K::Linear
                        | K::Struct
                        | K::Enum
                        | K::Effect
                        | K::Impl
                ) || t.kind == K::Lower && matches!(&raw[t.span.clone()], "trait" | "model" | "pub")
            })
}

/// The Rust implementation of [`generate`].
pub fn rust_generate(input: GenerateInput<'_>) -> Generated {
    let mut source = format!("{}\n", input.base);
    let snippet_start = source.len();
    let raw: String = input.lines.iter().map(|l| format!("{l}\n")).collect();
    let declaration = declaration(&raw);
    let entry = if input.expected {
        None
    } else if declaration {
        Some(input.entry.unwrap_or("main").to_string())
    } else {
        Some(input.entry.unwrap_or(input.wrapper).to_string())
    };
    if !declaration {
        // A simulated body fence may use exactly the devices it declared.
        let row = input.sim_effects.map_or(String::new(), |names| {
            format!(" eff [{}]", names.join(", "))
        });
        source.push_str(&format!(
            "fn {}(){row} {{\n",
            entry.as_deref().unwrap_or(input.wrapper)
        ));
    }
    let code_start = source.len();
    let mut segments = Vec::new();
    for line in input.lines {
        let start = source.len();
        source.push_str(line);
        segments.push((start, source.len()));
        source.push('\n');
    }
    if !declaration {
        source.push_str("}\n");
    }
    Generated {
        source,
        snippet_start,
        code_start,
        segments,
        entry,
    }
}
