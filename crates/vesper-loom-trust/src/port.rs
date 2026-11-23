//! The conversions between the crate's types and the export ABI of the
//! Vesper package (`vesper/src/lib.ves`): attestation values become its
//! trees, the catalog one text pool, and the results come back as the
//! crate's reports.

use std::collections::BTreeSet;

use crate::slopsquat::{
    CatalogEntry, QuarantineDecision, QuarantineFinding, QuarantinePolicy, QuarantineReason,
    QuarantineReport, SLOPSQUAT_SCHEMA_VERSION,
};
use crate::{
    AttestationBundle, BuildIdentity, BuilderPolicy, Diagnostic, SCHEMA_VERSION, Statement,
    VerificationReport,
};
use vesper_loom_calculus::PackageName;

// Generated and committed, but kept outside src/ so formatters, which
// check every file under src/, never reflow it (see the porting recipe).
#[allow(unsafe_code, missing_docs, dead_code, clippy::all)]
#[rustfmt::skip]
#[path = "../port/binding.rs"]
mod binding;

use binding as b;

fn identity(i: &BuildIdentity) -> b::IdentityTree {
    b::IdentityTree {
        package: i.package.as_str().to_owned(),
        version: i.version.clone(),
        source_tree: i.source_tree.clone(),
        derivation: i.derivation.clone(),
        target: i.target.clone(),
        builder_blake3: i.builder_blake3.clone(),
        sandbox_contract: i.sandbox_contract.clone(),
        output: i.output.clone(),
    }
}

fn statement(s: &Statement) -> b::StatementTree {
    b::StatementTree {
        schema_version: s.schema_version,
        key_id: s.key_id.clone(),
        identity: identity(&s.identity),
        issued_at: s.issued_at,
        expires_at: s.expires_at,
    }
}

pub(crate) fn signing_bytes(s: &Statement) -> Result<Vec<u8>, String> {
    let t = b::signing_bytes(&b::Statement::encode(&statement(s))).decode();
    if t.error.is_empty() {
        Ok(t.bytes)
    } else {
        Err(t.error)
    }
}

pub(crate) fn verify(
    expected: &BuildIdentity,
    bundle: &AttestationBundle,
    policy: &BuilderPolicy,
    now: u64,
) -> VerificationReport {
    let e = b::Identity::encode(&identity(expected));
    let bun = b::Bundle::encode(&b::BundleTree {
        schema_version: bundle.schema_version,
        attestations: bundle
            .attestations
            .iter()
            .map(|a| b::EnvelopeTree {
                statement: statement(&a.statement),
                signature: a.signature.clone(),
            })
            .collect(),
    });
    let pol = b::Policy::encode(&b::PolicyTree {
        schema_version: policy.schema_version,
        minimum_operators: policy.minimum_operators as u64,
        max_statement_ttl: policy.max_statement_ttl,
        builders: policy
            .builders
            .iter()
            .map(|t| b::BuilderTree {
                key_id: t.key_id.clone(),
                operator_id: t.operator_id.clone(),
                public_key: t.public_key.clone(),
                valid_from: t.valid_from,
                valid_until: t.valid_until,
                revoked: t.revoked,
            })
            .collect(),
    });
    let r = b::verify(&e, &bun, &pol, now).decode();
    let names = |v: Vec<b::NameTree>| v.into_iter().map(|n| n.text).collect::<BTreeSet<_>>();
    VerificationReport {
        schema_version: SCHEMA_VERSION,
        accepted: r.accepted,
        evaluated_at: now,
        minimum_operators: policy.minimum_operators,
        verified_keys: names(r.verified_keys),
        verified_operators: names(r.verified_operators),
        diagnostics: r
            .diagnostics
            .into_iter()
            .map(|d| Diagnostic {
                code: d.code,
                key_id: d.key_id,
                field: d.field,
                message: d.message,
            })
            .collect(),
    }
}

const NONE: u64 = u64::MAX;

fn size(v: u64) -> usize {
    usize::try_from(v).unwrap_or(usize::MAX)
}

pub(crate) fn split(text: &str) -> [usize; 4] {
    let p = b::split_package(text);
    [size(p[0]), size(p[1]), size(p[2]), size(p[3])]
}

pub(crate) fn conflation_key(text: &str) -> String {
    b::conflation_key(text)
}

pub(crate) fn damerau_levenshtein(a: &str, b: &str) -> usize {
    size(b::damerau_levenshtein(a, b))
}

const REASONS: [QuarantineReason; 8] = [
    QuarantineReason::MissingFromCatalog,
    QuarantineReason::OversizedPackageName,
    QuarantineReason::EditDistanceTyposquat,
    QuarantineReason::IdentifierConflation,
    QuarantineReason::StdNamespaceImpersonation,
    QuarantineReason::ImmatureNamespace,
    QuarantineReason::ExcessiveTrustCost,
    QuarantineReason::MissingOidcProvenance,
];

const DECISIONS: [QuarantineDecision; 3] = [
    QuarantineDecision::Admit,
    QuarantineDecision::Quarantined,
    QuarantineDecision::Rejected,
];

pub(crate) fn screen(
    candidate: &PackageName,
    catalog: &[CatalogEntry],
    policy: &QuarantinePolicy,
    now: u64,
) -> QuarantineReport {
    let mut names = String::with_capacity(catalog.iter().map(|e| e.package.as_str().len()).sum());
    let mut ends = Vec::with_capacity(catalog.len());
    let mut meta = Vec::with_capacity(catalog.len() * 3);
    for e in catalog {
        names.push_str(e.package.as_str());
        ends.push(names.len() as u64);
        meta.extend([
            e.org_created_at,
            e.trust_cost.into(),
            e.attested_oidc.into(),
        ]);
    }
    let thresholds = b::repr::Thresholds {
        max_edit_distance: policy.max_edit_distance as u64,
        min_namespace_age_secs: policy.min_namespace_age_secs,
        max_trust_cost: vesper_port_abi::Word::new(policy.max_trust_cost),
        require_oidc_provenance: vesper_port_abi::Word::new(policy.require_oidc_provenance),
        agent_mode: vesper_port_abi::Word::new(policy.agent_mode),
    };
    let s = b::screen(candidate.as_str(), &names, &ends, &meta, &thresholds, now).decode();
    QuarantineReport {
        schema_version: SLOPSQUAT_SCHEMA_VERSION,
        candidate: candidate.clone(),
        decision: DECISIONS[size(s.decision)],
        findings: s
            .findings
            .into_iter()
            .map(|f| QuarantineFinding {
                reason: REASONS[size(f.reason)],
                similar_to: (f.similar_to != NONE)
                    .then(|| catalog[size(f.similar_to)].package.clone()),
                edit_distance: (f.edit_distance != NONE).then(|| size(f.edit_distance)),
                message: f.message,
            })
            .collect(),
    }
}
