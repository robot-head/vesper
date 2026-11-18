#![allow(
    dead_code,
    reason = "shared helpers are used by different acceptance targets"
)]
//! Self-hosting Stage 1 (design §42.3): the Vesper-source lexer and parser in
//! `packages/vesper-compiler` run on the Stage 0 compiler (the reference
//! evaluator and the Cranelift JIT) over the repository's own `.ves` sources,
//! and their token streams and CST shapes are compared with `vesper-syntax`,
//! the Rust seed.
//!
//! The structured parser writes indexed token/node/diagnostic records through
//! a bounded memory-only adapter. Tests compare every record with the seed.
//! Scalar compatibility digests and the frozen five-file Stage 1 entry points
//! are checked separately. Errors return `-1 - (code << 32 | offset)`.
use std::path::{Path, PathBuf};
use vesper::parser::{self as seed, SyntaxNode};
use vesper::{check, lexer, native, parser, runtime::Runtime, runtime::Value};

/// The package sources, in compilation order (see its Loom.toml).
pub(super) const FRONT_END: [&str; 5] = [
    "packages/vesper-compiler/src/lexer.ves",
    "packages/vesper-compiler/src/cursor.ves",
    "packages/vesper-compiler/src/types_items.ves",
    "packages/vesper-compiler/src/expressions.ves",
    "packages/vesper-compiler/src/driver.ves",
];
pub(super) const CODE_LEX: i64 = 1;
pub(super) const CODE_EXPECTED: i64 = 2;
pub(super) const CODE_OVERFLOW: i64 = 3;
pub(super) const CODE_INVALID: i64 = 4;
pub(super) const CODE_LIMIT: i64 = 5;

pub(super) fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

pub(super) fn mix(h: i64, x: i64) -> i64 {
    let m = h.wrapping_add(x).wrapping_mul(0x100000001b3);
    m.wrapping_add(m / 2147483648)
}

pub(super) fn digest(h: i64) -> i64 {
    if h < 0 { (-1i64).wrapping_sub(h) } else { h }
}

pub(super) const OFFSET_BASIS: i64 = 0xcbf29ce484222325_u64 as i64;

/// The seed's token-stream digest, or None when the seed reports a lexical
/// diagnostic or limit (the Vesper lexer must then fail).
pub(super) fn seed_tokens(source: &str) -> Option<i64> {
    let lexed = lexer::lex_with_limits(source, lexer::LexLimits::default()).ok()?;
    if !lexed.diagnostics.is_empty() || lexed.limit.is_some() {
        return None;
    }
    let mut h = OFFSET_BASIS;
    for token in &lexed.tokens {
        h = mix(h, token.kind as u16 as i64);
        h = mix(h, token.span.start as i64);
        h = mix(h, token.span.end as i64);
    }
    Some(digest(mix(h, lexed.tokens.len() as i64)))
}

pub(super) fn cst_events(node: &SyntaxNode, h: &mut i64) {
    let range = node.text_range();
    *h = mix(*h, node.kind().0 as i64 + 1000);
    *h = mix(*h, u32::from(range.start()) as i64);
    for child in node.children() {
        cst_events(&child, h);
    }
    *h = mix(*h, 3000);
    *h = mix(*h, u32::from(range.end()) as i64);
}

/// The seed's CST digest, or None when the seed reports any diagnostic.
pub(super) fn seed_cst(source: &str) -> Option<i64> {
    let parsed = seed::parse_with_limits(source, seed::ParseLimits::default()).ok()?;
    if !parsed.diagnostics.is_empty() {
        return None;
    }
    let mut h = OFFSET_BASIS;
    cst_events(&parsed.syntax(), &mut h);
    Some(digest(h))
}

