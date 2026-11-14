//! §40.3 layout descriptors and typestate obligation graphs, with static SVG.
//!
//! Wire layouts come from Bridge declarations compiled by the real
//! `vesper_ffi::Registry` (explicit endianness, no implicit padding). Current
//! Vesper source has no `wire struct` syntax, so wire declarations are a
//! separate bounded JSON input. Native layouts are the checked source structs'
//! eight-byte-aligned ABI and are labelled as such, never as wire formats.
//! Typestate edges are derived from the validated [`TypestateGraph`].
use crate::{FunctionDoc, NativeLayout, Pass, TypeDoc, TypestateGraph};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use vesper_ffi::{Declaration, Endian, Field, FieldKind, Integer, Registry, WireLayout, WireType};

pub const CACHE_LINE_BYTES: u32 = 64;
pub const WIRE_INPUT_FORMAT: &str = "vesper-wire-layouts";
const MAX_WIRE_INPUT_BYTES: usize = 64 * 1024;
const MAX_DECLARATIONS: usize = 128;
const MAX_FIELDS: usize = 1024;
const MAX_DIAGRAM_BYTES: usize = 64 * 1024;

/// Endianness tag of [`WireFieldSpan::endian_tag`].
pub const ENDIAN_NATIVE: u8 = 0;
pub const ENDIAN_LITTLE: u8 = 1;
pub const ENDIAN_BIG: u8 = 2;
pub const ENDIAN_ZERO_PAD: u8 = 3;

/// The exact 16-byte field byte-span record of §40.3.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireFieldSpan {
    pub byte_offset: u32,
    pub byte_width: u32,
    pub cache_line_idx: u16,
    pub endian_tag: u8,
    pub is_hot_tile_col: u8,
    pub field_sym_id: u32,
}
impl WireFieldSpan {
    /// Little-endian, padding-free encoding: 4 + 4 + 2 + 1 + 1 + 4 bytes.
    pub fn to_bytes(self) -> [u8; 16] {
        let mut out = [0u8; 16];
        out[0..4].copy_from_slice(&self.byte_offset.to_le_bytes());
        out[4..8].copy_from_slice(&self.byte_width.to_le_bytes());
        out[8..10].copy_from_slice(&self.cache_line_idx.to_le_bytes());
        out[10] = self.endian_tag;
        out[11] = self.is_hot_tile_col;
        out[12..16].copy_from_slice(&self.field_sym_id.to_le_bytes());
        out
    }
}

/// Pass-mode tags of [`LinearEdge::pass_mode_tag`].
pub const PASS_INOUT_STEP: u8 = 1;
pub const PASS_TERMINAL_SINK: u8 = 2;
/// A `sink` consumption that produces another tracked typestate (an extension
/// of the two §40.3 tags; such an edge is not a discharge).
pub const PASS_SINK_STEP: u8 = 3;

/// The §40.3 typestate transition edge. Symbol 0 means terminal discharge.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinearEdge {
    pub from_typestate_sym: u32,
    pub method_sym: u32,
    pub pass_mode_tag: u8,
    pub to_typestate_opt: u32,
    pub resumption_is_linear: bool,
}

/// Interned names; ids start at 1 so 0 stays the terminal sentinel.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Symbols {
    pub names: Vec<String>,
}
impl Symbols {
    pub fn intern(&mut self, name: &str) -> Result<u32, String> {
        if let Some(i) = self.names.iter().position(|n| n == name) {
            return u32::try_from(i + 1).map_err(|_| "symbol overflow".into());
        }
        self.names.push(name.into());
        u32::try_from(self.names.len()).map_err(|_| "symbol overflow".into())
    }
    pub fn name(&self, id: u32) -> Option<&str> {
        let i = usize::try_from(id).ok()?.checked_sub(1)?;
        self.names.get(i).map(String::as_str)
    }
}

