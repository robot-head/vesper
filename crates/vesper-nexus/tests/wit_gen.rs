//! The `.wit` generator: golden Vesper and Rust output, the Vesper output
//! parses and is canonically formatted, the generated Rust bindings drive a
//! real component, and unsupported constructs are typed errors.
//!
//! Set `VESPER_BLESS=1` to rewrite the golden files after an intended change.

use std::path::PathBuf;
use vesper_nexus::guests::{BLOB_PLUGIN, scenario};
use vesper_nexus::wit_gen::{MAX_WIT_BYTES, WitGenError, generate};
use vesper_nexus::{NexusConfig, NexusEngine, PLUGIN_WIT, PluginError, TenantId, WitHandle};

const KV_WIT: &str = include_str!("wit/kv.wit");

fn golden(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name)
}

fn check_golden(name: &str, actual: &str) {
    let path = golden(name);
    if std::env::var_os("VESPER_BLESS").is_some() {
        std::fs::write(&path, actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path:?}: {e}"));
    assert!(
        expected == actual,
        "{name} differs from the generator output; rerun with VESPER_BLESS=1 if intended.\n\
         --- generated ---\n{actual}"
    );
}

#[test]
fn plugin_world_matches_golden() {
    let b = generate(PLUGIN_WIT, Some("plugin")).unwrap();
    assert_eq!(b.world, "vesper:nexus/plugin@0.1.0");
    check_golden("plugin.ves", &b.vesper);
    check_golden("plugin_bindings.rs", &b.rust);
    // The package has one world, so `None` selects the same one.
    assert_eq!(generate(PLUGIN_WIT, None).unwrap(), b);
}

#[test]
fn kv_world_matches_golden() {
    let b = generate(KV_WIT, Some("plugin-kv")).unwrap();
    assert_eq!(b.world, "vesper:kv/plugin-kv@0.2.0");
    check_golden("kv.ves", &b.vesper);
    check_golden("kv_bindings.rs", &b.rust);
}

#[test]
fn generation_is_deterministic() {
    for _ in 0..3 {
        assert_eq!(
            generate(KV_WIT, None).unwrap(),
            generate(KV_WIT, None).unwrap()
        );
    }
}

/// The generated Vesper parses without diagnostics and is already in
/// `vesper fmt` canonical form (CI runs `vesper fmt --check` on every
/// tracked `.ves` file, including the goldens).
#[test]
fn vesper_output_parses_and_is_canonical() {
    for (wit, world) in [(PLUGIN_WIT, "plugin"), (KV_WIT, "plugin-kv")] {
        let b = generate(wit, Some(world)).unwrap();
        let parsed = vesper_syntax::parser::parse(&b.vesper);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let formatted =
            vesper_fmt::format_source(&b.vesper, vesper_fmt::Limits::default()).unwrap();
        assert!(!formatted.changed, "{}", formatted.output);
    }
}

#[test]
fn vesper_output_has_the_capability_shape() {
    use vesper_syntax::ast::Item;
    let b = generate(PLUGIN_WIT, None).unwrap();
    let module = vesper_syntax::parser::parse(&b.vesper).module;
    let mut effects = Vec::new();
    let mut linear_structs = Vec::new();
    let mut traits = Vec::new();
    for item in &module.items {
        match item {
            Item::Effect(e) => effects.push((
                e.name.clone(),
                e.linear,
                e.ops.iter().map(|f| f.name.clone()).collect::<Vec<_>>(),
            )),
            Item::Adt(a) => linear_structs.push(format!("{a:?}")),
            Item::Trait(t) => traits.push(format!("{t:?}")),
            _ => {}
        }
    }
    assert_eq!(
        effects,
        vec![(
            "Host".to_string(),
            true,
            [
                "blob_new",
                "blob_tag",
                "blob_lend",
                "blob_raw",
                "unlend",
                "consume",
                "peek_raw",
                "blob_drop"
            ]
            .map(String::from)
            .to_vec()
        )]
    );
    assert_eq!(linear_structs.len(), 1);
    assert!(linear_structs[0].contains("Blob") && linear_structs[0].contains("Linear"));
    assert_eq!(traits.len(), 1);
    assert!(traits[0].contains("Plugin") && traits[0].contains("run"));
}

// ---------------------------------------------------------------------------
// The generated Rust bindings, compiled and run against a real component.
// ---------------------------------------------------------------------------

#[allow(dead_code)]
mod plugin_bindings {
    include!("golden/plugin_bindings.rs");
}

#[allow(dead_code)]
mod kv_bindings {
    include!("golden/kv_bindings.rs");
}

use plugin_bindings::host;
use vesper_nexus::table::WitResourceTable;
use wasmtime::component::{Component, Linker, Resource};
use wasmtime::{Engine, Store};

/// A host written only against the generated `Host` trait, backed by the
/// Nexus generational table (raw handle API).
struct TraitHost {
    tenant: TenantId,
    blobs: WitResourceTable<u32>,
    lends: WitResourceTable<WitHandle>,
    drops: u32,
}

#[derive(Debug)]
struct HostRejected(vesper_nexus::WitTableError);
impl std::fmt::Display for HostRejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.0)
    }
}
impl std::error::Error for HostRejected {}

