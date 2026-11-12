use vesper_tensor::{shape::*, Error};

#[test]
fn canonical_products_are_checked_and_keep_runtime_obligations() -> Result<(), Error> {
    let mut d = Dimensions::new(64, 16, 16)?;
    let batch = d.symbol("Batch")?;
    let heads = d.symbol("Heads")?;
    let two = d.constant(2)?;
    let left_pair = d.multiply(batch, two)?;
    let left = d.multiply(left_pair, heads)?;
    let right_pair = d.multiply(heads, batch)?;
    let right = d.multiply(two, right_pair)?;
    let env = Environment::new();
    assert_eq!(d.normalize(left, &env)?, d.normalize(right, &env)?);
    let checked = d.unify(left, right, &env)?;
    assert!(!checked.is_resolved());
    assert_eq!(checked.discharge(&d, &env), Err(Error::Unresolved));
    let mut env = Environment::from([("Batch".into(), 4), ("Heads".into(), 8)]);
    checked.discharge(&d, &env)?;
    assert_eq!(d.value(left, &env)?, 64);
    env.insert("Batch".into(), u64::MAX);
    assert_eq!(checked.discharge(&d, &env), Err(Error::Overflow));
    Ok(())
}

#[test]
fn ambiguous_equality_never_infers_a_factor_or_cancels_zero() -> Result<(), Error> {
    let mut d = Dimensions::new(32, 16, 16)?;
    let a = d.symbol("A")?;
    let b = d.symbol("B")?;
    let ab = d.multiply(a, b)?;
    let twelve = d.constant(12)?;
    let pending = d.unify(ab, twelve, &Environment::new())?;
    assert!(matches!(pending.obligations(), [Obligation::Equal(_, _)]));
    for (a, b, ok) in [(3, 4, true), (2, 6, true), (4, 4, false), (0, 12, false)] {
        let result = pending.discharge(&d, &Environment::from([("A".into(), a), ("B".into(), b)]));
        assert_eq!(result.is_ok(), ok);
    }
    let zero = d.constant(0)?;
    let za = d.multiply(zero, a)?;
    assert!(!d.unify(za, zero, &Environment::new())?.is_resolved());
    let max = d.constant(u64::MAX)?;
    let two = d.constant(2)?;
    let overflowing = d.multiply(max, two)?;
    let zero_overflow = d.multiply(zero, overflowing)?;
    assert_eq!(
        d.normalize(zero_overflow, &Environment::new()),
        Err(Error::Overflow)
    );
    Ok(())
}

#[test]
fn matmul_checks_contraction_zero_block_overflow_and_output_budget() -> Result<(), Error> {
    let mut d = Dimensions::new(64, 16, 16)?;
    let m = d.constant(3)?;
    let k = d.symbol("K")?;
    let rk = d.constant(64)?;
    let n = d.symbol("N")?;
    let axes = [m, k, rk, n];
    let pending = d.matmul(axes, Some(32), &Environment::new())?;
    assert!(!pending.is_resolved());
    let env = Environment::from([("K".into(), 64), ("N".into(), 7)]);
    pending.discharge(&d, &env)?;
    assert_eq!(d.resolved_matmul(axes, Some(32), &env, 21)?, [3, 7]);
    assert!(matches!(
        d.resolved_matmul(axes, Some(32), &env, 20),
        Err(Error::Limit(_))
    ));
    assert!(matches!(
        d.matmul(axes, Some(0), &env),
        Err(Error::Shape(_))
    ));
    let mismatch = Environment::from([("K".into(), 32), ("N".into(), 7)]);
    assert!(matches!(
        d.matmul(axes, Some(32), &mismatch),
        Err(Error::Shape(_))
    ));
    let bad_align = d.constant(33)?;
    assert!(d
        .matmul([m, bad_align, bad_align, m], Some(32), &env)
        .is_err());
    let zero = d.constant(0)?;
    assert!(d.matmul([zero, rk, rk, m], None, &env).is_err());
    let max = d.constant(u64::MAX)?;
    assert_eq!(d.matmul([max, rk, rk, m], None, &env), Err(Error::Overflow));
    let overflow_env = Environment::from([("K".into(), 64), ("N".into(), u64::MAX)]);
    assert_eq!(pending.discharge(&d, &overflow_env), Err(Error::Overflow));
    Ok(())
}

