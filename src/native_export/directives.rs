//! `@export` directives: line comments in a library's source that name
//! generic instances and `.vsys` imports (export ABI v2, #898).
//!
//! Directives are comments, so the source still checks, formats and runs
//! unchanged, and every build path (`vesper build --lib`, the Cargo harness,
//! the Bazel rule) reads the same ones from the same file.
//!
//! ```text
//! // @export abi 3 options
//! // @export instance largest_u64 = largest[u64]
//! // @export instance largest[i32]
//! // @export vsys direct rust=crate::shim
//! // | sys library Clock (target = "@TARGET@", abi = "@ABI@", link = "static",
//! // |     name = "vesper_port_pilot", artifact = "blake3:00..00") {
//! // |     fn pilot_clock_ticks() -> ForeignVal[u64];
//! // | }
//! ```
//!
//! * `abi 3` makes the library an export ABI v3 library: copy structs get
//!   `#[repr(C)]` mirrors that cross in place (see the module docs of
//!   `native::export`). `abi 3 options` also lends `Option` and `Result`
//!   parameters and results as `OptionMirror`/`ResultMirror`.
//! * `instance [name =] f[T1, ..]` exports that monomorphic instance of the
//!   generic `pub fn f` as `vesper_<lib>_<name>`. Without a name the symbol
//!   is the stable mangling `f__t1__t2` (type names lowercased, type
//!   arguments joined by `_`).
//! * `vsys <direct|isolated(mpk)> [rust=<path>]` imports the `.vsys` library
//!   written on the `// |` lines that follow. `@TARGET@` and `@ABI@` stand
//!   for the object target's triple and calling convention, so one source
//!   serves every target. The linking crate provides the library's symbols;
//!   `rust=<path>` names the Rust module that defines them, which the
//!   binding then references so every linker keeps them.
use super::*;
use crate::ffi_lowering::Handler;

/// The directives of one library source.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Directives {
    pub instances: Vec<Instance>,
    pub vsys: Vec<VsysImport>,
    /// `abi 3`: the export ABI version the library opts into (v3 is the
    /// only one a source names; v1 and v2 follow from the forms used).
    pub abi: Option<u32>,
    /// `abi 3 options`: `Option` and `Result` parameters and results of
    /// copy payloads cross as `OptionMirror`/`ResultMirror`, lent in place
    /// (#991). Opt-in, so existing v3 bindings stay byte for byte.
    pub option_mirrors: bool,
}

/// One generic instance to export.
#[derive(Debug, Clone, PartialEq)]
pub struct Instance {
    /// The export name; the mangled spelling when `None`.
    pub name: Option<String>,
    pub function: String,
    pub args: Vec<Type>,
}

impl Instance {
    /// `f[T1, T2]`.
    pub fn spelling(&self) -> String {
        format!(
            "{}[{}]",
            self.function,
            self.args
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        )
    }

    /// The export name: the given one, or `f__t1__t2`.
    pub fn alias(&self) -> String {
        self.name.clone().unwrap_or_else(|| {
            let mut out = self.function.clone();
            for a in &self.args {
                out.push_str("__");
                out.push_str(&mangle(a));
            }
            out
        })
    }
}

fn mangle(ty: &Type) -> String {
    match ty {
        Type::Named(n, args) if args.is_empty() => n.to_ascii_lowercase(),
        Type::Named(n, args) => format!(
            "{}_{}",
            n.to_ascii_lowercase(),
            args.iter().map(mangle).collect::<Vec<_>>().join("_")
        ),
        Type::Tuple(items) if items.is_empty() => "unit".into(),
        Type::Tuple(items) => format!(
            "tuple_{}",
            items.iter().map(mangle).collect::<Vec<_>>().join("_")
        ),
        other => other
            .to_string()
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_ascii_lowercase()
                } else {
                    '_'
                }
            })
            .collect(),
    }
}

/// One imported `.vsys` library.
#[derive(Debug, Clone, PartialEq)]
pub struct VsysImport {
    pub library: String,
    pub handler: Handler,
    /// The `.vsys` source with `@TARGET@` and `@ABI@` filled in.
    pub source: String,
    pub rust_path: Option<String>,
    /// The library's C symbols.
    pub symbols: Vec<String>,
}

/// Parse `[name =] f[T1, ..]` (an `instance` directive or `--instance`).
pub fn parse_instance(text: &str) -> Result<Instance, ExportError> {
    let bad = |why: &str| ExportError::Directive(format!("instance '{text}': {why}"));
    let (name, rest) = match text.split_once('=') {
        Some((n, r)) => (Some(n.trim().to_string()), r.trim()),
        None => (None, text.trim()),
    };
    let ident = |n: &str| {
        n.starts_with(|c: char| c.is_ascii_lowercase() || c == '_')
            && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    };
    if name.as_deref().is_some_and(|n| !ident(n)) {
        return Err(bad("expected a lowercase export name before '='"));
    }
    let open = rest.find('[').ok_or_else(|| bad("expected f[T, ..]"))?;
    if !rest.ends_with(']') {
        return Err(bad("expected f[T, ..]"));
    }
    let function = rest[..open].trim().to_string();
    if function.is_empty()
        || !function
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return Err(bad("expected a function name"));
    }
    let args = &rest[open + 1..rest.len() - 1];
    // The type arguments parse as the arguments of a probe type.
    let probe = format!("fn probe(x: Probe[{args}]) -> Unit {{\n    ()\n}}\n");
    let parsed = crate::parser::parse(&probe);
    let ty = match (&parsed.diagnostics[..], parsed.module.items.first()) {
        ([], Some(crate::ast::Item::Function(f))) => f.params.first().map(|p| p.ty.clone()),
        _ => None,
    };
    match ty {
        Some(Type::Named(n, args)) if n == "Probe" && !args.is_empty() => Ok(Instance {
            name,
            function,
            args,
        }),
        _ => Err(bad("the type arguments do not parse")),
    }
}

