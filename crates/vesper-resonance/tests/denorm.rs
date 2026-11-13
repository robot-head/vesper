//! The ConstDenorm scope on real hardware control registers.

use std::hint::black_box;
use vesper_resonance::denorm::{
    DenormMode, availability, control_word, flush_active, with_const_denorm,
};

/// A one-pole IIR decaying toward zero from `x0` for `steps` samples.
fn decay(x0: f32, steps: u32) -> f32 {
    let mut y = black_box(x0);
    let a = black_box(0.5f32);
    for _ in 0..steps {
        y = black_box(a * y);
    }
    y
}

#[test]
fn decaying_iir_flushes_to_zero_inside_the_scope_only() {
    let start = black_box(f32::MIN_POSITIVE * 4.0); // 4 * 2^-126
    // Outside: three halvings give a subnormal (2^-127 * ... ).
    let outside = decay(start, 3);
    match availability() {
        Ok(mode) => {
            assert!(outside > 0.0 && outside.is_subnormal(), "{outside:e}");
            assert!(!flush_active());
            // Captured after the outside run: on x86_64 MXCSR also holds
            // sticky exception flags (the subnormal result sets DE/UE).
            let before = control_word();
            let (inside, active, subnormal_input) = with_const_denorm(|| {
                let y = decay(start, 3);
                // DAZ / FZ also treats a subnormal *input* as zero.
                let sub = black_box(f32::from_bits(1));
                let product = black_box(sub * black_box(1.0f32));
                (y, flush_active(), product)
            })
            .unwrap();
            assert!(active);
            assert_eq!(inside, 0.0, "{mode:?}: decayed value should flush to zero");
            assert_eq!(
                subnormal_input, 0.0,
                "{mode:?}: subnormal input should read as zero"
            );
            assert_eq!(control_word(), before, "control word restored exactly");
            assert!(!flush_active());
            eprintln!("ConstDenorm exercised: {mode:?}");
            assert!(matches!(mode, DenormMode::MxcsrFtzDaz | DenormMode::FpcrFz));
        }
        Err(e) => {
            eprintln!("ConstDenorm unavailable: {e}");
            assert!(with_const_denorm(|| ()).is_err());
        }
    }
    // Normal arithmetic keeps working afterwards.
    assert!(decay(start, 3).is_subnormal() || availability().is_err());
}

#[test]
fn scope_restores_on_panic() {
    if availability().is_err() {
        return;
    }
    let before = control_word();
    let r = std::panic::catch_unwind(|| {
        with_const_denorm(|| {
            assert!(flush_active());
            panic!("inside realtime scope");
        })
    });
    assert!(r.is_err());
    assert_eq!(control_word(), before);
    assert!(!flush_active());
}
