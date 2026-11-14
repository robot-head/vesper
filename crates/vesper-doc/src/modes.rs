//! Mode-crossing tables (§40.1 item 1b): the exact `[K-Adt]` mode of each
//! generic type for every assignment of modes to its type parameters, e.g.
//! `Option[T]`: `T = copy -> copy`, `affine -> affine`, `linear -> linear`.
//!
//! The rows are computed by the checker's own mode function (the facade
//! passes them in): an ADT's mode is the join of its declared mode and its
//! fields' modes, with field modalities applied. An assignment that exceeds
//! a parameter's declared bound (`[T: mod affine]`) is rejected at every
//! instantiation site, and is shown as rejected rather than given a mode.
use crate::{Mode, visual::escape};
use serde::{Deserialize, Serialize};

/// At most this many type parameters get the full cross product (3^n rows).
pub const MAX_FULL_PARAMS: usize = 4;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypeParam {
    pub name: String,
    pub bound: Mode,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrossingRow {
    /// One mode per type parameter, in declaration order.
    pub args: Vec<Mode>,
    /// The instantiated type's mode; `None` when the row is rejected.
    pub mode: Option<Mode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rejected: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModeCrossing {
    pub ty: String,
    /// `documented` for source declarations, `builtin` for prelude types
    /// that documented signatures mention.
    pub origin: String,
    pub declared_mode: Mode,
    pub params: Vec<TypeParam>,
    pub rows: Vec<CrossingRow>,
    /// False when only the diagonal rows (every parameter the same mode)
    /// were computed because the type has more than [`MAX_FULL_PARAMS`].
    pub complete: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModeCrossings {
    pub tables: Vec<ModeCrossing>,
    pub unsupported: Vec<(String, String)>,
}

const MODES: [Mode; 3] = [Mode::Copy, Mode::Affine, Mode::Linear];

pub fn mode_text(m: Mode) -> &'static str {
    match m {
        Mode::Copy => "copy",
        Mode::Affine => "affine",
        Mode::Linear => "linear",
    }
}

/// The assignments a table lists: the full product for up to
/// [`MAX_FULL_PARAMS`] parameters, else the three diagonal rows.
pub fn assignments(params: usize) -> (Vec<Vec<Mode>>, bool) {
    if params > MAX_FULL_PARAMS {
        return (MODES.iter().map(|m| vec![*m; params]).collect(), false);
    }
    let mut rows = vec![vec![]];
    for _ in 0..params {
        rows = rows
            .into_iter()
            .flat_map(|r| {
                MODES.iter().map(move |m| {
                    let mut r = r.clone();
                    r.push(*m);
                    r
                })
            })
            .collect();
    }
    (rows, true)
}

/// Build one table; `mode_of(args)` is the checker's `[K-Adt]` mode of the
/// type instantiated at type parameters of those modes.
pub fn table(
    ty: &str,
    origin: &str,
    declared_mode: Mode,
    params: Vec<TypeParam>,
    mut mode_of: impl FnMut(&[Mode]) -> Mode,
) -> ModeCrossing {
    let (rows, complete) = assignments(params.len());
    let rows = rows
        .into_iter()
        .map(|args| {
            let over: Vec<String> = params
                .iter()
                .zip(&args)
                .filter(|(p, m)| **m > p.bound)
                .map(|(p, _)| format!("{} is bounded by mod {}", p.name, mode_text(p.bound)))
                .collect();
            if over.is_empty() {
                CrossingRow {
                    mode: Some(mode_of(&args)),
                    args,
                    rejected: None,
                }
            } else {
                CrossingRow {
                    args,
                    mode: None,
                    rejected: Some(over.join("; ")),
                }
            }
        })
        .collect();
    ModeCrossing {
        ty: ty.into(),
        origin: origin.into(),
        declared_mode,
        params,
        rows,
        complete,
    }
}

pub fn render(c: &ModeCrossings) -> String {
    let mut out = String::from(
        "<section id=\"codex-mode-crossings\"><h2>Mode-crossing tables</h2><p>The checker's [K-Adt] mode of each generic type for every mode of its type parameters. Rejected rows exceed a declared bound.</p>",
    );
    if c.tables.is_empty() {
        out.push_str("<p>No generic type is documented or used.</p>");
    }
    for t in &c.tables {
        let head = format!(
            "{}[{}]",
            t.ty,
            t.params
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
        out.push_str(&format!(
            "<article class=\"mode-crossing\" id=\"modes-{}\"><h3><code>{}</code> <small>({}, declared {})</small></h3><table><tr>",
            escape(&t.ty),
            escape(&head),
            escape(&t.origin),
            mode_text(t.declared_mode)
        ));
        for p in &t.params {
            out.push_str(&format!(
                "<th>{} <small>(mod {})</small></th>",
                escape(&p.name),
                mode_text(p.bound)
            ));
        }
        out.push_str("<th>mode</th></tr>");
        for r in &t.rows {
            out.push_str("<tr>");
            for m in &r.args {
                out.push_str(&format!("<td>{}</td>", mode_text(*m)));
            }
            match (r.mode, &r.rejected) {
                (Some(m), _) => {
                    out.push_str(&format!("<td><strong>{}</strong></td>", mode_text(m)))
                }
                (None, reason) => out.push_str(&format!(
                    "<td><em>rejected: {}</em></td>",
                    escape(reason.as_deref().unwrap_or(""))
                )),
            }
            out.push_str("</tr>");
        }
        out.push_str("</table>");
        if !t.complete {
            out.push_str("<p>Only the diagonal rows are listed for this many type parameters.</p>");
        }
        out.push_str("</article>");
    }
    for (ty, reason) in &c.unsupported {
        out.push_str(&format!(
            "<p>No table for {}: {}</p>",
            escape(ty),
            escape(reason)
        ));
    }
    out.push_str("</section>");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_product_then_diagonal() {
        assert_eq!(assignments(0), (vec![vec![]], true));
        assert_eq!(assignments(2).0.len(), 9);
        assert_eq!(assignments(4).0.len(), 81);
        let (rows, complete) = assignments(5);
        assert!(!complete);
        assert_eq!(rows.len(), 3);
        assert!(
            rows.iter()
                .all(|r| r.len() == 5 && r.iter().all(|m| *m == r[0]))
        );
    }

    #[test]
    fn bounds_reject_rows_and_modes_come_from_the_callback() {
        let t = table(
            "Box",
            "documented",
            Mode::Affine,
            vec![TypeParam {
                name: "T".into(),
                bound: Mode::Affine,
            }],
            |args| args[0].max(Mode::Affine),
        );
        let modes: Vec<Option<Mode>> = t.rows.iter().map(|r| r.mode).collect();
        assert_eq!(modes, [Some(Mode::Affine), Some(Mode::Affine), None]);
        assert!(
            t.rows[2]
                .rejected
                .as_deref()
                .unwrap()
                .contains("mod affine")
        );
        let html = render(&ModeCrossings {
            tables: vec![t],
            unsupported: vec![("<R>".into(), "row".into())],
        });
        assert!(html.contains("rejected: T is bounded by mod affine"));
        assert!(html.contains("&lt;R&gt;"));
    }
}
