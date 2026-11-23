//! Differential verification of `@vesper/pulse` (`crates/vesper-pulse/vesper/`):
//! relocatable segmented `FrameStack`, `FrameId`, `Address`, `FrameError`,
//! word/byte/address read/write, LIFO `pop`, relocation, and error invariants
//! against `vesper_pulse::frames::FrameStack`.
mod std_io_support;

use std_io_support::{MODES, Temp, agree};
use vesper::{ffi_lowering, packages};
use vesper_pulse::frames::{Address, FrameError, FrameStack};

fn pulse_imports() -> ffi_lowering::Imports {
    let (triple, abi) = packages::vsys_target(None).expect("host target");
    let os = if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "windows"
    };
    let arch = if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "x86_64"
    };
    let path = format!("crates/vesper-pulse/vesper/pulse-{os}-{arch}.vsys");
    let text = std::fs::read_to_string(&path).expect("read pulse .vsys");
    let text = text.replace("@TARGET@", triple).replace("@ABI@", abi);
    let mut imports = ffi_lowering::Imports::new();
    imports
        .add(&text, ffi_lowering::Handler::Direct)
        .expect("import pulse .vsys");
    imports
}

fn check_pulse_driver(source: &str) -> ffi_lowering::ForeignChecked {
    let dir = Temp::new("pulse-port");
    let pulse_pkg = std::fs::canonicalize("crates/vesper-pulse/vesper").unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(
        dir.path().join("Loom.toml"),
        format!(
            "[package]\nname = \"@test/pulse-driver\"\nversion = \"0.1.0\"\nedition = \"2026\"\n\n[dependencies]\n\"@vesper/pulse\" = {{ version = \"^0.1\", path = {:?} }}\n",
            pulse_pkg
        ),
    )
    .unwrap();
    std::fs::write(dir.path().join("src/lib.ves"), source).unwrap();
    let assembled = packages::load(dir.path()).expect("load @test/pulse-driver");
    assert!(
        assembled.ok(),
        "{:?} {:?}",
        assembled.diagnostics,
        assembled.manifest_errors
    );
    ffi_lowering::check_module(&assembled.module, &pulse_imports())
        .unwrap_or_else(|e| panic!("{e}: {:?}", e.diagnostics))
}

#[test]
fn segmented_relocation_and_word_ops_match_rust() {
    let mut stack = FrameStack::new(16).unwrap();
    let a = stack.push(16).unwrap();
    let b = stack.push(24).unwrap();
    let x = Address {
        frame: a,
        offset: 8,
    };
    let y = Address {
        frame: b,
        offset: 16,
    };
    stack.write_word(x, 20).unwrap();
    stack.write_word(y, 22).unwrap();
    stack.relocate().unwrap();
    let rust_sum = stack.read_word(x).unwrap() + stack.read_word(y).unwrap();
    stack.write_word(x, 100).unwrap();
    let rust_after = stack.read_word(x).unwrap() + stack.read_word(y).unwrap();
    let rust_depth = stack.depth() as u64;
    let rust_segs = stack.segments() as u64;
    let rust_bytes = stack.bytes() as u64;
    let popped_b = stack.pop(b).unwrap();
    let popped_a = stack.pop(a).unwrap();
    let rust_pop_lens = (popped_b.len() as u64) * 100 + (popped_a.len() as u64);
    let rust_final_depth = stack.depth() as u64;
    let rust_final_segs = stack.segments() as u64;

    let src = r#"
use @vesper/pulse::{
    Address, FrameError, frame_stack_bytes, frame_stack_depth, frame_stack_new, frame_stack_pop,
    frame_stack_push, frame_stack_read_word, frame_stack_relocate, frame_stack_segments,
    frame_stack_write_word,
};

pub fn check() -> u64 {
    handle Fail[FrameError] in {
        let mut stack = frame_stack_new(16);
        let a = frame_stack_push(inout stack, 16);
        let b = frame_stack_push(inout stack, 24);
        let x = Address { frame: a, offset: 8 };
        let y = Address { frame: b, offset: 16 };
        frame_stack_write_word(inout stack, x, 20);
        frame_stack_write_word(inout stack, y, 22);
        frame_stack_relocate(inout stack);
        let sum = wrapping_add(frame_stack_read_word(in stack, x), frame_stack_read_word(in stack, y));
        frame_stack_write_word(inout stack, x, 100);
        let after = wrapping_add(frame_stack_read_word(in stack, x), frame_stack_read_word(in stack, y));
        let depth = frame_stack_depth(in stack);
        let segs = frame_stack_segments(in stack);
        let bytes = frame_stack_bytes(in stack);
        let pb = frame_stack_pop(inout stack, b);
        let pa = frame_stack_pop(inout stack, a);
        let pop_lens = wrapping_add(wrapping_mul(pb.len(), 100), pa.len());
        let fdepth = frame_stack_depth(in stack);
        let fsegs = frame_stack_segments(in stack);
        let mut acc = sum;
        acc = wrapping_add(acc, wrapping_mul(after, 1000));
        acc = wrapping_add(acc, wrapping_mul(depth, 1000000));
        acc = wrapping_add(acc, wrapping_mul(segs, 10000000));
        acc = wrapping_add(acc, wrapping_mul(bytes, 100000000));
        acc = wrapping_add(acc, wrapping_mul(pop_lens, 10000000000));
        acc = wrapping_add(acc, wrapping_add(fdepth, fsegs));
        acc
    } with {
        return x => x,
        raise(_e, _k) => 0,
    }
}
"#;
    let checked = check_pulse_driver(src);
    let expected = rust_sum
        + rust_after * 1000
        + rust_depth * 1_000_000
        + rust_segs * 10_000_000
        + rust_bytes * 100_000_000
        + rust_pop_lens * 10_000_000_000
        + rust_final_depth
        + rust_final_segs;
    assert_eq!(agree(&checked, "check", &MODES), expected);
}

