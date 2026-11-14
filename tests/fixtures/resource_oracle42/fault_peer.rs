//! Negative transport peer only: never computes or reports a successful model evaluation.
use std::io::{self, Read, Write};
use std::time::Duration;

const READY: &[u8] = br#"[1,"ready","vesper.resource-model42/1","4.24.0","797c613eb9b6d4ec95db23e3e00af9ac6657f24b",[4194304,262144,8,1000000,131072,2048,65,256,9999999999,20,4096,67108864]]"#;
fn frame(out: &mut impl Write, payload: &[u8]) {
    out.write_all(&(payload.len() as u32).to_le_bytes())
        .unwrap();
    out.write_all(payload).unwrap();
    out.flush().unwrap();
}
fn request() {
    let mut input = io::stdin().lock();
    let mut header = [0; 4];
    input.read_exact(&mut header).unwrap();
    let length = u32::from_le_bytes(header) as usize;
    assert!(length <= 4_194_304);
    let mut payload = vec![0; length];
    input.read_exact(&mut payload).unwrap();
}
fn main() {
    let kind = std::env::args().nth(1).unwrap();
    let mut output = io::stdout().lock();
    if kind == "wrong_ready" {
        frame(&mut output, br#"[1,"ready","wrong","4.24.0","797c613eb9b6d4ec95db23e3e00af9ac6657f24b",[4194304,262144,8,1000000,131072,2048,65,256,9999999999,20,4096,67108864]]"#);
        return;
    }
    if kind == "stdout_noise" {
        output.write_all(b"noise").unwrap();
        output.flush().unwrap();
        return;
    }
    frame(&mut output, READY);
    if kind == "blocked_stdin" {
        std::thread::sleep(Duration::from_secs(10));
        return;
    }
    if kind == "bad_bye" || kind == "trailing" || kind == "exit_failure" || kind == "no_bye" {
        let mut input = Vec::new();
        io::stdin().read_to_end(&mut input).unwrap();
        assert!(input.is_empty());
        match kind.as_str() {
            "no_bye" => return,
            "bad_bye" => frame(&mut output, br#"[1,"bye",1,[1,1,0,0,0,0,0]]"#),
            _ => frame(&mut output, br#"[1,"bye",0,[0,0,0,0,0,0,0]]"#),
        }
        if kind == "trailing" {
            output.write_all(b"x").unwrap();
            output.flush().unwrap();
        }
        if kind == "exit_failure" {
            std::process::exit(9);
        }
        return;
    }
    request();
    match kind.as_str() {
        "malformed" => frame(&mut output, b"{}"),
        "unknown_constructor" => frame(
            &mut output,
            br#"[1,"result",0,"validation_error",[["invented"]]]"#,
        ),
        "reordered" => frame(
            &mut output,
            br#"[1,"result",1,"validation_error",[["invalidLimits"]]]"#,
        ),
        "oversize" => {
            output.write_all(&262_145_u32.to_le_bytes()).unwrap();
            output.flush().unwrap();
        }
        "zero_length" => {
            output.write_all(&0_u32.to_le_bytes()).unwrap();
            output.flush().unwrap();
        }
        "truncated_header" => {
            output.write_all(&[5, 0]).unwrap();
            output.flush().unwrap();
        }
        "truncated_payload" => {
            output.write_all(&50_u32.to_le_bytes()).unwrap();
            output.write_all(b"[1,").unwrap();
            output.flush().unwrap();
        }
        "flood" => {
            let mut error = io::stderr().lock();
            for _ in 0..1024 {
                if error.write_all(&[b'x'; 4096]).is_err() {
                    break;
                }
            }
            std::thread::sleep(Duration::from_secs(10));
        }
        "stall" => std::thread::sleep(Duration::from_secs(10)),
        "crash" => std::process::exit(7),
        _ => panic!("unknown negative transport fixture"),
    }
}
