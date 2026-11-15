#!/usr/bin/env python3
"""Explicit immutable-source build; only the genuine Rust driver seals it."""

import argparse
import ctypes
import json
import os
from pathlib import Path
import re
import shutil
import signal
import stat
import subprocess
import sys
import tempfile
import threading
import time


PROFILE = "vesper.logos42/2"
UPSTREAM = "8ce95bd31f92a75670cc6e003c48a664b795e433"
EXPORTER = "df81913eee5519d5a88ae5b0fd701b5298cc29ab"
LEAN_COMMIT = "eb6cafb7290e8bd5f2937522d8d804f58f02bafc"
RELEASE = "leanprover/lean4:v4.35.0"
TOOL = Path(__file__).resolve().parent
REPOSITORY = TOOL.parent.parent
LOG_BYTES = 65536
RECEIPT_BYTES = 4 * 1024 * 1024
SOURCE_FILES = 8192
SOURCE_BYTES = 256 * 1024 * 1024
SETUP_SECONDS = 1800
PATCHES = ["tools/logos42/patches/lean435-primitives.patch"]
ADAPTERS = ['Cargo.toml', 'Cargo.lock', 'bridge/Cargo.toml', 'bridge/src/main.rs', 'Main.lean', 'lakefile.toml', 'lake-manifest.json', 'lean-toolchain', 'LogosControls.lean', 'controls/cases.json']
CARGO_RECIPE = ["{cargo}", "build", "-p", "vesper-logos-kernel", "--locked", "--release", "--bin", "logos-kernel",
                "--target-dir", ".lake/build/rust", "--offline"]
ROOT_EXCLUSIONS = {"setup-inputs.json", "build.stdout", "build.stderr",
                   "setup-receipt.json"}
PROXY_NAMES = {"HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "NO_PROXY",
               "http_proxy", "https_proxy", "all_proxy", "no_proxy"}


class SetupError(Exception):
    """A refused setup; no published directory or success receipt."""


def fail(message):
    raise SetupError(message)


def regular(path):
    try:
        info = path.lstat()
    except OSError as error:
        fail(f"missing input {path.name}: {error}")
    if not stat.S_ISREG(info.st_mode) or getattr(info, "st_file_attributes", 0) & 0x400:
        fail(f"expected regular file: {path}")
    return info


def real_directory(path):
    info = path.lstat()
    if not stat.S_ISDIR(info.st_mode) or getattr(info, "st_file_attributes", 0) & 0x400:
        fail(f"expected real directory, not a link/reparse point: {path}")


def read_bounded(path, limit):
    if regular(path).st_size > limit:
        fail(f"input exceeds byte cap: {path.name}")
    with path.open("rb") as stream:
        data = stream.read(limit + 1)
    if len(data) > limit:
        fail(f"input grew beyond byte cap: {path.name}")
    return data


def unique_object(pairs):
    value = {}
    for key, item in pairs:
        if key in value:
            fail(f"duplicate JSON key: {key}")
        value[key] = item
    return value


def decode_json(data, label):
    try:
        return json.loads(data.decode("utf-8"), object_pairs_hook=unique_object,
                          parse_constant=lambda value: fail("nonfinite JSON"))
    except (UnicodeError, ValueError, RecursionError) as error:
        fail(f"invalid {label} JSON: {error}")


def digest_shape(value):
    return (isinstance(value, list) and len(value) == 32
            and all(type(byte) is int and 0 <= byte <= 255 for byte in value))


