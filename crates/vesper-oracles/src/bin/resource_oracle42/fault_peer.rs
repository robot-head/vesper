//! Negative transport fixture. It never evaluates or attests a model result.
use std::{
    io::{self, Read, Write},
    thread::sleep,
    time::Duration,
};

const IDENTITY_TAIL: &[u8] = br#"","4.35.0","eb6cafb7290e8bd5f2937522d8d804f58f02bafc",[4194304,262144,8,1000000,131072,2048,65,256,9999999999,20,4096,67108864]]"#;
const EMPTY_BYE: &[u8] = br#"[1,"bye",0,[0,0,0,0,0,0,0]]"#;
const WAIT: Duration = Duration::from_secs(10);

fn frame(output: &mut impl Write, payload: &[u8]) -> io::Result<()> {
    let len = u32::try_from(payload.len()).map_err(io::Error::other)?;
    output.write_all(&len.to_le_bytes())?;
    output.write_all(payload)?;
    output.flush()
}

fn request(input: &mut impl Read) -> io::Result<()> {
    let mut header = [0; 4];
    input.read_exact(&mut header)?;
    let length = u32::from_le_bytes(header);
    if length > 4_194_304 {
        return Err(io::Error::other("oversized negative-peer request"));
    }
    if io::copy(&mut input.take(u64::from(length)), &mut io::sink())? != u64::from(length) {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "incomplete negative-peer request",
        ));
    }
    Ok(())
}

fn require_eof(input: &mut impl Read) -> io::Result<()> {
    if input.read(&mut [0])? != 0 {
        return Err(io::Error::other("unexpected negative-peer request"));
    }
    Ok(())
}

#[cfg(unix)]
fn stall_after_eof() -> io::Result<u8> {
    use std::{
        os::unix::process::CommandExt,
        process::{Command, Stdio},
    };

    // Replace the direct child and release its stdout pipe without exiting.
    Err(Command::new(std::env::current_exe()?)
        .args(["--fault-peer", "idle"])
        .stdout(Stdio::null())
        .exec())
}

#[cfg(not(unix))]
fn stall_after_eof() -> io::Result<u8> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "EOF stall fixture requires Unix",
    ))
}

pub fn run(kind: &str) -> io::Result<u8> {
    if kind == "idle" {
        sleep(WAIT);
        return Ok(0);
    }
    let mut input = io::stdin().lock();
    let mut output = io::stdout().lock();
    if kind == "stdout_noise" {
        output.write_all(b"noise")?;
        output.flush()?;
        return Ok(0);
    }

    let identity: &[u8] = if kind == "wrong_ready" {
        b"wrong"
    } else {
        b"vesper.resource-model42/1"
    };
    frame(
        &mut output,
        &[br#"[1,"ready",""#, identity, IDENTITY_TAIL].concat(),
    )?;
    match kind {
        "wrong_ready" => return Ok(0),
        "blocked_stdin" => {
            sleep(WAIT);
            return Ok(0);
        }
        "bad_bye" | "trailing" | "exit_failure" | "no_bye" | "bye_only" | "bye_eof_stall" => {
            require_eof(&mut input)?;
            if kind == "no_bye" {
                return Ok(0);
            }
            let bye: &[u8] = if kind == "bad_bye" {
                br#"[1,"bye",1,[1,1,0,0,0,0,0]]"#
            } else {
                EMPTY_BYE
            };
            frame(&mut output, bye)?;
            return match kind {
                "trailing" => {
                    output.write_all(b"x")?;
                    output.flush()?;
                    Ok(0)
                }
                "exit_failure" => Ok(9),
                "bye_eof_stall" => {
                    drop(output);
                    stall_after_eof()
                }
                _ => Ok(0),
            };
        }
        _ => {}
    }

    request(&mut input)?;
    let semantic: &[u8] = match kind {
        "malformed" => b"{}",
        "unknown_constructor" => br#"[1,"result",0,"validation_error",[["invented"]]]"#,
        "reordered" => br#"[1,"result",1,"validation_error",[["invalidLimits"]]]"#,
        "oversize" => {
            output.write_all(&262_145_u32.to_le_bytes())?;
            output.flush()?;
            return Ok(0);
        }
        "zero_length" => {
            output.write_all(&0_u32.to_le_bytes())?;
            output.flush()?;
            return Ok(0);
        }
        "truncated_header" => {
            output.write_all(&[5, 0])?;
            output.flush()?;
            return Ok(0);
        }
        "truncated_payload" => {
            output.write_all(&50_u32.to_le_bytes())?;
            output.write_all(b"[1,")?;
            output.flush()?;
            return Ok(0);
        }
        "flood" => {
            let mut stderr = io::stderr().lock();
            for _ in 0..1024 {
                if let Err(error) = stderr
                    .write_all(&[b'x'; 4096])
                    .and_then(|()| stderr.flush())
                {
                    if error.kind() != io::ErrorKind::BrokenPipe {
                        return Err(error);
                    }
                    break;
                }
            }
            sleep(WAIT);
            return Ok(0);
        }
        "stall" => {
            sleep(WAIT);
            return Ok(0);
        }
        "crash" => return Ok(7),
        _ => return Err(io::Error::other("unknown negative transport fixture")),
    };
    frame(&mut output, semantic)?;
    // Keep semantic errors separate from an early peer exit.
    require_eof(&mut input)?;
    Ok(0)
}
