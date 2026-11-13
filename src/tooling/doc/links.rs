//! Link analysis is attached only to metadata extracted after real checking.
use vesper_doc::{Document, Limits, links};

pub(super) fn populate(
    document: &mut Document,
    source: &str,
    limits: Limits,
) -> Result<(), String> {
    if document.source_bytes != source.len() {
        return Err("documentation link source association mismatch".into());
    }
    let link_limits = links::LinkLimits {
        max_source_bytes: limits.max_source_bytes,
        ..Default::default()
    };
    let targets = links::document_targets(document, link_limits).map_err(|e| e.to_string())?;
    let report = links::analyze(source, &targets, link_limits).map_err(|e| e.to_string())?;
    if !matches!(report.status, links::LinkStatus::Resolved) {
        return Err(if let Some(diagnostic) = report.diagnostics.first() {
            format!(
                "intra-doc link {} at bytes {}..{}: {}",
                diagnostic.code, diagnostic.span.start, diagnostic.span.end, diagnostic.message
            )
        } else {
            "intra-doc links were not resolved".into()
        });
    }
    links::validate_report(&report, &targets, link_limits).map_err(|e| e.to_string())?;
    document.intra_doc_links = report;
    Ok(())
}