fn reject(e: vesper_nexus::WitTableError) -> wasmtime::Error {
    wasmtime::Error::new(HostRejected(e))
}

impl TraitHost {
    fn handle(&self, rep: u32) -> wasmtime::Result<WitHandle> {
        let generation = self.blobs.slot_state(rep).map_or(0, |s| s.generation);
        let h = WitHandle {
            idx: rep,
            generation,
        };
        self.blobs.lookup(self.tenant, h).map_err(reject)?;
        Ok(h)
    }
}

impl host::Host for TraitHost {
    fn blob_new(&mut self, tag: u32) -> wasmtime::Result<Resource<host::Blob>> {
        let own = self
            .blobs
            .insert_own(self.tenant, tag)
            .map_err(|(_, e)| reject(e))?;
        Ok(Resource::new_own(self.blobs.lower_to_guest(own).idx))
    }
    fn blob_tag(&mut self, blob: Resource<host::Blob>) -> wasmtime::Result<u32> {
        let h = self.handle(blob.rep())?;
        let lend = self.blobs.begin_borrow(self.tenant, h).map_err(reject)?;
        let tag = *self.blobs.get(&lend).map_err(reject)?;
        self.blobs.end_borrow(lend).map_err(|(_, e)| reject(e))?;
        Ok(tag)
    }
    fn blob_lend(&mut self, blob: Resource<host::Blob>) -> wasmtime::Result<u64> {
        let h = self.handle(blob.rep())?;
        self.blobs
            .begin_borrow_raw(self.tenant, h)
            .map_err(reject)?;
        let ticket = self
            .lends
            .insert_own(self.tenant, h)
            .map_err(|(_, e)| reject(e))?;
        Ok(self.lends.lower_to_guest(ticket).to_bits())
    }
    fn blob_raw(&mut self, blob: Resource<host::Blob>) -> wasmtime::Result<u64> {
        Ok(self.handle(blob.rep())?.to_bits())
    }
    fn unlend(&mut self, ticket: u64) -> wasmtime::Result<bool> {
        let Ok(h) = self
            .lends
            .drop_own_raw(self.tenant, WitHandle::from_bits(ticket))
        else {
            return Ok(false);
        };
        Ok(self.blobs.end_borrow_raw(self.tenant, h).is_ok())
    }
    fn consume(&mut self, b: Resource<host::Blob>) -> wasmtime::Result<u32> {
        let h = self.handle(b.rep())?;
        self.blobs.drop_own_raw(self.tenant, h).map_err(reject)
    }
    fn peek_raw(&mut self, raw: u64) -> wasmtime::Result<i32> {
        Ok(self
            .blobs
            .lookup(self.tenant, WitHandle::from_bits(raw))
            .map_or(-1, |t| *t as i32))
    }
    fn blob_drop(&mut self, blob: Resource<host::Blob>) -> wasmtime::Result<()> {
        self.drops += 1;
        let h = self.handle(blob.rep())?;
        self.blobs
            .drop_own_raw(self.tenant, h)
            .map(drop)
            .map_err(reject)
    }
}