def load_recipe():
    pins = decode_json(read_bounded(TOOL / "pins.json", 16384), "pins")
    expected = {"schema": 3, "profile": PROFILE, "upstream_commit": UPSTREAM,
                "exporter_commit": EXPORTER, "lean_release": RELEASE,
                "lean_commit": LEAN_COMMIT,
                "build_argv": ["{lake}", "build", "logos42"],
                "cargo_build_argv": CARGO_RECIPE,
                "checker_relative_path": ".lake/build/bin/logos42",
                "kernel_relative_path": ".lake/build/rust/release/logos-kernel"}
    fields = set(expected) | {"patches", "source_overlays", "known_fixes_blake3",
                             "source_manifest_blake3", "resource_bundle_blake3"}
    if (not isinstance(pins, dict) or set(pins) != fields
            or any(pins[key] != value for key, value in expected.items())):
        fail("pins differ from the immutable Logos recipe")
    if not all(digest_shape(pins[key]) for key in
               ("known_fixes_blake3", "source_manifest_blake3", "resource_bundle_blake3")):
        fail("invalid pin digest representation")
    if (len(pins["patches"]) != 1 or pins["patches"][0].get("path") != PATCHES[0]
            or not digest_shape(pins["patches"][0].get("blake3"))):
        fail("invalid fixed primitive port")
    overlays = pins["source_overlays"]
    if not isinstance(overlays, list) or len(overlays) != len(ADAPTERS):
        fail("invalid adapter overlay count")
    for overlay, path in zip(overlays, ADAPTERS):
        if (set(overlay) != {"source", "destination", "blake3"}
                or overlay["source"] != "tools/logos42/" + path
                or overlay["destination"] != path or not digest_shape(overlay["blake3"])):
            fail("invalid fixed adapter overlay")
    for name in ("source-manifest.json", "resource-source-manifest.json", "known-fixes.json"):
        read_bounded(TOOL / name, RECEIPT_BYTES)
    return pins, [read_bounded(REPOSITORY / path, RECEIPT_BYTES) for path in PATCHES]


def launcher(value, required_name=None):
    """Resolve parent directories, preserving the final invocation basename."""
    raw = Path(value).expanduser()
    if not raw.is_absolute() and raw.parent == Path("."):
        found = shutil.which(value)
        if found is None:
            fail(f"executable unavailable: {value}")
        raw = Path(found)
    raw = Path(os.path.abspath(raw))
    try:
        selected = raw.parent.resolve(strict=True) / raw.name
        real_directory(selected.parent)
        if not selected.is_file() or not os.access(selected, os.X_OK):
            fail(f"executable unavailable: {selected}")
    except OSError as error:
        fail(f"cannot select executable: {error}")
    if required_name is not None:
        expected = required_name + (".exe" if os.name == "nt" else "")
        matches = selected.name.casefold() == expected.casefold() if os.name == "nt" else selected.name == expected
        if not matches:
            fail(f"expected {expected} invocation basename")
    return selected


def sanitized_env(sysroot=None):
    # Keep required platform/user installation information, not arbitrary Lean,
    # Git config, package, Python, compiler or loader injection variables.
    names = {"HOME", "USERPROFILE", "HOMEDRIVE", "HOMEPATH", "SystemRoot",
             "SYSTEMROOT", "WINDIR", "COMSPEC", "PATHEXT", "TEMP", "TMP",
             "TMPDIR", "ELAN_HOME", "PATH", "CARGO_HOME", "RUSTUP_HOME", "SSL_CERT_FILE", "SSL_CERT_DIR",
             "REQUESTS_CA_BUNDLE", "CURL_CA_BUNDLE", "GIT_SSL_CAINFO", "GIT_SSL_CAPATH"}
    names |= PROXY_NAMES
    env = {key: value for key, value in os.environ.items() if key in names}
    env.update({"GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": os.devnull,
                "GIT_CONFIG_SYSTEM": os.devnull, "GIT_TERMINAL_PROMPT": "0",
                "LC_ALL": "C", "LANG": "C", "LEAN_NUM_THREADS": "1"})
    if sysroot is not None:
        if os.name == "nt":
            windows = Path(env.get("SystemRoot", env.get("SYSTEMROOT", r"C:\Windows")))
            env["PATH"] = os.pathsep.join(map(str, [sysroot / "bin", windows / "System32", windows]))
        env["LEAN_SYSROOT"] = str(sysroot)
    return env


