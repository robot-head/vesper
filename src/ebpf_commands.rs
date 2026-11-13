//! `vesper ebpf check <object.o | program.bin | program.hex>`: the Vesper
//! eBPF pre-flight verifier (§27.5) from the CLI (integration B, #136).
//!
//! Inputs: a BPF ELF object (an `xdp` section, BTF-defined `.maps`, as
//! written by `vesper_ebpf::elf`), raw instruction bytes (a multiple of 8
//! bytes), or a text file of instruction slots, one 16-hex-digit slot per
//! line in memory byte order (`#` starts a comment). A textual assembler is
//! not provided. Findings are the verifier's typed rule ids with their
//! instruction slot; an accepted program also gets the shared stack-bound
//! report (`crate::stack_bound`) over its BPF-to-BPF call graph. `--load`
//! additionally loads the program into the running kernel where `bpf()` is
//! permitted, and reports `unavailable` (not success) where it is not.
use std::{
    fs,
    path::{Path, PathBuf},
};
use vesper::stack_bound;
use vesper_ebpf::{Program, ProgramType, RawInsn, VerifierPolicy, elf, insn};

const HELP: &str = "Usage: vesper ebpf check <object.o | program.bin | program.hex> [--load] [--stack-budget <bytes>]\n\nRun the Vesper eBPF pre-flight verifier and report typed findings.\n\nOptions:\n  --load                 Also load into the kernel where bpf() is permitted (Linux)\n  --stack-budget <bytes> Root stack budget for the stack-bound report (default 512)\n  --disassemble          Print the instructions\n";

fn program(path: &Path, bytes: &[u8]) -> Result<Program, String> {
    let fail = |e: String| format!("{}: {e}", path.display());
    if bytes.starts_with(b"\x7fELF") {
        return elf::read_object(bytes).map_err(|e| fail(format!("{e:?}")));
    }
    let slots = if path.extension().is_some_and(|e| e == "hex" || e == "txt") {
        let mut raw = Vec::new();
        for (n, line) in String::from_utf8_lossy(bytes).lines().enumerate() {
            let line = line.split('#').next().unwrap_or_default().trim();
            if line.is_empty() {
                continue;
            }
            let digits: String = line.chars().filter(|c| !c.is_whitespace()).collect();
            if digits.len() != 16 || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(fail(format!(
                    "line {}: expected one 8-byte slot as 16 hex digits",
                    n + 1
                )));
            }
            for i in 0..8 {
                raw.push(
                    u8::from_str_radix(&digits[2 * i..2 * i + 2], 16).map_err(|e| e.to_string())?,
                );
            }
        }
        raw
    } else {
        bytes.to_vec()
    };
    if slots.is_empty() {
        return Err(fail("program has no instructions".into()));
    }
    let insns: Vec<RawInsn> = insn::slots_from_bytes(&slots).map_err(|e| fail(format!("{e:?}")))?;
    Ok(Program {
        name: path
            .file_stem()
            .map_or_else(|| "prog".into(), |s| s.to_string_lossy().into_owned()),
        prog_type: ProgramType::Xdp,
        insns,
        maps: Vec::new(),
        map_relocs: Vec::new(),
        functions: Vec::new(),
        license: "Dual MIT/GPL".into(),
    })
}

pub fn run(args: Vec<String>) -> Result<(), String> {
    let mut it = args.into_iter();
    match it.next().as_deref() {
        Some("check") => {}
        Some("-h" | "--help") | None => {
            print!("{HELP}");
            return Ok(());
        }
        Some(other) => {
            return Err(format!(
                "unknown ebpf subcommand '{other}'; see vesper ebpf --help"
            ));
        }
    }
    let mut input = None;
    let mut load = false;
    let mut disassemble = false;
    let mut budget = vesper_ebpf::BPF_MAX_STACK_BYTES;
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{HELP}");
                return Ok(());
            }
            "--load" => load = true,
            "--disassemble" => disassemble = true,
            "--stack-budget" => {
                budget = it
                    .next()
                    .ok_or("--stack-budget requires a byte count")?
                    .parse()
                    .map_err(|_| "--stack-budget requires a byte count")?
            }
            other if other.starts_with('-') => {
                return Err(format!("unknown ebpf check option '{other}'"));
            }
            _ => {
                if input.replace(PathBuf::from(&arg)).is_some() {
                    return Err("only one program is supported".into());
                }
            }
        }
    }
    let input = input.ok_or("missing program; see vesper ebpf --help")?;
    let bytes = fs::read(&input).map_err(|e| format!("{}: {e}", input.display()))?;
    let prog = program(&input, &bytes)?;
    if disassemble {
        print!("{}", insn::disassemble(&prog.insns));
    }
    let report = match vesper_ebpf::verify(&prog, &VerifierPolicy::default()) {
        Ok(report) => report,
        Err(e) => {
            for f in vesper_ebpf::findings(&prog, &VerifierPolicy::default()) {
                let at = f.inst.map_or_else(String::new, |i| format!(" at insn {i}"));
                println!("error[{}]: {}{at}: {}", f.rule, f.function, f.message);
            }
            return Err(format!("ebpf check: '{}' rejected ({})", prog.name, e.rule));
        }
    };
    println!(
        "ebpf check: '{}' accepted ({} insn(s) processed, {} path(s), worst stack {} bytes)",
        prog.name, report.insns_processed, report.paths_completed, report.worst_stack
    );
    let graph = stack_bound::StackGraph::from(&report.call_graph);
    let root = graph
        .functions
        .first()
        .map(|f| f.name.clone())
        .ok_or("ebpf check: the verifier returned an empty call graph")?;
    let bound = stack_bound::stack_bound_report(
        &graph,
        &stack_bound::StackBoundPolicy {
            roots: vec![stack_bound::StackBudget {
                function: root,
                budget_bytes: budget,
            }],
            ..Default::default()
        },
    )?;
    print!("{}", stack_bound::render(&bound));
    for f in &bound.findings {
        println!("error[{}]: {}: {}", f.rule, f.function, f.message);
    }
    if !bound.accepted() {
        return Err("ebpf check: stack bound exceeded".into());
    }
    if load {
        kernel_load(&prog)?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn kernel_load(prog: &Program) -> Result<(), String> {
    use vesper_ebpf::kernel::{LoadError, load};
    match load(prog) {
        Ok(_) => {
            println!("kernel: loaded and accepted by the running kernel's verifier");
            Ok(())
        }
        Err(LoadError::Unavailable { errno }) => {
            println!(
                "kernel: unavailable (bpf() refused with errno {errno}); only the pre-flight verifier ran"
            );
            Ok(())
        }
        Err(LoadError::Rejected { errno, log }) => Err(format!(
            "kernel: rejected by the running kernel's verifier (errno {errno}):\n{log}"
        )),
        Err(e) => Err(format!("kernel: {e}")),
    }
}

#[cfg(not(target_os = "linux"))]
fn kernel_load(_prog: &Program) -> Result<(), String> {
    println!("kernel: unavailable (eBPF loading needs Linux); only the pre-flight verifier ran");
    Ok(())
}