fn run_with_generated_bindings(scenario: u32, arg: u64) -> (Result<i64, String>, TraitHost) {
    let engine = Engine::default();
    let mut linker: Linker<TraitHost> = Linker::new(&engine);
    host::add_to_linker(&mut linker, |s| s as &mut dyn host::Host).unwrap();
    let component = Component::new(&engine, BLOB_PLUGIN).unwrap();
    let mut store = Store::new(
        &engine,
        TraitHost {
            tenant: 1,
            blobs: WitResourceTable::with_capacity(16),
            lends: WitResourceTable::with_capacity(16),
            drops: 0,
        },
    );
    let instance = linker.instantiate(&mut store, &component).unwrap();
    let exports = plugin_bindings::Exports::new(&mut store, &instance).unwrap();
    let r = exports
        .run
        .call(&mut store, (scenario, arg))
        .map(|(v,)| v)
        .map_err(|e| format!("{e:?}"));
    (r, store.into_data())
}

fn run_with_nexus_engine(scenario: u32, arg: u64) -> Result<i64, PluginError> {
    let mut e = NexusEngine::new(NexusConfig::default()).unwrap();
    let c = e.compile(BLOB_PLUGIN.as_bytes()).unwrap();
    let mut inst = e.instantiate_with_fuel(&c, 1, 10_000_000).unwrap();
    let r = e.call_run(&mut inst, scenario, arg);
    e.destroy_instance(inst);
    r
}

#[test]
fn generated_bindings_drive_the_blob_component_like_the_hand_written_host() {
    for (s, arg) in [
        (scenario::HAPPY, 0),
        (scenario::LEND_UNLEND_DROP, 0),
        (scenario::PEEK_RAW, 0),
        (scenario::PEEK_RAW, u64::MAX),
        (scenario::UNLEND, 12345),
        (scenario::SPIN, 300),
    ] {
        let (generated, _) = run_with_generated_bindings(s, arg);
        let hand = run_with_nexus_engine(s, arg);
        assert_eq!(generated.as_ref().ok(), hand.as_ref().ok(), "scenario {s}");
        assert!(generated.is_ok(), "scenario {s}: {generated:?}");
    }
    let (r, host) = run_with_generated_bindings(scenario::HAPPY, 0);
    assert_eq!(r, Ok(7007));
    assert_eq!(host.blobs.live_count(), 0);
    // A guest-held own leaks into the table; its raw integer resolves.
    let (r, host) = run_with_generated_bindings(scenario::LEAK_RAW, 42);
    let raw = r.unwrap() as u64;
    assert_eq!(host.blobs.lookup(1, WitHandle::from_bits(raw)), Ok(&42));
    // resource.drop reaches the generated destructor binding.
    let (r, host) = run_with_generated_bindings(scenario::LEND_UNLEND_DROP, 0);
    assert_eq!((r, host.drops), (Ok(1), 1));
}

#[test]
fn generated_bindings_trap_where_the_hand_written_host_traps() {
    for s in [
        scenario::DROP_WHILE_LENT,
        scenario::DOUBLE_DROP,
        scenario::USE_AFTER_MOVE,
        scenario::BORROW_AFTER_DROP,
    ] {
        let (generated, _) = run_with_generated_bindings(s, 0);
        let hand = run_with_nexus_engine(s, 0);
        assert!(generated.is_err(), "scenario {s}: {generated:?}");
        assert!(hand.is_err(), "scenario {s}: {hand:?}");
    }
    // Dropping while lent is the Nexus table's rejection, surfaced through
    // the generated destructor.
    let (r, host) = run_with_generated_bindings(scenario::DROP_WHILE_LENT, 0);
    assert!(r.unwrap_err().contains("ResourceHasActiveBorrows"));
    assert_eq!(host.blobs.live_count(), 1);
}

