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


PROFILE = "vesper.lean4lean42/2"
UPSTREAM = "4faf51403bc4cb35cf6666d91fc9c938ae46d3aa"
BATTERIES = "8da40b72fece29b7d3fe3d768bac4c8910ce9bee"
BATTERIES_DOC_ALIAS_BLOB = "32d46ee883b58d6a383eed06eb98f33aa6530ded"
LEAN_COMMIT = "797c613eb9b6d4ec95db23e3e00af9ac6657f24b"
RELEASE = "leanprover/lean4:v4.24.0"
TOOL = Path(__file__).resolve().parent
REPOSITORY = TOOL.parent.parent
LOG_BYTES = 65536
RECEIPT_BYTES = 4 * 1024 * 1024
SOURCE_FILES = 8192
SOURCE_BYTES = 256 * 1024 * 1024
SETUP_SECONDS = 1800
PATCHES = ["tools/lean4lean42/patches/" + name + ".patch"
           for name in ("toolchain", "kernel", "replay")]
OVERLAY = {"source": "tools/lean4lean42/controls/VesperControls.lean",
           "destination": "Lean4Lean/VesperControls.lean"}
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
    fields = {"schema", "profile", "upstream_commit", "batteries_commit",
              "lean_release", "lean_commit", "patches", "controls_overlay",
              "known_fixes_blake3", "source_manifest_blake3", "resource_bundle_blake3", "build_argv",
              "checker_relative_path"}
    if not isinstance(pins, dict) or set(pins) != fields:
        fail("pins must have the exact frozen schema fields")
    expected = {"schema": 2, "profile": PROFILE, "upstream_commit": UPSTREAM,
                "batteries_commit": BATTERIES, "lean_release": RELEASE,
                "lean_commit": LEAN_COMMIT,
                "build_argv": ["{lake}", "build", "lean4lean"],
                "checker_relative_path": ".lake/build/bin/lean4lean"}
    if type(pins["schema"]) is not int or any(pins[key] != value for key, value in expected.items()):
        fail("pins differ from the frozen immutable recipe")
    if not all(digest_shape(pins[key]) for key in
               ("known_fixes_blake3", "source_manifest_blake3", "resource_bundle_blake3")):
        fail("invalid pin digest representation")
    patches = pins["patches"]
    if not isinstance(patches, list) or len(patches) != len(PATCHES):
        fail("pins must select the three fixed ordered patches")
    for entry, path in zip(patches, PATCHES):
        if (not isinstance(entry, dict) or set(entry) != {"path", "blake3"}
                or entry["path"] != path or not digest_shape(entry["blake3"])):
            fail("invalid fixed patch entry or patch order")
    overlays = pins["controls_overlay"]
    if (not isinstance(overlays, list) or len(overlays) != 1
            or not isinstance(overlays[0], dict)
            or set(overlays[0]) != {"source", "destination", "blake3"}
            or any(overlays[0][key] != value for key, value in OVERLAY.items())
            or not digest_shape(overlays[0]["blake3"])):
        fail("only the fixed controls umbrella overlay is supported")
    # Verify presence/ordinary file bounds, but do not pretend Python stdlib
    # implements BLAKE3. The genuine sealer checks complete byte identities.
    read_bounded(TOOL / "source-manifest.json", RECEIPT_BYTES)
    read_bounded(TOOL / "resource-source-manifest.json", RECEIPT_BYTES)
    read_bounded(TOOL / "known-fixes.json", RECEIPT_BYTES)
    patch_bytes = [read_bounded(REPOSITORY / path, RECEIPT_BYTES) for path in PATCHES]
    controls = read_bounded(REPOSITORY / OVERLAY["source"], RECEIPT_BYTES)
    return pins, patch_bytes, controls


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
             "TMPDIR", "ELAN_HOME", "PATH", "SSL_CERT_FILE", "SSL_CERT_DIR",
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
    if not re.search(r"\bversion 4\.24\.0\b", version) or LEAN_COMMIT not in version:
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
    if lake_version != actual_lake_version or not re.search(r"\b4\.24\.0\b", lake_version):
        fail("Lake is not the selected Lean 4.24 tool")
    return actual_lean, actual_lake, sysroot


