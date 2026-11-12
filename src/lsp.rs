//! Minimal stdio Language Server: full/incremental text sync and UTF-16 diagnostics.
pub mod navigation;
pub mod actions;
use crate::database::{self, Database, SourceFile};
use salsa::Setter;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::{BufRead, Write},
};
pub fn serve(reader: impl BufRead, mut writer: impl Write) -> Result<(), String> {
    let mut reader = reader;
    let mut db = Database::default();
    let mut files: BTreeMap<String, SourceFile> = BTreeMap::new();
    let mut shutdown = false;
    loop {
        let mut length = None;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).map_err(|e| e.to_string())? == 0 {
                return Ok(());
            }
            if line.trim().is_empty() {
                break;
            }
            if let Some((k, v)) = line.split_once(':')
                && k.eq_ignore_ascii_case("content-length")
            {
                length = Some(
                    v.trim()
                        .parse::<usize>()
                        .map_err(|_| "invalid Content-Length")?,
                );
            }
        }
        let length = length.ok_or("missing Content-Length")?;
        if length > 16 * 1024 * 1024 {
            return Err("LSP message exceeds 16 MiB".into());
        }
        let mut bytes = vec![0; length];
        reader.read_exact(&mut bytes).map_err(|e| e.to_string())?;
        let msg: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        let method = msg["method"].as_str().unwrap_or("");
        let id = msg.get("id").cloned();
        let response = match method {
            "initialize" => Some(
                json!({"capabilities":{"textDocumentSync":{"openClose":true,"change":2}},"serverInfo":{"name":"vesper","version":env!("CARGO_PKG_VERSION")}}),
            ),
            "shutdown" => {
                shutdown = true;
                Some(Value::Null)
            }
            "exit" => {
                return if shutdown {
                    Ok(())
                } else {
                    Err("LSP exit before shutdown".into())
                };
            }
            "textDocument/didOpen" | "textDocument/didChange" => {
                let doc = &msg["params"]["textDocument"];
                let uri = doc["uri"]
                    .as_str()
                    .ok_or("missing document URI")?
                    .to_owned();
                let file = if let Some(f) = files.get(&uri) {
                    *f
                } else {
                    let f = SourceFile::new(&db, String::new());
                    files.insert(uri.clone(), f);
                    f
                };
                let text = if method.ends_with("didOpen") {
                    doc["text"]
                        .as_str()
                        .ok_or("missing document text")?
                        .to_owned()
                } else {
                    let mut text = file.text(&db).clone();
                    for change in msg["params"]["contentChanges"]
                        .as_array()
                        .ok_or("missing content changes")?
                    {
                        let replacement = change["text"].as_str().ok_or("missing change text")?;
                        if let Some(range) = change.get("range") {
                            let start = offset(&text, &range["start"])?;
                            let end = offset(&text, &range["end"])?;
                            if start > end {
                                return Err("reversed edit range".into());
                            }
                            text.replace_range(start..end, replacement);
                        } else {
                            text = replacement.to_owned();
                        }
                    }
                    text
                };
                file.set_text(&mut db).to(text.clone());
                let diagnostics=database::diagnostics(&db,file).iter().map(|d|json!({"range":{"start":position(&text,d.span.start),"end":position(&text,d.span.end)},"severity":1,"code":d.code,"source":"vesper","message":d.message})).collect::<Vec<_>>();
                send(
                    &mut writer,
                    &json!({"jsonrpc":"2.0","method":"textDocument/publishDiagnostics","params":{"uri":uri,"version":doc["version"],"diagnostics":diagnostics}}),
                )?;
                None
            }
            "textDocument/didClose" => {
                if let Some(uri) = msg["params"]["textDocument"]["uri"].as_str() {
                    files.remove(uri);
                    send(
                        &mut writer,
                        &json!({"jsonrpc":"2.0","method":"textDocument/publishDiagnostics","params":{"uri":uri,"diagnostics":[]}}),
                    )?;
                }
                None
            }
            "initialized" | "$/cancelRequest" | "$/setTrace" => None,
            _ => {
                if let Some(id) = id {
                    send(
                        &mut writer,
                        &json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"Method not found"}}),
                    )?;
                }
                continue;
            }
        };
        if let (Some(id), Some(result)) = (id, response) {
            send(
                &mut writer,
                &json!({"jsonrpc":"2.0","id":id,"result":result}),
            )?;
        }
    }
}
fn send(w: &mut impl Write, v: &Value) -> Result<(), String> {
    let bytes = serde_json::to_vec(v).map_err(|e| e.to_string())?;
    write!(w, "Content-Length: {}\r\n\r\n", bytes.len()).map_err(|e| e.to_string())?;
    w.write_all(&bytes).map_err(|e| e.to_string())?;
    w.flush().map_err(|e| e.to_string())
}
fn offset(text: &str, p: &Value) -> Result<usize, String> {
    let line = p["line"].as_u64().ok_or("invalid line")? as usize;
    let character = p["character"].as_u64().ok_or("invalid character")? as usize;
    let mut start = 0;
    for _ in 0..line {
        start += text[start..].find('\n').ok_or("line out of bounds")? + 1;
    }
    let mut units = 0;
    for (i, c) in text[start..].char_indices() {
        if units == character {
            return Ok(start + i);
        }
        if c == '\n' {
            break;
        }
        units += c.len_utf16();
    }
    if units == character {
        let end = text[start..]
            .find('\n')
            .map(|i| start + i)
            .unwrap_or(text.len());
        Ok(end)
    } else {
        Err("UTF-16 character offset out of bounds".into())
    }
}
fn position(text: &str, offset: usize) -> Value {
    let offset = offset.min(text.len());
    let line = text[..offset].bytes().filter(|b| *b == b'\n').count();
    let start = text[..offset].rfind('\n').map(|i| i + 1).unwrap_or(0);
    json!({"line":line,"character":text[start..offset].encode_utf16().count()})
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn utf16() {
        let t = "a😀b\nc";
        assert_eq!(offset(t, &json!({"line":0,"character":3})).unwrap(), 5);
        assert!(offset(t, &json!({"line":0,"character":2})).is_err());
        assert_eq!(position(t, 5), json!({"line":0,"character":3}));
    }
}
