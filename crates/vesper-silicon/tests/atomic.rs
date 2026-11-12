//! IrqGuard / sleep-in-atomic / lock-order checks over Cranelift functions.

mod common;

use common::{branchy, func};
use vesper_silicon::Arch;
use vesper_silicon::atomic::*;
use vesper_silicon::stack::Program;

fn policy() -> AtomicPolicy {
    AtomicPolicy {
        guard_acquire: vec!["cpu_irq_disable_local_save".into()],
        guard_release: vec!["cpu_irq_restore_local".into()],
        locks: vec![
            LockSpec {
                name: "stats".into(),
                acquire: "spin_lock_stats".into(),
                release: "spin_unlock_stats".into(),
                isr_shared: true,
            },
            LockSpec {
                name: "queue".into(),
                acquire: "spin_lock_queue".into(),
                release: "spin_unlock_queue".into(),
                isr_shared: false,
            },
        ],
        suspending: vec!["dma_await_complete".into(), "fiber_yield".into()],
        allocating: vec!["heap_alloc".into()],
        isr_roots: vec![],
        indirect_targets: Default::default(),
    }
}

fn rules(program: &Program, p: &AtomicPolicy) -> Vec<&'static str> {
    check_atomic(program, p)
        .into_iter()
        .map(|f| f.rule)
        .collect()
}

const G: &str = "cpu_irq_disable_local_save";
const R: &str = "cpu_irq_restore_local";

#[test]
fn guarded_isr_shared_lock_is_accepted() {
    let fs = vec![
        func(
            "record",
            0,
            &[G, "spin_lock_stats", "bump", "spin_unlock_stats", R],
            false,
        ),
        func("bump", 16, &[], false),
    ];
    for arch in [Arch::X86_64, Arch::Aarch64] {
        let p = Program::compile(arch, fs.clone()).unwrap();
        assert!(rules(&p, &policy()).is_empty());
    }
}

#[test]
fn isr_shared_lock_without_guard_is_rejected() {
    let fs = vec![func(
        "record",
        0,
        &["spin_lock_stats", "spin_unlock_stats"],
        false,
    )];
    let p = Program::compile(Arch::X86_64, fs).unwrap();
    let f = check_atomic(&p, &policy());
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].rule, RULE_LOCK_WITHOUT_GUARD);
    assert!(f[0].inst.is_some());
    // A lock not shared with an ISR does not need the guard.
    let fs = vec![func(
        "q",
        0,
        &["spin_lock_queue", "spin_unlock_queue"],
        false,
    )];
    let p = Program::compile(Arch::X86_64, fs).unwrap();
    assert!(rules(&p, &policy()).is_empty());
}

#[test]
fn suspending_and_allocating_while_guarded_are_rejected_transitively() {
    let fs = vec![
        func("bad", 0, &[G, "helper", R], false),
        func("helper", 0, &["inner"], false),
        func("inner", 0, &["dma_await_complete"], false),
        func(
            "bad_alloc",
            0,
            &["spin_lock_queue", "heap_alloc", "spin_unlock_queue"],
            false,
        ),
        // Suspending outside atomic context is fine.
        func("fine", 0, &["helper", G, R], false),
    ];
    let p = Program::compile(Arch::Aarch64, fs).unwrap();
    let f = check_atomic(&p, &policy());
    let sleep: Vec<_> = f
        .iter()
        .filter(|x| x.rule == RULE_SLEEP_IN_ATOMIC)
        .collect();
    assert_eq!(sleep.len(), 1, "{f:?}");
    assert_eq!(sleep[0].function, "bad");
    assert!(
        sleep[0]
            .message
            .contains("helper -> inner -> dma_await_complete"),
        "{}",
        sleep[0].message
    );
    let alloc: Vec<_> = f
        .iter()
        .filter(|x| x.rule == RULE_ALLOC_IN_ATOMIC)
        .collect();
    assert_eq!(alloc.len(), 1);
    assert_eq!(alloc[0].function, "bad_alloc");
    assert_eq!(f.len(), 2, "{f:?}");
}

#[test]
fn isr_roots_may_not_reach_suspension() {
    let fs = vec![
        func(
            "eth_mac_rx_isr",
            0,
            &["spin_lock_stats", "spin_unlock_stats", "notify"],
            false,
        ),
        func("notify", 0, &["fiber_yield"], false),
        func(
            "good_isr",
            0,
            &["spin_lock_stats", "spin_unlock_stats"],
            false,
        ),
    ];
    let p = Program::compile(Arch::X86_64, fs).unwrap();
    let mut pol = policy();
    pol.isr_roots = vec!["eth_mac_rx_isr".into(), "good_isr".into()];
    let f = check_atomic(&p, &pol);
    // ISR context may take the ISR-shared lock without an IrqGuard.
    assert!(
        !f.iter().any(|x| x.rule == RULE_LOCK_WITHOUT_GUARD),
        "{f:?}"
    );
    let isr: Vec<_> = f.iter().filter(|x| x.rule == RULE_SUSPEND_IN_ISR).collect();
    assert_eq!(isr.len(), 1);
    assert_eq!(isr[0].function, "eth_mac_rx_isr");
    assert!(isr[0].message.contains("notify -> fiber_yield"));
    assert!(!f.iter().any(|x| x.function == "good_isr"), "{f:?}");
}

