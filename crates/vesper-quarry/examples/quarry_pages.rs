use vesper_quarry::{
    page::SlottedPage,
    pool::{BufferPool, PageImage, PoolError, PoolLimits},
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut page = SlottedPage::new(42, 0);
    let slot = page.insert(b"original record")?;
    let original = PageImage::clean(page);
    let mut pool = BufferPool::new(PoolLimits { frames: 1, pins: 4 })?;
    pool.install(&original)?;
    let reader = pool.fix_shared(42)?;
    assert_eq!(pool.view_shared(&reader)?.read(slot)?, b"original record");
    assert_eq!(pool.fix_exclusive(42).unwrap_err(), PoolError::Conflict);
    assert_eq!(pool.evict(42).unwrap_err(), PoolError::Pinned);
    pool.unpin_clean(reader)?;

    let mut writer = pool.fix_exclusive(42)?;
    pool.update(&mut writer, slot, b"aborted replacement")?;
    pool.unpin_aborted(writer)?;
    let reader = pool.fix_shared(42)?;
    assert_eq!(
        pool.view_shared(&reader)?.as_bytes(),
        original.page().as_bytes()
    );
    pool.unpin_clean(reader)?;

    let mut writer = pool.fix_exclusive(42)?;
    pool.update(&mut writer, slot, b"committed replacement")?;
    let extra = pool.insert(&mut writer, b"second slot")?;
    let failure = pool.unpin_dirty(writer, 0).unwrap_err();
    assert_eq!(failure.error, PoolError::InvalidLsn);
    assert_eq!(
        pool.view_exclusive(&failure.pin)?.read(extra)?,
        b"second slot"
    );
    pool.unpin_dirty(failure.pin, 9)?;
    assert_eq!(pool.pin_count(), 0);
    let image = pool.evict(42)?;
    assert_eq!(image.dirty_lsn(), Some(9));
    let checked = SlottedPage::decode(image.page().as_bytes())?;
    assert_eq!(checked.read(slot)?, b"committed replacement");
    assert_eq!(checked.read(extra)?, b"second slot");
    assert_eq!(checked.header().page_lsn, 9);
    pool.install(&image)?;
    assert_eq!(pool.info(42)?.generation, 2);
    assert_eq!(pool.info(42)?.dirty_lsn, Some(9));
    println!("Quarry pages: CRC32C-checked 16384-byte page; 2 slots; dirty LSN 9; abort, recoverable release, and eviction/reinstall checks passed");
    Ok(())
}
