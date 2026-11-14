//! Source, tool, loader and artifact association for the fixed replay profile.
#![forbid(unsafe_code)]
use super::protocol::{ExpectedControl, Inventory, Machine, SessionResult};
use super::*;
use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::Component,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const MIB: usize = 1024 * 1024;
const MODEL0: &[u8] = include_bytes!("../../../lean/RewriteWitness.lean");
const MODEL1: &[u8] = include_bytes!("../../../lean/ArithmeticOracle.lean");
static NEXT: AtomicU64 = AtomicU64::new(0);
fn identity(detail: impl Into<String>) -> Error {
    Error::new(ErrorKind::ToolIdentity, detail)
}
fn limit(detail: &str) -> Error {
    Error::new(ErrorKind::OutputLimit, detail)
}
fn until(ms: u64) -> Instant {
    Instant::now() + Duration::from_millis(ms)
}
fn timely(end: Instant) -> Result<(), Error> {
    if Instant::now() > end {
        Err(Error::new(
            ErrorKind::Timeout,
            "artifact inspection deadline",
        ))
    } else {
        Ok(())
    }
}
fn checked(a: u64, b: u64) -> Result<u64, Error> {
    a.checked_add(b)
        .ok_or_else(|| limit("byte counter overflow"))
}
fn text(path: &Path) -> Result<String, Error> {
    let s = path
        .to_str()
        .ok_or_else(|| identity("non-UTF8 tool path"))?;
    if s.len() > 4096 {
        return Err(limit("path bytes"));
    }
    Ok(s.into())
}
fn relative(path: &str) -> Result<&Path, Error> {
    let p = Path::new(path);
    if path.is_empty()
        || path.len() > 4096
        || path.contains(['\\', ':'])
        || path
            .split('/')
            .any(|s| s.is_empty() || s == "." || s == "..")
        || p.is_absolute()
        || p.components().any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err(identity("invalid relative evidence path"));
    }
    Ok(p)
}
fn is_reparse(m: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        m.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        let _ = m;
        false
    }
}
pub(super) fn regular_meta(path: &Path) -> Result<fs::Metadata, Error> {
    let m = fs::symlink_metadata(path)?;
    if m.file_type().is_symlink() || is_reparse(&m) {
        return Err(identity("symlink/reparse point in regular input"));
    }
    Ok(m)
}
fn label(root: &Path, path: &Path) -> Result<String, Error> {
    let p = path
        .strip_prefix(root)
        .map_err(|_| identity("path outside bound root"))?;
    text(p).map(|s| s.replace('\\', "/"))
}
fn blob(h: &mut blake3::Hasher, s: &str) {
    h.update(&(s.len() as u32).to_le_bytes());
    h.update(s.as_bytes());
}
fn digest(bytes: &[u8]) -> Digest {
    *blake3::hash(bytes).as_bytes()
}
fn reserve<T>(v: &mut Vec<T>, n: usize) -> Result<(), Error> {
    v.try_reserve_exact(n)
        .map_err(|_| Error::new(ErrorKind::Allocation, "bounded vector allocation"))
}