#[test]
fn generated_exports_reject_a_component_of_another_world() {
    let engine = Engine::default();
    let mut linker: Linker<TraitHost> = Linker::new(&engine);
    host::add_to_linker(&mut linker, |s| s as &mut dyn host::Host).unwrap();
    let component = Component::new(&engine, BLOB_PLUGIN).unwrap();
    let mut store = Store::new(
        &engine,
        TraitHost {
            tenant: 1,
            blobs: WitResourceTable::with_capacity(4),
            lends: WitResourceTable::with_capacity(4),
            drops: 0,
        },
    );
    let instance = linker.instantiate(&mut store, &component).unwrap();
    let err = kv_bindings::Exports::new(&mut store, &instance)
        .err()
        .expect("the blob plugin does not export the kv world");
    assert!(format!("{err:?}").contains("missing export vesper:kv/tuning"));
}

// The kv world's records, enums and variants round-trip through a component
// that uses the generated derives on both sides of the boundary.

/// Implements part of `world plugin-kv`: `tuning#set-level` returns the
/// host's `floor(l)`, and `run(seed)` logs "kv" then returns
/// `ok(seed * 10 + floor(high))`, or `err(fault::denied)` for seed 0.
const KV_GUEST: &str = r#"
(component
  (type $level (enum "low" "high"))
  (type $fault (variant (case "missing" u64) (case "denied") (case "quota" u32)))
  (import "vesper:kv/store@0.2.0" (instance $store
    (export "level" (type $l (eq $level)))
    (export "floor" (func (param "l" $l) (result $l)))
  ))
  (import "log" (func $log (param "msg" string)))
  (core module $mem (memory (export "mem") 1))
  (core instance $mi (instantiate $mem))
  (alias core export $mi "mem" (core memory $memory))
  (core func $floor (canon lower (func $store "floor")))
  (core func $log (canon lower (func $log) (memory $memory)))
  (core module $m
    (import "h" "floor" (func $floor (param i32) (result i32)))
    (import "h" "log" (func $log (param i32 i32)))
    (import "h" "mem" (memory 1))
    (data (i32.const 64) "kv")
    (func (export "set-level") (param $l i32) (result i32)
      (call $floor (local.get $l)))
    (func (export "run") (param $seed i64) (result i32)
      (call $log (i32.const 64) (i32.const 2))
      (if (i64.eqz (local.get $seed)) (then
        ;; result::err at 0, fault::denied at 8
        (i32.store8 (i32.const 0) (i32.const 1))
        (i32.store8 (i32.const 8) (i32.const 1))
        (return (i32.const 0))))
      (i32.store8 (i32.const 0) (i32.const 0))
      (i64.store (i32.const 8)
        (i64.add (i64.mul (local.get $seed) (i64.const 10))
                 (i64.extend_i32_u (call $floor (i32.const 1)))))
      (i32.const 0)))
  (core instance $i (instantiate $m (with "h" (instance
    (export "floor" (func $floor))
    (export "log" (func $log))
    (export "mem" (memory $memory))))))
  (func $set (param "l" $level) (result $level)
    (canon lift (core func $i "set-level")))
  ;; An exported interface's functions must use the instance's own exported
  ;; types, so the instance is built by a nested component, as
  ;; wit-component does.
  (component $tuning_shim
    (type $lv (enum "low" "high"))
    (import "import-type-level" (type $li (eq $lv)))
    (import "import-func-set-level" (func $f (param "l" $li) (result $li)))
    (export $le "level" (type $li))
    (export "set-level" (func $f) (func (param "l" $le) (result $le))))
  (instance $tuning (instantiate $tuning_shim
    (with "import-func-set-level" (func $set))
    (with "import-type-level" (type $level))))
  (export "vesper:kv/tuning@0.2.0" (instance $tuning))
  (export $fault_e "fault" (type $fault))
  (func (export "run") (param "seed" u64) (result (result u64 (error $fault_e)))
    (canon lift (core func $i "run") (memory $memory)))
)
"#;

