//! Generates, from `packages/vesper-cranelift/cranelift.vsys`:
//! - `vcl.wit`, the `vesper:cranelift` package text;
//! - `component-type.bin`, that world encoded as the `component-type`
//!   custom section `wasm-component-ld` reads to make the cdylib a
//!   component;
//! - `exports.rs`, one canonical ABI export per function over the
//!   `vesper-cranelift-sys` shim.
//!
//! See the `interface` module below for the mapping.

/// The `vesper:cranelift/vcl` interface, kept inline so the build script is
/// one file (Bazel compiles `build.rs` alone).
#[allow(dead_code)]
mod interface {
    // The `vesper:cranelift/vcl` interface, derived one to one from
    // `packages/vesper-cranelift/cranelift.vsys` (#2210).
    //
    // This file is shared by `build.rs` (which generates the WIT text and the
    // export glue from it) and the crate's tests. The mapping, which the Wasm
    // backend's import lowering follows too:
    //
    // - A handle (`Module`, `Builder`, `Data`, `Object`), in any passing mode,
    //   is a `u64`: the handle's address inside the component, 0 for none.
    //   `ForeignVal[Option[H]]` is the same `u64`.
    // - `in CSlice[T]` is `list<T>`, except that a `u32` element crosses as a
    //   `u64` (the Wasm backend stores it in a word).
    // - `inout CSlice[T]` is `list<T>` passed in and handed back: the function
    //   returns `tuple<R, list<T>>`, and the caller copies the list back over
    //   its buffer. (The callee cannot write into the caller's memory.)
    // - `u32`, `u64`, `i32`, `i64` and `bool` stay as they are; a
    //   `ForeignVal[Unit]` result is no result.
    // - Names become kebab case: `vcl_module_new` is `vcl-module-new`, and every
    //   parameter name is written `%name` so none clashes with a WIT keyword.

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
        /// The WIT element type. The Wasm backend keeps every sequence element
        /// but a byte in an 8-byte word, so a `u32` slice crosses as
        /// `list<u64>` (each element zero-extended) and needs no repacking on
        /// the caller's side.
        pub fn wit(self) -> &'static str {
            match self {
                Elem::U8 => "u8",
                Elem::U32 | Elem::U64 => "u64",
            }
        }
        /// The element's size in the list (the caller's memory).
        pub fn size(self) -> usize {
            match self {
                Elem::U8 => 1,
                Elem::U32 | Elem::U64 => 8,
            }
        }
        /// The shim's element type.
        pub fn rust(self) -> &'static str {
            match self {
                Elem::U8 => "u8",
                Elem::U32 => "u32",
                Elem::U64 => "u64",
            }
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
        /// The type as the `.vsys` writes it (`Module`, `CSlice[u8]`).
        pub ty_text: String,
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
        /// The result inside `ForeignVal[..]` as the `.vsys` writes it.
        pub ret_text: String,
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
            let ret_text = ret.to_string();
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
                    ty_text: pty.trim().to_string(),
                });
            }
            out.push(Func {
                name: name.trim().to_string(),
                params: ps,
                ret,
                ret_text,
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

    /// The size of the component's return area (`RetArea` in src/lib.rs,
    /// which exports.rs sizes with this value).
    pub const RET_AREA_BYTES: usize = 32;

    /// Functions a lent call reads (`vcl_call_*` over a Rust host's crossing).
    /// A component has no Rust host to lend it anything, so they answer as for
    /// no crossing.
    pub fn is_lend_call(f: &Func) -> bool {
        f.params
            .first()
            .is_some_and(|p| p.name == "call" && p.ty == Ty::U64 && p.mode == Mode::Value)
    }

    /// One row's shape letters (see [`vesper_table`]).
    fn shape(p: &Param) -> char {
        match (p.mode, p.ty) {
            (_, Ty::Handle) => 'h',
            (_, Ty::U32 | Ty::I32 | Ty::Bool) => 'w',
            (_, Ty::U64 | Ty::I64) => 'q',
            (Mode::Inout, Ty::Slice(Elem::U8)) => 'A',
            (Mode::Inout, Ty::Slice(Elem::U32)) => 'C',
            (Mode::Inout, Ty::Slice(Elem::U64)) => 'E',
            (_, Ty::Slice(Elem::U8)) => 'a',
            (_, Ty::Slice(Elem::U32)) => 'c',
            (_, Ty::Slice(Elem::U64)) => 'e',
        }
    }

    fn pass(p: &Param) -> char {
        match p.mode {
            Mode::Value | Mode::In => 'i',
            Mode::Inout => 'o',
            Mode::Sink => 's',
        }
    }

    /// `packages/vesper-wasm-backend/src/vcl.ves`: the import table the Wasm
    /// backend lowers `Ffi[Cranelift]` calls with. Row `i` is the core import
    /// `i` of the interface: `name shapes passes result`, where a shape is
    /// `h` handle (i64), `w` 32-bit scalar (i32), `q` 64-bit scalar (i64),
    /// `a`/`c`/`e` an `in` slice of u8/u32/u64 (i32 address, i32 length) and
    /// `A`/`C`/`E` an `inout` one (passed the same way and handed back through
    /// the return area), `-` for no parameters; a pass is `i` (in or plain),
    /// `o` inout or `s` sink; the result is `u` none, `w`, `q`, or `o` an
    /// `Option` of a handle (a u64, 0 for `None`).
    pub fn vesper_table(funcs: &[Func]) -> String {
        let mut s = String::new();
        s.push_str("// Generated from packages/vesper-cranelift/cranelift.vsys by\n");
        s.push_str("// crates/vesper-cranelift-component (#2210). Do not edit: rerun its\n");
        s.push_str("// tests with VESPER_BLESS=1.\n//\n");
        s.push_str("// The `vesper:cranelift/vcl@0.1.0` imports (`vesper_table` in\n");
        s.push_str("// crates/vesper-cranelift-component/build.rs says what the\n");
        s.push_str("// letters of a row mean).\n\n");
        s.push_str(&format!(
            "pub fn vcl_count() -> u64 {{\n    {}\n}}\n\n",
            funcs.len()
        ));
        s.push_str("pub fn vcl_row(i: u64) -> Str {\n");
        for (i, f) in funcs.iter().enumerate() {
            let shapes: String = f.params.iter().map(shape).collect();
            let passes: String = f.params.iter().map(pass).collect();
            let (shapes, passes) = if shapes.is_empty() {
                ("-".to_string(), "-".to_string())
            } else {
                (shapes, passes)
            };
            let ret = match f.ret {
                Ret::Unit => 'u',
                Ret::OptHandle => 'o',
                Ret::Scalar(Ty::U64 | Ty::I64 | Ty::Handle) => 'q',
                Ret::Scalar(_) => 'w',
            };
            s.push_str(&format!(
                "    if i == {i} {{\n        return \"{} {shapes} {passes} {ret}\";\n    }}\n",
                f.name
            ));
        }
        s.push_str("    \"\"\n}\n");
        s
    }

    /// `packages/vesper-stage2/src/vcl_decls.ves`: the `Ffi$Cranelift`
    /// operations the Stage 2 front end declares. Row `i` is
    /// `name param.. result`, where a parameter is its pass (`i` in or
    /// plain, `o` inout, `s` sink) followed by its `.vsys` type and the
    /// result is the type inside `ForeignVal[..]`.
    pub fn stage2_table(funcs: &[Func]) -> String {
        let mut s = String::new();
        s.push_str("// Generated from packages/vesper-cranelift/cranelift.vsys by\n");
        s.push_str("// crates/vesper-cranelift-component (#2210). Do not edit: rerun its\n");
        s.push_str("// tests with VESPER_BLESS=1.\n//\n");
        s.push_str("// The `Ffi$Cranelift` operations (`stage2_table` in\n");
        s.push_str("// crates/vesper-cranelift-component/build.rs says what a row\n");
        s.push_str("// holds).\n\n");
        s.push_str(&format!(
            "pub fn vcl_decl_count() -> u64 {{\n    {}\n}}\n\n",
            funcs.len()
        ));
        s.push_str("pub fn vcl_decl(i: u64) -> Str {\n");
        for (i, f) in funcs.iter().enumerate() {
            let mut row = f.name.clone();
            for p in &f.params {
                row.push(' ');
                row.push(pass(p));
                row.push_str(&p.ty_text);
            }
            row.push(' ');
            row.push_str(&f.ret_text);
            s.push_str(&format!(
                "    if i == {i} {{\n        return \"{row}\";\n    }}\n"
            ));
        }
        s.push_str("    \"\"\n}\n");
        s
    }
}

use interface::{Elem, Func, Mode, Ret, Ty};
use std::fmt::Write as _;
use std::path::PathBuf;

fn main() {
    // Bazel names the file (its execpath); Cargo finds it in the tree.
    println!("cargo:rerun-if-env-changed=VESPER_CRANELIFT_VSYS");
    let vsys = match std::env::var_os("VESPER_CRANELIFT_VSYS") {
        Some(p) => PathBuf::from(p),
        None => PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap())
            .join("../../packages/vesper-cranelift/cranelift.vsys"),
    };
    println!("cargo:rerun-if-changed={}", vsys.display());
    let src = std::fs::read_to_string(&vsys).expect("read cranelift.vsys");
    let funcs = interface::parse(&src).expect("parse cranelift.vsys");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());

    let wit = interface::wit(&funcs);
    std::fs::write(out.join("vcl.wit"), &wit).unwrap();
    std::fs::write(out.join("vcl.ves"), interface::vesper_table(&funcs)).unwrap();
    std::fs::write(out.join("vcl_decls.ves"), interface::stage2_table(&funcs)).unwrap();

    let mut resolve = wit_parser::Resolve::default();
    let pkg = resolve
        .push_str("vcl.wit", &wit)
        .expect("generated WIT parses");
    let world = resolve
        .select_world(&[pkg], Some("cranelift"))
        .expect("world cranelift");
    let section = wit_component::metadata::encode(
        &resolve,
        world,
        wit_component::StringEncoding::UTF8,
        None,
        false,
    )
    .expect("encode component type");
    std::fs::write(out.join("component-type.bin"), &section).unwrap();

    let mut rs = String::new();
    writeln!(
        rs,
        "// Generated by build.rs from cranelift.vsys ({} functions).",
        funcs.len()
    )
    .unwrap();
    writeln!(
        rs,
        "/// The return area's size in bytes.\nconst RET_AREA_BYTES: usize = {};",
        interface::RET_AREA_BYTES
    )
    .unwrap();
    for f in &funcs {
        assert!(
            interface::flat_params(f) <= 16,
            "{}: more than 16 flat parameters",
            f.name
        );
        export(&mut rs, f);
    }
    std::fs::write(out.join("exports.rs"), rs).unwrap();

    let mut px = String::new();
    writeln!(
        px,
        "// Generated by build.rs: native C-ABI proxies that forward each shim\n// function to the component (the tests' `call`)."
    )
    .unwrap();
    for f in &funcs {
        proxy(&mut px, f);
    }
    writeln!(
        px,
        "pub fn proxy_symbols() -> Vec<(&'static str, *const u8)> {{\n    vec!["
    )
    .unwrap();
    for f in &funcs {
        writeln!(px, "        (\"{0}\", proxy_{0} as *const u8),", f.name).unwrap();
    }
    writeln!(px, "    ]\n}}").unwrap();
    std::fs::write(out.join("proxy.rs"), px).unwrap();
}

