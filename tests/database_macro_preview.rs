use salsa::Setter;
use std::sync::{Arc, Mutex};
use vesper::{
    database::{
        SourceFile, guarded_analysis,
        macro_preview::{compile_limits, preview},
    },
    forge_frontend::Stage,
};

const KEEP: &str = include_str!("fixtures/source_macros/keep.ves");
const PURE_ROW: &str = include_str!("fixtures/source_macros/pure_declared_row.ves");

#[salsa::db]
#[derive(Clone)]
struct ObservedDatabase {
    storage: salsa::Storage<Self>,
    executions: Arc<Mutex<Vec<String>>>,
}
#[salsa::db]
impl salsa::Database for ObservedDatabase {}
impl ObservedDatabase {
    fn new() -> Self {
        let executions = Arc::new(Mutex::new(Vec::new()));
        let events = Arc::clone(&executions);
        Self {
            storage: salsa::Storage::new(Some(Box::new(move |event| {
                if let salsa::EventKind::WillExecute { database_key } = event.kind {
                    events.lock().unwrap().push(format!("{database_key:?}"));
                }
            }))),
            executions,
        }
    }
    fn take_executions(&self) -> Vec<String> {
        std::mem::take(&mut *self.executions.lock().unwrap())
    }
}
fn only_preview(events: &[String]) {
    assert_eq!(events.len(), 1, "{events:?}");
    assert!(events[0].starts_with("preview"), "{events:?}");
}

#[test]
fn unchanged_full_input_reuses_actual_salsa_artifact_without_legacy_queries() {
    let mut db = ObservedDatabase::new();
    let file = SourceFile::new(&db, KEEP.into());
    let old = preview(&db, file).outcome().unwrap();
    only_preview(&db.take_executions());
    assert_eq!(old.original_source(), KEEP);
    assert_eq!(old.package(), "@local/source");
    assert!(Arc::ptr_eq(&old, &preview(&db, file).outcome().unwrap()));
    assert!(db.take_executions().is_empty());

    // A forced Salsa setter is a new input revision, even for equal bytes.
    // Session no-op handling must avoid the setter to preserve input identity.
    file.set_text(&mut db).to(KEEP.into());
    let forced_revision = preview(&db, file).outcome().unwrap();
    assert!(!Arc::ptr_eq(&old, &forced_revision));
    assert_eq!(old.original_hash(), forced_revision.original_hash());
    only_preview(&db.take_executions());

    let changed = KEEP.replace("42", "41");
    file.set_text(&mut db).to(changed.clone());
    let fresh = preview(&db, file).outcome().unwrap();
    only_preview(&db.take_executions());
    assert!(!Arc::ptr_eq(&old, &fresh));
    assert_eq!(fresh.original_source(), changed);
    assert_ne!(old.expanded_hash(), fresh.expanded_hash());
    assert_eq!(old.original_source(), KEEP);
}

#[test]
fn same_header_helper_body_change_reruns_actual_authority_gate() {
    let mut db = ObservedDatabase::new();
    let file = SourceFile::new(&db, PURE_ROW.into());
    let old = preview(&db, file).outcome().unwrap();
    assert_eq!(old.audits().len(), 1);
    db.take_executions();
    // The complete declarations and their effect annotations are unchanged.
    // Only the actual helper body gains implicit overflow authority.
    let changed = PURE_ROW.replace("wrapping_add(x, 1)", "x + 1");
    file.set_text(&mut db).to(changed.clone());
    let rejected = preview(&db, file);
    let failure = rejected.expanded().unwrap_err();
    assert_eq!(failure.stage(), Stage::Authority, "{failure:?}");
    assert_eq!(rejected.source(), Some(changed.as_str()));
    assert_eq!(
        rejected.source_identity().blake3(),
        Some(*blake3::hash(changed.as_bytes()).as_bytes())
    );
    // Authority failures expose no diagnostic candidate in the real compiler.
    assert!(failure.expanded_source().is_none());
    only_preview(&db.take_executions());
    let refusal = rejected.outcome().unwrap_err();
    assert!(Arc::ptr_eq(
        &refusal,
        &preview(&db, file).outcome().unwrap_err()
    ));
    assert!(db.take_executions().is_empty());

    file.set_text(&mut db).to(PURE_ROW.into());
    let restored = preview(&db, file).outcome().unwrap();
    assert_eq!(restored.original_source(), PURE_ROW);
    assert!(!Arc::ptr_eq(&old, &restored));
    only_preview(&db.take_executions());
}

