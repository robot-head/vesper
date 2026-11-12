use salsa::Setter;
use std::sync::{Arc, Mutex};
use vesper::database::{self, RecoveryFile, SyntaxSnapshot};
use vesper_syntax::{
    incremental::{SyntaxDocument, UpdateKind},
    recovery::Limits,
};
#[salsa::db]
#[derive(Clone)]
struct Observed {
    storage: salsa::Storage<Self>,
    events: Arc<Mutex<Vec<String>>>,
}
#[salsa::db]
impl salsa::Database for Observed {}
impl Observed {
    fn new() -> Self {
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&events);
        Self {
            storage: salsa::Storage::new(Some(Box::new(move |event| {
                if let salsa::EventKind::WillExecute { database_key } = event.kind {
                    sink.lock().unwrap().push(format!("{database_key:?}"));
                }
            }))),
            events,
        }
    }
    fn take(&self) -> Vec<String> {
        std::mem::take(&mut *self.events.lock().unwrap())
    }
}
const SOURCE: &str = "// 🦀 é\r\nlinear struct Tx{}\r\nfn good()->u64{1}\r\nfn damaged(sink tx:Tx){tx.}\r\nfn leak(sink other:Tx){}\r\n";
#[test]
fn actual_incremental_snapshot_drives_memoized_scoped_diagnostics_and_fresh_ranges() {
    let mut document = SyntaxDocument::open(SOURCE, Limits::default()).unwrap();
    let mut db = Observed::new();
    let file = RecoveryFile::new(&db, SyntaxSnapshot::new(Arc::clone(document.snapshot())));
    let first = database::recovery_diagnostics(&db, file)
        .as_ref()
        .unwrap()
        .clone();
    assert_eq!(first.source(), SOURCE);
    assert_eq!(first.source_hash(), document.snapshot().source_hash());
    assert!(
        first
            .diagnostics()
            .iter()
            .any(|d| d.code == "E0302" && d.message.contains("other"))
    );
    assert!(first.analyzed_functions().iter().any(|f| f.name == "leak"));
    assert!(!first.unavailable().is_empty());
    assert!(db.take().iter().any(|e| e.starts_with("recovery_query")));
    let repeated = database::recovery_diagnostics(&db, file).as_ref().unwrap();
    assert!(Arc::ptr_eq(&first, repeated));
    assert!(db.take().is_empty());
    assert_eq!(document.update(SOURCE).unwrap(), UpdateKind::Unchanged);
    file.set_snapshot(&mut db)
        .to(SyntaxSnapshot::new(Arc::clone(document.snapshot())));
    assert!(Arc::ptr_eq(
        &first,
        database::recovery_diagnostics(&db, file).as_ref().unwrap()
    ));
    let unchanged_events = db.take();
    assert!(
        unchanged_events
            .iter()
            .any(|e| e.starts_with("recovery_input"))
    );
    assert!(
        !unchanged_events
            .iter()
            .any(|e| e.starts_with("recovery_query"))
    );
    let after = SOURCE.replace("good()->u64{1}", "good()->u64{12345}");
    assert_eq!(document.update(&after).unwrap(), UpdateKind::BodyReparse);
    file.set_snapshot(&mut db)
        .to(SyntaxSnapshot::new(Arc::clone(document.snapshot())));
    let next = database::recovery_diagnostics(&db, file).as_ref().unwrap();
    assert_eq!(next.source(), after);
    assert_eq!(next.source_hash(), document.snapshot().source_hash());
    assert_ne!(next.source_hash(), first.source_hash());
    assert!(!Arc::ptr_eq(&first, next));
    assert!(db.take().iter().any(|e| e.starts_with("recovery_query")));
    let before = first
        .diagnostics()
        .iter()
        .find(|d| d.code == "E0302" && d.message.contains("other"))
        .unwrap();
    let current = next
        .diagnostics()
        .iter()
        .find(|d| d.code == "E0302" && d.message.contains("other"))
        .unwrap();
    assert_eq!(current.span.start, before.span.start + 4);
    assert_eq!(current.span.end, before.span.end + 4);
    assert_eq!(first.source(), SOURCE); // retained snapshots stay bound to old bytes
}
#[test]
fn distinct_sealed_allocations_invalidate_without_deep_cst_equality_or_legacy_queries() {
    let document = SyntaxDocument::open(SOURCE, Limits::default()).unwrap();
    let mut db = Observed::new();
    let file = RecoveryFile::new(&db, SyntaxSnapshot::new(Arc::clone(document.snapshot())));
    let old = database::recovery_diagnostics(&db, file)
        .as_ref()
        .unwrap()
        .clone();
    db.take();
    let independently_parsed = SyntaxDocument::open(SOURCE, Limits::default()).unwrap();
    file.set_snapshot(&mut db)
        .to(SyntaxSnapshot::new(Arc::clone(
            independently_parsed.snapshot(),
        )));
    let new = database::recovery_diagnostics(&db, file).as_ref().unwrap();
    assert!(!Arc::ptr_eq(&old, new));
    assert_eq!(old.source_hash(), new.source_hash());
    let events = db.take();
    assert!(events.iter().any(|e| e.starts_with("recovery_query")));
    assert!(!events.iter().any(|e| e.starts_with("parse_file")
        || e.starts_with("check_file")
        || e.starts_with("guarded_analysis")));
}
#[test]
fn failed_syntax_update_cannot_supply_a_current_source_association() {
    let limits = Limits {
        parse: vesper_syntax::parser::ParseLimits {
            max_source_bytes: SOURCE.len(),
            ..Limits::default().parse
        },
        ..Limits::default()
    };
    let mut document = SyntaxDocument::open(SOURCE, limits).unwrap();
    let old = Arc::clone(document.snapshot());
    let after = format!("{SOURCE} ");
    assert!(document.update(&after).is_err());
    assert!(Arc::ptr_eq(&old, document.snapshot()));
    let db = Observed::new();
    let file = RecoveryFile::new(&db, SyntaxSnapshot::new(Arc::clone(document.snapshot())));
    let report = database::recovery_diagnostics(&db, file).as_ref().unwrap();
    assert_eq!(report.source(), SOURCE);
    assert_ne!(report.source(), after);
    // The dispatcher must discard this association instead of publishing the
    // old snapshot for `after`; syntax/diagnostic APIs expose exact current bytes.
}
