//! Closed, transport-only fault peers for the developer oracle binary.
//! No mode computes arithmetic, supplies an oracle value, or emits a run report.
use crate::oracle::{FailureKind, HarnessError};
use std::io::{self, Write};

const MARKER: &[u8] = b"oracle42-transport-marker";

fn io_error(error: io::Error) -> HarnessError {
    HarnessError {
        kind: FailureKind::Io,
        detail: format!("fault peer IO: {error}"),
    }
}

fn frame(output: &mut impl Write, payload: &[u8]) -> io::Result<()> {
    // All payloads are fixed constants below, independent of caller data.
    output.write_all(&(payload.len() as u32).to_le_bytes())?;
    output.write_all(payload)?;
    output.flush()
}

fn park() -> ! {
    loop {
        std::thread::park();
    }
}

pub fn run(mode: &str) -> Result<(), HarnessError> {
    match mode {
        "malformed" => frame(&mut io::stdout().lock(), b"[").map_err(io_error),
        "oversize" => {
            let mut output = io::stdout().lock();
            output
                .write_all(&65537u32.to_le_bytes())
                .map_err(io_error)?;
            output.flush().map_err(io_error)
        }
        "truncated_header" => {
            let mut output = io::stdout().lock();
            output.write_all(&[5, 0]).map_err(io_error)?;
            output.flush().map_err(io_error)
        }
        "truncated_payload" => {
            let mut output = io::stdout().lock();
            output.write_all(&5u32.to_le_bytes()).map_err(io_error)?;
            output.write_all(b"[").map_err(io_error)?;
            output.flush().map_err(io_error)
        }
        "stall" => park(),
        "stderr_flood" => {
            let mut output = io::stderr().lock();
            for _ in 0..16 {
                output.write_all(&[b'x'; 4096]).map_err(io_error)?;
            }
            output.write_all(b"x").map_err(io_error)?;
            output.flush().map_err(io_error)?;
            drop(output);
            park()
        }
        "reordered" => frame(
            &mut io::stdout().lock(),
            br#"[1,"invalid_request",1,"schema"]"#,
        )
        .map_err(io_error),
        "bad_bye" => {
            frame(&mut io::stdout().lock(), br#"[1,"bye",1,0,0,0,0,0,0]"#).map_err(io_error)
        }
        "short_writes" => {
            let mut output = io::stdout().lock();
            for byte in (MARKER.len() as u32).to_le_bytes().iter().chain(MARKER) {
                output.write_all(&[*byte]).map_err(io_error)?;
                output.flush().map_err(io_error)?;
            }
            Ok(())
        }
        "crash" => std::process::exit(23),
        _ => Err(HarnessError {
            kind: FailureKind::InvalidConfig,
            detail: "unknown closed fault-peer mode".into(),
        }),
    }
}
