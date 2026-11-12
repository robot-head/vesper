//! Additive raw quotation runner. Cache hits carry no host or approval authority.
use crate::{
    identity::HostIdentity,
    quote_key,
    quote_record::{self, RawRecord},
    request::MacroRequest,
    runner::{CachePolicy, CacheStatus, CorruptionPolicy, ExecOptions, MacroEngine, MacroRunner},
};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
use vesper_loom_cas::{
    CasError, DerivationInputs, DerivationKey, Domain, MaterializeOptions, ObjectId, ObjectKind,
    Store, blob_id,
};
use vesper_loom_sandbox::{StepConfig, quote_host, quote_transport as q};
#[derive(Debug)]
pub enum Error {
    Legacy(crate::RunnerError),
    Transport(q::Error),
    Host(quote_host::Error),
    Record(quote_record::Error),
    Cas(CasError),
    Io(std::io::Error),
    Request(&'static str),
    Corrupt(String),
    Divergent,
    Identity,
    GuestFailed,
    OutputFiles,
    InputMutated,
    Allocation,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "quotation runner {self:?}")
    }
}
impl std::error::Error for Error {}
impl From<crate::RunnerError> for Error {
    fn from(e: crate::RunnerError) -> Self {
        Self::Legacy(e)
    }
}
impl From<q::Error> for Error {
    fn from(e: q::Error) -> Self {
        Self::Transport(e)
    }
}
impl From<quote_host::Error> for Error {
    fn from(e: quote_host::Error) -> Self {
        Self::Host(e)
    }
}
impl From<quote_record::Error> for Error {
    fn from(e: quote_record::Error) -> Self {
        Self::Record(e)
    }
}
impl From<CasError> for Error {
    fn from(e: CasError) -> Self {
        Self::Cas(e)
    }
}
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}
#[derive(Debug)]
pub struct Prepared {
    request: MacroRequest,
    invocation: q::Invocation,
    identity: HostIdentity,
    limits: q::Limits,
    component: ObjectId,
    mounts: Vec<(String, ObjectId)>,
    inputs: DerivationInputs,
    key: DerivationKey,
}
impl Prepared {
    pub fn key(&self) -> DerivationKey {
        self.key
    }
    pub fn invocation(&self) -> &q::Invocation {
        &self.invocation
    }
    pub fn derivation_inputs(&self) -> &DerivationInputs {
        &self.inputs
    }
    pub fn limits(&self) -> q::Limits {
        self.limits
    }
}
#[derive(Debug)]
pub struct RawExecution {
    pub status: CacheStatus,
    pub executed: bool,
    pub newly_recorded: bool,
    pub key: DerivationKey,
    pub record_id: ObjectId,
    pub record: RawRecord,
}
pub struct QuoteRunner {
    base: MacroRunner,
    scratch: PathBuf,
    caps: q::Limits,
}
impl QuoteRunner {
    pub fn open(
        engine: Arc<MacroEngine>,
        store: impl AsRef<Path>,
        scratch: impl AsRef<Path>,
    ) -> Result<Self, Error> {
        let base = MacroRunner::open(engine, store, &scratch)?;
        Ok(Self {
            base,
            scratch: scratch.as_ref().canonicalize()?,
            caps: q::Limits::default(),
        })
    }
    pub fn with_caps(mut self, caps: q::Limits) -> Result<Self, Error> {
        caps.validate()?;
        self.caps = caps;
        Ok(self)
    }
    pub fn store(&self) -> &Store {
        self.base.store()
    }
    pub fn engine(&self) -> &Arc<MacroEngine> {
        self.base.engine()
    }
    pub fn identity(&self) -> &HostIdentity {
        self.base.identity()
    }
    /// Reuses only the legacy verified snapshot preparation, never its key or
    /// flat invocation as quotation authority. Every new key starts from scratch.
    pub fn prepare(
        &self,
        request: &MacroRequest,
        invocation: &q::Invocation,
        limits: q::Limits,
    ) -> Result<Prepared, Error> {
        self.check(request, invocation, limits)?;
        let canonical = quote_key::canonical_invocation(invocation, limits)?;
        // Full structured encoding is bounded before source/component snapshots
        // or cloned request/slot metadata can be published.
        let legacy = self.base.prepare(request, Vec::new())?;
        let invocation_id = self.store().put_blob(Domain::Source, &canonical)?.id;
        let component = legacy.component();
        let mounts = legacy.snapshots().to_vec();
        let mut inputs =
            DerivationInputs::new(Domain::Artifact, "vesper-forge-quotation-runner", "1");
        for (k, v) in self.identity().key_options() {
            inputs.options.insert(k.into(), v);
        }
        for (k, v) in [
            (
                "quote.key-schema",
                quote_key::KEY_SCHEMA_VERSION.to_string(),
            ),
            ("quote.profile", quote_key::PROFILE.into()),
            ("quote.wit-package", q::WIT_PACKAGE.into()),
            (
                "quote.wit-blake3",
                blake3::hash(q::WIT_SOURCE.as_bytes()).to_hex().to_string(),
            ),
            ("quote.protocol", q::PROTOCOL_VERSION.to_string()),
            (
                "quote.record-schema",
                quote_record::RECORD_SCHEMA_VERSION.to_string(),
            ),
            (
                "quote.limits",
                serde_json::to_string(&limits).map_err(|_| Error::Request("limits encoding"))?,
            ),
            (
                "quote.runtime",
                serde_json::to_string(&request.limits)
                    .map_err(|_| Error::Request("runtime encoding"))?,
            ),
            (
                "quote.determinism",
                serde_json::to_string(&request.determinism)
                    .map_err(|_| Error::Request("determinism encoding"))?,
            ),
        ] {
            inputs.options.insert(k.into(), v);
        }
        inputs = inputs
            .with_input("component", component)
            .with_input("quote-invocation", invocation_id);
        for (guest, tree) in &mounts {
            inputs = inputs.with_input(format!("mount:{guest}"), *tree);
        }
        let key = inputs
            .key()
            .map_err(|_| Error::Request("invalid quotation derivation"))?;
        Ok(Prepared {
            request: request.clone(),
            invocation: invocation.clone(),
            identity: self.identity().clone(),
            limits,
            component,
            mounts,
            inputs,
            key,
        })
    }
    fn check(
        &self,
        req: &MacroRequest,
        inv: &q::Invocation,
        limits: q::Limits,
    ) -> Result<(), Error> {
        limits.validate()?;
        limits.check_invocation(inv)?;
        // Fixed additional finite limits cover cloned request metadata/components.
        if req.component.len() > 16 << 20 || req.inputs.len() > 32 {
            return Err(Error::Request("component/input-count limit"));
        }
        for i in &req.inputs {
            if i.guest_path.len() > 4096 || i.host_path.as_os_str().len() > 4096 {
                return Err(Error::Request("input path limit"));
            }
        }
        let kind = match req.kind {
            crate::MacroKind::ProcMacro => q::MacroKind::ProcMacro,
            crate::MacroKind::DeriveMacro => q::MacroKind::DeriveMacro,
        };
        if req.entry != inv.entry || req.args != inv.args || kind != inv.kind {
            return Err(Error::Request("invocation/request mismatch"));
        }
        let a = serde_json::to_value(limits).map_err(|_| Error::Request("limits"))?;
        let b = serde_json::to_value(self.caps).map_err(|_| Error::Request("caps"))?;
        if a.as_object()
            .ok_or(Error::Request("limits"))?
            .iter()
            .any(|(k, v)| v.as_u64() > b.get(k).and_then(|v| v.as_u64()))
        {
            return Err(Error::Request("limits exceed runner caps"));
        }
        req.validate(self.base.caps())
            .map_err(crate::RunnerError::from)?;
        Ok(())
    }
    pub fn execute(&self, p: &Prepared, options: ExecOptions) -> Result<RawExecution, Error> {
        if &p.identity != self.identity() {
            return Err(Error::Identity);
        }
        self.check(&p.request, &p.invocation, p.limits)?;
        if options.cache != CachePolicy::Uncached && !self.identity().is_complete() {
            return Err(Error::Identity);
        }
        let mut existing = None;
        let mut repaired = false;
        if options.cache != CachePolicy::Uncached {
            match self.lookup(p) {
                Ok(v) => existing = v,
                Err(e) => match options.on_corrupt {
                    CorruptionPolicy::Fail => return Err(e),
                    CorruptionPolicy::Rerun => repaired = true,
                },
            }
        }
        if options.cache == CachePolicy::ReadWrite
            && let Some((record_id, record)) = existing.take()
        {
            return Ok(RawExecution {
                status: CacheStatus::Hit,
                executed: false,
                newly_recorded: false,
                key: p.key,
                record_id,
                record,
            });
        }
        let record = self.run(p)?;
        let bytes = record.encode(
            p.limits,
            p.invocation.slots.len(),
            p.request.limits.max_stdio_bytes,
        )?;
        let record_id = blob_id(Domain::Artifact, &bytes);
        let mut newly_recorded = false;
        if options.cache != CachePolicy::Uncached {
            if existing.as_ref().is_some_and(|(id, _)| *id != record_id) {
                return Err(Error::Divergent);
            }
            self.store().put_blob(Domain::Artifact, &bytes)?;
            match self.store().publish_derivation(&p.inputs, record_id) {
                Ok(v) => newly_recorded = v.newly_recorded,
                Err(CasError::DerivationConflict { .. }) => return Err(Error::Divergent),
                Err(e) => return Err(e.into()),
            }
        }
        let status = match (options.cache, repaired) {
            (CachePolicy::Uncached, _) => CacheStatus::Uncached,
            (_, true) => CacheStatus::Repaired,
            (CachePolicy::Verify, _) => CacheStatus::Verified,
            _ => CacheStatus::Miss,
        };
        Ok(RawExecution {
            status,
            executed: true,
            newly_recorded,
            key: p.key,
            record_id,
            record,
        })
    }
    fn lookup(&self, p: &Prepared) -> Result<Option<(ObjectId, RawRecord)>, Error> {
        let Some(id) = self.store().lookup_derivation(&p.inputs)? else {
            return Ok(None);
        };
        if id.domain != Domain::Artifact || id.kind != ObjectKind::Blob {
            return Err(Error::Corrupt("not an artifact blob".into()));
        }
        let path = self.store().object_path(&id);
        let metadata = fs::symlink_metadata(&path)?;
        if !metadata.is_file() || metadata.len() > p.limits.max_record_bytes as u64 {
            return Err(Error::Corrupt("raw object kind/size".into()));
        }
        let mut options = fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let mut file = options.open(path)?;
        if !file.metadata()?.is_file() {
            return Err(Error::Corrupt("raw object is not regular".into()));
        }
        let mut bytes = Vec::new();
        let mut buffer = [0u8; 8192];
        loop {
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            if bytes
                .len()
                .checked_add(count)
                .is_none_or(|n| n > p.limits.max_record_bytes)
            {
                return Err(Error::Corrupt("raw object grew beyond cap".into()));
            }
            bytes
                .try_reserve_exact(count)
                .map_err(|_| Error::Allocation)?;
            bytes.extend_from_slice(&buffer[..count]);
        }
        if blob_id(Domain::Artifact, &bytes) != id {
            return Err(Error::Corrupt("raw object hash".into()));
        }
        let record = RawRecord::decode(
            &bytes,
            p.limits,
            p.invocation.slots.len(),
            p.request.limits.max_stdio_bytes,
        )?;
        if record.key != p.key.0.0 {
            return Err(Error::Corrupt("raw record execution key".into()));
        }
        Ok(Some((id, record)))
    }
    fn run(&self, p: &Prepared) -> Result<RawRecord, Error> {
        let run = RunDirectory::new(&self.scratch)?;
        let out = run.path.join("out");
        fs::create_dir(&out)?;
        let mut config = StepConfig::new(&out);
        let mut inputs = Vec::new();
        for (i, (guest, tree)) in p.mounts.iter().enumerate() {
            let dir = run.path.join(format!("in-{i}"));
            self.store()
                .materialize(tree, &dir, MaterializeOptions::default())?;
            config = config.input(&dir, guest);
            inputs.push((tree, dir));
        }
        config.limits = p.request.limits.clone();
        config.determinism = p.request.determinism;
        let component = self.engine().component(p.component, &p.request.component)?;
        self.engine().begin_quotation_execution()?;
        let report = self.engine().quotation_host().run_quotation(
            &component,
            &config,
            &p.invocation,
            p.limits,
        )?;
        let outcome = report.result.map_err(|_| Error::GuestFailed)?;
        if outcome
            .diagnostics
            .iter()
            .any(|d| d.severity == vesper_loom_sandbox::Severity::Error)
        {
            return Err(Error::GuestFailed);
        }
        if !report.outputs.is_empty() {
            return Err(Error::OutputFiles);
        }
        for (tree, dir) in inputs {
            if self.store().ingest_dir(Domain::Source, &dir)?.id != *tree {
                return Err(Error::InputMutated);
            }
        }
        Ok(RawRecord {
            key: p.key.0.0,
            fuel_consumed: report.fuel_consumed,
            peak_memory_bytes: report.peak_memory_bytes as u64,
            outcome,
            stdout: report.stdout,
            stderr: report.stderr,
        })
    }
}
struct RunDirectory {
    path: PathBuf,
}
impl RunDirectory {
    fn new(parent: &Path) -> Result<Self, Error> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        for _ in 0..32 {
            let n = mint_counter(&NEXT)?;
            let path = parent.join(format!("quote-{}-{n}", std::process::id()));
            match fs::create_dir(&path) {
                Ok(()) => return Ok(Self { path }),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e.into()),
            }
        }
        Err(Error::Request("scratch collisions"))
    }
}
impl Drop for RunDirectory {
    fn drop(&mut self) {
        let _ = writable(&self.path);
        let _ = fs::remove_dir_all(&self.path);
    }
}
fn writable(path: &Path) -> std::io::Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if meta.is_dir() {
        let mut permissions = meta.permissions();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // Cleanup needs owner traversal/write access, not broader grants.
            permissions.set_mode(permissions.mode() | 0o700);
        }
        #[cfg(not(unix))]
        {
            // Windows clears its readonly attribute; no Unix mode is changed.
            #[allow(clippy::permissions_set_readonly_false)]
            permissions.set_readonly(false);
        }
        fs::set_permissions(path, permissions)?;
        for entry in fs::read_dir(path)? {
            writable(&entry?.path())?;
        }
    } else if meta.is_file() {
        #[cfg(windows)]
        {
            let mut permissions = meta.permissions();
            // Windows clears its readonly attribute; no Unix mode is changed.
            #[allow(clippy::permissions_set_readonly_false)]
            permissions.set_readonly(false);
            fs::set_permissions(path, permissions)?;
        }
    }
    Ok(())
}

fn mint_counter(counter: &AtomicU64) -> Result<u64, Error> {
    let mut n = counter.load(Ordering::SeqCst);
    loop {
        let next = n
            .checked_add(1)
            .ok_or(Error::Request("scratch counter exhausted"))?;
        match counter.compare_exchange_weak(n, next, Ordering::SeqCst, Ordering::SeqCst) {
            Ok(_) => return Ok(n),
            Err(now) => n = now,
        }
    }
}
impl std::fmt::Debug for QuoteRunner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QuoteRunner")
            .field("store", &self.store().root())
            .field("scratch", &self.scratch)
            .field("caps", &self.caps)
            .finish_non_exhaustive()
    }
}
