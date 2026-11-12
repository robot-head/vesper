//! Strict structural search over the caller's current checked open buffers.
use crate::tooling::{doc::document_checked, frontend::CheckedSource};
use serde_json::{Value, json};

const MAX_DOCUMENTS: usize = 32;
const MAX_SOURCE_BYTES: usize = 2 * 1024 * 1024;
const MAX_OUTPUT_BYTES: usize = 1024 * 1024;

#[derive(Debug)]
pub(crate) enum SearchError {
    Query(String),
    Unavailable { uri: Option<String>, reason: String },
}
impl SearchError {
    fn unavailable(uri: Option<&str>, reason: impl Into<String>) -> Self {
        Self::Unavailable {
            uri: uri.map(str::to_owned),
            reason: reason.into(),
        }
    }
}

pub(crate) fn signature_search(
    documents: &[(&str, &CheckedSource)],
    query: &Value,
) -> Result<Value, SearchError> {
    if documents.len() > MAX_DOCUMENTS {
        return Err(SearchError::unavailable(
            None,
            "signature search document limit exceeded",
        ));
    }
    query_budget(query).map_err(SearchError::Query)?;
    // Check size while serializing. The protocol rejects duplicate decoded keys;
    // the leaf reader additionally validates every nested contract field.
    let bytes = vesper_doc::to_json(query, 32_768).map_err(SearchError::Query)?;
    let text = std::str::from_utf8(&bytes).map_err(|e| SearchError::Query(e.to_string()))?;
    let limits = vesper_doc::Limits {
        max_output_bytes: MAX_OUTPUT_BYTES,
        ..Default::default()
    };
    let query = vesper_doc::parse_query_json(text, limits).map_err(SearchError::Query)?;
    // Validate unsupported query contracts even with no open documents.
    vesper_doc::search(&query, &[], limits).map_err(SearchError::Query)?;
    let mut total_source = 0usize;
    let mut remaining_output = MAX_OUTPUT_BYTES - 1024;
    let mut results = Vec::new();
    for (uri, snapshot) in documents {
        if uri.len() > 4096 {
            return Err(SearchError::unavailable(
                None,
                "signature search URI limit exceeded",
            ));
        }
        total_source = total_source
            .checked_add(snapshot.source().len())
            .filter(|bytes| *bytes <= MAX_SOURCE_BYTES)
            .ok_or_else(|| {
                SearchError::unavailable(None, "signature search total source limit exceeded")
            })?;
        let document = document_checked(snapshot, limits)
            .map_err(|reason| SearchError::unavailable(Some(uri), reason))?;
        let result = vesper_doc::search(&query, &document.functions, limits)
            .map_err(|reason| SearchError::unavailable(Some(uri), reason))?;
        // Keep unsupported_functions instead of claiming exhaustive matches.
        let value = json!({
            "uri": uri,
            "source_blake3": blake3::Hash::from_bytes(snapshot.source_hash()).to_hex().to_string(),
            "result": result,
        });
        let encoded = vesper_doc::to_json(&value, remaining_output)
            .map_err(|reason| SearchError::unavailable(Some(uri), reason))?;
        remaining_output = remaining_output
            .checked_sub(encoded.len() + 8)
            .ok_or_else(|| {
                SearchError::unavailable(None, "signature search output limit exceeded")
            })?;
        results.push(value);
    }
    let value = json!({
        "scope": "open checked buffers",
        "documents": results,
        "proof_status": "unverified",
    });
    vesper_doc::to_json(&value, MAX_OUTPUT_BYTES)
        .map_err(|reason| SearchError::unavailable(None, reason))?;
    Ok(value)
}

// Normally supplied by the bounded protocol reader; keep the internal API safe
// for direct callers too before recursive Serde serialization.
fn query_budget(value: &Value) -> Result<(), String> {
    let mut pending = vec![(value, 0usize)];
    let mut nodes = 8192usize;
    let mut strings = 32_768usize;
    while let Some((value, depth)) = pending.pop() {
        nodes = nodes
            .checked_sub(1)
            .ok_or("search query node limit exceeded")?;
        if depth > 64 {
            return Err("search query depth limit exceeded".into());
        }
        let children = match value {
            Value::Array(values) => values.len(),
            Value::Object(values) => values.len(),
            _ => 0,
        };
        if children > nodes.saturating_sub(pending.len()) {
            return Err("search query pending node limit exceeded".into());
        }
        match value {
            Value::String(text) => {
                strings = strings
                    .checked_sub(text.len())
                    .ok_or("search query string limit exceeded")?;
            }
            Value::Array(values) => pending.extend(values.iter().map(|value| (value, depth + 1))),
            Value::Object(values) => {
                for (key, value) in values {
                    strings = strings
                        .checked_sub(key.len())
                        .ok_or("search query string limit exceeded")?;
                    pending.push((value, depth + 1));
                }
            }
            _ => (),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tooling::frontend::{AnalysisLimits, check_source};

    fn query() -> Value {
        json!({"parameters": [], "result": {"Named": ["u64", []]},
            "result_max_mode": "Copy", "effect_ceiling": {"instances": [], "tail": null}})
    }

    #[test]
    fn searches_actual_checked_contracts_and_keeps_source_identity() {
        let snapshot =
            check_source("fn answer() -> u64 { 42 }", AnalysisLimits::default()).unwrap();
        let result = signature_search(&[("untitled:answer", &snapshot)], &query()).unwrap();
        assert_eq!(
            result["documents"][0]["result"]["matches"][0]["function"],
            "answer"
        );
        assert_eq!(
            result["documents"][0]["source_blake3"]
                .as_str()
                .unwrap()
                .len(),
            64
        );
        assert_eq!(result["proof_status"], "unverified");
        let mut wrong = query();
        wrong["result"] = json!({"Named": ["i64", []]});
        assert!(signature_search(&[("untitled:answer", &snapshot)], &wrong).unwrap()["documents"][0]["result"]["matches"].as_array().unwrap().is_empty());
    }

    #[test]
    fn rejects_unknown_nested_fields_and_unsupported_empty_workspace_query() {
        let mut wrong = query();
        wrong["effect_ceiling"]["verified"] = json!(true);
        assert!(signature_search(&[], &wrong).is_err());
        let mut open = query();
        open["effect_ceiling"]["tail"] = json!("r");
        assert!(signature_search(&[], &open).is_err());
        assert!(
            signature_search(&[], &query()).unwrap()["documents"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn direct_query_values_are_guarded_before_recursive_serialization() {
        let mut deep = Value::Null;
        for _ in 0..80 {
            deep = Value::Array(vec![deep]);
        }
        assert!(
            matches!(signature_search(&[], &deep), Err(SearchError::Query(reason)) if reason.contains("depth"))
        );
        assert!(signature_search(&[], &json!("x".repeat(32_769))).is_err());
        assert!(signature_search(&[], &Value::Array(vec![Value::Null; 8192])).is_err());
    }

    #[test]
    fn checked_document_extraction_failure_is_not_a_malformed_query() {
        let source: String = (0..300)
            .map(|n| format!("copy struct S{n}{{}}\n"))
            .collect();
        let snapshot = check_source(&source, AnalysisLimits::default()).unwrap();
        assert!(matches!(
            signature_search(&[("opaque:many-types", &snapshot)], &query()),
            Err(SearchError::Unavailable { uri: Some(uri), .. }) if uri == "opaque:many-types"
        ));
        assert!(matches!(
            signature_search(&[], &json!({})),
            Err(SearchError::Query(_))
        ));
    }
}
