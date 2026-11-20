//! The export ABI v4 additions to a binding (`// @export abi 4`, #1149):
//! compiler trees across the boundary.
//!
//! * **Owned trees.** Every handle type whose fields are all tree forms
//!   (copy values, `String`, `Box[T]`, `Option[T]`, `ChunkSeq` of copy
//!   values or of handle types, and other such handle types) gets a plain
//!   Rust mirror `<Type>Tree` (`Box<_>`, `String`, `Vec<_>`, `Option<_>`),
//!   with `<Type>::encode(&tree)`, which builds the Vesper value once in
//!   this library's allocations, and `<Type>Ref::decode()` /
//!   `<Type>::decode()`, which copies a Vesper value into a tree once.
//! * **Lent views.** The v2 `<Type>Ref` views read a value in place; v4
//!   also reads a `ChunkSeq` of handle elements as `<Elem>Seq<'a>`, whose
//!   elements are `<Elem>Ref<'a>` views, so a whole Vesper tree can be
//!   walked from Rust without a copy.
//! * **Lent Rust handles.** An `in` parameter of a `.vsys` extern type
//!   `H` is `H<'a>`, made with `unsafe { H::lend(&value) }` from the Rust value the
//!   accessor imports expect (the caller vouches for its type): the
//!   handle word is the value's address, which Vesper passes on to the
//!   library's accessor imports (implemented by the linking crate) and
//!   never reads.
use super::*;

/// The handle types with a tree: those whose fields all have tree forms,
/// where a field naming another handle type (directly or as a sequence
/// element) counts only if that type has a tree too. The greatest fixed
/// point, so recursive trees keep theirs.
pub(super) fn tree_types(g: &Gen<'_>) -> BTreeSet<String> {
    let mut ok: BTreeSet<String> = g
        .types()
        .values()
        .filter(|d| d.handle)
        .map(|d| d.name.clone())
        .collect();
    loop {
        let next: BTreeSet<String> = g
            .types()
            .values()
            .filter(|d| ok.contains(&d.name))
            .filter(|d| {
                d.variants
                    .iter()
                    .flat_map(|v| &v.fields)
                    .all(|f| tree_form(g, &ok, &f.ty))
            })
            .map(|d| d.name.clone())
            .collect();
        if next == ok {
            return ok;
        }
        ok = next;
    }
}

fn tree_form(g: &Gen<'_>, ok: &BTreeSet<String>, ty: &AbiType) -> bool {
    match ty {
        _ if !g.owning(ty) && !matches!(ty, AbiType::Extern { .. }) => true,
        AbiType::Str { owned: true } => true,
        AbiType::Box { inner } => {
            !matches!(**inner, AbiType::Object { .. }) && tree_form(g, ok, inner)
        }
        AbiType::Seq { elem } => {
            !g.owning(elem) || seq_of_handles(g, elem).is_some_and(|d| ok.contains(&d.name))
        }
        AbiType::Named { name, args } if name == "Option" => tree_form(g, ok, &args[0]),
        AbiType::Named { name, .. } => ok.contains(name),
        _ => false,
    }
}

/// The handle element type of a `ChunkSeq` that v4 views read.
pub(super) fn seq_of_handles<'a>(g: &'a Gen<'_>, elem: &AbiType) -> Option<&'a TypeDecl> {
    g.handle(elem)
}

/// The Rust type of a tree field.
fn tree_ty(g: &Gen<'_>, ty: &AbiType) -> String {
    match ty {
        _ if !g.owning(ty) => g.copy_type(ty),
        AbiType::Str { .. } => "String".into(),
        AbiType::Box { inner } => format!("Box<{}>", tree_ty(g, inner)),
        AbiType::Seq { elem } => format!("Vec<{}>", tree_ty(g, elem)),
        AbiType::Named { name, args } if name == "Option" => {
            format!("Option<{}>", tree_ty(g, &args[0]))
        }
        AbiType::Named { name, .. } => format!("{name}Tree"),
        other => unreachable!("not a tree form: {other:?}"),
    }
}

