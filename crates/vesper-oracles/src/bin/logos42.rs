//! Bound Logos rechecking of the three fixed Vesper models.
#![forbid(unsafe_code)]
#[path = "logos42/mod.rs"]
pub mod driver;
fn main() -> std::process::ExitCode {
    driver::cli::main()
}
