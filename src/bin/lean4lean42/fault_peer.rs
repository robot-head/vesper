//! Closed transport faults; no mode supplies successful external replay evidence.
use super::{Error, ErrorKind};
use std::io::{self, Write};

const MARKER: &[u8] = b"lean4lean42-transport-marker";

fn failure(error: io::Error) -> Error {
    Error::new(ErrorKind::Io, format!("fault peer IO: {error}"))
}
fn frame(output: &mut impl Write, payload: &[u8]) -> io::Result<()> {
    output.write_all(&(payload.len() as u32).to_le_bytes())?;
    output.write_all(payload)?;
    output.flush()
}
fn park() -> ! {
    loop {
        std::thread::park();
    }
}

pub fn run(mode: &str) -> Result<(), Error> {
    match mode {
        "malformed" => frame(&mut io::stdout().lock(), b"[").map_err(failure),
        "oversize" => {
            let mut output = io::stdout().lock();
            output.write_all(&65537u32.to_le_bytes()).map_err(failure)?;
            output.flush().map_err(failure)
        }
        "truncated_header" => {
            let mut output = io::stdout().lock();
            output.write_all(&[5, 0]).map_err(failure)?;
            output.flush().map_err(failure)
        }
        "truncated_payload" => {
            let mut output = io::stdout().lock();
            output.write_all(&5u32.to_le_bytes()).map_err(failure)?;
            output.write_all(b"[").map_err(failure)?;
            output.flush().map_err(failure)
        }
        "wrong_token" => frame(
            &mut io::stdout().lock(),
            br#"[1,0,"11111111111111111111111111111111",3,2,"4.24.0","vesper.lean4lean42/1"]"#,
        )
        .map_err(failure),
        "stall" => park(),
        "stderr_flood" => {
            let mut output = io::stderr().lock();
            for _ in 0..64 {
                output.write_all(&[b'x'; 4096]).map_err(failure)?;
            }
            output.write_all(b"x").map_err(failure)?;
            output.flush().map_err(failure)?;
            drop(output);
            park()
        }
        "crash" => std::process::exit(23),
        "short_writes" => {
            let mut output = io::stdout().lock();
            for byte in (MARKER.len() as u32).to_le_bytes().iter().chain(MARKER) {
                output.write_all(&[*byte]).map_err(failure)?;
                output.flush().map_err(failure)?;
            }
            Ok(())
        }
        "queue_pressure" => {
            let mut output = io::stdout().lock();
            for _ in 0..64 {
                frame(&mut output, MARKER).map_err(failure)?;
            }
            Ok(())
        }
        "queue_cancel" => {
            let mut output = io::stdout().lock();
            loop {
                frame(&mut output, MARKER).map_err(failure)?;
            }
        }
        _ => Err(Error::new(
            ErrorKind::InvalidConfig,
            "unknown closed fault-peer mode",
        )),
    }
}
