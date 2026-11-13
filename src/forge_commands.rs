//! Explicit bounded Tier1 commands. Never executes native code or components.
use serde::Serialize;
use std::{
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};
use vesper::{
    ast::Diagnostic,
    forge_frontend::{
        self as forge, CompileConfig, CompileLimits, ExpandedChecked, PolicyConfig, Stage,
    },
};

const HELP: &str = "Usage: vesper forge <check|expand> SOURCE [--package @local/source] [--policy FILE] [--json]\n\nCheck declarative Expr macros using whole-body hygiene, the ordinary checker and actual compiler-derived authority.\ncheck returns bounded JSON; expand writes exactly the fully checked expanded source.\nMacro-bearing functions must be top-level and nongeneric, with at most eight plain named parameters.\nProjected parameters, source Tier2, attributes, derives, recursive/nested macros and other categories are unsupported.\nNo native code or WASI component is executed. Existing check/LSP behavior is unchanged.\n";
const MAX_POLICY: usize = 64 * 1024;
const MAX_JSON: usize = 2 * 1024 * 1024;
struct Args {
    check: bool,
    input: PathBuf,
    package: String,
    policy: Option<PathBuf>,
}
fn args(args: &[String]) -> Result<Args, String> {
    if args.len() > 16
        || args
            .iter()
            .try_fold(0usize, |n, s| n.checked_add(s.len()))
            .is_none_or(|n| n > 16384)
    {
        return Err("source macro argument budget exceeded".into());
    }
    let action = args
        .first()
        .ok_or("missing forge action; see forge --help")?;
    let check = match action.as_str() {
        "check" => true,
        "expand" => false,
        _ => return Err("expected forge check or expand".into()),
    };
    let mut input = None;
    let mut package = None;
    let mut policy = None;
    let mut json = false;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--package" => {
                i += 1;
                let value = args.get(i).ok_or("--package requires a namespace")?;
                if value.len() > 256 || package.replace(value.clone()).is_some() {
                    return Err("duplicate or oversized package namespace".into());
                }
            }
            "--policy" => {
                i += 1;
                let value = args.get(i).ok_or("--policy requires a file")?;
                if policy.replace(PathBuf::from(value)).is_some() {
                    return Err("duplicate --policy".into());
                }
            }
            "--json" if check && !json => json = true,
            value if value.starts_with('-') => {
                return Err(format!("unsupported source macro option '{value}'"));
            }
            value => {
                if input.replace(PathBuf::from(value)).is_some() {
                    return Err("only one source file is supported".into());
                }
            }
        }
        i += 1;
    }
    Ok(Args {
        check,
        input: input.ok_or("missing source file")?,
        package: package.unwrap_or_else(|| "@local/source".into()),
        policy,
    })
}
struct InputError {
    stage: Stage,
    message: String,
}
impl From<io::Error> for InputError {
    fn from(error: io::Error) -> Self {
        Self {
            stage: Stage::Io,
            message: error.to_string(),
        }
    }
}
fn read_regular(path: &Path, max: usize) -> Result<String, InputError> {
    if !fs::symlink_metadata(path)
        .map_err(InputError::from)?
        .is_file()
    {
        return Err(InputError {
            stage: Stage::Io,
            message: "source macros require a regular UTF-8 file without final symlinks".into(),
        });
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x0020_0000);
    }
    let file = options.open(path).map_err(InputError::from)?;
    let metadata = file.metadata().map_err(InputError::from)?;
    if !metadata.is_file() || metadata.len() > max as u64 {
        return Err(InputError {
            stage: Stage::Resource,
            message: "source macro file byte limit exceeded".into(),
        });
    }
    let mut bytes = Vec::new();
    file.take(max as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(InputError::from)?;
    if bytes.len() > max {
        return Err(InputError {
            stage: Stage::Resource,
            message: "source macro file byte limit exceeded".into(),
        });
    }
    String::from_utf8(bytes).map_err(|_| InputError {
        stage: Stage::Io,
        message: "source macro files must be UTF-8".into(),
    })
}
struct Output {
    bytes: Vec<u8>,
}
impl Write for Output {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self
            .bytes
            .len()
            .checked_add(bytes.len())
            .is_none_or(|n| n > MAX_JSON)
        {
            return Err(io::Error::other("source macro output byte limit exceeded"));
        }
        self.bytes
            .try_reserve(bytes.len())
            .map_err(|_| io::Error::other("source macro output allocation failed"))?;
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn write_json(value: &impl Serialize) -> Result<(), String> {
    let mut output = Output { bytes: Vec::new() };
    serde_json::to_writer(&mut output, value).map_err(|e| e.to_string())?;
    output.write_all(b"\n").map_err(|e| e.to_string())?;
    io::stdout()
        .lock()
        .write_all(&output.bytes)
        .map_err(|e| e.to_string())
}
#[derive(Serialize)]
struct Success<'a> {
    schema: &'static str,
    status: &'static str,
    scope: &'static str,
    package: &'a str,
    source_blake3: String,
    expanded_blake3: String,
    source_bytes: usize,
    expanded_bytes: usize,
    macro_definitions: usize,
    invocations: usize,
    expanded_source: &'a str,
    source_map: &'a [forge::SourceMapSegment],
    audits: &'a [forge::InvocationAudit],
}
impl<'a> From<&'a ExpandedChecked> for Success<'a> {
    fn from(value: &'a ExpandedChecked) -> Self {
        Self {
            schema: forge::SCHEMA,
            status: "expanded_checked",
            scope: "tier1_source",
            package: value.package(),
            source_blake3: blake3::Hash::from_bytes(value.original_hash())
                .to_hex()
                .to_string(),
            expanded_blake3: blake3::Hash::from_bytes(value.expanded_hash())
                .to_hex()
                .to_string(),
            source_bytes: value.original_source().len(),
            expanded_bytes: value.expanded_source().len(),
            macro_definitions: value.macro_definitions(),
            invocations: value.invocations(),
            expanded_source: value.expanded_source(),
            source_map: value.source_map(),
            audits: value.audits(),
        }
    }
}
#[derive(Serialize)]
struct Rejected<'a> {
    schema: &'static str,
    status: &'static str,
    stage: Stage,
    message: &'a str,
    source_blake3: Option<String>,
    expanded_source: Option<&'a str>,
    diagnostics: &'a [Diagnostic],
    source_map: [(); 0],
}
fn reject(
    check: bool,
    stage: Stage,
    message: &str,
    source: Option<&str>,
    expanded: Option<&str>,
    diagnostics: &[Diagnostic],
) -> Result<(), String> {
    if check {
        write_json(&Rejected {
            schema: forge::SCHEMA,
            status: "rejected",
            stage,
            message,
            source_blake3: source.map(|s| blake3::hash(s.as_bytes()).to_hex().to_string()),
            expanded_source: expanded,
            diagnostics,
            source_map: [],
        })?;
    }
    Err(message.chars().take(4096).collect())
}
pub fn run(arguments: Vec<String>) -> Result<(), String> {
    if arguments.iter().any(|a| a == "--help" || a == "-h") {
        print!("{HELP}");
        return Ok(());
    }
    let check = arguments.first().is_some_and(|a| a == "check");
    let args = match args(&arguments) {
        Ok(args) => args,
        Err(e) => return reject(check, Stage::Config, &e, None, None, &[]),
    };
    let limits = CompileLimits::default();
    let source = match read_regular(&args.input, limits.max_source_bytes) {
        Ok(s) => s,
        Err(e) => return reject(args.check, e.stage, &e.message, None, None, &[]),
    };
    let policy = if let Some(path) = &args.policy {
        let text = match read_regular(path, MAX_POLICY) {
            Ok(s) => s,
            Err(e) => {
                return reject(
                    args.check,
                    Stage::Config,
                    &e.message,
                    Some(&source),
                    None,
                    &[],
                );
            }
        };
        match serde_json::from_str::<PolicyConfig>(&text) {
            Ok(policy) => policy,
            Err(e) => {
                return reject(
                    args.check,
                    Stage::Config,
                    &e.to_string(),
                    Some(&source),
                    None,
                    &[],
                );
            }
        }
    } else {
        PolicyConfig::default()
    };
    match forge::expand_source(
        &source,
        &CompileConfig {
            package: args.package,
            policy,
        },
        limits,
    ) {
        Ok(artifact) => {
            if args.check {
                if let Err(error) = write_json(&Success::from(&artifact)) {
                    return reject(true, Stage::Resource, &error, Some(&source), None, &[]);
                }
                Ok(())
            } else {
                io::stdout()
                    .lock()
                    .write_all(artifact.expanded_source().as_bytes())
                    .map_err(|e| e.to_string())
            }
        }
        Err(error) => reject(
            args.check,
            error.stage(),
            error.message(),
            Some(&source),
            error.expanded_source(),
            error.diagnostics(),
        ),
    }
}