pub fn bounded_json<T: serde::de::DeserializeOwned>(bytes: &[u8], cap: usize) -> Result<T, Error> {
    if bytes.len() > cap {
        return Err(limit("JSON byte cap"));
    }
    let mut depth = 0usize;
    let mut string = false;
    let mut escape = false;
    for &b in bytes {
        if string {
            if escape {
                escape = false
            } else if b == b'\\' {
                escape = true
            } else if b == b'"' {
                string = false
            }
        } else {
            match b {
                b'"' => string = true,
                b'[' | b'{' => {
                    depth += 1;
                    if depth > 32 {
                        return Err(limit("JSON depth"));
                    }
                }
                b']' | b'}' => {
                    depth = depth
                        .checked_sub(1)
                        .ok_or_else(|| identity("JSON nesting"))?
                }
                _ => {}
            }
        }
    }
    serde_json::from_slice(bytes).map_err(|_| identity("invalid bounded metadata schema"))
}
fn read_bytes(path: &Path, cap: usize) -> Result<Vec<u8>, Error> {
    if !regular_meta(path)?.is_file() {
        return Err(identity("metadata must be a regular file"));
    }
    let n = usize::try_from(fs::metadata(path)?.len()).map_err(|_| limit("metadata size"))?;
    if n > cap {
        return Err(limit("metadata cap"));
    }
    let mut out = Vec::new();
    reserve(&mut out, n)?;
    let mut f = File::open(path)?;
    let mut buffer = [0u8; 65536];
    loop {
        let size = f.read(&mut buffer)?;
        if size == 0 {
            break;
        }
        let next = out
            .len()
            .checked_add(size)
            .ok_or_else(|| limit("metadata growth"))?;
        if next > n {
            return Err(identity("metadata changed while reading"));
        }
        out.extend_from_slice(&buffer[..size]);
    }
    if out.len() != n {
        return Err(identity("metadata shortened while reading"));
    }
    Ok(out)
}
fn file_digest(root: &Path, path: &Path, max: u64, end: Instant) -> Result<FileDigest, Error> {
    timely(end)?;
    let metadata = regular_meta(path)?;
    if !metadata.is_file() {
        return Err(identity("expected regular bound file"));
    }
    if metadata.len() > max {
        return Err(limit("bound file byte cap"));
    }
    let mut f = File::open(path)?;
    let mut h = blake3::Hasher::new();
    let mut buffer = [0u8; 65536];
    let mut bytes = 0;
    loop {
        timely(end)?;
        let n = f.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        bytes = checked(bytes, n as u64)?;
        if bytes > max {
            return Err(limit("bound file growth"));
        }
        h.update(&buffer[..n]);
    }
    if bytes != metadata.len() || fs::symlink_metadata(path)?.len() != bytes {
        return Err(identity("file changed while hashing"));
    }
    Ok(FileDigest {
        path: label(root, path)?,
        bytes,
        blake3: *h.finalize().as_bytes(),
    })
}
fn present(path: &Path) -> Result<bool, Error> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}
pub(super) fn manifest_hash(domain: &[u8], files: &[FileDigest]) -> Digest {
    let mut h = blake3::Hasher::new();
    h.update(domain);
    h.update(&1u16.to_le_bytes());
    h.update(&(files.len() as u32).to_le_bytes());
    for f in files {
        blob(&mut h, &f.path);
        h.update(&f.bytes.to_le_bytes());
        h.update(&f.blake3);
    }
    *h.finalize().as_bytes()
}
struct Inputs {
    pins: Pins,
    sources: SourceManifest,
    controls: Vec<ExpectedControl>,
}
fn inputs() -> Result<Inputs, Error> {
    let pins: Pins = bounded_json(PINS.as_bytes(), 4 * MIB)?;
    if pins.schema != 2
        || pins.profile != PROFILE
        || pins.upstream_commit != UPSTREAM
        || pins.batteries_commit != BATTERIES
        || pins.lean_release != "leanprover/lean4:v4.24.0"
        || pins.lean_commit != LEAN_COMMIT
        || pins.build_argv != ["{lake}", "build", "lean4lean"]
        || pins.checker_relative_path != ".lake/build/bin/lean4lean"
    {
        return Err(Error::new(
            ErrorKind::UnsupportedCandidate,
            "unassembled or incompatible embedded profile",
        ));
    }
    if resource::verify_bundle()? != pins.resource_bundle_blake3 {
        return Err(identity("embedded resource bundle association"));
    }
    let paths = [
        "tools/lean4lean42/patches/toolchain.patch",
        "tools/lean4lean42/patches/kernel.patch",
        "tools/lean4lean42/patches/replay.patch",
    ];
    if pins.patches.len() != 3
        || pins
            .patches
            .iter()
            .zip(paths)
            .zip(PATCHES)
            .any(|((p, path), bytes)| p.path != path || p.blake3 != digest(bytes))
    {
        return Err(identity("embedded patch association"));
    }
    if pins.controls_overlay.len() != 1
        || pins.controls_overlay[0].source != "tools/lean4lean42/controls/VesperControls.lean"
        || pins.controls_overlay[0].destination != "Lean4Lean/VesperControls.lean"
        || pins.controls_overlay[0].blake3 != digest(CONTROLS_SOURCE)
        || pins.known_fixes_blake3 != digest(KNOWN_FIXES.as_bytes())
    {
        return Err(identity("embedded controls association"));
    }
    let sources: SourceManifest = bounded_json(SOURCE_MANIFEST.as_bytes(), 4 * MIB)?;
    if sources.schema != 1
        || sources.files.is_empty()
        || sources.files.len() > 8192
        || manifest_hash(b"vesper.lean4lean42.sources\0", &sources.files)
            != pins.source_manifest_blake3
    {
        return Err(identity("embedded source manifest association"));
    }
    let mut total = 0;
    let mut previous = "";
    for f in &sources.files {
        relative(&f.path)?;
        if f.path.as_str() <= previous {
            return Err(identity("source manifest order"));
        }
        previous = &f.path;
        total = checked(total, f.bytes)?;
        if total > 256 * MIB as u64 {
            return Err(limit("source manifest bytes"));
        }
    }
    let v: serde_json::Value = bounded_json(KNOWN_FIXES.as_bytes(), 4 * MIB)?;
    let obj = v
        .as_object()
        .ok_or_else(|| identity("known-fixes object"))?;
    let keys = [
        "schema",
        "role",
        "upstream_base",
        "lean_toolchain",
        "batteries_commit",
        "controls",
        "fixes",
        "source_sha256",
        "executions",
        "unexecuted",
    ];
    if obj.len() != keys.len()
        || keys.iter().any(|k| !obj.contains_key(*k))
        || v["schema"] != 1
        || v["role"] != "review_evidence_not_success_receipt"
        || v["upstream_base"] != UPSTREAM
        || v["lean_toolchain"] != "leanprover/lean4:v4.24.0"
        || v["batteries_commit"] != BATTERIES
    {
        return Err(identity("known-fixes profile"));
    }
    fn walk(v: &serde_json::Value, count: &mut usize) -> Result<(), Error> {
        *count += 1;
        if *count > 65536 {
            return Err(limit("known-fixes values"));
        }
        match v {
            serde_json::Value::String(s) if s.len() > 65536 => Err(limit("known-fixes string")),
            serde_json::Value::Array(a) => {
                for x in a {
                    walk(x, count)?
                }
                Ok(())
            }
            serde_json::Value::Object(a) => {
                for (key, x) in a {
                    if key.len() > 65536 {
                        return Err(limit("known-fixes key"));
                    }
                    walk(x, count)?
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
    walk(&v, &mut 0)?;
    let controls: Vec<ExpectedControl> =
        serde_json::from_value(v["controls"].clone()).map_err(|_| identity("control registry"))?;
    if controls.is_empty() || controls.len() > 256 {
        return Err(identity("control registry count"));
    }
    let mut seen = BTreeSet::new();
    for c in &controls {
        if c.source != "tools/lean4lean42/controls/VesperControls.lean"
            || c.id.is_empty()
            || c.id.len() > 128
            || c.expected_reason.is_empty()
            || c.expected_reason.len() > 128
            || !c
                .id
                .bytes()
                .chain(c.expected_reason.bytes())
                .all(|b| b.is_ascii_graphic() || b == b' ')
            || !seen.insert(c.id.as_str())
        {
            return Err(identity("control expectation shape"));
        }
    }
    Ok(Inputs {
        pins,
        sources,
        controls,
    })
}

fn source_skip(path: &str) -> bool {
    if path.split('/').any(|c| c == ".git") {
        return true;
    }
    if [
        "setup-inputs.json",
        "build.stdout",
        "build.stderr",
        "setup-receipt.json",
    ]
    .contains(&path)
    {
        return true;
    }
    [".lake/build", ".lake/packages/batteries/.lake/build"]
        .iter()
        .any(|p| path == *p || path.strip_prefix(p).is_some_and(|s| s.starts_with('/')))
}
struct FileScan<'a> {
    root: &'a Path,
    max_files: usize,
    max_bytes: u64,
    end: Instant,
    sources: bool,
    files: Vec<FileDigest>,
    bytes: u64,
    entries: u64,
    allowed_dirs: Option<BTreeSet<PathBuf>>,
}
impl<'a> FileScan<'a> {
    fn new(root: &'a Path, max_files: usize, max_bytes: u64, end: Instant, sources: bool) -> Self {
        Self {
            root,
            max_files,
            max_bytes,
            end,
            sources,
            files: Vec::new(),
            bytes: 0,
            entries: 0,
            allowed_dirs: None,
        }
    }
    fn walk(&mut self, dir: &Path, depth: usize) -> Result<(), Error> {
        timely(self.end)?;
        if depth > 64 {
            return Err(limit("filesystem depth"));
        }
        if !regular_meta(dir)?.is_dir() {
            return Err(identity("bound directory is not regular"));
        }
        for entry in fs::read_dir(dir)? {
            timely(self.end)?;
            self.entries = checked(self.entries, 1)?;
            if self.entries
                > (self.max_files as u64)
                    .checked_mul(4)
                    .ok_or_else(|| limit("entry bound"))?
            {
                return Err(limit("filesystem entry count"));
            }
            let path = entry?.path();
            let name = label(self.root, &path)?;
            if self.sources && source_skip(&name) {
                continue;
            }
            let m = regular_meta(&path)?;
            if m.is_dir() {
                if self
                    .allowed_dirs
                    .as_ref()
                    .is_some_and(|dirs| !dirs.contains(&path))
                {
                    return Err(identity("unexpected resource directory"));
                }
                self.walk(&path, depth + 1)?
            } else if m.is_file() {
                if self.files.len() >= self.max_files {
                    return Err(limit("file inventory count"));
                }
                let f = file_digest(
                    self.root,
                    &path,
                    self.max_bytes.saturating_sub(self.bytes),
                    self.end,
                )?;
                self.bytes = checked(self.bytes, f.bytes)?;
                reserve(&mut self.files, 1)?;
                self.files.push(f);
            } else {
                return Err(identity("nonregular file in bound tree"));
            }
        }
        Ok(())
    }
    fn finish(mut self) -> Vec<FileDigest> {
        self.files.sort_unstable_by(|a, b| a.path.cmp(&b.path));
        self.files
    }
}
fn sources(root: &Path, end: Instant) -> Result<Vec<FileDigest>, Error> {
    let mut scan = FileScan::new(root, 8192, 256 * MIB as u64, end, true);
    scan.walk(root, 0)?;
    Ok(scan.finish())
}
pub fn verify_sources(root: &Path) -> Result<Digest, Error> {
    let i = inputs()?;
    let root = fs::canonicalize(root)?;
    let actual = sources(&root, until(Limits::default().inventory_timeout_ms))?;
    if actual != i.sources.files {
        return Err(identity("source/dependency set or contents changed"));
    }
    Ok(i.pins.source_manifest_blake3)
}
fn build_outputs(root: &Path, limits: &Limits, end: Instant) -> Result<Vec<FileDigest>, Error> {
    let mut scan = FileScan::new(
        root,
        limits.build_output_files,
        limits.build_output_bytes,
        end,
        false,
    );
    for p in [".lake/build", ".lake/packages/batteries/.lake/build"] {
        scan.walk(&root.join(p), 0)?
    }
    Ok(scan.finish())
}
pub fn read_receipt(root: &Path) -> Result<Receipt, Error> {
    bounded_json(
        &read_bytes(&root.join("setup-receipt.json"), 4 * MIB)?,
        4 * MIB,
    )
}

fn runtime_name(name: &str, bin: bool) -> bool {
    let owned;
    let name = if cfg!(windows) {
        owned = name.to_ascii_lowercase();
        owned.as_str()
    } else {
        name
    };
    if bin {
        return name.ends_with(".dll");
    }
    if [".a", ".lib", ".dylib", ".dll", ".so"]
        .iter()
        .any(|s| name.ends_with(s))
    {
        return true;
    }
    name.rsplit_once(".so.").is_some_and(|(_, suffix)| {
        suffix
            .split('.')
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
    })
}
fn normalize(path: &Path) -> Result<PathBuf, Error> {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::ParentDir => {
                if !out.pop() {
                    return Err(identity("library alias escaped root"));
                }
            }
            Component::CurDir => {}
            _ => out.push(c.as_os_str()),
        }
    }
    Ok(out)
}
fn real_parents(root: &Path, path: &Path) -> Result<(), Error> {
    let rel = path
        .strip_prefix(root)
        .map_err(|_| identity("alias outside sysroot"))?;
    let mut parent = root.to_owned();
    let mut parts = rel.components().peekable();
    let mut depth = 0;
    while let Some(c) = parts.next() {
        depth += 1;
        if depth > 128 {
            return Err(limit("library path depth"));
        }
        if parts.peek().is_none() {
            break;
        }
        parent.push(c.as_os_str());
        if !regular_meta(&parent)?.is_dir() {
            return Err(identity("library parent is not a real directory"));
        }
    }
    Ok(())
}
fn runtime_files(
    sysroot: &Path,
    limits: &Limits,
    end: Instant,
) -> Result<Vec<CompilerRuntimeFile>, Error> {
    let mut out = Vec::new();
    let mut total = 0;
    for (p, bin) in [("lib/lean", false), ("bin", true)] {
        let dir = sysroot.join(p);
        real_parents(sysroot, &dir)?;
        if !regular_meta(&dir)?.is_dir() {
            return Err(Error::new(
                ErrorKind::UnsupportedCandidate,
                "symlinked compiler library directory",
            ));
        }
        let mut entries = 0;
        for e in fs::read_dir(dir)? {
            timely(end)?;
            entries += 1;
            if entries > limits.artifact_files {
                return Err(limit("compiler directory entries"));
            }
            let path = e?.path();
            let filename = path
                .file_name()
                .and_then(|s| s.to_str())
                .ok_or_else(|| identity("library filename"))?;
            if !runtime_name(filename, bin) {
                continue;
            }
            if out.len() >= limits.compiler_runtime_files {
                return Err(limit("compiler library count"));
            }
            let mut current = path.clone();
            let mut links = Vec::new();
            let mut seen = BTreeSet::new();
            loop {
                real_parents(sysroot, &current)?;
                let m = fs::symlink_metadata(&current)?;
                if !m.file_type().is_symlink() && !is_reparse(&m) {
                    break;
                }
                timely(end)?;
                if links.len() >= 16 || !seen.insert(current.clone()) {
                    return Err(identity("compiler alias depth/cycle"));
                }
                let target = fs::read_link(&current)?;
                reserve(&mut links, 1)?;
                links.push(LibraryLink {
                    path: label(sysroot, &current)?,
                    target: text(&target)?,
                });
                current = normalize(&if target.is_absolute() {
                    target
                } else {
                    current
                        .parent()
                        .ok_or_else(|| identity("alias parent"))?
                        .join(target)
                })?;
                if !current.starts_with(sysroot) {
                    return Err(identity("compiler alias leaves sysroot"));
                }
            }
            current = fs::canonicalize(current)?;
            if !current.starts_with(sysroot) {
                return Err(identity("compiler alias target leaves sysroot"));
            }
            let f = file_digest(
                sysroot,
                &current,
                limits.compiler_runtime_bytes.saturating_sub(total),
                end,
            )?;
            total = checked(total, f.bytes)?;
            reserve(&mut out, 1)?;
            out.push(CompilerRuntimeFile {
                path: label(sysroot, &path)?,
                canonical_target: f.path,
                links,
                bytes: f.bytes,
                blake3: f.blake3,
            });
        }
    }
    if out.is_empty() {
        return Err(identity("empty compiler runtime envelope"));
    }
    out.sort_unstable_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}
fn runtime_hash(files: &[CompilerRuntimeFile]) -> Digest {
    let mut h = blake3::Hasher::new();
    h.update(b"vesper.lean4lean42.compiler_runtime\0");
    h.update(&1u16.to_le_bytes());
    h.update(&(files.len() as u32).to_le_bytes());
    for f in files {
        blob(&mut h, &f.path);
        blob(&mut h, &f.canonical_target);
        h.update(&[f.links.len() as u8]);
        for l in &f.links {
            blob(&mut h, &l.path);
            blob(&mut h, &l.target)
        }
        h.update(&f.bytes.to_le_bytes());
        h.update(&f.blake3);
    }
    *h.finalize().as_bytes()
}

pub fn resolve_invocation(path: &Path) -> Result<PathBuf, Error> {
    let bare = !path.is_absolute() && path.components().count() == 1;
    let mut paths = Vec::new();
    if bare {
        for dir in std::env::split_paths(
            &std::env::var_os("PATH").ok_or_else(|| identity("PATH unavailable"))?,
        ) {
            paths.push(dir.join(path));
        }
    } else {
        paths.push(path.to_path_buf())
    }
    for p in paths {
        let variants = if cfg!(windows) && p.extension().is_none() {
            vec![p.with_extension("exe"), p]
        } else {
            vec![p]
        };
        for p in variants {
            if p.is_file() {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if fs::metadata(&p)?.permissions().mode() & 0o111 == 0 {
                        continue;
                    }
                }
                return if p.is_absolute() {
                    normalized_invocation(&p)
                } else {
                    normalized_invocation(&std::env::current_dir()?.join(p))
                };
            }
        }
    }
    Err(identity("executable not found"))
}
fn normalized_invocation(path: &Path) -> Result<PathBuf, Error> {
    let name = path
        .file_name()
        .ok_or_else(|| identity("executable basename"))?;
    let parent = path.parent().ok_or_else(|| identity("executable parent"))?;
    Ok(fs::canonicalize(parent)?.join(name))
}
fn same_invocation(left: &Path, right: &Path) -> Result<bool, Error> {
    let left = resolve_invocation(left)?;
    let right = resolve_invocation(right)?;
    let names = if cfg!(windows) {
        left.file_name()
            .and_then(|x| x.to_str())
            .zip(right.file_name().and_then(|x| x.to_str()))
            .is_some_and(|(a, b)| a.eq_ignore_ascii_case(b))
    } else {
        left.file_name() == right.file_name()
    };
    Ok(names
        && left.parent() == right.parent()
        && fs::canonicalize(left)? == fs::canonicalize(right)?)
}
fn build_recipe(argv: &[String], lake: &Path) -> Result<bool, Error> {
    Ok(argv.len() == 3
        && argv[1] == "build"
        && argv[2] == "lean4lean"
        && same_invocation(Path::new(&argv[0]), lake)?)
}
fn command(
    program: &Path,
    cwd: &Path,
    sysroot: Option<&Path>,
    roots: &[PathBuf],
) -> Result<Command, Error> {
    let mut c = Command::new(program);
    c.current_dir(cwd).env_clear();
    for key in [
        "SYSTEMROOT",
        "WINDIR",
        "TEMP",
        "TMP",
        "TMPDIR",
        "HOME",
        "USERPROFILE",
        "ELAN_HOME",
    ] {
        if let Some(v) = std::env::var_os(key) {
            c.env(key, v);
        }
    }
    let mut paths = Vec::new();
    if let Some(s) = sysroot {
        c.env("LEAN_SYSROOT", s);
        paths.push(s.join("bin"));
    }
    #[cfg(windows)]
    {
        if let Some(w) = std::env::var_os("SYSTEMROOT") {
            paths.push(PathBuf::from(&w).join("System32"));
            paths.push(PathBuf::from(w));
        }
    }
    if !paths.is_empty() {
        c.env(
            "PATH",
            std::env::join_paths(paths).map_err(|_| identity("controlled PATH"))?,
        );
    } else if let Some(p) = std::env::var_os("PATH") {
        c.env("PATH", p);
    }
    c.env(
        "LEAN_PATH",
        std::env::join_paths(roots).map_err(|_| identity("controlled LEAN_PATH"))?,
    );
    Ok(c)
}
fn observed(
    report: &mut Report,
    index: Option<usize>,
    phase: u8,
    o: &process::ProcessObservation,
) -> Result<(), Error> {
    report.process_spawns = checked(report.process_spawns, 1)?;
    if let Some(index) = index {
        let t = &mut report.targets[index];
        t.stdout_bytes = o.stdout_bytes;
        t.stderr_bytes = o.stderr_bytes;
        t.stdout_blake3 = Some(o.stdout_blake3);
        t.stderr_blake3 = Some(o.stderr_blake3);
        match phase {
            0 => {
                t.native_compile_exit = o.exit;
                t.compile_ms = o.elapsed_ms
            }
            1 => t.inventory_ms = o.elapsed_ms,
            _ => {
                t.external_exit = o.exit;
                t.replay_ms = o.elapsed_ms
            }
        }
    }
    Ok(())
}
fn probe(
    program: &Path,
    arg: &str,
    cwd: &Path,
    limits: &Limits,
    report: Option<&mut Report>,
) -> Result<String, Error> {
    let mut c = command(program, cwd, None, &[])?;
    c.arg(arg);
    let mut observer_error = None;
    let out = process::capture_observed(c, limits.inventory_timeout_ms, limits, 4096, |o| {
        if let Some(r) = report
            && let Err(e) = observed(r, None, 0, o)
        {
            observer_error = Some(e)
        }
    })?;
    if let Some(e) = observer_error {
        return Err(e);
    }
    if out.process.exit != 0 {
        return Err(identity("tool discovery failed"));
    }
    String::from_utf8(out.stdout)
        .map(|s| s.trim().to_owned())
        .map_err(|_| identity("tool discovery UTF8"))
}
fn compiler(
    launcher: &Path,
    cwd: &Path,
    limits: &Limits,
    mut report: Option<&mut Report>,
) -> Result<(PathBuf, PathBuf), Error> {
    let launcher = resolve_invocation(launcher)?;
    let v = probe(&launcher, "--version", cwd, limits, report.as_deref_mut())?;
    if !v.contains("version 4.24.0") || !v.contains(&LEAN_COMMIT[..12]) {
        return Err(identity("required Lean4.24 compiler commit"));
    }
    let root = fs::canonicalize(probe(
        &launcher,
        "--print-prefix",
        cwd,
        limits,
        report.as_deref_mut(),
    )?)?;
    regular_meta(&root)?;
    let lean = root
        .join("bin")
        .join(if cfg!(windows) { "lean.exe" } else { "lean" });
    let lake = root
        .join("bin")
        .join(if cfg!(windows) { "lake.exe" } else { "lake" });
    let actual = probe(&lean, "--version", cwd, limits, report.as_deref_mut())?;
    if actual != v
        || fs::canonicalize(probe(
            &lean,
            "--print-prefix",
            cwd,
            limits,
            report.as_deref_mut(),
        )?)? != root
    {
        return Err(identity("compiler prefix/commit substitution"));
    }
    let lv = probe(&lake, "--version", cwd, limits, report)?;
    if lv
        != format!(
            "Lake version 5.0.0-src+{} (Lean version 4.24.0)",
            &LEAN_COMMIT[..7]
        )
    {
        return Err(identity("Lake does not match selected compiler"));
    }
    Ok((lean, root))
}