/// A native function with the shim's C signature that calls the
/// component's export through `call(name, args) -> results` (the tests
/// provide `call` and the `Val` helpers).
fn proxy(px: &mut String, f: &Func) {
    let mut params = Vec::new();
    let mut vals = Vec::new();
    let mut backs = Vec::new();
    for (i, p) in f.params.iter().enumerate() {
        let a = format!("a{i}");
        match p.ty {
            Ty::Slice(e) => {
                let m = if p.mode == Mode::Inout {
                    "mut"
                } else {
                    "const"
                };
                params.push(format!("{a}: *{m} {0}, {a}n: usize", e.rust()));
                let f = if e == Elem::U32 {
                    "list_in_wide"
                } else {
                    "list_in::<_>"
                };
                vals.push(format!("{f}({a}, {a}n)"));
                if p.mode == Mode::Inout {
                    backs.push((a, e));
                }
            }
            Ty::Handle => {
                params.push(format!("{a}: *mut u8"));
                vals.push(format!("Val::U64({a} as usize as u64)"));
            }
            Ty::Bool => {
                params.push(format!("{a}: bool"));
                vals.push(format!("Val::Bool({a})"));
            }
            Ty::U32 => {
                params.push(format!("{a}: u32"));
                vals.push(format!("Val::U32({a})"));
            }
            Ty::U64 => {
                params.push(format!("{a}: u64"));
                vals.push(format!("Val::U64({a})"));
            }
            Ty::I32 => {
                params.push(format!("{a}: i32"));
                vals.push(format!("Val::S32({a})"));
            }
            Ty::I64 => {
                params.push(format!("{a}: i64"));
                vals.push(format!("Val::S64({a})"));
            }
        }
    }
    let (ret, conv) = match f.ret {
        Ret::Unit => (String::new(), None),
        Ret::OptHandle => (
            " -> *mut u8".to_string(),
            Some("val_u64(r) as usize as *mut u8"),
        ),
        Ret::Scalar(Ty::U32) => (" -> u32".into(), Some("val_u64(r) as u32")),
        Ret::Scalar(Ty::U64 | Ty::Handle) => (" -> u64".into(), Some("val_u64(r)")),
        Ret::Scalar(Ty::I32) => (" -> i32".into(), Some("val_u64(r) as i32")),
        Ret::Scalar(Ty::I64) => (" -> i64".into(), Some("val_u64(r) as i64")),
        Ret::Scalar(Ty::Bool) => (" -> bool".into(), Some("val_u64(r) != 0")),
        Ret::Scalar(Ty::Slice(_)) => unreachable!(),
    };
    writeln!(
        px,
        "#[allow(clippy::missing_safety_doc)]\npub unsafe extern \"C\" fn proxy_{}({}){ret} {{",
        f.name,
        params.join(", ")
    )
    .unwrap();
    writeln!(
        px,
        "    let rs = call(\"{}\", &[{}]);",
        interface::kebab(&f.name),
        vals.join(", ")
    )
    .unwrap();
    writeln!(px, "    let parts = result_parts(&rs);").unwrap();
    let first = usize::from(f.ret != Ret::Unit);
    for (j, (a, e)) in backs.iter().enumerate() {
        let f = if *e == Elem::U32 {
            "list_back_wide"
        } else {
            "list_back::<_>"
        };
        writeln!(
            px,
            "    unsafe {{ {f}(&parts[{}], {a}, {a}n) }};",
            first + j
        )
        .unwrap();
    }
    if let Some(c) = conv {
        writeln!(px, "    let r = &parts[0];\n    {c}").unwrap();
    } else {
        writeln!(px, "    let _ = parts;").unwrap();
    }
    writeln!(px, "}}").unwrap();
}