/// An expression converting the view value `e` of a field of type `ty`
/// (as `field_view` reads it) into its tree.
fn decode(g: &Gen<'_>, ty: &AbiType, e: &str) -> String {
    match ty {
        _ if !g.owning(ty) => e.into(),
        AbiType::Str { .. } => format!("String::from({e})"),
        // The view of a box is the view of its pointee.
        AbiType::Box { inner } => format!("Box::new({})", decode(g, inner, e)),
        AbiType::Seq { elem } if !g.owning(elem) => format!("{e}.to_vec()"),
        AbiType::Seq { .. } => format!("{e}.iter().map(|x| x.decode()).collect()"),
        AbiType::Named { name, args } if name == "Option" => {
            format!("{e}.map(|x| {})", decode(g, &args[0], "x"))
        }
        AbiType::Named { .. } => format!("{e}.decode()"),
        other => unreachable!("not a tree form: {other:?}"),
    }
}

/// Statements that write the tree value `e` (a reference) of a field of
/// type `ty` into `w[at..]`, a `&mut [u64]` of the value's words.
fn encode(g: &Gen<'_>, ty: &AbiType, e: &str, w: &str, at: u32, out: &mut String, depth: usize) {
    let pad = "    ".repeat(depth);
    match ty {
        _ if !g.owning(ty) => {
            let _ = writeln!(out, "{pad}Abi::put({e}, &mut {w}[{at}..]);");
        }
        AbiType::Str { .. } => {
            let _ = writeln!(
                out,
                "{pad}{w}[{at}..{}].copy_from_slice(&unsafe {{ string_words({e}) }});",
                at + 3
            );
        }
        AbiType::Box { inner } => {
            let words = g.words(inner);
            let _ = writeln!(
                out,
                "{pad}{{\n{pad}    let block = unsafe {{ box_block({words}) }};"
            );
            encode(g, inner, &format!("&**{e}"), "block", 0, out, depth + 1);
            let _ = writeln!(
                out,
                "{pad}    {w}[{at}] = block.as_ptr() as usize as u64;\n{pad}}}"
            );
        }
        AbiType::Seq { elem } if !g.owning(elem) => {
            let _ = writeln!(
                out,
                "{pad}{w}[{at}] = unsafe {{ vesper_port_abi::seq_from(RUNTIME, {e}) }};"
            );
        }
        AbiType::Seq { elem } => {
            let words = g.words(elem);
            let name = &seq_of_handles(g, elem).expect("a tree form").name;
            let _ = writeln!(
                out,
                "{pad}{w}[{at}] = unsafe {{ handle_seq(({e}).len(), {words}, |i, slot| encode_{}(&({e})[i], slot)) }};",
                snake(name)
            );
        }
        AbiType::Named { name, args } if name == "Option" => {
            let _ = writeln!(out, "{pad}match {e} {{");
            let _ = writeln!(out, "{pad}    None => {w}[{at}] = 0,");
            let _ = writeln!(out, "{pad}    Some(x) => {{\n{pad}        {w}[{at}] = 1;");
            encode(g, &args[0], "x", w, at + 1, out, depth + 2);
            let _ = writeln!(out, "{pad}    }}\n{pad}}}");
        }
        AbiType::Named { name, .. } => {
            let words = g.words(ty);
            let _ = writeln!(
                out,
                "{pad}unsafe {{ encode_{}({e}, &mut {w}[{at}..{}]) }};",
                snake(name),
                at + words
            );
        }
        other => unreachable!("not a tree form: {other:?}"),
    }
}

fn field_name(name: &str) -> String {
    if name.chars().all(|c| c.is_ascii_digit()) {
        format!("field{name}")
    } else {
        ident(name)
    }
}

