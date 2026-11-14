//! Actual scalar flat-table operations compared with an independent ordered map.
use std::collections::BTreeMap;
use vesper_alloc::{FlatConfig, HandleError, Region, RegionConfig, RegionFlatTable};

fn compare(region: &Region, table: &RegionFlatTable<u64, i64>, model: &BTreeMap<u64, i64>) {
    let actual: BTreeMap<_, _> = region
        .flat_iter(table)
        .unwrap()
        .map(|(key, value)| (*key, *value))
        .collect();
    assert_eq!(&actual, model);
    assert_eq!(region.flat_len(table), Ok(model.len()));
    let iter = region.flat_iter(table).unwrap();
    assert_eq!(iter.len(), model.len());
}

fn main() {
    let mut region = Region::new(RegionConfig::bounded(4 * 1024 * 1024)).unwrap();
    let mut table = region
        .flat_new::<u64, i64>(FlatConfig { max_groups: 16 })
        .unwrap();
    let mut model = BTreeMap::new();
    for key in 0..96 {
        assert!(
            region
                .flat_insert(&mut table, key, key as i64 * 3)
                .unwrap()
                .is_none()
        );
        model.insert(key, key as i64 * 3);
    }
    compare(&region, &table, &model);
    let previous = region.flat_insert(&mut table, 7, 700).unwrap();
    assert_eq!(previous, model.insert(7, 700).map(|value| (7, value)));
    {
        let [a, b] = region.flat_get_disjoint_mut(&table, [&3, &5]).unwrap();
        *a.unwrap() += 1000;
        *b.unwrap() -= 7;
    }
    *model.get_mut(&3).unwrap() += 1000;
    *model.get_mut(&5).unwrap() -= 7;
    assert_eq!(
        region.flat_get_disjoint_mut(&table, [&3, &3]).unwrap_err(),
        HandleError::Aliased
    );
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    for step in 0..512 {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let key = state % 96;
        match step % 3 {
            0 => {
                assert_eq!(
                    region.flat_insert(&mut table, key, step).unwrap(),
                    model.insert(key, step).map(|value| (key, value))
                );
            }
            1 => {
                assert_eq!(
                    region.flat_remove(&table, &key).unwrap(),
                    model.remove(&key).map(|value| (key, value))
                );
            }
            _ => {
                assert_eq!(
                    region.flat_get(&table, &key).unwrap().copied(),
                    model.get(&key).copied()
                );
            }
        }
        compare(&region, &table, &model);
    }
    let groups = region.flat_group_count(&table).unwrap();
    for group in 0..groups {
        assert_eq!(
            region
                .flat_control_group(&table, group)
                .unwrap()
                .unwrap()
                .len(),
            16
        );
    }
    println!(
        "flat table verified: rows={}, groups={}, control_bytes={}",
        model.len(),
        groups,
        groups * 16
    );
    region.flat_release(table).unwrap();
}