#[test]
fn unbalanced_guards_and_escapes_are_rejected() {
    let fs = vec![
        // Guard taken on one branch only.
        branchy("half", &[G], &[], &[R]),
        func("leaks", 0, &[G], false),
        func("double_release", 0, &[G, R, R], false),
        func(
            "relock",
            0,
            &[
                "spin_lock_queue",
                "spin_lock_queue",
                "spin_unlock_queue",
                "spin_unlock_queue",
            ],
            false,
        ),
        // Balanced on both branches.
        branchy("both", &[G, R], &[G, R], &[]),
    ];
    let p = Program::compile(Arch::X86_64, fs).unwrap();
    let f = check_atomic(&p, &policy());
    let by = |name: &str| {
        f.iter()
            .filter(|x| x.function == name)
            .map(|x| x.rule)
            .collect::<Vec<_>>()
    };
    assert!(by("half").contains(&RULE_UNBALANCED_JOIN), "{f:?}");
    assert_eq!(by("leaks"), [RULE_ESCAPES]);
    assert_eq!(by("double_release"), [RULE_UNHELD_RELEASE]);
    assert!(by("relock").contains(&RULE_RECURSIVE_LOCK));
    assert!(by("both").is_empty(), "{f:?}");
}

#[test]
fn lock_order_cycles_are_rejected_including_through_callees() {
    let fs = vec![
        func(
            "ab",
            0,
            &[G, "spin_lock_stats", "take_queue", "spin_unlock_stats", R],
            false,
        ),
        func(
            "take_queue",
            0,
            &["spin_lock_queue", "spin_unlock_queue"],
            false,
        ),
        func(
            "ba",
            0,
            &[
                G,
                "spin_lock_queue",
                "spin_lock_stats",
                "spin_unlock_stats",
                "spin_unlock_queue",
                R,
            ],
            false,
        ),
    ];
    let p = Program::compile(Arch::Aarch64, fs).unwrap();
    let f = check_atomic(&p, &policy());
    assert_eq!(f.len(), 1, "{f:?}");
    assert_eq!(f[0].rule, RULE_LOCK_ORDER);
    assert!(f[0].message.contains("stats") && f[0].message.contains("queue"));
    // Consistent order everywhere: no cycle.
    let fs = vec![
        func(
            "ab",
            0,
            &[G, "spin_lock_stats", "take_queue", "spin_unlock_stats", R],
            false,
        ),
        func(
            "take_queue",
            0,
            &["spin_lock_queue", "spin_unlock_queue"],
            false,
        ),
    ];
    let p = Program::compile(Arch::Aarch64, fs).unwrap();
    assert!(rules(&p, &policy()).is_empty());
}

#[test]
fn indirect_calls_in_atomic_context_need_targets() {
    // `func(.., indirect=true)` places the indirect call after the direct ones.
    let fs = vec![func("cb", 0, &[G], true), func("h", 0, &[], false)];
    let p = Program::compile(Arch::X86_64, fs).unwrap();
    let f = check_atomic(&p, &policy());
    assert!(
        f.iter().any(|x| x.rule == RULE_UNRESOLVED_IN_ATOMIC),
        "{f:?}"
    );
}

#[test]
fn helpers_shared_by_isr_and_thread_code_still_need_the_guard() {
    let fs = vec![
        func("isr", 0, &["bump_stats"], false),
        func("thread_entry", 0, &["bump_stats"], false),
        func(
            "bump_stats",
            0,
            &["spin_lock_stats", "spin_unlock_stats"],
            false,
        ),
        func(
            "isr_only_helper",
            0,
            &["spin_lock_stats", "spin_unlock_stats"],
            false,
        ),
        func("isr2", 0, &["isr_only_helper"], false),
    ];
    let p = Program::compile(Arch::X86_64, fs).unwrap();
    let mut pol = policy();
    pol.isr_roots = vec!["isr".into(), "isr2".into()];
    let f = check_atomic(&p, &pol);
    let unguarded: Vec<_> = f
        .iter()
        .filter(|x| x.rule == RULE_LOCK_WITHOUT_GUARD)
        .map(|x| x.function.as_str())
        .collect();
    assert_eq!(unguarded, ["bump_stats"], "{f:?}");
}

#[test]
fn restoring_interrupts_while_isr_lock_held_is_rejected() {
    let fs = vec![
        func(
            "early_restore",
            0,
            &[G, "spin_lock_stats", R, "spin_unlock_stats"],
            false,
        ),
        func(
            "nested_ok",
            0,
            &[G, G, "spin_lock_stats", R, "spin_unlock_stats", R],
            false,
        ),
    ];
    let p = Program::compile(Arch::Aarch64, fs).unwrap();
    let f = check_atomic(&p, &policy());
    assert_eq!(f.len(), 1, "{f:?}");
    assert_eq!(f[0].rule, RULE_RESTORE_WITH_LOCK);
    assert_eq!(f[0].function, "early_restore");
}
