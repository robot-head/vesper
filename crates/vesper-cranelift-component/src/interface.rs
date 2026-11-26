//! The `vesper:cranelift/vcl` interface, derived one to one from
//! `packages/vesper-cranelift/cranelift.vsys` (#2210).
//!
//! This file is shared by `build.rs` (which generates the WIT text and the
//! export glue from it) and the crate's tests. The mapping, which the Wasm
//! backend's import lowering follows too:
//!
//! - A handle (`Module`, `Builder`, `Data`, `Object`), in any passing mode,
//!   is a `u64`: the handle's address inside the component, 0 for none.
//!   `ForeignVal[Option[H]]` is the same `u64`.
//! - `in CSlice[T]` is `list<T>`.
//! - `inout CSlice[T]` is `list<T>` passed in and handed back: the function
//!   returns `tuple<R, list<T>>`, and the caller copies the list back over
//!   its buffer. (The callee cannot write into the caller's memory.)
//! - `u32`, `u64`, `i32`, `i64` and `bool` stay as they are; a
//!   `ForeignVal[Unit]` result is no result.
//! - Names become kebab case: `vcl_module_new` is `vcl-module-new`, and every
//!   parameter name is written `%name` so none clashes with a WIT keyword.

/// A parameter or result type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ty {
    U32,
    U64,
    I32,
    I64,
    Bool,
    /// A linear extern type of the library: a `u64` handle.
    Handle,
    /// `CSlice[T]` of a scalar element type.
    Slice(Elem),
}

/// A slice element type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Elem {
    U8,
    U32,
    U64,
}

impl Elem {
    pub fn wit(self) -> &'static str {
        match self {
            Elem::U8 => "u8",
            Elem::U32 => "u32",
            Elem::U64 => "u64",
        }
    }
    pub fn size(self) -> usize {
        match self {
            Elem::U8 => 1,
            Elem::U32 => 4,
            Elem::U64 => 8,
        }
    }
    pub fn rust(self) -> &'static str {
        self.wit()
    }
}

/// How a parameter is passed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// A plain value (no mode keyword).
    Value,
    In,
    Inout,
    Sink,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Param {
    pub name: String,
    pub mode: Mode,
    pub ty: Ty,
}

impl Param {
    /// Whether the parameter is handed back in the result (`inout CSlice`).
    pub fn handed_back(&self) -> Option<Elem> {
        match (self.mode, self.ty) {
            (Mode::Inout, Ty::Slice(e)) => Some(e),
            _ => None,
        }
    }
}

/// The `ForeignVal[...]` result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ret {
    Unit,
    Scalar(Ty),
    /// `Option[H]` of a handle: the handle, 0 for `None`.
    OptHandle,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Func {
    pub name: String,
    pub params: Vec<Param>,
    pub ret: Ret,
}

/// The library's handle types, `linear extern type` names.
fn handle_types(src: &str) -> Vec<String> {
    src.lines()
        .filter_map(|l| {
            l.trim()
                .strip_prefix("linear extern type ")
                .and_then(|r| r.strip_suffix(';'))
                .map(str::to_string)
        })
        .collect()
}

fn scalar(t: &str) -> Option<Ty> {
    Some(match t {
        "u32" => Ty::U32,
        "u64" => Ty::U64,
        "i32" => Ty::I32,
        "i64" => Ty::I64,
        "bool" => Ty::Bool,
        _ => return None,
    })
}

fn ty(t: &str, handles: &[String]) -> Result<Ty, String> {
    if let Some(s) = scalar(t) {
        return Ok(s);
    }
    if handles.iter().any(|h| h == t) {
        return Ok(Ty::Handle);
    }
    if let Some(e) = t.strip_prefix("CSlice[").and_then(|r| r.strip_suffix(']')) {
        return Ok(Ty::Slice(match e {
            "u8" => Elem::U8,
            "u32" => Elem::U32,
            "u64" => Elem::U64,
            _ => return Err(format!("unsupported slice element `{e}`")),
        }));
    }
    Err(format!("unsupported type `{t}`"))
}

/// Every `fn` of a `.vsys` library, in declaration order.
pub fn parse(src: &str) -> Result<Vec<Func>, String> {
    let handles = handle_types(src);
    let mut out = Vec::new();
    for line in src.lines().map(str::trim) {
        let Some(rest) = line.strip_prefix("fn ") else {
            continue;
        };
        let err = |m: String| format!("{m} in `{line}`");
        let (name, rest) = rest.split_once('(').ok_or_else(|| err("no `(`".into()))?;
        let (params, rest) = rest.split_once(')').ok_or_else(|| err("no `)`".into()))?;
        let ret = rest
            .trim()
            .strip_prefix("->")
            .map(str::trim)
            .and_then(|r| r.strip_suffix(';'))
            .and_then(|r| r.strip_prefix("ForeignVal["))
            .and_then(|r| r.strip_suffix(']'))
            .ok_or_else(|| err("result is not ForeignVal[..]".into()))?;
        let ret = if ret == "Unit" {
            Ret::Unit
        } else if let Some(h) = ret
            .strip_prefix("Option[")
            .and_then(|r| r.strip_suffix(']'))
        {
            if !handles.iter().any(|x| x == h) {
                return Err(err(format!("Option of non-handle `{h}`")));
            }
            Ret::OptHandle
        } else {
            Ret::Scalar(scalar(ret).ok_or_else(|| err(format!("result `{ret}`")))?)
        };
        let mut ps = Vec::new();
        for p in params.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            let p = p.strip_prefix("nocapture ").unwrap_or(p);
            let (mode, p) = if let Some(r) = p.strip_prefix("inout ") {
                (Mode::Inout, r)
            } else if let Some(r) = p.strip_prefix("in ") {
                (Mode::In, r)
            } else if let Some(r) = p.strip_prefix("sink ") {
                (Mode::Sink, r)
            } else {
                (Mode::Value, p)
            };
            let (pname, pty) = p
                .split_once(':')
                .ok_or_else(|| err(format!("parameter `{p}`")))?;
            ps.push(Param {
                name: pname.trim().to_string(),
                mode,
                ty: ty(pty.trim(), &handles).map_err(err)?,
            });
        }
        out.push(Func {
            name: name.trim().to_string(),
            params: ps,
            ret,
        });
    }
    Ok(out)
}

