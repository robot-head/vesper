//! Self-checking durable-store lifecycle on the simulated crash device:
//! commit, crash with a transaction in flight and torn unsynced writes,
//! recover, and inspect the recovery report.
use vesper_quarry::device::{CrashChoice, SimDevice};
use vesper_quarry::durable::{Db, DbLimits};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let limits = DbLimits {
        pool_frames: 2,
        ..DbLimits::default()
    };
    let mut dev = SimDevice::new();
    let (mut db, _) = Db::open(&mut dev, limits)?;
    let mut t = db.begin()?;
    let ids = (0..24u8)
        .map(|i| db.insert(&mut t, &vec![i; 2000]))
        .collect::<Result<Vec<_>, _>>()?;
    db.commit(t)?;

    let mut loser = db.begin()?;
    for id in &ids[..8] {
        db.update(&mut loser, *id, b"never committed")?;
    }
    let mut winner = db.begin()?;
    let extra = db.insert(&mut winner, b"committed after the loser started")?;
    db.commit(winner)?; // syncs the log, including the loser's records
    std::mem::forget(loser); // the process dies here
    let stats = db.stats();
    drop(db);

    let mut medium = dev.crash(CrashChoice::Subset { seed: 42 });
    let (mut db, report) = Db::open(&mut medium, limits)?;
    let rows = db.scan()?;
    assert_eq!(rows.len(), 25);
    assert!(rows
        .iter()
        .any(|(id, v)| *id == extra && v == b"committed after the loser started"));
    for (i, id) in ids.iter().enumerate() {
        assert_eq!(db.get(None, *id)?, Some(vec![i as u8; 2000]));
    }
    println!("before crash: {stats:?}");
    println!("recovery: {report:?}");
    println!("ok: {} live records after recovery", rows.len());
    Ok(())
}