/// Where a layout's byte offsets come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayoutSource {
    /// A compiled Bridge wire declaration (explicit endian, no implicit padding).
    Wire,
    /// The checked source struct's native eight-byte-aligned ABI.
    Native,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayoutDiagram {
    pub name: String,
    pub source: LayoutSource,
    pub bytes: u32,
    pub alignment: u32,
    pub spans: Vec<WireFieldSpan>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Visuals {
    pub symbols: Symbols,
    pub layouts: Vec<LayoutDiagram>,
    pub unsupported_layouts: Vec<String>,
    pub typestate_edges: Vec<LinearEdge>,
    /// Interned state identities, in graph order.
    pub typestates: Vec<u32>,
    /// Readable type of each state in `typestates`, for rendering only.
    pub typestate_labels: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireInput {
    format: String,
    version: u32,
    declarations: Vec<WireDeclaration>,
    roots: Vec<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireDeclaration {
    name: String,
    fields: Vec<WireField>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireField {
    name: String,
    #[serde(rename = "type")]
    ty: String,
}

/// `u8`, `u16le`, `i32be`, `u64` (native), `bytes[N]`, `pad[N]` or a declared
/// struct name. Anything else is an error.
fn wire_type(spelling: &str, declared: &BTreeMap<&str, ()>) -> Result<WireType, String> {
    let bracket = |prefix: &str| {
        spelling
            .strip_prefix(prefix)
            .and_then(|s| s.strip_suffix(']'))
            .map(|n| {
                n.parse::<usize>()
                    .map_err(|_| format!("invalid width in '{spelling}'"))
            })
    };
    if let Some(n) = bracket("bytes[") {
        return Ok(WireType::ByteArray(n?));
    }
    if let Some(n) = bracket("pad[") {
        return Ok(WireType::ZeroPad(n?));
    }
    let integer = match spelling.as_bytes().first() {
        Some(b'u') => Some((false, &spelling[1..])),
        Some(b'i') => Some((true, &spelling[1..])),
        _ => None,
    };
    if let Some((signed, rest)) = integer {
        let (digits, endian) = if let Some(d) = rest.strip_suffix("le") {
            (d, Endian::Little)
        } else if let Some(d) = rest.strip_suffix("be") {
            (d, Endian::Big)
        } else {
            (rest, Endian::Native)
        };
        if let Ok(bits) = digits.parse::<u16>()
            && matches!(bits, 8 | 16 | 32 | 64)
            && !(bits == 8 && endian != Endian::Native)
        {
            return Ok(WireType::Integer(if signed {
                Integer::signed(bits, endian)
            } else {
                Integer::unsigned(bits, endian)
            }));
        }
    }
    if declared.contains_key(spelling) {
        return Ok(WireType::Struct(spelling.into()));
    }
    Err(format!("unsupported wire field type '{spelling}'"))
}

/// Parse and compile a bounded wire-layout JSON input with the Bridge registry.
pub fn compile_wire_input(text: &str) -> Result<Vec<WireLayout>, String> {
    if text.len() > MAX_WIRE_INPUT_BYTES {
        return Err("wire layout input exceeds 64 KiB".into());
    }
    let input: WireInput =
        serde_json::from_str(text).map_err(|e| format!("invalid wire layout input: {e}"))?;
    if input.format != WIRE_INPUT_FORMAT || input.version != 1 {
        return Err(format!(
            "wire layout input must be {WIRE_INPUT_FORMAT} version 1"
        ));
    }
    if input.declarations.len() > MAX_DECLARATIONS
        || input
            .declarations
            .iter()
            .map(|d| d.fields.len())
            .sum::<usize>()
            > MAX_FIELDS
        || input.roots.is_empty()
        || input.roots.len() > MAX_DECLARATIONS
    {
        return Err("wire layout input declaration/field/root budget".into());
    }
    let declared: BTreeMap<&str, ()> = input
        .declarations
        .iter()
        .map(|d| (d.name.as_str(), ()))
        .collect();
    let mut declarations = Vec::new();
    for d in &input.declarations {
        let mut fields = Vec::new();
        for f in &d.fields {
            fields.push(Field::new(f.name.clone(), wire_type(&f.ty, &declared)?));
        }
        declarations.push(Declaration::new(d.name.clone(), fields));
    }
    let registry = Registry::new(declarations).map_err(|e| e.to_string())?;
    let mut layouts = Vec::new();
    for root in &input.roots {
        layouts.push(registry.compile(root).map_err(|e| e.to_string())?);
    }
    Ok(layouts)
}

fn span(offset: usize, width: usize, endian: u8, sym: u32) -> Result<WireFieldSpan, String> {
    let byte_offset = u32::try_from(offset).map_err(|_| "layout offset overflow")?;
    Ok(WireFieldSpan {
        byte_offset,
        byte_width: u32::try_from(width).map_err(|_| "layout width overflow")?,
        cache_line_idx: u16::try_from(byte_offset / CACHE_LINE_BYTES)
            .map_err(|_| "cache line index overflow")?,
        endian_tag: endian,
        // No tiled(N) AoSoA source layout exists yet; nothing is a hot column.
        is_hot_tile_col: 0,
        field_sym_id: sym,
    })
}

fn flatten(
    layout: &WireLayout,
    base: usize,
    prefix: &str,
    symbols: &mut Symbols,
    out: &mut Vec<WireFieldSpan>,
) -> Result<(), String> {
    for field in layout.fields() {
        let name = if prefix.is_empty() {
            field.name().to_string()
        } else {
            format!("{prefix}.{}", field.name())
        };
        let offset = base + field.offset();
        match field.kind() {
            FieldKind::Struct(nested) => flatten(nested, offset, &name, symbols, out)?,
            kind => {
                let endian = match kind {
                    FieldKind::Padding => ENDIAN_ZERO_PAD,
                    FieldKind::Integer(i) if i.bits > 8 => match i.endian {
                        Endian::Native => ENDIAN_NATIVE,
                        Endian::Little => ENDIAN_LITTLE,
                        Endian::Big => ENDIAN_BIG,
                    },
                    _ => ENDIAN_NATIVE,
                };
                out.push(span(offset, field.size(), endian, symbols.intern(&name)?)?);
            }
        }
        if out.len() > MAX_FIELDS {
            return Err("layout field budget".into());
        }
    }
    Ok(())
}

/// Spans of a compiled wire layout, nested structs flattened to dotted names.
pub fn wire_diagram(layout: &WireLayout, symbols: &mut Symbols) -> Result<LayoutDiagram, String> {
    let mut spans = Vec::new();
    flatten(layout, 0, "", symbols, &mut spans)?;
    Ok(LayoutDiagram {
        name: layout.name().into(),
        source: LayoutSource::Wire,
        bytes: u32::try_from(layout.size()).map_err(|_| "layout size overflow")?,
        alignment: u32::try_from(layout.alignment()).map_err(|_| "alignment overflow")?,
        spans,
    })
}

/// Spans of a checked source struct's native layout. Enums (tag plus
/// overlapping variants) and unavailable layouts return the reason instead.
pub fn native_diagram(ty: &TypeDoc, symbols: &mut Symbols) -> Result<LayoutDiagram, String> {
    let name = &ty.declaration.name;
    match &ty.layout {
        NativeLayout::Unsupported { reason } => Err(format!("{name}: {reason}")),
        NativeLayout::Available { .. } if ty.declaration.is_enum => Err(format!(
            "{name}: enum variants overlap; no single byte ruler"
        )),
        NativeLayout::Available {
            alignment,
            bytes,
            variants,
            ..
        } => {
            let mut spans = Vec::new();
            for field in variants.iter().flat_map(|v| &v.fields) {
                let offset = usize::try_from(field.offset).map_err(|_| "negative field offset")?;
                let width = usize::try_from(field.bytes).map_err(|_| "field width overflow")?;
                let sym = symbols.intern(&field.name)?;
                spans.push(span(offset, width, ENDIAN_NATIVE, sym)?);
            }
            spans.sort_by_key(|s| s.byte_offset);
            Ok(LayoutDiagram {
                name: name.clone(),
                source: LayoutSource::Native,
                bytes: *bytes,
                alignment: *alignment,
                spans,
            })
        }
    }
}

/// §40.3 edges for the validated graph. `functions` supplies resumption modes.
pub fn linear_edges(
    graph: &TypestateGraph,
    functions: &[FunctionDoc],
    symbols: &mut Symbols,
) -> Result<(Vec<u32>, Vec<LinearEdge>), String> {
    let mut states = Vec::new();
    for state in &graph.states {
        states.push(symbols.intern(&state.identity)?);
    }
    let mut edges = Vec::new();
    for edge in &graph.edges {
        let linear_resumption = functions
            .iter()
            .find(|f| f.name == edge.function)
            .is_some_and(|f| f.effect_resumptions.contains(&Some(crate::Mode::Linear)));
        let to = match &edge.to {
            Some(identity) => symbols.intern(identity)?,
            None => 0,
        };
        let tag = match (edge.pass, to) {
            (Pass::Inout, _) => PASS_INOUT_STEP,
            (Pass::Sink, 0) => PASS_TERMINAL_SINK,
            (Pass::Sink, _) => PASS_SINK_STEP,
            (Pass::In, _) => return Err("borrowed typestate edge in a validated graph".into()),
        };
        edges.push(LinearEdge {
            from_typestate_sym: symbols.intern(&edge.from)?,
            method_sym: symbols.intern(&edge.function)?,
            pass_mode_tag: tag,
            to_typestate_opt: to,
            resumption_is_linear: linear_resumption,
        });
    }
    Ok((states, edges))
}

/// Build every §40.3 record for a document plus optional compiled wire layouts.
pub fn visuals(
    types: &[TypeDoc],
    graph: &TypestateGraph,
    functions: &[FunctionDoc],
    wire: &[WireLayout],
) -> Result<Visuals, String> {
    let mut v = Visuals::default();
    for layout in wire {
        v.layouts.push(wire_diagram(layout, &mut v.symbols)?);
    }
    for ty in types {
        if ty.declaration.mode == crate::Mode::Linear || !ty.declaration.is_enum {
            match native_diagram(ty, &mut v.symbols) {
                Ok(d) => v.layouts.push(d),
                Err(reason) => v.unsupported_layouts.push(reason),
            }
        }
    }
    let (states, edges) = linear_edges(graph, functions, &mut v.symbols)?;
    v.typestates = states;
    v.typestate_labels = graph.states.iter().map(|s| s.ty.to_string()).collect();
    v.typestate_edges = edges;
    Ok(v)
}

pub(crate) fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

const ENDIAN_FILL: [&str; 4] = ["#d9e2f1", "#cfe8d4", "#f4dcc2", "#e4e4e4"];
const ENDIAN_LABEL: [&str; 4] = ["native", "le", "be", "zero pad"];

/// A static SVG byte ruler: one 64-byte row per cache line, 10 px per byte.
pub fn layout_svg(diagram: &LayoutDiagram, symbols: &Symbols) -> Result<String, String> {
    let lines = diagram.bytes.div_ceil(CACHE_LINE_BYTES).max(1);
    if lines > 64 {
        return Err(format!("{}: layout exceeds 64 cache lines", diagram.name));
    }
    let (cell, row) = (10u32, 44u32);
    let width = CACHE_LINE_BYTES * cell + 40;
    let height = lines * row + 10;
    let mut svg = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" role=\"img\" width=\"{width}\" height=\"{height}\" viewBox=\"0 0 {width} {height}\"><title>{} byte layout ({} bytes)</title>",
        escape(&diagram.name),
        diagram.bytes
    );
    for line in 0..lines {
        let y = line * row + 4;
        svg.push_str(&format!(
            "<text x=\"0\" y=\"{}\" font-size=\"10\">L{line}</text><rect x=\"30\" y=\"{y}\" width=\"{}\" height=\"30\" fill=\"none\" stroke=\"#888\"/>",
            y + 18,
            CACHE_LINE_BYTES * cell
        ));
    }
    for s in &diagram.spans {
        let name = symbols.name(s.field_sym_id).unwrap_or("?");
        let mut at = s.byte_offset;
        let end = s
            .byte_offset
            .checked_add(s.byte_width)
            .ok_or("span overflow")?;
        if end > diagram.bytes {
            return Err(format!("{}: field {name} exceeds the layout", diagram.name));
        }
        // A field crossing a cache line is drawn as one piece per line.
        while at < end || (s.byte_width == 0 && at == end) {
            let line = at / CACHE_LINE_BYTES;
            let piece_end = end.min((line + 1) * CACHE_LINE_BYTES);
            let x = 30 + (at % CACHE_LINE_BYTES) * cell;
            let w = (piece_end - at) * cell;
            let y = line * row + 4;
            // Labels are truncated to the piece width (about 5 px per glyph);
            // the full name, offset and endianness stay in the <title>.
            let fit = usize::try_from(w / 5).unwrap_or(0);
            let label: String = if name.chars().count() <= fit {
                name.into()
            } else if fit >= 2 {
                name.chars().take(fit - 1).chain(['…']).collect()
            } else {
                String::new()
            };
            svg.push_str(&format!(
                "<rect x=\"{x}\" y=\"{y}\" width=\"{w}\" height=\"30\" fill=\"{}\" stroke=\"#333\"><title>{} @{}+{} {}</title></rect><text x=\"{}\" y=\"{}\" font-size=\"9\">{}</text>",
                ENDIAN_FILL[usize::from(s.endian_tag.min(3))],
                escape(name),
                s.byte_offset,
                s.byte_width,
                ENDIAN_LABEL[usize::from(s.endian_tag.min(3))],
                x + 2,
                y + 12,
                escape(&label)
            ));
            if piece_end == at {
                break;
            }
            at = piece_end;
        }
        if svg.len() > MAX_DIAGRAM_BYTES {
            return Err("layout diagram byte budget".into());
        }
    }
    svg.push_str("</svg>");
    Ok(svg)
}

/// A static SVG state machine, one row per checked transition: the source
/// typestate, a labelled arrow (dashed for `inout` steps), and the target
/// typestate or the terminal discharge. States without transitions are listed
/// as dead ends.
pub fn typestate_svg(v: &Visuals) -> Result<String, String> {
    let (row, box_w, arrow_w) = (44u32, 200u32, 260u32);
    let label = |sym: u32| -> String {
        v.typestates
            .iter()
            .position(|s| *s == sym)
            .and_then(|i| v.typestate_labels.get(i).cloned())
            .or_else(|| v.symbols.name(sym).map(str::to_string))
            .unwrap_or_else(|| "?".into())
    };
    let dead: Vec<u32> = v
        .typestates
        .iter()
        .copied()
        .filter(|s| !v.typestate_edges.iter().any(|e| e.from_typestate_sym == *s))
        .collect();
    let rows = u32::try_from(v.typestate_edges.len() + dead.len()).map_err(|_| "row overflow")?;
    let width = 2 * box_w + arrow_w + 20;
    let height = rows.max(1) * row + 10;
    let mut svg = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" role=\"img\" width=\"{width}\" height=\"{height}\" viewBox=\"0 0 {width} {height}\"><title>Linear obligation state machine</title><defs><marker id=\"arrow\" markerWidth=\"8\" markerHeight=\"8\" refX=\"8\" refY=\"4\" orient=\"auto\"><path d=\"M0,0 L8,4 L0,8 z\"/></marker></defs>"
    );
    let state_box = |svg: &mut String, x: u32, y: u32, text: &str, terminal: bool| {
        let (fill, stroke, rx) = if terminal {
            ("#e0f2e5", "#2d7a3e", 14)
        } else {
            ("#fff4d6", "#a06b00", 6)
        };
        svg.push_str(&format!(
            "<rect x=\"{x}\" y=\"{y}\" width=\"{box_w}\" height=\"28\" rx=\"{rx}\" fill=\"{fill}\" stroke=\"{stroke}\"/><text x=\"{}\" y=\"{}\" font-size=\"11\">{}</text>",
            x + 8,
            y + 18,
            escape(text)
        ));
    };
    let mut y = 8u32;
    for e in &v.typestate_edges {
        if !v.typestates.contains(&e.from_typestate_sym) {
            return Err("edge from an unknown state".into());
        }
        state_box(&mut svg, 10, y, &label(e.from_typestate_sym), false);
        let (target, terminal) = if e.to_typestate_opt == 0 {
            ("discharged (terminal)".to_string(), true)
        } else if v.typestates.contains(&e.to_typestate_opt) {
            (label(e.to_typestate_opt), false)
        } else {
            return Err("edge to an unknown state".into());
        };
        state_box(&mut svg, box_w + arrow_w + 10, y, &target, terminal);
        let dash = if e.pass_mode_tag == PASS_INOUT_STEP {
            " stroke-dasharray=\"5 3\""
        } else {
            ""
        };
        let mode = match e.pass_mode_tag {
            PASS_INOUT_STEP => "inout step",
            PASS_TERMINAL_SINK => "sink, discharge",
            _ => "sink step",
        };
        svg.push_str(&format!(
            "<line x1=\"{}\" y1=\"{}\" x2=\"{}\" y2=\"{}\" stroke=\"#333\"{dash} marker-end=\"url(#arrow)\"/><text x=\"{}\" y=\"{}\" font-size=\"10\">{} ({mode}){}</text>",
            box_w + 14,
            y + 20,
            box_w + arrow_w + 4,
            y + 20,
            box_w + 20,
            y + 13,
            escape(v.symbols.name(e.method_sym).unwrap_or("?")),
            if e.resumption_is_linear { ", linear resumption" } else { "" }
        ));
        y += row;
        if svg.len() > MAX_DIAGRAM_BYTES {
            return Err("typestate diagram byte budget".into());
        }
    }
    for sym in dead {
        state_box(&mut svg, 10, y, &label(sym), false);
        svg.push_str(&format!(
            "<text x=\"{}\" y=\"{}\" font-size=\"10\" fill=\"#b00020\">no public transition: dead end</text>",
            box_w + 20,
            y + 18
        ));
        y += row;
    }
    svg.push_str("</svg>");
    Ok(svg)
}

const BASE_NOTE: &str = "No audited artifact-bound proof evidence supplied: unverified. Layouts describe the native eight-byte-aligned ABI, not wire formats.";

/// The checked document page plus §40.3 diagrams and proof badges. Badges are
/// computed by [`crate::badges::badges`] from audit reports only.
pub fn render_codex_html(
    document: &crate::Document,
    report: &impl Serialize,
    visuals: &Visuals,
    badges: &[crate::badges::Badge],
    audits_supplied: usize,
    limits: crate::Limits,
) -> Result<Vec<u8>, String> {
    render_codex_page(
        document,
        report,
        visuals,
        badges,
        audits_supplied,
        &PageExtras::default(),
        limits,
    )
}
/// Optional page sections: FieldTree heatmaps and a link to the search page.
#[derive(Clone, Copy, Debug, Default)]
pub struct PageExtras<'a> {
    pub heatmaps: Option<&'a crate::heatmap::Heatmaps>,
    pub search_href: Option<&'a str>,
}
fn certificate_cell(b: &crate::badges::Badge) -> String {
    use crate::badges::CertificateStatus;
    let mut cells = Vec::new();
    for c in &b.certificates {
        cells.push(match (c.status, &c.href, &c.object_id) {
            (CertificateStatus::Linked, Some(href), _) => format!(
                "<a href=\"{}\">{}</a> <code>{}</code>",
                escape(href),
                escape(&c.path),
                escape(c.blake3.get(..16).unwrap_or(""))
            ),
            (CertificateStatus::Linked, None, Some(id)) => {
                format!("{} <code>{}</code>", escape(&c.path), escape(id))
            }
            _ => format!("{}: not in store", escape(&c.path)),
        });
    }
    cells.join("<br>")
}
pub fn render_codex_page(
    document: &crate::Document,
    report: &impl Serialize,
    visuals: &Visuals,
    badges: &[crate::badges::Badge],
    audits_supplied: usize,
    extras: &PageExtras<'_>,
    limits: crate::Limits,
) -> Result<Vec<u8>, String> {
    let base = crate::render_document_html(document, report, limits)?;
    let base = String::from_utf8(base).map_err(|_| "renderer produced non-UTF-8 output")?;
    let body = base
        .strip_suffix("</html>\n")
        .ok_or("invalid static renderer boundary")?;
    let note = if audits_supplied == 0 {
        "No proof-audit report supplied: every item is unverified. Native layouts describe the eight-byte-aligned ABI; wire layouts come only from compiled Bridge declarations.".to_string()
    } else {
        format!(
            "Badges come only from {audits_supplied} supplied kernel-replayed proof-audit report(s): a badge describes Lean theorems about a model, not the Rust or Vesper implementation. Native layouts describe the eight-byte-aligned ABI; wire layouts come only from compiled Bridge declarations."
        )
    };
    let at = body
        .find(BASE_NOTE)
        .ok_or("invalid static renderer header")?;
    let mut out = String::new();
    out.push_str(&body[..at]);
    out.push_str(&escape(&note));
    out.push_str(&body[at + BASE_NOTE.len()..]);
    if let Some(href) = extras.search_href {
        out.push_str(&format!(
            "<p id=\"codex-search-link\"><a href=\"{}\">Search signatures</a></p>",
            escape(href)
        ));
    }
    let with_certs = badges.iter().any(|b| !b.certificates.is_empty());
    out.push_str("<section id=\"codex-badges\"><h2>Proof badges</h2><table><tr><th>Item</th><th>Badge</th><th>Theorems</th><th>Reason</th>");
    if with_certs {
        out.push_str("<th>.olean certificates</th>");
    }
    out.push_str("</tr>");
    for b in badges {
        let color = match b.level {
            crate::badges::BadgeLevel::Proved => "#2d7a3e",
            crate::badges::BadgeLevel::Audited => "#a06b00",
            crate::badges::BadgeLevel::Unverified => "#777",
        };
        let theorems: Vec<String> = b
            .evidence
            .iter()
            .map(|e| {
                format!(
                    "{} [axioms: {}] {} {}",
                    e.theorem,
                    e.axioms.join(", "),
                    e.module,
                    e.artifact_inventory_blake3.get(..16).unwrap_or("")
                )
            })
            .collect();
        out.push_str(&format!(
            "<tr><td>{} {}</td><td><span class=\"badge badge-{}\" style=\"color:#fff;background:{color};padding:0 .4em;border-radius:.3em\">{}</span></td><td>{}</td><td>{}</td></tr>",
            escape(&b.kind),
            escape(&b.item),
            b.level.label(),
            b.level.label(),
            escape(&if theorems.is_empty() { b.anchors.join(", ") } else { theorems.join("; ") }),
            escape(&b.reason)
        ));
        if with_certs {
            let at = out.len() - "</tr>".len();
            out.insert_str(at, &format!("<td>{}</td>", certificate_cell(b)));
        }
    }
    out.push_str("</table></section>");
    if let Some(maps) = extras.heatmaps {
        out.push_str(&crate::heatmap::render(maps));
        if out.len() > limits.max_output_bytes {
            return Err("output byte budget exceeded".into());
        }
    }
    out.push_str("<section id=\"codex-layouts\"><h2>Byte layouts</h2>");
    for d in &visuals.layouts {
        out.push_str(&format!(
            "<article><h3>{} ({} layout, {} bytes, align {})</h3>",
            escape(&d.name),
            match d.source {
                LayoutSource::Wire => "wire",
                LayoutSource::Native => "native ABI",
            },
            d.bytes,
            d.alignment
        ));
        // An oversized diagram is omitted with its reason; the record table
        // and the rest of the document are still rendered.
        match layout_svg(d, &visuals.symbols) {
            Ok(svg) => out.push_str(&svg),
            Err(reason) => out.push_str(&format!("<p>Diagram omitted: {}</p>", escape(&reason))),
        }
        out.push_str("<table><tr><th>Field</th><th>Offset</th><th>Width</th><th>Line</th><th>Endian</th><th>Record</th></tr>");
        for s in &d.spans {
            let hex: String = s.to_bytes().iter().map(|b| format!("{b:02x}")).collect();
            out.push_str(&format!(
                "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td><code>{hex}</code></td></tr>",
                escape(visuals.symbols.name(s.field_sym_id).unwrap_or("?")),
                s.byte_offset,
                s.byte_width,
                s.cache_line_idx,
                ENDIAN_LABEL[usize::from(s.endian_tag.min(3))]
            ));
        }
        out.push_str("</table></article>");
        if out.len() > limits.max_output_bytes {
            return Err("output byte budget exceeded".into());
        }
    }
    for reason in &visuals.unsupported_layouts {
        out.push_str(&format!("<p>Layout unavailable: {}</p>", escape(reason)));
    }
    out.push_str(
        "</section><section id=\"codex-typestates\"><h2>Linear obligation state machine</h2>",
    );
    if visuals.typestates.is_empty() {
        out.push_str("<p>No concrete linear typestates.</p>");
    } else {
        match typestate_svg(visuals) {
            Ok(svg) => out.push_str(&svg),
            Err(reason) => out.push_str(&format!("<p>Diagram omitted: {}</p>", escape(&reason))),
        }
        out.push_str("<p>Edges show checked transitions; a terminal path is not an all-path discharge proof.</p>");
    }
    out.push_str("</section></html>\n");
    if out.len() > limits.max_output_bytes {
        return Err("output byte budget exceeded".into());
    }
    Ok(out.into_bytes())
}