/// The `<Type>Tree` declaration and its encoder and decoder.
fn tree(g: &Gen<'_>, out: &mut String, decl: &TypeDecl) {
    let name = &decl.name;
    let tree = format!("{name}Tree");
    let words = decl.words;
    let _ = writeln!(
        out,
        "/// An owned Rust tree of a Vesper `{name}` value (export ABI v4): `{name}::encode`\n/// builds the Vesper value from it once, and `{name}Ref::decode` reads one back.\n#[derive(Clone, Debug, PartialEq)]"
    );
    if decl.is_enum {
        let _ = writeln!(out, "pub enum {tree} {{");
        for v in &decl.variants {
            let shape = if v.fields.is_empty() {
                String::new()
            } else if positional(v) {
                format!(
                    "({})",
                    v.fields
                        .iter()
                        .map(|f| tree_ty(g, &f.ty))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            } else {
                format!(
                    " {{ {} }}",
                    v.fields
                        .iter()
                        .map(|f| format!("{}: {}", ident(&f.name), tree_ty(g, &f.ty)))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };
            let _ = writeln!(out, "    {}{shape},", v.name);
        }
        out.push_str("}\n\n");
    } else {
        let _ = writeln!(out, "pub struct {tree} {{");
        for f in &decl.variants[0].fields {
            let _ = writeln!(
                out,
                "    pub {}: {},",
                field_name(&f.name),
                tree_ty(g, &f.ty)
            );
        }
        out.push_str("}\n\n");
    }
    // The decoder: through the lent view.
    let _ = writeln!(
        out,
        "impl {name}Ref<'_> {{\n    /// Copies the value into an owned Rust tree (export ABI v4).\n    pub fn decode(&self) -> {tree} {{"
    );
    if decl.is_enum {
        let _ = writeln!(out, "        match self.view() {{");
        for v in &decl.variants {
            let binds = v
                .fields
                .iter()
                .enumerate()
                .map(|(i, _)| format!("f{i}"))
                .collect::<Vec<_>>();
            let (pattern, built) = if v.fields.is_empty() {
                (String::new(), String::new())
            } else if positional(v) {
                (
                    format!("({})", binds.join(", ")),
                    format!(
                        "({})",
                        v.fields
                            .iter()
                            .zip(&binds)
                            .map(|(f, b)| decode(g, &f.ty, b))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                )
            } else {
                (
                    format!(
                        " {{ {} }}",
                        v.fields
                            .iter()
                            .zip(&binds)
                            .map(|(f, b)| format!("{}: {b}", ident(&f.name)))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                    format!(
                        " {{ {} }}",
                        v.fields
                            .iter()
                            .zip(&binds)
                            .map(|(f, b)| format!("{}: {}", ident(&f.name), decode(g, &f.ty, b)))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                )
            };
            let _ = writeln!(
                out,
                "            {name}View::{}{pattern} => {tree}::{}{built},",
                v.name, v.name
            );
        }
        let _ = writeln!(
            out,
            "            {name}View::_Borrow(_) => unreachable!(),\n        }}"
        );
    } else {
        let _ = writeln!(out, "        {tree} {{");
        for f in &decl.variants[0].fields {
            let getter = field_name(&f.name);
            let _ = writeln!(
                out,
                "            {getter}: {},",
                decode(g, &f.ty, &format!("self.{getter}()"))
            );
        }
        let _ = writeln!(out, "        }}");
    }
    out.push_str("    }\n}\n\n");
    // The encoder: straight into the value's words.
    let _ = writeln!(
        out,
        "impl {name} {{\n    /// Builds the Vesper value of `tree` once, in this library's allocations\n    /// (export ABI v4). Dropping the handle frees them.\n    pub fn encode(tree: &{tree}) -> Self {{\n        let mut words = [0u64; {words}];\n        // SAFETY: `words` is the value's size, and every allocation the\n        // encoder makes belongs to this library and passes to the value.\n        unsafe {{ encode_{}(tree, &mut words) }};\n        Self {{ words }}\n    }}\n\n    /// Copies the value into an owned Rust tree (export ABI v4).\n    pub fn decode(&self) -> {tree} {{\n        self.as_ref().decode()\n    }}\n}}\n",
        snake(name)
    );
    let _ = writeln!(
        out,
        "/// # Safety\n/// `w` holds {words} zeroed words; the value written there owns what it allocates.\nunsafe fn encode_{}(tree: &{tree}, w: &mut [u64]) {{",
        snake(name)
    );
    if decl.is_enum {
        let _ = writeln!(out, "    match tree {{");
        for (tag, v) in decl.variants.iter().enumerate() {
            let binds = v
                .fields
                .iter()
                .enumerate()
                .map(|(i, _)| format!("f{i}"))
                .collect::<Vec<_>>();
            let pattern = if v.fields.is_empty() {
                String::new()
            } else if positional(v) {
                format!("({})", binds.join(", "))
            } else {
                format!(
                    " {{ {} }}",
                    v.fields
                        .iter()
                        .zip(&binds)
                        .map(|(f, b)| format!("{}: {b}", ident(&f.name)))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };
            let _ = writeln!(
                out,
                "        {tree}::{}{pattern} => {{\n            w[0] = {tag};",
                v.name
            );
            for (f, b) in v.fields.iter().zip(&binds) {
                encode(g, &f.ty, b, "w", f.offset, out, 3);
            }
            let _ = writeln!(out, "        }}");
        }
        let _ = writeln!(out, "    }}");
    } else {
        for f in &decl.variants[0].fields {
            encode(
                g,
                &f.ty,
                &format!("&tree.{}", field_name(&f.name)),
                "w",
                f.offset,
                out,
                1,
            );
        }
    }
    out.push_str("}\n\n");
}

/// The `<Elem>Seq<'a>` view of a `ChunkSeq` of handle elements.
fn seq_view(out: &mut String, decl: &TypeDecl) {
    let name = &decl.name;
    let words = decl.words;
    let _ = writeln!(
        out,
        "/// A borrowed view of a Vesper `ChunkSeq[{name}]` that a handle owns, read in\n/// place (export ABI v4): its elements are `{name}Ref` views.\n#[derive(Clone, Copy)]\npub struct {name}Seq<'a> {{\n    data: *const u64,\n    len: usize,\n    _owner: core::marker::PhantomData<&'a [u64]>,\n}}\n\nimpl<'a> {name}Seq<'a> {{\n    /// # Safety\n    /// `handle` is 0 (an empty slot) or a live sequence of `{name}` values that\n    /// stays alive and unchanged for `'a`.\n    unsafe fn from_handle(handle: u64) -> Self {{\n        if handle == 0 {{\n            return Self {{ data: core::ptr::null(), len: 0, _owner: core::marker::PhantomData }};\n        }}\n        // SAFETY: a sequence header is `{{len, cap, data}}`.\n        let h = handle as usize as *const u64;\n        unsafe {{ Self {{ data: *h.add(2) as usize as *const u64, len: *h as usize, _owner: core::marker::PhantomData }} }}\n    }}\n\n    pub fn len(&self) -> usize {{\n        self.len\n    }}\n\n    pub fn is_empty(&self) -> bool {{\n        self.len == 0\n    }}\n\n    /// The element at `i`, read in place.\n    pub fn get(&self, i: usize) -> Option<{name}Ref<'a>> {{\n        // SAFETY: element `i < len` is the {words} words at `data + i * {words}`.\n        (i < self.len).then(|| {name}Ref {{ words: unsafe {{ core::slice::from_raw_parts(self.data.add(i * {words}), {words}) }} }})\n    }}\n\n    pub fn iter(&self) -> impl Iterator<Item = {name}Ref<'a>> + 'a {{\n        let s = *self;\n        (0..s.len).map(move |i| s.get(i).expect(\"in bounds\"))\n    }}\n}}\n\nimpl core::fmt::Debug for {name}Seq<'_> {{\n    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {{\n        write!(f, \"{name}Seq(len {{}})\", self.len)\n    }}\n}}\n"
    );
}

/// The `H<'a>` lent handle of a `.vsys` extern type.
fn extern_handle(out: &mut String, name: &str) {
    let _ = writeln!(
        out,
        "/// A Rust value lent to Vesper as the `.vsys` extern-type handle `{name}` (export\n/// ABI v4). The handle word is the value's address: Vesper passes it to the\n/// library's accessor imports, which the linking crate implements over the\n/// same Rust type, and never reads or frees it.\n#[derive(Clone, Copy, Debug)]\npub struct {name}<'a> {{\n    word: u64,\n    _lent: core::marker::PhantomData<&'a ()>,\n}}\n\nimpl<'a> {name}<'a> {{\n    /// Lends `value` for the calls this handle is passed to.\n    ///\n    /// # Safety\n    /// The library's accessor imports for `{name}` (implemented by the linking\n    /// crate) must read the handle word as a `&T`: Vesper checks nothing\n    /// about `T`, so lending a value of another type is undefined behavior.\n    pub unsafe fn lend<T>(value: &'a T) -> Self {{\n        Self {{ word: value as *const T as usize as u64, _lent: core::marker::PhantomData }}\n    }}\n\n    /// The handle word.\n    pub fn word(&self) -> u64 {{\n        self.word\n    }}\n}}\n"
    );
}

/// Every v4 item of the binding: sequence views, trees, lent handles and
/// the encoders' helpers.
pub(super) fn items(g: &Gen<'_>, out: &mut String) {
    let mut seqs = BTreeSet::new();
    for decl in g.types().values().filter(|d| d.handle) {
        for f in decl.variants.iter().flat_map(|v| &v.fields) {
            let mut ty = &f.ty;
            loop {
                match ty {
                    AbiType::Box { inner } => ty = inner,
                    AbiType::Named { name, args } if name == "Option" => ty = &args[0],
                    _ => break,
                }
            }
            if let AbiType::Seq { elem } = ty
                && let Some(e) = seq_of_handles(g, elem)
            {
                seqs.insert(e.name.clone());
            }
        }
    }
    for name in &seqs {
        seq_view(out, &g.types()[name]);
    }
    let mut any_tree = false;
    let trees = tree_types(g);
    for decl in g.types().values().filter(|d| trees.contains(&d.name)) {
        tree(g, out, decl);
        any_tree = true;
    }
    if any_tree {
        let box_new = g
            .interface
            .runtime
            .as_ref()
            .and_then(|r| r.box_new.clone())
            .expect("a v4 library exports box_new");
        let _ = writeln!(
            out,
            "/// # Safety\n/// The block is a zeroed box of this library, owned by the value it is stored in.\nunsafe fn box_block(words: usize) -> &'static mut [u64] {{\n    // SAFETY: `box_new` returns a zeroed block of the requested size.\n    unsafe {{\n        let p = {box_new}((words.max(1) * 8) as i64) as usize as *mut u64;\n        core::slice::from_raw_parts_mut(p, words.max(1))\n    }}\n}}\n\n/// # Safety\n/// The returned words are a `String` that owns a buffer of this library.\nunsafe fn string_words(s: &str) -> [u64; 3] {{\n    // SAFETY: as `vesper_port_abi::with_string_owned`: the header's buffer\n    // passes to the string and only the header is freed.\n    unsafe {{\n        let handle = (RUNTIME.new)(s.len() as i64, 1) as u64;\n        let h = handle as usize as *mut u64;\n        let data = *h.add(2) as usize as *mut u8;\n        if !s.is_empty() {{\n            core::ptr::copy_nonoverlapping(s.as_ptr(), data, s.len());\n        }}\n        let words = [s.len() as u64, *h.add(1), *h.add(2)];\n        *h.add(1) = 0;\n        (RUNTIME.free)(handle as i64);\n        words\n    }}\n}}\n\n/// # Safety\n/// `fill(i, slot)` writes element `i` into its zeroed `words`-word slot; the\n/// sequence owns what the elements allocate.\nunsafe fn handle_seq(len: usize, words: usize, mut fill: impl FnMut(usize, &mut [u64])) -> u64 {{\n    // SAFETY: `seq_new(len, stride)` returns a header `{{len, cap, data}}`\n    // whose buffer holds `len` elements of `words * 8` bytes.\n    unsafe {{\n        let handle = (RUNTIME.new)(len as i64, (words * 8) as i64) as u64;\n        let h = handle as usize as *mut u64;\n        let data = *h.add(2) as usize as *mut u64;\n        for i in 0..len {{\n            let slot = core::slice::from_raw_parts_mut(data.add(i * words), words);\n            slot.fill(0);\n            fill(i, slot);\n        }}\n        *h = len as u64;\n        handle\n    }}\n}}\n"
        );
    }
    let mut externs = BTreeSet::new();
    for f in &g.interface.functions {
        for p in &f.params {
            if let AbiType::Extern { name } = &p.ty {
                externs.insert(name.clone());
            }
        }
    }
    for name in externs {
        extern_handle(out, &name);
    }
}
