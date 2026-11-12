//! Self-checking CPU BF16 storage, page rollback, reuse and cancellation.
use vesper_tensor::kv::*;
fn append(pool: &mut KvPool, seq: &mut Sequence, pos: usize, salt: u16) -> Result<(), KvError> {
    let bytes = salt.wrapping_add(pos as u16).to_le_bytes();
    let step = Bf16Step {
        heads: 1,
        head_dim: 1,
        bytes: &bytes,
    };
    pool.append(seq, pos, step, step)
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut pool = KvPool::new(KvConfig {
        heads: 1,
        head_dim: 1,
        pages: 3,
        max_sequences: 2,
        max_tokens_per_sequence: 48,
        max_bytes: 192,
    })?;
    let mut a = pool.begin()?;
    for i in 0..15 {
        append(&mut pool, &mut a, i, 0x3f80)?;
    }
    let mark = pool.checkpoint(&a)?;
    for i in 15..33 {
        append(&mut pool, &mut a, i, 0x3f80)?;
    }
    assert_eq!(pool.stats().allocated_pages, 3);
    pool.rollback(&mut a, &mark)?;
    assert_eq!(pool.len(&a)?, 15);
    assert_eq!(pool.stats().allocated_pages, 1);
    assert_eq!(pool.read(&a, 15).unwrap_err(), KvError::Position);
    let mut b = pool.begin()?;
    append(&mut pool, &mut b, 0, 0x7fc1)?;
    for i in 0..15 {
        assert_eq!(
            pool.read(&a, i)?.key,
            0x3f80u16.wrapping_add(i as u16).to_le_bytes()
        );
    }
    assert_eq!(pool.read(&b, 0)?.value, 0x7fc1u16.to_le_bytes());
    pool.release(a)?;
    pool.release(b)?;
    assert_eq!(pool.stats().free_pages, 3);
    assert_eq!(pool.stats().active_sequences, 0);
    println!("BF16 KV payloads survived page-boundary rollback and independent reuse; all 3 pages reclaimed");
    Ok(())
}
