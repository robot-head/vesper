//! Synthesis of Bridge `.vsys` sys-specs, Vesper extern declarations, wire
//! layouts and migration reports from a [`Migration`].
//!
//! Every `.vsys` text is re-parsed with `vesper_ffi::sys::parse` before it is
//! returned, so a synthesized spec is always one the Bridge accepts, and every
//! function signature is compared back against the parsed descriptor.

use std::fmt::Write as _;

use vesper_ffi::sys::{
    self, LibraryDescriptor, LinkMode, ParameterType, Pointee, Target, ValueType,
};
use vesper_ffi::{Declaration, Endian, Registry, WireType};

use crate::{
    Access, ErrorKind, LiftedStruct, MigrateError, Migration, Origin, Refusal, RefusalReason,
    SysParam, SysPointee, SysValue,
};

/// Memory a pointer parameter may name, for the aliasing check: the exact
/// pointee type. C also lets `uint32_t *` and `int32_t *` (and any byte
/// pointer) alias; those pairs are not refused here, which is why every lifted
/// pointer carries an `assumed:` line and why the Bridge's `prepare_call`
/// rejects overlapping written ranges at each call.
#[derive(Debug, Clone, PartialEq, Eq)]
enum AliasClass {
    Scalar(sys::Scalar),
    Struct(String),
}

/// The first pair of pointer parameters to the same pointee type where at
/// least one is a *single* pointer and at least one is written. Slice/slice pairs keep the
/// earlier behaviour (an `assumed:` line, plus the Bridge's call-time overlap
/// check in `prepare_call`).
pub fn aliasing_pair(params: &[(String, SysParam)]) -> Option<String> {
    let class = |p: &SysParam| match p {
        SysParam::Slice { .. } | SysParam::ElementSlice { .. } => {
            let (element, access, _) = p.slice_parts()?;
            Some((
                match element {
                    SysPointee::Scalar(s) => AliasClass::Scalar(s),
                    SysPointee::Struct(n) => AliasClass::Struct(n),
                },
                access == Access::Inout,
                false,
            ))
        }
        SysParam::Pointer {
            pointee, access, ..
        } => Some((
            match pointee {
                SysPointee::Scalar(s) => AliasClass::Scalar(*s),
                SysPointee::Struct(n) => AliasClass::Struct(n.clone()),
            },
            access.writes(),
            true,
        )),
        SysParam::Value(_) => None,
    };
    for (i, (a, pa)) in params.iter().enumerate() {
        let Some((ca, wa, sa)) = class(pa) else {
            continue;
        };
        for (b, pb) in &params[i + 1..] {
            let Some((cb, wb, sb)) = class(pb) else {
                continue;
            };
            if ca == cb && (wa || wb) && (sa || sb) {
                return Some(format!(
                    "`{a}` and `{b}` point to the same type and one is written; a signature cannot show they are distinct objects"
                ));
            }
        }
    }
    None
}

/// Refuse the `.vsys` signature of any function whose struct pointee or
/// struct slice element has no checked wire layout.
pub fn check_pointer_pointees(m: &mut Migration) {
    let laid_out: std::collections::BTreeSet<String> = m
        .structs
        .iter()
        .filter(|s| s.wire.is_ok())
        .map(|s| s.name.clone())
        .collect();
    for f in &mut m.functions {
        let Ok(sig) = &f.sys else { continue };
        let missing = sig.params.iter().find_map(|(n, p)| match p {
            SysParam::Pointer {
                pointee: SysPointee::Struct(s),
                ..
            }
            | SysParam::ElementSlice {
                element: SysPointee::Struct(s),
                ..
            } if !laid_out.contains(s) => Some((n.clone(), s.clone())),
            _ => None,
        });
        if let Some((param, s)) = missing {
            let item = match m.origin {
                Origin::CHeader { .. } => format!("fn {}", f.name),
                Origin::RustCrate { .. } => format!("fn {}: vsys", f.name),
            };
            f.sys = Err(Refusal::new(
                item,
                RefusalReason::PointerParameter,
                format!(
                    "parameter `{param}` points to struct `{s}` (one element or a slice), which has no checked wire layout"
                ),
            ));
        }
    }
}

