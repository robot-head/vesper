//! Stage 0 seed reproducibility evidence (`bootstrap/stage0_seed.lock`).
//!
//! There is no self-hosted compiler yet, so §42.2 Stage 1-3 artifacts do not
//! exist. What can be checked today is the Rust seed: build it twice from
//! clean target directories, compile a fixed corpus with each build and require
//! bit-identical objects; and run the corpus through the Lean reference built
//! with a non-Rust C compiler (Path B) and require the same results as the
//! Path A executables. This module holds that record and its checks.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Schema label of the seed lock.
pub const SEED_SCHEMA: &str = "vesper.bootstrap.stage0-seed/1";
/// Largest accepted seed lock or results transcript.
pub const MAX_SEED_BYTES: usize = 256 * 1024;
/// Largest corpus the driver accepts.
pub const MAX_CORPUS_ENTRIES: usize = 1024;

/// Failure of a seed-reproducibility check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SeedError {
    TooLarge {
        len: usize,
        max: usize,
    },
    Syntax {
        message: String,
    },
    InvalidField {
        field: &'static str,
        reason: &'static str,
    },
    /// Two corpus manifests list different programs.
    CorpusShapeMismatch,
    /// The two seed builds compiled a program to different objects.
    ObjectNotReproducible {
        program: String,
    },
    /// A program's Path A and Path B results differ.
    PathResultMismatch {
        program: String,
        path_a: String,
        path_b: String,
    },
    /// A recorded flag contradicts its digests.
    InconsistentRecord {
        field: &'static str,
    },
}

impl fmt::Display for SeedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SeedError::TooLarge { len, max } => write!(f, "{len} bytes exceeds {max}"),
            SeedError::Syntax { message } => write!(f, "syntax: {message}"),
            SeedError::InvalidField { field, reason } => write!(f, "{field}: {reason}"),
            SeedError::CorpusShapeMismatch => {
                f.write_str("corpus manifests list different programs")
            }
            SeedError::ObjectNotReproducible { program } => {
                write!(f, "{program}: objects from the two seed builds differ")
            }
            SeedError::PathResultMismatch {
                program,
                path_a,
                path_b,
            } => write!(f, "{program}: Path A printed {path_a:?}, Path B {path_b:?}"),
            SeedError::InconsistentRecord { field } => {
                write!(f, "{field} contradicts the recorded digests")
            }
        }
    }
}
impl std::error::Error for SeedError {}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-.".contains(&b))
        && !name.starts_with('.')
}

/// One compiled program: its name and object digest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectEntry {
    pub program: String,
    pub object: blake3::Hash,
}

/// Digests of every corpus object produced by one seed build.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectManifest {
    pub entries: Vec<ObjectEntry>,
}

impl ObjectManifest {
    /// Build from `(program, object bytes)`; names must be unique labels.
    pub fn from_objects(objects: &[(&str, &[u8])]) -> Result<Self, SeedError> {
        if objects.len() > MAX_CORPUS_ENTRIES {
            return Err(SeedError::TooLarge {
                len: objects.len(),
                max: MAX_CORPUS_ENTRIES,
            });
        }
        let mut entries: Vec<_> = objects
            .iter()
            .map(|(name, bytes)| {
                if valid_name(name) {
                    Ok(ObjectEntry {
                        program: (*name).to_owned(),
                        object: blake3::hash(bytes),
                    })
                } else {
                    Err(SeedError::InvalidField {
                        field: "program",
                        reason: "program names are bounded [A-Za-z0-9_.-] labels",
                    })
                }
            })
            .collect::<Result<_, _>>()?;
        entries.sort_by(|a, b| a.program.cmp(&b.program));
        if entries.windows(2).any(|w| w[0].program == w[1].program) {
            return Err(SeedError::InvalidField {
                field: "program",
                reason: "duplicate program",
            });
        }
        Ok(Self { entries })
    }

    /// Domain-separated digest of the sorted entries.
    pub fn digest(&self) -> blake3::Hash {
        let mut h = blake3::Hasher::new();
        h.update(b"vesper.bootstrap.object-manifest.v1\0");
        h.update(&(self.entries.len() as u64).to_le_bytes());
        for e in &self.entries {
            h.update(&(e.program.len() as u64).to_le_bytes());
            h.update(e.program.as_bytes());
            h.update(e.object.as_bytes());
        }
        h.finalize()
    }

