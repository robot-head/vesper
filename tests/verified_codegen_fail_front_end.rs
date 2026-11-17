//! Milestone 7 (abort-only `Fail[E]`) on the real self-hosted front end.
//!
//! Every function of `packages/vesper-compiler` (the parser half the
//! self-host driver loads) with scalar parameters and a `Fail`-only effect
//! row is wrapped in an entry that handles its non-arithmetic `Fail[E]`
//! instances, compiled with the verified backend, run (natively on x86-64
//! or AArch64 Linux, under QEMU for the other Linux targets when present)
//! and compared with the evaluator. The test also pins how many wrappers
//! compile, so a regression in the `Fail` lowering cannot hide behind the
//! other refusals (dynamic indexing, regions, generics) that later lanes
//! remove.
#[path = "fixtures/self_host/driver.rs"]
mod support;
use std::path::PathBuf;
use support::*;
use vesper::verified_codegen as vc;

fn on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

/// How executables for `target` run here, if they can.
fn runner(target: vc::Target) -> Option<Vec<PathBuf>> {
    if vc::Target::host() == Some(target) {
        return Some(vec![]);
    }
    if !cfg!(target_os = "linux") {
        return None;
    }
    let names: &[&str] = match target {
        vc::Target::AARCH64_LINUX => &["qemu-aarch64", "qemu-aarch64-static"],
        vc::Target::RISCV64_LINUX => &["qemu-riscv64", "qemu-riscv64-static"],
        vc::Target::X86_64_LINUX => &["qemu-x86_64", "qemu-x86_64-static"],
        _ => &[],
    };
    names.iter().find_map(|n| on_path(n)).map(|q| vec![q])
}

#[cfg(unix)]
fn run(bytes: &[u8], prefix: &[PathBuf], tag: &str) -> vc::Outcome {
    use std::os::unix::fs::PermissionsExt;
    let path = std::env::temp_dir().join(format!(
        "vesper-vc-fail-front-end-{}-{tag}",
        std::process::id()
    ));
    std::fs::write(&path, bytes).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut cmd = match prefix.split_first() {
        Some((q, rest)) => {
            let mut c = std::process::Command::new(q);
            c.args(rest).arg(&path);
            c
        }
        None => std::process::Command::new(&path),
    };
    let out = cmd.output().unwrap();
    let _ = std::fs::remove_file(&path);
    vc::Outcome::from_exit(out.status.code(), &out.stdout)
}
#[cfg(not(unix))]
fn run(_: &[u8], _: &[PathBuf], _: &str) -> vc::Outcome {
    unreachable!("only Linux targets are run")
}

#[test]
fn front_end_functions_with_fail_effects_compile_and_agree() {
    on_big_stack(|| {
        let front = read_front_end();
        let base = program(&front, b"");
        let mut fns: Vec<_> = base.world.functions.iter().collect();
        fns.sort_by(|a, b| a.0.cmp(b.0));
        let mut wrappers = String::new();
        let mut names = vec![];
        let mut handled_user_errors = 0;
        for (n, f) in fns {
            let scalar = f.params.iter().all(|p| {
                matches!(p.ty.name(), "i64" | "u8" | "u64" | "bool")
                    && p.pass != vesper::ast::Pass::Inout
            });
            let fail_only =
                f.effects.tail.is_none() && f.effects.instances.iter().all(|e| e.name == "Fail");
            if n.contains("::") || !f.generics.is_empty() || !scalar || !fail_only {
                continue;
            }
            let args = f
                .params
                .iter()
                .map(|p| if p.ty.name() == "bool" { "false" } else { "0" })
                .collect::<Vec<_>>()
                .join(", ");
            let mut body = if f.result.name() == "i64" {
                format!("{{ {n}({args}) }}")
            } else {
                format!("{{ let _ = {n}({args}); 0 }}")
            };
            for e in &f.effects.instances {
                let t = e.args[0].to_string();
                if t == "ArithOverflow" || t == "RuntimeError" {
                    continue;
                }
                handled_user_errors += 1;
                body = format!(
                    "{{ handle Fail[{t}] in {body} with {{ return v => v, raise(_, _) => -424242, }} }}"
                );
            }
            let w = format!("vc_fail_probe_{}", names.len());
            wrappers.push_str(&format!(
                "\nfn {w}() -> i64 eff [Fail[ArithOverflow], Fail[RuntimeError]] {body}\n"
            ));
            names.push((w, n.clone()));
        }
        assert!(handled_user_errors > 0, "{handled_user_errors}");
        let p = program(&format!("{front}{wrappers}"), b"");
        let mut compiled = 0;
        let mut raised = 0;
        let mut refusals = std::collections::BTreeMap::<String, usize>::new();
        for (w, n) in &names {
            let mut runtime = vesper::runtime::Runtime::new(&p.world);
            runtime.optimize = false;
            runtime.fuel = 2_000_000_000;
            let expected = runtime.run(w, vec![]).map(|v| v.to_string());
            let mut any = false;
            for target in [
                vc::Target::X86_64_LINUX,
                vc::Target::AARCH64_LINUX,
                vc::Target::RISCV64_LINUX,
            ] {
                let options = vc::Options {
                    optimize: false,
                    protocol: vc::ResultProtocol::Words,
                    ..vc::Options::new(target)
                };
                let artifact = match vc::compile(&p.world, w, &options) {
                    Ok(a) => a,
                    Err(vc::VerifiedError::Unsupported {
                        construct, detail, ..
                    }) => {
                        // Region and SyntaxMemory effects are later lanes.
                        assert!(
                            construct != vc::Unsupported::Effect || !detail.contains("Fail["),
                            "{n}: Fail effects must compile: {detail}"
                        );
                        *refusals.entry(format!("{construct:?}")).or_default() += 1;
                        break;
                    }
                    Err(e) => panic!("{n}: {e}"),
                };
                any = true;
                let Some(prefix) = runner(target) else {
                    continue;
                };
                let got = run(&artifact.bytes, &prefix, w);
                match (&got, &expected) {
                    (vc::Outcome::Words(words), Ok(value)) => {
                        assert_eq!(&(words[0] as i64).to_string(), value, "{n} on {target:?}");
                        if value == "-424242" {
                            raised += 1;
                        }
                    }
                    (vc::Outcome::Overflow | vc::Outcome::DivisionByZero, Err(e))
                        if e.contains("unhandled effect Fail") => {}
                    other => panic!("{n} on {target:?}: {other:?}"),
                }
            }
            compiled += usize::from(any);
        }
        eprintln!(
            "{} wrappers: {compiled} compile ({raised} target runs took a raise clause); other refusals {refusals:?}",
            names.len()
        );
        // 22 compiled before Milestone 7 (every handler was refused).
        assert!(compiled >= 54, "{compiled} of {} compile", names.len());
        assert!(raised > 0, "no run exercised a handled raise");
    });
}
