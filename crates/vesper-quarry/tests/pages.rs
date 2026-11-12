use vesper_quarry::page::{
    page_crc32c, PageError, SlottedPage, HEADER_SIZE, MAX_RECORD_SIZE, MAX_SLOTS, PAGE_SIZE,
    SLOT_SIZE,
};
use vesper_quarry::pool::{BufferPool, PageImage, PoolError, PoolLimits};

fn pool(frames: usize, pins: usize) -> BufferPool {
    BufferPool::new(PoolLimits { frames, pins }).unwrap()
}
fn image(id: u64) -> PageImage {
    let mut page = SlottedPage::new(id, 7);
    page.insert(b"committed record").unwrap();
    PageImage::clean(page)
}
fn resign(bytes: &mut [u8]) {
    let crc = page_crc32c(bytes).unwrap();
    bytes[16..20].copy_from_slice(&crc.to_le_bytes());
}
fn u16_at(bytes: &mut [u8], offset: usize, value: usize) {
    bytes[offset..offset + 2].copy_from_slice(&(value as u16).to_le_bytes());
}

#[test]
fn exact_little_endian_header_and_actual_record_roundtrip() {
    let mut page = SlottedPage::new(0x0807_0605_0403_0201, 0x0c0b_0a09);
    let bytes = page.as_bytes();
    assert_eq!(bytes.len(), 16_384);
    assert_eq!(&bytes[0..8], &[0; 8]);
    assert_eq!(&bytes[8..16], &[1, 2, 3, 4, 5, 6, 7, 8]);
    assert_eq!(&bytes[20..24], &[0, 0, 0, 0x40]);
    assert_eq!(&bytes[24..28], &[1, 0, 0, 0]);
    assert_eq!(&bytes[28..32], &[9, 10, 11, 12]);
    assert_eq!(page.free_bytes(), PAGE_SIZE - HEADER_SIZE);
    let a = page.insert(b"hello").unwrap();
    let b = page.insert(&[0, 255, 0, 42]).unwrap();
    assert_eq!(a, 0);
    assert_eq!(b, 1);
    assert_eq!(page.read(a).unwrap(), b"hello");
    assert_eq!(page.read(b).unwrap(), [0, 255, 0, 42]);
    let old = page.clone();
    page.update(a, b"a much longer replacement").unwrap();
    assert_eq!(page.read(a).unwrap(), b"a much longer replacement");
    assert_eq!(page.read(b).unwrap(), old.read(b).unwrap());
    page.update(a, b"x").unwrap();
    let decoded = SlottedPage::decode(page.as_bytes()).unwrap();
    assert_eq!(decoded, page);
    assert_eq!(decoded.read(a).unwrap(), b"x");
    assert_eq!(
        page_crc32c(page.as_bytes()).unwrap(),
        page.header().checksum_crc32c
    );
    assert_eq!(page.header().right_sibling_page, 0x0c0b_0a09);
}
#[test]
fn corruption_and_wrong_sizes_are_rejected_before_trusting_offsets() {
    let page = image(3);
    let original = page.page().as_bytes();
    for offset in [0, 8, 16, 20, 22, 24, 28, 32, PAGE_SIZE / 2, PAGE_SIZE - 1] {
        let mut bytes = *original;
        bytes[offset] ^= 1;
        assert_eq!(
            SlottedPage::decode(&bytes).unwrap_err(),
            PageError::Checksum,
            "byte {offset}"
        );
    }
    assert_eq!(
        SlottedPage::decode(&original[..PAGE_SIZE - 1]).unwrap_err(),
        PageError::Size
    );
    assert_eq!(
        SlottedPage::decode(&vec![0; PAGE_SIZE + 1]).unwrap_err(),
        PageError::Size
    );
    assert_eq!(page_crc32c(&[]).unwrap_err(), PageError::Size);
    assert_eq!(page.page().as_bytes(), original);
}
#[test]
fn malicious_crc_valid_slot_layouts_fail_closed() {
    let mut page = SlottedPage::new(1, 0);
    page.insert(b"alpha").unwrap();
    page.insert(b"beta").unwrap();
    let original = *page.as_bytes();
    // Invalid count, directory/free overlap, free beyond page, slot into header,
    // zero-size record, end beyond page, pair overlap, and unexplained free edge.
    for (offset, value) in [
        (20, MAX_SLOTS + 1),
        (22, HEADER_SIZE),
        (22, PAGE_SIZE + 1),
        (32, HEADER_SIZE),
        (34, 0),
        (32, PAGE_SIZE - 1),
        (34, 100),
        (36, PAGE_SIZE - 5),
        (22, PAGE_SIZE - 10),
    ] {
        let mut bytes = original;
        u16_at(&mut bytes, offset, value);
        resign(&mut bytes);
        assert_eq!(
            SlottedPage::decode(&bytes).unwrap_err(),
            PageError::SlotLayout,
            "offset {offset},value {value}"
        );
    }
    for offset in [24, 25, 26] {
        let mut bytes = original;
        bytes[offset] = 2;
        resign(&mut bytes);
        assert_eq!(
            SlottedPage::decode(&bytes).unwrap_err(),
            PageError::UnsupportedHeader
        );
    }
    let mut empty = *SlottedPage::new(1, 0).as_bytes();
    u16_at(&mut empty, 22, PAGE_SIZE - 1);
    resign(&mut empty);
    assert_eq!(
        SlottedPage::decode(&empty).unwrap_err(),
        PageError::SlotLayout
    );
}
#[test]
fn full_pages_and_rejected_updates_preserve_exact_bytes() {
    let mut page = SlottedPage::new(2, 0);
    let input = vec![17; MAX_RECORD_SIZE];
    page.insert(&input).unwrap();
    assert_eq!(page.free_bytes(), 0);
    let before = page.clone();
    assert_eq!(page.insert(b"x").unwrap_err(), PageError::Full);
    assert_eq!(page, before);
    assert_eq!(
        page.update(1, b"x").unwrap_err(),
        PageError::SlotOutOfBounds
    );
    assert_eq!(page, before);
    assert_eq!(page.update(0, &[]).unwrap_err(), PageError::RecordSize);
    assert_eq!(page, before);
    assert_eq!(
        page.update(0, &vec![0; MAX_RECORD_SIZE + 1]).unwrap_err(),
        PageError::RecordSize
    );
    assert_eq!(page, before);
    page.update(0, b"small").unwrap();
    assert!(page.free_bytes() > 16_000);
    page.insert(b"reclaimed capacity").unwrap();
    let mut page = SlottedPage::new(2, 0);
    page.insert(&vec![1; 8000]).unwrap();
    page.insert(&vec![2; 8000]).unwrap();
    let before = page.clone();
    assert_eq!(page.update(0, &vec![3; 9000]).unwrap_err(), PageError::Full);
    assert_eq!(page, before);
    assert_eq!(input, vec![17; MAX_RECORD_SIZE]);
}
#[test]
fn maximum_slot_directory_is_bounded_and_checked() {
    let mut bytes = *SlottedPage::new(9, 0).as_bytes();
    u16_at(&mut bytes, 20, MAX_SLOTS);
    u16_at(&mut bytes, 22, PAGE_SIZE - MAX_SLOTS);
    for i in 0..MAX_SLOTS {
        u16_at(&mut bytes, HEADER_SIZE + i * SLOT_SIZE, PAGE_SIZE - i - 1);
        u16_at(&mut bytes, HEADER_SIZE + i * SLOT_SIZE + 2, 1);
        bytes[PAGE_SIZE - i - 1] = (i % 256) as u8;
    }
    resign(&mut bytes);
    let mut page = SlottedPage::decode(&bytes).unwrap();
    assert_eq!(page.slot_count(), MAX_SLOTS);
    assert_eq!(
        page.read((MAX_SLOTS - 1) as u16).unwrap(),
        [((MAX_SLOTS - 1) % 256) as u8]
    );
    let before = page.clone();
    assert_eq!(page.insert(b"x").unwrap_err(), PageError::Full);
    assert_eq!(page, before);
}
#[test]
fn shared_exclusive_conflicts_capacity_and_pinned_eviction() {
    let mut p = pool(1, 2);
    let original = image(1);
    p.install(&original).unwrap();
    assert_eq!(p.install(&original).unwrap_err(), PoolError::DuplicatePage);
    let other = image(2);
    assert_eq!(p.install(&other).unwrap_err(), PoolError::Full);
    assert_eq!(other, image(2));
    let a = p.fix_shared(1).unwrap();
    let b = p.fix_shared(1).unwrap();
    assert_eq!(p.pin_count(), 2);
    assert_eq!(p.fix_shared(1).unwrap_err(), PoolError::Limit);
    assert_eq!(p.fix_exclusive(1).unwrap_err(), PoolError::Conflict);
    assert_eq!(p.evict(1).unwrap_err(), PoolError::Pinned);
    assert_eq!(
        p.view_shared(&a).unwrap().read(0).unwrap(),
        b"committed record"
    );
    p.unpin_clean(a).unwrap();
    p.unpin_clean(b).unwrap();
    assert_eq!(p.pin_count(), 0);
    let mut writer = p.fix_exclusive(1).unwrap();
    assert_eq!(p.fix_shared(1).unwrap_err(), PoolError::Conflict);
    assert_eq!(p.fix_exclusive(1).unwrap_err(), PoolError::Conflict);
    p.update(&mut writer, 0, b"temporary").unwrap();
    p.unpin_aborted(writer).unwrap();
    let reader = p.fix_shared(1).unwrap();
    assert_eq!(
        p.view_shared(&reader).unwrap().as_bytes(),
        original.page().as_bytes()
    );
    p.unpin_clean(reader).unwrap();
    assert_eq!(p.evict(1).unwrap(), original);
    assert_eq!(p.resident_count(), 0);
    p.install(&other).unwrap();
    assert_eq!(p.info(2).unwrap().generation, 2);
    assert_eq!(p.fix_shared(1).unwrap_err(), PoolError::UnknownPage);
}
#[test]
fn dirty_commit_advances_lsn_and_abort_restores_previous_dirty_image() {
    let mut p = pool(1, 4);
    p.install(&image(1)).unwrap();
    let mut w = p.fix_exclusive(1).unwrap();
    p.update(&mut w, 0, b"first commit").unwrap();
    let rejected = p.unpin_dirty(w, 0).unwrap_err();
    assert_eq!(rejected.error, PoolError::InvalidLsn);
    assert_eq!(p.pin_count(), 1);
    assert_eq!(
        p.view_exclusive(&rejected.pin).unwrap().read(0).unwrap(),
        b"first commit"
    );
    p.unpin_dirty(rejected.pin, 10).unwrap();
    let info = p.info(1).unwrap();
    assert_eq!(info.page_lsn, 10);
    assert_eq!(info.dirty_lsn, Some(10));
    assert_eq!(p.pin_count(), 0);
    let mut w = p.fix_exclusive(1).unwrap();
    p.update(&mut w, 0, b"second commit").unwrap();
    let rejected = p.unpin_dirty(w, 10).unwrap_err();
    assert_eq!(rejected.error, PoolError::InvalidLsn);
    let rejected = p.unpin_dirty(rejected.pin, 9).unwrap_err();
    assert_eq!(rejected.error, PoolError::InvalidLsn);
    p.unpin_dirty(rejected.pin, 20).unwrap();
    assert_eq!(p.info(1).unwrap().page_lsn, 20);
    assert_eq!(p.info(1).unwrap().dirty_lsn, Some(10));
    let r = p.fix_shared(1).unwrap();
    let committed = p.view_shared(&r).unwrap().clone();
    p.unpin_clean(r).unwrap();
    assert_eq!(p.info(1).unwrap().dirty_lsn, Some(10));
    let mut w = p.fix_exclusive(1).unwrap();
    p.insert(&mut w, b"aborted extra slot").unwrap();
    p.update(&mut w, 0, b"aborted edit").unwrap();
    p.unpin_aborted(w).unwrap();
    let r = p.fix_shared(1).unwrap();
    assert_eq!(p.view_shared(&r).unwrap(), &committed);
    p.unpin_clean(r).unwrap();
    assert_eq!(p.info(1).unwrap().dirty_lsn, Some(10));
    let evicted = p.evict(1).unwrap();
    assert_eq!(evicted.dirty_lsn(), Some(10));
    assert_eq!(evicted.page().header().page_lsn, 20);
    SlottedPage::decode(evicted.page().as_bytes()).unwrap();
    p.install(&evicted).unwrap();
    assert_eq!(p.info(1).unwrap().dirty_lsn, Some(10));
    let w = p.fix_exclusive(1).unwrap();
    let rejected = p.unpin_dirty(w, 21).unwrap_err();
    assert_eq!(rejected.error, PoolError::Unmodified);
    p.unpin_aborted(rejected.pin).unwrap();
}
#[test]
fn wrong_pool_failures_return_unchanged_live_owners() {
    let mut a = pool(1, 4);
    let mut b = pool(1, 4);
    a.install(&image(1)).unwrap();
    b.install(&image(1)).unwrap();
    let reader = a.fix_shared(1).unwrap();
    assert_eq!(b.view_shared(&reader).unwrap_err(), PoolError::WrongPool);
    let failed = b.unpin_clean(reader).unwrap_err();
    assert_eq!(failed.error, PoolError::WrongPool);
    assert_eq!(a.pin_count(), 1);
    assert_eq!(b.pin_count(), 0);
    a.unpin_clean(failed.pin).unwrap();
    let mut writer = a.fix_exclusive(1).unwrap();
    a.update(&mut writer, 0, b"staged owner survives").unwrap();
    assert_eq!(
        b.update(&mut writer, 0, b"wrong pool must not edit")
            .unwrap_err(),
        PoolError::WrongPool
    );
    let failed = b.unpin_dirty(writer, 10).unwrap_err();
    assert_eq!(failed.error, PoolError::WrongPool);
    assert_eq!(
        a.view_exclusive(&failed.pin).unwrap().read(0).unwrap(),
        b"staged owner survives"
    );
    let failed = b.unpin_aborted(failed.pin).unwrap_err();
    assert_eq!(failed.error, PoolError::WrongPool);
    a.unpin_dirty(failed.pin, 10).unwrap();
    assert_eq!(b.evict(1).unwrap(), image(1));
    assert_eq!(
        a.evict(1).unwrap().page().read(0).unwrap(),
        b"staged owner survives"
    );
}
#[test]
fn failed_staged_operations_preserve_data_and_abort_remains_available() {
    let mut p = pool(1, 1);
    let original = image(1);
    p.install(&original).unwrap();
    let mut w = p.fix_exclusive(1).unwrap();
    p.insert(&mut w, b"live stage").unwrap();
    let before = p.view_exclusive(&w).unwrap().clone();
    assert_eq!(
        p.update(&mut w, 10, b"bad index").unwrap_err(),
        PoolError::Page(PageError::SlotOutOfBounds)
    );
    assert_eq!(p.view_exclusive(&w).unwrap(), &before);
    assert_eq!(
        p.insert(&mut w, &vec![1; MAX_RECORD_SIZE]).unwrap_err(),
        PoolError::Page(PageError::Full)
    );
    assert_eq!(p.view_exclusive(&w).unwrap(), &before);
    assert_eq!(p.fix_shared(1).unwrap_err(), PoolError::Conflict);
    p.unpin_aborted(w).unwrap();
    assert_eq!(p.evict(1).unwrap(), original);
    for limits in [
        PoolLimits { frames: 0, pins: 1 },
        PoolLimits {
            frames: 257,
            pins: 1,
        },
        PoolLimits { frames: 1, pins: 0 },
        PoolLimits {
            frames: 1,
            pins: 4097,
        },
    ] {
        assert_eq!(BufferPool::new(limits).unwrap_err(), PoolError::Limit);
    }
}