fn fresh_token() -> Result<String, Error> {
    let mut n = NEXT.load(Ordering::Relaxed);
    loop {
        let next = n
            .checked_add(1)
            .ok_or_else(|| limit("session counter exhausted"))?;
        match NEXT.compare_exchange_weak(n, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => break,
            Err(actual) => n = actual,
        }
    }
    let mut h = blake3::Hasher::new();
    h.update(b"vesper.lean4lean42.session\0");
    h.update(&std::process::id().to_le_bytes());
    h.update(&n.to_le_bytes());
    h.update(
        &SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| identity("system clock"))?
            .as_nanos()
            .to_le_bytes(),
    );
    Ok(h.finalize().as_bytes()[..16]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}
pub(super) struct Workspace {
    pub(super) path: PathBuf,
    retained: bool,
}
impl Workspace {
    fn new(base: Option<&Path>) -> Result<Self, Error> {
        let parent = base
            .map(Path::to_path_buf)
            .unwrap_or_else(std::env::temp_dir);
        fs::create_dir_all(&parent)?;
        for _ in 0..16 {
            let path = parent.join(format!("vesper-lean4lean42-{}", fresh_token()?));
            match fs::create_dir(&path) {
                Ok(()) => {
                    return Ok(Self {
                        path: fs::canonicalize(path)?,
                        retained: base.is_some(),
                    });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e.into()),
            }
        }
        Err(identity("private directory collision"))
    }
}
impl Drop for Workspace {
    fn drop(&mut self) {
        if !self.retained {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}
fn tool_path(root: &Path, pins: &Pins) -> PathBuf {
    let p = root.join(&pins.checker_relative_path);
    if cfg!(windows) {
        p.with_extension("exe")
    } else {
        p
    }
}
fn roots(root: &Path, models: &Path, sysroot: &Path, controls: bool) -> Vec<PathBuf> {
    let mut v = vec![models.to_owned(), sysroot.join("lib/lean")];
    if controls {
        v.push(root.join(".lake/build/lib/lean"));
        v.push(root.join(".lake/packages/batteries/.lake/build/lib/lean"));
    }
    v
}
struct MachineContext<'a> {
    root: &'a Path,
    models: &'a Path,
    sysroot: &'a Path,
    inputs: &'a Inputs,
    limits: &'a Limits,
    reserved_bytes: usize,
}
impl MachineContext<'_> {
    fn run(
        &self,
        mode: u8,
        target: u8,
        expected: Option<&Inventory>,
        mut report: Option<&mut Report>,
        target_index: Option<usize>,
    ) -> Result<(SessionResult, process::ProcessResult), Error> {
        let Self {
            root,
            models,
            sysroot,
            inputs: i,
            limits,
            reserved_bytes,
        } = *self;
        let token = fresh_token()?;
        let mut parser_limits = limits.clone();
        parser_limits.inventory_bytes = parser_limits
            .inventory_bytes
            .checked_sub(process::transport_bytes(limits)?)
            .and_then(|n| n.checked_sub(reserved_bytes))
            .ok_or_else(|| limit("transport, descriptor and inventory reservation"))?;
        let mut m = Machine::new(mode, target, &token, &parser_limits, &i.controls, expected)?;
        let mut c = command(
            &tool_path(root, &i.pins),
            models,
            Some(sysroot),
            &roots(root, models, sysroot, target == 2),
        )?;
        c.args(["--vesper42", &mode.to_string(), &target.to_string(), &token]);
        let timeout = if mode == 1 {
            limits.replay_timeout_ms
        } else {
            limits.inventory_timeout_ms
        };
        let mut observer_error = None;
        let observation = process::framed_observed(
            c,
            timeout,
            limits,
            |bytes, _| m.feed(bytes),
            |o| {
                if let Some(r) = report.as_deref_mut()
                    && let Err(e) = observed(r, target_index, if mode == 1 { 2 } else { 1 }, o)
                {
                    observer_error = Some(e)
                }
            },
        );
        if let Some(index) = target_index
            && let Some(r) = report
        {
            r.targets[index].coverage = m.progress();
        }
        let observation = observation?;
        if let Some(e) = observer_error {
            return Err(e);
        }
        if observation.exit != 0 {
            return Err(Error::new(
                ErrorKind::ExternalRejection,
                "external checker did not exit successfully",
            ));
        }
        Ok((m.finish()?, observation))
    }
}
struct ImportEnvelope {
    hash: Digest,
    files: u64,
    bytes: u64,
}
fn import_envelope(
    inv: &Inventory,
    root: &Path,
    models: &Path,
    sysroot: &Path,
    controls: bool,
    limits: &Limits,
) -> Result<ImportEnvelope, Error> {
    let end = until(limits.inventory_timeout_ms);
    let mut h = blake3::Hasher::new();
    h.update(b"vesper.lean4lean42.imports\0");
    h.update(&1u16.to_le_bytes());
    h.update(&[inv.target]);
    h.update(&(inv.modules.len() as u32).to_le_bytes());
    let mut files = 0u64;
    let mut bytes = 0u64;
    for module in &inv.modules {
        timely(end)?;
        let parts = module
            .name
            .strings()
            .ok_or_else(|| identity("numeric module component"))?;
        if parts.is_empty()
            || parts
                .iter()
                .any(|p| p.is_empty() || p.contains(['/', '\\']) || *p == "." || *p == "..")
        {
            return Err(identity("module path component"));
        }
        let first = parts[0];
        let base = if module.name.is(&["RewriteWitness"])
            || module.name.is(&["ArithmeticOracle"])
            || (inv.target == 3 && module.name.is(&[resource::MODULE]))
        {
            models.to_owned()
        } else if ["Init", "Std", "Lean", "Lake"].contains(&first) {
            sysroot.join("lib/lean")
        } else if controls && first == "Lean4Lean" {
            root.join(".lake/build/lib/lean")
        } else if controls && first == "Batteries" {
            root.join(".lake/packages/batteries/.lake/build/lib/lean")
        } else {
            return Err(identity("unexpected module routing root"));
        };
        let expected = parts
            .iter()
            .fold(base.clone(), |p, c| p.join(c))
            .with_extension("olean");
        let actual = PathBuf::from(&module.path);
        if !actual.is_absolute()
            || fs::canonicalize(&actual)? != fs::canonicalize(&expected)?
            || !fs::canonicalize(&expected)?.starts_with(fs::canonicalize(&base)?)
        {
            return Err(identity("module artifact substitution"));
        }
        let server = expected.with_extension("olean.server");
        let private = expected.with_extension("olean.private");
        let ir = expected.with_extension("ir");
        if present(&private)? && !present(&server)? {
            return Err(identity("private olean without server precursor"));
        }
        let mut selected = vec![(0u8, expected)];
        if present(&server)? {
            selected.push((1, server));
            if present(&private)? {
                selected.push((2, private));
            }
        }
        if present(&ir)? {
            selected.push((3, ir));
        }
        h.update(&module.id.to_le_bytes());
        module.name.hash_into(&mut h);
        h.update(&[selected.len() as u8]);
        for (tag, path) in selected {
            if files >= limits.artifact_files as u64 {
                return Err(limit("import artifact count"));
            }
            let f = file_digest(
                &base,
                &path,
                limits.artifact_bytes.saturating_sub(bytes),
                end,
            )?;
            files = checked(files, 1)?;
            bytes = checked(bytes, f.bytes)?;
            h.update(&[tag]);
            h.update(&f.bytes.to_le_bytes());
            h.update(&f.blake3);
        }
    }
    Ok(ImportEnvelope {
        hash: *h.finalize().as_bytes(),
        files,
        bytes,
    })
}
fn checker_digest(root: &Path, pins: &Pins, end: Instant) -> Result<Digest, Error> {
    Ok(file_digest(root, &tool_path(root, pins), 1024 * MIB as u64, end)?.blake3)
}
fn compiler_digest(lean: &Path, sysroot: &Path, end: Instant) -> Result<Digest, Error> {
    let actual = fs::canonicalize(lean)?;
    if !actual.starts_with(sysroot) {
        return Err(identity("compiler binary outside sysroot"));
    }
    Ok(file_digest(sysroot, &actual, 128 * MIB as u64, end)?.blake3)
}
fn compare_sources(root: &Path, i: &Inputs, end: Instant) -> Result<(), Error> {
    if sources(root, end)? != i.sources.files {
        return Err(identity("source/dependency envelope changed"));
    }
    Ok(())
}
fn validate_receipt(
    root: &Path,
    r: &Receipt,
    i: &Inputs,
    limits: &Limits,
) -> Result<(PathBuf, PathBuf), Error> {
    if r.schema != 2
        || r.profile != i.pins.profile
        || r.upstream_commit != UPSTREAM
        || r.batteries_commit != BATTERIES
        || r.lean_release != i.pins.lean_release
        || r.lean_commit != LEAN_COMMIT
        || r.resource_bundle_blake3 != i.pins.resource_bundle_blake3
        || r.pins_blake3 != digest(PINS.as_bytes())
        || r.known_fixes_blake3 != i.pins.known_fixes_blake3
        || r.patches != i.pins.patches
        || r.source_manifest != i.sources.files
        || r.source_manifest_blake3 != i.pins.source_manifest_blake3
        || r.checker_relative_path != i.pins.checker_relative_path
        || r.build_exit != 0
    {
        return Err(identity("receipt does not match embedded candidate"));
    }
    let sysroot = fs::canonicalize(&r.sysroot)?;
    if text(&sysroot)? != r.sysroot {
        return Err(identity("noncanonical receipt sysroot"));
    }
    let lean = sysroot
        .join("bin")
        .join(if cfg!(windows) { "lean.exe" } else { "lean" });
    let lake = sysroot
        .join("bin")
        .join(if cfg!(windows) { "lake.exe" } else { "lake" });
    if !same_invocation(Path::new(&r.compiler_path), &lean)? || !build_recipe(&r.build_argv, &lake)?
    {
        return Err(identity("receipt actual compiler/build recipe"));
    }
    let end = until(limits.inventory_timeout_ms);
    compare_sources(root, i, end)?;
    if checker_digest(root, &i.pins, end)? != r.checker_blake3
        || compiler_digest(&lean, &sysroot, end)? != r.compiler_blake3
    {
        return Err(identity("compiler/checker bytes changed"));
    }
    let runtime = runtime_files(&sysroot, limits, end)?;
    if runtime != r.compiler_runtime_manifest
        || runtime_hash(&runtime) != r.compiler_runtime_manifest_blake3
    {
        return Err(identity("compiler library envelope changed"));
    }
    let outputs = build_outputs(root, limits, end)?;
    if outputs != r.build_output_manifest
        || manifest_hash(b"vesper.lean4lean42.build_outputs\0", &outputs)
            != r.build_output_manifest_blake3
    {
        return Err(identity("generated build envelope changed"));
    }
    if digest(&read_bytes(&root.join("build.stdout"), 65536)?) != r.build_stdout_blake3
        || digest(&read_bytes(&root.join("build.stderr"), 65536)?) != r.build_stderr_blake3
    {
        return Err(identity("build log association changed"));
    }
    if protocol::controls_hash(&r.controls) != r.controls_blake3 {
        return Err(identity("receipt control digest"));
    }
    Ok((lean, sysroot))
}

