//! Stage 1 frontend evidence. A Cranelift-only record is deliberately not a
//! cross-backend certificate. This is not the Stage 2/3 fixed-point gate.
use crate::{Blake3Digest, seed::SeedLock, source_merkle};
use serde::{Deserialize, Serialize};

pub const SCHEMA: &str = "vesper.bootstrap.stage1/1";
pub const MAX_BYTES: usize = 256 * 1024;
pub const MAX_PROGRAMS: usize = 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Stage1Error {
    TooLarge,
    Syntax(String),
    InvalidField(&'static str),
    ParentInvalid(String),
    DigestMismatch {
        field: String,
    },
    CorpusMismatch,
    ResultMismatch {
        program: String,
    },
    BackendUnavailable {
        program: String,
        reason: String,
    },
    /// A fresh measurement lost a verified build that the reference lock has.
    Regression {
        program: String,
        reason: String,
    },
    TargetMismatch {
        locked: String,
        measured: String,
    },
}
impl std::fmt::Display for Stage1Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Stage1Error {}

/// Actual output bytes and the frontend's full result word (CST digest or byte-class checksum).
pub struct FrontendOutput<'a> {
    pub artifact: &'a [u8],
    pub result: i64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ResultKind {
    CstDigest,
    LexerByteClasses,
}

pub struct ProgramEvidence<'a> {
    pub name: &'a str,
    pub kind: ResultKind,
    pub source: &'a [u8],
    pub seed_result: i64,
    pub cranelift: FrontendOutput<'a>,
    /// A typed backend refusal, never a substitute artifact or a seed result.
    pub verified: Result<FrontendOutput<'a>, &'a str>,
}
pub struct Stage1Evidence<'a> {
    pub parent_lock: &'a [u8],
    pub seed_binary: &'a [u8],
    pub frontend_sources: &'a [(&'a str, &'a [u8])],
    pub programs: &'a [ProgramEvidence<'a>],
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stage1Program {
    pub name: String,
    pub kind: ResultKind,
    pub source: String,
    pub cranelift_artifact: String,
    pub result: i64,
    pub verified: VerifiedRecord,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "kebab-case", deny_unknown_fields)]
pub enum VerifiedRecord {
    Built { artifact: String },
    Unavailable { reason: String },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stage1Lock {
    pub schema: String,
    pub target: String,
    pub parent_seed_lock: String,
    /// The actual seed used for this run. The historical parent lock does not
    /// certify a newer seed binary; it is a lineage anchor only.
    pub seed_binary: String,
    pub frontend_sources: String,
    pub programs: Vec<Stage1Program>,
}

pub fn digest(bytes: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(bytes))
}
fn check_digest(value: &str) -> Result<(), Stage1Error> {
    Blake3Digest::parse_prefixed(value, "blake3:", "stage1")
        .map(|_| ())
        .map_err(|_| Stage1Error::InvalidField("digest"))
}
fn label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && !value.starts_with('.')
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-.".contains(&b))
}
/// How a fresh measurement differs from a reference lock that it does not
/// contradict. `improved` names programs whose verified build is new;
/// `stale` names fields whose recorded values a re-record would change.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OutcomeReport {
    pub improved: Vec<String>,
    pub stale: Vec<String>,
}
impl OutcomeReport {
    /// True when the fresh measurement would re-record to the same lock.
    pub fn is_current(&self) -> bool {
        self.improved.is_empty() && self.stale.is_empty()
    }
}

