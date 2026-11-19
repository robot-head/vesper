//! The Rust binding of an export ABI v2 library (#898).
//!
//! On top of the v1 binding (copy types implement `vesper_port_abi::Abi`,
//! one safe wrapper per function), a v2 binding has:
//!
//! * sequence and slice parameters as `&[T]`, `&mut [T]` and `&mut Vec<T>`,
//!   and sequence results as `Vec<T>`, through the helpers of
//!   `vesper_port_abi` and the library's `RUNTIME`;
//! * one owned handle type per non-copy Vesper type: a newtype over the
//!   value's words whose `Drop` calls the exported drop glue (linear handles
//!   have no `Drop` and are `#[must_use]`), a `<Type>Ref` view with one
//!   getter per field (or a `view()` into a `<Type>View` enum), and methods
//!   for the functions named `<type>_<method>`: `in` makes `&self`, `inout`
//!   makes `&mut self`, by value consumes `self`, and a function that
//!   returns the type without taking it is an associated function;
//! * `live_allocations()`, the library's live sequence and box count.
//!
//! A v3 library (#934) also gets `pub mod repr`: one `#[repr(C)]` mirror per
//! copy struct, with compile-time layout asserts, `from_value`/`to_value`
//! conversions and `vesper_port_abi::Mirror`. Its `in`/`inout` parameters,
//! slices and results of mirrored structs take and return the mirrors, lent
//! in place, and an `in` handle is lent as the caller's own words (#1087).
use super::*;
use std::fmt::Write as _;

struct Gen<'a> {
    interface: &'a LibraryInterface,
    options: &'a BindingOptions,
    types_path: String,
}