pub const PACKAGE: &str = "vesper:cranelift@0.1.0";
pub const INTERFACE: &str = "vcl";
/// The core export (and import) name prefix of the interface's functions.
pub const EXPORT_PREFIX: &str = "vesper:cranelift/vcl@0.1.0#";

pub fn kebab(name: &str) -> String {
    name.replace('_', "-")
}

fn wit_ty(t: Ty) -> String {
    match t {
        Ty::U32 => "u32".into(),
        Ty::U64 | Ty::Handle => "u64".into(),
        Ty::I32 => "s32".into(),
        Ty::I64 => "s64".into(),
        Ty::Bool => "bool".into(),
        Ty::Slice(e) => format!("list<{}>", e.wit()),
    }
}

/// The WIT result of `f`, if any.
pub fn wit_result(f: &Func) -> Option<String> {
    let mut parts = Vec::new();
    match f.ret {
        Ret::Unit => {}
        Ret::OptHandle => parts.push("u64".to_string()),
        Ret::Scalar(t) => parts.push(wit_ty(t)),
    }
    for p in &f.params {
        if let Some(e) = p.handed_back() {
            parts.push(format!("list<{}>", e.wit()));
        }
    }
    match parts.len() {
        0 => None,
        1 => parts.pop(),
        _ => Some(format!("tuple<{}>", parts.join(", "))),
    }
}

/// The WIT package text: interface `vcl` and world `cranelift` exporting it.
pub fn wit(funcs: &[Func]) -> String {
    let mut s = String::new();
    s.push_str("// Generated from packages/vesper-cranelift/cranelift.vsys by\n");
    s.push_str("// crates/vesper-cranelift-component (#2210). Do not edit.\n");
    s.push_str(&format!("package {PACKAGE};\n\ninterface {INTERFACE} {{\n"));
    for f in funcs {
        let params: Vec<String> = f
            .params
            .iter()
            .map(|p| format!("%{}: {}", kebab(&p.name), wit_ty(p.ty)))
            .collect();
        s.push_str(&format!(
            "    %{}: func({})",
            kebab(&f.name),
            params.join(", ")
        ));
        if let Some(r) = wit_result(f) {
            s.push_str(&format!(" -> {r}"));
        }
        s.push_str(";\n");
    }
    s.push_str("}\n\nworld cranelift {\n    export vcl;\n}\n");
    s
}

/// The canonical ABI layout of `f`'s result when it goes through memory
/// (more than one flat value): the byte offset of each part (the scalar
/// result first, then each handed-back list) and the area's size.
pub fn result_layout(f: &Func) -> (Vec<usize>, usize) {
    let mut offsets = Vec::new();
    let mut at = 0usize;
    let mut align_of_all = 1usize;
    let mut place = |size: usize, align: usize, at: &mut usize| {
        *at = at.div_ceil(align) * align;
        offsets.push(*at);
        *at += size;
        align_of_all = align_of_all.max(align);
    };
    match f.ret {
        Ret::Unit => {}
        Ret::OptHandle | Ret::Scalar(Ty::U64 | Ty::I64 | Ty::Handle) => place(8, 8, &mut at),
        Ret::Scalar(_) => place(4, 4, &mut at),
    }
    for p in &f.params {
        if p.handed_back().is_some() {
            place(8, 4, &mut at);
        }
    }
    (offsets, at.div_ceil(align_of_all) * align_of_all)
}

/// Whether `f`'s result goes through a return area (its flat form has more
/// than one value).
pub fn returns_through_memory(f: &Func) -> bool {
    f.params.iter().any(|p| p.handed_back().is_some())
}

/// The number of flat (core) parameters of `f`.
pub fn flat_params(f: &Func) -> usize {
    f.params
        .iter()
        .map(|p| if matches!(p.ty, Ty::Slice(_)) { 2 } else { 1 })
        .sum()
}

/// Functions a lent call reads (`vcl_call_*` over a Rust host's crossing).
/// A component has no Rust host to lend it anything, so they answer as for
/// no crossing.
pub fn is_lend_call(f: &Func) -> bool {
    f.params
        .first()
        .is_some_and(|p| p.name == "call" && p.ty == Ty::U64 && p.mode == Mode::Value)
}