class WindowsJob:
    """Documented Job Object plus suspended-start assignment and thread resume."""
    def __init__(self):
        from ctypes import wintypes as w
        self.w = w
        self.kernel = ctypes.WinDLL("kernel32", use_last_error=True)
        k = self.kernel

        class Basic(ctypes.Structure):
            _fields_ = [("process_time", ctypes.c_longlong), ("job_time", ctypes.c_longlong),
                        ("flags", w.DWORD), ("min_working", ctypes.c_size_t),
                        ("max_working", ctypes.c_size_t), ("active_limit", w.DWORD),
                        ("affinity", ctypes.c_size_t), ("priority", w.DWORD), ("scheduling", w.DWORD)]

        class IO(ctypes.Structure):
            _fields_ = [(name, ctypes.c_ulonglong) for name in ("reads", "writes", "other", "read_bytes", "write_bytes", "other_bytes")]

        class Extended(ctypes.Structure):
            _fields_ = [("basic", Basic), ("io", IO), ("process_memory", ctypes.c_size_t),
                        ("job_memory", ctypes.c_size_t), ("peak_process", ctypes.c_size_t), ("peak_job", ctypes.c_size_t)]

        class Accounting(ctypes.Structure):
            _fields_ = [("times", ctypes.c_longlong * 4), ("faults", w.DWORD),
                        ("processes", w.DWORD), ("active", w.DWORD), ("terminated", w.DWORD)]

        self.accounting = Accounting
        k.CreateJobObjectW.argtypes = [ctypes.c_void_p, w.LPCWSTR]
        k.CreateJobObjectW.restype = w.HANDLE
        k.SetInformationJobObject.argtypes = [w.HANDLE, ctypes.c_int, ctypes.c_void_p, w.DWORD]
        k.SetInformationJobObject.restype = w.BOOL
        k.AssignProcessToJobObject.argtypes = [w.HANDLE, w.HANDLE]
        k.AssignProcessToJobObject.restype = w.BOOL
        k.TerminateJobObject.argtypes = [w.HANDLE, w.UINT]
        k.TerminateJobObject.restype = w.BOOL
        k.QueryInformationJobObject.argtypes = [w.HANDLE, ctypes.c_int, ctypes.c_void_p, w.DWORD, ctypes.c_void_p]
        k.QueryInformationJobObject.restype = w.BOOL
        k.CloseHandle.argtypes = [w.HANDLE]
        k.CloseHandle.restype = w.BOOL
        self.handle = k.CreateJobObjectW(None, None)
        if not self.handle:
            raise ctypes.WinError(ctypes.get_last_error())
        limits = Extended()
        limits.basic.flags = 0x2000  # JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
        if not k.SetInformationJobObject(self.handle, 9, ctypes.byref(limits), ctypes.sizeof(limits)):
            error = ctypes.WinError(ctypes.get_last_error())
            self.close()
            raise error

    def start(self, child):
        w, k = self.w, self.kernel
        # Popen was created suspended: no compiler descendant can escape before
        # assignment. CPython's Windows Popen retains its owned process handle.
        if not k.AssignProcessToJobObject(self.handle, int(child._handle)):
            raise ctypes.WinError(ctypes.get_last_error())

        class ThreadEntry(ctypes.Structure):
            _fields_ = [("size", w.DWORD), ("usage", w.DWORD), ("thread", w.DWORD),
                        ("owner", w.DWORD), ("base_priority", w.LONG),
                        ("delta_priority", w.LONG), ("flags", w.DWORD)]

        k.CreateToolhelp32Snapshot.argtypes = [w.DWORD, w.DWORD]
        k.CreateToolhelp32Snapshot.restype = w.HANDLE
        k.Thread32First.argtypes = [w.HANDLE, ctypes.POINTER(ThreadEntry)]
        k.Thread32First.restype = w.BOOL
        k.Thread32Next.argtypes = [w.HANDLE, ctypes.POINTER(ThreadEntry)]
        k.Thread32Next.restype = w.BOOL
        k.OpenThread.argtypes = [w.DWORD, w.BOOL, w.DWORD]
        k.OpenThread.restype = w.HANDLE
        k.ResumeThread.argtypes = [w.HANDLE]
        k.ResumeThread.restype = w.DWORD
        snapshot = k.CreateToolhelp32Snapshot(4, 0)  # TH32CS_SNAPTHREAD
        if snapshot == ctypes.c_void_p(-1).value:
            raise ctypes.WinError(ctypes.get_last_error())
        try:
            entry = ThreadEntry()
            entry.size = ctypes.sizeof(entry)
            found = k.Thread32First(snapshot, ctypes.byref(entry))
            while found:
                if entry.owner == child.pid:
                    thread = k.OpenThread(2, False, entry.thread)  # THREAD_SUSPEND_RESUME
                    if not thread:
                        raise ctypes.WinError(ctypes.get_last_error())
                    try:
                        if k.ResumeThread(thread) == 0xFFFFFFFF:
                            raise ctypes.WinError(ctypes.get_last_error())
                    finally:
                        k.CloseHandle(thread)
                    return
                found = k.Thread32Next(snapshot, ctypes.byref(entry))
            fail("suspended Windows child thread was not found")
        finally:
            k.CloseHandle(snapshot)

    def active(self):
        info = self.accounting()
        if not self.kernel.QueryInformationJobObject(self.handle, 1, ctypes.byref(info), ctypes.sizeof(info), None):
            raise ctypes.WinError(ctypes.get_last_error())
        return info.active

    def stop(self):
        if not self.kernel.TerminateJobObject(self.handle, 1):
            raise ctypes.WinError(ctypes.get_last_error())
        deadline = time.monotonic() + 5
        while self.active():
            if time.monotonic() >= deadline:
                fail("Windows process tree did not terminate within grace")
            time.sleep(0.01)

    def close(self):
        if self.handle:
            self.kernel.CloseHandle(self.handle)
            self.handle = None


