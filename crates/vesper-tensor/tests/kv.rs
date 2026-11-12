use vesper_tensor::kv::*;
fn config(pages: usize, sequences: usize) -> KvConfig {
    KvConfig {
        heads: 2,
        head_dim: 3,
        pages,
        max_sequences: sequences,
        max_tokens_per_sequence: pages * 16,
        max_bytes: 64 * 1024,
    }
}
fn payload(token: usize, salt: u16) -> Vec<u8> {
    (0..6)
        .flat_map(|i| (salt.wrapping_add((token * 7 + i) as u16)).to_le_bytes())
        .collect()
}
fn step(bytes: &[u8]) -> Bf16Step<'_> {
    Bf16Step {
        heads: 2,
        head_dim: 3,
        bytes,
    }
}
fn append(pool: &mut KvPool, seq: &mut Sequence, token: usize, salt: u16) -> Result<(), KvError> {
    let k = payload(token, salt);
    let v = payload(token, salt ^ 0xffff);
    pool.append(seq, token, step(&k), step(&v))
}
fn verify(pool: &KvPool, seq: &Sequence, len: usize, salt: u16) -> Result<(), KvError> {
    assert_eq!(pool.len(seq)?, len);
    for token in 0..len {
        let data = pool.read(seq, token)?;
        assert_eq!(data.key, payload(token, salt));
        assert_eq!(data.value, payload(token, salt ^ 0xffff));
        let head = pool.read_head(seq, token, 1)?;
        assert_eq!(head.key, &payload(token, salt)[6..]);
        assert_eq!(head.value, &payload(token, salt ^ 0xffff)[6..]);
    }
    assert_eq!(pool.read(seq, len).unwrap_err(), KvError::Position);
    Ok(())
}
#[test]
fn real_bf16_payloads_span_pages_and_independent_sequences(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut pool = KvPool::new(config(6, 2))?;
    let mut a = pool.begin()?;
    let mut b = pool.begin()?;
    for i in 0..33 {
        append(&mut pool, &mut a, i, 0x7f80)?;
    }
    for i in 0..17 {
        append(&mut pool, &mut b, i, 0xff80)?;
    }
    verify(&pool, &a, 33, 0x7f80)?;
    verify(&pool, &b, 17, 0xff80)?;
    assert_eq!(pool.stats().allocated_pages, 5);
    assert_eq!(pool.read_head(&a, 0, 2).unwrap_err(), KvError::Shape);
    pool.release(a)?;
    verify(&pool, &b, 17, 0xff80)?;
    pool.release(b)?;
    assert_eq!(pool.stats().active_sequences, 0);
    assert_eq!(pool.stats().free_pages, 6);
    Ok(())
}
#[test]
fn rollback_within_across_and_at_page_boundaries_then_reappend(
) -> Result<(), Box<dyn std::error::Error>> {
    for prefix in [0, 1, 15, 16, 17, 31, 32] {
        let mut pool = KvPool::new(config(4, 1))?;
        let mut seq = pool.begin()?;
        for i in 0..prefix {
            append(&mut pool, &mut seq, i, 0x3f80)?;
        }
        let checkpoint = pool.checkpoint(&seq)?;
        for i in prefix..49 {
            append(&mut pool, &mut seq, i, 0x3f80)?;
        }
        pool.rollback(&mut seq, &checkpoint)?;
        verify(&pool, &seq, prefix, 0x3f80)?;
        assert_eq!(pool.stats().allocated_pages, prefix.div_ceil(16));
        let stale = pool.rollback(&mut seq, &checkpoint);
        assert_eq!(stale, Err(KvError::StaleCheckpoint));
        for i in prefix..prefix + 2 {
            append(&mut pool, &mut seq, i, 0xbc00)?;
        }
        for i in prefix..prefix + 2 {
            assert_eq!(pool.read(&seq, i)?.key, payload(i, 0xbc00));
        }
        pool.release(seq)?;
    }
    Ok(())
}
#[test]
fn oom_shape_position_and_foreign_owner_leave_owners_and_bytes_intact(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut pool = KvPool::new(config(1, 2))?;
    let mut a = pool.begin()?;
    let mut b = pool.begin()?;
    for i in 0..16 {
        append(&mut pool, &mut a, i, 0x1234)?;
    }
    assert_eq!(
        append(&mut pool, &mut b, 0, 0x4321),
        Err(KvError::OutOfPages)
    );
    assert_eq!(pool.len(&b)?, 0);
    verify(&pool, &a, 16, 0x1234)?;
    let bytes = payload(0, 0);
    assert_eq!(
        pool.append(&mut a, usize::MAX, step(&bytes), step(&bytes)),
        Err(KvError::Position)
    );
    // Shape/length failures on a sequence with committed tokens precede mutation.
    let mut roomy = KvPool::new(config(2, 1))?;
    let mut seq = roomy.begin()?;
    append(&mut roomy, &mut seq, 0, 0x1234)?;
    for wrong in [
        Bf16Step {
            heads: 1,
            head_dim: 6,
            bytes: &bytes,
        },
        Bf16Step {
            heads: 2,
            head_dim: 3,
            bytes: &bytes[..10],
        },
    ] {
        assert_eq!(
            roomy.append(&mut seq, 1, wrong, step(&bytes)),
            Err(KvError::Shape)
        );
        assert_eq!(
            roomy.append(&mut seq, 1, step(&bytes), wrong),
            Err(KvError::Shape)
        );
    }
    verify(&roomy, &seq, 1, 0x1234)?;
    assert_eq!(
        pool.append(&mut seq, 1, step(&bytes), step(&bytes)),
        Err(KvError::ForeignOwner)
    );
    assert_eq!(pool.read(&seq, 0).unwrap_err(), KvError::ForeignOwner);
    let failure = pool.release(seq).unwrap_err();
    assert_eq!(failure.error, KvError::ForeignOwner);
    let seq = failure.sequence;
    verify(&roomy, &seq, 1, 0x1234)?;
    roomy.release(seq)?;
    pool.release(a)?;
    append(&mut pool, &mut b, 0, 0x4321)?;
    verify(&pool, &b, 1, 0x4321)?;
    pool.release(b)?;
    Ok(())
}
#[test]
fn cancellation_reuse_and_checkpoints_cannot_cross_sequences(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut pool = KvPool::new(config(2, 2))?;
    let mut old = pool.begin()?;
    append(&mut pool, &mut old, 0, 0xffff)?;
    let old_checkpoint = pool.checkpoint(&old)?;
    pool.release(old)?; // explicit cancellation
    let mut new = pool.begin()?;
    assert!(pool.is_empty(&new)?);
    assert_eq!(pool.read(&new, 0).unwrap_err(), KvError::Position);
    append(&mut pool, &mut new, 0, 0x0000)?;
    assert_eq!(
        pool.rollback(&mut new, &old_checkpoint),
        Err(KvError::ForeignOwner)
    );
    verify(&pool, &new, 1, 0x0000)?;
    let mut another = pool.begin()?;
    append(&mut pool, &mut another, 0, 0x8000)?;
    let foreign = pool.checkpoint(&another)?;
    assert_eq!(
        pool.rollback(&mut new, &foreign),
        Err(KvError::ForeignOwner)
    );
    verify(&pool, &new, 1, 0x0000)?;
    verify(&pool, &another, 1, 0x8000)?;
    assert_eq!(pool.begin().unwrap_err(), KvError::SequenceLimit);
    pool.release(new)?;
    pool.release(another)?;
    Ok(())
}
#[test]
fn config_products_limits_and_exact_bf16_lengths_are_checked() -> Result<(), KvError> {
    let base = config(2, 2);
    for bad in [
        KvConfig { heads: 0, ..base },
        KvConfig {
            head_dim: 0,
            ..base
        },
        KvConfig {
            heads: usize::MAX,
            head_dim: 2,
            ..base
        },
        KvConfig { pages: 0, ..base },
        KvConfig {
            max_bytes: 1,
            ..base
        },
        KvConfig {
            max_tokens_per_sequence: 33,
            ..base
        },
        KvConfig {
            max_sequences: 1025,
            ..base
        },
        KvConfig {
            head_dim: usize::MAX / 2,
            heads: 1,
            ..base
        },
    ] {
        assert!(KvPool::new(bad).is_err());
    }
    let mut pool = KvPool::new(KvConfig {
        max_tokens_per_sequence: 1,
        ..base
    })?;
    let mut seq = pool.begin()?;
    append(&mut pool, &mut seq, 0, 0x7fc1)?;
    assert_eq!(
        append(&mut pool, &mut seq, 1, 0x7fc1),
        Err(KvError::TokenLimit)
    );
    verify(&pool, &seq, 1, 0x7fc1)?;
    pool.release(seq).map_err(|e| e.error)?;
    Ok(())
}
