use std::path::{Path, PathBuf};
use std::process::Command;

/// Build the runner with the host C compiler.
pub fn build_runner(dir: &Path) -> PathBuf {
    let src = super::check_harness::repo().join("bootstrap/seed/runner/vwasm.c");
    let exe = dir.join(if cfg!(windows) { "vwasm.exe" } else { "vwasm" });
    let status = if cfg!(windows) && std::env::var_os("CC").is_none() {
        let target = if cfg!(target_arch = "aarch64") {
            "aarch64-pc-windows-msvc"
        } else {
            "x86_64-pc-windows-msvc"
        };
        let mut cmd = find_cl(target);
        cmd.arg("/nologo")
            .arg("/O2")
            .arg("/std:c11")
            .arg(format!("/Fo{}\\", dir.display()))
            .arg(format!("/Fe{}", exe.display()))
            .arg(&src)
            .status()
    } else {
        let cc = std::env::var("CC").unwrap_or_else(|_| "cc".into());
        Command::new(cc)
            .args(["-std=c11", "-O2", "-o"])
            .arg(&exe)
            .arg(&src)
            .arg("-lm")
            .status()
    };
    let status = status.expect("run the C compiler for bootstrap/seed/runner/vwasm.c");
    assert!(status.success(), "building vwasm failed: {status}");
    exe
}

#[cfg(windows)]
fn find_cl(target: &str) -> Command {
    find_msvc_tools::find_tool(target, "cl.exe")
        .expect("cl.exe (Visual Studio C compiler)")
        .to_command()
}
#[cfg(not(windows))]
fn find_cl(_: &str) -> Command {
    unreachable!()
}
