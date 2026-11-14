//! Tiled<T, N>: AoSoA layout, column iteration and a differential model.
use vesper_alloc::{Tiled, TiledConfig, TiledRecord, TiledSeqError};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Entry {
    base_offset: u64,
    batch_crc: u32,
    byte_len: u32,
    producer_id: u64,
    codec: u8,
}

impl TiledRecord for Entry {
    type Hot = (u64, u32, u32);
    type Cold = (u64, u8);
    fn split(self) -> ((u64, u32, u32), (u64, u8)) {
        (
            (self.base_offset, self.batch_crc, self.byte_len),
            (self.producer_id, self.codec),
        )
    }
    fn join(
        (base_offset, batch_crc, byte_len): (u64, u32, u32),
        (producer_id, codec): (u64, u8),
    ) -> Self {
        Self {
            base_offset,
            batch_crc,
            byte_len,
            producer_id,
            codec,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AllHot(u8, u16, u32, u64, i8, i64);
impl TiledRecord for AllHot {
    type Hot = (u8, u16, u32, u64, i8, i64);
    type Cold = ();
    fn split(self) -> (Self::Hot, ()) {
        ((self.0, self.1, self.2, self.3, self.4, self.5), ())
    }
    fn join(h: Self::Hot, (): ()) -> Self {
        AllHot(h.0, h.1, h.2, h.3, h.4, h.5)
    }
}

fn entry(i: u64) -> Entry {
    Entry {
        base_offset: i * 1000,
        batch_crc: (i * 2654435761 % 7) as u32,
        byte_len: i as u32,
        producer_id: i ^ 0x55,
        codec: (i % 4) as u8,
    }
}

#[test]
fn every_column_of_every_tile_starts_on_a_cache_line() {
    let mut t = Tiled::<Entry, 16>::new();
    for i in 0..50 {
        t.push(entry(i)).unwrap();
    }
    assert_eq!(t.tile_count(), 4);
    for tile in t.tiles() {
        let raw = tile.raw();
        for addr in [raw.0.addr(), raw.1.addr(), raw.2.addr()] {
            assert_eq!(addr % 64, 0);
        }
        // Columns are separate arrays: the CRC column is 16 contiguous u32.
        assert_eq!(raw.1.addr() - raw.0.addr(), 16 * 8);
    }
    let lens: Vec<_> = t.tiles().map(|x| x.len()).collect();
    assert_eq!(lens, [16, 16, 16, 2]);
}

#[test]
fn column_scans_see_only_initialized_lanes() {
    let mut t = Tiled::<Entry, 8>::new();
    for i in 0..21 {
        t.push(entry(i)).unwrap();
    }
    let crc: Vec<u32> = t.column(|x| &x.1).flatten().copied().collect();
    let expected: Vec<u32> = (0..21).map(|i| entry(i).batch_crc).collect();
    assert_eq!(crc, expected);
    let per_tile: Vec<usize> = t.column(|x| &x.2).map(<[u32]>::len).collect();
    assert_eq!(per_tile, [8, 8, 5]);
    assert_eq!(
        t.tile(2).unwrap().col(|x| &x.0),
        &[16_000, 17_000, 18_000, 19_000, 20_000]
    );
    assert!(t.tile(3).is_none());
}

#[test]
fn disjoint_columns_are_mutated_together() {
    let mut t = Tiled::<Entry, 32>::new();
    for i in 0..40 {
        t.push(entry(i)).unwrap();
    }
    for tile in t.tiles_mut() {
        let (offsets, lens) = tile.cols2_mut(|x| (&mut x.0, &mut x.2));
        for (o, l) in offsets.iter_mut().zip(lens.iter_mut()) {
            *o += 1;
            *l *= 2;
        }
    }
    let last = t.tile_mut(1).unwrap().col_mut(|x| &mut x.1);
    assert_eq!(last.len(), 8);
    last.fill(0);
    for i in 0..40 {
        let e = t.get(i).unwrap();
        let want = entry(i as u64);
        assert_eq!(e.base_offset, want.base_offset + 1);
        assert_eq!(e.byte_len, want.byte_len * 2);
        assert_eq!(e.batch_crc, if i >= 32 { 0 } else { want.batch_crc });
        assert_eq!(
            *t.cold(i).unwrap(),
            (want.producer_id, want.codec),
            "cold fields untouched"
        );
    }
}

/// Small xorshift generator; deterministic across hosts.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

fn differential<const N: usize>(seed: u64) {
    let mut rng = Rng(seed);
    let mut model: Vec<Entry> = Vec::new();
    let mut t = Tiled::<Entry, N>::new();
    for step in 0..3000 {
        match rng.next() % 10 {
            0..=4 => {
                let e = entry(rng.next() % 1_000_000);
                t.push(e).unwrap();
                model.push(e);
            }
            5 => assert_eq!(t.pop(), model.pop()),
            6 if !model.is_empty() => {
                let i = (rng.next() as usize) % model.len();
                let e = entry(step);
                t.set(i, e).unwrap();
                model[i] = e;
            }
            7 => {
                let n = (rng.next() as usize) % (model.len() + 3);
                t.truncate(n);
                model.truncate(n);
            }
            8 if !model.is_empty() => {
                let i = (rng.next() as usize) % model.len();
                t.cold_mut(i).unwrap().1 = 9;
                model[i].codec = 9;
            }
            _ => {
                let i = rng.next() as usize % (model.len() + 2);
                assert_eq!(t.get(i), model.get(i).copied());
            }
        }
        assert_eq!(t.len(), model.len());
        assert_eq!(t.tile_count(), model.len().div_ceil(N));
    }
    assert!(t.iter().eq(model.iter().copied()));
    let crc: Vec<u32> = t.column(|x| &x.1).flatten().copied().collect();
    assert!(crc.iter().eq(model.iter().map(|e| &e.batch_crc)));
    assert_eq!(t.cold_slice().len(), model.len());
    assert_eq!(
        t.set(model.len(), entry(0)),
        Err(TiledSeqError::OutOfBounds {
            index: model.len(),
            len: model.len()
        })
    );
}

#[test]
fn behaves_like_an_array_for_every_width() {
    differential::<8>(1);
    differential::<16>(2);
    differential::<32>(3);
    differential::<64>(4);
}

#[test]
fn six_hot_columns_and_no_cold_side_array() {
    let mut t = Tiled::<AllHot, 8>::with_capacity(20).unwrap();
    assert!(t.capacity() >= 20);
    for i in 0..20u8 {
        t.push(AllHot(i, i as u16 * 2, 3, 4, -(i as i8), -5))
            .unwrap();
    }
    assert_eq!(t.get(19), Some(AllHot(19, 38, 3, 4, -19, -5)));
    let sum: i64 = t.column(|x| &x.5).flatten().sum();
    assert_eq!(sum, -100);
    assert_eq!(t.hot(0), Some((0, 0, 3, 4, 0, -5)));
    t.clear();
    assert!(t.is_empty() && t.tile_count() == 0);
}

#[test]
fn length_limit_returns_the_record() {
    let mut t = Tiled::<Entry, 8>::with_config(TiledConfig { max_len: 3 });
    for i in 0..3 {
        t.push(entry(i)).unwrap();
    }
    let err = t.push(entry(9)).unwrap_err();
    assert_eq!(err.value, entry(9));
    assert_eq!(err.error, TiledSeqError::LengthLimit { limit: 3 });
    assert!(matches!(
        t.reserve(usize::MAX),
        Err(TiledSeqError::SizeOverflow)
    ));
    let clone = t.clone();
    assert!(clone.iter().eq(t.iter()));
    assert_eq!(format!("{:?}", Tiled::<Entry, 8>::default()), "[]");
}
