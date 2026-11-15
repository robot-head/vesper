//! `vesper highlight --json`: the wire records as JSON (schema
//! `vesper.chroma.spans/1`), plus the hover table and the records' bytes in
//! hexadecimal, so a reader can check the 16-byte encoding directly.
use crate::{ChromaError, Highlight, Tier, wire};
use std::fmt::Write as _;

pub const SCHEMA: &str = "vesper.chroma.spans/1";

/// Appends `s` as a JSON string literal.
pub fn string_into(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

pub fn render(h: &Highlight, theme: &str) -> Result<String, ChromaError> {
    let bytes = wire::encode(&h.spans)?;
    let mut out = String::with_capacity(h.spans.len() * 110 + bytes.len() * 2 + 128);
    out.push_str("{\"schema\":");
    string_into(SCHEMA, &mut out);
    out.push_str(",\"theme\":");
    string_into(theme, &mut out);
    let _ = write!(
        out,
        ",\"tier\":{}",
        match h.tier {
            Tier::Lossless0 => 0,
            Tier::Semantic1 => 1,
        }
    );
    out.push_str(",\"fallback\":");
    match &h.fallback {
        None => out.push_str("null"),
        Some(reason) => {
            out.push_str("{\"kind\":");
            string_into(reason.kind(), &mut out);
            out.push_str(",\"code\":");
            string_into(reason.code(), &mut out);
            if let crate::FallbackReason::Syntax { message, span, .. }
            | crate::FallbackReason::Check { message, span, .. } = reason
            {
                out.push_str(",\"message\":");
                string_into(message, &mut out);
                let _ = write!(out, ",\"start\":{},\"end\":{}", span.start, span.end);
            }
            if let crate::FallbackReason::Budget { message } = reason {
                out.push_str(",\"message\":");
                string_into(message, &mut out);
            }
            out.push('}');
        }
    }
    out.push_str(",\"spans\":[");
    let (records, _) = bytes.as_chunks::<{ wire::RECORD_BYTES }>();
    for (i, rec) in records.iter().enumerate() {
        let w = wire::ChromaTokenSpanWire::from_bytes(rec);
        if i > 0 {
            out.push(',');
        }
        let _ = write!(
            out,
            "{{\"byte_start\":{},\"byte_len\":{},\"token_class\":{},\"class\":\"{}\",\"semantic_flags\":{},\"hover_sym_id\":",
            w.byte_start,
            w.byte_len,
            w.token_class,
            h.spans[i].class.name(),
            w.semantic_flags
        );
        if w.hover_sym_id == wire::NO_HOVER {
            out.push_str("null}");
        } else {
            let _ = write!(out, "{}}}", w.hover_sym_id);
        }
    }
    out.push_str("],\"hovers\":[");
    for (i, row) in h.hovers.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let _ = write!(
            out,
            "{{\"start\":{},\"end\":{},\"text\":",
            row.span.start, row.span.end
        );
        string_into(&row.text, &mut out);
        out.push('}');
    }
    out.push_str("],\"wire_hex\":\"");
    for b in &bytes {
        let _ = write!(out, "{b:02x}");
    }
    out.push_str("\"}\n");
    Ok(out)
}
