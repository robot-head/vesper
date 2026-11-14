//! Capability badges from checked effect rows (§40.1 item 2).
//!
//! Vesper has no ambient authority: everything a function may do is in its
//! checked `eff [...]` row, which the checker enforces against the body (an
//! operation outside the row is an error, and the capability effects
//! `ConstTime`, `Realtime` and `DetFloat` add their own source rules). A
//! badge is a fact about that row:
//!
//! - **Pure**: `eff []`.
//! - **Zero-Alloc**: no `Alloc` in the row.
//! - **Uncancellable Linear I/O**: an effect in the row resumes linearly
//!   (`res(eff) = linear`, a `linear effect` declaration): its continuation
//!   must be resumed exactly once.
//! - **Abortable Checkpoint**: `Fail` or `Cancel` is in the row, so the call
//!   may abort; the badge names the caller's linear values that stay live
//!   across it (linear parameters passed `in`/`inout`).
//! - **Constant-Time Verified**: `ConstTime` in the row; the checker's
//!   secret-flow rules hold for the body (not a machine-code proof).
//! - **Hard-Realtime Safe**: `Realtime` in the row; the checker's realtime
//!   discipline holds for the body. No WCSD stack byte bound is computed.
//! - **Deterministic Float**: `DetFloat` in the row.
//!
//! An open row (`eff [..r]`) gets no badge: what it may perform is not known.
//! When a [`crate::footprints`] analysis is supplied, each badge also states
//! whether the inferred row of the body agrees. A package summary lists the
//! badges every documented function holds and, against a `Loom.toml`
//! `capability_ceiling`, the effects outside it.
use crate::{FunctionDoc, Mode, Pass, footprints::MayUseReport, visual::escape};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    Pure,
    ZeroAlloc,
    UncancellableLinearIo,
    AbortableCheckpoint,
    ConstantTime,
    HardRealtime,
    DeterministicFloat,
}
impl Capability {
    pub const ALL: [Capability; 7] = [
        Capability::Pure,
        Capability::ZeroAlloc,
        Capability::UncancellableLinearIo,
        Capability::AbortableCheckpoint,
        Capability::ConstantTime,
        Capability::HardRealtime,
        Capability::DeterministicFloat,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::Pure => "Pure",
            Self::ZeroAlloc => "Zero-Alloc",
            Self::UncancellableLinearIo => "Uncancellable Linear I/O",
            Self::AbortableCheckpoint => "Abortable Checkpoint",
            Self::ConstantTime => "Constant-Time Verified",
            Self::HardRealtime => "Hard-Realtime Safe",
            Self::DeterministicFloat => "Deterministic Float",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityBadge {
    pub capability: Capability,
    pub evidence: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemCapabilities {
    pub item: String,
    /// Canonical effect names of the checked row, sorted.
    pub effects: Vec<String>,
    pub badges: Vec<CapabilityBadge>,
    /// Why no badge could be derived (open rows).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub undetermined: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageCapabilities {
    /// The manifest's `capability_ceiling`, when one was read.
    pub ceiling: Option<Vec<String>>,
    /// Union of every documented function's effect names.
    pub effects: Vec<String>,
    /// Capabilities every documented function with a closed row holds.
    pub badges: Vec<Capability>,
    /// (function, effect) pairs outside the ceiling.
    pub outside_ceiling: Vec<(String, String)>,
    /// Functions with an open row (no capability is known for them).
    pub undetermined: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    pub items: Vec<ItemCapabilities>,
}

fn inferred_note(name: &str, effect: &str, inferred: Option<&MayUseReport>) -> &'static str {
    let Some(f) = inferred.and_then(|r| r.functions.iter().find(|f| f.function == name)) else {
        return "";
    };
    if !f.effects_complete {
        ""
    } else if f.performed_effects.iter().any(|e| e == effect) {
        " The body performs it."
    } else {
        " The inferred body row does not perform it."
    }
}

/// Badges for one function from its checked row.
pub fn item(f: &FunctionDoc, inferred: Option<&MayUseReport>) -> ItemCapabilities {
    let names: BTreeSet<String> = f.effects.instances.iter().map(|e| e.name.clone()).collect();
    let mut out = ItemCapabilities {
        item: f.name.clone(),
        effects: names.iter().cloned().collect(),
        badges: vec![],
        undetermined: None,
    };
    if f.effects.tail.is_some() {
        out.undetermined = Some("open effect row: capabilities need a closed row".into());
        return out;
    }
    let mut push = |capability, evidence: String| {
        out.badges.push(CapabilityBadge {
            capability,
            evidence,
        })
    };
    if names.is_empty() {
        push(Capability::Pure, "checked row is eff []".into());
    }
    if !names.contains("Alloc") {
        push(
            Capability::ZeroAlloc,
            "Alloc is absent from the checked row".into(),
        );
    }
    let linear: Vec<String> = f
        .effects
        .instances
        .iter()
        .zip(&f.effect_resumptions)
        .filter(|(_, r)| **r == Some(Mode::Linear))
        .map(|(e, _)| e.name.clone())
        .collect();
    if !linear.is_empty() {
        let note = inferred_note(&f.name, &linear[0], inferred);
        push(
            Capability::UncancellableLinearIo,
            format!(
                "linear effect(s) {} resume exactly once.{note}",
                linear.join(", ")
            ),
        );
    }
    let aborts: Vec<&str> = ["Fail", "Cancel"]
        .into_iter()
        .filter(|e| names.contains(*e))
        .collect();
    if !aborts.is_empty() {
        let live: Vec<String> = f
            .parameters
            .iter()
            .filter(|p| p.mode == Mode::Linear && p.pass != Pass::Sink)
            .map(|p| p.name.clone())
            .collect();
        push(
            Capability::AbortableCheckpoint,
            format!(
                "{} may abort the call; {}",
                aborts.join(" and "),
                if live.is_empty() {
                    "no linear parameter stays live across it".to_string()
                } else {
                    format!(
                        "linear parameter(s) {} stay live in the caller across an abort",
                        live.join(", ")
                    )
                }
            ),
        );
    }
    for (effect, capability, evidence) in [
        (
            "ConstTime",
            Capability::ConstantTime,
            "the checker's ConstTime secret-flow rules hold for the body; not a machine-code proof",
        ),
        (
            "Realtime",
            Capability::HardRealtime,
            "the checker's Realtime discipline holds for the body; no WCSD stack byte bound is computed",
        ),
        (
            "DetFloat",
            Capability::DeterministicFloat,
            "only correctly rounded IEEE 754 basic float operations",
        ),
    ] {
        if names.contains(effect) {
            push(capability, evidence.into());
        }
    }
    out
}

pub fn capabilities(functions: &[FunctionDoc], inferred: Option<&MayUseReport>) -> Capabilities {
    let mut items: Vec<_> = functions.iter().map(|f| item(f, inferred)).collect();
    items.sort_by(|a, b| a.item.cmp(&b.item));
    Capabilities { items }
}

/// Package summary over (qualified name, capabilities) pairs.
pub fn package(items: &[ItemCapabilities], ceiling: Option<&[String]>) -> PackageCapabilities {
    let mut out = PackageCapabilities {
        ceiling: ceiling.map(|c| {
            let mut c = c.to_vec();
            c.sort();
            c.dedup();
            c
        }),
        ..Default::default()
    };
    let mut effects = BTreeSet::new();
    let mut held: BTreeSet<Capability> = Capability::ALL.into_iter().collect();
    let mut any = false;
    for item in items {
        if item.undetermined.is_some() {
            out.undetermined.push(item.item.clone());
            continue;
        }
        any = true;
        let own: BTreeSet<Capability> = item.badges.iter().map(|b| b.capability).collect();
        held = held.intersection(&own).copied().collect();
        for e in &item.effects {
            effects.insert(e.clone());
            if let Some(c) = &out.ceiling
                && !c.contains(e)
            {
                out.outside_ceiling.push((item.item.clone(), e.clone()));
            }
        }
    }
    if any && out.undetermined.is_empty() {
        out.badges = held.into_iter().collect();
    }
    out.effects = effects.into_iter().collect();
    out.outside_ceiling.sort();
    out.undetermined.sort();
    out
}

fn chips(badges: &[CapabilityBadge]) -> String {
    badges
        .iter()
        .map(|b| {
            format!(
                "<span class=\"cap cap-{}\" title=\"{}\" style=\"display:inline-block;margin:0 .3em .3em 0;padding:0 .4em;border:1px solid #567;border-radius:.6em;font-size:13px\">{}</span>",
                serde_json::to_value(b.capability)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or_default(),
                escape(&b.evidence),
                escape(b.capability.label())
            )
        })
        .collect()
}

/// Static HTML for item badges and, when given, the package summary.
pub fn render(caps: &Capabilities, package: Option<&PackageCapabilities>) -> String {
    let mut out = String::from(
        "<section id=\"codex-capabilities\"><h2>Capability badges</h2><p>Derived from each function's checked effect row, which the checker enforces against its body. Hover a badge for its evidence.</p>",
    );
    if let Some(p) = package {
        out.push_str(&render_package(p));
    }
    out.push_str("<table><tr><th style=\"text-align:left\">Function</th><th style=\"text-align:left\">Row</th><th style=\"text-align:left\">Badges</th></tr>");
    for item in &caps.items {
        out.push_str(&format!(
            "<tr id=\"caps-{0}\"><td><code>{0}</code></td><td><code>eff [{1}]</code></td><td>{2}{3}</td></tr>",
            escape(&item.item),
            escape(&item.effects.join(", ")),
            chips(&item.badges),
            item.undetermined
                .as_deref()
                .map(|r| format!("<em>{}</em>", escape(r)))
                .unwrap_or_default()
        ));
    }
    out.push_str("</table></section>");
    out
}

pub fn render_package(p: &PackageCapabilities) -> String {
    let mut out = String::from("<div class=\"package-caps\"><h3>Package</h3>");
    out.push_str(&format!(
        "<p>Every documented function: {}</p>",
        if p.badges.is_empty() {
            "no common badge".to_string()
        } else {
            escape(
                &p.badges
                    .iter()
                    .map(|c| c.label())
                    .collect::<Vec<_>>()
                    .join(", "),
            )
        }
    ));
    out.push_str(&format!(
        "<p>Effects used: <code>eff [{}]</code>; manifest ceiling: {}</p>",
        escape(&p.effects.join(", ")),
        match &p.ceiling {
            Some(c) => format!("<code>[{}]</code>", escape(&c.join(", "))),
            None => "none declared".into(),
        }
    ));
    for (f, e) in &p.outside_ceiling {
        out.push_str(&format!(
            "<p class=\"outside\"><strong>{}</strong> uses <code>{}</code> outside the ceiling</p>",
            escape(f),
            escape(e)
        ));
    }
    for f in &p.undetermined {
        out.push_str(&format!(
            "<p>{}: open effect row, no capability known</p>",
            escape(f)
        ));
    }
    out.push_str("</div>");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Effect, Effects, Parameter, Selector, Type};

    fn f(name: &str, effects: &[&str], resumptions: &[Option<Mode>], tail: bool) -> FunctionDoc {
        FunctionDoc {
            name: name.into(),
            docs: String::new(),
            span: crate::Span { start: 0, end: 0 },
            generics: vec![],
            parameters: vec![Parameter {
                name: "tx".into(),
                pass: Pass::Inout,
                mode: Mode::Linear,
                ty: Type::named("Tx"),
                selector: Selector::All,
            }],
            result: Type::named("u64"),
            declared_result: Type::named("u64"),
            result_mode: Mode::Copy,
            effects: Effects {
                instances: effects
                    .iter()
                    .map(|e| Effect {
                        label: None,
                        name: (*e).into(),
                        args: vec![],
                    })
                    .collect(),
                tail: tail.then(|| "r".to_string()),
            },
            effect_resumptions: resumptions.to_vec(),
            unsupported: vec![],
        }
    }
    fn caps(i: &ItemCapabilities) -> Vec<Capability> {
        i.badges.iter().map(|b| b.capability).collect()
    }

    #[test]
    fn badges_follow_the_row() {
        let pure = item(&f("p", &[], &[], false), None);
        assert_eq!(caps(&pure), [Capability::Pure, Capability::ZeroAlloc]);
        let alloc = item(&f("a", &["Alloc"], &[Some(Mode::Affine)], false), None);
        assert!(caps(&alloc).is_empty());
        let io = item(
            &f(
                "io",
                &["Disk", "Cancel"],
                &[Some(Mode::Linear), Some(Mode::Affine)],
                false,
            ),
            None,
        );
        assert_eq!(
            caps(&io),
            [
                Capability::ZeroAlloc,
                Capability::UncancellableLinearIo,
                Capability::AbortableCheckpoint
            ]
        );
        assert!(io.badges[2].evidence.contains("tx stay live"));
        let open = item(&f("o", &[], &[], true), None);
        assert!(open.badges.is_empty() && open.undetermined.is_some());
    }

    #[test]
    fn package_summary_intersects_and_checks_the_ceiling() {
        let items = [
            item(&f("p", &[], &[], false), None),
            item(&f("d", &["Disk"], &[Some(Mode::Affine)], false), None),
        ];
        let p = package(&items, Some(&["Net".to_string()]));
        assert_eq!(p.badges, [Capability::ZeroAlloc]);
        assert_eq!(p.outside_ceiling, [("d".to_string(), "Disk".to_string())]);
        let with_open = [items[0].clone(), item(&f("o", &[], &[], true), None)];
        let p = package(&with_open, None);
        assert!(p.badges.is_empty());
        assert_eq!(p.undetermined, ["o"]);
    }
}
