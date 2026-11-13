//! Data-only Lean protocol and the shared bounded blocking frame codec.
use super::*;
use serde_json::{Value, json};
use std::io::{Read, Write};

fn protocol(detail: impl Into<String>) -> HarnessError {
    HarnessError::new(FailureKind::Protocol, detail)
}

pub fn read_frame<R: Read>(reader: &mut R, cap: usize) -> Result<Option<Vec<u8>>, HarnessError> {
    if cap == 0 || cap > FRAME_CAP {
        return Err(protocol("frame cap"));
    }
    let mut header = [0; 4];
    let mut done = 0;
    while done < header.len() {
        match reader.read(&mut header[done..]) {
            Ok(0) if done == 0 => return Ok(None),
            Ok(0) => return Err(protocol("truncated frame header")),
            Ok(n) => done += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e.into()),
        }
    }
    let len = u32::from_le_bytes(header) as usize;
    if len == 0 || len > cap {
        return Err(protocol("frame limit"));
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(len)
        .map_err(|_| HarnessError::new(FailureKind::Allocation, "frame allocation"))?;
    bytes.resize(len, 0);
    done = 0;
    while done < len {
        match reader.read(&mut bytes[done..]) {
            Ok(0) => return Err(protocol("truncated frame payload")),
            Ok(n) => done += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(Some(bytes))
}
pub fn write_frame<W: Write>(
    writer: &mut W,
    payload: &[u8],
    cap: usize,
) -> Result<(), HarnessError> {
    if cap == 0 || cap > FRAME_CAP || payload.is_empty() || payload.len() > cap {
        return Err(protocol("frame limit"));
    }
    writer.write_all(&(payload.len() as u32).to_le_bytes())?;
    writer.write_all(payload)?;
    writer.flush()?;
    Ok(())
}
pub(crate) fn hash_frame(hash: &mut blake3::Hasher, bytes: &[u8]) {
    hash.update(&(bytes.len() as u32).to_le_bytes());
    hash.update(bytes);
}

pub fn encode_request(
    id: u64,
    p: &Program,
    fuel: u16,
    max_bytes: usize,
) -> Result<Vec<u8>, HarnessError> {
    if id > SEQUENCE_CAP || fuel > 4096 || max_bytes == 0 || max_bytes > FRAME_CAP {
        return Err(protocol("request limits"));
    }
    p.validate()?;
    let code = |nodes: &Code| -> Vec<Value> {
        nodes
            .iter()
            .map(|n| match n {
                Node::Int(v) => json!([0, v.to_string()]),
                Node::Bool(v) => json!([5, u8::from(*v)]),
                Node::Variable(Scope::Input, i) => json!([1, i]),
                Node::Variable(Scope::Local, i) => json!([2, i]),
                Node::Checked(op) => json!([3, op.tag()]),
                Node::Call(callee, argc) => {
                    let (target, selector) = match callee {
                        Callee::Intrinsic(op) => (0, u16::from(op.tag())),
                        Callee::Variable(Scope::Input, i) => (1, *i),
                        Callee::Variable(Scope::Local, i) => (2, *i),
                    };
                    json!([4, target, selector, argc])
                }
            })
            .collect()
    };
    let inputs: Vec<Value> = p
        .inputs
        .iter()
        .map(|i| json!([i.kind.tag(), i.value.to_string()]))
        .collect();
    let locals: Vec<Vec<Value>> = p.locals.iter().map(code).collect();
    let request = json!([
        1,
        id,
        p.kind.tag(),
        fuel,
        u8::from(p.allow_fail),
        inputs,
        locals,
        code(&p.result)
    ]);
    // Shape quotas bound the pre-serialization tree and its upper bound is <16KiB.
    let bytes = serde_json::to_vec(&request).map_err(|e| protocol(e.to_string()))?;
    if bytes.len() > max_bytes {
        return Err(protocol("request byte limit"));
    }
    Ok(bytes)
}

/// Small lexical envelope before JSON parsing: arrays, quoted ASCII and unsigned controls only.
pub(crate) fn wire_value(bytes: &[u8], cap: usize) -> Result<Value, HarnessError> {
    if bytes.is_empty() || bytes.len() > cap {
        return Err(protocol("JSON byte cap"));
    }
    let (mut depth, mut tokens, mut i) = (0usize, 0usize, 0usize);
    while i < bytes.len() {
        match bytes[i] {
            b'[' => {
                depth += 1;
                if depth > 4 {
                    return Err(protocol("JSON depth"));
                }
                i += 1;
            }
            b']' => {
                depth = depth
                    .checked_sub(1)
                    .ok_or_else(|| protocol("JSON nesting"))?;
                i += 1;
            }
            b'"' => {
                tokens += 1;
                i += 1;
                let start = i;
                while i < bytes.len() && bytes[i] != b'"' {
                    if !(32..=126).contains(&bytes[i]) || bytes[i] == b'\\' {
                        return Err(protocol("JSON string lexical envelope"));
                    }
                    i += 1;
                }
                if i == bytes.len() || i - start > 40 {
                    return Err(protocol("JSON string cap"));
                }
                i += 1;
            }
            b'0'..=b'9' => {
                tokens += 1;
                let start = i;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
                if i - start > 10 {
                    return Err(protocol("JSON control cap"));
                }
            }
            b',' | b' ' | b'\n' | b'\r' | b'\t' => i += 1,
            _ => return Err(protocol("JSON lexical envelope")),
        }
        if tokens > 1024 {
            return Err(protocol("JSON token cap"));
        }
    }
    if depth != 0 {
        return Err(protocol("JSON nesting"));
    }
    serde_json::from_slice(bytes).map_err(|e| protocol(e.to_string()))
}
fn array(value: &Value, len: usize) -> Result<&[Value], HarnessError> {
    let a = value.as_array().ok_or_else(|| protocol("response array"))?;
    if a.len() != len {
        return Err(protocol("response arity"));
    }
    Ok(a)
}
fn number(value: &Value) -> Result<u64, HarnessError> {
    value
        .as_u64()
        .filter(|n| *n <= SEQUENCE_CAP)
        .ok_or_else(|| protocol("response number"))
}
fn string(value: &Value) -> Result<&str, HarnessError> {
    value.as_str().ok_or_else(|| protocol("response string"))
}
fn envelope(bytes: &[u8]) -> Result<Vec<Value>, HarnessError> {
    let v = wire_value(bytes, RESPONSE_CAP)?;
    let a = v.as_array().ok_or_else(|| protocol("response array"))?;
    if a.len() < 2 || number(&a[0])? != 1 {
        return Err(protocol("response version"));
    }
    Ok(a.clone())
}
pub(crate) fn ready(bytes: &[u8]) -> Result<(), HarnessError> {
    let expected = json!([
        1,
        "ready",
        1,
        "4.24.0",
        "vesper.arith.checked/1",
        63,
        8,
        3,
        3,
        4096
    ]);
    if wire_value(bytes, RESPONSE_CAP)? != expected {
        return Err(HarnessError::new(
            FailureKind::ToolVersion,
            "Lean READY profile/version mismatch",
        ));
    }
    Ok(())
}
pub fn decode_answer(id: u64, bytes: &[u8]) -> Result<OracleAnswer, HarnessError> {
    let a = envelope(bytes)?;
    let tag = string(&a[1])?;
    if a.len() < 3 || number(&a[2])? != id {
        return Err(protocol("response sequence"));
    }
    let steps = |v: &Value| -> Result<u16, HarnessError> {
        let n = number(v)?;
        if n > 4096 {
            return Err(protocol("response steps"));
        }
        Ok(n as u16)
    };
    match tag {
        "value" => {
            array(&Value::Array(a.clone()), 5)?;
            let n = decimal_i128::parse(string(&a[3])?).map_err(protocol)?;
            Ok(OracleAnswer::Outcome {
                outcome: Outcome::Value(n),
                steps: steps(&a[4])?,
            })
        }
        "overflow" => {
            if a.len() != 4 {
                return Err(protocol("overflow arity"));
            }
            Ok(OracleAnswer::Outcome {
                outcome: Outcome::Overflow,
                steps: steps(&a[3])?,
            })
        }
        "diagnostic" => {
            if a.len() != 6 {
                return Err(protocol("diagnostic arity"));
            }
            let code = string(&a[3])?;
            let label = string(&a[4])?;
            let class = [
                DiagnosticClass::UnknownVariable,
                DiagnosticClass::TypeMismatch,
                DiagnosticClass::LiteralRange,
                DiagnosticClass::LiteralParseRange,
                DiagnosticClass::MissingFail,
                DiagnosticClass::CallArity,
                DiagnosticClass::NotCallable,
            ]
            .into_iter()
            .find(|c| c.code() == code && c.label() == label)
            .ok_or_else(|| protocol("diagnostic pair"))?;
            let b = array(&a[5], 3)?;
            let (scope, slot, node) = (number(&b[0])?, number(&b[1])?, number(&b[2])?);
            if scope > 2 || slot > 2 || (scope == 2 && slot != 0) || node >= 63 {
                return Err(protocol("diagnostic blame"));
            }
            Ok(OracleAnswer::Diagnostic {
                class,
                blame: Blame {
                    scope: scope as u8,
                    slot: slot as u16,
                    node: node as u16,
                },
            })
        }
        "fuel_exhausted" => Err(HarnessError::new(FailureKind::Fuel, "Lean fuel exhausted")),
        "invariant_failure" => Err(HarnessError::new(
            FailureKind::Invariant,
            "Lean invariant failure",
        )),
        _ => Err(protocol("unexpected Lean response")),
    }
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Counters {
    pub seen: u64,
    pub checked: u64,
    pub diagnostics: u64,
    pub invalid: u64,
    pub values: u64,
    pub overflows: u64,
    pub fuel: u64,
}
impl Counters {
    pub fn record(&mut self, answer: &OracleAnswer) -> Result<(), HarnessError> {
        let add = |v: &mut u64| -> Result<(), HarnessError> {
            *v = v
                .checked_add(1)
                .filter(|n| *n <= SEQUENCE_CAP)
                .ok_or_else(|| protocol("counter overflow"))?;
            Ok(())
        };
        add(&mut self.seen)?;
        match answer {
            OracleAnswer::Outcome { outcome, .. } => {
                add(&mut self.checked)?;
                match outcome {
                    Outcome::Value(_) => add(&mut self.values)?,
                    Outcome::Overflow => add(&mut self.overflows)?,
                }
            }
            OracleAnswer::Diagnostic { .. } => add(&mut self.diagnostics)?,
        }
        Ok(())
    }
}
pub(crate) fn bye(bytes: &[u8], expected: Counters) -> Result<(), HarnessError> {
    let a = envelope(bytes)?;
    if a.len() != 9 || string(&a[1])? != "bye" {
        return Err(protocol("missing BYE"));
    }
    let actual = Counters {
        seen: number(&a[2])?,
        checked: number(&a[3])?,
        diagnostics: number(&a[4])?,
        invalid: number(&a[5])?,
        values: number(&a[6])?,
        overflows: number(&a[7])?,
        fuel: number(&a[8])?,
    };
    if actual != expected
        || actual.checked != actual.values + actual.overflows + actual.fuel
        || actual.seen != actual.checked + actual.diagnostics + actual.invalid
    {
        return Err(HarnessError::new(
            FailureKind::CounterMismatch,
            "Lean BYE counter mismatch",
        ));
    }
    Ok(())
}
