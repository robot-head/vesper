//! The Stage 1 checker runs entirely in Vesper. Rust only supplies source
//! bytes, invokes the evaluator/JIT, and compares against the seed oracle.
use std::path::{Path, PathBuf};
use vesper::{
    check, lexer, native, parser,
    runtime::{Runtime, Value},
};

const SOURCES: &[&str] = &[
    "lexer",
    "cursor",
    "types_items",
    "expressions",
    "driver",
    "ast_core",
    "resolve_core",
    "check_modes",
    "check_adt",
    "check_expr",
    "check_blocks",
    "check_driver",
];
fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}
struct Checker {
    template: check::World,
    source: String,
}
fn checker() -> Checker {
    let source = SOURCES
        .iter()
        .map(|f| {
            std::fs::read_to_string(root().join(format!("packages/vesper-compiler/src/{f}.ves")))
                .unwrap()
        })
        .collect::<Vec<_>>()
        .join("\n");
    let mut template = checked(&format!("{source}{}", source_module(&[])));
    // Only generated input-provider metadata is replaced per subject. Check
    // the Vesper compiler itself once, and retain its checked AST unchanged.
    let boundary = source.len();
    template.types.retain(|(start, _), _| *start < boundary);
    template.moves.retain(|(start, _), _| *start < boundary);
    template.calls.retain(|(start, _), _| *start < boundary);
    template
        .handler_effects
        .retain(|(start, _), _| *start < boundary);
    template.partial_ops.retain(|(start, _)| *start < boundary);
    template.overflow_ops.retain(|(start, _)| *start < boundary);
    Checker { template, source }
}
#[derive(Debug, PartialEq, Eq)]
struct Diagnostic {
    code: String,
    start: usize,
    end: usize,
}
fn decode(value: Value) -> Option<Diagnostic> {
    let Value::Int(value) = value else {
        panic!("checker returned {value:?}")
    };
    if value == 0 {
        return None;
    }
    assert!(value < 0, "invalid checker outcome {value}");
    let error = -1 - value;
    Some(Diagnostic {
        code: format!("E{:04}", error >> 32),
        start: ((error >> 16) & 0xffff) as usize,
        end: (error & 0xffff) as usize,
    })
}
fn checked(program: &str) -> check::World {
    let parsed = parser::parse_with_limits(
        program,
        parser::ParseLimits {
            max_source_bytes: lexer::HARD_SOURCE_BYTES,
            max_tokens: lexer::HARD_TOKENS,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let checked = check::check(&parsed.module);
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
    checked.world
}
fn world(checker: &Checker, source: &str) -> check::World {
    // Disjoint spans preserve all seed metadata. The generated functions
    // contain source bytes only and cannot call or supply checker decisions.
    let provider = checked(&format!(
        "{}{}",
        " ".repeat(checker.source.len()),
        source_module(source.as_bytes())
    ));
    let mut world = checker.template.clone();
    world.functions.extend(provider.functions);
    world.types.extend(provider.types);
    world.moves.extend(provider.moves);
    world.calls.extend(provider.calls);
    world.function_results.extend(provider.function_results);
    world.handler_effects.extend(provider.handler_effects);
    world.partial_ops.extend(provider.partial_ops);
    world.overflow_ops.extend(provider.overflow_ops);
    world
}
fn evaluate(world: &check::World) -> Option<Diagnostic> {
    let mut runtime = Runtime::new(world);
    runtime.fuel = 100_000_000;
    runtime.optimize = false;
    decode(
        runtime
            .run("check_main", vec![])
            .expect("self-host checker execution"),
    )
}
fn oracle(source: &str) -> Option<Diagnostic> {
    let parsed = parser::parse(source);
    let diagnostics = if parsed.diagnostics.is_empty() {
        check::check(&parsed.module).diagnostics
    } else {
        parsed.diagnostics
    };
    diagnostics.first().map(|d| Diagnostic {
        code: d.code.clone(),
        start: d.span.start,
        end: d.span.end,
    })
}
fn supported(d: &Option<Diagnostic>) -> bool {
    d.as_ref().is_none_or(|d| d.code.as_str() < "E9000")
}
fn on_stack(f: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap();
}
fn source_module(bytes: &[u8]) -> String {
    const LEAF: usize = 16;
    const FAN: usize = 8;
    const EFFECTS: &str = "eff [Fail[RuntimeError]]";
    let mut out = String::from("\n");
    let mut nodes: Vec<(String, usize)> = vec![];
    for (k, chunk) in bytes.chunks(LEAF).enumerate() {
        let start = k * LEAF;
        let mut values: Vec<String> = chunk.iter().map(u8::to_string).collect();
        values.resize(LEAF, "0".into());
        let name = format!("source_leaf_{k}");
        out.push_str(&format!(
            "fn {name}(i: i64) -> i64 {EFFECTS} {{ ({})[wrapping_sub(i, {start})] }}\n",
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
         handle Fail[RuntimeError] in {{ {top} }} with {{ \
         return v => v, raise(e, r) => -1, }} }} }}\n"
    ));
    out
}

