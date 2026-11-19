//! `@vesper-std/env` (self-hosting SH0 lane 3, #1125): arguments,
//! variables and the current directory, in the JIT (optimize on and off)
//! and the evaluator, each answer checked against `std::env`.
mod std_io_support;

use std_io_support::{MODES, agree, check_driver};

const DRIVER: &str = r#"use @vesper-std/fs::IoError;
use @vesper-std/env::{
    arg_count, arg, arg_bytes, var, var_bytes, var_count, var_at_bytes, current_dir, os
};

fn mix(h: u64, b: u64) -> u64 {
    wrapping_mul(h ^ b, 1099511628211)
}

fn hash(in s: Slice[u8]) -> u64 {
    let mut h: u64 = 14695981039346656037;
    for i in 0..s.len() {
        let b = match s.try_get(i) {
            Option::Some(b) => b,
            Option::None => 0,
        };
        h = mix(h, widen_u64(b));
    }
    mix(h, s.len())
}

fn opt(sink o: Option[ChunkSeq[u8]]) -> u64 {
    match o {
        Option::Some(b) => hash(in b.as_slice()),
        Option::None => 1,
    }
}

pub fn args() -> u64 eff [Ffi[StdEnv]] {
    let n = arg_count();
    let mut h = mix(7, n);
    for i in 0..n {
        h = mix(h, opt(arg_bytes(i)));
    }
    // Past the end.
    mix(h, opt(arg_bytes(n)))
}

pub fn vars() -> u64 eff [Ffi[StdEnv]] {
    let a = opt(var_bytes(in "VESPER_STD_ENV_LONG"));
    let b = opt(var_bytes(in "VESPER_STD_ENV_UNSET"));
    let c = opt(var_bytes(in ""));
    let d = match var(in "VESPER_STD_ENV_TEXT") {
        Option::Some(s) => hash(in s.as_bytes()),
        Option::None => 1,
    };
    let n = var_count();
    let mut found: u64 = 0;
    for i in 0..n {
        match var_at_bytes(i) {
            Option::Some(kv) => {
                if hash(in kv.as_slice()) == hash(in "VESPER_STD_ENV_TEXT=wörld".as_bytes()) {
                    found = wrapping_add(found, 1);
                }
            },
            Option::None => {},
        }
    }
    mix(mix(mix(mix(a, b), c), d), found)
}

pub fn directory() -> u64 eff [Ffi[StdEnv]] {
    handle Fail[IoError] in {
        let d = current_dir();
        mix(hash(in d.as_bytes()), widen_u64(os()))
    } with {
        return v => v,
        raise(e, _) => wrapping_add(1000, widen_u64(e.kind)),
    }
}
"#;

fn mix(h: u64, b: u64) -> u64 {
    (h ^ b).wrapping_mul(1099511628211)
}

fn hash(s: &[u8]) -> u64 {
    let mut h: u64 = 14695981039346656037;
    for &b in s {
        h = mix(h, u64::from(b));
    }
    mix(h, s.len() as u64)
}

fn bytes(s: &std::ffi::OsStr) -> Vec<u8> {
    vesper_std_host::os_bytes(s)
}

/// The variables these tests read, set once for the whole test binary
/// (before any test reads the environment).
fn set_vars() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        // SAFETY: set before any other thread of this binary reads the
        // environment through these names, and never changed again.
        unsafe {
            std::env::set_var("VESPER_STD_ENV_LONG", "v".repeat(1000));
            std::env::set_var("VESPER_STD_ENV_TEXT", "wörld");
            std::env::remove_var("VESPER_STD_ENV_UNSET");
        }
    });
}

#[test]
fn arguments_match_std_env() {
    set_vars();
    let checked = check_driver(DRIVER);
    let args: Vec<_> = std::env::args_os().collect();
    let mut want = mix(7, args.len() as u64);
    for a in &args {
        want = mix(want, hash(&bytes(a)));
    }
    want = mix(want, 1);
    assert_eq!(agree(&checked, "args", &MODES), want);
}

#[test]
fn variables_match_std_env() {
    set_vars();
    let checked = check_driver(DRIVER);
    let want = mix(
        mix(
            mix(mix(hash("v".repeat(1000).as_bytes()), 1), 1),
            hash("wörld".as_bytes()),
        ),
        1,
    );
    assert_eq!(agree(&checked, "vars", &MODES), want);
}

#[test]
fn current_directory_matches_std_env() {
    set_vars();
    let checked = check_driver(DRIVER);
    let dir = bytes(std::env::current_dir().unwrap().as_os_str());
    let os = if cfg!(target_os = "linux") {
        1
    } else if cfg!(target_os = "macos") {
        2
    } else if cfg!(windows) {
        3
    } else {
        0
    };
    assert_eq!(agree(&checked, "directory", &MODES), mix(hash(&dir), os));
}