#[test]
fn gqa_and_rope_reject_all_zero_ratios_and_odd_widths() -> Result<(), Error> {
    for q in 0..20 {
        for kv in 0..12 {
            let ratio = gqa_ratio(q, kv);
            let valid = q > 0 && kv > 0 && q % kv == 0;
            assert_eq!(ratio.is_ok(), valid, "{q}/{kv}");
            if valid {
                for head in 0..q {
                    assert!(gqa_kv_head(q, kv, head)? < kv);
                }
                assert!(gqa_kv_head(q, kv, q).is_err());
            }
        }
    }
    let mut d = Dimensions::new(16, 8, 8)?;
    let q = d.symbol("QueryHeads")?;
    let kv = d.symbol("KvHeads")?;
    let w = d.symbol("HeadDim")?;
    let pending = d.gqa_rope(q, kv, w, &Environment::new())?;
    for (query, kv, width, valid) in [
        (32, 8, 128, true),
        (0, 8, 128, false),
        (32, 0, 128, false),
        (32, 8, 0, false),
        (32, 8, 3, false),
        (31, 8, 128, false),
    ] {
        let env = Environment::from([
            ("QueryHeads".into(), query),
            ("KvHeads".into(), kv),
            ("HeadDim".into(), width),
        ]);
        assert_eq!(pending.discharge(&d, &env).is_ok(), valid);
    }
    Ok(())
}

#[test]
fn expression_limits_and_foreign_ids_fail_without_recursion() -> Result<(), Error> {
    assert!(Dimensions::new(4097, 2, 2).is_err());
    let mut d = Dimensions::new(8, 3, 2)?;
    let a = d.symbol("Seq")?;
    assert!(d.symbol("").is_err());
    assert!(d.symbol(&"x".repeat(129)).is_err());
    let aa = d.multiply(a, a)?;
    let aaa = d.multiply(aa, a)?;
    assert!(matches!(
        d.normalize(aaa, &Environment::new()),
        Err(Error::Limit(_))
    ));
    assert!(matches!(d.multiply(aaa, a), Err(Error::Limit(_))));
    let mut foreign = Dimensions::new(8, 8, 8)?;
    let f = foreign.constant(4)?;
    assert_eq!(d.value(f, &Environment::new()), Err(Error::InvalidId));
    assert_eq!(d.multiply(a, f), Err(Error::InvalidId));
    let mut tiny = Dimensions::new(1, 1, 1)?;
    tiny.constant(1)?;
    assert!(matches!(tiny.constant(2), Err(Error::Limit(_))));
    Ok(())
}

#[test]
fn input_matmul_products_are_checked_even_with_small_output() -> Result<(), Error> {
    let mut d = Dimensions::new(8, 8, 8)?;
    let large = d.constant(u64::MAX)?;
    let two = d.constant(2)?;
    let one = d.constant(1)?;
    assert_eq!(
        d.matmul([two, large, large, one], None, &Environment::new()),
        Err(Error::Overflow)
    );
    let symbolic = d.symbol("K")?;
    let pending = d.matmul([two, symbolic, symbolic, one], None, &Environment::new())?;
    assert_eq!(
        pending.discharge(&d, &Environment::from([("K".into(), u64::MAX)])),
        Err(Error::Overflow)
    );
    Ok(())
}

#[test]
fn final_bindings_recheck_predicates_previously_proven_by_an_environment() -> Result<(), Error> {
    let mut d = Dimensions::new(8, 8, 8)?;
    let k = d.symbol("K")?;
    let fixed = d.constant(32)?;
    let one = d.constant(1)?;
    let env = Environment::from([("K".into(), 32)]);
    let checked = d.matmul([one, k, fixed, one], Some(32), &env)?;
    assert!(checked.is_resolved());
    checked.discharge(&d, &env)?;
    assert_eq!(
        checked.discharge(&d, &Environment::new()),
        Err(Error::Unresolved)
    );
    assert!(checked
        .discharge(&d, &Environment::from([("K".into(), 64)]))
        .is_err());
    Ok(())
}
