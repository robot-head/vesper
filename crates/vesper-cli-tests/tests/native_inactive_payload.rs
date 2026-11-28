//! Inactive payload words of an enum value are unspecified (manuscript
//! §16.6, port speed lane 4, #1024).
//!
//! On the native optimize path a variant no longer zeroes the words past
//! its own fields. Each program here builds narrow variants into storage
//! that last held a wider one (a reused local, a loop-carried value, a
//! sequence slot, a value replaced through `inout`), so their inactive
//! words hold the wider variant's fields. Equality, `match`, drops and the
//! export boundary must read only the active variant. Every program runs
//! in the evaluator, the JIT with the optimizer on and off and, where a C
//! compiler links, an executable; all must agree, and the JIT must free
//! every sequence exactly once.
mod support;
use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "vesper-inactive-payload-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn links() -> bool {
    cfg!(unix)
        && Command::new("cc")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
}

fn vesper(args: &[&std::ffi::OsStr]) {
    let out = support::vesper().args(args).output().unwrap();
    assert!(
        out.status.success(),
        "vesper {args:?} failed\n{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The Ignis `check` shape: one wide variant and three one-word ones.
const CHECK: &str = r#"
copy struct Rec {
    a: u64,
    b: u64,
    c: u32,
    d: bool
}

copy enum Check {
    Ok(Rec),
    BadA(u64),
    BadB(u64),
    BadC(u32)
}

fn check(a: u64, b: u64, c: u32, depth: u64) -> Check {
    if depth > 0 {
        return check(a, b, c, wrapping_sub(depth, 1));
    }
    if a == 0 {
        return Check::BadA(a);
    }
    if b == 0 {
        return Check::BadB(b);
    }
    if c == 0 {
        return Check::BadC(c);
    }
    Check::Ok(Rec {
        a: a,
        b: b,
        c: c,
        d: true
    })
}

fn score(x: Check) -> u64 {
    match x {
        Check::Ok(r) => {
            let d: u64 = if r.d { 1000 } else { 0 };
            wrapping_add(wrapping_add(wrapping_mul(r.a, 3), wrapping_mul(r.b, 5)), wrapping_add(widen_u64(r.c), d))
        }
        Check::BadA(v) => wrapping_add(v, 11),
        Check::BadB(v) => wrapping_add(v, 13),
        Check::BadC(v) => wrapping_add(widen_u64(v), 17),
    }
}
"#;

/// Builds `source` as a C-ABI library and calls `pub fn pick(k: u64) ->
/// Check` from C with a result buffer whose words start as garbage; C reads
/// the tag and the active fields only.
#[test]
fn export_results_read_the_active_variant() {
    if !links() {
        return;
    }
    let temp = Temp::new();
    let file = temp.0.join("lib.ves");
    fs::write(
        &file,
        format!(
            "{CHECK}
pub fn pick(k: u64) -> Check {{
    if k == 0 {{ return check(1, 2, 3, 2); }}
    if k == 1 {{ return check(0, 2, 3, 2); }}
    if k == 2 {{ return check(1, 0, 3, 2); }}
    check(1, 2, 0, 2)
}}
"
        ),
    )
    .unwrap();
    let obj = temp.0.join("lib.o");
    vesper(&[
        "build".as_ref(),
        file.as_os_str(),
        "--lib".as_ref(),
        "-o".as_ref(),
        obj.as_os_str(),
    ]);
    let main_c = temp.0.join("main.c");
    fs::write(
        &main_c,
        r#"
#include <stdint.h>
#include <stdio.h>
extern void vesper_lib_pick(uint64_t *out, uint64_t k);
int main(void) {
    uint64_t total = 0, kept = 0;
    for (uint64_t k = 0; k < 4; k++) {
        uint64_t w[5] = {0xdead, 0xdead, 0xdead, 0xdead, 0xdead};
        vesper_lib_pick(w, k);
        for (int i = 2; i < 5; i++) kept += w[i] == 0xdead;
        switch (w[0]) {
        case 0: total += w[1] * 3 + w[2] * 5 + (uint32_t)w[3] + (w[4] & 1) * 1000; break;
        case 1: case 2: total += w[1] + 10 * w[0]; break;
        case 3: total += (uint32_t)w[1] + 30; break;
        default: return 99;
        }
    }
    printf("%llu %llu\n", (unsigned long long)total, (unsigned long long)kept);
    return 0;
}
"#,
    )
    .unwrap();
    let exe = temp.0.join("main");
    let cc = Command::new("cc")
        .arg(&main_c)
        .arg(&obj)
        .arg("-o")
        .arg(&exe)
        .output()
        .unwrap();
    assert!(
        cc.status.success(),
        "cc failed: {}",
        String::from_utf8_lossy(&cc.stderr)
    );
    let run = Command::new(&exe).output().unwrap();
    assert!(run.status.success(), "C caller saw an invalid tag");
    // Ok(1, 2, 3, true) = 3 + 10 + 3 + 1000; BadA(0) = 10; BadB(0) = 20;
    // BadC(0) = 30. The three one-word variants leave their 3 inactive
    // words as the caller's garbage (the optimize path does not zero them).
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "1076 9");
}