fn expected_code(name: &str) -> Option<&'static str> {
    match name {
        "fail_function_mode_last" => Some("E0400"),
        "fail_function_scalar_last" => Some("E0400"),

        "callee_affine_in_arg" => Some("E0303"),
        "match_function_row" => Some("E0401"),
        "match_function_row_reverse" => Some("E0401"),
        "if_function_row" => Some("E0401"),
        "fail_function_mode" => Some("E0400"),
        "reserved_unit" => Some("E0100"),
        "reserved_option" => Some("E0100"),
        "reserved_result" => Some("E0100"),
        "reserved_runtime_error" => Some("E0100"),
        "reserved_arith_overflow" => Some("E0100"),
        "reserved_string" => Some("E0100"),
        "reserved_never" => Some("E0100"),
        "reserved_intrinsic" => Some("E0100"),

        "abort_frame" => Some("E0402"),
        "affine_twice" => Some("E0303"),
        "arity" => Some("E0207"),
        "borrowed_move" => Some("E0305"),
        "branch_linear" => Some("E0309"),
        "constructor_arguments" => Some("E0203"),
        "duplicate_fields" => Some("E0100"),
        "duplicate_functions" => Some("E0100"),
        "duplicate_parameters" => Some("E0100"),
        "duplicate_variants" => Some("E0100"),
        "effect_missing" => Some("E0401"),
        "field_mode" => Some("E0105"),
        "explicit_linear_field_drop" => Some("E0302"),
        "field_mode_dag" => Some("E0105"),
        "function_linear_drop" => Some("E0302"),
        "function_linear_twice" => Some("E0303"),
        "function_mode_floor" => Some("E0105"),
        "inout_abort" => Some("E0402"),
        "inout_overlap" => Some("E0301"),
        "inout_temporary" => Some("E0300"),
        "let_annotation" => Some("E0200"),
        "linear_drop" => Some("E0302"),
        "linear_temporary" => Some("E0302"),
        "linear_twice" => Some("E0303"),
        "missing_fields" => Some("E0204"),
        "nested_loans" => Some("E0301"),
        "nominal_join" => Some("E0302"),
        "non_callable" => Some("E0206"),
        "non_exhaustive" => Some("E0212"),
        "pass_mismatch" => Some("E0308"),
        "row_type_mismatch" => Some("E0400"),
        "inout_type" => Some("E0403"),
        "inout_unknown_temporary" => Some("E0300"),
        "abort_argument_loan" => Some("E0402"),
        "scope_escape" => Some("E0201"),
        "shadow_linear" => Some("E0302"),
        "shared_mutation" => Some("E0307"),
        "unknown_name" => Some("E0201"),
        "unknown_type" => Some("E0101"),
        "unknown_function_effect_type" => Some("E0101"),
        "wrong_type" => Some("E0200"),
        "comparison_order_type" => Some("E0200"),
        _ => None,
    }
}
#[test]
fn acceptance() {
    on_stack(|| {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        let checker = Arc::new(checker());
        let mut paths: Vec<_> = std::fs::read_dir(root().join("tests/fixtures/self_host_check"))
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|e| e == "ves"))
            .collect();
        paths.sort();
        let paths = Arc::new(paths);
        let cursor = Arc::new(AtomicUsize::new(0));
        let mut workers = vec![];
        for _ in 0..4 {
            let checker = Arc::clone(&checker);
            let paths = Arc::clone(&paths);
            let cursor = Arc::clone(&cursor);
            workers.push(
                std::thread::Builder::new()
                    .stack_size(64 << 20)
                    .spawn(move || {
                        let mut errors = vec![];
                        loop {
                            let i = cursor.fetch_add(1, Ordering::Relaxed);
                            let Some(path) = paths.get(i) else { break };
                            let source = std::fs::read_to_string(path).unwrap();
                            let actual = evaluate(&world(&checker, &source));
                            let expected = oracle(&source);
                            eprintln!("{}: {actual:?}", path.display());
                            if actual != expected
                                || actual.as_ref().map(|d| d.code.as_str())
                                    != expected_code(path.file_stem().unwrap().to_str().unwrap())
                            {
                                errors.push(format!(
                                    "{}: actual {actual:?}, expected {expected:?}",
                                    path.display()
                                ));
                            }
                        }
                        errors
                    })
                    .unwrap(),
            );
        }
        let errors = workers
            .into_iter()
            .flat_map(|w| w.join().unwrap())
            .collect::<Vec<_>>();
        assert!(errors.is_empty(), "{}", errors.join("\n"));
    });
}
#[test]
fn native_agrees() {
    on_stack(|| {
        let checker = checker();
        for name in [
            "linear_drop",
            "scopes",
            "branch_success",
            "callee_affine_in_arg",
            "match_function_row",
        ] {
            let source = std::fs::read_to_string(
                root().join(format!("tests/fixtures/self_host_check/{name}.ves")),
            )
            .unwrap();
            let world = world(&checker, &source);
            let whole = checked(&format!(
                "{}{}",
                checker.source,
                source_module(source.as_bytes())
            ));
            assert_eq!(evaluate(&world), evaluate(&whole), "cached assembly {name}");
            assert_eq!(
                decode(native::jit_with_options(&world, "check_main", false).unwrap()),
                oracle(&source),
                "{name}"
            );
        }
    });
}
fn walk(dir: &Path, paths: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        // `use` imports (lane F0, #873; follow-up #880) are not in the self-hosted front end
        // yet; package fixtures join this corpus once it parses them.
        if path.ends_with("tests/fixtures/packages") {
            continue;
        }
        if path.is_dir() {
            walk(&path, paths)
        } else if path.extension().is_some_and(|e| e == "ves") {
            paths.push(path)
        }
    }
}
#[test]
fn repository_differential() {
    on_stack(|| {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        let checker = Arc::new(checker());
        let mut paths = vec![];
        // Scan every repository source. In Bazel this walks the declared runfiles.
        for dir in [
            "examples",
            "tests/fixtures",
            "packages",
            "bootstrap",
            "src",
            "crates",
        ] {
            let dir = root().join(dir);
            if dir.is_dir() {
                walk(&dir, &mut paths)
            }
        }
        paths.sort();
        let paths = Arc::new(paths);
        let cursor = Arc::new(AtomicUsize::new(0));
        let mut workers = vec![];
        for _ in 0..4 {
            let checker = Arc::clone(&checker);
            let paths = Arc::clone(&paths);
            let cursor = Arc::clone(&cursor);
            workers.push(
                std::thread::Builder::new()
                    .stack_size(64 << 20)
                    .spawn(move || {
                        let mut counts = [0; 6];
                        let mut mismatches = vec![];
                        loop {
                            let i = cursor.fetch_add(1, Ordering::Relaxed);
                            let Some(path) = paths.get(i) else { break };
                            let source = std::fs::read_to_string(path).unwrap();
                            let actual = evaluate(&world(&checker, &source));
                            if supported(&actual) {
                                eprintln!("supported {}: {actual:?}", path.display());
                                let expected = oracle(&source);
                                if actual != expected {
                                    mismatches.push(format!(
                                        "{}: actual {actual:?}, expected {expected:?}",
                                        path.display()
                                    ));
                                }
                                let category = usize::from(actual.is_some());
                                counts[category] += 1;
                                if !path.to_string_lossy().contains("self_host_check") {
                                    counts[3 + category] += 1;
                                }
                            } else {
                                counts[2] += 1
                            }
                        }
                        (counts, mismatches)
                    })
                    .unwrap(),
            );
        }
        let (counts, mismatches) = workers.into_iter().map(|w| w.join().unwrap()).fold(
            ([0; 6], vec![]),
            |(a, mut errors), (b, more)| {
                errors.extend(more);
                (std::array::from_fn(|i| a[i] + b[i]), errors)
            },
        );
        assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
        eprintln!(
            "Stage 1 differential: accepted={}, rejected={}, explicitly unsupported={}, total={}",
            counts[0],
            counts[1],
            counts[2],
            paths.len()
        );
        assert!(
            counts[0] >= 5 && counts[1] >= 10,
            "subset unexpectedly shrank"
        );
        assert!(
            counts[3] >= 5 && counts[4] >= 5,
            "independent repository coverage shrank: {counts:?}"
        );
    });
}
#[test]
fn unsupported_and_limits() {
    on_stack(|| {
        let checker = checker();
        for source in [
            "fn f[T](x: T) {}",
            "fn f() { || 1 }",
            "fn f(x: i64) -> i64 { x + 1 }",
            "copy enum Inner {A, B} copy enum Outer {Wrap(Inner)} fn f(x: Outer) {match x {Outer::Wrap(Inner::A) => (),}}",
            "fn f() -> i64 {-9223372036854775809}",
            "copy struct Recursive {next: Recursive} fn f(x: Recursive) {}",
            "fn f() { handle Fail[i64] in { 1 } with {return v => v, raise(e, r) => 0,} }",
        ] {
            let actual = evaluate(&world(&checker, source));
            assert!(
                !supported(&actual),
                "unsupported program silently checked: {source}: {actual:?}"
            );
        }
        for (name, code, seed_code) in [
            ("implicit_capability", "E9007", "E0403"),
            ("ambiguous_constructor", "E9008", "E0200"),
            ("incomparable_function_rows", "E9002", "E0401"),
            ("function_param_join", "E9002", "E0200"),
        ] {
            let source = std::fs::read_to_string(root().join(format!(
                "tests/fixtures/self_host_check/unsupported/{name}.ves"
            )))
            .unwrap();
            assert_eq!(
                evaluate(&world(&checker, &source)).unwrap().code,
                code,
                "{name}"
            );
            assert_eq!(
                oracle(&source).unwrap().code,
                seed_code,
                "{name}: seed control"
            );
        }
        for source in ["fn f() {", "fn f() { let x = ; }"] {
            assert!(
                !supported(&evaluate(&world(&checker, source))),
                "malformed input accepted: {source}"
            );
        }
        let source = format!(
            "fn f() {{ {} }}",
            (0..17)
                .map(|i| format!("let x{i} = 0;"))
                .collect::<String>()
        );
        assert_eq!(evaluate(&world(&checker, &source)).unwrap().code, "E9900");
        let source = format!("fn f() {{{}}}", "0;".repeat(260));
        assert_eq!(evaluate(&world(&checker, &source)).unwrap().code, "E9900");
        let source = (0..17)
            .map(|i| format!("fn f{i}() {{}} "))
            .collect::<String>();
        assert_eq!(evaluate(&world(&checker, &source)).unwrap().code, "E9900");
    });
}