/// Compare all records, including trivia ownership, without a digest.
pub(super) fn assert_flat(source: &str, cells: &[i64]) {
    let parsed = seed::parse(source);
    if !parsed.diagnostics.is_empty() {
        return;
    }
    fn walk(
        node: &SyntaxNode,
        parent: i64,
        nodes: &mut Vec<[i64; 4]>,
        owners: &mut std::collections::BTreeMap<(i64, i64), i64>,
    ) {
        let id = nodes.len() as i64;
        let range = node.text_range();
        nodes.push([
            node.kind().0 as i64,
            u32::from(range.start()) as i64,
            u32::from(range.end()) as i64,
            parent,
        ]);
        for child in node.children_with_tokens() {
            if let Some(node) = child.as_node() {
                walk(node, id + 1, nodes, owners);
            } else if let Some(token) = child.as_token() {
                let range = token.text_range();
                owners.insert(
                    (
                        u32::from(range.start()) as i64,
                        u32::from(range.end()) as i64,
                    ),
                    id + 1,
                );
            }
        }
    }
    let mut nodes = vec![];
    let mut owners = std::collections::BTreeMap::new();
    walk(&parsed.syntax(), 0, &mut nodes, &mut owners);
    assert_eq!(cells[1], nodes.len() as i64, "node count: {source}");
    for (i, node) in nodes.iter().enumerate() {
        assert_eq!(
            &cells[270000 + i * 4..270000 + i * 4 + 4],
            node,
            "node {i}: {source}"
        );
    }
    let (tokens, diagnostics) = lexer::lex(source);
    assert!(diagnostics.is_empty());
    assert_eq!(cells[0], tokens.len() as i64, "token count");
    for (i, token) in tokens.iter().enumerate() {
        let start = token.span.start as i64;
        let end = token.span.end as i64;
        let record = [
            token.kind as u16 as i64,
            start,
            end,
            owners.get(&(start, end)).copied().unwrap_or(0),
        ];
        assert_eq!(
            &cells[16 + i * 4..16 + i * 4 + 4],
            &record,
            "token {i}: {source}"
        );
    }
}

