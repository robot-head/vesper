use salsa::Setter;
use std::sync::{Arc, Mutex};
use vesper::{
    database::{
        self, SignatureAvailability, SourceFile, guarded_analysis, signature_inventory,
        signature_snapshot,
    },
    tooling::frontend::AnalysisFailure,
};

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
        let executions = Arc::new(Mutex::new(vec![]));
        let events = executions.clone();
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
fn executed(events: &[String], query: &str) -> bool {
    events.iter().any(|event| event.starts_with(query))
}
fn complete(db: &ObservedDatabase, file: SourceFile) -> [u8; 32] {
    let snapshot = signature_snapshot(db, file);
    assert_eq!(
        snapshot.availability(),
        SignatureAvailability::Complete,
        "{snapshot:?}"
    );
    snapshot.digest().unwrap()
}

#[test]
fn actual_salsa_cutoff_reuses_consumer_but_positions_observe_current_source() {
    let mut db = ObservedDatabase::new();
    let before = "fn value()->u64{1}\nfn later()->u64{value()}";
    let file = SourceFile::new(&db, before.into());
    let inventory = signature_inventory(&db, file).clone();
    assert!(
        inventory
            .declarations()
            .iter()
            .any(|name| name == "fn::later")
    );
    let old = guarded_analysis(&db, file).checked_arc().unwrap();
    let old_span = old.world().functions["later"].span.clone();
    let old_identity = guarded_analysis(&db, file).source_identity().clone();
    db.take_executions();
    let after =
        "// source positions move\nfn value()->u64{let ignored=123;2}\nfn later()->u64{value()}";
    file.set_text(&mut db).to(after.into());
    assert_eq!(&inventory, signature_inventory(&db, file));
    let events = db.take_executions();
    assert!(executed(&events, "guarded_analysis"), "{events:?}");
    assert!(executed(&events, "signature_snapshot"), "{events:?}");
    assert!(!executed(&events, "signature_inventory"), "{events:?}");
    assert!(!executed(&events, "check_file"), "{events:?}");
    assert!(!executed(&events, "parse_file"), "{events:?}");
    let fresh = guarded_analysis(&db, file);
    assert_ne!(fresh.source_identity(), &old_identity);
    let checked = fresh.checked().unwrap();
    assert_eq!(checked.source(), after);
    let span = &checked.world().functions["later"].span;
    assert!(span.start > old_span.start);
    assert!(checked.source()[span.clone()].starts_with("fn later"));
    assert_eq!(old.source(), before); // retained immutable snapshot stays valid
    db.take_executions();
    signature_inventory(&db, file);
    guarded_analysis(&db, file);
    assert!(db.take_executions().is_empty());
}

#[test]
fn string_capacity_is_abi_driving_even_when_header_is_unchanged() {
    let mut db = ObservedDatabase::new();
    let file = SourceFile::new(&db, "fn text()->String{\"a\"}".into());
    let old = complete(&db, file);
    assert_eq!(
        guarded_analysis(&db, file)
            .checked()
            .unwrap()
            .world()
            .string_capacity,
        1
    );
    signature_inventory(&db, file);
    db.take_executions();
    file.set_text(&mut db)
        .to("fn text()->String{\"a longer string\"}".into());
    signature_inventory(&db, file);
    assert_ne!(old, complete(&db, file));
    assert!(executed(&db.take_executions(), "signature_inventory"));
    let current = complete(&db, file);
    file.set_text(&mut db)
        .to("fn text()->String{\"b longer string\"}".into());
    assert_eq!(current, complete(&db, file));
}

#[test]
fn actual_inferred_latent_result_changes_invalidate_identical_source_headers() {
    let mut db = ObservedDatabase::new();
    let prefix = "linear effect Ask{fn ask()->i64;} fn make()->fn()->i64 eff[Ask]";
    let first = format!("{prefix}{{|| Ask::ask()}}");
    let second = format!("{prefix}{{|| 0}}");
    let file = SourceFile::new(&db, first);
    let old = guarded_analysis(&db, file).checked_arc().unwrap();
    let digest = complete(&db, file);
    signature_inventory(&db, file);
    db.take_executions();
    file.set_text(&mut db).to(second);
    let after = guarded_analysis(&db, file).checked().unwrap();
    assert_eq!(
        old.world().functions["make"].result,
        after.world().functions["make"].result
    );
    assert_ne!(
        old.world().function_results["make"],
        after.world().function_results["make"]
    );
    assert_ne!(digest, complete(&db, file));
    signature_inventory(&db, file);
    assert!(executed(&db.take_executions(), "signature_inventory"));
}

#[test]
fn typed_headers_preserve_semantic_names_passes_selectors_modes_and_effects() {
    let pairs = [
        (
            "copy struct Record{body:u64,span:u64}",
            "copy struct Record{body:bool,span:u64}",
        ),
        (
            "copy struct Record{body:u64,span:u64}",
            "copy struct Record{body:u64,span:bool}",
        ),
        ("copy struct Token{}", "affine struct Token{}"),
        ("fn use_value(in x:u64){}", "fn use_value(sink x:u64){}"),
        (
            "copy struct Pair{a:u64,b:u64} fn update(inout p.{a}:Pair){p.a=1;}",
            "copy struct Pair{a:u64,b:u64} fn update(inout p.{b}:Pair){p.b=1;}",
        ),
        (
            "linear effect Log{fn event();} fn f()eff[left:Log]{}",
            "linear effect Log{fn event();} fn f()eff[right:Log]{}",
        ),
        ("linear effect Log{fn event();}", "effect Log{fn event();}"),
        (
            "effect E[T:mod copy]{fn take(in x:T);} fn f()eff[E[u64]]{}",
            "effect E[T:mod copy]{fn take(in x:T);} fn f()eff[E[bool]]{}",
        ),
    ];
    for (before, after) in pairs {
        let mut db = ObservedDatabase::new();
        let file = SourceFile::new(&db, before.into());
        let first = complete(&db, file);
        file.set_text(&mut db).to(after.into());
        assert_ne!(first, complete(&db, file), "{before}\n{after}");
    }
}

