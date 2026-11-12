//! Conservative release-version policy over a freshly computed interface report.
//!
//! Policy 1 compares numeric version components uniformly, including before
//! 1.0.0: a breaking change requires increasing the numeric major component.
//! Prerelease versions are outside this policy. Build metadata never increases
//! precedence. Acceptance permits a declared release bump; it does not assert
//! source compatibility, package provenance, or permission to publish.
use crate::{Classification, Interface, Report, compare};
use semver::Version;
use serde::{Deserialize, Serialize};
use std::{cmp::Ordering, fmt};

/// Version of the release-report schema and its conservative bump policy.
pub const RELEASE_POLICY_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseBump {
    Patch,
    Minor,
    Major,
}
impl fmt::Display for ReleaseBump {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Patch => "patch",
            Self::Minor => "minor",
            Self::Major => "major",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseIssueCode {
    UnknownInterface,
    PrereleaseUnsupported,
    NonIncreasingVersion,
    InsufficientBump,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseIssue {
    pub code: ReleaseIssueCode,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseReport {
    pub policy_version: u32,
    pub old_version: String,
    pub new_version: String,
    /// The actual comparison, including breaking issues and explicit adapters.
    pub comparison: Report,
    pub required_bump: Classification,
    /// Numeric component increase, or None for an unchanged/decreased core.
    /// Prerelease versions may show a numeric bump but are still rejected.
    pub actual_bump: Option<ReleaseBump>,
    /// Permits this version bump, not a claim that old callers still compile.
    pub accepted: bool,
    pub issues: Vec<ReleaseIssue>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseVersionSide {
    Old,
    New,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseVersionError {
    pub side: ReleaseVersionSide,
    pub version: String,
    pub message: String,
}
impl fmt::Display for ReleaseVersionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let side = match self.side {
            ReleaseVersionSide::Old => "old",
            ReleaseVersionSide::New => "new",
        };
        write!(
            f,
            "invalid {side} release version '{}': {}",
            self.version, self.message
        )
    }
}
impl std::error::Error for ReleaseVersionError {}

fn version(text: &str, side: ReleaseVersionSide) -> Result<Version, ReleaseVersionError> {
    Version::parse(text).map_err(|error| ReleaseVersionError {
        side,
        version: text.into(),
        message: error.to_string(),
    })
}

/// Check the proposed version change against a newly computed comparison.
///
/// Unknown interfaces never pass, even with a major bump. Known breaking changes
/// may pass a major bump while the retained comparison remains incompatible and
/// still lists required call adapters. Higher-than-required bumps are allowed.
/// Pre-1.0 versions receive no automatic breaking-change exception under policy 1.
pub fn check_release(
    old: &Interface,
    new: &Interface,
    old_version: &str,
    new_version: &str,
) -> Result<ReleaseReport, ReleaseVersionError> {
    let before = version(old_version, ReleaseVersionSide::Old)?;
    let after = version(new_version, ReleaseVersionSide::New)?;
    let comparison = compare(old, new);
    let required_bump = comparison.classification;
    let required = match required_bump {
        Classification::Patch => Some(ReleaseBump::Patch),
        Classification::Minor => Some(ReleaseBump::Minor),
        Classification::Major => Some(ReleaseBump::Major),
        Classification::Unknown => None,
    };
    let actual_bump = if after.major > before.major {
        Some(ReleaseBump::Major)
    } else if after.major == before.major && after.minor > before.minor {
        Some(ReleaseBump::Minor)
    } else if after.major == before.major
        && after.minor == before.minor
        && after.patch > before.patch
    {
        Some(ReleaseBump::Patch)
    } else {
        None
    };
    let mut issues = vec![];
    if required.is_none() {
        issues.push(ReleaseIssue {
            code: ReleaseIssueCode::UnknownInterface,
            message: "unsupported or unknown interface contracts cannot certify a release".into(),
        });
    }
    if !before.pre.is_empty() || !after.pre.is_empty() {
        issues.push(ReleaseIssue {
            code: ReleaseIssueCode::PrereleaseUnsupported,
            message: "release policy 1 does not classify prerelease version transitions".into(),
        });
    }
    if after.cmp_precedence(&before) != Ordering::Greater {
        issues.push(ReleaseIssue {
            code: ReleaseIssueCode::NonIncreasingVersion,
            message: "candidate precedence must increase; build metadata is ignored".into(),
        });
    }
    if let (Some(actual), Some(required)) = (actual_bump, required)
        && actual < required
    {
        issues.push(ReleaseIssue {
            code: ReleaseIssueCode::InsufficientBump,
            message: format!(
                "interface change requires a {required} bump; candidate has a {actual} bump"
            ),
        });
    }
    Ok(ReleaseReport {
        policy_version: RELEASE_POLICY_VERSION,
        old_version: before.to_string(),
        new_version: after.to_string(),
        comparison,
        required_bump,
        actual_bump,
        accepted: issues.is_empty(),
        issues,
    })
}
