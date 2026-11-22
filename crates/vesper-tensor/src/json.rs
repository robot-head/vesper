//! A strict, bounded JSON reader for the safetensors header.
//!
//! It accepts RFC 8259 JSON with two deliberate restrictions that the
//! safetensors header never needs: numbers must be non-negative integers that
//! fit in `u64`, and nesting is limited to [`MAX_DEPTH`]. Duplicate object
//! keys, trailing bytes, unpaired surrogates and control characters in strings
//! are errors, so an ambiguous header is never silently resolved.
use crate::Error;

pub const MAX_DEPTH: usize = 8;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(u64),
    String(String),
    Array(Vec<Value>),
    /// Keys in document order (duplicates rejected).
    Object(Vec<(String, Value)>),
}
impl Value {
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Self::Object(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
}

/// Parses one header document. The package `vesper-tensor` reads it with
/// the Vesper port (`vesper/src/text.ves`); the seed copy the compiler links
/// runs the frozen Rust reader in `seed/src/engine/json.rs`.
#[cfg(not(vesper_port))]
pub fn parse(bytes: &[u8]) -> Result<Value, Error> {
    crate::engine::json::parse(bytes)
}

/// The port-backed reader: `vesper/src/text.ves` checks the document and
/// writes it as a tape, which this turns into a [`Value`]. The tape and the
/// duplicate-key table grow when the port asks for more room.
#[cfg(vesper_port)]
pub fn parse(bytes: &[u8]) -> Result<Value, Error> {
    // String offsets and lengths are 28-bit fields of a tape word.
    if bytes.len() >= 1 << 28 {
        return Err(Error::Limit("JSON header bytes"));
    }
    // The port writes every tape word and string byte before it reads it,
    // so per-thread buffers serve every call without being cleared; the
    // duplicate-key table must start empty, so it is zeroed per attempt.
    thread_local! {
        static BUFFERS: std::cell::RefCell<(Vec<u8>, Vec<u64>)> =
            const { std::cell::RefCell::new((Vec::new(), Vec::new())) };
    }
    BUFFERS.with(|buffers| {
        let (strs, tape) = &mut *buffers.borrow_mut();
        if strs.len() < bytes.len() {
            strs.resize(bytes.len(), 0);
        }
        let strs = &mut strs[..bytes.len()];
        let mut levels = [0u64; MAX_DEPTH + 1];
        let mut tape_words = bytes.len() / 8 + 16;
        let mut key_slots = (bytes.len() / 16 + 16).next_power_of_two();
        loop {
            if tape.len() < tape_words {
                tape.resize(tape_words, 0);
            }
            let mut keys = vec![0u64; 2 * key_slots];
            // An integer above `u64::MAX` fails the port's checked
            // arithmetic, which `check` maps to `Error::Overflow` as the
            // Rust build returns.
            let r = crate::port::check(
                crate::port::binding::parse_json(
                    bytes,
                    &mut tape[..tape_words],
                    &mut keys,
                    strs,
                    &mut levels,
                ),
                |_| BAD,
            )?;
            let n = match r >> 60 {
                0 => r as usize,
                11 => {
                    tape_words *= 2;
                    continue;
                }
                12 => {
                    key_slots *= 2;
                    continue;
                }
                code => return Err(failure(code)),
            };
            return Ok(from_tape(&tape[..n], strs));
        }
    })
}

#[cfg(vesper_port)]
fn failure(code: u64) -> Error {
    match code {
        2 => Error::Malformed("trailing bytes after JSON header"),
        3 => Error::Limit("JSON nesting"),
        4 => Error::Malformed("duplicate JSON key"),
        5 => Error::Unsupported("negative JSON number".into()),
        6 => Error::Malformed("JSON number with a leading zero"),
        7 => Error::Unsupported("non-integer JSON number".into()),
        8 => Error::Malformed("unpaired JSON surrogate"),
        9 => Error::Malformed("control character in JSON string"),
        10 => Error::Malformed("JSON string is not UTF-8"),
        _ => BAD,
    }
}

/// Builds the [`Value`] a checked tape describes (see `text.ves`).
#[cfg(vesper_port)]
fn from_tape(tape: &[u64], strs: &[u8]) -> Value {
    let string = |w: u64| {
        let start = ((w >> 4) & 0x0fff_ffff) as usize;
        let len = (w >> 32) as usize;
        String::from_utf8(strs[start..start + len].to_vec()).expect("the port checked UTF-8")
    };
    // Open containers: the items or fields so far, and an object's pending key.
    let mut stack: Vec<(Value, Option<String>)> = Vec::new();
    let mut done = None;
    let mut i = 0;
    while i < tape.len() {
        let w = tape[i];
        i += 1;
        let v = match w & 15 {
            0 => Value::Null,
            1 => Value::Bool(true),
            2 => Value::Bool(false),
            3 => {
                i += 1;
                Value::Int(tape[i - 1])
            }
            4 => Value::String(string(w)),
            5 => {
                stack.last_mut().expect("a key is in an object").1 = Some(string(w));
                continue;
            }
            6 => {
                stack.push((Value::Array(Vec::new()), None));
                continue;
            }
            8 => {
                stack.push((Value::Object(Vec::new()), None));
                continue;
            }
            _ => stack.pop().expect("a close has an open").0,
        };
        match stack.last_mut() {
            Some((Value::Array(items), _)) => items.push(v),
            Some((Value::Object(fields), key)) => {
                fields.push((key.take().expect("a field has a key"), v))
            }
            Some(_) => unreachable!("only containers are open"),
            None => done = Some(v),
        }
    }
    done.expect("the port checked one value")
}

pub(crate) const BAD: Error = Error::Malformed("JSON header");
/// JSON strings must escape `U+0000` through `U+001F` (RFC 8259 §7).
pub(crate) const CONTROL_END: u8 = 0x20;

/// Serializes a string as a JSON string literal.
pub fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < u32::from(CONTROL_END) => {
                out.push_str(&format!("\\u{:04x}", c as u32))
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn strict_header_json() {
        let v = parse(b" {\"a\": [1, 2], \"b\": {\"c\": \"x\\u00e9\\ud83d\\ude00\"}, \"d\": true, \"e\": null} ")
            .unwrap();
        assert_eq!(
            v.get("a"),
            Some(&Value::Array(vec![Value::Int(1), Value::Int(2)]))
        );
        assert_eq!(
            v.get("b").and_then(|b| b.get("c")),
            Some(&Value::String("xé😀".into()))
        );
        for bad in [
            &br#"{"a":1,"a":2}"#[..],
            br#"{"a":1} x"#,
            br#"{"a":01}"#,
            br#"{"a":1.5}"#,
            br#"{"a":-1}"#,
            br#"{"a":18446744073709551616}"#,
            br#"{"a":"\ud800"}"#,
            br#"{"a":"\udc00x"}"#,
            b"{\"a\":\"\x01\"}",
            br#"{"a":[1,]}"#,
            br#"{"a" 1}"#,
            br#"[[[[[[[[[[]]]]]]]]]]"#,
            b"",
            br#"{"a":"\x"}"#,
            b"{\"a\":\"\xff\"}",
        ] {
            assert!(parse(bad).is_err(), "{}", String::from_utf8_lossy(bad));
        }
        assert_eq!(quote("a\"b\\\n"), "\"a\\\"b\\\\\\u000a\"");
        assert_eq!(
            parse(quote("é\u{1}").as_bytes()).unwrap(),
            Value::String("é\u{1}".into())
        );
    }
}
