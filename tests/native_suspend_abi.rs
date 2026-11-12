//! Negative and ABI-level checks for checked source to native suspension:
//! wrong bindings, unsupported source shapes, duplicate/stale/wrong-evidence
//! resumes, stale data-frame bases, and object emission for both targets.
use vesper::{check, native::session::*, parser};

const PULSE: &str = r#"
linear struct Ticket { id: u64 }
copy struct Block { lo: u64, hi: u64 }
copy enum Io { Got(u64), IoFailed(u64), IoCancelled }
copy enum Tick { Ticked, TickFailed(u32), TickCancelled }
linear effect Pulse {
    fn pause() -> Tick;
    fn read(inout buf: Block) -> Io;
}
fn main() -> u64 eff [Pulse] {
    let ticket = Ticket { id: 7 };
    let buf = Block { lo: 1, hi: 2 };
    let n: u64 = match Pulse::read(inout buf) { Got(n) => n, IoFailed(c) => c, IoCancelled => 0, };
    let t: u64 = match Pulse::pause() { Ticked => 1, TickFailed(c) => 2, TickCancelled => 3, };
    let Ticket { id } = ticket;
    wrapping_add(wrapping_add(id, n), wrapping_add(t, buf.lo))
}
"#;

fn world(source: &str) -> check::World {
    let parsed = parser::parse(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let checked = check::check(&parsed.module);
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
    checked.world
}

fn pause() -> OperationBinding {
    OperationBinding::new(
        "pause",
        Operation::Yield,
        "Ticked",
        "TickFailed",
        Cancellation::Constructor("TickCancelled".into()),
    )
}

fn read() -> OperationBinding {
    OperationBinding::new(
        "read",
        Operation::Read,
        "Got",
        "IoFailed",
        Cancellation::Constructor("IoCancelled".into()),
    )
}

fn pulse(ops: Vec<OperationBinding>) -> Vec<EffectBinding> {
    vec![EffectBinding {
        effect: "Pulse".into(),
        ops,
    }]
}

fn rejected(world: &check::World, bindings: &[EffectBinding], needle: &str) {
    let error = plan(world, "main", bindings).expect_err("binding must be rejected");
    assert!(error.contains(needle), "expected '{needle}' in: {error}");
    // Rejection happens before any machine code exists, on every path.
    let error = object(world, "main", bindings, Some("x86_64-unknown-linux-gnu"))
        .expect_err("object emission must reject the binding");
    assert!(error.contains(needle), "expected '{needle}' in: {error}");
}

#[test]
fn wrong_effect_and_operation_bindings_are_rejected() {
    let w = world(PULSE);
    assert!(plan(&w, "main", &pulse(vec![pause(), read()])).is_ok());
    rejected(&w, &[], "entry effect Pulse has no native binding");
    rejected(
        &w,
        &pulse(vec![pause()]),
        "operation Pulse::read has no native binding",
    );
    rejected(
        &w,
        &pulse(vec![pause(), read(), read()]),
        "operation Pulse::read is bound twice",
    );
    let mut both = pulse(vec![pause(), read()]);
    both.extend(pulse(vec![pause(), read()]));
    rejected(&w, &both, "effect Pulse is bound twice");
    let mut extra = pulse(vec![pause(), read()]);
    extra.push(EffectBinding {
        effect: "Clock".into(),
        ops: vec![],
    });
    rejected(
        &w,
        &extra,
        "binding names effect Clock which the entry does not declare",
    );
    let mut unknown = read();
    unknown.op = "write".into();
    rejected(
        &w,
        &pulse(vec![pause(), read(), unknown]),
        "effect Pulse has no operation 'write'",
    );
    // Operation kinds must match the checked signatures.
    let mut yield_read = read();
    yield_read.operation = Operation::Yield;
    rejected(
        &w,
        &pulse(vec![pause(), yield_read]),
        "bound to Yield but takes 1 parameters",
    );
    let mut read_pause = pause();
    read_pause.operation = Operation::Read;
    rejected(
        &w,
        &pulse(vec![read_pause, read()]),
        "must take exactly one inout buffer",
    );
    // Result constructors must exist and have the right shapes.
    let mut missing = read();
    missing.value = "Ticked".into();
    rejected(
        &w,
        &pulse(vec![pause(), missing]),
        "has no value constructor 'Ticked'",
    );
    let mut same = read();
    same.error = "Got".into();
    rejected(
        &w,
        &pulse(vec![pause(), same]),
        "value and error constructors must differ",
    );
    let mut cancel_is_value = read();
    cancel_is_value.cancel = Cancellation::Constructor("Got".into());
    rejected(
        &w,
        &pulse(vec![pause(), cancel_is_value]),
        "cancellation constructor 'Got' must differ",
    );
    let mut swapped = read();
    swapped.value = "IoCancelled".into();
    swapped.cancel = Cancellation::Constructor("Got".into());
    rejected(
        &w,
        &pulse(vec![pause(), swapped]),
        "value constructor 'IoCancelled' must carry exactly one u64 byte count",
    );
    let mut field_cancel = pause();
    field_cancel.cancel = Cancellation::Constructor("TickFailed".into());
    rejected(
        &w,
        &pulse(vec![field_cancel, read()]),
        "cancellation constructor 'TickFailed' must differ",
    );
    let mut bad_error = pause();
    bad_error.error = "TickCancelled".into();
    bad_error.cancel = Cancellation::ErrorCode(ECANCELED);
    rejected(
        &w,
        &pulse(vec![bad_error, read()]),
        "error constructor 'TickCancelled' must carry one u32 or u64 code",
    );
}

fn shape(source: &str, bindings: &[EffectBinding], needle: &str) {
    let w = world(source);
    let error = plan(&w, "main", bindings).expect_err("shape must be rejected");
    assert!(error.contains(needle), "expected '{needle}' in: {error}");
}

const TYPES: &str = "copy struct Block { lo: u64, hi: u64 } copy struct Bytes { a: u8 } copy enum Io { Got(u64), IoFailed(u64), IoCancelled } copy enum Tick { Ticked, TickFailed(u32), TickCancelled } linear struct Ticket { id: u64 }";

#[test]
fn unsupported_source_shapes_are_rejected_before_code_exists() {
    let read_only = |sig: &str, body: &str| {
        format!("{TYPES} linear effect Pulse {{ {sig} }} fn main() -> u64 eff [Pulse] {{ {body} }}")
    };
    shape(
        &format!(
            "{TYPES} effect Pulse {{ fn pause() -> Tick; }} fn main() -> u64 eff [Pulse] {{ match Pulse::pause() {{ Ticked => 1, TickFailed(c) => 2, TickCancelled => 3, }} }}"
        ),
        &pulse(vec![pause()]),
        "must be declared linear",
    );
    shape(
        &read_only(
            "fn read(buf: Block) -> Io;",
            "let b = Block { lo: 0, hi: 0 }; match Pulse::read(b) { Got(n) => n, IoFailed(c) => c, IoCancelled => 0, }",
        ),
        &pulse(vec![read()]),
        "its buffer is not passed inout",
    );
    shape(
        &read_only(
            "fn read(sink t: Ticket) -> Io;",
            "let t = Ticket { id: 1 }; match Pulse::read(t) { Got(n) => n, IoFailed(c) => c, IoCancelled => 0, }",
        ),
        &pulse(vec![read()]),
        "its buffer is not passed inout",
    );
    for (buffer, init) in [
        ("Io", "IoCancelled"),
        ("bool", "true"),
        ("u32", "0"),
        ("Bytes", "Bytes { a: 0 }"),
        ("f64", "0.0"),
    ] {
        shape(
            &read_only(
                &format!("fn read(inout buf: {buffer}) -> Io;"),
                &format!(
                    "let b: {buffer} = {init}; match Pulse::read(inout b) {{ Got(n) => n, IoFailed(c) => c, IoCancelled => 0, }}"
                ),
            ),
            &pulse(vec![read()]),
            "word buffer",
        );
    }
    shape(
        &read_only("fn pause() -> u64;", "Pulse::pause()"),
        &pulse(vec![OperationBinding::new(
            "pause",
            Operation::Yield,
            "Ticked",
            "TickFailed",
            Cancellation::ErrorCode(ECANCELED),
        )]),
        "must be an enum",
    );
    shape(
        &format!(
            "{TYPES} linear effect Pulse {{ fn pause() -> Tick; }} fn tick() -> u64 eff [Pulse] {{ match Pulse::pause() {{ Ticked => 1, TickFailed(c) => 2, TickCancelled => 3, }} }} fn go(n: u64) -> u64 eff [Pulse] {{ if n == 0 {{ 0 }} else {{ wrapping_add(tick(), go(wrapping_sub(n, 1))) }} }} fn main() -> u64 eff [Pulse] {{ go(3) }}"
        ),
        &pulse(vec![pause()]),
        "recursive suspending call cycle go -> go is unsupported",
    );
    shape(
        &format!(
            "{TYPES} linear effect Pulse {{ fn pause() -> Tick; }} fn main() -> u64 eff [Pulse, Fail[fn() -> u64]] {{ 1 }}"
        ),
        &pulse(vec![pause()]),
        "contains a function value",
    );
    shape(
        &format!(
            "{TYPES} linear effect Pulse {{ fn pause() -> Tick; }} fn main(x: u64) -> u64 eff [Pulse] {{ x }}"
        ),
        &pulse(vec![pause()]),
        "take no parameters",
    );
    shape(
        &format!(
            "{TYPES} linear effect Chan[T] {{ fn pause() -> Tick; }} fn main() -> u64 eff [Chan[u64]] {{ match Chan::pause() {{ Ticked => 1, TickFailed(c) => 2, TickCancelled => 3, }} }}"
        ),
        &[EffectBinding {
            effect: "Chan".into(),
            ops: vec![pause()],
        }],
        "generic external effect Chan is unsupported",
    );
}

#[test]
fn locals_shadowing_functions_are_not_recursive_calls() {
    // `main` and `tick` are locals here, not calls of the world functions.
    let source = format!(
        "{TYPES} linear effect Pulse {{ fn pause() -> Tick; }} fn tick() -> u64 eff [Pulse] {{ match Pulse::pause() {{ Ticked => 1, TickFailed(c) => 2, TickCancelled => 3, }} }} fn main() -> u64 eff [Pulse] {{ let main: u64 = 40; let tick = |x: u64| wrapping_add(x, 1); wrapping_add(tick(main), 1) }}"
    );
    let w = world(&source);
    plan(&w, "main", &pulse(vec![pause()])).unwrap();
}

#[test]
fn ordinary_jit_and_build_stay_unchanged_for_external_entries() {
    let w = world(PULSE);
    let error = vesper::native::jit(&w, "main").unwrap_err();
    assert_eq!(error, "native entry must handle all effects");
    let error = vesper::native::object(&w, "main").unwrap_err();
    assert_eq!(error, "native entry must handle all effects");
    // A pure entry still compiles as before and can also be a session with
    // no bound effects.
    let pure = world("fn main() -> u64 { wrapping_add(40, 2) }");
    assert_eq!(
        vesper::native::jit(&pure, "main").unwrap().to_string(),
        "42"
    );
    assert!(plan(&pure, "main", &[]).is_ok());
}

/// The ELF machine of an emitted object, and whether it names a symbol.
fn elf(bytes: &[u8]) -> u16 {
    assert_eq!(&bytes[..4], b"\x7fELF");
    u16::from_le_bytes([bytes[18], bytes[19]])
}

fn names(bytes: &[u8], symbol: &str) -> bool {
    bytes.windows(symbol.len()).any(|w| w == symbol.as_bytes())
}

#[test]
fn session_objects_for_x86_64_and_aarch64_export_the_abi_entry() {
    let w = world(PULSE);
    let bindings = pulse(vec![pause(), read()]);
    for (target, machine) in [
        ("x86_64-unknown-linux-gnu", 62u16),
        ("aarch64-unknown-linux-gnu", 183u16),
    ] {
        let (bytes, clif) = object(&w, "main", &bindings, Some(target)).unwrap();
        assert_eq!(elf(&bytes), machine, "{target}");
        assert!(names(&bytes, &entry_symbol("main")), "{target}");
        assert!(names(&bytes, "vesper_fiber_park"), "{target}");
        assert!(clif.matches("call fn").count() >= 2);
    }
    let error = object(&w, "main", &bindings, Some("riscv64gc-unknown-linux-gnu")).unwrap_err();
    assert!(error.contains("x86_64 or AArch64"), "{error}");
}

mod freshness {
    use cranelift_codegen::ir::{
        self, AbiParam, ExtFuncData, ExternalName, InstBuilder, MemFlags, Signature,
        UserExternalName, UserFuncName, types,
    };
    use cranelift_codegen::isa::CallConv;
    use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
    use vesper::native::session::check_data_base_freshness;
    use vesper_fiber_abi::abi::offsets;

    /// Build `fn(ctx)`: optionally load a base, park, then read through
    /// `after` (a base from before the park, from ctx again, or from the
    /// park's signal).
    fn build(after: &str, through_block: bool) -> Result<(), String> {
        let mut sig = Signature::new(CallConv::SystemV);
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(types::I64));
        let mut func = ir::Function::with_name_signature(UserFuncName::user(0, 0), sig);
        let mut fctx = FunctionBuilderContext::new();
        let mut b = FunctionBuilder::new(&mut func, &mut fctx);
        let mut park_sig = Signature::new(CallConv::SystemV);
        park_sig.params.push(AbiParam::new(types::I64));
        park_sig.returns.push(AbiParam::new(types::I64));
        let park_sig = b.import_signature(park_sig);
        let name = b
            .func
            .declare_imported_user_function(UserExternalName::new(0, 1));
        let park = b.import_function(ExtFuncData {
            name: ExternalName::user(name),
            signature: park_sig,
            colocated: false,
        });
        let entry = b.create_block();
        b.append_block_params_for_function_params(entry);
        b.switch_to_block(entry);
        let ctx = b.block_params(entry)[0];
        let old = b
            .ins()
            .load(types::I64, MemFlags::trusted(), ctx, offsets::CTX_DATA_BASE);
        let field = b.ins().iadd_imm(old, 8);
        // Using the old base before the park is fine.
        let early = b.ins().load(types::I64, MemFlags::trusted(), field, 0);
        let field = if through_block {
            let next = b.create_block();
            let p = b.append_block_param(next, types::I64);
            b.ins().jump(next, &[field.into()]);
            b.switch_to_block(next);
            p
        } else {
            field
        };
        let call = b.ins().call(park, &[ctx]);
        let signal = b.inst_results(call)[0];
        let address = match after {
            "stale" => field,
            "ctx" => {
                let fresh =
                    b.ins()
                        .load(types::I64, MemFlags::trusted(), ctx, offsets::CTX_DATA_BASE);
                b.ins().iadd_imm(fresh, 8)
            }
            "signal" => {
                let fresh = b.ins().load(
                    types::I64,
                    MemFlags::trusted(),
                    signal,
                    offsets::RESUME_DATA_BASE,
                );
                b.ins().iadd_imm(fresh, 8)
            }
            _ => unreachable!(),
        };
        let late = b.ins().load(types::I64, MemFlags::trusted(), address, 0);
        let sum = b.ins().iadd(early, late);
        b.ins().return_(&[sum]);
        b.seal_all_blocks();
        b.finalize();
        check_data_base_freshness(
            &func,
            func.dfg.block_params(func.layout.entry_block().unwrap())[0],
            park,
        )
    }

    #[test]
    fn a_base_from_before_a_park_is_rejected_after_it() {
        for through_block in [false, true] {
            let error = build("stale", through_block).unwrap_err();
            assert!(error.contains("stale data-frame base"), "{error}");
            build("ctx", through_block).unwrap();
            build("signal", through_block).unwrap();
        }
    }
}