#[derive(Default)]
struct KvHost {
    logged: Vec<String>,
    floors: u32,
}
impl kv_bindings::plugin_kv::Host for KvHost {
    fn log(&mut self, msg: String) -> wasmtime::Result<()> {
        self.logged.push(msg);
        Ok(())
    }
}
impl kv_bindings::store::Host for KvHost {
    fn cursor_new(&mut self, _: u64) -> wasmtime::Result<Resource<kv_bindings::store::Cursor>> {
        wasmtime::bail!("not used")
    }
    fn cursor_next(
        &mut self,
        _: Resource<kv_bindings::store::Cursor>,
    ) -> wasmtime::Result<Option<kv_bindings::types::Entry>> {
        wasmtime::bail!("not used")
    }
    fn cursor_skip(
        &mut self,
        _: Resource<kv_bindings::store::Cursor>,
        _: u32,
    ) -> wasmtime::Result<Resource<kv_bindings::store::Cursor>> {
        wasmtime::bail!("not used")
    }
    fn get(
        &mut self,
        key: u64,
    ) -> wasmtime::Result<Result<kv_bindings::types::Entry, kv_bindings::types::Fault>> {
        Ok(Err(kv_bindings::types::Fault::Missing(key)))
    }
    fn put(
        &mut self,
        _: kv_bindings::types::Entry,
    ) -> wasmtime::Result<Result<(), kv_bindings::types::Fault>> {
        Ok(Err(kv_bindings::types::Fault::Denied))
    }
    fn span(&mut self, lo: u64, hi: u64) -> wasmtime::Result<(u64, u64)> {
        Ok((lo, hi))
    }
    fn rename(&mut self, _: String, _: String) -> wasmtime::Result<bool> {
        Ok(false)
    }
    fn floor(
        &mut self,
        l: kv_bindings::types::Level,
    ) -> wasmtime::Result<kv_bindings::types::Level> {
        self.floors += 1;
        // The host floors every level to `low`, except it echoes `low`.
        Ok(match l {
            kv_bindings::types::Level::High => kv_bindings::types::Level::Low,
            kv_bindings::types::Level::Low => kv_bindings::types::Level::Low,
        })
    }
    fn cursor_drop(&mut self, _: Resource<kv_bindings::store::Cursor>) -> wasmtime::Result<()> {
        Ok(())
    }
}

#[test]
fn generated_kv_bindings_round_trip_enums_variants_and_strings() {
    use kv_bindings::types::{Fault, Level};
    let engine = Engine::default();
    let mut linker: Linker<KvHost> = Linker::new(&engine);
    kv_bindings::store::add_to_linker(&mut linker, |s| s as &mut dyn kv_bindings::store::Host)
        .unwrap();
    kv_bindings::plugin_kv::add_to_linker(&mut linker, |s| {
        s as &mut dyn kv_bindings::plugin_kv::Host
    })
    .unwrap();
    let component = Component::new(&engine, KV_GUEST).unwrap();
    let mut store = Store::new(&engine, KvHost::default());
    let instance = linker.instantiate(&mut store, &component).unwrap();
    let exports = kv_bindings::Exports::new(&mut store, &instance).unwrap();
    let (r,) = exports.run.call(&mut store, (4,)).unwrap();
    assert_eq!(r, Ok(40));
    let (r,) = exports.run.call(&mut store, (0,)).unwrap();
    assert_eq!(r, Err(Fault::Denied));
    let (l,) = exports
        .tuning_set_level
        .call(&mut store, (Level::High,))
        .unwrap();
    assert_eq!(l, Level::Low);
    assert_eq!(
        store.data().logged,
        vec!["kv".to_string(), "kv".to_string()]
    );
    assert_eq!(store.data().floors, 2);
}

// ---------------------------------------------------------------------------
// Typed rejections
// ---------------------------------------------------------------------------

fn unsupported(wit: &str) -> &'static str {
    match generate(wit, None) {
        Err(WitGenError::Unsupported { construct, .. }) => construct,
        other => panic!("expected Unsupported, got {other:?}"),
    }
}

