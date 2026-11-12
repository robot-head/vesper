//! Real Wasmtime component tests: the `blob` resource of `PLUGIN_WIT` is
//! backed by the Nexus table, and own/borrow/drop violations are rejected at
//! the boundary, either by Wasmtime's handle tables or by the Nexus table.

use vesper_nexus::guests::{BLOB_PLUGIN, scenario};
use vesper_nexus::{NexusConfig, NexusEngine, PluginError, WitHandle, WitTableError};

const A: u64 = 0xA;
const B: u64 = 0xB;
const FUEL: u64 = 10_000_000;

fn engine() -> (NexusEngine, vesper_nexus::CompiledComponent) {
    let e = NexusEngine::new(NexusConfig::default()).expect("engine");
    let c = e
        .compile(BLOB_PLUGIN.as_bytes())
        .expect("component compiles");
    (e, c)
}

#[test]
fn wit_text_describes_the_guest_world() {
    // The hand-written component implements exactly this interface.
    for name in [
        "resource blob",
        "constructor(tag: u32)",
        "tag: func() -> u32",
        "lend: func() -> u64",
        "raw: func() -> u64",
        "unlend: func(ticket: u64) -> bool",
        "consume: func(b: blob) -> u32",
        "peek-raw: func(raw: u64) -> s32",
        "export run: func(scenario: u32, arg: u64) -> s64",
    ] {
        assert!(vesper_nexus::PLUGIN_WIT.contains(name), "{name}");
    }
    for name in [
        "[constructor]blob",
        "[method]blob.tag",
        "[method]blob.lend",
        "[method]blob.raw",
        "\"unlend\"",
        "\"consume\"",
        "\"peek-raw\"",
    ] {
        assert!(BLOB_PLUGIN.contains(name), "{name}");
    }
}

#[test]
fn own_borrow_and_consume_round_trip() {
    let (mut e, c) = engine();
    let mut inst = e.instantiate_with_fuel(&c, A, FUEL).unwrap();
    assert_eq!(e.call_run(&mut inst, scenario::HAPPY, 0), Ok(7007));
    assert_eq!(e.shared().table.live_count(), 0, "consume dropped the own");
    assert!(inst.fuel_consumed() > 0);
    assert_eq!(e.call_run(&mut inst, scenario::LEND_UNLEND_DROP, 0), Ok(1));
    assert_eq!(e.shared().table.live_count(), 0);
    assert_eq!(e.shared().open_lends(A), 0);
    let report = e.destroy_instance(inst);
    assert_eq!(report.reclaimed_resources, 0);
    assert_eq!(e.leaked_instances(), 0);
    assert_eq!(e.shared().table.leaked_obligations(), 0);
}

#[test]
fn drop_while_lent_is_rejected_by_the_nexus_table() {
    let (mut e, c) = engine();
    let mut inst = e.instantiate_with_fuel(&c, A, FUEL).unwrap();
    let err = e
        .call_run(&mut inst, scenario::DROP_WHILE_LENT, 0)
        .unwrap_err();
    let PluginError::Table(WitTableError::ResourceHasActiveBorrows { active_borrows, .. }) = err
    else {
        panic!("expected active-borrow rejection, got {err:?}");
    };
    assert_eq!(active_borrows, 1);
    // The resource survives the rejected drop, and the trap poisons the instance.
    assert_eq!(e.shared().table.live_count_for(A), 1);
    assert!(inst.is_poisoned());
    assert_eq!(
        e.call_run(&mut inst, scenario::HAPPY, 0),
        Err(PluginError::Poisoned)
    );
    let report = e.destroy_instance(inst);
    assert_eq!((report.reclaimed_resources, report.reclaimed_lends), (1, 1));
    assert_eq!(e.shared().table.live_count(), 0);
    assert_eq!(e.shared().table.leaked_obligations(), 0);
}

#[test]
fn wasmtime_rejects_double_drop_use_after_move_and_borrow_after_drop() {
    let (mut e, c) = engine();
    for s in [
        scenario::DOUBLE_DROP,
        scenario::USE_AFTER_MOVE,
        scenario::BORROW_AFTER_DROP,
    ] {
        let mut inst = e.instantiate_with_fuel(&c, A, FUEL).unwrap();
        let err = e.call_run(&mut inst, s, 0).unwrap_err();
        assert!(
            matches!(&err, PluginError::GuestTrap(m) if m.contains("unknown handle index")),
            "scenario {s}: {err:?}"
        );
        // The first drop/consume released the slot; nothing is left behind.
        let report = e.destroy_instance(inst);
        assert_eq!(report.reclaimed_resources, 0, "scenario {s}");
    }
    assert_eq!(e.shared().table.live_count(), 0);
}