    /// Require the same programs with bit-identical objects.
    pub fn require_identical(&self, other: &Self) -> Result<(), SeedError> {
        if self.entries.len() != other.entries.len()
            || self
                .entries
                .iter()
                .zip(&other.entries)
                .any(|(a, b)| a.program != b.program)
        {
            return Err(SeedError::CorpusShapeMismatch);
        }
        match self
            .entries
            .iter()
            .zip(&other.entries)
            .find(|(a, b)| a.object != b.object)
        {
            Some((a, _)) => Err(SeedError::ObjectNotReproducible {
                program: a.program.clone(),
            }),
            None => Ok(()),
        }
    }
}

/// `program result` lines, one per corpus program, in program order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResultTranscript {
    pub lines: Vec<(String, String)>,
}

impl ResultTranscript {
    /// Parse `name<space>result` lines. Results are bounded printable ASCII.
    pub fn parse(bytes: &[u8]) -> Result<Self, SeedError> {
        if bytes.len() > MAX_SEED_BYTES {
            return Err(SeedError::TooLarge {
                len: bytes.len(),
                max: MAX_SEED_BYTES,
            });
        }
        let text = std::str::from_utf8(bytes).map_err(|_| SeedError::Syntax {
            message: "transcript is not UTF-8".into(),
        })?;
        let mut lines = Vec::new();
        for line in text.lines() {
            let (name, result) = line.split_once(' ').ok_or(SeedError::Syntax {
                message: "expected `program result`".into(),
            })?;
            if !valid_name(name)
                || result.is_empty()
                || result.len() > 64
                || !result.bytes().all(|b| b.is_ascii_graphic())
            {
                return Err(SeedError::InvalidField {
                    field: "transcript",
                    reason: "bad program name or result",
                });
            }
            lines.push((name.to_owned(), result.to_owned()));
        }
        if lines.len() > MAX_CORPUS_ENTRIES {
            return Err(SeedError::TooLarge {
                len: lines.len(),
                max: MAX_CORPUS_ENTRIES,
            });
        }
        lines.sort();
        if lines.windows(2).any(|w| w[0].0 == w[1].0) {
            return Err(SeedError::InvalidField {
                field: "transcript",
                reason: "duplicate program",
            });
        }
        Ok(Self { lines })
    }

    /// Canonical bytes: sorted `name result\n` lines.
    pub fn canonical(&self) -> String {
        self.lines
            .iter()
            .map(|(n, r)| format!("{n} {r}\n"))
            .collect()
    }

    pub fn digest(&self) -> blake3::Hash {
        blake3::hash(self.canonical().as_bytes())
    }

    /// Path A (seed executables) and Path B (Lean reference) must agree on
    /// every program and list the same programs.
    pub fn require_agreement(&self, path_b: &Self) -> Result<(), SeedError> {
        if self.lines.len() != path_b.lines.len()
            || self
                .lines
                .iter()
                .zip(&path_b.lines)
                .any(|(a, b)| a.0 != b.0)
        {
            return Err(SeedError::CorpusShapeMismatch);
        }
        match self
            .lines
            .iter()
            .zip(&path_b.lines)
            .find(|(a, b)| a.1 != b.1)
        {
            Some((a, b)) => Err(SeedError::PathResultMismatch {
                program: a.0.clone(),
                path_a: a.1.clone(),
                path_b: b.1.clone(),
            }),
            None => Ok(()),
        }
    }
}

/// `[seed]` table: how the two builds were made.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SeedBuild {
    pub schema: String,
    pub base_commit: String,
    pub rustc: String,
    pub cargo_profile: String,
    pub source_date_epoch: u64,
    pub corpus_programs: u32,
}

/// `[reproducibility]` table: the two seed builds compared.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reproducibility {
    pub seed_binary_a: String,
    pub seed_binary_b: String,
    pub seed_binary_identical: bool,
    pub corpus_objects_a: String,
    pub corpus_objects_b: String,
    pub corpus_objects_identical: bool,
}

/// `[ddc_semantic]` table: Path A executables against the Lean reference.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DdcSemantic {
    pub path_a_results: String,
    pub path_b_results: String,
    pub path_b_lean: String,
    pub path_b_c_compiler: String,
    pub results_agree: bool,
}

