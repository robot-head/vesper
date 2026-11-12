//! Embedding demo: compile `ticket_tally.ves` as a native session, run it
//! under the owned Pulse runtime with a kernel io_uring read when the kernel
//! allows it, and emit x86_64/AArch64 session objects.
//!
//! `cargo run --example native-pulse -- [out-dir]`
#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
fn main() -> Result<(), String> {
    use vesper::native::session::*;
    use vesper_fiber_abi::pulse::{NativeRuntime, NativeTask};
    use vesper_pulse::io_driver::IoDriver;

    let source = include_str!("ticket_tally.ves");
    let parsed = vesper::parser::parse(source);
    let checked = vesper::check::check(&parsed.module);
    if !parsed.diagnostics.is_empty() || !checked.diagnostics.is_empty() {
        return Err(format!(
            "{:?} {:?}",
            parsed.diagnostics, checked.diagnostics
        ));
    }
    let world = checked.world;
    let bindings = vec![EffectBinding {
        effect: "Pulse".into(),
        ops: vec![
            OperationBinding::new(
                "pause",
                Operation::Yield,
                "Ticked",
                "TickFailed",
                Cancellation::Constructor("TickCancelled".into()),
            ),
            OperationBinding::new(
                "read",
                Operation::Read,
                "Got",
                "IoFailed",
                Cancellation::Constructor("IoCancelled".into()),
            ),
        ],
    }];
    let session = NativeSession::compile(&world, "main", &bindings)?;
    println!("frame: {:?}", session.plan().frame());

    match IoDriver::<NativeTask>::new(8) {
        Ok(driver) => {
            let path = std::env::temp_dir().join(format!("native-pulse-{}", std::process::id()));
            let bytes: Vec<u8> = [40u64, 2, 500, 7]
                .iter()
                .flat_map(|n| n.to_le_bytes())
                .collect();
            std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
            let file = std::fs::File::open(&path).map_err(|e| e.to_string())?;
            let mut runtime = NativeRuntime::new(driver);
            session
                .spawn(&mut runtime, Some(file))
                .map_err(|e| e.to_string())?;
            // Drain to completion; an IncompleteShutdown keeps its owners
            // and is reported as a failure, never as success.
            let drained = runtime
                .drain(64, true)
                .map_err(|e| format!("incomplete shutdown: {}", e.reason))?;
            let _ = std::fs::remove_file(&path);
            for completed in &drained.completed {
                println!(
                    "io_uring session: {:?}",
                    session.decode_completed(&world, completed)?
                );
            }
        }
        Err(error) => println!("io_uring unavailable ({error}); no kernel run"),
    }

    let out = std::env::args()
        .nth(1)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    for target in ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"] {
        let (bytes, clif) = object(&world, "main", &bindings, Some(target))?;
        let path = out.join(format!("ticket_tally-{target}.o"));
        std::fs::write(&path, &bytes).map_err(|e| e.to_string())?;
        std::fs::write(path.with_extension("clif"), clif).map_err(|e| e.to_string())?;
        println!(
            "{}: {} bytes, entry {}",
            path.display(),
            bytes.len(),
            entry_symbol("main")
        );
    }
    Ok(())
}

#[cfg(not(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
)))]
fn main() {
    eprintln!("native sessions run on Linux x86_64/AArch64 only");
}