#[test]
fn stored_addresses_and_stale_frame_errors_match_rust() {
    let mut stack = FrameStack::new(32).unwrap();
    let a = stack.push(16).unwrap();
    let b = stack.push(16).unwrap();
    let target = Address {
        frame: b,
        offset: 8,
    };
    let slot = Address {
        frame: a,
        offset: 0,
    };
    stack.write_address(slot, target).unwrap();
    let roundtrip = stack.read_address(slot).unwrap();
    assert_eq!(roundtrip, target);
    assert_eq!(stack.pop(a), Err(FrameError::NotTopFrame));
    stack.pop(b).unwrap();
    assert_eq!(stack.read_address(slot), Err(FrameError::StaleFrame));
    assert_eq!(stack.live_id(b.bits()), Err(FrameError::StaleFrame));
    assert_eq!(
        stack.read(
            Address {
                frame: a,
                offset: 12
            },
            8
        ),
        Err(FrameError::OutOfBounds)
    );
    assert!(matches!(FrameStack::new(0), Err(FrameError::InvalidSize)));

    let src = r#"
use @vesper/pulse::{
    Address, FrameError, frame_stack_live_id, frame_stack_new, frame_stack_pop,
    frame_stack_push, frame_stack_read, frame_stack_read_address, frame_stack_write_address,
};

fn err_code(e: FrameError) -> u64 {
    match e {
        FrameError::Allocation => 1,
        FrameError::ExhaustedIds => 2,
        FrameError::InvalidSize => 3,
        FrameError::StaleFrame => 4,
        FrameError::OutOfBounds => 5,
        FrameError::NotTopFrame => 6,
    }
}

pub fn check_errors() -> u64 {
    let bad = handle Fail[FrameError] in {
        let _s = frame_stack_new(0);
        0
    } with {
        return x => x,
        raise(e, _k) => err_code(e),
    };
    handle Fail[FrameError] in {
        let mut stack = frame_stack_new(32);
        let a = frame_stack_push(inout stack, 16);
        let b = frame_stack_push(inout stack, 16);
        let target = Address { frame: b, offset: 8 };
        let slot = Address { frame: a, offset: 0 };
        frame_stack_write_address(inout stack, slot, target);
        let rt = frame_stack_read_address(in stack, slot);
        let rt_ok: u64 = if rt.frame.bits == b.bits && rt.offset == 8 { 1 } else { 0 };
        let not_top = handle Fail[FrameError] in {
            let _p = frame_stack_pop(inout stack, a);
            0
        } with {
            return x => x,
            raise(e, _k) => err_code(e),
        };
        let _pb = frame_stack_pop(inout stack, b);
        let stale_addr = handle Fail[FrameError] in {
            let _r = frame_stack_read_address(in stack, slot);
            0
        } with {
            return x => x,
            raise(e, _k) => err_code(e),
        };
        let stale_id = handle Fail[FrameError] in {
            let _id = frame_stack_live_id(in stack, b.bits);
            0
        } with {
            return x => x,
            raise(e, _k) => err_code(e),
        };
        let oob = handle Fail[FrameError] in {
            let _bytes = frame_stack_read(in stack, Address { frame: a, offset: 12 }, 8);
            0
        } with {
            return x => x,
            raise(e, _k) => err_code(e),
        };
        let mut acc = bad;
        acc = wrapping_add(acc, wrapping_mul(rt_ok, 10));
        acc = wrapping_add(acc, wrapping_mul(not_top, 100));
        acc = wrapping_add(acc, wrapping_mul(stale_addr, 1000));
        acc = wrapping_add(acc, wrapping_mul(stale_id, 10000));
        acc = wrapping_add(acc, wrapping_mul(oob, 100000));
        acc
    } with {
        return x => x,
        raise(_e, _k) => 0,
    }
}
"#;
    let checked = check_pulse_driver(src);
    let expected = 3 + 10 + 6 * 100 + 4 * 1000 + 4 * 10000 + 5 * 100000;
    assert_eq!(agree(&checked, "check_errors", &MODES), expected);
}

#[cfg(target_os = "linux")]
#[test]
fn linux_epoll_pipe_roundtrip() {
    let src = r#"
use @vesper/pulse::epoll_pipe_roundtrip_check;

pub fn check_epoll() -> u64 eff [Ffi[PulseSys]] {
    epoll_pipe_roundtrip_check()
}
"#;
    let checked = check_pulse_driver(src);
    assert_eq!(
        agree(
            &checked,
            "check_epoll",
            &[std_io_support::Mode::Jit, std_io_support::Mode::JitNoOpt],
        ),
        100
    );
}