fn wire_field(t: &WireType) -> String {
    match t {
        WireType::Integer(i) => format!(
            "{}{}{}",
            if i.signed { "i" } else { "u" },
            i.bits,
            match i.endian {
                Endian::Native => "",
                Endian::Little => "le",
                Endian::Big => "be",
            }
        ),
        WireType::ByteArray(n) => format!("ByteArray[{n}]"),
        WireType::ZeroPad(n) => format!("ZeroPad[{n}]"),
        WireType::Struct(n) => n.clone(),
        // Not reachable for a declaration that compiled.
        other => format!("{other:?}"),
    }
}

/// Words that cannot be parameter names in Vesper or `.vsys` text.
const RESERVED: &[&str] = &[
    "in",
    "inout",
    "sink",
    "nocapture",
    "fn",
    "let",
    "mut",
    "type",
    "match",
    "if",
    "else",
    "return",
    "eff",
    "handle",
    "with",
    "struct",
    "enum",
    "sys",
    "library",
    "extern",
    "self",
    "Self",
    "Unit",
    "bool",
    "true",
    "false",
];

/// Parameter names that are safe in Vesper and `.vsys` text and distinct
/// within one signature: a reserved word, or a name already taken by another
/// parameter, gets trailing underscores until it is free. Parameter names do
/// not affect the C ABI.
pub fn safe_idents<'a>(names: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let names: Vec<&str> = names.into_iter().collect();
    let mut taken: std::collections::BTreeSet<String> = names
        .iter()
        .filter(|n| !RESERVED.contains(n))
        .map(|n| (*n).to_owned())
        .collect();
    names
        .iter()
        .map(|n| {
            if !RESERVED.contains(n) {
                return (*n).to_owned();
            }
            let mut candidate = format!("{n}_");
            while taken.contains(&candidate) || RESERVED.contains(&candidate.as_str()) {
                candidate.push('_');
            }
            taken.insert(candidate.clone());
            candidate
        })
        .collect()
}

/// Compile every struct's wire declaration through `vesper_ffi::Registry`,
/// turning layout errors (implicit padding, unknown or refused nested structs)
/// into refusals. Iterates to a fixpoint so a refused nested struct also
/// refuses its containers.
pub fn check_wire_layouts(structs: &mut [LiftedStruct]) {
    loop {
        let decls: Vec<Declaration> = structs
            .iter()
            .filter_map(|s| s.wire.as_ref().ok().cloned())
            .collect();
        let Ok(registry) = Registry::new(decls) else {
            // Names are unique by construction; refuse everything rather than guess.
            for s in structs.iter_mut() {
                if s.wire.is_ok() {
                    s.wire = Err(Refusal::new(
                        format!("struct {}", s.name),
                        RefusalReason::WireLayout,
                        "duplicate wire declaration",
                    ));
                }
            }
            return;
        };
        let mut changed = false;
        for s in structs.iter_mut() {
            if s.wire.is_err() {
                continue;
            }
            if let Err(e) = registry.compile(&s.name) {
                s.wire = Err(Refusal::new(
                    format!("struct {}", s.name),
                    RefusalReason::WireLayout,
                    e.to_string(),
                ));
                changed = true;
            }
        }
        if !changed {
            return;
        }
    }
}

/// Compiled byte size of each struct that has a wire layout.
pub fn wire_sizes(m: &Migration) -> Vec<(String, usize, usize)> {
    let decls: Vec<Declaration> = m
        .structs
        .iter()
        .filter_map(|s| s.wire.as_ref().ok().cloned())
        .collect();
    let Ok(registry) = Registry::new(decls) else {
        return vec![];
    };
    m.structs
        .iter()
        .filter(|s| s.wire.is_ok())
        .filter_map(|s| {
            registry
                .compile(&s.name)
                .ok()
                .map(|l| (s.name.clone(), l.size(), l.alignment()))
        })
        .collect()
}

/// Library options for a synthesized `.vsys` spec. They are explicit because
/// a header or crate does not determine them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SysSpecOptions {
    pub library: String,
    pub target: Target,
    pub link: LinkMode,
    pub link_name: String,
    pub artifact_blake3: [u8; 32],
    pub header: Option<String>,
    pub pkg_config: Option<String>,
}