/// The `@TARGET@`/`@ABI@` values of an object target.
fn vsys_target(target: Option<&str>) -> Result<(&'static str, &'static str), ExportError> {
    let triple: target_lexicon::Triple = match target {
        Some(t) => t
            .parse()
            .map_err(|e| ExportError::Directive(format!("invalid target: {e}")))?,
        None => target_lexicon::Triple::host(),
    };
    use target_lexicon::{Architecture as A, OperatingSystem as O};
    Ok(match (triple.architecture, triple.operating_system) {
        (A::X86_64, O::Linux) => ("x86_64-unknown-linux-gnu", "sysv64"),
        (A::Aarch64(_), O::Linux) => ("aarch64-unknown-linux-gnu", "aapcs64"),
        (A::Aarch64(_), O::Darwin(_) | O::MacOSX(_)) => ("aarch64-apple-darwin", "darwin64"),
        (A::X86_64, O::Windows) => ("x86_64-pc-windows-msvc", "win64"),
        _ => {
            return Err(ExportError::Directive(format!(
                "@export vsys: .vsys libraries have no target for {triple}"
            )));
        }
    })
}

impl Directives {
    /// Read the directives of `source` for an object of `target`.
    pub fn parse(source: &str, target: Option<&str>) -> Result<Self, ExportError> {
        let mut out = Self::default();
        let mut lines = source.lines().peekable();
        while let Some(line) = lines.next() {
            let Some(rest) = line.trim_start().strip_prefix("// @export") else {
                continue;
            };
            let rest = rest.trim();
            if let Some(spec) = rest.strip_prefix("abi") {
                let mut words = spec.split_whitespace();
                let version = words.next().unwrap_or_default();
                for w in words {
                    match w {
                        "options" if !out.option_mirrors => out.option_mirrors = true,
                        other => {
                            return Err(ExportError::Directive(format!(
                                "@export abi: unknown option '{other}' (expected options)"
                            )));
                        }
                    }
                }
                match version {
                    "3" if out.abi.is_none() => out.abi = Some(3),
                    "3" => {
                        return Err(ExportError::Directive(
                            "@export abi: the ABI is named twice".into(),
                        ));
                    }
                    other => {
                        return Err(ExportError::Directive(format!(
                            "@export abi: expected 3, found '{other}' (v1 and v2 follow from the forms a library uses)"
                        )));
                    }
                }
            } else if let Some(spec) = rest.strip_prefix("instance ") {
                out.instances.push(parse_instance(spec.trim())?);
            } else if let Some(spec) = rest.strip_prefix("vsys") {
                let mut words = spec.split_whitespace();
                let handler = match words.next() {
                    Some("direct") => Handler::Direct,
                    Some("isolated(mpk)") => Handler::IsolatedMpk,
                    other => {
                        return Err(ExportError::Directive(format!(
                            "@export vsys: expected direct or isolated(mpk), found {}",
                            other.unwrap_or("nothing")
                        )));
                    }
                };
                let mut rust_path = None;
                for w in words {
                    match w.strip_prefix("rust=") {
                        Some(p) if !p.is_empty() => rust_path = Some(p.to_string()),
                        _ => {
                            return Err(ExportError::Directive(format!(
                                "@export vsys: unknown option '{w}'"
                            )));
                        }
                    }
                }
                let mut text = String::new();
                while let Some(next) = lines.peek() {
                    let Some(body) = next.trim_start().strip_prefix("// |") else {
                        break;
                    };
                    text.push_str(body.strip_prefix(' ').unwrap_or(body));
                    text.push('\n');
                    lines.next();
                }
                let (triple, abi) = vsys_target(target)?;
                let text = text.replace("@TARGET@", triple).replace("@ABI@", abi);
                let descriptor = vesper_ffi::sys::parse(&text, vesper_ffi::sys::Limits::default())
                    .map_err(|e| ExportError::Directive(format!("@export vsys: {e}")))?;
                out.vsys.push(VsysImport {
                    library: descriptor.name().into(),
                    handler,
                    symbols: descriptor
                        .functions()
                        .iter()
                        .map(|f| f.symbol().to_string())
                        .collect(),
                    source: text,
                    rust_path,
                });
            } else {
                return Err(ExportError::Directive(format!(
                    "unknown directive '// @export {rest}' (expected abi, instance or vsys)"
                )));
            }
        }
        Ok(out)
    }
}