#[test]
fn comment_and_definition_edits_refresh_current_hashes_maps_and_audits() {
    let mut db = ObservedDatabase::new();
    let source = include_str!("fixtures/source_macros/repeated.ves");
    let file = SourceFile::new(&db, source.into());
    let old = preview(&db, file).outcome().unwrap();
    db.take_executions();
    let prefix = "// unsaved Unicode comment: λ 🦀\n";
    let changed = format!("{prefix}{source}");
    file.set_text(&mut db).to(changed.clone());
    let snapshot = preview(&db, file);
    let current = snapshot.expanded().unwrap();
    assert_eq!(snapshot.source(), Some(changed.as_str()));
    assert_eq!(snapshot.source_identity().byte_len(), changed.len());
    assert_eq!(
        snapshot.source_identity().blake3(),
        Some(current.original_hash())
    );
    assert_eq!(
        current.original_hash(),
        *blake3::hash(changed.as_bytes()).as_bytes()
    );
    assert_eq!(
        current.expanded_hash(),
        *blake3::hash(current.expanded_source().as_bytes()).as_bytes()
    );
    assert_eq!(current.checked().source(), current.expanded_source());
    assert_eq!(current.audits().len(), 1);
    assert_eq!(current.audits()[0].scope, "expanded_function");
    assert_eq!(
        current.audits()[0].invocation.start,
        old.audits()[0].invocation.start + prefix.len()
    );
    let invocation = current.audits()[0].invocation;
    assert_eq!(&changed[invocation.start..invocation.end], "twice!(n)");
    let argument = changed.rfind("(n)").unwrap() + 1;
    let repeated: Vec<_> = current
        .source_map()
        .iter()
        .filter(|entry| entry.original.start == argument && entry.original.end == argument + 1)
        .collect();
    assert_eq!(repeated.len(), 2);
    assert_ne!(repeated[0].expanded, repeated[1].expanded);
    for entry in current.source_map() {
        assert!(entry.original.start <= entry.original.end && entry.original.end <= changed.len());
        assert!(
            entry.expanded.start <= entry.expanded.end
                && entry.expanded.end <= current.expanded_source().len()
        );
        assert!(
            changed.is_char_boundary(entry.original.start)
                && changed.is_char_boundary(entry.original.end)
        );
        assert!(
            current
                .expanded_source()
                .is_char_boundary(entry.expanded.start)
                && current
                    .expanded_source()
                    .is_char_boundary(entry.expanded.end)
        );
    }
    only_preview(&db.take_executions());

    let changed_definition = changed.replace("wrapping_add($x, $x)", "$x");
    file.set_text(&mut db).to(changed_definition.clone());
    let fresh = preview(&db, file).outcome().unwrap();
    assert_eq!(fresh.original_source(), changed_definition);
    assert_ne!(fresh.expanded_hash(), old.expanded_hash());
    only_preview(&db.take_executions());
}

#[test]
fn linear_duplication_and_leaks_remain_actual_checker_refusals() {
    let db = ObservedDatabase::new();
    let duplicate = include_str!("fixtures/source_macros/linear_duplicate.ves");
    let leak = "linear struct Tx {} fn make()->Tx{Tx{}} macro lose($x:expr)->expr{quote{0}} fn main()->i64{let tx=make();lose!(tx)}";
    for (source, code) in [(duplicate, "E0303"), (leak, "E0302")] {
        let file = SourceFile::new(&db, source.into());
        let snapshot = preview(&db, file);
        let failure = snapshot.expanded().unwrap_err();
        assert_eq!(failure.stage(), Stage::Check, "{failure:?}");
        assert!(
            failure.diagnostics().iter().any(|d| d.code == code),
            "{failure:?}"
        );
        let expanded = failure.expanded_source().unwrap();
        for diagnostic in failure.diagnostics() {
            assert!(
                diagnostic.span.start <= diagnostic.span.end
                    && diagnostic.span.end <= expanded.len()
            );
        }
        assert_eq!(snapshot.source(), Some(source));
        only_preview(&db.take_executions());
    }
}

#[test]
fn malformed_unsupported_and_oversized_inputs_are_real_bounded_refusals() {
    let db = ObservedDatabase::new();
    for (source, expected) in [
        (
            "macro keep($x:expr)->expr{quote{$x}} fn main(){keep!(",
            Stage::Parse,
        ),
        (
            "macro keep($x:expr)->expr{quote{$x}} fn main[T](){keep!(1)}",
            Stage::Unsupported,
        ),
    ] {
        let file = SourceFile::new(&db, source.into());
        let snapshot = preview(&db, file);
        assert_eq!(snapshot.expanded().unwrap_err().stage(), expected);
        assert_eq!(snapshot.source(), Some(source));
        assert!(snapshot.source_identity().blake3().is_some());
        only_preview(&db.take_executions());
    }
    let oversized = "x".repeat(compile_limits().max_source_bytes + 1);
    let file = SourceFile::new(&db, oversized);
    let snapshot = preview(&db, file);
    assert_eq!(snapshot.expanded().unwrap_err().stage(), Stage::Resource);
    assert_eq!(snapshot.source(), None);
    assert_eq!(snapshot.source_identity().blake3(), None);
    assert_eq!(
        snapshot.source_identity().byte_len(),
        compile_limits().max_source_bytes + 1
    );
    assert!(snapshot.expanded().unwrap_err().expanded_source().is_none());
    assert!(snapshot.expanded().unwrap_err().diagnostics().is_empty());
    only_preview(&db.take_executions());
    let old_identity = snapshot.source_identity().clone();
    let independent = SourceFile::new(&db, "x".repeat(compile_limits().max_source_bytes + 1));
    assert_ne!(&old_identity, preview(&db, independent).source_identity());
}

#[test]
fn expanded_preview_never_promotes_original_source_semantics() {
    let db = ObservedDatabase::new();
    let file = SourceFile::new(&db, KEEP.into());
    assert!(guarded_analysis(&db, file).checked().is_err());
    let artifact = preview(&db, file).expanded().unwrap();
    assert_eq!(artifact.original_source(), KEEP);
    assert_ne!(artifact.checked().source(), KEEP);
    assert!(guarded_analysis(&db, file).checked().is_err());
}
