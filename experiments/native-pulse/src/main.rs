//! `cargo run -- [--out DIR] [--fibers N]`
//!
//! 1. JIT-compiles the prototype entries for the host (x86_64 or AArch64).
//! 2. Schedules N native fibers on the unmodified `vesper_pulse` scheduler.
//!    Each fiber yields, then reads 8 bytes from a local fixture file. The
//!    real io_uring driver is used when the kernel allows it; otherwise the
//!    run says so and uses an in-process fixture driver.
//! 3. Writes x86_64 and AArch64 relocatable objects (and their CLIF) to DIR.
//! 4. Shuts native fibers down explicitly through `NativeRuntime`:
//!    * a clean shutdown that cancels one fiber before start, one after a
//!      yield and one during I/O;
//!    * a recoverable incomplete shutdown: fibers whose cancellation edge
//!      parks again exhaust a small drain budget, the runtime comes back with
//!      every owner, and a second drain finishes them;
//!    * when io_uring is available, a kernel read on an idle pipe cancelled
//!      during I/O.
#[cfg(not(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
)))]
fn main() {
    eprintln!("the native Pulse prototype runs on Linux x86_64/AArch64 only");
    std::process::exit(2);
}

#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
fn main() {
    if let Err(e) = linux::run() {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
mod linux {
    use std::fs::File;
    use std::io::{self, Write};
    use std::path::PathBuf;
    use std::rc::Rc;
    use vesper_fiber_abi::abi::exit;
    use vesper_fiber_abi::evidence::EvidenceRegistry;
    use vesper_fiber_abi::evidence::EvidenceVector;
    use vesper_fiber_abi::prototype::{self, JitPrototype, OptLevel};
    use vesper_fiber_abi::pulse::{Completed, Incomplete, NativeRuntime, NativeTask, TaskRecord};
    use vesper_pulse::io_driver::{IoDriver, Request};
    use vesper_pulse::scheduler::{Driver, Finished, PollError, Scheduler};

    /// Completes reads synchronously from the file contents (fixture only).
    #[derive(Default)]
    struct FixtureDriver {
        requests: Vec<Request<NativeTask>>,
        cancelled: bool,
    }
    impl Driver<NativeTask> for FixtureDriver {
        fn submit(
            &mut self,
            r: Request<NativeTask>,
        ) -> Result<(), (io::Error, Request<NativeTask>)> {
            self.requests.push(r);
            Ok(())
        }
        fn poll(&mut self, _: bool) -> Result<Vec<Finished<NativeTask>>, PollError<NativeTask>> {
            use std::os::unix::fs::FileExt;
            Ok(self
                .requests
                .drain(..)
                .map(|mut request| {
                    let result = if self.cancelled {
                        Err(io::Error::from_raw_os_error(125))
                    } else {
                        request.file.read_at(&mut request.buffer, request.offset)
                    };
                    Finished { request, result }
                })
                .collect())
        }
        fn pending(&self) -> usize {
            self.requests.len()
        }
        fn cancel_all(&mut self) -> io::Result<()> {
            self.cancelled = true;
            Ok(())
        }
    }

    fn schedule<D: Driver<NativeTask>>(
        mut s: Scheduler<NativeTask, D>,
        jit: &JitPrototype,
        fibers: u64,
        path: &PathBuf,
    ) -> Result<Vec<Completed>, String> {
        let mut registry = EvidenceRegistry::new();
        let (evidence, _) =
            prototype::pulse_read_evidence(&mut registry).map_err(|e| e.to_string())?;
        for arg in 0..fibers {
            let fiber = jit
                .spawn_pulse_read(arg, evidence.clone())
                .map_err(|e| e.to_string())?;
            s.spawn(NativeTask::new(
                fiber,
                Some(File::open(path).map_err(|e| e.to_string())?),
            ));
        }
        let mut ticks = 0u64;
        while !s.is_idle() {
            s.tick(true).map_err(|e| e.to_string())?;
            ticks += 1;
        }
        println!("scheduler ticks: {ticks}");
        let mut done = vec![];
        while let Some(bytes) = s.take_completed() {
            done.push(Completed::decode(&bytes).ok_or("bad completion")?);
        }
        if let Some(f) = s.take_failure() {
            return Err(f);
        }
        Ok(done)
    }

    fn status(s: u64) -> &'static str {
        match s {
            exit::COMPLETE => "complete",
            exit::FAILED => "failed",
            exit::CANCELLED => "cancelled",
            _ => "abi-violation",
        }
    }

    fn print_records(records: &[TaskRecord]) {
        for r in records {
            println!(
                "  fiber {}: {} result={} parks={} cancelled={:?}",
                r.fiber_id,
                status(r.status),
                r.result,
                r.parks,
                r.cancelled
            );
        }
    }

    fn shutdown_demo(jit: &JitPrototype, path: &PathBuf) -> Result<bool, String> {
        let mut registry = EvidenceRegistry::new();
        let (evidence, _) =
            prototype::pulse_read_evidence(&mut registry).map_err(|e| e.to_string())?;
        let open = || File::open(path).map_err(|e| e.to_string());
        let spawn_read = |rt: &mut NativeRuntime<FixtureDriver>, arg, ev: &Rc<EvidenceVector>| {
            let fiber = jit
                .spawn_pulse_read(arg, ev.clone())
                .map_err(|e| e.to_string())?;
            Ok::<u64, String>(rt.spawn(fiber, Some(open()?)))
        };
        let mut ok = true;

        // Clean shutdown.
        let mut rt = NativeRuntime::new(FixtureDriver::default());
        spawn_read(&mut rt, 1, &evidence)?;
        spawn_read(&mut rt, 2, &evidence)?;
        for _ in 0..3 {
            rt.tick(false).map_err(|e| e.to_string())?;
        }
        spawn_read(&mut rt, 3, &evidence)?;
        let drained = rt
            .shutdown(64, false)
            .map_err(|e| format!("clean shutdown incomplete: {}", e.reason))?;
        println!("clean shutdown: {} fibers resolved", drained.records.len());
        print_records(&drained.records);
        ok &= drained.records.len() == 3
            && drained.records.iter().all(|r| r.status == exit::CANCELLED);

        // Recoverable incomplete shutdown.
        let mut rt = NativeRuntime::new(FixtureDriver::default());
        for _ in 0..2 {
            let fiber = jit
                .spawn_cancel_repark(evidence.clone())
                .map_err(|e| e.to_string())?;
            rt.spawn(fiber, None);
        }
        rt.tick(false).map_err(|e| e.to_string())?;
        rt.tick(false).map_err(|e| e.to_string())?;
        let incomplete = match rt.shutdown(2, false) {
            Ok(_) => return Err("budget of 2 ticks unexpectedly sufficed".into()),
            Err(incomplete) => incomplete,
        };
        let pending = match &incomplete.reason {
            Incomplete::BudgetExhausted { pending } => pending.clone(),
            other => return Err(format!("unexpected incomplete shutdown: {other}")),
        };
        println!(
            "incomplete shutdown: {} (pending owners returned: {pending:?})",
            incomplete.reason
        );
        let drained = incomplete
            .runtime
            .drain(64, false)
            .map_err(|e| format!("second drain incomplete: {}", e.reason))?;
        println!("second drain: {} fibers resolved", drained.records.len());
        print_records(&drained.records);
        ok &= pending.len() == 2
            && drained
                .records
                .iter()
                .all(|r| r.status == exit::CANCELLED && r.result == 1 + prototype::CLEANUP_DONE);

        // Kernel cancellation during I/O.
        match IoDriver::<NativeTask>::new(8) {
            Ok(driver) => {
                let (reader, _writer) = std::io::pipe().map_err(|e| e.to_string())?;
                let mut rt = NativeRuntime::new(driver);
                let fiber = jit
                    .spawn_pulse_read(4, evidence.clone())
                    .map_err(|e| e.to_string())?;
                rt.spawn(fiber, Some(File::from(std::os::fd::OwnedFd::from(reader))));
                for _ in 0..4 {
                    rt.tick(false).map_err(|e| e.to_string())?;
                }
                let in_flight = rt.driver().pending();
                let drained = rt
                    .shutdown(64, true)
                    .map_err(|e| format!("kernel shutdown incomplete: {}", e.reason))?;
                println!("kernel cancellation during I/O (in flight: {in_flight}):");
                print_records(&drained.records);
                ok &= in_flight == 1 && drained.records[0].status == exit::CANCELLED;
            }
            Err(e) => {
                println!("kernel cancellation: io_uring unavailable ({e}); NOT exercised");
            }
        }
        Ok(ok)
    }

    pub fn run() -> Result<(), String> {
        let mut out = PathBuf::from("target/native-pulse");
        let mut fibers = 4u64;
        let mut args = std::env::args().skip(1);
        while let Some(a) = args.next() {
            match a.as_str() {
                "--out" => out = args.next().ok_or("--out needs a directory")?.into(),
                "--fibers" => {
                    fibers = args
                        .next()
                        .and_then(|n| n.parse().ok())
                        .ok_or("--fibers needs a number")?
                }
                other => return Err(format!("unknown argument {other}")),
            }
        }
        std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
        let payload = [1u8, 2, 3, 4, 5, 6, 7, 8];
        let path = out.join("payload.bin");
        File::create(&path)
            .and_then(|mut f| f.write_all(&payload))
            .map_err(|e| e.to_string())?;

        let jit = JitPrototype::new(OptLevel::Speed)?;
        println!("host: {}", std::env::consts::ARCH);
        let done = match IoDriver::<NativeTask>::new(64) {
            Ok(driver) => {
                println!("driver: io_uring (kernel)");
                schedule(Scheduler::new(driver), &jit, fibers, &path)?
            }
            Err(e) => {
                println!("driver: fixture (io_uring unavailable: {e}; kernel I/O NOT exercised)");
                schedule(
                    Scheduler::new(FixtureDriver::default()),
                    &jit,
                    fibers,
                    &path,
                )?
            }
        };
        let mut ok = true;
        let mut results: Vec<u64> = done.iter().map(|c| c.result).collect();
        results.sort();
        let mut expected: Vec<u64> = (0..fibers)
            .map(|i| prototype::expected_pulse_read(i, 1001 + i, &payload))
            .collect();
        expected.sort();
        for c in &done {
            ok &= c.status == 0 && c.word(0) == prototype::PULSE_READ_COUNTER;
        }
        ok &= results == expected;
        println!("fibers completed: {} (results {:?})", done.len(), results);
        for (triple, name) in [
            ("x86_64-unknown-linux-gnu", "pulse-x86_64.o"),
            ("aarch64-unknown-linux-gnu", "pulse-aarch64.o"),
        ] {
            let (bytes, clif) = prototype::emit_object(triple, OptLevel::Speed)?;
            std::fs::write(out.join(name), &bytes).map_err(|e| e.to_string())?;
            std::fs::write(out.join(format!("{name}.clif")), clif).map_err(|e| e.to_string())?;
            println!("wrote {} ({} bytes)", out.join(name).display(), bytes.len());
        }
        if !ok {
            return Err("round trip results did not match".into());
        }
        println!("round trip: OK");
        if shutdown_demo(&jit, &path)? {
            println!("shutdown: OK");
            Ok(())
        } else {
            Err("shutdown results did not match".into())
        }
    }
}
