//! Native JIT runtime calls behind `Trace` lowering (§31.2 / §22).
//!
//! Cranelift native codegen lowers unhandled `Trace` operations to the six
//! `vesper_trace_*` C-ABI symbols below. [`with_traced`] drives a real
//! [`BeaconHost`] (`vesper_beacon::SpanPool` and `TelemetryRing`) on a scoped
//! host thread while the JIT-compiled entry runs on the calling thread, so both
//! native execution and the reference evaluator share one host state machine.
use super::host::{
    ACTIVE_SPANS, BeaconHost, ExportedSpan, RawActiveSpan, RawSpanContext, TraceConfig, TraceRun,
    TraceRunError, redacted_attribute, validate_traced_entry,
};
use crate::check::Checked;
use std::cell::RefCell;
use std::num::NonZeroU64;
use std::sync::mpsc::{self, Receiver, SyncSender};
use vesper_beacon::{PrimaryAttribute, SpanPool};

enum TraceCmd {
    CurrentCtx,
    StartSpan {
        schema_id: u64,
        name: String,
    },
    StartChild {
        parent: RawSpanContext,
        schema_id: u64,
        name: String,
    },
    EmitOk {
        span: RawSpanContext,
        attr_u64: u64,
    },
    EmitErr {
        span: RawSpanContext,
        err_code: u64,
        message: String,
    },
    EmitAttr {
        span: RawSpanContext,
        attr: PrimaryAttribute,
    },
}

enum TraceReply {
    Ctx(RawSpanContext),
    Span(RawActiveSpan),
    Unit,
    Err,
}

struct Client {
    tx: SyncSender<TraceCmd>,
    rx: Receiver<TraceReply>,
    failed: bool,
}

thread_local! {
    static ACTIVE: RefCell<Option<Client>> = const { RefCell::new(None) };
}

struct ResetGuard;
impl Drop for ResetGuard {
    fn drop(&mut self) {
        ACTIVE.with(|cell| {
            *cell.borrow_mut() = None;
        });
    }
}