#[test]
fn unsupported_trait_headers_use_raw_identity_instead_of_fake_stable_headers() {
    let mut db = ObservedDatabase::new();
    let source = "trait Score{fn score(in self)->u64;} copy struct Item{} impl Score for Item{fn score(in self)->u64{1}} fn main()->u64{0}";
    let file = SourceFile::new(&db, source.into());
    let old = signature_snapshot(&db, file).clone();
    assert!(matches!(
        old.availability(),
        SignatureAvailability::Unavailable { .. }
    ));
    assert!(old.digest().is_none());
    assert_eq!(
        old.fallback_source_identity().unwrap().blake3(),
        Some(*blake3::hash(source.as_bytes()).as_bytes())
    );
    signature_inventory(&db, file);
    db.take_executions();
    file.set_text(&mut db)
        .to(format!("// move source\n{source}"));
    assert_ne!(&old, signature_snapshot(&db, file));
    signature_inventory(&db, file);
    assert!(executed(&db.take_executions(), "signature_inventory"));
}

#[test]
fn parse_and_semantic_failures_recover_to_checked_signatures() {
    let mut db = ObservedDatabase::new();
    let file = SourceFile::new(&db, "fn f( {".into());
    assert!(matches!(
        guarded_analysis(&db, file).checked(),
        Err(AnalysisFailure::Parse { .. })
    ));
    let invalid = signature_snapshot(&db, file).clone();
    assert!(invalid.digest().is_none());
    file.set_text(&mut db).to("fn f()->u64{unknown}".into());
    assert!(matches!(
        guarded_analysis(&db, file).checked(),
        Err(AnalysisFailure::Semantic { .. })
    ));
    assert_ne!(&invalid, signature_snapshot(&db, file));
    file.set_text(&mut db).to("fn f()->_ {42}".into());
    assert!(matches!(
        guarded_analysis(&db, file).checked(),
        Err(AnalysisFailure::Semantic { .. })
    ));
    assert!(matches!(
        signature_snapshot(&db, file).availability(),
        SignatureAvailability::Unavailable { .. }
    ));
    file.set_text(&mut db).to("fn f()->u64{42}".into());
    assert!(complete(&db, file).iter().any(|&byte| byte != 0));
    assert!(
        signature_snapshot(&db, file)
            .fallback_source_identity()
            .is_none()
    );
}

#[test]
fn oversize_legacy_inputs_are_refused_before_hashing_or_source_cloning() {
    let mut db = ObservedDatabase::new();
    let mut source = "x".repeat(128 * 1024 + 1);
    let file = SourceFile::new(&db, source.clone());
    assert!(matches!(
        guarded_analysis(&db, file).checked(),
        Err(AnalysisFailure::Resource { .. })
    ));
    let first = signature_snapshot(&db, file).clone();
    let identity = first.fallback_source_identity().unwrap();
    assert_eq!(identity.byte_len(), source.len());
    assert_eq!(identity.blake3(), None);
    source.replace_range(source.len() - 1.., "y");
    file.set_text(&mut db).to(source);
    let second = signature_snapshot(&db, file);
    assert_ne!(&first, second);
    assert_eq!(second.fallback_source_identity().unwrap().blake3(), None);
}

#[test]
fn hostile_inference_and_nominal_growth_are_refused_by_guarded_queries() {
    // Frontend preflight refuses these patterns before check::check: tuple
    // construction, generic value inference, and repeated nominal expansion.
    let mut tuple = "fn main(){let x=0;".to_owned();
    for _ in 0..25 {
        tuple.push_str("let x=(x,x);");
    }
    tuple.push('}');
    let generic = format!(
        "fn dup[T:mod copy](in x:T)->(T,T){{(x,x)}} fn main(){{{}0{};}}",
        "dup(".repeat(25),
        ")".repeat(25)
    );
    let mut nominal = "copy struct N0{x:u64}".to_owned();
    for i in 1..25 {
        nominal.push_str(&format!("copy struct N{i}{{a:N{},b:N{}}}", i - 1, i - 1));
    }
    for source in [tuple, generic, nominal, "copy struct Node{x:Node}".into()] {
        let db = ObservedDatabase::new();
        let file = SourceFile::new(&db, source);
        assert!(matches!(
            guarded_analysis(&db, file).checked(),
            Err(AnalysisFailure::Unsupported { .. } | AnalysisFailure::Resource { .. })
        ));
        assert!(signature_snapshot(&db, file).digest().is_none());
        let events = db.take_executions();
        assert!(!executed(&events, "check_file"));
        assert!(!executed(&events, "parse_file"));
    }
}

#[test]
fn old_public_queries_retain_batch_behavior() {
    let db = database::Database::default();
    let file = SourceFile::new(&db, "fn main(){}".into());
    assert!(database::parse_file(&db, file).diagnostics.is_empty());
    assert!(database::check_file(&db, file).diagnostics.is_empty());
    assert!(database::diagnostics(&db, file).is_empty());
}