#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
mod resumes {
    use super::*;
    use vesper_fiber_abi::{
        evidence::{EvidenceRegistry, EvidenceVector, Installation},
        fiber::{ContinuationTable, ResumeError, Resumption},
        prototype::{OP_READ, OP_YIELD},
    };

    fn session() -> (check::World, NativeSession) {
        let w = world(PULSE);
        let s = NativeSession::compile(&w, "main", &pulse(vec![pause(), read()])).unwrap();
        (w, s)
    }

    #[test]
    fn duplicate_and_stale_resumes_are_rejected_without_losing_the_owner() {
        let (w, s) = session();
        let mut table = ContinuationTable::new();
        let k = s.fiber().unwrap().start().parked().unwrap();
        assert_eq!(k.request().op, OP_READ);
        let evidence = k.request().evidence;
        let first = table.insert(k);
        let parked = table
            .resume(first, Resumption::value(evidence, 0))
            .unwrap()
            .parked()
            .unwrap();
        // Duplicate: the token was consumed.
        assert_eq!(
            table
                .resume(first, Resumption::value(evidence, 0))
                .unwrap_err(),
            ResumeError::UnknownToken
        );
        assert_eq!(parked.request().op, OP_YIELD);
        let second = table.insert(parked);
        // Stale: the same fiber is parked again at a newer generation.
        assert!(matches!(
            table
                .resume(first, Resumption::value(evidence, 0))
                .unwrap_err(),
            ResumeError::StaleToken { .. }
        ));
        assert_eq!(table.len(), 1);
        let exit = table
            .resume(second, Resumption::value(evidence, 0))
            .unwrap()
            .exited()
            .unwrap();
        // 7 + 0 bytes + Ticked(1) + lo 1.
        match s.decode_exit(&w, &exit).unwrap() {
            SessionResult::Returned(v) => assert_eq!(v.to_string(), "9"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn another_installation_cannot_resume_a_park() {
        let (w, s) = session();
        let a = s.fiber().unwrap().start().parked().unwrap();
        let b = s.fiber().unwrap().start().parked().unwrap();
        assert_ne!(a.request().evidence, b.request().evidence);
        let rejected = a
            .resume(Resumption::value(b.request().evidence, 0))
            .unwrap_err();
        assert!(matches!(rejected.error, ResumeError::WrongEvidence { .. }));
        let a = rejected.continuation;
        let evidence = a.request().evidence;
        let a = a
            .resume(Resumption::error(evidence, 5))
            .unwrap()
            .parked()
            .unwrap();
        let evidence = a.request().evidence;
        let exit = a.cancel().exited().unwrap();
        match s.decode_exit(&w, &exit).unwrap() {
            // 7 + IoFailed(5) + TickCancelled(3) + lo 1.
            SessionResult::Returned(v) => assert_eq!(v.to_string(), "16"),
            other => panic!("{other:?}"),
        }
        let _ = evidence;
        let _ = b.cancel();
    }

    #[test]
    fn evidence_without_the_session_slots_is_rejected_before_start() {
        let (_, s) = session();
        let empty =
            std::rc::Rc::new(EvidenceVector::install(&mut EvidenceRegistry::new(), &[]).unwrap());
        assert!(
            s.fiber_with(empty)
                .unwrap_err()
                .to_string()
                .contains("evidence slots")
        );
        let wrong = std::rc::Rc::new(
            EvidenceVector::install(&mut EvidenceRegistry::new(), &[Installation::parking(9)])
                .unwrap(),
        );
        assert!(
            s.fiber_with(wrong)
                .unwrap_err()
                .to_string()
                .contains("parking installation of Pulse")
        );
        assert!(s.fiber_with(s.evidence().unwrap()).is_ok());
    }

    #[test]
    fn data_frame_reads_after_a_park_use_the_fresh_base() {
        // Deliver the bytes, then move the frame before resuming: the copy
        // into the inout buffer must read from the moved frame.
        let (w, s) = session();
        let mut k = s.fiber().unwrap().start().parked().unwrap();
        let at = k.request().arg1 as usize;
        k.data_mut()[at..at + 16]
            .copy_from_slice(&[40u64.to_le_bytes(), 0u64.to_le_bytes()].concat());
        let old = k.data_base();
        k.relocate_data();
        assert_ne!(old, k.data_base());
        let evidence = k.request().evidence;
        let k = k
            .resume(Resumption::value(evidence, 16))
            .unwrap()
            .parked()
            .unwrap();
        let evidence = k.request().evidence;
        let exit = k
            .resume(Resumption::value(evidence, 0))
            .unwrap()
            .exited()
            .unwrap();
        assert_eq!(exit.data_relocations, 1);
        match s.decode_exit(&w, &exit).unwrap() {
            // 7 + 16 bytes + Ticked(1) + lo 40.
            SessionResult::Returned(v) => assert_eq!(v.to_string(), "64"),
            other => panic!("{other:?}"),
        }
    }
}
