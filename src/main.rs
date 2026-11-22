mod doc_commands;
mod doctest_commands;
mod ebpf_commands;
mod fmt_commands;
mod forge_commands;
mod highlight_commands;
mod lib_commands;
mod lint_commands;
mod package_graph;
mod shot_commands;
mod tool_commands;
mod verify_commands;
mod vsys_commands;
use std::{
    env, fs, io,
    path::PathBuf,
    process::{self, Command},
};
use vesper::{
    ast::Diagnostic, check, ir, lean, lsp, native, optimize, parser, proof_commands,
    runtime::Runtime,
};

const HELP: &str = "Vesper — modal ownership and lexical effects\n\nUsage: vesper <command> [FILE|DIR] [options]   (most commands take one .ves file; check/run/jit/build/emit also take a package directory with a Loom.toml; see vesper <command> --help)\n\nCommands:\n  forge   Check and expand explicit source Expr macros (forge --help)\n  grammar Validate canonical protocol bytes and Arrow IPC\n  verify  Kernel-check formatter document models (--format), or refute a contract (--refute)\n  fuzz    Crucible fuzzing of a source function (fuzz --help)\n  migrate Lift C/Rust signatures into .ves/.vsys (migrate --help)\n  replay  Verify and replay a Chrono .vchrono bundle (replay --help)\n  ebpf    Pre-flight verify an eBPF program (ebpf check --help)\n  fmt     Format supported source without changing tokens/comments\n  highlight  Chroma semantic highlighting: --html, --ansi, --json, --css (highlight --help)\n  shot    Render an SVG or 1x-4x PNG code card (shot --help)\n  lint    Analyze checked source and validate supported fixes\n  doc     Generate checked API docs, heatmaps and a search bundle (doc --help)\n  test    Run unit tests, or run doctests with --doc\n  bench   Measure finite scalar source kernels (bench --help)\n  check   Check types, effects, ownership, and places\n  run     Execute with the one-shot effect stack machine\n  jit     Execute native code with Cranelift\n  build   Compile and link a native executable (-o <path>), or a C-ABI library (--lib)\n  emit    Emit cst, hir, ir, clif, object, lean, or interface (--emit <kind>)\n  resolve Resolve or validate a locked local Loom catalog\n  graph   Inspect a solution snapshot (see graph --help)\n  sbom    Emit a CycloneDX snapshot inventory (see sbom --help)\n  audit   Audit declared dependencies against local policies (see audit --help)\n  top     Inspect Pulse fiber telemetry and linear obligations (top --help)\n  publish Run offline release gates and publish to a local CAS (publish --help)\n  json    Validate bounded JSON documents (see json --help)\n  traceparent  Validate W3C TraceContext v00 (see traceparent --help)\n  simd    Run the native byte scanner (see simd --help)\n  proof-audit  Kernel-replay local Lean artifacts (see proof-audit --help)\n  attest  Verify and import configured-key signed artifacts (see attest --help)\n  cas     Ingest, verify and materialize local content (see cas --help)\n  semver  Compare baseline and candidate interface JSON files\n  lsp     Serve incremental diagnostics over stdio\n\nOptions:\n  -o <path>       Output file (stdout for textual output by default)\n  --emit <kind>   Output format (default: ir)\n  --package <id>  Interface namespace (@org/name or std/name)\n  --entry <name>  Entry function (default: main)\n  --include-tests  check/run/jit: enable test-only declarations\n  --fuel <steps>  Evaluation step limit (default: 10000000)\n  --target <triple>  Object/executable target (default: host)\n  --target-features <list>  With --target: CPU features to use, as Rust's names (ssse3,avx2,...; cargo's CARGO_CFG_TARGET_FEATURE)\n  --linker <path>    Linker for build (default: cc)\n  --link-search <dir>  build: a directory the linker searches for .vsys libraries (repeatable)\n  --codegen verified  build: direct verified code generator (scalar subset, no Cranelift)\n  --no-opt       Disable equality saturation\n  --verify=<list>    Run Stage-5 verifiers on emitted Cranelift (const-time,stack,realtime,detfloat,all)\n  --verify-<name>    One verifier (--verify-const-time, --verify-stack, ...)\n  --verify-policy <file>  Verifier policy JSON (vesper.native-verifiers/1)\n  --lib          build: a C-ABI library object (--lib-name, --interface, --rust-binding, --rust-types, --instance f[T])\n  --tee          build: write the TDX RTMR / CCA REM measurement of the executable\n  --json         Machine-readable diagnostics\n  -h, --help     Show help\n  -V, --version  Show version\n";
/// Stack for the CLI thread. The checker, parser and lowering recurse over
/// expression depth (bounded by their own budgets), and debug builds use large
/// frames. Windows gives the main thread only 1 MiB, which a 25-deep generic
/// call chain already exceeds, so run every command on a thread with an
/// explicit, platform-independent stack.
const CLI_STACK_BYTES: usize = 64 * 1024 * 1024;