pub fn seal(root: &Path) -> Result<Receipt, Error> {
    let limits = Limits::default();
    limits.validate()?;
    let root = fs::canonicalize(root)?;
    if root.join("setup-receipt.json").exists() {
        return Err(identity("setup already has a receipt"));
    }
    let i = inputs()?;
    compare_sources(&root, &i, until(limits.inventory_timeout_ms))?;
    let setup: SetupInputs =
        bounded_json(&read_bytes(&root.join("setup-inputs.json"), 16384)?, 16384)?;
    if setup.schema != 1
        || setup.build_exit != 0
        || setup.build_stdout != "build.stdout"
        || setup.build_stderr != "build.stderr"
        || !Path::new(&setup.lean).is_absolute()
        || !Path::new(&setup.lake).is_absolute()
        || !Path::new(&setup.git).is_absolute()
        || setup.build_argv.len() > 32
        || setup.build_argv.iter().map(String::len).sum::<usize>() > 8192
    {
        return Err(identity(
            "setup inputs are not an observed fixed successful build",
        ));
    }
    let (lean, sysroot) = compiler(Path::new(&setup.lean), &root, &limits, None)?;
    let lake = sysroot
        .join("bin")
        .join(if cfg!(windows) { "lake.exe" } else { "lake" });
    if !same_invocation(Path::new(&setup.lake), &lake)? || !build_recipe(&setup.build_argv, &lake)?
    {
        return Err(identity("setup Lake/build recipe"));
    }
    let end = until(limits.inventory_timeout_ms);
    let runtime = runtime_files(&sysroot, &limits, end)?;
    let outputs = build_outputs(&root, &limits, end)?;
    let initial_compiler = compiler_digest(&lean, &sysroot, end)?;
    let initial_checker = checker_digest(&root, &i.pins, end)?;
    let models = Workspace::new(None)?;
    let context = MachineContext {
        root: &root,
        models: &models.path,
        sysroot: &sysroot,
        inputs: &i,
        limits: &limits,
        reserved_bytes: 0,
    };
    context.run(3, 2, None, None, None)?;
    let (baseline, _) = context.run(0, 2, None, None, None)?;
    let baseline = baseline
        .inventory
        .ok_or_else(|| identity("missing control inventory"))?;
    let imports = import_envelope(&baseline, &root, &models.path, &sysroot, true, &limits)?;
    let (controls, _) = context.run(2, 2, None, None, None)?;
    let (repeated, _) = context.run(0, 2, Some(&baseline), None, None)?;
    if repeated.inventory_blake3 != Some(baseline.blake3) {
        return Err(identity("control inventory changed during controls"));
    }
    let after = import_envelope(&baseline, &root, &models.path, &sysroot, true, &limits)?;
    if imports.hash != after.hash {
        return Err(identity("control baseline changed during controls"));
    }
    let receipt = Receipt {
        schema: 2,
        profile: i.pins.profile.clone(),
        upstream_commit: UPSTREAM.into(),
        batteries_commit: BATTERIES.into(),
        lean_release: i.pins.lean_release.clone(),
        lean_commit: LEAN_COMMIT.into(),
        resource_bundle_blake3: i.pins.resource_bundle_blake3,
        pins_blake3: digest(PINS.as_bytes()),
        known_fixes_blake3: i.pins.known_fixes_blake3,
        patches: i.pins.patches.clone(),
        source_manifest: i.sources.files.clone(),
        source_manifest_blake3: i.pins.source_manifest_blake3,
        compiler_path: text(&lean)?,
        sysroot: text(&sysroot)?,
        compiler_blake3: initial_compiler,
        compiler_runtime_manifest_blake3: runtime_hash(&runtime),
        compiler_runtime_manifest: runtime,
        checker_relative_path: i.pins.checker_relative_path.clone(),
        checker_blake3: initial_checker,
        driver_blake3: digest_driver()?,
        build_argv: setup.build_argv,
        build_exit: 0,
        build_stdout_blake3: digest(&read_bytes(&root.join("build.stdout"), 65536)?),
        build_stderr_blake3: digest(&read_bytes(&root.join("build.stderr"), 65536)?),
        controls_blake3: controls
            .controls_blake3
            .ok_or_else(|| identity("missing controls digest"))?,
        controls: controls.controls,
        control_inventory_blake3: baseline.blake3,
        control_import_manifest_blake3: imports.hash,
        build_output_manifest_blake3: manifest_hash(
            b"vesper.lean4lean42.build_outputs\0",
            &outputs,
        ),
        build_output_manifest: outputs,
    };
    validate_receipt(&root, &receipt, &i, &limits)?;
    let bytes = serde_json::to_vec(&receipt).map_err(|_| identity("receipt serialization"))?;
    if bytes.len() > 4 * MIB {
        return Err(limit("receipt byte cap"));
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(root.join("setup-receipt.json"))?;
    if let Err(e) = file.write_all(&bytes).and_then(|()| file.sync_all()) {
        drop(file);
        let _ = fs::remove_file(root.join("setup-receipt.json"));
        return Err(e.into());
    }
    Ok(receipt)
}
fn digest_driver() -> Result<Digest, Error> {
    let path = fs::canonicalize(std::env::current_exe()?)?;
    let parent = path
        .parent()
        .ok_or_else(|| identity("driver executable parent"))?;
    Ok(file_digest(
        parent,
        &path,
        1024 * MIB as u64,
        until(Limits::default().inventory_timeout_ms),
    )?
    .blake3)
}

fn create_file(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    let mut f = OpenOptions::new().create_new(true).write(true).open(path)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    Ok(())
}
fn copy_evidence(from: &Path, to: &Path, expected: &FileDigest, end: Instant) -> Result<(), Error> {
    if !regular_meta(from)?.is_file() || fs::metadata(from)?.len() != expected.bytes {
        return Err(Error::new(
            ErrorKind::Artifact,
            "evidence source changed after preflight",
        ));
    }
    let mut input = File::open(from)?;
    let mut output = OpenOptions::new().create_new(true).write(true).open(to)?;
    let mut hash = blake3::Hasher::new();
    let mut bytes = 0;
    let mut buffer = [0u8; 65536];
    loop {
        timely(end)?;
        let n = input.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        bytes = checked(bytes, n as u64)?;
        if bytes > expected.bytes {
            return Err(Error::new(
                ErrorKind::Artifact,
                "evidence source grew after preflight",
            ));
        }
        output.write_all(&buffer[..n])?;
        hash.update(&buffer[..n]);
    }
    if bytes != expected.bytes || hash.finalize().as_bytes() != &expected.blake3 {
        return Err(Error::new(
            ErrorKind::Artifact,
            "evidence source changed after preflight",
        ));
    }
    if !regular_meta(from)?.is_file() || fs::metadata(from)?.len() != expected.bytes {
        return Err(Error::new(
            ErrorKind::Artifact,
            "evidence source changed while copying",
        ));
    }
    output.sync_all()?;
    Ok(())
}
fn publish_evidence(
    config: &Config,
    models: &Path,
    report: &mut Report,
) -> Result<Option<PathBuf>, Error> {
    let Some(parent) = &config.artifacts else {
        return Ok(None);
    };
    let mut names = BTreeSet::new();
    for t in &report.targets {
        if let Some(f) = &t.source {
            names.insert(f.path.clone());
        }
        for f in &t.model_artifacts {
            names.insert(f.path.clone());
        }
    }
    for name in ["setup-receipt.json", "controls.json"] {
        if present(&models.join(name))? {
            names.insert(name.into());
        }
    }
    if present(&models.join("resource"))? {
        for file in resource_files(&models.join("resource"), &config.limits)? {
            names.insert(format!("resource/{}", file.path));
        }
    }
    if names.len() + 1 > config.limits.artifact_files {
        return Err(Error::new(
            ErrorKind::Artifact,
            "retained evidence file count",
        ));
    }
    let end = until(config.limits.inventory_timeout_ms);
    let mut files = Vec::new();
    let mut bytes = config.limits.report_bytes as u64;
    // Compiler output is always private. Account the entire actual publication
    // and reserve the maximum report bytes BEFORE creating a retained directory.
    for name in names {
        relative(&name)?;
        let f = file_digest(
            models,
            &models.join(name),
            config.limits.artifact_bytes.saturating_sub(bytes),
            end,
        )?;
        bytes = checked(bytes, f.bytes)?;
        if bytes > config.limits.artifact_bytes {
            return Err(Error::new(
                ErrorKind::Artifact,
                "retained evidence byte cap",
            ));
        }
        reserve(&mut files, 1)?;
        files.push(f);
    }
    if bytes > config.limits.artifact_bytes {
        return Err(Error::new(
            ErrorKind::Artifact,
            "retained report reservation",
        ));
    }
    let mut publication = Workspace::new(Some(parent))?;
    publication.retained = false;
    let base = fs::canonicalize(parent)?;
    let mut published = Vec::new();
    reserve(&mut published, files.len())?;
    for mut f in files {
        if let Some(parent) = publication.path.join(&f.path).parent() {
            fs::create_dir_all(parent)?;
        }
        copy_evidence(
            &models.join(&f.path),
            &publication.path.join(&f.path),
            &f,
            end,
        )?;
        f.path = label(&base, &publication.path.join(&f.path))?;
        published.push(f);
    }
    report.artifacts = published;
    publication.retained = true;
    Ok(Some(publication.path.clone()))
}
pub fn save_report(config: &Config, dir: Option<&Path>, report: &Report) -> Result<(), Error> {
    if let Some(dir) = dir {
        let bytes = serde_json::to_vec(report)
            .map_err(|_| Error::new(ErrorKind::Artifact, "report serialization"))?;
        let used = report
            .artifacts
            .iter()
            .try_fold(0u64, |n, f| checked(n, f.bytes))?;
        if bytes.len() > config.limits.report_bytes
            || checked(used, bytes.len() as u64)? > config.limits.artifact_bytes
        {
            return Err(Error::new(ErrorKind::Artifact, "retained report byte cap"));
        }
        create_file(&dir.join("report.json"), &bytes)?;
    }
    Ok(())
}
fn target_files(models: &Path, target: Target, limits: &Limits) -> Result<Vec<FileDigest>, Error> {
    let mut out = Vec::new();
    let base = models.join(format!("{}.olean", target.module()));
    let server = base.with_extension("olean.server");
    let private = base.with_extension("olean.private");
    if present(&private)? && !present(&server)? {
        return Err(identity("model private artifact without server"));
    }
    for p in [
        base,
        server,
        private,
        models.join(format!("{}.ir", target.module())),
    ] {
        if present(&p)? {
            reserve(&mut out, 1)?;
            out.push(file_digest(
                models,
                &p,
                64 * MIB as u64,
                until(limits.inventory_timeout_ms),
            )?)
        }
    }
    if out.is_empty() {
        return Err(Error::new(
            ErrorKind::Compile,
            "compiler produced no model artifact",
        ));
    }
    Ok(out)
}
pub fn execute(
    config: &Config,
    report: &mut Report,
    artifact_directory: &mut Option<PathBuf>,
) -> Result<(), Error> {
    regular_meta(&config.tool_root)?;
    let root = fs::canonicalize(&config.tool_root)?;
    let i = inputs()?;
    let receipt_bytes = read_bytes(&root.join("setup-receipt.json"), 4 * MIB)?;
    let r: Receipt = bounded_json(&receipt_bytes, 4 * MIB)?;
    let (lean, sysroot) = validate_receipt(&root, &r, &i, &config.limits)?;
    // Offline overrides must already name the bound actual compiler. In
    // particular, never execute an elan proxy to discover a replacement here.
    if !same_invocation(&config.lean, &lean)? {
        return Err(identity(
            "offline compiler override is not the sealed actual compiler",
        ));
    }
    if let Some(parent) = &config.artifacts {
        if config.limits.artifact_bytes < config.limits.report_bytes as u64 {
            return Err(Error::new(
                ErrorKind::Artifact,
                "artifact budget cannot reserve the bounded final report",
            ));
        }
        fs::create_dir_all(parent)?;
        regular_meta(parent)?;
        let parent = fs::canonicalize(parent)?;
        if parent.starts_with(&root) || parent.starts_with(&sysroot) {
            return Err(Error::new(
                ErrorKind::InvalidConfig,
                "artifacts must be outside immutable tool/compiler trees",
            ));
        }
    }
    let models = Workspace::new(None)?;
    let context = MachineContext {
        root: &root,
        models: &models.path,
        sysroot: &sysroot,
        inputs: &i,
        limits: &config.limits,
        reserved_bytes: 0,
    };
    let mut resource_publication_files = None;
    let result = (|| -> Result<(), Error> {
        let (actual, actual_root) = compiler(&lean, &models.path, &config.limits, Some(report))?;
        if actual != lean || actual_root != sysroot {
            return Err(identity("offline compiler prefix mismatch"));
        }
        report.tools = Some(ToolInfo {
            profile: r.profile.clone(),
            upstream_commit: r.upstream_commit.clone(),
            batteries_commit: r.batteries_commit.clone(),
            lean_release: r.lean_release.clone(),
            lean_commit: r.lean_commit.clone(),
            checker_blake3: r.checker_blake3,
            compiler_blake3: r.compiler_blake3,
            compiler_runtime_manifest_blake3: r.compiler_runtime_manifest_blake3,
            resource_bundle_blake3: r.resource_bundle_blake3,
            receipt_blake3: digest(&receipt_bytes),
            pins_blake3: r.pins_blake3,
            known_fixes_blake3: r.known_fixes_blake3,
            source_manifest_blake3: r.source_manifest_blake3,
            build_output_manifest_blake3: r.build_output_manifest_blake3,
            control_inventory_blake3: r.control_inventory_blake3,
            control_import_manifest_blake3: r.control_import_manifest_blake3,
            os: std::env::consts::OS.into(),
            arch: std::env::consts::ARCH.into(),
        });
        context.run(3, 2, None, Some(report), None)?;
        {
            let (before, _) = context.run(0, 2, None, Some(report), None)?;
            let before = before
                .inventory
                .ok_or_else(|| identity("missing native control baseline"))?;
            let envelope =
                import_envelope(&before, &root, &models.path, &sysroot, true, &config.limits)?;
            if before.blake3 != r.control_inventory_blake3
                || envelope.hash != r.control_import_manifest_blake3
            {
                return Err(identity("control baseline does not match sealed inputs"));
            }
            let (controls, _) = context.run(2, 2, None, Some(report), None)?;
            report.controls = controls.controls;
            report.controls_blake3 = controls.controls_blake3;
            if report.controls != r.controls || report.controls_blake3 != Some(r.controls_blake3) {
                return Err(Error::new(
                    ErrorKind::Controls,
                    "actual controls differ from sealed candidate",
                ));
            }
            let (after, _) = context.run(0, 2, Some(&before), Some(report), None)?;
            if after.inventory_blake3 != Some(before.blake3)
                || import_envelope(&before, &root, &models.path, &sysroot, true, &config.limits)?
                    .hash
                    != envelope.hash
            {
                return Err(identity("control baseline changed during checks"));
            }
        }
        for (index, (target, bytes)) in [
            (Target::RewriteWitness, MODEL0),
            (Target::ArithmeticOracle, MODEL1),
        ]
        .into_iter()
        .enumerate()
        {
            let source_name = format!("{}.lean", target.module());
            let source_path = models.path.join(&source_name);
            create_file(&source_path, bytes)?;
            let source = file_digest(
                &models.path,
                &source_path,
                4 * MIB as u64,
                until(config.limits.inventory_timeout_ms),
            )?;
            report.targets[index].source = Some(source);
            let mut c = command(
                &lean,
                &models.path,
                Some(&sysroot),
                &roots(&root, &models.path, &sysroot, false),
            )?;
            c.arg("-o")
                .arg(models.path.join(format!("{}.olean", target.module())))
                .arg(&source_path);
            let mut observation_error = None;
            let out = process::capture_observed(
                c,
                config.limits.compile_timeout_ms,
                &config.limits,
                65536,
                |o| {
                    if let Err(e) = observed(report, Some(index), 0, o) {
                        observation_error = Some(e)
                    }
                },
            );
            let out = match out {
                Ok(out) => out,
                Err(e) => {
                    report.targets[index].status = TargetStatus::Failed;
                    return Err(e);
                }
            };
            if let Some(e) = observation_error {
                return Err(e);
            }
            if out.process.exit != 0 {
                report.targets[index].status = TargetStatus::Failed;
                return Err(Error::new(
                    ErrorKind::Compile,
                    "bundled model failed native compilation",
                ));
            }
            report.targets[index].model_artifacts =
                target_files(&models.path, target, &config.limits)?;
            report.targets[index].status = TargetStatus::Compiled;
        }
        let prepared = prepare_resource_with(
            &root,
            &lean,
            &sysroot,
            &receipt_bytes,
            &config.limits,
            Some(report),
        )
        .inspect_err(|_| report.targets[2].status = TargetStatus::Failed)?;
        if config.artifacts.is_some() {
            let destination = models.path.join("resource");
            fs::create_dir(&destination)?;
            for file in prepared.files() {
                let to = destination.join(&file.path);
                if let Some(parent) = to.parent() {
                    fs::create_dir_all(parent)?;
                }
                copy_evidence(
                    &prepared.root().join(&file.path),
                    &to,
                    file,
                    until(config.limits.inventory_timeout_ms),
                )?;
            }
            resource_publication_files = Some(prepared.files().to_vec());
            prepared.revalidate(&config.limits)?;
        }
        report.targets[2].source = Some(file_digest(
            prepared.root(),
            &prepared.source_path(),
            4 * MIB as u64,
            until(config.limits.inventory_timeout_ms),
        )?);
        if let Some(source) = &mut report.targets[2].source {
            source.path = format!("resource/{}", source.path);
        }
        report.targets[2].resource = Some(prepared.association().clone());
        if let Some(association) = &mut report.targets[2].resource {
            for file in [
                &mut association.snapshot_tool,
                &mut association.request,
                &mut association.producer_report,
                &mut association.projection,
            ] {
                file.path = format!("resource/{}", file.path);
            }
        }
        report.targets[2].model_artifacts = vec![FileDigest {
            path: "resource/projection/ResourceModel42Kernel.olean".into(),
            ..prepared.association().projection.clone()
        }];
        report.targets[2].status = TargetStatus::Compiled;
        for index in 0..3 {
            let target = report.targets[index].target;
            let model_root = if index == 2 {
                prepared.projection_root()
            } else {
                models.path.clone()
            };
            // The descriptor remains live for all model sessions in this loop.
            let target_context = MachineContext {
                models: &model_root,
                reserved_bytes: prepared.retained_bytes()?,
                ..context
            };
            if index == 2 {
                prepared.revalidate(&config.limits)?;
            }
            let (native, _) =
                match target_context.run(0, target.tag(), None, Some(report), Some(index)) {
                    Ok(v) => v,
                    Err(e) => {
                        report.targets[index].status = TargetStatus::Failed;
                        return Err(e);
                    }
                };
            let native = native
                .inventory
                .ok_or_else(|| identity("missing native target inventory"))?;
            if index == 2 {
                prepared.validate_inventory(&native)?;
                prepared.revalidate(&config.limits)?;
            }

            let before =
                import_envelope(&native, &root, &model_root, &sysroot, false, &config.limits)?;
            report.targets[index].status = TargetStatus::Inventoried;
            report.targets[index].inventory = Some(native.totals.clone());
            report.targets[index].inventory_blake3 = Some(native.blake3);
            report.targets[index].import_files = before.files;
            report.targets[index].import_bytes = before.bytes;
            report.targets[index].import_manifest_blake3 = Some(before.hash);
            let (replayed, _) =
                match target_context.run(1, target.tag(), Some(&native), Some(report), Some(index))
                {
                    Ok(v) => v,
                    Err(e) => {
                        report.targets[index].status = TargetStatus::Failed;
                        return Err(e);
                    }
                };
            report.targets[index].coverage = replayed.coverage;
            report.targets[index].coverage_blake3 = replayed.coverage_blake3;
            if import_envelope(&native, &root, &model_root, &sysroot, false, &config.limits)?.hash
                != before.hash
            {
                return Err(identity(
                    "model import envelope changed during external replay",
                ));
            }
            if index == 2 {
                prepared.revalidate(&config.limits)?;
            }
            report.targets[index].status = TargetStatus::Complete;
        }
        validate_receipt(&root, &r, &i, &config.limits)?;
        if read_bytes(&root.join("setup-receipt.json"), 4 * MIB)? != receipt_bytes {
            return Err(identity("setup receipt bytes changed during run"));
        }
        for t in &report.targets[..2] {
            let name = format!("{}.lean", t.target.module());
            if Some(file_digest(
                &models.path,
                &models.path.join(name),
                4 * MIB as u64,
                until(config.limits.inventory_timeout_ms),
            )?) != t.source
                || target_files(&models.path, t.target, &config.limits)? != t.model_artifacts
            {
                return Err(identity(
                    "bundled model source/artifact changed during replay",
                ));
            }
        }
        Ok(())
    })();
    if let Some(expected) = resource_publication_files {
        if resource_files(&models.path.join("resource"), &config.limits)? != expected {
            return Err(identity(
                "private resource evidence changed before publication",
            ));
        }
    }
    if config.artifacts.is_some() {
        create_file(&models.path.join("setup-receipt.json"), &receipt_bytes)?;
        let controls = serde_json::to_vec(&report.controls)
            .map_err(|_| identity("controls evidence serialization"))?;
        if controls.len() > MIB {
            return Err(limit("control evidence bytes"));
        }
        create_file(&models.path.join("controls.json"), &controls)?;
    }
    *artifact_directory = publish_evidence(config, &models.path, report)?;
    result
}

/// Full finite private tree, including unknown entries; publication uses this same scan.
pub(super) fn resource_files(root: &Path, limits: &Limits) -> Result<Vec<FileDigest>, Error> {
    let mut scan = FileScan::new(
        root,
        limits.artifact_files.min(128),
        limits.artifact_bytes,
        until(limits.inventory_timeout_ms),
        false,
    );
    let mut dirs = BTreeSet::new();
    for base in ["sources", "originals"] {
        dirs.insert(root.join(base));
        for (module, _) in resource::SOURCES {
            let path = root.join(base).join(resource::module_path(module, "olean"));
            let mut parent = path.parent();
            while let Some(p) = parent {
                if p == root {
                    break;
                }
                dirs.insert(p.to_owned());
                parent = p.parent();
            }
        }
    }
    dirs.insert(root.join("projector"));
    dirs.insert(root.join("projection"));
    scan.allowed_dirs = Some(dirs);
    scan.walk(root, 0)?;
    Ok(scan.finish())
}

pub(super) fn validate_resource_tool(
    prepared: &resource::PreparedResource,
    limits: &Limits,
) -> Result<(), Error> {
    let i = inputs()?;
    let receipt: Receipt = bounded_json(prepared.receipt(), 4 * MIB)?;
    if read_bytes(&prepared.tool_root().join("setup-receipt.json"), 4 * MIB)? != prepared.receipt()
    {
        return Err(identity("resource preparation receipt changed"));
    }
    let (_, sysroot) = validate_receipt(prepared.tool_root(), &receipt, &i, limits)?;
    if sysroot != prepared.sysroot() {
        return Err(identity("resource compiler sysroot changed"));
    }
    Ok(())
}

/// Genuine preparation-only entry point for bounded binding refusals. No external result is manufactured.
pub fn prepare_resource(config: &Config) -> Result<resource::PreparedResource, Error> {
    config.limits.validate()?;
    regular_meta(&config.tool_root)?;
    let root = fs::canonicalize(&config.tool_root)?;
    let i = inputs()?;
    let bytes = read_bytes(&root.join("setup-receipt.json"), 4 * MIB)?;
    let receipt: Receipt = bounded_json(&bytes, 4 * MIB)?;
    let (lean, sysroot) = validate_receipt(&root, &receipt, &i, &config.limits)?;
    if !same_invocation(&config.lean, &lean)? {
        return Err(identity("preparation compiler override mismatch"));
    }
    prepare_resource_with(&root, &lean, &sysroot, &bytes, &config.limits, None)
}

/// Observe the actual native inventory of a genuine immutable preparation, without external replay.
pub fn resource_inventory(
    config: &Config,
    prepared: &resource::PreparedResource,
) -> Result<Inventory, Error> {
    if fs::canonicalize(&config.tool_root)? != prepared.tool_root() {
        return Err(identity("resource preparation tool root mismatch"));
    }
    prepared.revalidate(&config.limits)?;
    let i = inputs()?;
    let receipt: Receipt = bounded_json(prepared.receipt(), 4 * MIB)?;
    let (lean, sysroot) = validate_receipt(prepared.tool_root(), &receipt, &i, &config.limits)?;
    if !same_invocation(&config.lean, &lean)? {
        return Err(identity("resource inventory compiler override"));
    }
    let projection = prepared.projection_root();
    let context = MachineContext {
        root: prepared.tool_root(),
        models: &projection,
        sysroot: &sysroot,
        inputs: &i,
        limits: &config.limits,
        reserved_bytes: prepared.retained_bytes()?,
    };
    let (result, _) = context.run(0, 3, None, None, None)?;
    let inventory = result
        .inventory
        .ok_or_else(|| identity("missing actual resource inventory"))?;
    prepared.validate_inventory(&inventory)?;
    prepared.revalidate(&config.limits)?;
    Ok(inventory)
}

fn prepare_resource_with(
    root: &Path,
    lean: &Path,
    sysroot: &Path,
    receipt: &[u8],
    limits: &Limits,
    mut report: Option<&mut Report>,
) -> Result<resource::PreparedResource, Error> {
    resource::verify_bundle()?;
    let workspace = Workspace::new(None)?;
    let sources = workspace.path.join("sources");
    let originals = workspace.path.join("originals");
    let projector = workspace.path.join("projector");
    for path in [&sources, &originals, &projector] {
        fs::create_dir(path)?;
    }
    create_file(
        &workspace.path.join("resource-source-manifest.json"),
        resource::bundle_bytes(),
    )?;
    create_file(
        &workspace.path.join("historical-source-manifest.json"),
        resource::HISTORICAL,
    )?;
    create_file(&projector.join("KernelSnapshot.lean"), resource::TOOL)?;
    // All source bytes are embedded and immutable; every actual compiled object is subsequently bound.
    for (module, bytes) in resource::SOURCES {
        let source = sources.join(resource::module_path(module, "lean"));
        let output = originals.join(resource::module_path(module, "olean"));
        fs::create_dir_all(
            source
                .parent()
                .ok_or_else(|| identity("resource source parent"))?,
        )?;
        fs::create_dir_all(
            output
                .parent()
                .ok_or_else(|| identity("resource object parent"))?,
        )?;
        create_file(&source, bytes)?;
        let mut c = command(
            lean,
            &sources,
            Some(sysroot),
            &[originals.clone(), sysroot.join("lib/lean")],
        )?;
        c.env("LEAN_NUM_THREADS", "1")
            .env("LEAN_ABORT_ON_PANIC", "1")
            .args(["-j1", "-M4096", "-o"])
            .arg(&output)
            .arg(resource::module_path(module, "lean"));
        let mut observer_error = None;
        let result = process::capture_observed(c, limits.compile_timeout_ms, limits, 65536, |o| {
            if let Some(r) = report.as_deref_mut() {
                let previous = r.targets[2].compile_ms;
                if let Err(e) = observed(r, Some(2), 0, o) {
                    observer_error = Some(e);
                }
                match checked(previous, o.elapsed_ms) {
                    Ok(ms) => r.targets[2].compile_ms = ms,
                    Err(e) => observer_error = Some(e),
                }
            }
        });
        let result = result?;
        if let Some(e) = observer_error {
            return Err(e);
        }
        if result.process.exit != 0 {
            return Err(Error::new(
                ErrorKind::Compile,
                format!("fixed resource module failed compilation: {module}"),
            ));
        }
    }
    let before = resource_files(&workspace.path, limits)?;
    let projection = workspace.path.join("projection"); // Exporter requires fresh, nonexistent output root.
    let request=serde_json::to_vec(&serde_json::json!({"schema_version":1,"root":text(&originals)?,"toolchain_prefix":text(sysroot)?,"module":resource::ORIGINAL,"output_root":text(&projection)?,"output_module":resource::MODULE})).map_err(|_|identity("resource request serialization"))?;
    create_file(&workspace.path.join("request.json"), &request)?;
    let mut c = command(lean, &projector, Some(sysroot), &[sysroot.join("lib/lean")])?;
    c.env("LEAN_NUM_THREADS", "1")
        .env("LEAN_ABORT_ON_PANIC", "1")
        .args(["-j1", "-M4096", "--run"])
        .arg(projector.join("KernelSnapshot.lean"))
        .arg(workspace.path.join("request.json"));
    let mut observer_error = None;
    let result = process::capture_observed(c, limits.compile_timeout_ms, limits, 16 * MIB, |o| {
        if let Some(r) = report.as_deref_mut() {
            let previous = r.targets[2].compile_ms;
            if let Err(e) = observed(r, Some(2), 0, o) {
                observer_error = Some(e);
            }
            match checked(previous, o.elapsed_ms) {
                Ok(ms) => r.targets[2].compile_ms = ms,
                Err(e) => observer_error = Some(e),
            }
        }
    });
    let result = result?;
    if let Some(e) = observer_error {
        return Err(e);
    }
    if result.process.exit != 0 {
        return Err(Error::new(
            ErrorKind::Compile,
            "fixed resource kernel projection failed",
        ));
    }
    let line = result.stdout.strip_suffix(b"\n").unwrap_or(&result.stdout);
    let bytes = line
        .strip_prefix(resource::MARKER)
        .ok_or_else(|| identity("missing exact kernel projection marker"))?;
    if bytes.contains(&b'\n') {
        return Err(identity("unexpected kernel projection output"));
    }
    // Bound parsing + retained maps conservatively before decoding producer metadata.
    if bytes
        .len()
        .checked_mul(32)
        .and_then(|n| n.checked_add(2 * MIB))
        .and_then(|n| n.checked_add(result.stdout.capacity()))
        .and_then(|n| n.checked_add(result.process.stderr.capacity()))
        .is_none_or(|n| n > limits.inventory_bytes)
    {
        return Err(limit("resource producer metadata envelope"));
    }
    for file in &before {
        if file_digest(
            &workspace.path,
            &workspace.path.join(&file.path),
            file.bytes,
            until(limits.inventory_timeout_ms),
        )? != *file
        {
            return Err(identity("resource compiler inputs changed during snapshot"));
        }
    }
    create_file(&workspace.path.join("producer-report.json"), bytes)?;
    let prepared = resource::bind(
        workspace,
        root.to_owned(),
        sysroot.to_owned(),
        receipt.to_vec(),
        bytes,
        limits,
    )?;
    prepared.revalidate(limits)?;
    Ok(prepared)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(limits: Limits) -> (Workspace, Workspace, Config, Report) {
        let models = Workspace::new(None).unwrap();
        let retained = Workspace::new(None).unwrap();
        let config = Config {
            tool_root: models.path.clone(),
            lean: PathBuf::from("unused-tool"),
            artifacts: Some(retained.path.clone()),
            limits: limits.clone(),
        };
        let mut report = Report::new(limits);
        let path = models.path.join("RewriteWitness.lean");
        create_file(&path, b"bounded evidence").unwrap();
        report.targets[0].source =
            Some(file_digest(&models.path, &path, 1024, until(5000)).unwrap());
        (models, retained, config, report)
    }

    #[test]
    fn retained_quota_failure_creates_no_publication_or_unlisted_payload() {
        let limits = Limits {
            artifact_bytes: 1,
            ..Limits::default()
        };
        let (models, parent, config, mut report) = fixture(limits);
        assert!(publish_evidence(&config, &models.path, &mut report).is_err());
        assert_eq!(fs::read_dir(&parent.path).unwrap().count(), 0);
        assert!(report.artifacts.is_empty());
        assert_eq!(
            fs::read(models.path.join("RewriteWitness.lean")).unwrap(),
            b"bounded evidence"
        );
    }

    #[test]
    fn bounded_publication_copies_exact_bytes_and_accounts_the_report() {
        let limits = Limits {
            artifact_bytes: 1024 * 1024,
            ..Limits::default()
        };
        let (models, parent, config, mut report) = fixture(limits);
        let dir = publish_evidence(&config, &models.path, &mut report)
            .unwrap()
            .unwrap();
        assert_eq!(report.artifacts.len(), 1);
        assert_eq!(report.artifacts[0].blake3, digest(b"bounded evidence"));
        assert_eq!(
            fs::read(dir.join("RewriteWitness.lean")).unwrap(),
            b"bounded evidence"
        );
        save_report(&config, Some(&dir), &report).unwrap();
        let mut scan = FileScan::new(
            &parent.path,
            16,
            config.limits.artifact_bytes,
            until(5000),
            false,
        );
        scan.walk(&dir, 0).unwrap();
        assert_eq!(scan.files.len(), 2);
        assert!(scan.bytes <= config.limits.artifact_bytes);
        assert_eq!(report.status, Status::Failed); // This storage unit never supplies replay evidence.
    }

    #[test]
    fn raw_relative_paths_cannot_hide_empty_dot_or_drive_components() {
        for path in ["a//b", "a/./b", "a/../b", "a/", "/a", "C:/a", "a\\b"] {
            assert_eq!(relative(path).unwrap_err().kind, ErrorKind::ToolIdentity);
        }
        assert!(relative(".lake/packages/batteries/README.md").is_ok());
    }

    #[test]
    fn empty_directories_are_charged_before_unbounded_scan() {
        let space = Workspace::new(None).unwrap();
        for n in 0..5 {
            fs::create_dir(space.path.join(format!("empty{n}"))).unwrap();
        }
        let mut scan = FileScan::new(&space.path, 1, 1024, until(5000), false);
        let error = scan.walk(&space.path, 0).unwrap_err();
        assert_eq!(error.kind, ErrorKind::OutputLimit);
        assert!(scan.files.is_empty());
        assert_eq!(scan.entries, 5);
    }

    #[cfg(unix)]
    #[test]
    fn dangling_optional_artifact_is_present_and_refused_as_nonregular() {
        let space = Workspace::new(None).unwrap();
        let path = space.path.join("Model.olean.server");
        std::os::unix::fs::symlink("missing", &path).unwrap();
        assert!(present(&path).unwrap());
        assert_eq!(
            file_digest(&space.path, &path, 1024, until(5000))
                .unwrap_err()
                .kind,
            ErrorKind::ToolIdentity
        );
    }
}