#[test]
fn fragmented_valid_pages_are_compacted_without_changing_slot_identity() {
    let mut page = SlottedPage::new(4, 8);
    page.insert(b"abc").unwrap();
    page.insert(b"def").unwrap();
    let mut raw = *page.as_bytes();
    // Move slot 1 downward by 100 bytes, retaining a checked interior hole.
    let start = PAGE_SIZE - 6 - 100;
    raw[start..start + 3].copy_from_slice(b"def");
    u16_at(&mut raw, HEADER_SIZE + SLOT_SIZE, start);
    u16_at(&mut raw, 22, start);
    resign(&mut raw);
    let mut decoded = SlottedPage::decode(&raw).unwrap();
    assert_eq!(decoded.read(0).unwrap(), b"abc");
    assert_eq!(decoded.read(1).unwrap(), b"def");
    let fragmented_free = decoded.free_bytes();
    decoded.update(0, b"ab").unwrap();
    assert!(decoded.free_bytes() > fragmented_free + 100);
    assert_eq!(decoded.read(0).unwrap(), b"ab");
    assert_eq!(decoded.read(1).unwrap(), b"def");
    assert_eq!(decoded.header().right_sibling_page, 8);
    SlottedPage::decode(decoded.as_bytes()).unwrap();
}

#[test]
fn page_lsn_wire_bytes_are_little_endian_and_maximum_lsn_fails_future_advancement() {
    let mut pool = pool(1, 1);
    pool.install(&image(5)).unwrap();
    let mut w = pool.fix_exclusive(5).unwrap();
    pool.update(&mut w, 0, b"one").unwrap();
    pool.unpin_dirty(w, 0x0807_0605_0403_0201).unwrap();
    let r = pool.fix_shared(5).unwrap();
    assert_eq!(
        &pool.view_shared(&r).unwrap().as_bytes()[..8],
        &[1, 2, 3, 4, 5, 6, 7, 8]
    );
    pool.unpin_clean(r).unwrap();
    let mut w = pool.fix_exclusive(5).unwrap();
    pool.update(&mut w, 0, b"maximum").unwrap();
    pool.unpin_dirty(w, u64::MAX).unwrap();
    let mut w = pool.fix_exclusive(5).unwrap();
    pool.update(&mut w, 0, b"cannot commit").unwrap();
    let failure = pool.unpin_dirty(w, u64::MAX).unwrap_err();
    assert_eq!(failure.error, PoolError::InvalidLsn);
    pool.unpin_aborted(failure.pin).unwrap();
    assert_eq!(pool.evict(5).unwrap().page().read(0).unwrap(), b"maximum");
}
