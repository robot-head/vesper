//! Run with `cargo run --locked -p vesper-alloc --example tiled_columns`.
use vesper_alloc::{Region, RegionConfig};

fn scalar_row(index: u32) -> (u32, i64) {
    (index * 7 + 3, 1000 - i64::from(index) * 13)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    const WIDTH: usize = 16;
    const ROWS: u32 = 73;
    let mut region = Region::new(RegionConfig::bounded(128 * 1024))?;
    let mut columns = region.tiled2_with_capacity::<u32, i64, WIDTH>(ROWS as usize)?;
    for row in 0..ROWS {
        let (first, second) = scalar_row(row);
        region.tiled2_push(&mut columns, first, second)?;
    }

    let mut observed = (0u64, 0i64);
    for tile in 0..region.tiled2_tile_count(&columns)? {
        let (first, second) = region.tiled2_columns(&columns, tile)?.unwrap();
        observed.0 += first.iter().map(|&value| u64::from(value)).sum::<u64>();
        observed.1 += second.iter().sum::<i64>();
    }
    let expected = (0..ROWS).map(scalar_row).fold((0u64, 0i64), |sum, row| {
        (sum.0 + u64::from(row.0), sum.1 + row.1)
    });
    assert_eq!(observed, expected);

    // Both projections come from one mutable tile borrow. Only initialized
    // lanes are visible, including the nine-row final tile.
    for tile in 0..region.tiled2_tile_count(&columns)? {
        let (first, second) = region.tiled2_columns_mut(&columns, tile)?.unwrap();
        for (a, b) in first.iter_mut().zip(second) {
            *a += 5;
            *b = -*b + i64::from(*a);
        }
    }
    for row in 0..ROWS {
        let (a, b) = scalar_row(row);
        assert_eq!(
            region.tiled2_get(&columns, row as usize)?,
            Some((a + 5, -b + i64::from(a + 5)))
        );
    }
    assert_eq!(region.tiled2_len(&columns)?, ROWS as usize);
    println!(
        "verified {ROWS} logical rows in {} actual column tiles; initial sums {}, {}",
        region.tiled2_tile_count(&columns)?,
        observed.0,
        observed.1
    );
    region.tiled2_release(columns)?;
    Ok(())
}
