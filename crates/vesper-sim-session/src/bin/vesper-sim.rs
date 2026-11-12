//! `vesper-sim record <source.ves> <scenario.json> [recording.json]`
//! `vesper-sim replay <source.ves> <scenario.json> <recording.json>`
//!
//! Runs checked source on the reference evaluator under SimPulse. Recordings
//! are bound to this executable's SHA-256 digest; replay with a different
//! build, source, scenario, model semantics or options is rejected. Version 2
//! scenarios also print lost tasks and the final durable log bytes; version 3
//! scenarios print each NIC's final UMEM frame accounting.
use std::process::ExitCode;
use vesper_sim_session::{CheckedProgram, Recording, RunResult, Scenario, Session};

const STEPS_PER_CALL: u64 = 1024;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("vesper-sim: {message}");
            ExitCode::FAILURE
        }
    }
}

fn read(path: &str) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("cannot read {path}: {e}"))
}

fn run(args: &[String]) -> Result<(), String> {
    let (mode, source, scenario, recording) = match args {
        [mode, source, scenario] if mode == "record" => (mode, source, scenario, None),
        [mode, source, scenario, recording] if mode == "record" || mode == "replay" => {
            (mode, source, scenario, Some(recording))
        }
        _ => {
            return Err(
                "usage: vesper-sim record <source.ves> <scenario.json> [recording.json]\n       vesper-sim replay <source.ves> <scenario.json> <recording.json>"
                    .into(),
            );
        }
    };
    let program = CheckedProgram::new(read(source)?).map_err(|e| e.to_string())?;
    let scenario = Scenario::from_json(&read(scenario)?).map_err(|e| e.to_string())?;
    let session = if mode == "replay" {
        let path = recording.expect("replay has a recording path");
        let recording = Recording::from_json(&read(path)?).map_err(|e| e.to_string())?;
        Session::replay(&program, scenario, recording)
    } else {
        Session::record(&program, scenario)
    }
    .map_err(|e| e.to_string())?;
    match session.run(STEPS_PER_CALL).finish(STEPS_PER_CALL) {
        RunResult::Complete(report) => {
            for (task, value) in &report.outputs {
                println!("{task}: {value}");
            }
            for lost in &report.lost {
                println!(
                    "lost: {} (process {} epoch {} at {} ns, {:?})",
                    lost.task, lost.process, lost.epoch, lost.at_ns, lost.state
                );
            }
            for (log, durable) in &report.recording.durable {
                println!(
                    "durable {log}: {} bytes sha256 {}",
                    durable.bytes_hex.len() / 2,
                    durable.sha256
                );
            }
            if let Some(warp) = report.model.as_ref().and_then(|m| m.warp.as_ref()) {
                for (nic, state) in &warp.nics {
                    let a = state.accounting;
                    println!(
                        "umem {nic}: {} frames conserved (fill {}, rx {}, harvested {}, staged {}, leased {}, parked {})",
                        a.total, a.fill, a.rx, a.harvested, a.staged, a.leased, a.parked
                    );
                }
            }
            println!("virtual time: {} ns", report.virtual_ns);
            println!("source events: {}", report.recording.trace.len());
            if !report.recording.model_trace.is_empty() {
                println!("model events: {}", report.recording.model_trace.len());
            }
            if mode == "replay" {
                println!("replay matched the recording");
            } else if let Some(path) = recording {
                std::fs::write(path, report.recording.to_json())
                    .map_err(|e| format!("cannot write {path}: {e}"))?;
                println!("recording written to {path}");
            }
            Ok(())
        }
        RunResult::Quiescent(session) => Err(format!(
            "{} operations wait for external completions, which this command cannot supply",
            session.pending().len()
        )),
        RunResult::Interrupted { session, error } => Err(format!(
            "interrupted at {} ns with owners retained: {error}",
            session.now_ns()
        )),
        RunResult::Terminal(failure) => Err(format!(
            "terminal failure in task {} at {} ns: {}",
            failure.task, failure.at_ns, failure.message
        )),
        RunResult::Budget(_) => unreachable!("finish drives budget exits"),
    }
}
