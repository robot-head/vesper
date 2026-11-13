//! Borrow-summary to Vesper projection lifting (design §29.5).
//!
//! [`lift_rust_param_to_vesper_projection`] keeps the design's rendering
//! rules: an owned move becomes `sink`, any mutated field makes the whole
//! footprint `inout x.{...}` (mutated and read paths, sorted and deduplicated),
//! otherwise read paths give `in x.{...}` and no paths give `in x`.
//!
//! One correction to the sketch: a summary records whether the parameter was
//! used as a *whole value* (passed on, dereferenced, method receiver, inside a
//! macro). The sketch would render such a parameter as `in x` even when it was
//! a unique borrow that the callee mutates through a call, which loses the
//! write. Here a whole-value unique borrow stays `inout x` without projection.

use crate::{LiftedParam, Mode};

/// Summary of field accesses extracted from one legacy Rust parameter
/// (`&T`, `&mut T`, or `T`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LegacyRustParamSummary {
    pub param_name: String,
    pub ty_name: String,
    pub is_owned_move: bool,
    /// The parameter is a unique (`&mut`) borrow.
    pub is_unique_borrow: bool,
    /// The parameter is used other than through field projections, so no
    /// FieldTree footprint can be justified.
    pub whole_value_access: bool,
    pub read_field_paths: Vec<String>,
    pub mutated_field_paths: Vec<String>,
}

impl LegacyRustParamSummary {
    /// The mode and footprint this summary justifies.
    pub fn lift(&self) -> LiftedParam {
        let (mode, footprint) = if self.is_owned_move {
            (Mode::Sink, None)
        } else if self.whole_value_access {
            (
                if self.is_unique_borrow {
                    Mode::Inout
                } else {
                    Mode::In
                },
                None,
            )
        } else if !self.mutated_field_paths.is_empty() {
            (
                Mode::Inout,
                Some(sorted_paths(
                    self.mutated_field_paths
                        .iter()
                        .chain(self.read_field_paths.iter()),
                )),
            )
        } else if self.read_field_paths.is_empty() {
            (Mode::In, None)
        } else {
            (Mode::In, Some(sorted_paths(self.read_field_paths.iter())))
        };
        LiftedParam {
            name: self.param_name.clone(),
            mode,
            footprint,
            ty: self.ty_name.clone(),
        }
    }
}

/// Sort, deduplicate, and drop paths subsumed by a shorter prefix
/// (`a` covers `a.b`), so the footprint is a FieldTree antichain.
fn sorted_paths<'a>(paths: impl Iterator<Item = &'a String>) -> Vec<String> {
    let mut all: Vec<String> = paths.cloned().collect();
    all.sort_unstable();
    all.dedup();
    let mut out: Vec<String> = Vec::with_capacity(all.len());
    for p in all {
        let covered = out.iter().any(|q| {
            p.len() > q.len() && p.starts_with(q.as_str()) && p.as_bytes()[q.len()] == b'.'
        });
        if !covered {
            out.push(p);
        }
    }
    out
}

/// Render one lifted parameter as Vesper source text.
pub fn render_param(param: &LiftedParam) -> String {
    let mut out = String::new();
    if let Some(k) = param.mode.keyword() {
        out.push_str(k);
        out.push(' ');
    }
    out.push_str(&param.name);
    if let Some(paths) = &param.footprint {
        out.push_str(".{");
        out.push_str(&paths.join(", "));
        out.push('}');
    }
    out.push_str(": ");
    out.push_str(&param.ty);
    out
}

/// Synthesizes a lifetime-free Vesper parameter signature (`in`, `inout x.{...}`
/// or `sink`), as sketched in design §29.5.
pub fn lift_rust_param_to_vesper_projection(summary: &LegacyRustParamSummary) -> String {
    render_param(&summary.lift())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(
        owned: bool,
        unique: bool,
        whole: bool,
        reads: &[&str],
        writes: &[&str],
    ) -> LegacyRustParamSummary {
        LegacyRustParamSummary {
            param_name: "state".into(),
            ty_name: "DecoderStats".into(),
            is_owned_move: owned,
            is_unique_borrow: unique,
            whole_value_access: whole,
            read_field_paths: reads.iter().map(|s| s.to_string()).collect(),
            mutated_field_paths: writes.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn design_rendering_rules() {
        assert_eq!(
            lift_rust_param_to_vesper_projection(&summary(true, false, false, &["a"], &[])),
            "sink state: DecoderStats"
        );
        assert_eq!(
            lift_rust_param_to_vesper_projection(&summary(
                false,
                true,
                false,
                &["record_count"],
                &["last_crc", "record_count"]
            )),
            "inout state.{last_crc, record_count}: DecoderStats"
        );
        assert_eq!(
            lift_rust_param_to_vesper_projection(&summary(
                false,
                false,
                false,
                &["b", "a", "b"],
                &[]
            )),
            "in state.{a, b}: DecoderStats"
        );
        assert_eq!(
            lift_rust_param_to_vesper_projection(&summary(false, false, false, &[], &[])),
            "in state: DecoderStats"
        );
    }

    #[test]
    fn whole_value_unique_borrow_keeps_inout() {
        assert_eq!(
            lift_rust_param_to_vesper_projection(&summary(false, true, true, &["a"], &[])),
            "inout state: DecoderStats"
        );
        assert_eq!(
            lift_rust_param_to_vesper_projection(&summary(false, false, true, &["a"], &[])),
            "in state: DecoderStats"
        );
    }

    #[test]
    fn nested_paths_are_an_antichain() {
        assert_eq!(
            lift_rust_param_to_vesper_projection(&summary(
                false,
                true,
                false,
                &["hdr.crc", "len"],
                &["hdr"]
            )),
            "inout state.{hdr, len}: DecoderStats"
        );
    }
}