/// The source as Vesper code. Leaves hold 64 bytes in a tuple indexed
/// dynamically; internal functions dispatch over at most 8 children. A single
/// comparison tree over every byte would be correct too, but the native
/// backend's CLIF grows quadratically with the nesting of one function's
/// `if` blocks (4.3 million CLIF lines for 593 leaves), so the tree is split
/// into small functions.
pub(super) fn source_module(bytes: &[u8]) -> String {
    const LEAF: usize = 64;
    const FAN: usize = 8;
    const EFFECTS: &str = "eff [Fail[RuntimeError], Fail[ArithOverflow]]";
    let mut out = String::from("\n");
    let mut nodes: Vec<(String, usize)> = vec![];
    for (k, chunk) in bytes.chunks(LEAF).enumerate() {
        let start = k * LEAF;
        let mut values: Vec<String> = chunk.iter().map(u8::to_string).collect();
        values.resize(LEAF, "0".into());
        let name = format!("source_leaf_{k}");
        out.push_str(&format!(
            "fn {name}(i: i64) -> i64 {EFFECTS} {{ ({})[i - {start}] }}\n",
            values.join(",")
        ));
        nodes.push((name, start));
    }
    fn tree(group: &[(String, usize)], out: &mut String) {
        if group.len() == 1 {
            out.push_str(&format!("{}(i)", group[0].0));
            return;
        }
        let mid = group.len() / 2;
        out.push_str(&format!("if i < {} {{ ", group[mid].1));
        tree(&group[..mid], out);
        out.push_str(" } else { ");
        tree(&group[mid..], out);
        out.push_str(" }");
    }
    let mut level = 0;
    while nodes.len() > 1 {
        level += 1;
        let mut next = vec![];
        for (g, group) in nodes.chunks(FAN).enumerate() {
            let name = format!("source_node_{level}_{g}");
            let mut body = String::new();
            tree(group, &mut body);
            out.push_str(&format!(
                "fn {name}(i: i64) -> i64 {EFFECTS} {{ {body} }}\n"
            ));
            next.push((name, group[0].1));
        }
        nodes = next;
    }
    let n = bytes.len();
    let top = nodes
        .first()
        .map_or_else(|| "-1".to_owned(), |(name, _)| format!("{name}(i)"));
    out.push_str(&format!(
        "fn source_len() -> i64 {{ {n} }}\n\
         fn source_byte(i: i64) -> i64 {{ if i < 0 || i >= {n} {{ -1 }} else {{ \
         handle Fail[ArithOverflow] in {{ handle Fail[RuntimeError] in {{ {top} }} with {{ \
         return v => v, raise(e, r) => -1, }} }} with {{ return v => v, raise(e, r) => -1, }} }} }}\n"
    ));
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Outcome {
    Digest(i64),
    Error { code: i64, at: i64 },
}

pub(super) fn decode(value: &Value) -> Outcome {
    let Value::Int(v) = value else {
        panic!("entry returned {value:?}");
    };
    if *v >= 0 {
        Outcome::Digest(*v)
    } else {
        let e = -1 - *v;
        Outcome::Error {
            code: e >> 32,
            at: e & 0xffff_ffff,
        }
    }
}

pub(super) struct Program {
    pub(super) world: check::World,
}

pub(super) fn program(front_end: &str, bytes: &[u8]) -> Program {
    let text = format!("{front_end}{}", source_module(bytes));
    let limits = parser::ParseLimits {
        max_source_bytes: lexer::HARD_SOURCE_BYTES,
        max_tokens: lexer::HARD_TOKENS,
        ..Default::default()
    };
    let parsed = parser::parse_with_limits(&text, limits).expect("program limits");
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let checked = check::check(&parsed.module);
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
    Program {
        world: checked.world,
    }
}

pub(super) fn evaluate(program: &Program, entry: &str) -> Outcome {
    if entry == "parse_tree_main" {
        let (value, _) = transport(program);
        return decode(&value);
    }
    let mut runtime = Runtime::new(&program.world);
    runtime.fuel = u64::MAX / 2;
    runtime.optimize = false;
    decode(&runtime.run(entry, vec![]).expect(entry))
}

/// A bounded memory adapter, with no parsing or seed logic. The same
/// SyntaxMemory interface is closed by syntax_run in the Vesper package.
pub(super) fn transport(program: &Program) -> (Value, Vec<i64>) {
    use vesper::runtime::Execution;
    let mut runtime = Runtime::new(&program.world);
    runtime.fuel = u64::MAX / 2;
    runtime.optimize = false;
    let mut execution = runtime.begin_external("parse_transport", vec![]).unwrap();
    let mut cells = vec![0_i64; 1 << 20];
    loop {
        match execution {
            Execution::Complete(value) => return (value, cells),
            Execution::Suspended(pending) => {
                let request = pending.request();
                assert_eq!(request.effect.name, "SyntaxMemory");
                let Value::Int(index) = request.arguments[0] else {
                    panic!("memory index")
                };
                let index = usize::try_from(index).expect("non-negative memory index");
                assert!(index < cells.len(), "memory address {index}");
                let answer = match request.operation.as_str() {
                    "read" => Value::Int(cells[index]),
                    "write" => {
                        let Value::Int(value) = request.arguments[1] else {
                            panic!("memory value")
                        };
                        cells[index] = value;
                        Value::Tuple(vec![])
                    }
                    other => panic!("unexpected memory operation {other}"),
                };
                execution = pending.resume(answer).unwrap();
            }
        }
    }
}

pub(super) fn jit(program: &Program, entry: &str) -> Outcome {
    decode(&native::jit_with_options(&program.world, entry, false).expect(entry))
}

/// Every `.ves` source of the root package (examples, test fixtures and
/// this package), sorted.
pub(super) fn fixtures() -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                // Syntax the self-hosted parser does not have yet: `use` imports
                // (lane F0, #873; follow-up #880) and slice intervals `p[a..b]`
                // (lane F1, #874; follow-up #887, also used by the string fixtures of
                // lane F2, #896). These fixtures and the @vesper-std packages (#897)
                // join the corpus once packages/vesper-compiler parses them.
                // Each port's Loom package (`crates/<crate>/vesper`, port wave P1)
                // imports with `use` too, so ports list it here, one line each.
                if path.ends_with("tests/fixtures/packages")
                    || path.ends_with("tests/fixtures/seq")
                    || path.ends_with("tests/fixtures/strings")
                    || path.ends_with("packages/vesper-std")
                    // The Quarry port (#904) uses `use`-free syntax the
                    // Stage 1 parser lacks (#880); it joins with the others.
                    || path.ends_with("crates/vesper-quarry/vesper")
                    || path.ends_with("crates/vesper-port-build/vesper")
                    || path.ends_with("crates/vesper-beacon/vesper")
                    // Port P1 lane 16 (#913): `use` (#880).
                    || path.ends_with("crates/vesper-loom-calculus/vesper")
                    // Port P1 lane 17 (#914): `use` (#880).
                    || path.ends_with("crates/vesper-loom-solver/vesper")
                    // Port P3a lane 3 (#1038): `use` (#880).
                    || path.ends_with("crates/vesper-pact/vesper")
                    // Port P3a lane 4 (#1039): `use` (#880).
                    || path.ends_with("crates/vesper-loom-audit/vesper")
                    || path.ends_with("crates/vesper-prism/vesper")
                    || path.ends_with("crates/vesper-aura/vesper")
                    // The Grammar port (#906) is one package of modules
                    // that do not check on their own; joins with #880.
                    || path.ends_with("crates/vesper-grammar/vesper")
                    // Port P1 lane 10 (#907): parts of one compilation unit,
                    // which do not check on their own; joins with #880.
                    || path.ends_with("crates/vesper-warp-bbr/vesper")
                    || path.ends_with("crates/vesper-warp-tcp/vesper")
                    // The warp-wheel port's files form one library (#900).
                    || path.ends_with("crates/vesper-warp-wheel/vesper")
                    // Port P1 lane 12 (#909): its fragments need `use` (#880).
                    || path.ends_with("crates/vesper-ignis/vesper")
                    // Port P1 lane 19 (#916): the serde port uses `use` (#880).
                    || path.ends_with("crates/vesper-serde/vesper")
                    // Port P1 lane 15 (#912): the semver port uses `use` (#880).
                    || path.ends_with("crates/vesper-semver/vesper")
                    // Port P1 lane 8 (#905): the Tensor port uses `use` (#880).
                    || path.ends_with("crates/vesper-tensor/vesper")
                    // Port P1 lane 6 (#903): the Strata port, a Loom package
                    // like the others; joins with them once #880 lands.
                    || path.ends_with("crates/vesper-alloc/vesper")
                    // Port P1 lane 5 (#902): its package needs `use` (#880).
                    || path.ends_with("crates/vesper-simpulse/vesper")
                    // Port P1 lane 18 (#915): a Loom package like the others;
                    // joins with them once #880 lands.
                    || path.ends_with("crates/vesper-crucible/vesper")
                    // Port P3a lane 6 (#1041): the eBPF port uses `use` (#880).
                    || path.ends_with("crates/vesper-ebpf/vesper")
                {
                    continue;
                }
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "ves") {
                out.push(path);
            }
        }
    }
    let mut out = vec![];
    for dir in [
        "bootstrap",
        "crates",
        "examples",
        "packages",
        "src",
        "tests/fixtures",
    ] {
        walk(&root().join(dir), &mut out);
    }
    // Loops, early exits and `let mut` (lane F6, #876) are not in the
    // self-hosted parser yet; such sources join the corpus once
    // packages/vesper-compiler parses them.
    out.retain(|path| {
        std::fs::read_to_string(path).map_or(true, |source| {
            !vesper::loops::uses_loop_syntax(&vesper::parser::parse(&source).module)
        })
    });
    out.sort();
    out
}

pub(super) fn read_front_end() -> String {
    let mut files: Vec<_> = std::fs::read_dir(root().join("packages/vesper-compiler/src"))
        .unwrap()
        .map(|f| f.unwrap().path())
        .filter(|f| f.extension().is_some_and(|ext| ext == "ves"))
        .filter(|f| {
            let name = f.file_name().unwrap().to_str().unwrap();
            FRONT_END.iter().any(|own| own.ends_with(name))
                || name.starts_with("parse_")
                || name.starts_with("syntax_")
        })
        .collect();
    files.sort();
    files
        .iter()
        .map(|f| std::fs::read_to_string(f).unwrap())
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) fn on_big_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(512 << 20)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap()
}