def git_command(git, hooks, *args):
    return [git, "-c", "core.hooksPath=" + str(hooks), "-c", "core.autocrlf=false", *args]


def checkout(commands, git, hooks, directory, url, commit):
    commands.run(git_command(git, hooks, "init", "--quiet", directory))
    commands.run(git_command(git, hooks, "-C", directory, "fetch", "--quiet", "--depth=1", "--no-tags", url, commit))
    commands.run(git_command(git, hooks, "-C", directory, "checkout", "--quiet", "--detach", "FETCH_HEAD"))
    head, _ = commands.run(git_command(git, hooks, "-C", directory, "rev-parse", "--verify", "HEAD"))
    if head.decode("ascii").strip() != commit:
        fail("detached Git source identity mismatch")


def normalize_batteries_readme(commands, git, hooks, dependency):
    """Normalize exactly the published immutable documentation alias, once."""
    real_directory(dependency)
    real_directory(dependency / "docs")

    def query(*args):
        data, _ = commands.run(git_command(git, hooks, "-C", dependency, *args))
        return data

    if query("rev-parse", "--verify", "HEAD").decode("ascii").strip() != BATTERIES:
        fail("documentation normalization requires exact Batteries commit")
    path = "docs/README.md"
    expected = f"120000 blob {BATTERIES_DOC_ALIAS_BLOB}\t{path}\0".encode("ascii")
    if query("ls-tree", "-z", BATTERIES, "--", path) != expected:
        fail("pinned Batteries documentation alias metadata mismatch")
    index = f"120000 {BATTERIES_DOC_ALIAS_BLOB} 0\t{path}\0".encode("ascii")
    if query("ls-files", "--stage", "-z", "--", path) != index:
        fail("checked-out Batteries documentation alias index changed")
    if query("cat-file", "blob", BATTERIES_DOC_ALIAS_BLOB) != b"../README.md":
        fail("pinned Batteries documentation alias target mismatch")
    root_entry = query("ls-tree", "-z", BATTERIES, "--", "README.md")
    if re.fullmatch(rb"100644 blob [0-9a-f]{40}\tREADME\.md\x00", root_entry) is None:
        fail("pinned Batteries root README is not a regular source file")
    source = query("show", BATTERIES + ":README.md")
    if read_bounded(dependency / "README.md", LOG_BYTES) != source:
        fail("Batteries root README differs from pinned regular bytes")
    leaf = dependency / path
    info = leaf.lstat()
    if stat.S_ISLNK(info.st_mode):
        if os.readlink(leaf) != "../README.md":
            fail("actual Batteries documentation alias target changed")
    elif (os.name == "nt" and stat.S_ISREG(info.st_mode)
          and not getattr(info, "st_file_attributes", 0) & 0x400):
        if read_bounded(leaf, LOG_BYTES) != b"../README.md":
            fail("Windows literal documentation alias text changed")
    else:
        fail("expected only the pinned documentation symlink or Windows literal alias")
    leaf.unlink()
    with leaf.open("xb") as stream:
        stream.write(source)
    regular(leaf)


