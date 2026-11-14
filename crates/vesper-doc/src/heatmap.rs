//! FieldTree borrow-footprint heatmaps (§40.1 item 1c).
//!
//! A heatmap is computed from checked function contracts only: each
//! parameter's pass mode and its declared projection selector
//! (`inout broker.{index, metrics}`). The checker enforces those footprints:
//! a body access outside a declared projection is `E0304`, overlapping
//! projections are `E0301`, and unknown fields are `E0202`, so a document that
//! exists already satisfies them. Leaves outside every selector of a
//! parameter are *free*: callers may hold disjoint borrows of them across the
//! call (`[T-CallProj]`).
//!
//! The footprint is the declared upper bound the checker enforces, not a
//! may-use analysis of the body: a declared leaf the body never touches is
//! still shown as accessed.
use crate::{Adt, FunctionDoc, Limits, Pass, Selector, Type, TypeDoc, visual::escape};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// (item, reason) pairs.
type Reasons = Vec<(String, String)>;
const MAX_DEPTH: usize = 8;
const MAX_LEAVES: usize = 256;
const MAX_ROWS: usize = 512;
const MAX_CELLS: usize = 65_536;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Access {
    /// Outside every declared projection of the parameter.
    Free,
    /// Shared `in` borrow of the leaf.
    Read,
    /// Exclusive `inout` borrow of the leaf.
    Write,
    /// The whole value is consumed by `sink`.
    Consume,
}
impl Access {
    pub fn label(self) -> &'static str {
        match self {
            Self::Free => "free",
            Self::Read => "read",
            Self::Write => "write",
            Self::Consume => "consume",
        }
    }
    fn letter(self) -> &'static str {
        match self {
            Self::Free => "",
            Self::Read => "R",
            Self::Write => "W",
            Self::Consume => "S",
        }
    }
}