#[test]
fn forged_stale_and_cross_tenant_integers_resolve_to_nothing() {
    let (mut e, c) = engine();
    let mut a = e.instantiate_with_fuel(&c, A, FUEL).unwrap();
    let mut b = e.instantiate_with_fuel(&c, B, FUEL).unwrap();
    let raw = e.call_run(&mut a, scenario::LEAK_RAW, 42).unwrap() as u64;
    let h = WitHandle::from_bits(raw);
    // Tenant A resolves its own integer.
    assert_eq!(e.call_run(&mut a, scenario::PEEK_RAW, raw), Ok(42));
    // Tenant B presenting A's exact integer gets the generic invalid code.
    assert_eq!(e.call_run(&mut b, scenario::PEEK_RAW, raw), Ok(-1));
    // Forged neighbours: wrong generation, wrong index, garbage.
    let wrong_gen = WitHandle {
        generation: h.generation + 1,
        ..h
    }
    .to_bits();
    let wrong_idx = WitHandle {
        idx: h.idx + 7,
        ..h
    }
    .to_bits();
    for forged in [wrong_gen, wrong_idx, 0, u64::MAX] {
        assert_eq!(e.call_run(&mut a, scenario::PEEK_RAW, forged), Ok(-1));
    }
    let rejected: Vec<_> = e
        .shared()
        .rejections
        .iter()
        .map(|r| r.error.clone())
        .collect();
    assert_eq!(rejected[0], WitTableError::UnauthorizedTenant { actual: B });
    assert!(
        rejected[1..]
            .iter()
            .all(|e| matches!(e, WitTableError::InvalidOrStaleHandle(_)))
    );
    // Teardown of A reclaims the leaked own; the integer is now stale.
    let report = e.destroy_instance(a);
    assert_eq!(report.reclaimed_resources, 1);
    assert_eq!(e.call_run(&mut b, scenario::PEEK_RAW, raw), Ok(-1));
    let mut a2 = e.instantiate_with_fuel(&c, A, FUEL).unwrap();
    assert_eq!(
        e.call_run(&mut a2, scenario::PEEK_RAW, raw),
        Ok(-1),
        "stale after teardown"
    );
    // A new resource reuses the slot under a new generation.
    let raw2 = e.call_run(&mut a2, scenario::LEAK_RAW, 9).unwrap() as u64;
    let h2 = WitHandle::from_bits(raw2);
    assert_eq!(h2.idx, h.idx);
    assert!(h2.generation > h.generation);
    e.destroy_instance(a2);
    e.destroy_instance(b);
    assert_eq!(e.shared().table.live_count(), 0);
}

#[test]
fn lend_tickets_are_tenant_scoped_and_reclaimed() {
    let (mut e, c) = engine();
    let mut a = e.instantiate_with_fuel(&c, A, FUEL).unwrap();
    let mut b = e.instantiate_with_fuel(&c, B, FUEL).unwrap();
    let ticket = e.call_run(&mut a, scenario::LEAK_LEND, 0).unwrap() as u64;
    assert_eq!(e.shared().open_lends(A), 1);
    // B cannot end A's lend (which would unblock a drop of A's resource).
    assert_eq!(e.call_run(&mut b, scenario::UNLEND, ticket), Ok(0));
    assert_eq!(e.call_run(&mut a, scenario::UNLEND, ticket ^ 1), Ok(0));
    assert_eq!(e.shared().open_lends(A), 1);
    assert_eq!(e.call_run(&mut a, scenario::UNLEND, ticket), Ok(1));
    assert_eq!(
        e.call_run(&mut a, scenario::UNLEND, ticket),
        Ok(0),
        "ticket is one-shot"
    );
    let ticket = e.call_run(&mut a, scenario::LEAK_LEND, 0).unwrap();
    assert!(ticket != 0);
    let report = e.destroy_instance(a);
    assert_eq!((report.reclaimed_resources, report.reclaimed_lends), (2, 1));
    e.destroy_instance(b);
    let sh = e.shared();
    assert_eq!((sh.table.live_count(), sh.open_lends(A)), (0, 0));
    assert_eq!(sh.table.leaked_obligations(), 0);
}

#[test]
fn dropped_instance_is_a_counted_leak() {
    let (mut e, c) = engine();
    let inst = e.instantiate_with_fuel(&c, A, FUEL).unwrap();
    drop(inst);
    assert_eq!(e.leaked_instances(), 1);
}

#[test]
fn out_of_fuel_traps_and_teardown_still_reclaims() {
    let (mut e, c) = engine();
    let mut inst = e.instantiate_with_fuel(&c, A, 5_000).unwrap();
    assert_eq!(
        e.call_run(&mut inst, scenario::SPIN, 1_000_000),
        Err(PluginError::OutOfFuel)
    );
    assert_eq!(inst.remaining_fuel(), 0);
    let report = e.destroy_instance(inst);
    assert_eq!(report.fuel_consumed, 5_000);
}