/// A synthesized sys-spec, already parsed by the Bridge.
#[derive(Debug, Clone)]
pub struct SysSpec {
    pub text: String,
    pub descriptor: LibraryDescriptor,
    /// Functions present in the migration but left out of the spec.
    pub omitted: Vec<Refusal>,
}

fn sys_value(v: &SysValue) -> String {
    match v {
        SysValue::Unit => "Unit".into(),
        SysValue::Scalar(s) => s.name().into(),
        SysValue::Bool => "bool".into(),
        SysValue::Enum(n) => n.clone(),
        SysValue::Address {
            pointee,
            nullable: true,
        } => format!("NullablePtr[{}]", pointee.name()),
        SysValue::Address {
            pointee,
            nullable: false,
        } => format!("Ptr[{}]", pointee.name()),
    }
}

fn origin_text(o: &Origin) -> String {
    match o {
        Origin::CHeader { header } => format!("C header `{header}`"),
        Origin::RustCrate { name } => format!("Rust crate `{name}`"),
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

fn value_matches(rendered: &SysValue, parsed: &ValueType) -> bool {
    match (rendered, parsed) {
        (SysValue::Unit, ValueType::Unit) | (SysValue::Bool, ValueType::Bool) => true,
        (SysValue::Scalar(a), ValueType::Scalar(b)) => a == b,
        (SysValue::Enum(a), ValueType::Enum(b)) => a == b.name(),
        (
            SysValue::Address {
                pointee: a,
                nullable: x,
            },
            ValueType::Address {
                pointee: b,
                nullable: y,
            },
        ) => a == b && x == y,
        _ => false,
    }
}

fn pointee_matches(
    parsed: &Pointee,
    rendered: &SysPointee,
    decls: &std::collections::BTreeMap<&str, &Declaration>,
) -> bool {
    match (parsed, rendered) {
        (Pointee::Scalar(x), SysPointee::Scalar(y)) => x == y,
        (Pointee::Struct(x), SysPointee::Struct(y)) => {
            x.name() == y && decls.get(y.as_str()).is_some_and(|d| x.declaration() == *d)
        }
        _ => false,
    }
}

/// Render the `.vsys` spec for every function that has a C-ABI signature,
/// parse it with `vesper_ffi::sys::parse`, and check the parsed descriptor
/// against the migration.
pub fn sys_spec(m: &Migration, options: &SysSpecOptions) -> Result<SysSpec, MigrateError> {
    let fail = |msg: String| MigrateError::new(ErrorKind::Synthesis, None, msg);
    if !m.functions.iter().any(|f| f.sys.is_ok()) {
        return Err(fail(
            "no function has a C-ABI signature, and a .vsys library must declare at least one"
                .into(),
        ));
    }
    let mut out = String::new();
    let _ = writeln!(
        out,
        "// {}: synthesized by vesper migrate from {}.",
        crate::SCHEMA,
        origin_text(&m.origin)
    );
    let _ = writeln!(
        out,
        "// Review every `assumed:` line; finite shadow tests do not prove equivalence."
    );
    let _ = writeln!(out, "sys library {} (", options.library);
    let _ = writeln!(
        out,
        "    target = \"{}\", abi = \"{}\",",
        options.target.triple(),
        options.target.convention().name()
    );
    let _ = writeln!(
        out,
        "    link = \"{}\", name = \"{}\",",
        match options.link {
            LinkMode::Static => "static",
            LinkMode::Dynamic => "dynamic",
        },
        options.link_name
    );
    let _ = write!(
        out,
        "    artifact = \"blake3:{}\"",
        hex(&options.artifact_blake3)
    );
    if let Some(h) = &options.header {
        let _ = write!(out, ",\n    header = \"{h}\"");
    }
    if let Some(p) = &options.pkg_config {
        let _ = write!(out, ",\n    pkg_config = \"{p}\"");
    }
    out.push_str("\n) {\n");
    let mut used_enums = std::collections::BTreeSet::new();
    let mut used_structs = std::collections::BTreeSet::new();
    let mut omitted = vec![];
    let mut fns = String::new();
    for f in &m.functions {
        let sig = match &f.sys {
            Ok(s) => s,
            Err(r) => {
                omitted.push(r.clone());
                continue;
            }
        };
        for a in &f.assumptions {
            let _ = writeln!(fns, "    // assumed: {a}");
        }
        let params: Vec<String> = sig
            .params
            .iter()
            .map(|(n, p)| match p {
                SysParam::Value(v) => {
                    if let SysValue::Enum(e) = v {
                        used_enums.insert(e.clone());
                    }
                    format!("{n}: {}", sys_value(v))
                }
                SysParam::Slice { .. } | SysParam::ElementSlice { .. } => {
                    let (element, access, nullable) = p.slice_parts().expect("slice");
                    if let SysPointee::Struct(st) = &element {
                        used_structs.insert(st.clone());
                    }
                    let slice = format!("CSlice[{}]", element.name());
                    format!(
                        "nocapture {} {n}: {}",
                        match access {
                            Access::In => "in",
                            Access::Inout => "inout",
                        },
                        if nullable {
                            format!("Option[{slice}]")
                        } else {
                            slice
                        }
                    )
                }
                SysParam::Pointer {
                    pointee,
                    access,
                    nullable,
                } => {
                    if let SysPointee::Struct(st) = pointee {
                        used_structs.insert(st.clone());
                    }
                    if *nullable {
                        format!(
                            "nocapture {} {n}: Option[{}]",
                            access.keyword(),
                            pointee.name()
                        )
                    } else {
                        format!("nocapture {} {n}: {}", access.keyword(), pointee.name())
                    }
                }
            })
            .collect();
        if let SysValue::Enum(e) = &sig.result {
            used_enums.insert(e.clone());
        }
        let _ = writeln!(
            fns,
            "    fn {}({}) -> ForeignVal[{}];",
            f.name,
            params.join(", "),
            sys_value(&sig.result)
        );
    }
    for e in &m.enums {
        let Ok(carrier) = &e.carrier else { continue };
        if !used_enums.contains(&e.name) {
            continue;
        }
        let vs: Vec<String> = e
            .variants
            .iter()
            .map(|(n, v)| format!("{n} = {v}"))
            .collect();
        let _ = writeln!(
            out,
            "    enum {}: {} {{ {} }}",
            e.name,
            carrier.name(),
            vs.join(", ")
        );
    }
    // Wire struct pointees and, transitively, the structs they contain.
    let decls: std::collections::BTreeMap<&str, &Declaration> = m
        .structs
        .iter()
        .filter_map(|s| s.wire.as_ref().ok().map(|d| (s.name.as_str(), d)))
        .collect();
    let mut pending: Vec<String> = used_structs.iter().cloned().collect();
    while let Some(n) = pending.pop() {
        let Some(d) = decls.get(n.as_str()) else {
            continue;
        };
        for f in &d.fields {
            if let WireType::Struct(inner) = &f.ty
                && used_structs.insert(inner.clone())
            {
                pending.push(inner.clone());
            }
        }
    }
    for n in &used_structs {
        let Some(d) = decls.get(n.as_str()) else {
            return Err(fail(format!(
                "pointee struct `{n}` has no wire layout; run check_pointer_pointees"
            )));
        };
        let fields: Vec<String> = d
            .fields
            .iter()
            .map(|f| format!("{}: {}", f.name, wire_field(&f.ty)))
            .collect();
        let _ = writeln!(out, "    wire struct {n} {{ {} }}", fields.join(", "));
    }
    out.push_str(&fns);
    out.push_str("}\n");
    let descriptor = sys::parse(&out, sys::Limits::default()).map_err(|e| {
        fail(format!(
            "synthesized .vsys was rejected by vesper-ffi: {e:?}"
        ))
    })?;
    // Compare the parsed descriptor back against what was rendered.
    for f in m.functions.iter().filter(|f| f.sys.is_ok()) {
        let Ok(sig) = &f.sys else { continue };
        let parsed = descriptor
            .function(&f.name)
            .ok_or_else(|| fail(format!("function `{}` missing after round-trip", f.name)))?;
        let same_params = parsed.parameters().len() == sig.params.len()
            && parsed
                .parameters()
                .iter()
                .zip(&sig.params)
                .all(|(p, (n, s))| {
                    p.name() == n
                        && match (p.ty(), s) {
                            (ParameterType::Value(a), SysParam::Value(b)) => value_matches(b, a),
                            (
                                ParameterType::Slice {
                                    element,
                                    access,
                                    nullable,
                                },
                                SysParam::Slice { .. } | SysParam::ElementSlice { .. },
                            ) => {
                                let (e, a, z) = s.slice_parts().expect("slice");
                                access == &a
                                    && *nullable == z
                                    && pointee_matches(element, &e, &decls)
                            }
                            (
                                ParameterType::Pointer {
                                    pointee,
                                    access,
                                    nullable,
                                },
                                SysParam::Pointer {
                                    pointee: e,
                                    access: a,
                                    nullable: z,
                                },
                            ) => {
                                access == a && nullable == z && pointee_matches(pointee, e, &decls)
                            }
                            _ => false,
                        }
                });
        if !same_params || !value_matches(&sig.result, parsed.result()) {
            return Err(fail(format!("function `{}` changed in round-trip", f.name)));
        }
    }
    Ok(SysSpec {
        text: out,
        descriptor,
        omitted,
    })
}

/// Render Vesper declarations: structs, enums and `extern fn` signatures with
/// lifted parameter modes and FieldTree footprints.
pub fn vesper_declarations(m: &Migration) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "// {}: Vesper declarations lifted by vesper migrate from {}.",
        crate::SCHEMA,
        origin_text(&m.origin)
    );
    let _ = writeln!(
        out,
        "// Modes summarize signatures and body field accesses; they are not a check of Vesper typing rules."
    );
    for s in &m.structs {
        out.push('\n');
        if s.wire.is_ok() {
            let _ = writeln!(out, "@wire_layout(native)");
        }
        let _ = writeln!(out, "struct {} {{", s.name);
        for f in &s.fields {
            let _ = writeln!(out, "    {}: {},", f.name, f.ty);
        }
        out.push_str("}\n");
    }
    for e in &m.enums {
        out.push('\n');
        let vs: Vec<String> = e
            .variants
            .iter()
            .map(|(n, v)| format!("{n} = {v}"))
            .collect();
        match &e.carrier {
            Ok(c) => {
                let _ = writeln!(out, "enum {}: {} {{ {} }}", e.name, c.name(), vs.join(", "));
            }
            Err(_) => {
                let _ = writeln!(out, "enum {} {{ {} }}", e.name, vs.join(", "));
            }
        }
    }
    if !m.functions.is_empty() {
        out.push('\n');
    }
    for f in &m.functions {
        let params: Vec<String> = f.params.iter().map(crate::lift::render_param).collect();
        let _ = writeln!(
            out,
            "extern fn {}({}) -> {};",
            f.name,
            params.join(", "),
            f.result
        );
    }
    out
}

/// A plain-text migration report: what was lifted, what was assumed, which
/// wire layouts compiled, and every refusal with its reason.
pub fn report(m: &Migration) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{} report for {}",
        crate::SCHEMA,
        origin_text(&m.origin)
    );
    let _ = writeln!(out, "\nfunctions lifted: {}", m.functions.len());
    for f in &m.functions {
        let sys = if f.sys.is_ok() { "vsys" } else { "vesper only" };
        let _ = writeln!(out, "  {} [{sys}]", f.name);
        for a in &f.assumptions {
            let _ = writeln!(out, "    assumed: {a}");
        }
    }
    let sizes = wire_sizes(m);
    let _ = writeln!(out, "\nwire layouts: {}", sizes.len());
    for (n, size, align) in &sizes {
        let _ = writeln!(out, "  {n}: {size} bytes, align {align}");
    }
    let refusals = m.all_refusals();
    let _ = writeln!(out, "\nrefusals: {}", refusals.len());
    for r in refusals {
        let _ = writeln!(out, "  {} ({:?}): {}", r.item, r.reason, r.detail);
    }
    let _ = writeln!(
        out,
        "\nlimits: finite differential shadowing is not an equivalence proof; MPK/PKRU isolation was not exercised."
    );
    out
}
