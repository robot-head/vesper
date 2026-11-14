mod attestation_commands;
mod audit_commands;
mod bench_commands;
mod cas_commands;
mod crucible_commands;
mod doc_commands;
mod doctest_commands;
mod ebpf_commands;
mod fmt_commands;
mod forge_commands;
mod grammar_commands;
mod json_commands;
mod lint_commands;
mod migrate_commands;
mod package_commands;
mod package_graph;
mod proof_commands;
mod replay_commands;
mod sbom_commands;
mod simd_commands;
mod tee_commands;
mod trace_commands;
mod verify_commands;
use std::{
    env, fs, io,
    path::PathBuf,
    process::{self, Command},
};
use vesper::{ast::Diagnostic, check, ir, lean, lsp, native, optimize, parser, runtime::Runtime};

const HELP: &str = "Vesper — modal ownership and lexical effects\n\nUsage: vesper <command> [FILE|DIR] [options]   (most commands take one .ves file; see vesper <command> --help)\n\nCommands:\n  forge   Check and expand explicit source Expr macros (forge --help)\n  grammar Validate canonical protocol bytes and Arrow IPC\n  verify  Kernel-check formatter document models (--format), or refute a contract (--refute)\n  fuzz    Crucible fuzzing of a source function (fuzz --help)\n  migrate Lift C/Rust signatures into .ves/.vsys (migrate --help)\n  replay  Verify and replay a Chrono .vchrono bundle (replay --help)\n  ebpf    Pre-flight verify an eBPF program (ebpf check --help)\n  fmt     Format supported source without changing tokens/comments\n  lint    Analyze checked source and validate supported fixes\n  doc     Generate checked API docs, heatmaps and a search bundle (doc --help)\n  test    Run doctests in one shared JIT module, else the evaluator (test --doc)\n  bench   Measure finite scalar source kernels (bench --help)\n  check   Check types, effects, ownership, and places\n  run     Execute with the one-shot effect stack machine\n  jit     Execute native code with Cranelift\n  build   Compile and link a native executable (-o <path>)\n  emit    Emit cst, hir, ir, clif, object, lean, or interface (--emit <kind>)\n  resolve Resolve or validate a locked local Loom catalog\n  graph   Inspect a solution snapshot (see graph --help)\n  sbom    Emit a CycloneDX snapshot inventory (see sbom --help)\n  audit   Audit declared dependencies against local policies (see audit --help)\n  json    Validate bounded JSON documents (see json --help)\n  traceparent  Validate W3C TraceContext v00 (see traceparent --help)\n  simd    Run the native byte scanner (see simd --help)\n  proof-audit  Kernel-replay local Lean artifacts (see proof-audit --help)\n  attest  Verify and import configured-key signed artifacts (see attest --help)\n  cas     Ingest, verify and materialize local content (see cas --help)\n  semver  Compare baseline and candidate interface JSON files\n  lsp     Serve incremental diagnostics over stdio\n\nOptions:\n  -o <path>       Output file (stdout for textual output by default)\n  --emit <kind>   Output format (default: ir)\n  --package <id>  Interface namespace (@org/name or std/name)\n  --entry <name>  Entry function (default: main)\n  --fuel <steps>  Evaluation step limit (default: 10000000)\n  --target <triple>  Object/executable target (default: host)\n  --linker <path>    Linker for build (default: cc)\n  --no-opt       Disable equality saturation\n  --verify=<list>    Run Stage-5 verifiers on emitted Cranelift (const-time,stack,realtime,detfloat,all)\n  --verify-<name>    One verifier (--verify-const-time, --verify-stack, ...)\n  --verify-policy <file>  Verifier policy JSON (vesper.native-verifiers/1)\n  --tee          build: write the TDX RTMR / CCA REM measurement of the executable\n  --json         Machine-readable diagnostics\n  -h, --help     Show help\n  -V, --version  Show version\n";
fn main() {
    if let Err(e) = cli() {
        eprintln!("vesper: {e}");
        process::exit(1);
    }
}
fn cli() -> Result<(), String> {
    let mut args = env::args().skip(1).peekable();
    let Some(command) = args.next() else {
        print!("{HELP}");
        return Ok(());
    };
    if command == "--help" || command == "-h" {
        print!("{HELP}");
        return Ok(());
    }
    if command == "--version" || command == "-V" {
        println!("vesper {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if command == "resolve" {
        return package_commands::resolve(args.collect());
    }
    if command == "fmt" {
        return fmt_commands::run(args.collect());
    }
    if command == "migrate" {
        return migrate_commands::run(args.collect());
    }
    if command == "replay" {
        return replay_commands::run(args.collect());
    }
    if command == "ebpf" {
        return ebpf_commands::run(args.collect());
    }
    if command == "fuzz" {
        return crucible_commands::fuzz(args.collect());
    }
    if command == "verify" {
        let args: Vec<String> = args.collect();
        if args.iter().any(|a| a == "--refute") {
            return crucible_commands::refute(args);
        }
        return verify_commands::run(args);
    }
    if command == "grammar" {
        return grammar_commands::run(args.collect());
    }
    if command == "forge" {
        return forge_commands::run(args.collect());
    }
    if command == "lint" {
        return lint_commands::run(args.collect());
    }
    if command == "test" {
        return doctest_commands::run(args.collect());
    }
    if command == "bench" {
        return bench_commands::run(args.collect());
    }
    if command == "doc" {
        return doc_commands::run(args.collect());
    }
    if command == "json" {
        return json_commands::run(args.collect());
    }
    if command == "traceparent" {
        return trace_commands::run(args.collect());
    }
    if command == "audit" {
        return audit_commands::run(args.collect());
    }
    if command == "attest" {
        return attestation_commands::run(args.collect());
    }
    if command == "cas" {
        return cas_commands::run(args.collect());
    }
    if command == "proof-audit" {
        return proof_commands::run(args.collect());
    }
    if command == "sbom" {
        return sbom_commands::run(args.collect());
    }
    if command == "simd" {
        return simd_commands::run(args.collect());
    }
    if command == "graph" {
        return package_graph::run(args.collect());
    }
    if command == "semver" {
        return package_commands::semver(args.collect());
    }
    if command == "lsp" {
        return lsp::serve(io::BufReader::new(io::stdin()), io::stdout().lock());
    }
    if !matches!(command.as_str(), "check" | "run" | "jit" | "build" | "emit") {
        return Err(format!("unknown command '{command}'; see --help"));
    }
    let mut input = None;
    let mut output = None;
    let mut format = "ir".to_string();
    let mut entry = "main".to_string();
    let mut fuel = 10_000_000;
    let mut json = false;
    let mut opt = true;
    let mut target = None;
    let mut linker = String::from("cc");
    let mut package = None;
    let mut verifiers = std::collections::BTreeSet::new();
    let mut verifier_policy: Option<PathBuf> = None;
    let mut tee_measure = false;
    while let Some(a) = args.next() {
        if let Some(v) = vesper::native_verifiers::NativeVerifier::from_flag(&a) {
            verifiers.insert(v);
            continue;
        }
        if let Some(list) = a.strip_prefix("--verify=") {
            verifiers.extend(vesper::native_verifiers::parse_verifier_list(list)?);
            continue;
        }
        match a.as_str() {
            "-o" => output = Some(PathBuf::from(args.next().ok_or("-o requires a path")?)),
            "--emit" => format = args.next().ok_or("--emit requires a format")?,
            "--entry" => entry = args.next().ok_or("--entry requires a name")?,
            "--fuel" => {
                fuel = args
                    .next()
                    .ok_or("--fuel requires a positive integer")?
                    .parse()
                    .map_err(|_| "invalid fuel value")?
            }
            "--json" => json = true,
            "--no-opt" => opt = false,
            "--verify-policy" => {
                verifier_policy = Some(PathBuf::from(
                    args.next().ok_or("--verify-policy requires a path")?,
                ))
            }
            "--tee" => tee_measure = true,
            "--target" => target = Some(args.next().ok_or("--target requires a target triple")?),
            "--linker" => linker = args.next().ok_or("--linker requires a path")?,
            "--package" => {
                package = Some(
                    args.next()
                        .ok_or("--package requires a package namespace")?,
                )
            }
            "--help" | "-h" => {
                print!("{HELP}");
                return Ok(());
            }
            _ if a.starts_with('-') => return Err(format!("unknown option '{a}'")),
            _ => {
                if input.replace(a).is_some() {
                    return Err("only one input module is supported".into());
                }
            }
        }
    }
    if target.is_some()
        && !(command == "build"
            || command == "emit" && matches!(format.as_str(), "object" | "clif"))
    {
        return Err("--target requires build or emit --emit object/clif".into());
    }
    let native_output =
        command == "build" || command == "emit" && matches!(format.as_str(), "object" | "clif");
    if (!verifiers.is_empty() || verifier_policy.is_some()) && !native_output {
        return Err("--verify flags require build or emit --emit object/clif".into());
    }
    if verifier_policy.is_some() && verifiers.is_empty() {
        return Err("--verify-policy requires --verify=<list> or a --verify-<name> flag".into());
    }
    if tee_measure && command != "build" {
        return Err("--tee requires build".into());
    }
    if package.is_some() && !(command == "emit" && format == "interface") {
        return Err("--package requires emit --emit interface".into());
    }
    let input = input.ok_or("missing source file; see --help")?;
    let source = fs::read_to_string(&input).map_err(|e| format!("{input}: {e}"))?;
    if command == "build" && target.as_deref() == Some("bpf") {
        // Compiler eBPF target (§27): verified BPF ELF object with BTF.
        let summary =
            vesper::ebpf_backend::build_command(&input, &source, &entry, output.as_deref())?;
        print!("{summary}");
        return Ok(());
    }
    let mut parsed = parser::parse(&source);
    if command == "emit" && format == "cst" {
        if !parsed.diagnostics.is_empty() {
            diagnostics(&input, &source, &parsed.diagnostics, json);
        }
        return write(output, format!("{:#?}\n", parsed.syntax()).as_bytes());
    }
    if !parsed.diagnostics.is_empty() {
        diagnostics(&input, &source, &parsed.diagnostics, json);
        return Err(format!("{} syntax error(s)", parsed.diagnostics.len()));
    }
    let mut checked = check::check(&parsed.module);
    if !checked.diagnostics.is_empty() {
        diagnostics(&input, &source, &checked.diagnostics, json);
        return Err(format!("{} semantic error(s)", checked.diagnostics.len()));
    }
    if command == "check" {
        if json {
            println!("[]");
        } else {
            println!("checked {input}");
        }
        return Ok(());
    }
    if command == "emit" && format == "interface" {
        let package = package.ok_or("interface export requires --package @org/name or std/name")?;
        let interface = vesper::interface::export(&checked.world, &package)?;
        let bytes = serde_json::to_vec_pretty(&interface).map_err(|e| e.to_string())?;
        return if let Some(path) = output {
            package_commands::atomic_write(&path, &bytes)
        } else {
            write(None, &bytes)
        };
    }
    if opt {
        optimize::optimize(&mut parsed.module, &checked.expression_types);
        checked = check::check(&parsed.module);
        if !checked.diagnostics.is_empty() {
            return Err(format!(
                "optimizer validation failed: {:?}",
                checked.diagnostics
            ));
        }
    }
    match command.as_str() {
        "run" => {
            let f = checked
                .world
                .functions
                .get(&entry)
                .ok_or("missing entry function")?;
            if !f.params.is_empty() {
                return Err("entry function must take no arguments".into());
            }
            if !f.effects.instances.is_empty() || f.effects.tail.is_some() {
                return Err("entry must handle all effects".into());
            }
            let mut runtime = Runtime::new(&checked.world);
            runtime.fuel = fuel;
            runtime.optimize = opt;
            println!("{}", runtime.run(&entry, vec![])?);
        }
        "jit" => println!("{}", native::jit_with_options(&checked.world, &entry, opt)?),
        // Wave-seven lane 25: `--target wasm32-wasip2` builds a component.
        "build" if target.as_deref() == Some(vesper::wasm_backend::TARGET) => {
            if !verifiers.is_empty() || verifier_policy.is_some() || tee_measure {
                return Err("--verify and --tee apply to native targets only".into());
            }
            let options = vesper::wasm_backend::WasmOptions {
                optimize: opt,
                ..Default::default()
            };
            let artifact = vesper::wasm_backend::compile(&checked.world, &entry, &options)
                .map_err(|e| e.to_string())?;
            let path = output.unwrap_or_else(|| PathBuf::from("a.wasm"));
            package_commands::atomic_write(&path, &artifact.component)?;
            println!(
                "built {} ({} component)",
                path.display(),
                vesper::wasm_backend::TARGET
            );
        }
        "build" => {
            if entry != "main" {
                return Err("build currently requires entry 'main'".into());
            }
            let f = checked
                .world
                .functions
                .get("main")
                .ok_or("missing main function")?;
            if !f.params.is_empty()
                || !matches!(f.result.name(), "i64" | "u64" | "u32" | "u8" | "Unit" | "")
            {
                return Err(
                    "executable main must take no arguments and return an integer or Unit".into(),
                );
            }
            let path = output.unwrap_or_else(|| PathBuf::from("a.out"));
            let object = path.with_extension("vesper.o");
            let (bytes, _) = native_code(
                &checked.world,
                &entry,
                target.as_deref(),
                opt,
                &verifiers,
                verifier_policy.as_deref(),
                (&input, &source),
            )?;
            fs::write(&object, bytes).map_err(|e| e.to_string())?;
            let status = Command::new(&linker)
                .arg(&object)
                .arg("-o")
                .arg(&path)
                .arg("-lm")
                .status()
                .map_err(|e| format!("could not invoke native linker '{linker}': {e}"))?;
            if !status.success() {
                return Err("native linker failed".into());
            }
            fs::remove_file(&object).map_err(|e| e.to_string())?;
            println!("built {}", path.display());
            if tee_measure {
                tee_commands::measure_executable(&path)?;
            }
        }
        "emit" => match format.as_str() {
            "hir" => write(
                output,
                serde_json::to_string_pretty(&parsed.module)
                    .map_err(|e| e.to_string())?
                    .as_bytes(),
            )?,
            "ir" => write(
                output,
                serde_json::to_string_pretty(&{
                    let mut program = ir::lower(&checked);
                    if opt {
                        vesper::ir_opt::optimize(&checked.world, &mut program)?;
                    }
                    program
                })
                .map_err(|e| e.to_string())?
                .as_bytes(),
            )?,
            "lean" => write(output, lean::extract(&checked.world)?.as_bytes())?,
            "object" => {
                let path = output.ok_or("object output requires -o <path>")?;
                let (bytes, _) = native_code(
                    &checked.world,
                    &entry,
                    target.as_deref(),
                    opt,
                    &verifiers,
                    verifier_policy.as_deref(),
                    (&input, &source),
                )?;
                fs::write(path, bytes).map_err(|e| e.to_string())?;
            }
            "clif" => {
                let (_, clif) = native_code(
                    &checked.world,
                    &entry,
                    target.as_deref(),
                    opt,
                    &verifiers,
                    verifier_policy.as_deref(),
                    (&input, &source),
                )?;
                write(output, clif.as_bytes())?;
            }
            _ => return Err(format!("unknown output format '{format}'")),
        },
        _ => unreachable!(),
    }
    Ok(())
}
/// Object bytes and CLIF for `entry`. With no verifier flag this is exactly
/// `native::object_with_options`; otherwise the integration-B hook runs the
/// selected Stage-5 verifiers on the emitted functions and any finding fails
/// the command.
fn native_code(
    world: &vesper::check::World,
    entry: &str,
    target: Option<&str>,
    opt: bool,
    verifiers: &std::collections::BTreeSet<vesper::native_verifiers::NativeVerifier>,
    policy: Option<&std::path::Path>,
    (file, source): (&str, &str),
) -> Result<(Vec<u8>, String), String> {
    // Capability effects (`eff [ConstTime]`, ...) imply their verifiers
    // even without a flag (lane 24).
    if verifiers.is_empty() && vesper::native_verifiers::capability_verifiers(world).is_empty() {
        return native::object_with_options(world, entry, target, opt);
    }
    let policy = match policy {
        Some(path) => vesper::native_verifiers::NativeVerifierPolicy::parse(
            &fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?,
        )?,
        None => vesper::native_verifiers::NativeVerifierPolicy {
            schema: vesper::native_verifiers::POLICY_SCHEMA.into(),
            ..Default::default()
        },
    };
    let (bytes, clif, report) = vesper::native_verifiers::verify_native_with_capabilities(
        world, entry, target, opt, verifiers, &policy,
    )?;
    eprint!(
        "{}",
        vesper::native_verifiers::render(&report, file, source)
    );
    if !report.accepted() {
        return Err(format!(
            "{} native verifier finding(s)",
            report.findings.len()
        ));
    }
    let names: Vec<_> = report.verifiers.iter().map(|v| v.name()).collect();
    eprintln!(
        "verified {} emitted function(s): {}",
        report.functions,
        names.join(", ")
    );
    Ok((bytes, clif))
}
fn write(path: Option<PathBuf>, bytes: &[u8]) -> Result<(), String> {
    if let Some(path) = path {
        fs::write(path, bytes).map_err(|e| e.to_string())
    } else {
        use io::Write;
        io::stdout()
            .lock()
            .write_all(bytes)
            .map_err(|e| e.to_string())
    }
}
fn diagnostics(file: &str, source: &str, ds: &[Diagnostic], json: bool) {
    if json {
        println!("{}", serde_json::to_string(ds).unwrap());
        return;
    }
    let rendered = (|| {
        if source.len() > vesper_span::Limits::HARD.max_source_bytes {
            return Err("source exceeds the 1 MiB diagnostic display profile".into());
        }
        let index =
            vesper_span::LineIndex::new(std::sync::Arc::from(source), vesper_span::Limits::HARD)
                .map_err(|error| error.to_string())?;
        vesper_diagnostics::prepare(&index, ds, vesper_diagnostics::Limits::HARD)
            .and_then(|prepared| prepared.terminal(file))
            .map_err(|error| error.to_string())
    })();
    match rendered {
        Ok(bytes) => {
            use io::Write;
            let _ = io::stderr().lock().write_all(&bytes);
        }
        Err(reason) => {
            // Compiler errors remain errors when source presentation is outside
            // its profile. These are raw byte offsets, never clamped squiggles.
            eprintln!("diagnostic source rendering unavailable: {reason}");
            for diagnostic in ds.iter().take(128) {
                let code: String = diagnostic.code.chars().take(128).collect();
                let message: String = diagnostic.message.chars().take(4096).collect();
                eprintln!(
                    "{code}: {message} (UTF-8 bytes {}..{})",
                    diagnostic.span.start, diagnostic.span.end
                );
            }
            if ds.len() > 128 {
                eprintln!(
                    "{} additional compiler errors exceed the display profile",
                    ds.len() - 128
                );
            }
        }
    }
}
