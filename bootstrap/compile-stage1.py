"""Compile the root adapter using Cargo's already-built libraries, without
adding a workspace member or making the auditor depend on the root package.
"""
import pathlib
import subprocess
import sys

profile, source, output = map(pathlib.Path, sys.argv[1:4])
if sys.argv[4:] not in ([], ["--test"]):
    raise SystemExit("expected an optional --test")
search = sorted({str(p.parent) for p in profile.rglob("*")
                 if p.suffix in {".rlib", ".rmeta", ".so", ".dylib", ".dll"}})
command = ["rustc", "--edition=2024", "-O", str(source), "-o", str(output),
           f"--remap-path-prefix={source.resolve().parent.parent}=/vesper",
           f"--remap-path-prefix={profile.resolve().parent}=/target"] + sys.argv[4:]
for crate in ["vesper", "vesper_codegen_verified"]:
    command += ["--extern", f"{crate}={profile / ('lib' + crate + '.rlib')}"]
for directory in search:
    command += ["-L", f"dependency={directory}"]
subprocess.run(command, check=True)
