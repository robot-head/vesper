//! Vesper tool subcommands that live outside the compiler binary
//! (self-hosting SH0 lane 2, #1124).
//!
//! `vesper <tool> ARGS...` runs the `vesper-<tool>` executable built from this
//! crate (see `src/tool_commands.rs` in the root package). Each tool links the
//! libraries it needs, so the compiler's own closure, which Stage 2 must port,
//! no longer contains them. Tools that need compiler services (checking,
//! native batches, interfaces) call the `vesper` library.
//!
//! The libraries are the ones the compiler linked before the split, the frozen
//! Rust seed copies: under the owner's parity rule no tool switches to a
//! Vesper port here.
pub mod audit;
pub mod bench;
pub mod grammar;
pub mod json;
pub mod publish;
pub mod resolve;
pub mod sbom;
pub mod tee;

use std::process;

/// Stack for the tool thread, the same as the compiler CLI's (see
/// `CLI_STACK_BYTES` in the root `src/main.rs`): tools check and lower source.
const TOOL_STACK_BYTES: usize = 64 * 1024 * 1024;

/// Run one tool's entry point the way the compiler ran it: on a large-stack
/// thread, with errors printed as `vesper: <error>` and exit status 1.
pub fn main(run: fn(Vec<String>) -> Result<(), String>) {
    let result = std::thread::Builder::new()
        .name("vesper".to_owned())
        .stack_size(TOOL_STACK_BYTES)
        .spawn(move || run(std::env::args().skip(1).collect()))
        .expect("spawn the vesper tool thread")
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
