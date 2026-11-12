//! Canonical VFQI invocation bytes; slot boundaries/categories are observable.
use crate::quote_record::{Error, Writer, kind};
use vesper_loom_sandbox::quote_transport as q;
pub const KEY_SCHEMA_VERSION: u32 = 1;
pub const PROFILE: &str = "vesper:forge-quote@0.1.0/expr-quotation/1";
pub fn canonical_invocation(inv: &q::Invocation, limits: q::Limits) -> Result<Vec<u8>, Error> {
    limits.check_invocation(inv)?;
    let len = q::invocation_size(inv)?;
    if len > limits.max_invocation_bytes {
        return Err(q::Error::Budget(q::Resource::Invocation).into());
    }
    if len.checked_mul(3).is_none_or(|n| n > limits.max_work) {
        return Err(q::Error::Budget(q::Resource::Work).into());
    }
    let mut w = Writer::new(len)?;
    w.bytes(b"VFQI");
    w.u32(KEY_SCHEMA_VERSION);
    w.u8(match inv.kind {
        q::MacroKind::ProcMacro => 0,
        q::MacroKind::DeriveMacro => 1,
    });
    w.count(inv.slots.len())?;
    w.count(inv.args.len())?;
    w.string(&inv.entry)?;
    for a in &inv.args {
        w.string(a)?;
    }
    for slot in &inv.slots {
        w.u8(match slot.category {
            q::Category::Expr => 0,
        });
        w.count(slot.tokens.len())?;
        for token in &slot.tokens {
            let (k, d) = kind(token.kind);
            w.u8(k);
            w.u8(d);
            w.span(token.reported_span);
            w.string(&token.text)?;
        }
    }
    debug_assert_eq!(len, w.0.len());
    Ok(w.0)
}
