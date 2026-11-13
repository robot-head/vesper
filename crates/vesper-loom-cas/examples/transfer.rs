//! cargo run -p vesper-loom-cas --example transfer
//!
//! Transport-neutral CLI example; imported content is not trusted or executed.
use std::{
    error::Error,
    fs,
    io::{self, Read, Write},
    path::Path,
};
use vesper_loom_cas::{
    CopyStrategy, Domain, MaterializeOptions, ObjectId, Store, Tree,
    transfer::{TransferLimits, export_pack, import_pack},
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

fn read_pack(mut input: impl Read, maximum: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 8192];
    loop {
        // One probe byte proves that an oversized input cannot be mistaken for
        // a complete capped pack. Caller-owned input storage is a separate cap.
        let remaining = maximum.checked_sub(bytes.len()).ok_or("input byte limit")?;
        let available = chunk.len().min(remaining.saturating_add(1));
        let count = input.read(&mut chunk[..available])?;
        if count == 0 {
            return Ok(bytes);
        }
        if count > remaining {
            return Err("pack exceeds the example input byte limit".into());
        }
        bytes.try_reserve_exact(count)?;
        bytes.extend_from_slice(&chunk[..count]);
    }
}

fn demonstration() -> Result<()> {
    let scratch = tempfile::tempdir()?;
    let source = Store::open(scratch.path().join("source"))?;
    let bytes = b"immutable source bytes\n";
    let blob = source.put_blob(Domain::Source, bytes)?;
    let mut tree = Tree::new(Domain::Source);
    tree.insert_file("hello.txt", blob.id, blob.size, false)
        .map_err(vesper_loom_cas::CasError::InvalidTree)?;
    let root = source.put_tree(&tree)?.id;
    let pack = export_pack(&source, root, TransferLimits::default())?;
    let destination = scratch.path().join("destination");
    let report = import_pack(&destination, root, &pack, TransferLimits::default())?;
    assert_eq!(report.root, root);
    assert_eq!(report.objects, 2);
    assert_eq!(report.inserted, 2);
    assert_eq!(report.reused, 0);
    let destination = Store::open(destination)?;
    assert_eq!(destination.read_blob(&blob.id)?, bytes);
    assert_eq!(
        export_pack(&destination, root, TransferLimits::default())?,
        pack
    );
    let workspace = scratch.path().join("workspace");
    destination.materialize(
        &root,
        &workspace,
        MaterializeOptions {
            strategy: CopyStrategy::CopyOnly,
            verify_reflinks: true,
        },
    )?;
    assert_eq!(fs::read(workspace.join("hello.txt"))?, bytes);
    println!(
        "Verified {}-byte pack: {} immutable objects; expected root {root}",
        report.pack_bytes, report.objects
    );
    println!(
        "Content identity only: no derivation, trust, proof or execution approval transferred."
    );
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.is_empty() {
        return demonstration();
    }
    if args.len() == 1 && args[0] == "--help" {
        println!("transfer export STORE EXPECTED_ROOT > closure.pack");
        println!("transfer import DESTINATION EXPECTED_ROOT < closure.pack");
        println!("No arguments runs a disposable verified round trip.");
        return Ok(());
    }
    if args.len() != 3 {
        return Err(
            "expected export/import, explicit store path, and caller-known root; see --help".into(),
        );
    }
    let root: ObjectId = args[2].to_str().ok_or("root ID is not UTF8")?.parse()?;
    let path = Path::new(&args[1]);
    if args[0] == "export" {
        if !path.is_dir() {
            return Err("export requires an existing CAS directory".into());
        }
        let store = Store::open(path)?;
        let pack = export_pack(&store, root, TransferLimits::default())?;
        io::stdout().lock().write_all(&pack)?;
    } else if args[0] == "import" {
        let limits = TransferLimits::default();
        let pack = read_pack(io::stdin().lock(), limits.max_pack_bytes)?;
        let report = import_pack(path, root, &pack, limits)?;
        eprintln!(
            "Verified {} objects: {} inserted, {} reused; root {}",
            report.objects, report.inserted, report.reused, report.root
        );
    } else {
        return Err("unknown transfer action; see --help".into());
    }
    Ok(())
}
