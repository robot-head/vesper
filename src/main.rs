mod attestation_commands;
mod package_commands;
mod package_graph;
mod proof_commands;
mod sbom_commands;
mod simd_commands;
use ariadne::{Label, Report, ReportKind, Source};
use std::{
    env, fs, io,
    path::PathBuf,
    process::{self, Command},
};
use vesper::{ast::Diagnostic, check, ir, lean, lsp, native, optimize, parser, runtime::Runtime};

const HELP: &str = "Vesper — modal ownership and lexical effects\n\nUsage: vesper <command> <file.ves> [options]\n\nCommands:\n  check   Check types, effects, ownership, and places\n  run     Execute with the one-shot effect stack machine\n  jit     Execute native code with Cranelift\n  build   Compile and link a native executable (-o <path>)\n  emit    Emit cst, hir, ir, clif, object, lean, or interface (--emit <kind>)\n  resolve Resolve or validate a locked local Loom catalog\n  graph   Inspect a solution snapshot (see graph --help)\n  sbom    Emit a CycloneDX snapshot inventory (see sbom --help)\n  simd    Run the native byte scanner (see simd --help)\n  proof-audit  Kernel-replay local Lean artifacts (see proof-audit --help)\n  attest  Verify configured-key builder attestations (see attest --help)\n  semver  Compare baseline and candidate interface JSON files\n  lsp     Serve incremental diagnostics over stdio\n\nOptions:\n  -o <path>       Output file (stdout for textual output by default)\n  --emit <kind>   Output format (default: ir)\n  --package <id>  Interface namespace (@org/name or std/name)\n  --entry <name>  Entry function (default: main)\n  --fuel <steps>  Evaluation step limit (default: 10000000)\n  --target <triple>  Object/executable target (default: host)\n  --linker <path>    Linker for build (default: cc)\n  --no-opt       Disable equality saturation\n  --json         Machine-readable diagnostics\n  -h, --help     Show help\n  -V, --version  Show version\n";
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
    if command == "attest" {
        return attestation_commands::run(args.collect());
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
        return lsp::serve(io::stdin().lock(), io::stdout().lock());
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
    while let Some(a) = args.next() {
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
    if package.is_some() && !(command == "emit" && format == "interface") {
        return Err("--package requires emit --emit interface".into());
    }
    let input = input.ok_or("missing source file; see --help")?;
    let source = fs::read_to_string(&input).map_err(|e| format!("{input}: {e}"))?;
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
            let (bytes, _) =
                native::object_with_options(&checked.world, &entry, target.as_deref(), opt)?;
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
                let (bytes, _) =
                    native::object_with_options(&checked.world, &entry, target.as_deref(), opt)?;
                fs::write(path, bytes).map_err(|e| e.to_string())?;
            }
            "clif" => {
                let (_, clif) =
                    native::object_with_options(&checked.world, &entry, target.as_deref(), opt)?;
                write(output, clif.as_bytes())?;
            }
            _ => return Err(format!("unknown output format '{format}'")),
        },
        _ => unreachable!(),
    }
    Ok(())
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
    for d in ds {
        let span = d.span.start.min(source.len())..d.span.end.min(source.len());
        let _ = Report::build(ReportKind::Error, (file, span.clone()))
            .with_code(&d.code)
            .with_message(&d.message)
            .with_label(Label::new((file, span)).with_message(&d.message))
            .finish()
            .eprint((file, Source::from(source)));
    }
}