#[test]
fn unsupported_constructs_are_typed_errors() {
    let world =
        |body: &str| format!("package a:b;\ninterface i {{\n{body}\n}}\nworld w {{ import i; }}\n");
    assert_eq!(unsupported(&world("f: func(x: list<u8>);")), "list");
    assert_eq!(
        unsupported(&world("flags perms { read, write }\nf: func(p: perms);")),
        "flags"
    );
    assert_eq!(unsupported(&world("f: func() -> stream<u8>;")), "stream");
    assert_eq!(unsupported(&world("f: func() -> future<u8>;")), "future");
    assert_eq!(unsupported(&world("f: async func();")), "an async function");
    assert_eq!(
        unsupported(&world(
            "resource r;\nrecord h { x: borrow<r> }\nf: func(v: h);"
        )),
        "a borrow handle stored in a record"
    );
    assert_eq!(
        unsupported("package a:b;\ninterface e { resource r; }\nworld w { export e; }\n"),
        "a guest-exported resource"
    );
    assert_eq!(
        unsupported("package a:b;\nworld w { import i: interface { f: func(); } }\n"),
        "an inline or renamed interface import"
    );
}

#[test]
fn malformed_input_and_bad_worlds_are_typed_errors() {
    assert!(matches!(
        generate("package a:b;\ninterface i { f: func( }", None),
        Err(WitGenError::Parse(_))
    ));
    assert!(matches!(
        generate(PLUGIN_WIT, Some("nope")),
        Err(WitGenError::World(_))
    ));
    assert!(matches!(
        generate("package a:b;\nworld x {}\nworld y {}\n", None),
        Err(WitGenError::World(_))
    ));
    let huge = " ".repeat(MAX_WIT_BYTES + 1);
    assert_eq!(
        generate(&huge, None),
        Err(WitGenError::TooLarge {
            len: MAX_WIT_BYTES + 1,
            max: MAX_WIT_BYTES
        })
    );
}

#[test]
fn colliding_generated_names_are_rejected() {
    // `a-b` and `a-B`... WIT names are case-insensitive kebab; use a
    // record and a resource whose camel names coincide across interfaces.
    let wit = "package a:b;\n\
               interface x { record thing { v: u32 } f: func(t: thing); }\n\
               interface y { resource thing; g: func(t: borrow<thing>); }\n\
               world w { import x; import y; }\n";
    assert!(matches!(
        generate(wit, None),
        Err(WitGenError::NameCollision { name, .. }) if name == "Thing"
    ));
    // An interface named like the world-level import effect.
    let wit = "package a:b;\ninterface w-imports { f: func(); }\n\
               world w { import w-imports; import g: func(); }\n";
    assert!(matches!(
        generate(wit, None),
        Err(WitGenError::NameCollision { name, .. }) if name == "WImports"
    ));
}

#[test]
fn keywords_are_escaped_in_both_outputs() {
    let wit = "package a:b;\ninterface i { record r { %type: u32, sink: u32 } \
               f: func(%in: r, linear: u64) -> u64; }\nworld w { import i; }\n";
    let b = generate(wit, None).unwrap();
    assert!(b.vesper.contains("type_: u32"));
    assert!(b.vesper.contains("sink_: u32"));
    assert!(b.vesper.contains("fn f(in_: R, linear_: u64) -> u64;"));
    assert!(
        b.rust
            .contains("#[component(name = \"type\")]\n        pub type_: u32,")
    );
    // `sink` is not a Rust keyword, so the Rust field keeps its name.
    assert!(b.rust.contains("pub sink: u32,"));
    let parsed = vesper_syntax::parser::parse(&b.vesper);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
}

/// Mutated WIT never panics the generator: every input is `Ok` or a typed
/// error.
#[test]
fn mutated_wit_never_panics() {
    let mut seed = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let alphabet = b"{}()<>;:,.-_@/%abcxyz0123 \n";
    let mut outcomes = [0usize; 2];
    for base in [PLUGIN_WIT, KV_WIT] {
        for _ in 0..300 {
            let mut bytes = base.as_bytes().to_vec();
            for _ in 0..1 + next() % 4 {
                let at = (next() as usize) % bytes.len();
                match next() % 3 {
                    0 => {
                        bytes.remove(at);
                    }
                    1 => bytes.insert(at, alphabet[(next() as usize) % alphabet.len()]),
                    _ => bytes[at] = alphabet[(next() as usize) % alphabet.len()],
                }
            }
            let text = String::from_utf8_lossy(&bytes);
            outcomes[usize::from(generate(&text, None).is_ok())] += 1;
        }
    }
    assert!(outcomes[0] > 0 && outcomes[1] > 0, "{outcomes:?}");
}