def validate_dependency(stage):
    if (stage / "lean-toolchain").read_text(encoding="utf-8").strip() != RELEASE:
        fail("patched Lean toolchain mismatch")
    # stdlib tomllib is available with the Python 3.11 minimum.
    import tomllib
    lake = tomllib.loads(read_bounded(stage / "lakefile.toml", LOG_BYTES).decode("utf-8"))
    requirements = lake.get("require")
    if (not isinstance(requirements, list) or len(requirements) != 1
            or requirements[0].get("name") != "batteries"
            or requirements[0].get("git") != "https://github.com/leanprover-community/batteries"
            or requirements[0].get("rev") != BATTERIES):
        fail("patched lakefile must name only exact Batteries dependency")
    manifest = decode_json(read_bounded(stage / "lake-manifest.json", LOG_BYTES), "Lake manifest")
    packages = manifest.get("packages") if isinstance(manifest, dict) else None
    if (not isinstance(packages, list) or len(packages) != 1
            or packages[0].get("name") != "batteries"
            or packages[0].get("rev") != BATTERIES or packages[0].get("inputRev") != BATTERIES):
        fail("patched Lake manifest dependency mismatch")


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
            if (path[:2] == (".lake", "build")
                    or path[:5] == (".lake", "packages", "batteries", ".lake", "build")):
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
    _, patches, controls = load_recipe()
    lean, lake = launcher(args.lean, "lean"), launcher(args.lake, "lake")
    git, driver = launcher(args.git), launcher(args.driver)
    stage = Path(tempfile.mkdtemp(prefix="." + destination.name + ".stage-", dir=destination.parent))
    scratch = None
    published = False
    try:
        scratch = Path(tempfile.mkdtemp(prefix="." + destination.name + ".work-", dir=destination.parent))
        hooks = scratch / "hooks"
        hooks.mkdir()
        commands = Commands(scratch, sanitized_env())
        actual_lean, actual_lake, _ = probe_tools(commands, lean, lake)
        checkout(commands, git, hooks, stage, "https://github.com/digama0/lean4lean.git", UPSTREAM)
        for index, data in enumerate(patches):
            patch = scratch / f"patch-{index}.patch"
            patch.write_bytes(data)
            commands.run(git_command(git, hooks, "-C", stage, "apply", "--check", "--index", "--whitespace=error", patch))
            commands.run(git_command(git, hooks, "-C", stage, "apply", "--index", "--whitespace=error", patch))
        validate_dependency(stage)
        dependency = stage / ".lake" / "packages" / "batteries"
        dependency.parent.mkdir(parents=True, exist_ok=True)
        checkout(commands, git, hooks, dependency, "https://github.com/leanprover-community/batteries.git", BATTERIES)
        normalize_batteries_readme(commands, git, hooks, dependency)
        overlay = stage / OVERLAY["destination"]
        if os.path.lexists(overlay):
            fail("controls overlay destination already exists")
        with overlay.open("xb") as stream:
            stream.write(controls)
        if any(os.path.lexists(stage / name) for name in ROOT_EXCLUSIONS):
            fail("reserved setup root filename already exists in source")
        before = source_paths(stage)
        # Published source-preflight amendment: the real embedded Rust pins and
        # BLAKE3 implementation approve source identity BEFORE Lake executes it.
        commands.run([driver, "--verify-setup-source", stage])
        build_argv = [str(actual_lake), "build", "lean4lean"]
        ordinary_environment = commands.environment
        if os.name == "nt":
            # Lake verifies an existing dependency's HEAD through Git. Add only
            # the explicitly selected executable directory for this build.
            commands.environment = dict(ordinary_environment)
            roots = ordinary_environment["PATH"].split(os.pathsep)
            roots.insert(1, str(git.parent))
            commands.environment["PATH"] = os.pathsep.join(dict.fromkeys(roots))
        try:
            stdout, stderr = commands.run(build_argv, cwd=stage)
        finally:
            commands.environment = ordinary_environment
        if source_paths(stage) != before:
            fail("build changed the source file set")
        for name, data in [("build.stdout", stdout), ("build.stderr", stderr)]:
            with (stage / name).open("xb") as stream:
                stream.write(data)
        inputs = {"schema": 1, "lean": str(actual_lean), "lake": str(actual_lake), "git": str(git),
                  "build_argv": build_argv, "build_exit": 0,
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
    for option in ("directory", "lean", "lake", "git", "driver"):
        parser.add_argument("--" + option, required=True, action=Once)
    args = parser.parse_args()
    try:
        destination = setup(args)
    except (SetupError, OSError, subprocess.SubprocessError, UnicodeError, ValueError) as error:
        print("lean4lean42 setup refused: " + str(error)[:4096], file=sys.stderr)
        return 1
    print("Published sealed tool root: " + str(destination))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
