//! Record and replay the two-task fixture: `cargo run --example two_tasks`.
use vesper_sim_session::{CheckedProgram, RunResult, Scenario, Session};

fn main() {
    let root = env!("CARGO_MANIFEST_DIR");
    let source = std::fs::read_to_string(format!("{root}/fixtures/pipeline.ves")).unwrap();
    let scenario = std::fs::read_to_string(format!("{root}/fixtures/pipeline.json")).unwrap();
    let program = CheckedProgram::new(source).expect("fixture checks");
    let scenario = Scenario::from_json(&scenario).expect("fixture scenario parses");
    let report = match Session::record(&program, scenario.clone())
        .expect("scenario validates")
        .run(u64::MAX)
    {
        RunResult::Complete(report) => report,
        other => panic!("recording did not complete: {other:?}"),
    };
    for event in &report.recording.trace {
        println!("{}", serde_json::to_string(event).unwrap());
    }
    for (task, value) in &report.outputs {
        println!("{task} => {value}");
    }
    println!("virtual time {} ns", report.virtual_ns);
    let replay = Session::replay(&program, scenario, report.recording.clone())
        .expect("identity matches")
        .run(u64::MAX);
    match replay {
        RunResult::Complete(again) if again == report => println!("replay reproduced the run"),
        other => panic!("replay diverged: {other:?}"),
    }
}
