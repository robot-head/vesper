//! Actual component execution for the separate structured quotation ABI.
//! The private parent supplies exactly the legacy bounded WASI runtime setup.
use super::*;
use crate::quote_transport as q;
mod quote_bindings {
    wasmtime::component::bindgen!({path:"wit/forge-quote",world:"sandboxed-quotation",exports:{default:async}});
}
use quote_bindings::vesper::forge_quote::types as w;
#[derive(Debug)]
pub enum Error {
    Sandbox(SandboxError),
    Transport(q::Error),
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Sandbox(e) => e.fmt(f),
            Self::Transport(e) => e.fmt(f),
        }
    }
}
impl std::error::Error for Error {}
impl From<SandboxError> for Error {
    fn from(e: SandboxError) -> Self {
        Self::Sandbox(e)
    }
}
impl From<crate::policy::ConfigError> for Error {
    fn from(e: crate::policy::ConfigError) -> Self {
        Self::Sandbox(SandboxError::Config(e))
    }
}
impl From<q::Error> for Error {
    fn from(e: q::Error) -> Self {
        Self::Transport(e)
    }
}
#[derive(Debug)]
pub struct QuotationReport {
    pub result: Result<q::Outcome, Vec<transport::Diagnostic>>,
    pub fuel_consumed: u64,
    pub peak_memory_bytes: usize,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub outputs: Vec<OutputFile>,
}
impl SandboxHost {
    pub fn run_quotation(
        &self,
        component: &StepComponent,
        config: &StepConfig,
        inv: &q::Invocation,
        limits: q::Limits,
    ) -> Result<QuotationReport, Error> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .enable_io()
            .build()
            .map_err(host_err)?;
        rt.block_on(self.run_quotation_async(component, config, inv, limits))
    }
    pub async fn run_quotation_async(
        &self,
        component: &StepComponent,
        config: &StepConfig,
        inv: &q::Invocation,
        limits: q::Limits,
    ) -> Result<QuotationReport, Error> {
        let layout = config.validate()?;
        limits.check_invocation(inv)?;
        let RunContext {
            mut store,
            stdout,
            stderr,
            layout,
        } = self.start_validated(config, layout)?;
        let invocation = to_wit(inv);
        let call = async {
            let step = quote_bindings::SandboxedQuotation::instantiate_async(
                &mut store,
                &component.component,
                &self.linker,
            )
            .await
            .map_err(Failure::Link)?;
            let api = step.vesper_forge_quote_quotation();
            let version = api
                .call_protocol_version(&mut store)
                .await
                .map_err(Failure::Run)?;
            if version != q::PROTOCOL_VERSION {
                return Err(Failure::Protocol(version));
            }
            api.call_run(&mut store, &invocation)
                .await
                .map_err(Failure::Run)
        };
        let outcome = tokio::time::timeout(config.limits.wall_clock_backstop, call).await;
        let fuel_consumed = config.limits.fuel - store.get_fuel().unwrap_or(0);
        let result = complete_call(outcome, &config.limits, &store.data().limiter)?;
        let (failed, packed) = match result {
            Ok(p) => (false, p),
            Err(p) => (true, p),
        };
        // WIT lifting is bounded by guest linear memory. Move the lifted tables
        // without extra strings, then perform alias/copy validation before DTOs.
        q::bound(
            packed.items.len(),
            limits.max_output_items,
            q::Resource::Items,
        )?;
        q::bound(
            packed.diagnostics.len(),
            limits.max_diagnostics,
            q::Resource::Diagnostics,
        )?;
        let table_cap = q::add(
            limits.max_copied_output_text_bytes,
            limits.max_diagnostic_text_bytes,
            q::Resource::Text,
        )?;
        q::bound(packed.text.len(), table_cap, q::Resource::Text)?;
        if failed && !packed.items.is_empty() {
            return Err(q::Error::ItemsInFailure.into());
        }
        let packed = from_wit(packed);
        let outcome = limits.unpack(&packed, inv.slots.len(), failed)?;
        let result = if failed {
            Err(outcome.diagnostics)
        } else {
            Ok(outcome)
        };
        let peak_memory_bytes = store.data().limiter.peak_memory;
        drop(store);
        let outputs = scan_outputs(&layout.output_dir, &config.limits)
            .map_err(|e| Error::Sandbox(SandboxError::Output(e)))?;
        Ok(QuotationReport {
            result,
            fuel_consumed,
            peak_memory_bytes,
            stdout: stdout.contents().to_vec(),
            stderr: stderr.contents().to_vec(),
            outputs,
        })
    }
}
fn span(s: transport::Span) -> w::Span {
    w::Span {
        file: s.file,
        start: s.start,
        end: s.end,
    }
}
fn unspan(s: w::Span) -> transport::Span {
    transport::Span {
        file: s.file,
        start: s.start,
        end: s.end,
    }
}
fn delim(d: transport::Delimiter) -> w::Delimiter {
    match d {
        transport::Delimiter::Paren => w::Delimiter::Paren,
        transport::Delimiter::Bracket => w::Delimiter::Bracket,
        transport::Delimiter::Brace => w::Delimiter::Brace,
    }
}
fn undelim(d: w::Delimiter) -> transport::Delimiter {
    match d {
        w::Delimiter::Paren => transport::Delimiter::Paren,
        w::Delimiter::Bracket => transport::Delimiter::Bracket,
        w::Delimiter::Brace => transport::Delimiter::Brace,
    }
}
fn kind(k: transport::TokenKind) -> w::TokenKind {
    use transport::TokenKind as K;
    match k {
        K::Ident => w::TokenKind::Ident,
        K::Punct => w::TokenKind::Punct,
        K::IntLiteral => w::TokenKind::IntLiteral,
        K::StringLiteral => w::TokenKind::StringLiteral,
        K::Open(d) => w::TokenKind::Open(delim(d)),
        K::Close(d) => w::TokenKind::Close(delim(d)),
    }
}
fn unkind(k: w::TokenKind) -> transport::TokenKind {
    use transport::TokenKind as K;
    match k {
        w::TokenKind::Ident => K::Ident,
        w::TokenKind::Punct => K::Punct,
        w::TokenKind::IntLiteral => K::IntLiteral,
        w::TokenKind::StringLiteral => K::StringLiteral,
        w::TokenKind::Open(d) => K::Open(undelim(d)),
        w::TokenKind::Close(d) => K::Close(undelim(d)),
    }
}
fn to_wit(inv: &q::Invocation) -> w::Invocation {
    w::Invocation {
        protocol_version: inv.protocol_version,
        kind: match inv.kind {
            q::MacroKind::ProcMacro => w::MacroKind::ProcMacro,
            q::MacroKind::DeriveMacro => w::MacroKind::DeriveMacro,
        },
        entry: inv.entry.clone(),
        args: inv.args.clone(),
        slots: inv
            .slots
            .iter()
            .map(|s| w::InputSlot {
                category: match s.category {
                    q::Category::Expr => w::Category::Expr,
                },
                tokens: s
                    .tokens
                    .iter()
                    .map(|t| w::InputToken {
                        kind: kind(t.kind),
                        text: t.text.clone(),
                        reported_span: t.reported_span.map(span),
                    })
                    .collect(),
            })
            .collect(),
    }
}
fn from_wit(p: w::PackedResult) -> q::PackedResult {
    q::PackedResult {
        items: p
            .items
            .into_iter()
            .map(|i| match i {
                w::PackedItem::InputSlot(n) => q::PackedItem::InputSlot(n),
                w::PackedItem::Generated(t) => q::PackedItem::Generated(q::PackedGenerated {
                    kind: unkind(t.kind),
                    text: q::Slice {
                        offset: t.text.offset,
                        len: t.text.len,
                    },
                    reported_span: t.reported_span.map(unspan),
                    claimed_def: t.claimed_def.map(|d| transport::DefId {
                        package: d.package,
                        item: d.item,
                    }),
                }),
            })
            .collect(),
        diagnostics: p
            .diagnostics
            .into_iter()
            .map(|d| q::PackedDiagnostic {
                severity: match d.severity {
                    w::Severity::Error => transport::Severity::Error,
                    w::Severity::Warning => transport::Severity::Warning,
                    w::Severity::Note => transport::Severity::Note,
                },
                message: q::Slice {
                    offset: d.message.offset,
                    len: d.message.len,
                },
                reported_span: d.reported_span.map(unspan),
            })
            .collect(),
        text: p.text,
    }
}