/// One parameter of one function whose type is the heatmap's struct.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FootprintRow {
    pub function: String,
    pub parameter: String,
    pub pass: Pass,
    /// The declared selector as dotted paths; empty means the whole value.
    pub selector: Vec<String>,
    /// One access per leaf of the heatmap, in leaf order.
    pub cells: Vec<Access>,
    /// Leaves left free for concurrent disjoint borrows, by name.
    pub free: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Heatmap {
    pub ty: String,
    /// Dotted FieldTree leaf paths in declaration order.
    pub leaves: Vec<String>,
    pub rows: Vec<FootprintRow>,
    /// Per leaf: how many rows write, read and consume it.
    pub writers: Vec<u32>,
    pub readers: Vec<u32>,
    pub consumers: Vec<u32>,
    /// Leaves whose type is not expanded further (generic, enum, tuple,
    /// undocumented or depth-limited), with the reason.
    pub opaque: Vec<(String, String)>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Heatmaps {
    pub heatmaps: Vec<Heatmap>,
    /// Documented types without a heatmap, with the reason.
    pub unsupported: Vec<(String, String)>,
}

fn product(adt: &Adt) -> Option<&vesper_core::ast::Variant> {
    (!adt.is_enum && adt.generics.is_empty() && adt.variants.len() == 1)
        .then(|| &adt.variants[0])
        .filter(|v| v.named)
}

/// Expand the FieldTree leaves of a nongeneric struct through nested
/// nongeneric documented structs.
fn leaves(root: &Adt, adts: &BTreeMap<&str, &Adt>) -> Result<(Vec<Vec<String>>, Reasons), String> {
    let mut out = Vec::new();
    let mut opaque = Vec::new();
    let mut stack = vec![root.name.clone()];
    fn walk(
        adt: &Adt,
        prefix: &mut Vec<String>,
        adts: &BTreeMap<&str, &Adt>,
        stack: &mut Vec<String>,
        out: &mut Vec<Vec<String>>,
        opaque: &mut Vec<(String, String)>,
    ) -> Result<(), String> {
        let variant = product(adt).ok_or("not a named nongeneric struct")?;
        for field in &variant.fields {
            prefix.push(field.name.clone());
            let nested = match &field.ty {
                Type::Named(n, args) if args.is_empty() => adts.get(n.as_str()).copied(),
                _ => None,
            };
            let reason = match nested {
                Some(inner) if product(inner).is_none() => {
                    Some("enum or generic type is a single leaf")
                }
                Some(inner) if stack.contains(&inner.name) => Some("recursive type is a leaf"),
                Some(_) if prefix.len() >= MAX_DEPTH => Some("FieldTree depth limit"),
                Some(_) => None,
                None => match &field.ty {
                    Type::Named(_, args) if args.is_empty() => None,
                    _ => Some("generic, tuple or function type is a single leaf"),
                },
            };
            match (nested, reason) {
                (Some(inner), None) if !product(inner).unwrap().fields.is_empty() => {
                    stack.push(inner.name.clone());
                    walk(inner, prefix, adts, stack, out, opaque)?;
                    stack.pop();
                }
                _ => {
                    if out.len() == MAX_LEAVES {
                        return Err("FieldTree leaf limit exceeded".into());
                    }
                    if let Some(reason) = reason {
                        opaque.push((prefix.join("."), reason.into()));
                    }
                    out.push(prefix.clone());
                }
            }
            prefix.pop();
        }
        Ok(())
    }
    walk(root, &mut vec![], adts, &mut stack, &mut out, &mut opaque)?;
    Ok((out, opaque))
}

/// Compute one heatmap per documented nongeneric named struct that at least
/// one documented function borrows or consumes.
pub fn heatmaps(
    types: &[TypeDoc],
    functions: &[FunctionDoc],
    limits: Limits,
) -> Result<Heatmaps, String> {
    limits.validate()?;
    if types.len() > limits.max_items || functions.len() > limits.max_items {
        return Err("heatmap item budget exceeded".into());
    }
    let adts: BTreeMap<&str, &Adt> = types
        .iter()
        .map(|t| (t.declaration.name.as_str(), &t.declaration))
        .collect();
    let mut result = Heatmaps::default();
    let mut cells = 0usize;
    for ty in types {
        let adt = &ty.declaration;
        let uses: Vec<_> = functions
            .iter()
            .flat_map(|f| f.parameters.iter().map(move |p| (f, p)))
            .filter(|(_, p)| matches!(&p.ty, Type::Named(n, a) if *n == adt.name && a.is_empty()))
            .collect();
        if product(adt).is_none() {
            let reason = if adt.is_enum {
                "enum variants overlap: no FieldTree heatmap"
            } else if !adt.generics.is_empty() {
                "generic struct needs concrete arguments"
            } else {
                "positional or multi-variant type has no named FieldTree"
            };
            result.unsupported.push((adt.name.clone(), reason.into()));
            continue;
        }
        if uses.is_empty() {
            result.unsupported.push((
                adt.name.clone(),
                "no documented function borrows or consumes it".into(),
            ));
            continue;
        }
        let (paths, opaque) = leaves(adt, &adts)?;
        if paths.is_empty() {
            result
                .unsupported
                .push((adt.name.clone(), "struct has no fields".into()));
            continue;
        }
        let mut heatmap = Heatmap {
            ty: adt.name.clone(),
            leaves: paths.iter().map(|p| p.join(".")).collect(),
            rows: Vec::new(),
            writers: vec![0; paths.len()],
            readers: vec![0; paths.len()],
            consumers: vec![0; paths.len()],
            opaque,
        };
        for (f, p) in uses {
            if heatmap.rows.len() == MAX_ROWS {
                return Err("heatmap row limit exceeded".into());
            }
            cells = cells
                .checked_add(paths.len())
                .filter(|n| *n <= MAX_CELLS)
                .ok_or("heatmap cell budget exceeded")?;
            let selected: Vec<Vec<String>> = match &p.selector {
                Selector::All => vec![vec![]],
                Selector::Paths(s) => s.clone(),
            };
            let access = match p.pass {
                Pass::In => Access::Read,
                Pass::Inout => Access::Write,
                Pass::Sink => Access::Consume,
            };
            // A selector path either prefixes leaves, or reaches below an
            // opaque leaf; either way the leaf is (partly) borrowed.
            let row_cells: Vec<Access> = paths
                .iter()
                .map(|leaf| {
                    if selected
                        .iter()
                        .any(|s| leaf.starts_with(s) || s.starts_with(leaf))
                    {
                        access
                    } else {
                        Access::Free
                    }
                })
                .collect();
            if p.pass == Pass::Sink && row_cells.contains(&Access::Free) {
                return Err(format!(
                    "{}: sink parameter {} with a partial projection is unsupported",
                    f.name, p.name
                ));
            }
            if row_cells.iter().all(|c| *c == Access::Free) {
                return Err(format!(
                    "{}: parameter {} selects no FieldTree leaf of {}",
                    f.name, p.name, adt.name
                ));
            }
            for (i, c) in row_cells.iter().enumerate() {
                let counter = match c {
                    Access::Write => &mut heatmap.writers[i],
                    Access::Read => &mut heatmap.readers[i],
                    Access::Consume => &mut heatmap.consumers[i],
                    Access::Free => continue,
                };
                *counter = counter.checked_add(1).ok_or("heatmap counter overflow")?;
            }
            heatmap.rows.push(FootprintRow {
                function: f.name.clone(),
                parameter: p.name.clone(),
                pass: p.pass,
                selector: match &p.selector {
                    Selector::All => vec![],
                    Selector::Paths(s) => s.iter().map(|x| x.join(".")).collect(),
                },
                free: row_cells
                    .iter()
                    .zip(&heatmap.leaves)
                    .filter(|(c, _)| **c == Access::Free)
                    .map(|(_, l)| l.clone())
                    .collect(),
                cells: row_cells,
            });
        }
        heatmap
            .rows
            .sort_by(|a, b| (&a.function, &a.parameter).cmp(&(&b.function, &b.parameter)));
        result.heatmaps.push(heatmap);
    }
    result.heatmaps.sort_by(|a, b| a.ty.cmp(&b.ty));
    result.unsupported.sort();
    Ok(result)
}

fn color(access: Access) -> &'static str {
    match access {
        Access::Free => "#eef0f3",
        Access::Read => "#9cc3e6",
        Access::Write => "#e8916f",
        Access::Consume => "#b48ad6",
    }
}

