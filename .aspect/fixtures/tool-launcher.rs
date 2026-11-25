// Forward a tool invocation and its working directory to an AXL test fixture.
use std::env;
use std::io;
use std::path::Path;
use std::process::{Command, ExitCode};

fn run() -> io::Result<ExitCode> {
    let required = |name| {
        env::var_os(name).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, name))
    };
    let mut args = env::args_os();
    let executable = args.next().expect("fixture executable name");
    let cwd = env::current_dir()?;
    let tool = Path::new(&executable)
        .file_name()
        .expect("fixture tool name");
    let status = Command::new(required("VESPER_FIXTURE_ASPECT")?)
        .current_dir(required("VESPER_FIXTURE_SOURCE")?)
        .env("PATH", required("VESPER_FIXTURE_PATH")?)
        .args(["ci", "tests", "internal", "tool-fixture", "--"])
        .arg(required("VESPER_FIXTURE_CASE")?)
        .arg(tool)
        .arg(cwd)
        .arg(executable)
        .args(args)
        .status()?;
    Ok(ExitCode::from(status.code().unwrap_or(1) as u8))
}

fn main() -> ExitCode {
    run().unwrap_or_else(|error| {
        eprintln!("Aspect fixture launcher: {error}");
        ExitCode::FAILURE
    })
}
