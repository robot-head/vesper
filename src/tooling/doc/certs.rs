//! `.olean` proof certificates in a Loom content-addressed store (§40.1 item
//! 4, §8.2.6 OLEAN_CERTS).
//!
//! A certificate record is a Loom derivation whose single input is the exact
//! bytes of a `vesper proof-audit --json` report, stored as an artifact blob,
//! and whose output is the audited `.olean` blob. Options name the module,
//! the file's audited path, size and BLAKE3. Lookup recomputes the same key
//! from the report bytes, so a certificate is only ever linked to the audit
//! that inventoried it. Every read rehashes the object (Loom CAS) and the
//! bytes must also match the audited size and plain BLAKE3.
//!
//! A link is content identity only. It is not provenance, it does not change
//! a badge level, and the badge still describes Lean theorems about a model.
use std::{
    fs,
    io::Read,
    path::{Component, Path, PathBuf},
};
use vesper_doc::badges::{self, CertificateLink, CertificateStatus, FileDigest, ProofAudit};
use vesper_loom_cas::{DerivationInputs, Domain, ObjectId, Store, blob_id};

pub const BUILDER: &str = "vesper-codex-olean";
pub const BUILDER_VERSION: &str = "1";
const MAX_OLEAN_BYTES: u64 = 256 * 1024 * 1024;

fn inputs(audit: &ProofAudit, report: ObjectId, file: &FileDigest) -> DerivationInputs {
    DerivationInputs::new(Domain::Artifact, BUILDER, BUILDER_VERSION)
        .with_option("module", audit.module.clone())
        .with_option(
            "artifact_inventory_blake3",
            audit.artifact_inventory_blake3.clone(),
        )
        .with_option("path", file.path.clone())
        .with_option("bytes", file.bytes.to_string())
        .with_option("blake3", file.blake3.clone())
        .with_input("proof_audit", report)
}

fn relative_path(path: &str) -> Result<&Path, String> {
    let p = Path::new(path);
    if path.is_empty()
        || path.len() > 1024
        || !p.components().all(|c| matches!(c, Component::Normal(_)))
    {
        return Err(format!(
            "audited artifact path '{path}' is not a plain relative path"
        ));
    }
    Ok(p)
}

fn read_regular(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let meta = fs::symlink_metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if !meta.is_file() {
        return Err(format!("{} is not a regular file", path.display()));
    }
    if meta.len() > limit {
        return Err(format!("{} exceeds the size limit", path.display()));
    }
    let mut bytes = Vec::new();
    fs::File::open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if bytes.len() as u64 > limit {
        return Err(format!("{} exceeds the size limit", path.display()));
    }
    Ok(bytes)
}

fn matches_audit(bytes: &[u8], file: &FileDigest) -> bool {
    bytes.len() as u64 == file.bytes && blake3::hash(bytes).to_hex().as_str() == file.blake3
}

#[derive(Debug, serde::Serialize)]
pub struct Stored {
    pub module: String,
    pub path: String,
    pub object_id: String,
    pub newly_recorded: bool,
}

/// Store every `.olean` an accepted audit inventories, from `root` (the Lean
/// build directory the audit read), and record the certificate derivations.
/// Bytes that differ from the audited size or BLAKE3 are refused.
pub fn store_certificates(
    store: &Store,
    report_bytes: &[u8],
    root: &Path,
) -> Result<Vec<Stored>, String> {
    let text = std::str::from_utf8(report_bytes).map_err(|_| "proof audit is not UTF-8")?;
    let audit = badges::parse_audit(text)?;
    if !(audit.accepted && audit.kernel_replayed) {
        return Err("only accepted, kernel-replayed audits have certificates to store".into());
    }
    let files = badges::olean_files(&audit);
    if files.is_empty() {
        return Err("the audit inventories no .olean artifact".into());
    }
    let report = store
        .put_blob(Domain::Artifact, report_bytes)
        .map_err(|e| e.to_string())?
        .id;
    let mut out = Vec::new();
    for file in files {
        let bytes = read_regular(&root.join(relative_path(&file.path)?), MAX_OLEAN_BYTES)?;
        if !matches_audit(&bytes, file) {
            return Err(format!(
                "{}: bytes differ from the audited size/BLAKE3; rerun the audit",
                file.path
            ));
        }
        let object = store
            .put_blob(Domain::Artifact, &bytes)
            .map_err(|e| e.to_string())?
            .id;
        let outcome = store
            .publish_derivation(&inputs(&audit, report, file), object)
            .map_err(|e| e.to_string())?;
        out.push(Stored {
            module: audit.module.clone(),
            path: file.path.clone(),
            object_id: object.to_string(),
            newly_recorded: outcome.newly_recorded,
        });
    }
    Ok(out)
}

/// The default store, `~/.vesper/store`.
pub fn default_store() -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .filter(|h| !h.is_empty())
        .ok_or("cannot locate ~/.vesper/store: HOME is unset")?;
    Ok(PathBuf::from(home).join(".vesper").join("store"))
}

/// A certificate found in the store, with its verified bytes.
#[derive(Debug)]
pub struct Found {
    pub link: CertificateLink,
    pub bytes: Option<Vec<u8>>,
}

