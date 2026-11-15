//! Compatibility alias for the Logos external checker.
#![forbid(unsafe_code)]
#[path = "logos42/mod.rs"]
pub mod driver;
fn main() -> std::process::ExitCode {
    driver::cli::main()
}