/// The whole `bootstrap/stage0_seed.lock`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SeedLock {
    pub seed: SeedBuild,
    pub reproducibility: Reproducibility,
    pub ddc_semantic: DdcSemantic,
}

fn digest_field(text: &str, field: &'static str) -> Result<blake3::Hash, SeedError> {
    crate::Blake3Digest::parse_prefixed(text, "blake3:", field)
        .map(|d| d.0)
        .map_err(|_| SeedError::InvalidField {
            field,
            reason: "expected blake3:<64 lowercase hex>",
        })
}

fn short_label(text: &str, field: &'static str) -> Result<(), SeedError> {
    if text.is_empty() || text.len() > 128 || !text.bytes().all(|b| (0x20..0x7f).contains(&b)) {
        return Err(SeedError::InvalidField {
            field,
            reason: "bounded printable ASCII required",
        });
    }
    Ok(())
}

impl SeedLock {
    /// Parse and check internal consistency: flags must match the digests,
    /// and the gate fields (corpus objects, result agreement) must hold.
    pub fn parse(bytes: &[u8]) -> Result<Self, SeedError> {
        if bytes.len() > MAX_SEED_BYTES {
            return Err(SeedError::TooLarge {
                len: bytes.len(),
                max: MAX_SEED_BYTES,
            });
        }
        let text = std::str::from_utf8(bytes).map_err(|_| SeedError::Syntax {
            message: "seed lock is not UTF-8".into(),
        })?;
        let lock: SeedLock = toml::from_str(text).map_err(|e| SeedError::Syntax {
            message: e.message().to_owned(),
        })?;
        lock.validate()?;
        Ok(lock)
    }

    pub fn validate(&self) -> Result<(), SeedError> {
        let s = &self.seed;
        if s.schema != SEED_SCHEMA {
            return Err(SeedError::InvalidField {
                field: "schema",
                reason: "unsupported seed lock schema",
            });
        }
        if s.base_commit.len() != 40 || !s.base_commit.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(SeedError::InvalidField {
                field: "base_commit",
                reason: "expected a 40-digit commit",
            });
        }
        short_label(&s.rustc, "rustc")?;
        short_label(&s.cargo_profile, "cargo_profile")?;
        if s.corpus_programs == 0 || s.corpus_programs as usize > MAX_CORPUS_ENTRIES {
            return Err(SeedError::InvalidField {
                field: "corpus_programs",
                reason: "corpus must be nonempty and bounded",
            });
        }
        let r = &self.reproducibility;
        let ba = digest_field(&r.seed_binary_a, "seed_binary_a")?;
        let bb = digest_field(&r.seed_binary_b, "seed_binary_b")?;
        if (ba == bb) != r.seed_binary_identical {
            return Err(SeedError::InconsistentRecord {
                field: "seed_binary_identical",
            });
        }
        let oa = digest_field(&r.corpus_objects_a, "corpus_objects_a")?;
        let ob = digest_field(&r.corpus_objects_b, "corpus_objects_b")?;
        if (oa == ob) != r.corpus_objects_identical {
            return Err(SeedError::InconsistentRecord {
                field: "corpus_objects_identical",
            });
        }
        if !r.corpus_objects_identical {
            return Err(SeedError::ObjectNotReproducible {
                program: "<corpus>".into(),
            });
        }
        let d = &self.ddc_semantic;
        let pa = digest_field(&d.path_a_results, "path_a_results")?;
        let pb = digest_field(&d.path_b_results, "path_b_results")?;
        short_label(&d.path_b_lean, "path_b_lean")?;
        short_label(&d.path_b_c_compiler, "path_b_c_compiler")?;
        if (pa == pb) != d.results_agree {
            return Err(SeedError::InconsistentRecord {
                field: "results_agree",
            });
        }
        if !d.results_agree {
            return Err(SeedError::PathResultMismatch {
                program: "<corpus>".into(),
                path_a: d.path_a_results.clone(),
                path_b: d.path_b_results.clone(),
            });
        }
        Ok(())
    }

    pub fn to_toml(&self) -> String {
        let mut out = String::from(
            "# bootstrap/stage0_seed.lock — Stage 0 seed reproducibility and Path B results.\n\
             # Written by bootstrap/run.sh; checked by crates/vesper-codegen-verified.\n",
        );
        out.push_str(&toml::to_string(self).expect("seed lock serializes"));
        out
    }
}