/// Look up the certificates of each audit (given with its exact report
/// bytes). An existing store is required; it is never created here. A record
/// whose object does not match the audit is an error, never a link.
pub fn link_certificates(
    store_root: &Path,
    audits: &[(Vec<u8>, ProofAudit)],
) -> Result<Vec<Found>, String> {
    if !fs::symlink_metadata(store_root).is_ok_and(|m| m.is_dir()) {
        return Err(format!(
            "certificate store {} is not an existing directory",
            store_root.display()
        ));
    }
    let store = Store::open(store_root).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for (bytes, audit) in audits {
        let report = blob_id(Domain::Artifact, bytes);
        for file in badges::olean_files(audit) {
            let mut link = CertificateLink {
                module: audit.module.clone(),
                artifact_inventory_blake3: audit.artifact_inventory_blake3.clone(),
                path: file.path.clone(),
                bytes: file.bytes,
                blake3: file.blake3.clone(),
                status: CertificateStatus::Missing,
                object_id: None,
                href: None,
            };
            let found = store
                .lookup_derivation(&inputs(audit, report, file))
                .map_err(|e| format!("{}: {e}", file.path))?;
            let mut data = None;
            if let Some(id) = found {
                let blob = store
                    .read_blob(&id)
                    .map_err(|e| format!("{}: {e}", file.path))?;
                if !matches_audit(&blob, file) {
                    return Err(format!(
                        "{}: stored certificate {id} does not match the audited size/BLAKE3",
                        file.path
                    ));
                }
                link.status = CertificateStatus::Linked;
                link.object_id = Some(id.to_string());
                data = Some(blob);
            }
            out.push(Found { link, bytes: data });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);
    impl TempDir {
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn tempdir() -> TempDir {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "vesper-codex-certs-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }

    fn audit_for(olean: &[u8]) -> String {
        format!(
            r#"{{"schema_version":1,"status":"audited","accepted":true,"kernel_replayed":true,"cache_eligible":false,"module":"M","lean_version":"Lean (version 4.24.0)","lean_binary_blake3":"00","auditor_blake3":"01","artifact_inventory_blake3":"02","checked_constants":1,"allowed_axioms":[],"forbidden_axioms":[],"theorems":[{{"name":"M.t","axioms":[],"type_repr":"True"}}],"artifact_files":[{{"path":"M.olean","bytes":{},"blake3":"{}"}},{{"path":"M.ilean","bytes":1,"blake3":"03"}}],"imports":[]}}"#,
            olean.len(),
            blake3::hash(olean).to_hex()
        )
    }

    #[test]
    fn stored_certificates_link_only_to_their_own_audit() {
        let dir = tempdir();
        let build = dir.path().join("build");
        fs::create_dir(&build).unwrap();
        fs::write(build.join("M.olean"), b"olean bytes").unwrap();
        let report = audit_for(b"olean bytes");
        let store_root = dir.path().join("store");
        let store = Store::open(&store_root).unwrap();
        let stored = store_certificates(&store, report.as_bytes(), &build).unwrap();
        assert_eq!(stored.len(), 1);
        assert!(stored[0].newly_recorded);
        // Idempotent.
        assert!(!store_certificates(&store, report.as_bytes(), &build).unwrap()[0].newly_recorded);
        let audit = badges::parse_audit(&report).unwrap();
        let found =
            link_certificates(&store_root, &[(report.clone().into_bytes(), audit)]).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].link.status, CertificateStatus::Linked);
        assert_eq!(
            found[0].link.object_id.as_deref(),
            Some(stored[0].object_id.as_str())
        );
        assert_eq!(found[0].bytes.as_deref(), Some(&b"olean bytes"[..]));
        // A different report (other bytes, same theorems) finds nothing.
        let other = report.replace("\"checked_constants\":1", "\"checked_constants\":2");
        let other_audit = badges::parse_audit(&other).unwrap();
        let missing = link_certificates(&store_root, &[(other.into_bytes(), other_audit)]).unwrap();
        assert_eq!(missing[0].link.status, CertificateStatus::Missing);
        assert!(missing[0].bytes.is_none());
    }

    #[test]
    fn mismatched_bytes_paths_and_stores_are_refused() {
        let dir = tempdir();
        fs::write(dir.path().join("M.olean"), b"tampered").unwrap();
        let store = Store::open(dir.path().join("store")).unwrap();
        let report = audit_for(b"original");
        assert!(
            store_certificates(&store, report.as_bytes(), dir.path())
                .unwrap_err()
                .contains("differ from the audited")
        );
        let escaping = audit_for(b"x").replace("\"M.olean\"", "\"../M.olean\"");
        assert!(
            store_certificates(&store, escaping.as_bytes(), dir.path())
                .unwrap_err()
                .contains("plain relative path")
        );
        let rejected = audit_for(b"x").replace(
            "\"accepted\":true,\"kernel_replayed\":true",
            "\"accepted\":true,\"kernel_replayed\":false",
        );
        assert!(store_certificates(&store, rejected.as_bytes(), dir.path()).is_err());
        let audit = badges::parse_audit(&report).unwrap();
        assert!(
            link_certificates(&dir.path().join("absent"), &[(report.into_bytes(), audit)])
                .unwrap_err()
                .contains("not an existing directory")
        );
        assert!(!dir.path().join("absent").exists());
    }

    #[test]
    fn a_record_pointing_at_other_bytes_is_an_error() {
        let dir = tempdir();
        let root = dir.path().join("store");
        let store = Store::open(&root).unwrap();
        let report = audit_for(b"real");
        let audit = badges::parse_audit(&report).unwrap();
        let report_id = store
            .put_blob(Domain::Artifact, report.as_bytes())
            .unwrap()
            .id;
        let wrong = store.put_blob(Domain::Artifact, b"fake").unwrap().id;
        store
            .publish_derivation(&inputs(&audit, report_id, &audit.artifact_files[0]), wrong)
            .unwrap();
        let error = link_certificates(&root, &[(report.into_bytes(), audit)])
            .err()
            .unwrap();
        assert!(error.contains("does not match the audited"), "{error}");
    }
}
