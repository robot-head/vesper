//! Wave-seven lane 23 (#346): source locations on emitted Cranelift
//! instructions, the native fuel bound, the capability-passing ABI, generic
//! ConstTime instances, NaN canonicalisation in the batch backend and native
//! coverage for `verify --refute` and `fuzz --differential`.
mod support;
use std::{
    fs,
    path::PathBuf,
    process::Output,
    sync::atomic::{AtomicU64, Ordering},
};

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "vesper-native-spans-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    fn write(&self, name: &str, text: &str) -> String {
        let path = self.0.join(name);
        fs::write(&path, text).unwrap();
        path.to_string_lossy().into_owned()
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn vesper(args: &[&str]) -> Output {
    support::vesper().args(args).output().unwrap()
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

const MAGIC_SOURCE: &str = "fn unlock(key: u64, len: u32) -> u32 eff [Fail[ArithOverflow]] {
    let magic: u64 = 4660 * 4660 + 77;
    if key == magic {
        len * 65536
    } else {
        len / 2
    }
}

fn below(x: i64) -> bool {
    x < 5
}

fn spin(n: u64) -> u64 eff [Fail[ArithOverflow]] {
    if n == 3 {
        spin(n) + 1
    } else {
        n
    }
}

fn main() -> i64 {
    7
}
";

#[test]
fn fuzzing_a_non_terminating_target_reports_invalid_runs() {
    let temp = Temp::new();
    let file = temp.write("spin.ves", MAGIC_SOURCE);
    let o = vesper(&[
        "fuzz",
        &file,
        "--target",
        "spin",
        "--native-coverage",
        "--execs",
        "300",
        "--fuel",
        "5000",
    ]);
    let t = text(&o);
    assert!(t.contains("fuel 5000 block(s)"), "{t}");
    // spin(3) never terminates; the evaluator runs out of fuel too, so it is
    // an invalid run, never a hang or a crash.
    assert!(t.contains("no failure found within budget"), "{t}");
    assert!(o.status.success(), "{t}");
}

#[test]
fn native_differential_compares_results_native_code_does_not_report() {
    // f64 results have no native result word; the evaluator's outcomes
    // (cross-checked against native code) still decide the comparison.
    let source = "fn fa(x: u64) -> f64 {
    1.0
}

fn fb(x: u64) -> f64 {
    2.0
}

fn main() -> u64 {
    0
}
";
    let temp = Temp::new();
    let file = temp.write("floats.ves", source);
    let o = vesper(&[
        "fuzz",
        &file,
        "--target",
        "fa",
        "--differential",
        "fb",
        "--native-coverage",
        "--execs",
        "50",
    ]);
    let t = text(&o);
    assert!(t.contains("failure [divergence]"), "{t}");
    assert!(!o.status.success(), "{t}");
}