/// Symbols imported by native `eff [Trace]` codegen and registered with JIT.
pub fn symbols() -> [(&'static str, *const u8); 6] {
    [
        (
            "vesper_trace_current_ctx",
            vesper_trace_current_ctx as *const u8,
        ),
        (
            "vesper_trace_start_span",
            vesper_trace_start_span as *const u8,
        ),
        (
            "vesper_trace_start_child",
            vesper_trace_start_child as *const u8,
        ),
        ("vesper_trace_emit_ok", vesper_trace_emit_ok as *const u8),
        ("vesper_trace_emit_err", vesper_trace_emit_err as *const u8),
        (
            "vesper_trace_emit_attr",
            vesper_trace_emit_attr as *const u8,
        ),
    ]
}

fn dispatch(cmd: TraceCmd) -> Option<TraceReply> {
    ACTIVE.with(|cell| {
        let mut slot = cell.borrow_mut();
        let client = slot.as_mut()?;
        if client.failed || client.tx.send(cmd).is_err() {
            client.failed = true;
            return None;
        }
        match client.rx.recv() {
            Ok(TraceReply::Err) | Err(_) => {
                client.failed = true;
                None
            }
            Ok(reply) => Some(reply),
        }
    })
}

/// # Safety
/// `ptr` must point to an 8-byte-aligned native `String` block `[len: u64, utf8...]`.
unsafe fn read_native_string(ptr: *const u8) -> Result<String, ()> {
    if ptr.is_null() {
        return Err(());
    }
    // SAFETY: caller guarantees `ptr` points to a readable native `String` header.
    let len = unsafe { *(ptr as *const u64) } as usize;
    // SAFETY: caller guarantees `len` UTF-8 bytes follow the 8-byte length prefix.
    let bytes = unsafe { std::slice::from_raw_parts(ptr.add(8), len) };
    std::str::from_utf8(bytes)
        .map(str::to_owned)
        .map_err(|_| ())
}

/// # Safety
/// `ptr` must point to 4 readable `u64` words representing a native `SpanContext`.
unsafe fn read_native_ctx(ptr: *const u64) -> Result<RawSpanContext, ()> {
    if ptr.is_null() {
        return Err(());
    }
    // SAFETY: caller guarantees `ptr` has 4 readable `u64` slots.
    let words = unsafe { std::slice::from_raw_parts(ptr, 4) };
    let flags = u8::try_from(words[3]).map_err(|_| ())?;
    Ok(RawSpanContext {
        trace_id_hi: words[0],
        trace_id_lo: words[1],
        span_id: words[2],
        flags,
    })
}

/// # Safety
/// `out` must point to 4 writable `u64` words.
unsafe fn write_native_ctx(out: *mut u64, ctx: RawSpanContext) {
    // SAFETY: caller guarantees `out` has 4 writable `u64` slots.
    let words = unsafe { std::slice::from_raw_parts_mut(out, 4) };
    words[0] = ctx.trace_id_hi;
    words[1] = ctx.trace_id_lo;
    words[2] = ctx.span_id;
    words[3] = u64::from(ctx.flags);
}

/// # Safety
/// `out` must point to 7 writable `u64` words.
unsafe fn write_native_span(out: *mut u64, span: RawActiveSpan) {
    // SAFETY: caller guarantees `out` has 7 writable `u64` slots (56 bytes).
    unsafe { write_native_ctx(out, span.ctx) };
    // SAFETY: caller guarantees offsets 4..7 are writable.
    let words = unsafe { std::slice::from_raw_parts_mut(out, 7) };
    words[4] = span.parent_span_id;
    words[5] = span.start_mono_ns;
    words[6] = u64::from(span.schema_id);
}

/// # Safety
/// `out_ctx` must be a valid pointer to 4 writable `u64` words.
pub unsafe extern "C" fn vesper_trace_current_ctx(out_ctx: *mut u64) -> i64 {
    let Some(TraceReply::Ctx(ctx)) = dispatch(TraceCmd::CurrentCtx) else {
        return 1;
    };
    if out_ctx.is_null() {
        return 1;
    }
    // SAFETY: JIT caller allocated 32 bytes for `SpanContext`.
    unsafe { write_native_ctx(out_ctx, ctx) };
    0
}

/// # Safety
/// `name_ptr` must be a readable native `String` and `out_span` must be a writable 56-byte `ActiveSpan`.
pub unsafe extern "C" fn vesper_trace_start_span(
    schema_id: u64,
    name_ptr: *const u8,
    out_span: *mut u64,
) -> i64 {
    // SAFETY: JIT caller passes a native `String` and 56-byte `ActiveSpan` output slot.
    let Ok(name) = (unsafe { read_native_string(name_ptr) }) else {
        return 1;
    };
    let Some(TraceReply::Span(span)) = dispatch(TraceCmd::StartSpan { schema_id, name }) else {
        return 1;
    };
    if out_span.is_null() {
        return 1;
    }
    // SAFETY: JIT caller allocated 56 bytes for `ActiveSpan`.
    unsafe { write_native_span(out_span, span) };
    0
}

/// # Safety
/// `parent_ptr` must be a readable `SpanContext`, `name_ptr` a readable `String`,
/// and `out_span` a writable 56-byte `ActiveSpan`.
pub unsafe extern "C" fn vesper_trace_start_child(
    parent_ptr: *const u64,
    schema_id: u64,
    name_ptr: *const u8,
    out_span: *mut u64,
) -> i64 {
    // SAFETY: JIT caller passes valid pointers to `SpanContext` and `String`.
    let (Ok(parent), Ok(name)) =
        (unsafe { (read_native_ctx(parent_ptr), read_native_string(name_ptr)) })
    else {
        return 1;
    };
    let Some(TraceReply::Span(span)) = dispatch(TraceCmd::StartChild {
        parent,
        schema_id,
        name,
    }) else {
        return 1;
    };
    if out_span.is_null() {
        return 1;
    }
    // SAFETY: JIT caller allocated 56 bytes for `ActiveSpan`.
    unsafe { write_native_span(out_span, span) };
    0
}

/// # Safety
/// `span_ptr` must be a readable `ActiveSpan` (whose first 4 `u64` words are `SpanContext`).
pub unsafe extern "C" fn vesper_trace_emit_ok(span_ptr: *const u64, attr_u64: u64) -> i64 {
    // SAFETY: `ActiveSpan` starts with inline `SpanContext` (4 `u64` words).
    let Ok(span) = (unsafe { read_native_ctx(span_ptr) }) else {
        return 1;
    };
    match dispatch(TraceCmd::EmitOk { span, attr_u64 }) {
        Some(TraceReply::Unit) => 0,
        _ => 1,
    }
}

/// # Safety
/// `span_ptr` must be a readable `ActiveSpan` and `msg_ptr` a readable native `String`.
pub unsafe extern "C" fn vesper_trace_emit_err(
    span_ptr: *const u64,
    err_code: u64,
    msg_ptr: *const u8,
) -> i64 {
    // SAFETY: JIT caller passes valid pointers to `ActiveSpan` and `String`.
    let (Ok(span), Ok(message)) =
        (unsafe { (read_native_ctx(span_ptr), read_native_string(msg_ptr)) })
    else {
        return 1;
    };
    match dispatch(TraceCmd::EmitErr {
        span,
        err_code,
        message,
    }) {
        Some(TraceReply::Unit) => 0,
        _ => 1,
    }
}

/// # Safety
/// `span_ptr` must be a readable `ActiveSpan` and `attr_ptr` a readable 2-word `TelemetryAttr`.
pub unsafe extern "C" fn vesper_trace_emit_attr(span_ptr: *const u64, attr_ptr: *const u64) -> i64 {
    if attr_ptr.is_null() {
        return 1;
    }
    // SAFETY: `ActiveSpan` starts with `SpanContext`; `TelemetryAttr` is 2 `u64` slots.
    let Ok(span) = (unsafe { read_native_ctx(span_ptr) }) else {
        return 1;
    };
    // SAFETY: enum `TelemetryAttr` has tag slot 0 and payload slot 1.
    let attr_words = unsafe { std::slice::from_raw_parts(attr_ptr, 2) };
    let attr = match attr_words[0] {
        0 => PrimaryAttribute::None,
        1 => PrimaryAttribute::ItemCount(attr_words[1]),
        2 => PrimaryAttribute::ByteCount(attr_words[1]),
        3 => redacted_attribute(),
        _ => return 1,
    };
    match dispatch(TraceCmd::EmitAttr { span, attr }) {
        Some(TraceReply::Unit) => 0,
        _ => 1,
    }
}

/// Whether the active native trace client on this thread has observed a host failure.
pub fn has_failed() -> bool {
    ACTIVE.with(|cell| cell.borrow().as_ref().is_some_and(|c| c.failed))
}

/// Executes `f` while serving `vesper_trace_*` callbacks against a live
/// [`BeaconHost`] on a scoped host thread.
pub fn with_traced<T>(
    config: TraceConfig,
    f: impl FnOnce() -> T,
) -> Result<(T, Vec<ExportedSpan>), TraceRunError> {
    let first = NonZeroU64::new(config.first_span_id).ok_or(TraceRunError::InvalidContext)?;
    let _guard = ResetGuard;
    let (cmd_tx, cmd_rx) = mpsc::sync_channel::<TraceCmd>(0);
    let (rep_tx, rep_rx) = mpsc::sync_channel::<TraceReply>(0);
    ACTIVE.with(|cell| {
        *cell.borrow_mut() = Some(Client {
            tx: cmd_tx,
            rx: rep_rx,
            failed: false,
        });
    });

    std::thread::scope(|scope| {
        // Ensure ACTIVE is cleared (closing cmd_tx) even if `f()` panics,
        // before `scope` joins `host_thread` waiting in `cmd_rx.recv()`.
        let _inner_guard = ResetGuard;
        let host_thread = scope.spawn(move || -> Result<Vec<ExportedSpan>, TraceRunError> {
            let pool = SpanPool::<ACTIVE_SPANS>::new(config.trace_id, first, config.flags)
                .map_err(|_| TraceRunError::InvalidContext)?;
            let mut host = BeaconHost::new(&pool, config);
            let mut error: Option<TraceRunError> = None;
            while let Ok(cmd) = cmd_rx.recv() {
                if error.is_some() {
                    let _ = rep_tx.send(TraceReply::Err);
                    continue;
                }
                let reply = match cmd {
                    TraceCmd::CurrentCtx => Ok(TraceReply::Ctx(host.current_ctx())),
                    TraceCmd::StartSpan { schema_id, name } => {
                        host.start_span(schema_id, &name).map(TraceReply::Span)
                    }
                    TraceCmd::StartChild {
                        parent,
                        schema_id,
                        name,
                    } => host
                        .start_child(parent, schema_id, &name)
                        .map(TraceReply::Span),
                    TraceCmd::EmitOk { span, attr_u64 } => {
                        host.emit_ok(span, attr_u64).map(|()| TraceReply::Unit)
                    }
                    TraceCmd::EmitErr {
                        span,
                        err_code,
                        message,
                    } => host
                        .emit_err(span, err_code, &message)
                        .map(|()| TraceReply::Unit),
                    TraceCmd::EmitAttr { span, attr } => {
                        host.emit_attr(span, attr).map(|()| TraceReply::Unit)
                    }
                };
                match reply {
                    Ok(r) => {
                        let _ = rep_tx.send(r);
                    }
                    Err(e) => {
                        error = Some(e);
                        let _ = rep_tx.send(TraceReply::Err);
                    }
                }
            }
            match error {
                Some(e) => {
                    host.cancel_open();
                    Err(e)
                }
                None => host.finish(),
            }
        });

        let value = f();
        ACTIVE.with(|cell| {
            *cell.borrow_mut() = None;
        });
        let spans = host_thread
            .join()
            .map_err(|_| TraceRunError::Runtime("native trace host panicked".into()))??;
        Ok((value, spans))
    })
}

/// Compiles `entry` (which must carry `eff [Trace]`) with Cranelift JIT and
/// executes it against a real `vesper-beacon` host, returning the same
/// [`TraceRun`] as the reference evaluator.
pub fn run_traced_native(
    checked: &Checked,
    entry: &str,
    config: TraceConfig,
) -> Result<TraceRun, TraceRunError> {
    run_traced_native_with_options(checked, entry, config, true)
}

/// [`run_traced_native`] with explicit Place IR optimization flag.
pub fn run_traced_native_with_options(
    checked: &Checked,
    entry: &str,
    config: TraceConfig,
    optimize: bool,
) -> Result<TraceRun, TraceRunError> {
    validate_traced_entry(checked, entry)?;
    let (value, spans) = with_traced(config, || {
        crate::native::jit_traced_with_options(&checked.world, entry, optimize)
    })?;
    let value = value.map_err(TraceRunError::Runtime)?;
    Ok(TraceRun { value, spans })
}
