use std::error::Error;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use vesper_loom_audit::{Limits, Outcome, audit, parse_catalog, parse_policy, parse_solution};

fn read(path: &Path, limits: &Limits) -> Result<Vec<u8>, Box<dyn Error>> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(limits.max_input_bytes as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limits.max_input_bytes {
        return Err("input file exceeds audit byte budget".into());
    }
    Ok(bytes)
}
fn run() -> Result<i32, Box<dyn Error>> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if args.len() != 3 {
        return Err("usage: audit Loom.lock advisories.json policy.json".into());
    }
    let limits = Limits::default();
    let solution = parse_solution(&read(Path::new(&args[0]), &limits)?, &limits)?;
    let catalog = parse_catalog(&read(Path::new(&args[1]), &limits)?, &limits)?;
    let policy = parse_policy(&read(Path::new(&args[2]), &limits)?, &limits)?;
    let report = audit(&solution, &catalog, &policy, &limits)?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(match report.outcome {
        Outcome::Clean => 0,
        Outcome::Findings => 1,
        Outcome::Unknown => 2,
    })
}
fn main() {
    match run() {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(3);
        }
    }
}