/// Aggregate heat: white to `full` by the share of rows using a leaf.
fn heat(count: u32, rows: usize, full: (u32, u32, u32)) -> String {
    let rows = u32::try_from(rows.max(1)).unwrap_or(u32::MAX);
    let level = count.min(rows) * 100 / rows;
    let mix = |lo: u32| 255 - (255 - lo) * level / 100;
    format!("#{:02x}{:02x}{:02x}", mix(full.0), mix(full.1), mix(full.2))
}

/// Static HTML for every heatmap: a function-by-leaf grid and the
/// aggregate writer/reader rows.
pub fn render(maps: &Heatmaps) -> String {
    let mut out = String::from(
        "<section id=\"codex-heatmaps\"><h2>FieldTree borrow footprints</h2><p>Cells come from each parameter's checked pass mode and declared projection: W inout, R in, S sink, blank free for concurrent disjoint borrows. This is the checker-enforced upper bound, not a may-use analysis of the body.</p>",
    );
    if maps.heatmaps.is_empty() {
        out.push_str("<p>No documented struct is borrowed by a documented function.</p>");
    }
    for m in &maps.heatmaps {
        out.push_str(&format!(
            "<article class=\"heatmap\" id=\"heatmap-{0}\"><h3>{0}</h3><table style=\"border-collapse:collapse;font:13px ui-monospace,monospace\"><tr><th style=\"text-align:left\">Function(parameter)</th>",
            escape(&m.ty)
        ));
        for leaf in &m.leaves {
            out.push_str(&format!(
                "<th style=\"padding:0 .4em\">{}</th>",
                escape(leaf)
            ));
        }
        out.push_str("<th style=\"text-align:left\">Free siblings</th></tr>");
        for r in &m.rows {
            let selector = if r.selector.is_empty() {
                String::new()
            } else {
                format!(".{{{}}}", r.selector.join(", "))
            };
            out.push_str(&format!(
                "<tr><td>{}({} {}{})</td>",
                escape(&r.function),
                match r.pass {
                    Pass::In => "in",
                    Pass::Inout => "inout",
                    Pass::Sink => "sink",
                },
                escape(&r.parameter),
                escape(&selector)
            ));
            for c in &r.cells {
                out.push_str(&format!(
                    "<td title=\"{}\" style=\"background:{};text-align:center;border:1px solid #fff;min-width:2em\">{}</td>",
                    c.label(),
                    color(*c),
                    c.letter()
                ));
            }
            out.push_str(&format!("<td>{}</td></tr>", escape(&r.free.join(", "))));
        }
        for (label, counts, full) in [
            ("writers", &m.writers, (217, 72, 15)),
            ("readers", &m.readers, (40, 110, 190)),
            ("consumers", &m.consumers, (120, 60, 170)),
        ] {
            out.push_str(&format!("<tr><th style=\"text-align:left\">{label}</th>"));
            for n in counts {
                out.push_str(&format!(
                    "<td style=\"background:{};text-align:center;border:1px solid #fff\">{n}</td>",
                    heat(*n, m.rows.len(), full)
                ));
            }
            out.push_str("<td></td></tr>");
        }
        out.push_str("</table>");
        for (leaf, reason) in &m.opaque {
            out.push_str(&format!("<p>Leaf {}: {}</p>", escape(leaf), escape(reason)));
        }
        out.push_str("</article>");
    }
    for (ty, reason) in &maps.unsupported {
        out.push_str(&format!(
            "<p>No heatmap for {}: {}</p>",
            escape(ty),
            escape(reason)
        ));
    }
    out.push_str("</section>");
    out
}