impl Stage1Lock {
    pub fn parse(bytes: &[u8]) -> Result<Self, Stage1Error> {
        if bytes.len() > MAX_BYTES {
            return Err(Stage1Error::TooLarge);
        }
        let text = std::str::from_utf8(bytes).map_err(|e| Stage1Error::Syntax(e.to_string()))?;
        let lock: Self = toml::from_str(text).map_err(|e| Stage1Error::Syntax(e.to_string()))?;
        lock.validate()?;
        Ok(lock)
    }
    pub fn validate(&self) -> Result<(), Stage1Error> {
        if self.schema != SCHEMA || !label(&self.target) {
            return Err(Stage1Error::InvalidField("schema or target"));
        }
        for d in [
            &self.parent_seed_lock,
            &self.seed_binary,
            &self.frontend_sources,
        ] {
            check_digest(d)?;
        }
        if self.programs.is_empty() || self.programs.len() > MAX_PROGRAMS {
            return Err(Stage1Error::InvalidField("program count"));
        }
        if !self
            .programs
            .iter()
            .any(|p| p.kind == ResultKind::CstDigest)
        {
            return Err(Stage1Error::InvalidField("missing CST corpus"));
        }
        if self.programs.windows(2).any(|w| w[0].name >= w[1].name) {
            return Err(Stage1Error::InvalidField("program order or duplicate"));
        }
        for p in &self.programs {
            if !label(&p.name) || (p.kind == ResultKind::CstDigest && p.result < 0) {
                return Err(Stage1Error::InvalidField("program name or result"));
            }
            check_digest(&p.source)?;
            check_digest(&p.cranelift_artifact)?;
            match &p.verified {
                VerifiedRecord::Built { artifact } => check_digest(artifact)?,
                VerifiedRecord::Unavailable { reason }
                    if !reason.is_empty() && reason.len() <= 1024 && !reason.contains('\0') => {}
                _ => return Err(Stage1Error::InvalidField("verified backend")),
            }
        }
        Ok(())
    }
    pub fn to_toml(&self) -> Result<String, Stage1Error> {
        self.validate()?;
        let text = toml::to_string(self).map_err(|e| Stage1Error::Syntax(e.to_string()))?;
        if text.len() > MAX_BYTES {
            return Err(Stage1Error::TooLarge);
        }
        Ok(text)
    }
    /// Record measured evidence, including explicit backend refusals. Call
    /// `require_cross_backend` before using it as cross-backend certification.
    pub fn record(target: &str, evidence: &Stage1Evidence<'_>) -> Result<Self, Stage1Error> {
        if evidence.programs.is_empty() || evidence.programs.len() > MAX_PROGRAMS {
            return Err(Stage1Error::InvalidField("program count"));
        }
        SeedLock::parse(evidence.parent_lock)
            .map_err(|e| Stage1Error::ParentInvalid(e.to_string()))?;
        if evidence.seed_binary.is_empty() || evidence.frontend_sources.is_empty() {
            return Err(Stage1Error::InvalidField("empty seed or frontend"));
        }
        let sources = source_merkle(evidence.frontend_sources)
            .map_err(|e| Stage1Error::Syntax(e.to_string()))?;
        let mut programs = Vec::new();
        for p in evidence.programs {
            if p.cranelift.artifact.is_empty()
                || (p.kind == ResultKind::CstDigest && p.cranelift.result < 0)
                || p.cranelift.result != p.seed_result
            {
                return Err(Stage1Error::ResultMismatch {
                    program: p.name.into(),
                });
            }
            let verified = match &p.verified {
                Ok(v) => {
                    if v.artifact.is_empty() || v.result != p.cranelift.result {
                        return Err(Stage1Error::ResultMismatch {
                            program: p.name.into(),
                        });
                    }
                    VerifiedRecord::Built {
                        artifact: digest(v.artifact),
                    }
                }
                Err(reason) => VerifiedRecord::Unavailable {
                    reason: (*reason).into(),
                },
            };
            programs.push(Stage1Program {
                name: p.name.into(),
                kind: p.kind,
                source: digest(p.source),
                cranelift_artifact: digest(p.cranelift.artifact),
                result: p.cranelift.result,
                verified,
            });
        }
        programs.sort_by(|a, b| a.name.cmp(&b.name));
        let lock = Self {
            schema: SCHEMA.into(),
            target: target.into(),
            parent_seed_lock: digest(evidence.parent_lock),
            seed_binary: digest(evidence.seed_binary),
            frontend_sources: format!("blake3:{sources}"),
            programs,
        };
        lock.validate()?;
        lock.to_toml()?;
        Ok(lock)
    }
    /// Rehash actual evidence; changing the parent, seed, source, corpus or
    /// either artifact fails even if the CST results still agree.
    pub fn verify(&self, evidence: &Stage1Evidence<'_>) -> Result<(), Stage1Error> {
        self.validate()?;
        let actual = Self::record(&self.target, evidence)?;
        for (field, locked, measured) in [
            (
                "parent_seed_lock",
                &self.parent_seed_lock,
                &actual.parent_seed_lock,
            ),
            ("seed_binary", &self.seed_binary, &actual.seed_binary),
            (
                "frontend_sources",
                &self.frontend_sources,
                &actual.frontend_sources,
            ),
        ] {
            if locked != measured {
                return Err(Stage1Error::DigestMismatch {
                    field: field.into(),
                });
            }
        }
        if self
            .programs
            .iter()
            .map(|p| &p.name)
            .ne(actual.programs.iter().map(|p| &p.name))
        {
            return Err(Stage1Error::CorpusMismatch);
        }
        for (locked, measured) in self.programs.iter().zip(&actual.programs) {
            if locked != measured {
                return Err(Stage1Error::DigestMismatch {
                    field: locked.name.clone(),
                });
            }
        }
        Ok(())
    }
    /// Ratchet a fresh measurement against this reference lock without
    /// requiring bit-identical artifacts. The seed, the frontend sources and
    /// the generated artifacts change with every compiler commit, so a
    /// difference there is only reported as stale. Hard failures: another
    /// target or parent, another corpus (names, kinds or source bytes), and a
    /// program whose verified build the reference has but the fresh run lost.
    /// Every fresh result was already checked against the seed and across
    /// backends when the fresh lock was recorded.
    pub fn compare_outcome(&self, fresh: &Stage1Lock) -> Result<OutcomeReport, Stage1Error> {
        self.validate()?;
        fresh.validate()?;
        if self.target != fresh.target {
            return Err(Stage1Error::TargetMismatch {
                locked: self.target.clone(),
                measured: fresh.target.clone(),
            });
        }
        if self.parent_seed_lock != fresh.parent_seed_lock {
            return Err(Stage1Error::DigestMismatch {
                field: "parent_seed_lock".into(),
            });
        }
        if self.programs.len() != fresh.programs.len()
            || self
                .programs
                .iter()
                .zip(&fresh.programs)
                .any(|(a, b)| a.name != b.name || a.kind != b.kind || a.source != b.source)
        {
            return Err(Stage1Error::CorpusMismatch);
        }
        let mut report = OutcomeReport::default();
        for (field, locked, measured) in [
            ("seed_binary", &self.seed_binary, &fresh.seed_binary),
            (
                "frontend_sources",
                &self.frontend_sources,
                &fresh.frontend_sources,
            ),
        ] {
            if locked != measured {
                report.stale.push(field.into());
            }
        }
        for (locked, measured) in self.programs.iter().zip(&fresh.programs) {
            let name = &locked.name;
            if locked.result != measured.result {
                report.stale.push(format!("{name}.result"));
            }
            if locked.cranelift_artifact != measured.cranelift_artifact {
                report.stale.push(format!("{name}.cranelift_artifact"));
            }
            match (&locked.verified, &measured.verified) {
                (VerifiedRecord::Built { .. }, VerifiedRecord::Unavailable { reason }) => {
                    return Err(Stage1Error::Regression {
                        program: name.clone(),
                        reason: reason.clone(),
                    });
                }
                (VerifiedRecord::Unavailable { .. }, VerifiedRecord::Built { .. }) => {
                    report.improved.push(name.clone());
                }
                (a, b) if a != b => report.stale.push(format!("{name}.verified")),
                _ => {}
            }
        }
        Ok(report)
    }
    pub fn require_cross_backend(&self) -> Result<(), Stage1Error> {
        self.validate()?;
        for p in &self.programs {
            if let VerifiedRecord::Unavailable { reason } = &p.verified {
                return Err(Stage1Error::BackendUnavailable {
                    program: p.name.clone(),
                    reason: reason.clone(),
                });
            }
        }
        Ok(())
    }
}