fn snake(name: &str) -> String {
    let mut out = String::new();
    for (i, c) in name.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

impl Gen<'_> {
    fn types(&self) -> &BTreeMap<String, TypeDecl> {
        &self.interface.types
    }

    fn handle(&self, ty: &AbiType) -> Option<&TypeDecl> {
        match ty {
            AbiType::Named { name, .. } => self.types().get(name).filter(|d| d.handle),
            _ => None,
        }
    }

    fn owning(&self, ty: &AbiType) -> bool {
        owning(ty, self.types())
    }

    fn words(&self, ty: &AbiType) -> u32 {
        abi_words(ty, self.types())
    }

    /// The Rust type of a copy value.
    fn copy_type(&self, ty: &AbiType) -> String {
        rust_type(ty, &self.types_path)
    }

    /// v3: the mirror of a mirrored struct, or (`abi 3 options`) the
    /// `OptionMirror`/`ResultMirror` of an `Option`/`Result`, as seen from
    /// the binding.
    fn mirror(&self, ty: &AbiType) -> Option<String> {
        match ty {
            AbiType::Named { name, .. } if self.interface.mirrored(ty) => {
                Some(format!("repr::{name}"))
            }
            AbiType::Named { name, args } if self.interface.lent(ty) => {
                let cells = args
                    .iter()
                    .map(|a| self.payload_cell(a))
                    .collect::<Vec<_>>()
                    .join(", ");
                Some(format!("vesper_port_abi::{name}Mirror<{cells}>"))
            }
            _ => None,
        }
    }

    /// v3: the cell of an `Option`/`Result` payload outside `mod repr`.
    fn payload_cell(&self, ty: &AbiType) -> String {
        self.mirror(ty)
            .unwrap_or_else(|| self.cell(ty, &self.types_path))
    }

    /// v3: every `OptionMirror`/`ResultMirror` type a wrapper uses, with
    /// its Vesper size in words.
    fn option_mirrors(&self) -> BTreeMap<String, u32> {
        let mut out = BTreeMap::new();
        for f in &self.interface.functions {
            for ty in f.params.iter().map(|p| &p.ty).chain([&f.result]) {
                if !self.interface.mirrored(ty)
                    && let Some(m) = self.mirror(ty)
                {
                    out.insert(m, self.words(ty));
                }
            }
        }
        out
    }

    /// The Rust type a value of `ty` is handed out as.
    fn out_type(&self, ty: &AbiType) -> String {
        if let Some(m) = self.mirror(ty) {
            return m;
        }
        if !self.owning(ty) {
            return self.copy_type(ty);
        }
        match ty {
            AbiType::Seq { elem } => format!("Vec<{}>", self.copy_type(elem)),
            AbiType::Named { name, args } if name == "Option" => {
                format!("Option<{}>", self.out_type(&args[0]))
            }
            AbiType::Named { name, .. } => name.clone(),
            AbiType::Tuple { items } if items.len() == 1 => {
                format!("({},)", self.out_type(&items[0]))
            }
            AbiType::Tuple { items } => format!(
                "({})",
                items
                    .iter()
                    .map(|t| self.out_type(t))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            other => self.copy_type(other),
        }
    }

    /// The Rust type of a parameter.
    fn param_type(&self, p: &ExportParam) -> String {
        let inout = p.pass() == "inout";
        if let Some(m) = self.mirror(&p.ty) {
            return match p.pass() {
                "in" => format!("&{m}"),
                "inout" => format!("&mut {m}"),
                _ => m,
            };
        }
        match &p.ty {
            AbiType::Seq { elem } if p.pass() == "sink" => format!("Vec<{}>", self.copy_type(elem)),
            AbiType::Slice { elem } if p.in_place => {
                let m = self.mirror(elem).expect("an in-place slice holds mirrors");
                if inout {
                    format!("&mut [{m}]")
                } else {
                    format!("&[{m}]")
                }
            }
            AbiType::Slice { elem } | AbiType::Seq { elem } if !inout => {
                format!("&[{}]", self.copy_type(elem))
            }
            AbiType::Str { .. } if p.pass() == "sink" => "String".into(),
            AbiType::Slice { elem } => format!("&mut [{}]", self.copy_type(elem)),
            AbiType::Seq { elem } => format!("&mut Vec<{}>", self.copy_type(elem)),
            AbiType::Str { .. } if !inout => "&str".into(),
            AbiType::Str { .. } => "&mut String".into(),
            ty if self.handle(ty).is_some() => {
                let name = &self.handle(ty).expect("handle").name;
                match p.pass() {
                    "in" => format!("&{name}"),
                    "inout" => format!("&mut {name}"),
                    _ => name.clone(),
                }
            }
            ty if inout => format!("&mut {}", self.copy_type(ty)),
            ty => self.copy_type(ty),
        }
    }

    /// An expression that takes ownership of a value of `ty` stored in
    /// `buffer[offset..]`.
    fn take(&self, ty: &AbiType, buffer: &str, offset: u32) -> String {
        if let Some(m) = self.mirror(ty) {
            return format!("vesper_port_abi::mirror_from_words::<{m}>(&{buffer}[{offset}..])");
        }
        if !self.owning(ty) {
            return format!(
                "<{} as Abi>::get(&{buffer}[{offset}..])",
                self.copy_type(ty)
            );
        }
        match ty {
            AbiType::Seq { elem } => format!(
                "unsafe {{ vesper_port_abi::seq_take::<{}>(RUNTIME, {buffer}[{offset}]) }}",
                self.copy_type(elem)
            ),
            AbiType::Str { .. } => {
                format!("unsafe {{ vesper_port_abi::string_take(RUNTIME, &{buffer}[{offset}..]) }}")
            }
            AbiType::Named { name, args } if name == "Option" => format!(
                "match {buffer}[{offset}] {{ 0 => None, 1 => Some({}), tag => panic!(\"invalid Option tag {{tag}} from Vesper\") }}",
                self.take(&args[0], buffer, offset + 1)
            ),
            AbiType::Named { name, .. } => {
                format!("unsafe {{ {name}::from_words(&{buffer}[{offset}..]) }}")
            }
            AbiType::Tuple { items } => {
                let mut at = offset;
                let mut parts = vec![];
                for item in items {
                    parts.push(self.take(item, buffer, at));
                    at += self.words(item);
                }
                if parts.len() == 1 {
                    format!("({},)", parts[0])
                } else {
                    format!("({})", parts.join(", "))
                }
            }
            other => format!(
                "<{} as Abi>::get(&{buffer}[{offset}..])",
                self.copy_type(other)
            ),
        }
    }

    /// `(type, expression)` of a borrowed field view of `ty` at
    /// `base[offset..]`, where `base` is a `&'a [u64]`.
    fn field_view(&self, ty: &AbiType, base: &str, offset: u32) -> (String, String) {
        if !self.owning(ty) {
            return (
                self.copy_type(ty),
                format!("<{} as Abi>::get(&{base}[{offset}..])", self.copy_type(ty)),
            );
        }
        match ty {
            // Elements Rust does not read (#1087): only the length.
            AbiType::Seq { elem } if self.owning(elem) => (
                "vesper_port_abi::OpaqueSeq<'a>".into(),
                format!("unsafe {{ vesper_port_abi::OpaqueSeq::from_handle({base}[{offset}]) }}"),
            ),
            AbiType::Box { inner } if matches!(**inner, AbiType::Object { .. }) => (
                "vesper_port_abi::OpaqueObject<'a>".into(),
                format!("vesper_port_abi::OpaqueObject::from_word({base}[{offset}])"),
            ),
            AbiType::Seq { elem } => (
                format!("vesper_port_abi::SeqRef<'a, {}>", self.copy_type(elem)),
                format!("unsafe {{ vesper_port_abi::SeqRef::from_handle({base}[{offset}]) }}"),
            ),
            AbiType::Str { .. } => (
                "&'a str".into(),
                format!("unsafe {{ vesper_port_abi::str_view(&{base}[{offset}..]) }}"),
            ),
            AbiType::Box { inner } => {
                let pointee = format!(
                    "unsafe {{ vesper_port_abi::box_words({base}[{offset}], {}) }}",
                    self.words(inner)
                );
                self.field_view(inner, &format!("({pointee})"), 0)
            }
            AbiType::Named { name, args } if name == "Option" => {
                let (t, e) = self.field_view(&args[0], base, offset + 1);
                (
                    format!("Option<{t}>"),
                    format!("match {base}[{offset}] {{ 0 => None, _ => Some({e}) }}"),
                )
            }
            AbiType::Named { name, .. } => {
                let words = self.words(ty);
                (
                    format!("{name}Ref<'a>"),
                    format!(
                        "{name}Ref {{ words: &{base}[{offset}..{}] }}",
                        offset + words
                    ),
                )
            }
            other => (self.copy_type(other), "unreachable!()".into()),
        }
    }

    fn handle_type(&self, out: &mut String, decl: &TypeDecl) {
        let name = &decl.name;
        let words = decl.words;
        let release = match &decl.drop_symbol {
            Some(symbol) => format!(" Dropping it calls `{symbol}`."),
            None => {
                " It is linear: it has no `Drop`, and only a consuming call releases it.".into()
            }
        };
        let _ = writeln!(
            out,
            "/// Owned handle of a Vesper `{name}` value: its words, which own the Vesper\n/// allocations they point to.{release}"
        );
        if decl.linear {
            let _ = writeln!(
                out,
                "#[must_use = \"a linear Vesper value must be released by a consuming call\"]"
            );
        }
        let _ = writeln!(
            out,
            "pub struct {name} {{\n    words: [u64; {words}],\n}}\n"
        );
        let _ = writeln!(
            out,
            "impl {name} {{\n    /// # Safety\n    /// `words` must hold a `{name}` value of this library that the caller owns.\n    unsafe fn from_words(words: &[u64]) -> Self {{\n        let mut owned = [0u64; {words}];\n        owned.copy_from_slice(&words[..{words}]);\n        Self {{ words: owned }}\n    }}\n\n    /// A borrowed view of the value.\n    pub fn as_ref(&self) -> {name}Ref<'_> {{\n        {name}Ref {{ words: &self.words }}\n    }}\n}}\n"
        );
        if let Some(symbol) = &decl.drop_symbol {
            let _ = writeln!(
                out,
                "impl Drop for {name} {{\n    fn drop(&mut self) {{\n        // SAFETY: the handle owns its words and is dropped once.\n        unsafe {{ {symbol}(self.words.as_mut_ptr()) }};\n    }}\n}}\n"
            );
        }
        let _ = writeln!(
            out,
            "impl core::fmt::Debug for {name} {{\n    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {{\n        f.write_str(\"{name} {{ .. }}\")\n    }}\n}}\n"
        );
        let _ = writeln!(
            out,
            "// SAFETY: the handle owns its Vesper allocations exclusively (no Vesper\n// code keeps a reference to them), and they are plain heap memory.\nunsafe impl Send for {name} {{}}\n// SAFETY: shared access only reads the words and the memory they own.\nunsafe impl Sync for {name} {{}}\n"
        );
        // The view.
        let _ = writeln!(
            out,
            "/// A borrowed view of a Vesper `{name}` value, read in place.\n#[derive(Clone, Copy)]\npub struct {name}Ref<'a> {{\n    words: &'a [u64],\n}}\n"
        );
        let _ = writeln!(out, "impl<'a> {name}Ref<'a> {{");
        if decl.is_enum {
            let _ = writeln!(
                out,
                "    /// The variant and its fields.\n    pub fn view(&self) -> {name}View<'a> {{\n        let w: &'a [u64] = self.words;\n        match w[0] {{"
            );
            for (tag, v) in decl.variants.iter().enumerate() {
                let fields = v
                    .fields
                    .iter()
                    .map(|f| self.field_view(&f.ty, "w", f.offset))
                    .collect::<Vec<_>>();
                let built = if v.fields.is_empty() {
                    String::new()
                } else if positional(v) {
                    format!(
                        "({})",
                        fields
                            .iter()
                            .map(|(_, e)| e.clone())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                } else {
                    format!(
                        " {{ {} }}",
                        v.fields
                            .iter()
                            .zip(&fields)
                            .map(|(f, (_, e))| format!("{}: {e}", ident(&f.name)))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                };
                let _ = writeln!(out, "            {tag} => {name}View::{}{built},", v.name);
            }
            let _ = writeln!(
                out,
                "            tag => panic!(\"invalid {name} tag {{tag}} from Vesper\"),\n        }}\n    }}\n}}\n"
            );
            let _ = writeln!(
                out,
                "/// The variants of a Vesper `{name}` value, with borrowed fields.\n#[derive(Clone, Copy)]\npub enum {name}View<'a> {{"
            );
            for v in &decl.variants {
                let fields = v
                    .fields
                    .iter()
                    .map(|f| (f, self.field_view(&f.ty, "w", f.offset).0))
                    .collect::<Vec<_>>();
                let shape = if v.fields.is_empty() {
                    String::new()
                } else if positional(v) {
                    format!(
                        "({})",
                        fields
                            .iter()
                            .map(|(_, t)| t.clone())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                } else {
                    format!(
                        " {{ {} }}",
                        fields
                            .iter()
                            .map(|(f, t)| format!("{}: {t}", ident(&f.name)))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                };
                let _ = writeln!(out, "    {}{shape},", v.name);
            }
            let _ = writeln!(
                out,
                "    #[doc(hidden)]\n    _Borrow(core::marker::PhantomData<&'a ()>),\n}}\n"
            );
        } else {
            for f in &decl.variants[0].fields {
                let (t, e) = self.field_view(&f.ty, "w", f.offset);
                let getter = if f.name.chars().all(|c| c.is_ascii_digit()) {
                    format!("field{}", f.name)
                } else {
                    ident(&f.name)
                };
                let _ = writeln!(
                    out,
                    "    pub fn {getter}(&self) -> {t} {{\n        let w: &'a [u64] = self.words;\n        {e}\n    }}"
                );
            }
            out.push_str("}\n\n");
        }
    }

    fn function(&self, out: &mut String, f: &ExportedFunction) {
        let value = self.out_type(&f.result);
        let error = match f.errors.as_slice() {
            [] => None,
            [one] => Some(self.copy_type(one)),
            _ if self.options.types_path.is_some() => {
                Some(format!("{}{}Error", self.types_path, camel(&f.name)))
            }
            many => {
                let enum_name = format!("{}Error", camel(&f.name));
                let _ = writeln!(
                    out,
                    "/// The errors `{}` can raise, in status order.\n#[derive(Debug, Clone, Copy, PartialEq)]\npub enum {enum_name} {{",
                    f.name
                );
                for e in many {
                    let _ = writeln!(out, "    {}({}),", error_variant(e), self.copy_type(e));
                }
                out.push_str("}\n\n");
                Some(enum_name)
            }
        };
        let ret = match &error {
            Some(e) => format!("Result<{value}, {e}>"),
            None => value.clone(),
        };
        let params = f
            .params
            .iter()
            .map(|p| format!("{}: {}", ident(&p.name), self.param_type(p)))
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(
            out,
            "/// Calls `{}` of the Vesper library `{}` (`{}`).",
            f.name, self.interface.library, f.symbol
        );
        let _ = writeln!(out, "pub fn {}({params}) -> {ret} {{", ident(&f.name));
        for library in &f.foreign {
            if let Some(l) = self
                .interface
                .foreign
                .iter()
                .find(|l| &l.name == library && l.rust_path.is_some())
            {
                let path = l.rust_path.as_deref().expect("filtered");
                let _ = writeln!(
                    out,
                    "    // Keep the crate's `{}` symbols linked: the Vesper object calls them.\n    let _ = core::hint::black_box([{}]);",
                    l.name,
                    l.symbols
                        .iter()
                        .map(|s| format!("{path}::{s} as *const ()"))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
        }
        // Generated locals never shadow a parameter (or each other).
        let mut names = Names::new(f.params.iter().map(|p| ident(&p.name)));
        let result = names.fresh("result");
        let fail = names.fresh("fail");
        let raw = names.fresh("raw");
        let mut args = vec![];
        let mut wraps: Vec<(String, String)> = vec![];
        let mut after = vec![];
        if f.result_pointer {
            let _ = writeln!(out, "    let mut {result} = [0u64; {}];", f.result_words);
            args.push(format!("{result}.as_mut_ptr()"));
        }
        for p in &f.params {
            let n = ident(&p.name);
            let inout = p.pass() == "inout";
            let view = names.fresh(&format!("view_{}", p.name));
            let seq = names.fresh(&format!("seq_{}", p.name));
            let arg = names.fresh(&format!("arg_{}", p.name));
            match &p.ty {
                AbiType::Slice { .. } if p.in_place => {
                    let helper = if inout {
                        "with_mirror_slice_mut"
                    } else {
                        "with_mirror_slice"
                    };
                    wraps.push((
                        format!("vesper_port_abi::{helper}({n}, |{view}| "),
                        ")".into(),
                    ));
                    args.push(view.clone());
                }
                ty if self.mirror(ty).is_some() => {
                    let m = self.mirror(ty).expect("checked");
                    match p.pass() {
                        // In place: Vesper reads (`in`) or updates
                        // (`inout`) the caller's mirror.
                        "in" => args.push(format!("({n} as *const {m}).cast_mut().cast::<u64>()")),
                        "inout" => args.push(format!("({n} as *mut {m}).cast::<u64>()")),
                        // A consumed value's storage may be reused.
                        _ => {
                            let _ = writeln!(out, "    let mut {arg} = {n};");
                            args.push(format!("(&mut {arg} as *mut {m}).cast::<u64>()"));
                        }
                    }
                }
                AbiType::Slice { .. } => {
                    let helper = if inout {
                        "with_slice_mut"
                    } else {
                        "with_slice"
                    };
                    wraps.push((
                        format!("vesper_port_abi::{helper}({n}, |{view}| "),
                        ")".into(),
                    ));
                    args.push(view.clone());
                }
                AbiType::Str { .. } => {
                    // A by-value string is copied into a buffer of this
                    // library: the callee owns it and may keep it past the
                    // call (in a returned handle, say).
                    let helper = if inout {
                        "with_string_mut(RUNTIME, "
                    } else if p.pass() == "sink" {
                        "with_string_owned(RUNTIME, &"
                    } else {
                        "with_str("
                    };
                    wraps.push((
                        format!("vesper_port_abi::{helper}{n}, |{view}| "),
                        ")".into(),
                    ));
                    args.push(view.clone());
                }
                AbiType::Seq { .. } if inout => {
                    wraps.push((
                        format!("vesper_port_abi::with_seq_mut(RUNTIME, {n}, |{seq}| "),
                        ")".into(),
                    ));
                    args.push(seq.clone());
                }
                AbiType::Seq { .. } if p.pass() == "sink" => {
                    // A by-value sequence is given to the callee, which frees it.
                    let _ = writeln!(
                        out,
                        "    // SAFETY: RUNTIME is this library's runtime.\n    let {seq} = unsafe {{ vesper_port_abi::seq_from(RUNTIME, &{n}) }};"
                    );
                    args.push(format!("{seq} as i64"));
                }
                AbiType::Seq { .. } => {
                    wraps.push((
                        format!("vesper_port_abi::with_seq({n}, |{seq}| "),
                        ")".into(),
                    ));
                    args.push(format!("{seq} as i64"));
                }
                ty if self.handle(ty).is_some() => match p.pass() {
                    // v3 (#1087): lent as the caller's own words.
                    "in" if p.in_place => {
                        args.push(format!("{n}.words.as_ptr().cast_mut()"));
                    }
                    "in" => {
                        let _ = writeln!(out, "    let mut {arg} = {n}.words;");
                        args.push(format!("{arg}.as_mut_ptr()"));
                    }
                    "inout" => args.push(format!("{n}.words.as_mut_ptr()")),
                    _ => {
                        let _ = writeln!(
                            out,
                            "    let mut {arg} = core::mem::ManuallyDrop::new({n});"
                        );
                        args.push(format!("{arg}.words.as_mut_ptr()"));
                    }
                },
                ty if inout => {
                    let t = self.copy_type(ty);
                    let _ = writeln!(
                        out,
                        "    let mut {arg} = [0u64; <{t} as Abi>::WORDS];\n    Abi::put(&*{n}, &mut {arg});"
                    );
                    args.push(format!("{arg}.as_mut_ptr()"));
                    after.push(format!("    *{n} = <{t} as Abi>::get(&{arg});"));
                }
                ty if p.pointer => {
                    let t = self.copy_type(ty);
                    let _ = writeln!(
                        out,
                        "    let mut {arg} = [0u64; <{t} as Abi>::WORDS];\n    Abi::put(&{n}, &mut {arg});"
                    );
                    args.push(format!("{arg}.as_mut_ptr()"));
                }
                ty => args.push(scalar_in(&n, ty)),
            }
        }
        if let Some(words) = f.fail_record_words {
            let _ = writeln!(out, "    let mut {fail} = [0u64; {words}];");
            args.push(format!("{fail}.as_mut_ptr()"));
        }
        let mut call = format!("unsafe {{ {}({}) }}", f.symbol, args.join(", "));
        for (open, close) in wraps.iter().rev() {
            call = format!("{open}{call}{close}");
        }
        let _ = writeln!(
            out,
            "    // SAFETY: `{}` has exactly this signature in the export ABI; every\n    // pointer is a live, writable buffer of the size the ABI requires, and\n    // every handle passed is owned or borrowed as its mode requires.\n    let {raw} = {call};",
            f.symbol
        );
        for line in &after {
            let _ = writeln!(out, "{line}");
        }
        let produce = if f.result_pointer {
            self.take(&f.result, &result, 0)
        } else {
            match &f.result {
                AbiType::Seq { elem } => format!(
                    "unsafe {{ vesper_port_abi::seq_take::<{}>(RUNTIME, {raw} as u64) }}",
                    self.copy_type(elem)
                ),
                other => scalar_out(&raw, other),
            }
        };
        match f.errors.as_slice() {
            [] => {
                let _ = writeln!(out, "    {produce}");
            }
            errors => {
                let _ = writeln!(out, "    match {fail}[0] {{");
                let _ = writeln!(out, "        0 => Ok({produce}),");
                for (k, e) in errors.iter().enumerate() {
                    let decode = format!("<{} as Abi>::get(&{fail}[1..])", self.copy_type(e));
                    if errors.len() == 1 {
                        let _ = writeln!(out, "        {} => Err({decode}),", k + 1);
                    } else {
                        let _ = writeln!(
                            out,
                            "        {} => Err({}::{}({decode})),",
                            k + 1,
                            error.as_deref().unwrap_or_default(),
                            error_variant(e)
                        );
                    }
                }
                let _ = writeln!(
                    out,
                    "        status => panic!(\"invalid failure status {{status}} from {}\"),",
                    f.symbol
                );
                out.push_str("    }\n");
            }
        }
        out.push_str("}\n");
    }

    /// v3: the `repr` module (one `#[repr(C)]` mirror per mirrored copy
    /// struct), the layout asserts, and the conversions.
    fn mirrors(&self, out: &mut String) {
        let decls = self
            .types()
            .values()
            .filter(|d| d.mirror == Some(Mirror::ReprC))
            .collect::<Vec<_>>();
        // Paths as seen from inside `mod repr`.
        let inner_path = match &self.options.types_path {
            Some(p) => format!("{p}::"),
            None => "super::".into(),
        };
        out.push_str(
            "/// `#[repr(C)]` mirrors of the library's copy structs in the Vesper word
/// layout (export ABI v3): `in`/`inout` parameters and slices of them are
/// lent to Vesper in place.
pub mod repr {
",
        );
        for decl in &decls {
            let v = &decl.variants[0];
            let mut fields = v.fields.iter().collect::<Vec<_>>();
            fields.sort_by_key(|f| f.offset);
            let cells = fields
                .iter()
                .map(|f| (f, self.cell(&f.ty, &inner_path)))
                .collect::<Vec<_>>();
            let _ = writeln!(
                out,
                "    /// Mirror of the Vesper copy struct `{}` ({} words).
    #[repr(C)]
    #[derive(Clone, Copy, Debug, PartialEq)]",
                decl.name, decl.words
            );
            if positional(v) {
                let _ = writeln!(
                    out,
                    "    pub struct {}({});
",
                    decl.name,
                    cells
                        .iter()
                        .map(|(_, c)| format!("pub {c}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            } else {
                let _ = writeln!(out, "    pub struct {} {{", decl.name);
                for (f, c) in &cells {
                    let _ = writeln!(out, "        pub {}: {c},", ident(&f.name));
                }
                out.push_str(
                    "    }

",
                );
            }
        }
        out.push_str(
            "}

",
        );
        for decl in &decls {
            let name = &decl.name;
            let words = decl.words;
            let value = format!("{}{name}", self.types_path);
            let _ = writeln!(
                out,
                "// The mirror of `{name}` has exactly the Vesper layout.
const _: () = {{
    assert!(core::mem::size_of::<repr::{name}>() == {words} * 8);
    assert!(core::mem::align_of::<repr::{name}>() == 8);"
            );
            for f in &decl.variants[0].fields {
                let _ = writeln!(
                    out,
                    "    assert!(core::mem::offset_of!(repr::{name}, {}) == {} * 8);",
                    ident(&f.name),
                    f.offset
                );
            }
            out.push_str(
                "};

",
            );
            let _ = writeln!(
                out,
                "// SAFETY: `repr::{name}` is `#[repr(C)]`, {words} words, eight-byte aligned
// (asserted above), and every field is a word cell or a mirror.
unsafe impl vesper_port_abi::Mirror for repr::{name} {{
    const WORDS: usize = {words};
}}
"
            );
            let _ = writeln!(
                out,
                "impl repr::{name} {{
    /// The mirror of a Rust value.
    pub fn from_value(value: &{value}) -> Self {{
        let mut words = [0u64; {words}];
        Abi::put(value, &mut words);
        vesper_port_abi::mirror_from_words(&words)
    }}

    /// The Rust value of the mirror.
    pub fn to_value(&self) -> {value} {{
        <{value} as Abi>::get(vesper_port_abi::mirror_words(self))
    }}
}}
"
            );
        }
    }

    /// v3: the mirror cell of a field of type `ty`, with `path` the prefix
    /// of the library's Rust types inside `mod repr`.
    fn cell(&self, ty: &AbiType, path: &str) -> String {
        match ty {
            AbiType::Scalar { name }
                if matches!(name.as_str(), "i64" | "u64" | "isize" | "usize" | "f64") =>
            {
                name.clone()
            }
            AbiType::Scalar { .. } => format!("vesper_port_abi::Word<{}>", rust_type(ty, path)),
            AbiType::Named { name, .. } if self.interface.mirrored(ty) => name.clone(),
            // An array field (#1091) is its element cells in place.
            AbiType::Tuple { items } if super::array(items).is_some() => {
                let (elem, n) = super::array(items).expect("array");
                format!("[{}; {n}]", self.cell(elem, path))
            }
            ty if self.words(ty) == 1 => {
                format!("vesper_port_abi::Word<{}>", rust_type(ty, path))
            }
            ty => format!(
                "vesper_port_abi::Words<{}, {}>",
                rust_type(ty, path),
                self.words(ty)
            ),
        }
    }

    /// Methods of handle type `decl`: the functions named `<type>_<method>`
    /// that take the handle first, or return it without taking it.
    fn methods(&self, out: &mut String, decl: &TypeDecl) {
        let prefix = format!("{}_", snake(&decl.name));
        let mut lines = vec![];
        for f in &self.interface.functions {
            let Some(method) = f.name.strip_prefix(&prefix).filter(|m| !m.is_empty()) else {
                continue;
            };
            let returns_it =
                |ty: &AbiType| matches!(ty, AbiType::Named { name, .. } if name == &decl.name);
            let receiver = f
                .params
                .first()
                .filter(|p| returns_it(&p.ty))
                .map(|p| match p.pass() {
                    "in" => "&self",
                    "inout" => "&mut self",
                    _ => "self",
                });
            if receiver.is_none() && !returns_it(&f.result) {
                continue;
            }
            let rest = &f.params[usize::from(receiver.is_some())..];
            let mut params = vec![];
            if let Some(r) = receiver {
                params.push(r.to_string());
            }
            params.extend(
                rest.iter()
                    .map(|p| format!("{}: {}", ident(&p.name), self.param_type(p))),
            );
            let mut args = vec![];
            if receiver.is_some() {
                args.push("self".to_string());
            }
            args.extend(rest.iter().map(|p| ident(&p.name)));
            let value = self.out_type(&f.result);
            let ret = match f.errors.as_slice() {
                [] => value,
                [one] => format!("Result<{value}, {}>", self.copy_type(one)),
                _ if self.options.types_path.is_some() => {
                    format!(
                        "Result<{value}, {}{}Error>",
                        self.types_path,
                        camel(&f.name)
                    )
                }
                _ => format!("Result<{value}, {}Error>", camel(&f.name)),
            };
            lines.push(format!(
                "    /// `{}` (`{}`).\n    pub fn {}({}) -> {ret} {{\n        {}({})\n    }}",
                f.name,
                f.symbol,
                ident(method),
                params.join(", "),
                ident(&f.name),
                args.join(", ")
            ));
        }
        if !lines.is_empty() {
            let _ = writeln!(out, "\nimpl {} {{\n{}\n}}", decl.name, lines.join("\n\n"));
        }
    }
}

/// The binding text of a v2 library.
pub(super) fn generate(interface: &LibraryInterface, options: &BindingOptions) -> String {
    let types_path = match &options.types_path {
        Some(p) => format!("{p}::"),
        None => String::new(),
    };
    let g = Gen {
        interface,
        options,
        types_path,
    };
    let mut out = String::new();
    let _ = writeln!(
        out,
        "// @generated by `vesper build --lib` ({}) for library `{}`.\n// Do not edit: regenerate it from the Vesper source.\n",
        interface.abi, interface.library
    );
    out.push_str("#![allow(clippy::all, unused_imports, unused_mut, unused_unsafe, unused_variables, non_snake_case, non_camel_case_types, dead_code)]\n\nuse vesper_port_abi::Abi;\n\n");
    write_copy_types(
        &mut out,
        interface.types.values().filter(|d| !d.handle),
        options,
        &g.types_path,
    );
    for decl in interface.types.values().filter(|d| d.handle) {
        g.handle_type(&mut out, decl);
    }
    if interface.is_v3() {
        g.mirrors(&mut out);
        for (m, words) in g.option_mirrors() {
            let _ = writeln!(
                out,
                "// `{m}` has the Vesper layout of its value.\nconst _: () = assert!(core::mem::size_of::<{m}>() == {words} * 8 && <{m} as vesper_port_abi::Mirror>::WORDS == {words});"
            );
        }
        if interface.option_mirrors {
            out.push('\n');
        }
    }
    // Compile-time cross-check of every copy boundary type's word count.
    let mut checked = BTreeSet::new();
    for f in &interface.functions {
        if !g.owning(&f.result) {
            let _ = checked.insert((g.copy_type(&f.result), f.result_words));
        }
    }
    for decl in interface.types.values().filter(|d| !d.handle) {
        let _ = checked.insert((format!("{}{}", g.types_path, decl.name), decl.words));
    }
    for (ty, words) in &checked {
        let _ = writeln!(
            out,
            "const _: () = assert!(<{ty} as Abi>::WORDS == {words});"
        );
    }
    let runtime = interface
        .runtime
        .as_ref()
        .expect("a v2 library lists its runtime symbols");
    out.push_str("\nunsafe extern \"C\" {\n");
    let _ = writeln!(
        out,
        "    fn {}(capacity: i64, stride: i64) -> i64;\n    fn {}(seq: i64) -> i64;\n    fn {}() -> i64;",
        runtime.seq_new, runtime.seq_free, runtime.live_allocations
    );
    for decl in interface.types.values() {
        if let Some(symbol) = &decl.drop_symbol {
            let _ = writeln!(out, "    fn {symbol}(value: *mut u64) -> i64;");
        }
    }
    for f in &interface.functions {
        let mut params = vec![];
        if f.result_pointer {
            params.push("result: *mut u64".to_string());
        }
        for p in &f.params {
            params.push(format!("{}: {}", ident(&p.name), c_type(&p.ty, p.pointer)));
        }
        if f.fail_record_words.is_some() {
            params.push("fail: *mut u64".into());
        }
        let ret = result_c_type(f);
        let _ = writeln!(out, "    fn {}({}) -> {ret};", f.symbol, params.join(", "));
    }
    out.push_str("}\n\n");
    let _ = writeln!(
        out,
        "/// The sequence runtime of the Vesper library `{0}`.\npub const RUNTIME: vesper_port_abi::SeqRuntime = vesper_port_abi::SeqRuntime {{\n    new: {1},\n    free: {2},\n}};\n\n/// Live sequence and box allocations of the Vesper library `{0}` (for leak tests).\npub fn live_allocations() -> i64 {{\n    // SAFETY: the function only reads two counters.\n    unsafe {{ {3}() }}\n}}",
        interface.library, runtime.seq_new, runtime.seq_free, runtime.live_allocations
    );
    for f in &interface.functions {
        out.push('\n');
        g.function(&mut out, f);
    }
    for decl in interface.types.values().filter(|d| d.handle) {
        g.methods(&mut out, decl);
    }
    out
}

/// Fresh local names for a generated wrapper: a name already taken (by a
/// parameter or an earlier local) gets trailing underscores.
struct Names(BTreeSet<String>);

impl Names {
    fn new(taken: impl IntoIterator<Item = String>) -> Self {
        Self(taken.into_iter().collect())
    }

    fn fresh(&mut self, base: &str) -> String {
        let mut name = base.to_string();
        while self.0.contains(&name) {
            name.push('_');
        }
        self.0.insert(name.clone());
        name
    }
}
