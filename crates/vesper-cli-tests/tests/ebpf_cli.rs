//! Integration B (#136): `vesper ebpf check` and the shared stack-bound
//! report that merges the eBPF and Silicon stack analyzers.
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
            "vesper-ebpf-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    fn write(&self, name: &str, bytes: &[u8]) -> String {
        let path = self.0.join(name);
        fs::write(&path, bytes).unwrap();
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

#[test]
fn ebpf_check_accepts_the_mitigator_object() {
    let temp = Temp::new();
    let prog = vesper_ebpf::mitigator::build(&Default::default()).unwrap();
    let object = temp.write(
        "mitigator.o",
        &vesper_ebpf::elf::write_object(&prog).unwrap(),
    );
    let o = vesper(&["ebpf", "check", &object]);
    let t = text(&o);
    assert!(o.status.success(), "{t}");
    assert!(t.contains("ebpf check: '"), "{t}");
    assert!(t.contains("accepted"), "{t}");
    assert!(t.contains("of 512 byte budget (ok)"), "{t}");
    // --load either loads it or says the kernel path is unavailable.
    let o = vesper(&["ebpf", "check", &object, "--load"]);
    let t = text(&o);
    assert!(o.status.success(), "{t}");
    assert!(
        t.contains("kernel: loaded") || t.contains("kernel: unavailable"),
        "{t}"
    );
    // A budget below its worst case is a stack-bound failure.
    let o = vesper(&["ebpf", "check", &object, "--stack-budget", "8"]);
    let t = text(&o);
    assert!(!o.status.success(), "{t}");
    assert!(t.contains("silicon.stack.budget_exceeded"), "{t}");
    assert!(t.contains("ebpf check: stack bound exceeded"), "{t}");
}

#[test]
fn ebpf_check_reads_raw_and_hex_slots_and_reports_typed_findings() {
    let temp = Temp::new();
    let ok = temp.write(
        "ok.hex",
        b"# r0 = 2; exit\nb700000002000000\n95 00 00 00 00 00 00 00\n",
    );
    let o = vesper(&["ebpf", "check", &ok, "--disassemble"]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(
        text(&o).contains("worst case 32 bytes of 512 byte budget (ok)"),
        "{}",
        text(&o)
    );
    let raw = temp.write(
        "ok.bin",
        &[0xb7, 0, 0, 0, 2, 0, 0, 0, 0x95, 0, 0, 0, 0, 0, 0, 0],
    );
    assert!(vesper(&["ebpf", "check", &raw]).status.success());
    // `exit` with an uninitialised r0.
    let bad = temp.write("bad.hex", b"9500000000000000\n");
    let o = vesper(&["ebpf", "check", &bad]);
    let t = text(&o);
    assert!(!o.status.success());
    assert!(t.contains("error[reg.uninit_read]: bad at insn 0"), "{t}");
    assert!(
        t.contains("ebpf check: 'bad' rejected (reg.uninit_read)"),
        "{t}"
    );
    // A packet read without a bounds check.
    let unchecked = temp.write(
        "unchecked.hex",
        // r2 = *(u32 *)(r1 + 0) (ctx->data); r0 = *(u8 *)(r2 + 0); exit
        b"6112000000000000\n7120000000000000\n9500000000000000\n",
    );
    let o = vesper(&["ebpf", "check", &unchecked]);
    assert!(!o.status.success());
    assert!(text(&o).contains("rejected (mem."), "{}", text(&o));
    for (name, bytes, message) in [
        ("short.bin", &b"\x95\0\0"[..], "short.bin"),
        (
            "garbage.hex",
            &b"zz\n"[..],
            "line 1: expected one 8-byte slot",
        ),
        (
            "empty.hex",
            &b"# nothing\n"[..],
            "program has no instructions",
        ),
    ] {
        let path = temp.write(name, bytes);
        let o = vesper(&["ebpf", "check", &path]);
        assert!(!o.status.success(), "{name}");
        assert!(text(&o).contains(message), "{name}: {}", text(&o));
    }
    let o = vesper(&["ebpf", "verify"]);
    assert!(text(&o).contains("unknown ebpf subcommand 'verify'"));
}
