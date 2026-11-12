use std::{fs::OpenOptions, io::Read};
use vesper::doctest::{self, DocTestLimits};

const HELP: &str = "Usage: vesper test --doc FILE [--json]\n\nExtract actual Vesper doc-comment fences, fully check examples, and run supported finite scalar entries in one owned JIT module.\ncompile_fail(E...) must match exact example diagnostics; base errors and unsupported execution never count as success.\nSource/check/module budgets apply; this is not general SimPulse I/O, an execution sandbox or a hard deadline.\n";

pub fn run(args: Vec<String>) -> Result<(), String> {
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print!("{HELP}");
        return Ok(());
    }
    let requested_json = args.iter().any(|arg| arg == "--json");
    let parsed = || -> Result<(String, bool), String> {
        let mut file = None;
        let mut json = false;
        let mut doc = false;
        for argument in args {
            match argument.as_str() {
                "--doc" if !doc => doc = true,
                "--json" if !json => json = true,
                value if !value.starts_with('-') && file.is_none() => {
                    file = Some(value.to_string())
                }
                _ => return Err("usage: vesper test --doc FILE [--json]".into()),
            }
        }
        if !doc {
            return Err("usage: vesper test --doc FILE [--json]".into());
        }
        Ok((file.ok_or("missing doctest source file")?, json))
    };
    let (file, json) = match parsed() {
        Ok(options) => options,
        Err(error) => {
            if requested_json {
                println!("{}", serde_json::json!({"passed":false,"error":error}));
            }
            return Err(error);
        }
    };
    let limits = DocTestLimits::default();
    let operation = || -> Result<_, String> {
        if !std::fs::metadata(&file)
            .map_err(|e| format!("{file}: {e}"))?
            .is_file()
        {
            return Err("doctest input must be a regular file".into());
        }
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            // A file-to-FIFO replacement must not block before handle validation.
            options.custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC);
        }
        let input = options.open(&file).map_err(|e| format!("{file}: {e}"))?;
        if !input.metadata().map_err(|e| e.to_string())?.is_file() {
            return Err("doctest input must be a regular file".into());
        }
        let mut bytes = Vec::new();
        input
            .take(
                u64::try_from(
                    limits
                        .max_source_bytes
                        .checked_add(1)
                        .ok_or("source limit overflow")?,
                )
                .map_err(|e| e.to_string())?,
            )
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        let source = std::str::from_utf8(&bytes).map_err(|e| e.to_string())?;
        doctest::run(source, limits)
    };
    let report = match operation() {
        Ok(report) => report,
        Err(error) => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({"file":file,"passed":false,"error":error})
                );
            }
            return Err(error);
        }
    };
    if json {
        println!(
            "{}",
            serde_json::json!({"file":file,"passed":report.passed,"report":report})
        );
    } else {
        for d in &report.base_diagnostics {
            println!("{file}:{}:{}: {}: {}", d.line, d.column, d.code, d.message);
        }
        for example in &report.examples {
            println!(
                "{file}:{}:{}: {} doctest {}: {}",
                example.line,
                example.column,
                if example.passed { "PASS" } else { "FAIL" },
                example.index,
                example.message
            );
            for d in &example.diagnostics {
                println!("{file}:{}:{}: {}: {}", d.line, d.column, d.code, d.message);
            }
        }
    }
    if report.passed {
        Ok(())
    } else {
        Err("documentation tests failed".into())
    }
}