class Commands:
    def __init__(self, cwd, environment):
        self.cwd = cwd
        self.environment = environment
        self.deadline = time.monotonic() + SETUP_SECONDS

    def run(self, argv, cwd=None, stdout_cap=LOG_BYTES, seconds=None):
        if time.monotonic() >= self.deadline:
            fail("setup deadline exceeded")
        deadline = self.deadline if seconds is None else min(self.deadline, time.monotonic() + seconds)
        job = WindowsJob() if os.name == "nt" else None
        kwargs = {"start_new_session": True} if job is None else {"creationflags": 0x4 | 0x200}
        try:
            child = subprocess.Popen([str(value) for value in argv], cwd=cwd or self.cwd,
                                     env=self.environment, stdin=subprocess.DEVNULL,
                                     stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                     shell=False, **kwargs)
        except BaseException:
            if job is not None:
                job.close()
            raise
        if job is not None:
            try:
                job.start(child)
            except BaseException:
                job.close()
                child.kill()
                child.wait(timeout=5)
                child.stdout.close()
                child.stderr.close()
                raise
        readers = []
        try:
            fault = threading.Event()
            outputs = [bytearray(), bytearray()]
            failures = []

            def drain(stream, output, cap):
                try:
                    while True:
                        data = stream.read1(8192)
                        if not data:
                            break
                        room = cap - len(output)
                        output.extend(data[:max(room, 0)])
                        if len(data) > room:
                            fault.set()
                except OSError as error:
                    failures.append(str(error))
                    fault.set()
                finally:
                    stream.close()

            readers = [threading.Thread(target=drain, args=(child.stdout, outputs[0], stdout_cap), daemon=True),
                       threading.Thread(target=drain, args=(child.stderr, outputs[1], LOG_BYTES), daemon=True)]
            for reader in readers:
                reader.start()
            reason = None
            while True:
                status = child.poll()
                observed = time.monotonic()
                if fault.is_set() or observed >= deadline:
                    reason = "subprocess output cap exceeded" if fault.is_set() else "subprocess deadline exceeded"
                    break
                if status is not None:
                    break
                time.sleep(0.01)
            if reason is not None:
                if os.name != "nt":
                    try:
                        os.killpg(child.pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
                else:
                    job.stop()
            try:
                child.wait(timeout=5)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait(timeout=5)
                reason = reason or "subprocess reap exceeded grace"
            if job is not None:
                try:
                    if job.active():
                        job.stop()
                        reason = reason or "Windows child left live descendants"
                finally:
                    job.close()
            for reader in readers:
                reader.join(timeout=5)
            if any(reader.is_alive() for reader in readers):
                if os.name != "nt":
                    try:
                        os.killpg(child.pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
                for reader in readers:
                    reader.join(timeout=5)
                fail("subprocess pipe did not close after reaping")
            if reason or fault.is_set() or failures:
                fail(reason or "subprocess output/read fault")
            if child.returncode != 0:
                detail = outputs[1].decode("utf-8", errors="replace")[-2048:]
                for name in PROXY_NAMES:
                    value = self.environment.get(name)
                    if value:
                        detail = detail.replace(value, "<proxy>")
                fail(f"{Path(argv[0]).name} exited {child.returncode}: {detail}")
            return bytes(outputs[0]), bytes(outputs[1])
        finally:
            # Kill/reap ownership is independent of a successful parser/build
            # path. Job close kills descendants even if an earlier query failed.
            if job is not None:
                job.close()
            elif os.name != "nt":
                try:
                    os.killpg(child.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
            if child.poll() is None:
                child.kill()
            child.wait(timeout=5)
            for reader in readers:
                reader.join(timeout=5)
            if any(reader.is_alive() for reader in readers):
                fail("subprocess cleanup could not join every pipe reader")


def probe_tools(commands, lean, lake):
    def text(argv):
        data, _ = commands.run(argv, seconds=30)
        try:
            return data.decode("utf-8").strip()
        except UnicodeError:
            fail("tool probe emitted non-UTF8")

    version = text([lean, "--version"])
    if not re.search(r"\bversion 4\.35\.0\b", version) or LEAN_COMMIT not in version:
        fail("Lean release/commit identity mismatch")
    prefix = text([lean, "--print-prefix"])
    try:
        sysroot = Path(prefix).resolve(strict=True)
    except OSError as error:
        fail(f"invalid Lean sysroot: {error}")
    if not sysroot.is_dir():
        fail("Lean prefix is not a directory")
    real_directory(sysroot)
    suffix = ".exe" if os.name == "nt" else ""
    actual_lean = launcher(str(sysroot / "bin" / ("lean" + suffix)), "lean")
    actual_lake = launcher(str(sysroot / "bin" / ("lake" + suffix)), "lake")
    commands.environment = sanitized_env(sysroot)
    actual_version = text([actual_lean, "--version"])
    if actual_version != version or Path(text([actual_lean, "--print-prefix"])).resolve(strict=True) != sysroot:
        fail("launcher and actual Lean disagree")
    lake_version = text([lake, "--version"])
    actual_lake_version = text([actual_lake, "--version"])
    if lake_version != actual_lake_version or not re.search(r"\b4\.35\.0\b", lake_version):
        fail("Lake is not the selected Lean 4.35 tool")
    return actual_lean, actual_lake, sysroot


def git_command(git, hooks, *args):
    return [git, "-c", "core.hooksPath=" + str(hooks), "-c", "core.autocrlf=false", *args]


def checkout(commands, git, hooks, directory, url, commit, selected=None):
    commands.run(git_command(git, hooks, "init", "--quiet", directory))
    commands.run(git_command(git, hooks, "-C", directory, "fetch", "--quiet", "--depth=1", "--no-tags", url, commit))
    if selected:
        commands.run(git_command(git, hooks, "-C", directory, "sparse-checkout", "set", "--no-cone", *selected))
    commands.run(git_command(git, hooks, "-C", directory, "checkout", "--quiet", "--detach", "FETCH_HEAD"))
    head, _ = commands.run(git_command(git, hooks, "-C", directory, "rev-parse", "--verify", "HEAD"))
    if head.decode("ascii").strip() != commit:
        fail("detached Git source identity mismatch")


def copy_sources(source, destination):
    """Copy only regular, bounded source files, never following upstream links."""
    for current, dirs, files in os.walk(source, followlinks=False):
        directory = Path(current)
        real_directory(directory)
        for name in dirs:
            real_directory(directory / name)
        relative = directory.relative_to(source)
        target = destination / relative
        target.mkdir(parents=True, exist_ok=True)
        for name in files:
            data = read_bounded(directory / name, SOURCE_BYTES)
            with (target / name).open("xb") as stream:
                stream.write(data)


def source_paths(stage):
    """Exact shared source walk; the Rust sealer supplies authoritative hashes."""
    paths = []
    total = 0
    real_directory(stage)

    pending = [(stage, ())]
    entries = 0
    while pending:
        directory, relative = pending.pop()
        children = []
        with os.scandir(directory) as scan:
            for entry in scan:
                entries += 1
                if entries > SOURCE_FILES * 4:
                    fail("source walk entry cap exceeded")
                children.append(entry.name)
        children.sort(key=lambda name: name.encode("utf-8"))
        for name in children:
            entry = directory / name
            path = (*relative, name)
            if name == ".git" or (not relative and name in ROOT_EXCLUSIONS):
                continue
            # Published strict walk: unknown Lake state is an input too. Only
            # these two exact generated trees are exempt from the manifest.
            if path[:2] == (".lake", "build"):
                continue
            label = "/".join(path)
            if len(label.encode("utf-8")) > 4096:
                fail("source path cap exceeded")
            info = entry.lstat()
            if getattr(info, "st_file_attributes", 0) & 0x400:
                fail(f"reparse source entry: {label}")
            if stat.S_ISDIR(info.st_mode):
                pending.append((entry, path))
            elif stat.S_ISREG(info.st_mode):
                total += info.st_size
                if len(paths) >= SOURCE_FILES or total > SOURCE_BYTES:
                    fail("source inventory cap exceeded")
                paths.append(label)
            else:
                fail(f"symlink/nonregular source entry: {'/'.join(path)}")

    paths.sort(key=lambda name: name.encode("utf-8"))
    return paths


def exclusive_rename(stage, destination):
    """One atomic no-clobber publication; never fall back to replacement."""
    if os.name == "nt":
        os.rename(stage, destination)
        return
    library = ctypes.CDLL(None, use_errno=True)
    old, new = os.fsencode(stage), os.fsencode(destination)
    if sys.platform.startswith("linux"):
        try:
            operation = library.renameat2
        except AttributeError:
            fail("atomic exclusive renameat2 is unavailable")
        operation.argtypes = [ctypes.c_int, ctypes.c_char_p, ctypes.c_int,
                              ctypes.c_char_p, ctypes.c_uint]
        operation.restype = ctypes.c_int
        result = operation(-100, old, -100, new, 1)  # AT_FDCWD, RENAME_NOREPLACE
    elif sys.platform == "darwin":
        try:
            operation = library.renamex_np
        except AttributeError:
            fail("atomic exclusive renamex_np is unavailable")
        operation.argtypes = [ctypes.c_char_p, ctypes.c_char_p, ctypes.c_uint]
        operation.restype = ctypes.c_int
        result = operation(old, new, 4)  # RENAME_EXCL
    else:
        fail("atomic exclusive publication is unsupported on this platform")
    if result != 0:
        code = ctypes.get_errno()
        raise OSError(code, os.strerror(code), str(destination))


def fresh_destination(value):
    raw = Path(value).expanduser()
    if raw.name in {"", ".", ".."}:
        fail("destination must name a new directory")
    try:
        parent = raw.absolute().parent.resolve(strict=True)
    except OSError as error:
        fail(f"destination parent must exist: {error}")
    destination = parent / raw.name
    real_directory(parent)
    if os.path.lexists(destination):
        fail("destination already exists; setup never overwrites it")
    return destination


def setup(args):
    destination = fresh_destination(args.directory)
    _, patches = load_recipe()
    lean, lake = launcher(args.lean, "lean"), launcher(args.lake, "lake")
    git, driver = launcher(args.git), launcher(args.driver)
    cargo = launcher(args.cargo, "cargo")
    stage = Path(tempfile.mkdtemp(prefix="." + destination.name + ".stage-", dir=destination.parent))
    scratch = None
    published = False
    try:
        scratch = Path(tempfile.mkdtemp(prefix="." + destination.name + ".work-", dir=destination.parent))
        hooks = scratch / "hooks"
        hooks.mkdir()
        commands = Commands(scratch, sanitized_env())
        actual_lean, actual_lake, _ = probe_tools(commands, lean, lake)
        # Only the two self-contained kernel packages enter the bound source tree.
        upstream = scratch / "logos"
        checkout(commands, git, hooks, upstream,
                 "https://github.com/Brahmastra-Labs/logicaffeine.git", UPSTREAM,
                 ["/crates/logicaffeine_kernel/", "/crates/logicaffeine_base/"])
        for name in ("logicaffeine_kernel", "logicaffeine_base"):
            copy_sources(upstream / "crates" / name, stage / "vendor/logos/crates" / name)
        exporter = scratch / "exporter"
        checkout(commands, git, hooks, exporter,
                 "https://github.com/leanprover/lean4export.git", EXPORTER)
        for source, target in (("Export.lean", "Export.lean"), ("LICENSE", "EXPORT-LICENSE")):
            (stage / target).write_bytes(read_bounded(exporter / source, RECEIPT_BYTES))
        for index, data in enumerate(patches):
            patch = scratch / f"patch-{index}.patch"
            patch.write_bytes(data)
            commands.run(git_command(git, hooks, "-C", stage, "apply", "--check", "--whitespace=error", patch))
            commands.run(git_command(git, hooks, "-C", stage, "apply", "--whitespace=error", patch))
        # Cases are raw exports, so the actual Logos kernel has to refuse them.
        copy_sources(TOOL / "controls", stage / "controls")
        for path in ADAPTERS:
            target = stage / path
            data = read_bounded(TOOL / path, RECEIPT_BYTES)
            if target.exists():
                if read_bounded(target, RECEIPT_BYTES) != data:
                    fail("adapter overlay collision")
            else:
                target.parent.mkdir(parents=True, exist_ok=True)
                with target.open("xb") as stream:
                    stream.write(data)
        if any(os.path.lexists(stage / name) for name in ROOT_EXCLUSIONS):
            fail("reserved setup root filename already exists in source")
        before = source_paths(stage)
        # Published source-preflight amendment: the real embedded Rust pins and
        # BLAKE3 implementation approve source identity BEFORE Lake executes it.
        commands.run([driver, "--verify-setup-source", stage])
        build_argv = [str(actual_lake), "build", "logos42"]
        cargo_build_argv = [str(cargo), *CARGO_RECIPE[1:]]
        commands.environment["PATH"] = os.pathsep.join([str(cargo.parent), commands.environment["PATH"]])
        suffix = ".exe" if os.name == "nt" else ""
        rustc = cargo.parent / ("rustc" + suffix)
        if rustc.is_file():
            commands.environment["RUSTC"] = str(rustc)
        commands.run([cargo, "--version"], seconds=30)
        # Fetch checksummed, locked registry inputs before the offline build.
        commands.run([cargo, "fetch", "--locked"], cwd=stage)
        lean_stdout, lean_stderr = commands.run(build_argv, cwd=stage)
        rust_stdout, rust_stderr = commands.run(cargo_build_argv, cwd=stage)
        stdout, stderr = lean_stdout + rust_stdout, lean_stderr + rust_stderr
        if len(stdout) > LOG_BYTES or len(stderr) > LOG_BYTES:
            fail("combined build log cap exceeded")
        if source_paths(stage) != before:
            fail("build changed the source file set")
        for name, data in [("build.stdout", stdout), ("build.stderr", stderr)]:
            with (stage / name).open("xb") as stream:
                stream.write(data)
        inputs = {"schema": 2, "lean": str(actual_lean), "lake": str(actual_lake), "git": str(git), "cargo": str(cargo),
                  "build_argv": build_argv, "cargo_build_argv": cargo_build_argv, "build_exit": 0,
                  "build_stdout": "build.stdout", "build_stderr": "build.stderr"}
        encoded = (json.dumps(inputs, ensure_ascii=True, separators=(",", ":")) + "\n").encode("utf-8")
        if len(encoded) > 16384 or sum(len(s.encode("utf-8")) for s in build_argv) > 8192:
            fail("setup input cap exceeded")
        with (stage / "setup-inputs.json").open("xb") as stream:
            stream.write(encoded)
        stdout, _ = commands.run([driver, "--seal-setup", stage], stdout_cap=RECEIPT_BYTES)
        receipt = read_bounded(stage / "setup-receipt.json", RECEIPT_BYTES)
        if decode_json(stdout, "sealer stdout") != decode_json(receipt, "sealed receipt"):
            fail("genuine sealer stdout and receipt differ")
        if os.path.lexists(destination):
            fail("destination appeared during setup; refusing publication")
        exclusive_rename(stage, destination)
        published = True
        return destination
    finally:
        if not published:
            shutil.rmtree(stage, ignore_errors=True)
        if scratch is not None:
            shutil.rmtree(scratch, ignore_errors=True)


def main():
    class Once(argparse.Action):
        def __call__(self, parser, namespace, value, option_string=None):
            if getattr(namespace, self.dest, None) is not None:
                parser.error(f"duplicate option: {option_string}")
            setattr(namespace, self.dest, value)

    parser = argparse.ArgumentParser(description=__doc__, allow_abbrev=False)
    for option in ("directory", "lean", "lake", "git", "cargo", "driver"):
        parser.add_argument("--" + option, required=True, action=Once)
    args = parser.parse_args()
    try:
        destination = setup(args)
    except (SetupError, OSError, subprocess.SubprocessError, UnicodeError, ValueError) as error:
        print("logos42 setup refused: " + str(error)[:4096], file=sys.stderr)
        return 1
    print("Published sealed tool root: " + str(destination))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
