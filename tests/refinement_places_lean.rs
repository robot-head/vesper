//! Kernel-check soundness extensions against their freshly compiled dependencies.
use std::{collections::BTreeSet, path::PathBuf, process::Command};

fn declarations(source: &str) -> BTreeSet<String> {
    let mut namespace = "";
    let mut names = BTreeSet::new();
    for line in source.lines() {
        if let Some(name) = line.strip_prefix("namespace ") {
            namespace = name.trim();
        }
        let line = line.strip_prefix("@[simp] ").unwrap_or(line);
        if let Some(rest) = line.strip_prefix("theorem ") {
            let name = rest.split_whitespace().next().unwrap();
            names.insert(format!("{namespace}.{name}"));
        }
    }
    names
}

#[test]
fn refinement_helpers_and_operational_results_are_audited() {
    for module in [
        "StrongUpdate",
        "RefinementPlaces",
        "ClockQuotientSafety",
        "OrderedContexts",
        "ResourceGrades",
        "HandlerGrades",
        "OperationalGhostBridge",
        "TypedPlaceRefinements",
        "PlacePolymorphismBridge",
        "CodegenComposition",
        "CodegenValidation",
        "CodegenControlFlow",
        "CodegenFlowValidation",
    ] {
        let source = std::fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("lean/{module}.lean")),
        )
        .unwrap();
        for name in declarations(&source) {
            assert!(
                source.contains(&format!("#print axioms {name}\n")),
                "{name} not audited"
            );
        }
    }
}

#[test]
fn compiled_soundness_extensions_have_only_standard_axioms() {
    let Ok(lean) = std::env::var("VESPER_LEAN") else {
        eprintln!("VESPER_LEAN unset; refinement kernel checks skipped locally");
        return;
    };
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let dir = std::env::temp_dir().join(format!("vesper-refinement-places-{}", std::process::id()));
    std::fs::create_dir(&dir).unwrap();
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(dir.clone());
    for module in [
        "EffectPlaces",
        "VesperUnifiedTypeTheory",
        "StrongUpdate",
        "RefinementPlaces",
        "ClockQuotientSafety",
        "OrderedContexts",
        "ResourceGrades",
        "HandlerGrades",
        "MetatheoryAppendix",
        "GhostErasure",
        "Metatheory/Polymorphism",
        "OperationalGhostBridge",
        "MetatheoryPlaces",
        "TypedPlaceRefinements",
        "PlacePolymorphismBridge",
        "VerifiedCodegen",
        "CodegenComposition",
        "CodegenValidation",
        "CodegenControlFlow",
        "CodegenFlowValidation",
    ] {
        let source = std::fs::read_to_string(root.join(format!("lean/{module}.lean"))).unwrap();
        let path = dir.join(format!("{module}.lean"));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &source).unwrap();
        let output = Command::new(&lean)
            .current_dir(&root)
            .env("LEAN_PATH", &dir)
            .arg(format!("--root={}", dir.display()))
            .args(["-o"])
            .arg(dir.join(format!("{module}.olean")))
            .arg(path)
            .output()
            .expect("run pinned Lean");
        let report = String::from_utf8(output.stdout).unwrap();
        assert!(
            output.status.success(),
            "{module}: {report}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            !report.contains("sorryAx")
                && !report.contains("warning:")
                && !report.contains("error:"),
            "{report}"
        );
        // EffectPlaces has its own source/extraction tests. Require every theorem
        // in these extension modules to appear in the audited report exactly once.
        if matches!(
            module,
            "VerifiedCodegen"
                | "EffectPlaces"
                | "VesperUnifiedTypeTheory"
                | "MetatheoryAppendix"
                | "GhostErasure"
                | "Metatheory/Polymorphism"
                | "MetatheoryPlaces"
        ) {
            continue;
        }
        let mut audited = BTreeSet::new();
        // Lean wraps long axiom lists; join indented continuations before
        // enforcing the allowlist so no dependency is silently skipped.
        let report = report.replace("\n ", " ");
        for line in report.lines() {
            let Some(rest) = line.strip_prefix('\'') else {
                continue;
            };
            let (name, axioms) = rest.split_once("' ").unwrap();
            if let Some(list) = axioms.strip_prefix("depends on axioms: [") {
                for axiom in list.strip_suffix(']').unwrap().split(',') {
                    let axiom = axiom.trim();
                    assert!(
                        ["propext", "Quot.sound", "Classical.choice"].contains(&axiom),
                        "{name}: {axiom}"
                    );
                }
            } else {
                assert_eq!(axioms, "does not depend on any axioms");
            }
            assert!(audited.insert(name.to_owned()));
        }
        assert_eq!(audited, declarations(&source));
    }
}