fn flat_rust(t: Ty) -> &'static str {
    match t {
        Ty::U64 | Ty::I64 | Ty::Handle => "i64",
        _ => "i32",
    }
}

fn export(rs: &mut String, f: &Func) {
    let wit_name = interface::kebab(&f.name);
    let export_name = format!("{}{}", interface::EXPORT_PREFIX, wit_name);
    let mem = interface::returns_through_memory(f);
    let mut core_params = Vec::new();
    let mut prologue = String::new();
    let mut args = Vec::new();
    let mut handed_back = Vec::new();
    let mut epilogue = String::new();
    for (i, p) in f.params.iter().enumerate() {
        let a = format!("a{i}");
        match p.ty {
            Ty::Slice(e) => {
                core_params.push(format!("{a}p: *mut u8, {a}n: usize"));
                let wire = wire(e);
                let m = if p.mode == Mode::Inout && e == Elem::U32 {
                    "mut "
                } else {
                    ""
                };
                writeln!(
                    prologue,
                    "        let {m}{a} = List::<{wire}>::lifted({a}p, {a}n);"
                )
                .unwrap();
                // A `u32` slice crosses as words: narrow it for the shim
                // and widen what the shim wrote back.
                let (ptr, mut_ptr) = if e == Elem::U32 {
                    writeln!(
                        prologue,
                        "        let {m}{a}w: Vec<u32> = {a}.as_slice().iter().map(|&x| x as u32).collect();"
                    )
                    .unwrap();
                    if p.mode == Mode::Inout {
                        writeln!(
                            epilogue,
                            "        for (d, s) in {a}.as_mut_slice().iter_mut().zip(&{a}w) {{\n            *d = u64::from(*s);\n        }}"
                        )
                        .unwrap();
                    }
                    (format!("{a}w.as_ptr()"), format!("{a}w.as_mut_ptr()"))
                } else {
                    (format!("{a}.ptr()"), format!("{a}.ptr_mut()"))
                };
                if p.mode == Mode::Inout {
                    args.push(format!("{mut_ptr}, {a}.len()"));
                    handed_back.push((a.clone(), e));
                } else {
                    args.push(format!("{ptr}, {a}.len()"));
                }
            }
            Ty::Bool => {
                core_params.push(format!("{a}: i32"));
                args.push(format!("{a} != 0"));
            }
            Ty::Handle => {
                core_params.push(format!("{a}: i64"));
                args.push(format!("{a} as usize as _"));
            }
            t => {
                core_params.push(format!("{a}: {}", flat_rust(t)));
                args.push(format!("{a} as _"));
            }
        }
    }
    let ret_core = if mem {
        " -> *mut u8".to_string()
    } else {
        match f.ret {
            Ret::Unit => String::new(),
            Ret::OptHandle => " -> i64".into(),
            Ret::Scalar(t) => format!(" -> {}", flat_rust(t)),
        }
    };
    if interface::is_lend_call(f) {
        // No Rust host lends a component anything: the shim answers as for
        // no crossing (and still consumes a `sink` handle it is given).
        args[0] = "0".into();
    }
    let call = format!("vesper_cranelift_sys::{}({})", f.name, args.join(", "));
    writeln!(rs, "#[unsafe(export_name = \"{export_name}\")]").unwrap();
    writeln!(
        rs,
        "unsafe extern \"C\" fn __{}({}){} {{",
        f.name,
        core_params.join(", "),
        ret_core
    )
    .unwrap();
    writeln!(rs, "    unsafe {{").unwrap();
    rs.push_str(&prologue);
    if f.ret == Ret::Unit {
        writeln!(rs, "        {call};").unwrap();
    } else {
        writeln!(rs, "        let r = {call};").unwrap();
    }
    rs.push_str(&epilogue);
    let conv = match f.ret {
        Ret::Unit => String::new(),
        Ret::OptHandle => "r as usize as i64".into(),
        Ret::Scalar(t) => format!("r as {}", flat_rust(t)),
    };
    if !mem {
        writeln!(rs, "        {conv}").unwrap();
    } else {
        let (offsets, size) = interface::result_layout(f);
        assert!(
            size <= interface::RET_AREA_BYTES,
            "{}: result area over {} bytes",
            f.name,
            interface::RET_AREA_BYTES
        );
        writeln!(rs, "        let ret = ret_area();").unwrap();
        let mut k = 0;
        match f.ret {
            Ret::Unit => {}
            Ret::OptHandle | Ret::Scalar(Ty::U64 | Ty::I64 | Ty::Handle) => {
                writeln!(
                    rs,
                    "        ret.add({}).cast::<i64>().write({conv});",
                    offsets[0]
                )
                .unwrap();
                k = 1;
            }
            Ret::Scalar(_) => {
                writeln!(
                    rs,
                    "        ret.add({}).cast::<i32>().write({conv});",
                    offsets[0]
                )
                .unwrap();
                k = 1;
            }
        }
        for (a, _) in &handed_back {
            writeln!(rs, "        {a}.hand_back(ret.add({}));", offsets[k]).unwrap();
            k += 1;
        }
        writeln!(rs, "        ret").unwrap();
    }
    writeln!(rs, "    }}\n}}").unwrap();
    if mem {
        let (offsets, _) = interface::result_layout(f);
        let first = usize::from(f.ret != Ret::Unit);
        writeln!(
            rs,
            "#[unsafe(export_name = \"cabi_post_{export_name}\")]\nunsafe extern \"C\" fn __post_{}(ret: *mut u8) {{\n    unsafe {{",
            f.name
        )
        .unwrap();
        for (j, (_, e)) in handed_back.iter().enumerate() {
            writeln!(
                rs,
                "        List::<{}>::free_handed_back(ret.add({}));",
                wire(*e),
                offsets[first + j]
            )
            .unwrap();
        }
        writeln!(rs, "    }}\n}}").unwrap();
    }
}

/// The Rust type of a list element as it crosses (`Elem::wit`).
fn wire(e: Elem) -> &'static str {
    match e {
        Elem::U8 => "u8",
        Elem::U32 | Elem::U64 => "u64",
    }
}
