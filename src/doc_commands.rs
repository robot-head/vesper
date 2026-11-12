//! Filesystem/CLI adapter for pure documentation tooling.
use serde::Serialize;
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
};
pub use vesper::tooling::doc::document_source;
use vesper_doc::{Document, Limits};
pub const HELP: &str = "Usage: vesper doc FILE [--json] [-o OUTPUT] [--query QUERY.json]\n\nCheck source and generate deterministic local API documentation.\nDefault: escaped static HTML on stdout. --json: typed JSON metadata and search results.\n-o creates a new local output file; existing files are preserved. --query reads a full structural JSON query (no wildcard/generic inference).\nSource is limited to 128 KiB and bounded tokens/nesting/items/types; query to 32 KiB; output to 2 MiB.\nGenerated private Pact helpers are excluded. Layouts describe the native eight-byte ABI, not wire formats.\nNo audited proof evidence is accepted by this command; documentation is unverified. No network, JIT or doctest execution.\n";

fn read_file(path: &str, limit: usize) -> Result<String, String> {
    if !fs::symlink_metadata(path)
        .map_err(|e| e.to_string())?
        .is_file()
    {
        return Err("documentation input must be a regular file without final symlinks".into());
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
    let file = options.open(path).map_err(|e| e.to_string())?;
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.len() > limit as u64 {
        return Err("documentation input byte/file limit exceeded".into());
    }
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > limit {
        return Err("documentation input byte limit exceeded".into());
    }
    String::from_utf8(bytes).map_err(|_| "documentation input is not UTF-8".into())
}
#[derive(Serialize)]
struct Report {
    schema_version: u32,
    status: &'static str,
    document: Document,
    search: Option<vesper_doc::SearchResults>,
}
struct Rendered {
    bytes: Vec<u8>,
    output: Option<String>,
}
fn generate(args: Vec<String>) -> Result<Option<Rendered>, String> {
    let mut input = None;
    let mut output = None;
    let mut query = None;
    let mut json = false;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                print!("{HELP}");
                return Ok(None);
            }
            "--json" => json = true,
            "-o" => {
                if output.is_some() {
                    return Err("duplicate -o".into());
                }
                output = Some(
                    args.next()
                        .filter(|p| !p.starts_with('-'))
                        .ok_or("-o requires a new output path")?,
                );
            }
            "--query" => {
                if query.is_some() {
                    return Err("duplicate --query".into());
                }
                query = Some(
                    args.next()
                        .filter(|p| !p.starts_with('-'))
                        .ok_or("--query requires a query JSON file")?,
                );
            }
            _ if arg.starts_with('-') => return Err(format!("unknown doc option '{arg}'")),
            _ => {
                if input.replace(arg).is_some() {
                    return Err("doc accepts one source file".into());
                }
            }
        }
    }
    let input = input.ok_or("doc requires a source file; see doc --help")?;
    let limits = Limits::default();
    let source = read_file(&input, limits.max_source_bytes)?;
    let document = document_source(&source, limits)?;
    let search = if let Some(path) = query {
        let text = read_file(&path, 32 * 1024)?;
        let query = vesper_doc::parse_query_json(&text, limits)
            .map_err(|e| format!("invalid structural query: {e}"))?;
        Some(vesper_doc::search(&query, &document.functions, limits)?)
    } else {
        None
    };
    let report = Report {
        schema_version: 1,
        status: "valid",
        document,
        search,
    };
    let bytes = if json {
        vesper_doc::to_json(&report, limits.max_output_bytes)?
    } else {
        vesper_doc::render_html(&report, limits)?
    };
    Ok(Some(Rendered { bytes, output }))
}
pub fn run(args: Vec<String>) -> Result<(), String> {
    let json = args.iter().any(|a| a == "--json");
    let result = (|| {
        if let Some(Rendered { bytes, output }) = generate(args)? {
            if let Some(path) = output {
                let mut file = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&path)
                    .map_err(|e| format!("cannot create new output: {e}"))?;
                if let Err(error) = file.write_all(&bytes).and_then(|_| file.sync_all()) {
                    drop(file);
                    let _ = fs::remove_file(&path);
                    return Err(error.to_string());
                }
            } else {
                std::io::stdout()
                    .lock()
                    .write_all(&bytes)
                    .map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    })()
    .map_err(|mut message: String| {
        if message.len() > 4096 {
            let mut end = 4096;
            while !message.is_char_boundary(end) {
                end -= 1;
            }
            message.truncate(end);
            message.push_str(" [truncated]");
        }
        message
    });
    if json && let Err(message) = &result {
        println!(
            "{}",
            serde_json::json!({"schema_version":1,"status":"invalid","error":message})
        );
    }
    result
}