/// Commands the compiler binary runs itself; `tool_commands::TOOLS` and any
/// other `vesper-<name>` executable are dispatched before the CLI starts.
const COMPILER_COMMANDS: &[&str] = &[
    "build",
    "check",
    "doc",
    "ebpf",
    "emit",
    "fmt",
    "forge",
    "graph",
    "highlight",
    "jit",
    "lint",
    "lsp",
    "proof-audit",
    "run",
    "shot",
    "test",
    "verify",
];

fn main() {
    if let Some(code) = tool_commands::dispatch(|c| COMPILER_COMMANDS.contains(&c)) {
        process::exit(code);
    }
    let result = std::thread::Builder::new()
        .name("vesper".to_owned())
        .stack_size(CLI_STACK_BYTES)
        .spawn(cli)
        .expect("spawn the vesper CLI thread")
        .join();
    match result {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            eprintln!("vesper: {e}");
            process::exit(1);
        }
        Err(panic) => std::panic::resume_unwind(panic),
    }
}
fn format_version_banner() -> String {
    format!(
        "  \\\\    //   vesper {} (stage3-ddc)\n   \\\\  //    target: {}-{} (cranelift + lean4)\n    \\\\||/    modes: copy <= affine <= linear | 0 sorryAx\n     \\/\n",
        env!("CARGO_PKG_VERSION"),
        env::consts::ARCH,
        env::consts::OS,
    )
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
        print!("{}", format_version_banner());
        return Ok(());
    }
    if command == "fmt" {
        return fmt_commands::run(args.collect());
    }
    if command == "highlight" {
        return highlight_commands::run(args.collect());
    }
    if command == "ebpf" {
        return ebpf_commands::run(args.collect());
    }
    if command == "verify" {
        let args: Vec<String> = args.collect();
        if args.iter().any(|a| a == "--refute") {
            // Counterexample search links Crucible: the `vesper-refute` tool.
            let args: Vec<std::ffi::OsString> = args.into_iter().map(Into::into).collect();
            let status = tool_commands::run(tool_commands::REFUTE_TOOL, &args)?;
            process::exit(tool_commands::exit_code(status));
        }
        return verify_commands::run(args);
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
    if command == "shot" {
        return shot_commands::run(args.collect());
    }
    if command == "doc" {
        return doc_commands::run(args.collect());
    }
    if command == "proof-audit" {
        return proof_commands::run(args.collect());
    }
    if command == "graph" {
        return package_graph::run(args.collect());
    }
    if command == "lsp" {
        return lsp::serve(io::BufReader::new(io::stdin()), io::stdout());
    }
    if !matches!(command.as_str(), "check" | "run" | "jit" | "build" | "emit") {
        return Err(format!("unknown command '{command}'; see --help"));
    }
    let mut input = None;
    let mut output = None;
    let mut format = "ir".to_string();
    let mut entry = "main".to_string();
    let mut include_tests = false;
    let mut fuel = 10_000_000;
    let mut json = false;
    let mut opt = true;
    let mut target = None;
    let mut target_features = None;
    let mut linker = String::from("cc");
    let mut link_search: Vec<PathBuf> = vec![];
    let mut package = None;
    let mut verifiers = std::collections::BTreeSet::new();
    let mut verifier_policy: Option<PathBuf> = None;
    let mut tee_measure = false;
    let mut codegen = None;
    let mut lib = lib_commands::LibFlags::default();
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
            "--include-tests" => include_tests = true,
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
            "--codegen" => codegen = Some(args.next().ok_or("--codegen requires a backend")?),
            "--target" => target = Some(args.next().ok_or("--target requires a target triple")?),
            "--target-features" => {
                target_features = Some(
                    args.next()
                        .ok_or("--target-features requires a comma-separated list")?,
                )
            }
            "--linker" => linker = args.next().ok_or("--linker requires a path")?,
            "--link-search" => link_search.push(PathBuf::from(
                args.next().ok_or("--link-search requires a directory")?,
            )),
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
            _ if lib.flag(&a, &mut args)? => {}
            _ if a.starts_with('-') => return Err(format!("unknown option '{a}'")),
            _ => {
                if input.replace(a).is_some() {
                    return Err("only one input module is supported".into());
                }
            }
        }
    }
    if include_tests && !matches!(command.as_str(), "check" | "run" | "jit") {
        return Err("--include-tests requires check, run, or jit".into());
    }
    if target.is_some()
        && !(command == "build"
            || command == "emit" && matches!(format.as_str(), "object" | "clif"))
    {
        return Err("--target requires build or emit --emit object/clif".into());
    }
    if let Some(features) = &target_features {
        if target.is_none() {
            return Err("--target-features requires --target".into());
        }
        vesper::native::set_target_features(features);
    }
    let native_output =
        command == "build" || command == "emit" && matches!(format.as_str(), "object" | "clif");
    if (!verifiers.is_empty() || verifier_policy.is_some()) && !native_output {
        return Err("--verify flags require build or emit --emit object/clif".into());
    }
    if verifier_policy.is_some() && verifiers.is_empty() {
        return Err("--verify-policy requires --verify=<list> or a --verify-<name> flag".into());
    }
    if codegen.is_some() && command != "build" {
        return Err("--codegen requires build".into());
    }
    if tee_measure && command != "build" {
        return Err("--tee requires build".into());
    }
    if package.is_some() && !(command == "emit" && format == "interface") {
        return Err("--package requires emit --emit interface".into());
    }
    let input = input.ok_or("missing source file; see --help")?;
    // Lane F0: a directory with a Loom.toml is a package; its modules and
    // path dependencies are assembled into one module (`vesper::packages`).
    let loaded = if vesper::packages::is_package(std::path::Path::new(&input)) {
        if command == "emit" && format == "cst" {
            return Err("--emit cst takes one source file, not a package".into());
        }
        if target.as_deref() == Some("bpf") {
            return Err("--target bpf takes one source file, not a package".into());
        }
        let path = std::path::Path::new(&input);
        let assembled = if include_tests {
            vesper::packages::load_for_tests(path)?
        } else {
            vesper::packages::load(path)?
        };
        if !assembled.ok() {
            package_diagnostics(&assembled, &assembled.diagnostics, json);
            return Err(format!(
                "{} package error(s)",
                assembled.diagnostics.len() + assembled.manifest_errors.len()
            ));
        }
        Some(assembled)
    } else {
        None
    };
    let source = match &loaded {
        Some(p) => p.sources.combined(),
        None => fs::read_to_string(&input).map_err(|e| format!("{input}: {e}"))?,
    };
    if command == "build" && target.as_deref() == Some("bpf") {
        // Compiler eBPF target (§27): verified BPF ELF object with BTF.
        let summary =
            vesper::ebpf_backend::build_command(&input, &source, &entry, output.as_deref())?;
        print!("{summary}");
        return Ok(());
    }
    let mut parsed = match &loaded {
        Some(p) => parser::Parse {
            module: p.module.clone(),
            green: parser::parse("").green,
            diagnostics: vec![],
        },
        None if include_tests => parser::parse_for_tests(&source),
        None => parser::parse(&source),
    };
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
    // `.vsys` libraries the loaded packages carry (`[package] vsys`).
    let package_vsys = loaded.as_ref().map_or(&[][..], |p| &p.vsys[..]);
    if !package_vsys.is_empty() && !(lib.enabled || lib.any_option()) {
        // Programs over package-carried imports check and run only through
        // the Bridge lowering (#1150).
        let p = loaded.as_ref().expect("package_vsys comes from a package");
        return vsys_commands::run(vsys_commands::Command {
            command: &command,
            format: &format,
            package: p,
            module: &parsed.module,
            entry: &entry,
            output,
            target: target.as_deref(),
            optimize: opt,
            linker: &linker,
            link_search: &link_search,
            json,
            exclusive: !verifiers.is_empty()
                || verifier_policy.is_some()
                || tee_measure
                || codegen.is_some(),
        });
    }
    let mut checked = check::check(&parsed.module);
    // A library with `.vsys` imports checks only through the Bridge
    // lowering, which the export path below runs.
    if !checked.diagnostics.is_empty() && !lib.needs_module(&source, !package_vsys.is_empty()) {
        match &loaded {
            Some(p) => {
                let localized: Vec<_> = checked
                    .diagnostics
                    .iter()
                    .map(|d| p.sources.localize(d))
                    .collect();
                package_diagnostics(p, &localized, json);
            }
            None => diagnostics(&input, &source, &checked.diagnostics, json),
        }
        return Err(format!("{} semantic error(s)", checked.diagnostics.len()));
    }
    if lib.enabled || lib.any_option() {
        // Exports are the root module's `pub` functions. Items of package
        // submodules and dependencies are renamed (`SourceMap::qualified`)
        // and stay internal for now. The module is compiled again by the
        // export path, which adds generic-instance wrappers and checks
        // `.vsys` imports through the Bridge lowering (export ABI v2, #898).
        let public = parsed
            .module
            .public_items
            .iter()
            .filter(|n| {
                loaded
                    .as_ref()
                    .is_none_or(|p| !p.sources.qualified.contains_key(*n))
            })
            .cloned()
            .collect();
        // `@export` directives belong to the root package's root module
        // (`src/lib.ves`), which is not file 0 when another source sorts
        // first. Other files are blanked so their directives, written
        // against their own names, do not apply. The length is kept, since
        // the export path places generated code past the end.
        let directives_source = match &loaded {
            Some(p) => export_directives_source(p)?,
            None => source.clone(),
        };
        let foreign = loaded.is_some() && lib.needs_module(&source, !package_vsys.is_empty());
        let module = foreign.then(|| parsed.module.clone());
        let built = lib.build(
            &command,
            parsed.module,
            &public,
            &input,
            &directives_source,
            package_vsys,
            output,
            target.as_deref(),
            opt,
            json,
            !verifiers.is_empty() || verifier_policy.is_some() || tee_measure || codegen.is_some(),
        );
        // A package that imports `.vsys` libraries is checked by the export
        // path, whose errors carry offsets into the combined sources. On
        // failure, check it again through the Bridge lowering to report each
        // diagnostic at its own file and line.
        if built.is_err()
            && let (Some(p), Some(module)) = (&loaded, module)
            && let Some(ds) =
                foreign_package_diagnostics(p, &module, &directives_source, target.as_deref())
        {
            package_diagnostics(p, &ds, json);
            return Err(format!("{} semantic error(s)", ds.len()));
        }
        return built;
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
        // A package directory exports under its manifest name by default.
        let package = package
            .or_else(|| loaded.as_ref().map(|p| p.package.clone()))
            .ok_or("interface export requires --package @org/name or std/name")?;
        let interface = vesper::interface::export(&checked.world, &package)?;
        let bytes = serde_json::to_vec_pretty(&interface).map_err(|e| e.to_string())?;
        return if let Some(path) = output {
            vesper::atomic_file::atomic_write(&path, &bytes)
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
        // Wave-eight lane 2: the direct verified code generator (§42).
        "build" if codegen.is_some() => {
            if codegen.as_deref() != Some(vesper::verified_codegen::CODEGEN) {
                return Err("--codegen accepts only 'verified'".into());
            }
            if !verifiers.is_empty() || verifier_policy.is_some() || tee_measure {
                return Err("--verify and --tee do not apply to --codegen verified".into());
            }
            vesper::dyn_trait::refuse(&checked.world, "verified code generator")
                .map_err(|d| format!("{}: {}", d.code, d.message))?;
            let path = output.unwrap_or_else(|| PathBuf::from("a.out"));
            println!(
                "{}",
                vesper::verified_codegen::build_command(
                    &checked.world,
                    &entry,
                    target.as_deref(),
                    opt,
                    &linker,
                    &path,
                )?
            );
        }
        // Wave-seven lane 25: `--target wasm32-wasip2` builds a component.
        "build" if target.as_deref() == Some(vesper::wasm_backend::TARGET) => {
            if !verifiers.is_empty() || verifier_policy.is_some() || tee_measure {
                return Err("--verify and --tee apply to native targets only".into());
            }
            vesper::dyn_trait::refuse(&checked.world, "WebAssembly backend")
                .map_err(|d| format!("{}: {}", d.code, d.message))?;
            let options = vesper::wasm_backend::WasmOptions {
                optimize: opt,
                ..Default::default()
            };
            let artifact = vesper::wasm_backend::compile(&checked.world, &entry, &options)
                .map_err(|e| e.to_string())?;
            let path = output.unwrap_or_else(|| PathBuf::from("a.wasm"));
            vesper::atomic_file::atomic_write(&path, &artifact.component)?;
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
                || !(vesper::numeric::is_integer_name(f.result.name())
                    || matches!(f.result.name(), "Unit" | ""))
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
            // Windows targets link with link.exe/lld-link (lane 24, #347);
            // the default `cc` there means "find the MSVC linker".
            if native::windows::is_windows_target(target.as_deref()) {
                let explicit = (linker != "cc").then(|| PathBuf::from(&linker));
                let path = native::windows::link_executable(&object, &path, explicit.as_deref())?;
                fs::remove_file(&object).map_err(|e| e.to_string())?;
                println!("built {}", path.display());
                if tee_measure {
                    measure_tee(&path)?;
                }
                return Ok(());
            }
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
                measure_tee(&path)?;
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
                        vesper::ir_opt_policy::optimize(&checked.world, &mut program)?;
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
/// Package diagnostics: manifest errors, then each file's diagnostics
/// rendered against that file (JSON adds a `file` member).
fn package_diagnostics(
    package: &vesper::packages::Assembled,
    ds: &[vesper::packages::PackageDiagnostic],
    json: bool,
) {
    let files = &package.sources.files;
    let file_name = |d: &vesper::packages::PackageDiagnostic| {
        d.file.map_or("Loom.toml", |f| files[f].display.as_str())
    };
    if json {
        let mut out = vec![];
        for (file, d) in &package.manifest_errors {
            let mut v = serde_json::to_value(d).unwrap_or_default();
            v["file"] = serde_json::Value::String(file.clone());
            out.push(v);
        }
        for d in ds {
            let mut v = serde_json::to_value(&d.diagnostic).unwrap_or_default();
            v["file"] = serde_json::Value::String(file_name(d).to_owned());
            out.push(v);
        }
        println!("{}", serde_json::Value::Array(out));
        return;
    }
    for (file, d) in &package.manifest_errors {
        eprintln!("error[{}]: {} ({file})", d.code, d.message);
    }
    for d in ds {
        match d.file {
            Some(f) => diagnostics(
                &files[f].display,
                &files[f].text,
                std::slice::from_ref(&d.diagnostic),
                false,
            ),
            None => eprintln!("error[{}]: {}", d.diagnostic.code, d.diagnostic.message),
        }
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

/// The Bridge lowering's diagnostics for a package that imports `.vsys`
/// libraries, mapped to their files; `None` when the failure was not a
/// located source error (the export path's own message stands).
fn foreign_package_diagnostics(
    p: &vesper::packages::Assembled,
    module: &vesper::ast::Module,
    directives_source: &str,
    target: Option<&str>,
) -> Option<Vec<vesper::packages::PackageDiagnostic>> {
    let mut directives =
        vesper::native::export::Directives::parse(directives_source, target).ok()?;
    directives.add_packages(&p.vsys, target).ok()?;
    let mut imports = vesper::ffi_lowering::Imports::new();
    for v in &directives.vsys {
        imports.add(&v.source, v.handler).ok()?;
    }
    let error = vesper::ffi_lowering::check_module(module, &imports).err()?;
    let mut ds: Vec<Diagnostic> = error.diagnostics.clone();
    if let Some(span) = &error.span {
        // Bridge errors have a kind, not a diagnostic code.
        ds.push(Diagnostic::new(
            "bridge",
            format!("{:?}: {}", error.kind, error.message),
            span.clone(),
        ));
    }
    if ds.is_empty() {
        return None;
    }
    Some(ds.iter().map(|d| p.sources.localize(d)).collect())
}

/// The package source the export path reads `@export` directives from:
/// every file in load order, with the root package's root module kept and
/// every other file blanked to spaces (lengths are kept, so offsets past the
/// end still line up).
///
/// `// @export abi N` names no item, so it is also read, with a warning,
/// from the root package's other files: ports placed it in the file that
/// sorted first while directives were read from file 0. `instance` and
/// `vsys` directives name items and are an error outside the root module.
fn export_directives_source(p: &vesper::packages::Assembled) -> Result<String, String> {
    let root = p.root_module.unwrap_or(0);
    let root_shown = p
        .sources
        .files
        .get(root)
        .map(|f| f.display.clone())
        .unwrap_or_default();
    let is_abi = |line: &str| {
        line.trim_start()
            .strip_prefix("// @export")
            .is_some_and(|rest| rest.trim().starts_with("abi"))
    };
    // Only one `abi` directive is forwarded: the root module's, else the
    // first one found in another file (ports that named it in every file
    // would otherwise name it twice).
    let mut abi_seen = p
        .sources
        .files
        .get(root)
        .is_some_and(|f| f.text.lines().any(is_abi));
    let mut out = String::new();
    for (i, f) in p.sources.files.iter().enumerate() {
        if i == root {
            out.push_str(&f.text);
        } else if p.root_package_files.contains(&i) {
            for line in f.text.split_inclusive('\n') {
                let body = line.trim_end_matches(['\n', '\r']);
                let directive = body.trim_start().strip_prefix("// @export");
                match directive.map(str::trim) {
                    Some(rest) if rest.starts_with("abi") => {
                        eprintln!(
                            "warning: {}: `// @export {rest}` belongs in the root module {root_shown}; \
                             it is read from here for now",
                            f.display
                        );
                        if abi_seen {
                            out.push_str(&" ".repeat(body.len()));
                            out.push_str(&line[body.len()..]);
                        } else {
                            abi_seen = true;
                            out.push_str(line);
                        }
                    }
                    Some(rest) => {
                        return Err(format!(
                            "{}: `// @export {rest}` is outside the root module; \
                             @export directives are read from {root_shown}",
                            f.display
                        ));
                    }
                    None => {
                        out.push_str(&" ".repeat(body.len()));
                        out.push_str(&line[body.len()..]);
                    }
                }
            }
        } else {
            out.push_str(&" ".repeat(f.text.len()));
        }
        out.push('\n');
    }
    Ok(out)
}

/// `build --tee`: the `vesper-tee` tool measures the executable, so Aegis
/// stays out of the compiler's closure (SH0 lane 2).
fn measure_tee(path: &std::path::Path) -> Result<(), String> {
    let status = tool_commands::run(
        tool_commands::TEE_TOOL,
        &["measure".into(), path.as_os_str().to_owned()],
    )?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "vesper-tee could not measure {} (exit {})",
            path.display(),
            tool_commands::exit_code(status)
        ))
    }
}
