//! Whole-store differential tests of the Vesper port (`vesper_loom_cas`)
//! against the frozen Rust seed copy (`vesper_loom_cas_rust`), verifying
//! exact agreement on names, identities, canonical codecs, on-disk store
//! operations, error reports, materialization and pack transfer (#1411).

use std::fs;
use std::path::Path;
use tempfile::TempDir;

use vesper_loom_cas as port;
use vesper_loom_cas_rust as rust;

fn to_rust_domain(d: port::Domain) -> rust::Domain {
    match d {
        port::Domain::Source => rust::Domain::Source,
        port::Domain::Interface => rust::Domain::Interface,
        port::Domain::Artifact => rust::Domain::Artifact,
    }
}

fn to_rust_kind(k: port::ObjectKind) -> rust::ObjectKind {
    match k {
        port::ObjectKind::Blob => rust::ObjectKind::Blob,
        port::ObjectKind::Tree => rust::ObjectKind::Tree,
    }
}

fn to_rust_id(id: port::ObjectId) -> rust::ObjectId {
    rust::ObjectId::new(
        to_rust_domain(id.domain),
        to_rust_kind(id.kind),
        rust::Digest(id.digest.0),
    )
}

fn collect_tree_files(dir: &Path, prefix: &str, out: &mut Vec<(String, Vec<u8>)>) {
    let mut entries: Vec<_> = fs::read_dir(dir).unwrap().map(|e| e.unwrap()).collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        let rel = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        let path = entry.path();
        if path.is_dir() {
            out.push((format!("{rel}/"), Vec::new()));
            collect_tree_files(&path, &rel, out);
        } else {
            out.push((rel, fs::read(&path).unwrap()));
        }
    }
}

fn corrupt_first_byte(path: &Path) {
    let mut perms = fs::metadata(path).unwrap().permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o644);
    }
    #[cfg(not(unix))]
    #[allow(clippy::permissions_set_readonly_false)]
    perms.set_readonly(false);
    fs::set_permissions(path, perms).unwrap();
    let mut data = fs::read(path).unwrap();
    if let Some(b) = data.first_mut() {
        *b ^= 0xff;
    } else {
        data.push(0xaa);
    }
    fs::write(path, data).unwrap();
}

#[test]
fn whole_store_port_matches_rust_seed() {
    assert_eq!(port::IMPLEMENTATION, "vesper");
    assert_eq!(rust::IMPLEMENTATION, "rust");

    let mut differences = 0usize;
    let mut cases = 0usize;

    // 1. Name validation across valid, invalid, Windows-reserved and UTF-8 names.
    let names = [
        "",
        ".",
        "..",
        "hello.txt",
        "lib.ves",
        "a/b",
        "a\\b",
        "foo:bar",
        "bad<name",
        "nul",
        "CON.txt",
        "com1",
        "LPT9.rs",
        "com0",
        "trailing.",
        "trailing ",
        "has\0nul",
        "ctrl\x07bell",
        "Straße",
        "STRASSE",
        "café.rs",
        "Σ",
        "ς",
        &"x".repeat(255),
        &"x".repeat(256),
    ];
    for name in names {
        cases += 1;
        let p = port::validate_name(name).map_err(|e| format!("{e}"));
        let r = rust::validate_name(name).map_err(|e| format!("{e}"));
        if p != r {
            eprintln!("validate_name diff for {name:?}: port={p:?} rust={r:?}");
            differences += 1;
        }
    }

    // 2. Blob IDs and ObjectId text formatting/parsing across domains and sizes.
    for domain in [
        port::Domain::Source,
        port::Domain::Interface,
        port::Domain::Artifact,
    ] {
        for len in [0usize, 1, 31, 63, 64, 65, 1024, 4096] {
            cases += 1;
            let payload: Vec<u8> = (0..len)
                .map(|i| ((i * 37 + domain as usize * 13) & 0xff) as u8)
                .collect();
            let pid = port::blob_id(domain, &payload);
            let rid = rust::blob_id(to_rust_domain(domain), &payload);
            if to_rust_id(pid) != rid || pid.to_string() != rid.to_string() {
                differences += 1;
            }
            let text = pid.to_string();
            cases += 1;
            let pp: Result<port::ObjectId, _> = text.parse();
            let rp: Result<rust::ObjectId, _> = text.parse();
            if pp.ok().map(to_rust_id) != rp.ok() {
                differences += 1;
            }
        }
    }
    for bad in [
        "",
        "b3v2:source:blob:00",
        "b3v1:unknown:blob:00",
        "b3v1:source:bad:00",
        &format!("b3v1:source:blob:{}", "A".repeat(64)),
        &format!("b3v1:source:blob:{}", "0".repeat(63)),
    ] {
        cases += 1;
        if bad.parse::<port::ObjectId>().is_ok() != bad.parse::<rust::ObjectId>().is_ok() {
            differences += 1;
        }
    }

    // 3. Tree encoding, decoding, and collision checks.
    for domain in [
        port::Domain::Source,
        port::Domain::Interface,
        port::Domain::Artifact,
    ] {
        cases += 1;
        let b1 = port::blob_id(domain, b"alpha");
        let b2 = port::blob_id(domain, b"beta\nline2");
        let rb1 = rust::blob_id(to_rust_domain(domain), b"alpha");
        let rb2 = rust::blob_id(to_rust_domain(domain), b"beta\nline2");

        let mut psub = port::Tree::new(domain);
        psub.insert_file("leaf.bin", b2, 10, true).unwrap();
        let mut rsub = rust::Tree::new(to_rust_domain(domain));
        rsub.insert_file("leaf.bin", rb2, 10, true).unwrap();

        let mut ptree = port::Tree::new(domain);
        ptree.insert_file("a.txt", b1, 5, false).unwrap();
        ptree.insert_dir("sub", psub.id()).unwrap();

        let mut rtree = rust::Tree::new(to_rust_domain(domain));
        rtree.insert_file("a.txt", rb1, 5, false).unwrap();
        rtree.insert_dir("sub", rsub.id()).unwrap();

        let penc = ptree.encode();
        let renc = rtree.encode();
        if penc != renc || to_rust_id(ptree.id()) != rtree.id() {
            differences += 1;
        }
        cases += 1;
        let pdec = port::Tree::decode(domain, &penc).unwrap();
        let rdec = rust::Tree::decode(to_rust_domain(domain), &renc).unwrap();
        if pdec.encode() != rdec.encode() || to_rust_id(pdec.id()) != rdec.id() {
            differences += 1;
        }
        cases += 1;
        if ptree.insert_file("A.TXT", b1, 5, false).is_ok()
            != rtree.insert_file("A.TXT", rb1, 5, false).is_ok()
        {
            differences += 1;
        }
        // Corrupt magic and trailing byte must fail identically.
        let mut bad = penc.clone();
        bad[0] ^= 1;
        cases += 1;
        if port::Tree::decode(domain, &bad).is_ok()
            != rust::Tree::decode(to_rust_domain(domain), &bad).is_ok()
        {
            differences += 1;
        }
        let mut trailing = penc.clone();
        trailing.push(0);
        cases += 1;
        if port::Tree::decode(domain, &trailing).is_ok()
            != rust::Tree::decode(to_rust_domain(domain), &trailing).is_ok()
        {
            differences += 1;
        }
    }

    // 4. Derivation codec & key checks.
    for target in [None, Some("x86_64-unknown-linux-gnu")] {
        cases += 1;
        let src_id = port::blob_id(port::Domain::Source, b"fn main() {}");
        let mut pi = port::DerivationInputs::new(port::Domain::Artifact, "vesper-cc", "1.0.0")
            .with_option("opt", "2")
            .with_option("arch", "native")
            .with_input("main", src_id);
        let mut ri = rust::DerivationInputs::new(rust::Domain::Artifact, "vesper-cc", "1.0.0")
            .with_option("opt", "2")
            .with_option("arch", "native")
            .with_input("main", to_rust_id(src_id));
        if let Some(t) = target {
            pi = pi.with_target(t);
            ri = ri.with_target(t);
        }
        let pe = pi.encode().unwrap();
        let re = ri.encode().unwrap();
        if pe != re || pi.key().unwrap().0.0 != ri.key().unwrap().0.0 {
            differences += 1;
        }
        cases += 1;
        if port::DerivationInputs::decode(&pe)
            .unwrap()
            .encode()
            .unwrap()
            != rust::DerivationInputs::decode(&re)
                .unwrap()
                .encode()
                .unwrap()
        {
            differences += 1;
        }
    }

    // 5. Whole-store end-to-end: put_blob, put_blob_reader, put_tree, ingest_dir,
    //    materialize, publish/lookup_derivation, repair, verify_all, export/import.
    let tmp = TempDir::new().unwrap();
    let ws = tmp.path().join("workspace");
    fs::create_dir_all(ws.join("pkg/sub")).unwrap();
    fs::write(ws.join("README.md"), b"# Vesper CAS\n").unwrap();
    fs::write(ws.join("pkg/mod.ves"), vec![b'a'; 8192]).unwrap();
    fs::write(
        ws.join("pkg/sub/data.bin"),
        (0..1024u32)
            .flat_map(|x| x.to_le_bytes())
            .collect::<Vec<_>>(),
    )
    .unwrap();

    let p_store = port::Store::open_unsynced(tmp.path().join("port_store")).unwrap();
    let r_store = rust::Store::open_unsynced(tmp.path().join("rust_store")).unwrap();

    cases += 1;
    let p_ing = p_store.ingest_dir(port::Domain::Source, &ws).unwrap();
    let r_ing = r_store.ingest_dir(rust::Domain::Source, &ws).unwrap();
    if to_rust_id(p_ing.id) != r_ing.id
        || p_ing.size != r_ing.size
        || p_ing.newly_stored != r_ing.newly_stored
    {
        differences += 1;
    }

    cases += 1;
    let p_dup = p_store.ingest_dir(port::Domain::Source, &ws).unwrap();
    let r_dup = r_store.ingest_dir(rust::Domain::Source, &ws).unwrap();
    if p_dup.newly_stored != r_dup.newly_stored || to_rust_id(p_dup.id) != r_dup.id {
        differences += 1;
    }

    // Materialize and compare every file/dir byte for byte.
    let p_mat = tmp.path().join("p_mat");
    let r_mat = tmp.path().join("r_mat");
    cases += 1;
    let pmr = p_store
        .materialize(&p_ing.id, &p_mat, port::MaterializeOptions::default())
        .unwrap();
    let rmr = r_store
        .materialize(&r_ing.id, &r_mat, rust::MaterializeOptions::default())
        .unwrap();
    if pmr.directories != rmr.directories || pmr.files != rmr.files || pmr.bytes != rmr.bytes {
        differences += 1;
    }
    let mut p_files = Vec::new();
    let mut r_files = Vec::new();
    collect_tree_files(&p_mat, "", &mut p_files);
    collect_tree_files(&r_mat, "", &mut r_files);
    cases += 1;
    if p_files != r_files {
        differences += 1;
    }

    // Put blobs (new + dedup + stream + readback) & build outer tree.
    let extra = vec![0x5a_u8; 70_000];
    cases += 1;
    let pb = p_store.put_blob(port::Domain::Artifact, &extra).unwrap();
    let rb = r_store.put_blob(rust::Domain::Artifact, &extra).unwrap();
    if to_rust_id(pb.id) != rb.id || pb.size != rb.size || pb.newly_stored != rb.newly_stored {
        differences += 1;
    }
    cases += 1;
    if p_store.read_blob(&pb.id).unwrap() != r_store.read_blob(&rb.id).unwrap() {
        differences += 1;
    }
    let p_stream = p_store
        .put_blob_reader(port::Domain::Interface, &b"iface-bytes"[..])
        .unwrap();
    let r_stream = r_store
        .put_blob_reader(rust::Domain::Interface, &b"iface-bytes"[..])
        .unwrap();
    cases += 1;
    if to_rust_id(p_stream.id) != r_stream.id {
        differences += 1;
    }

    // Derivations: miss -> publish -> duplicate -> conflict -> lookup hit.
    let p_inputs = port::DerivationInputs::new(port::Domain::Artifact, "vesperc", "0.1")
        .with_target("x86_64-pc-windows-msvc")
        .with_option("opt", "3")
        .with_input("src", p_ing.id);
    let r_inputs = rust::DerivationInputs::new(rust::Domain::Artifact, "vesperc", "0.1")
        .with_target("x86_64-pc-windows-msvc")
        .with_option("opt", "3")
        .with_input("src", r_ing.id);
    cases += 1;
    if p_store.lookup_derivation(&p_inputs).unwrap().is_some()
        || r_store.lookup_derivation(&r_inputs).unwrap().is_some()
    {
        differences += 1;
    }
    cases += 1;
    let pdo = p_store.publish_derivation(&p_inputs, pb.id).unwrap();
    let rdo = r_store.publish_derivation(&r_inputs, rb.id).unwrap();
    if pdo.key.0.0 != rdo.key.0.0
        || to_rust_id(pdo.output) != rdo.output
        || pdo.newly_recorded != rdo.newly_recorded
    {
        differences += 1;
    }
    cases += 1;
    let phit = p_store
        .lookup_derivation(&p_inputs)
        .unwrap()
        .map(to_rust_id);
    let rhit = r_store.lookup_derivation(&r_inputs).unwrap();
    if phit != rhit {
        differences += 1;
    }

    // Export pack byte parity + import pack parity.
    cases += 1;
    let p_pack = port::transfer::export_pack(
        &p_store,
        p_ing.id,
        port::transfer::TransferLimits::default(),
    )
    .unwrap();
    let r_pack = rust::transfer::export_pack(
        &r_store,
        r_ing.id,
        rust::transfer::TransferLimits::default(),
    )
    .unwrap();
    if p_pack != r_pack {
        differences += 1;
    }
    let p_dest = tmp.path().join("p_import");
    let r_dest = tmp.path().join("r_import");
    cases += 1;
    let pir = port::transfer::import_pack(
        &p_dest,
        p_ing.id,
        &p_pack,
        port::transfer::TransferLimits::default(),
    )
    .unwrap();
    let rir = rust::transfer::import_pack(
        &r_dest,
        r_ing.id,
        &r_pack,
        rust::transfer::TransferLimits::default(),
    )
    .unwrap();
    if to_rust_id(pir.root) != rir.root
        || pir.objects != rir.objects
        || pir.pack_bytes != rir.pack_bytes
        || pir.payload_bytes != rir.payload_bytes
        || pir.inserted != rir.inserted
        || pir.reused != rir.reused
    {
        differences += 1;
    }

    // verify_all on clean store and with injected corruption + stray file.
    cases += 1;
    let p_rep = p_store.verify_all().unwrap();
    let r_rep = r_store.verify_all().unwrap();
    if p_rep.verified_objects != r_rep.verified_objects
        || p_rep.verified_derivations != r_rep.verified_derivations
        || p_rep.is_clean() != r_rep.is_clean()
    {
        differences += 1;
    }

    corrupt_first_byte(&p_store.object_path(&pb.id));
    corrupt_first_byte(&r_store.object_path(&rb.id));
    fs::write(p_store.root().join("objects/v1/stray.txt"), b"x").unwrap();
    fs::write(r_store.root().join("objects/v1/stray.txt"), b"x").unwrap();
    cases += 1;
    let p_bad = p_store.verify_all().unwrap();
    let r_bad = r_store.verify_all().unwrap();
    if p_bad.verified_objects != r_bad.verified_objects
        || p_bad
            .corrupt_objects
            .iter()
            .copied()
            .map(to_rust_id)
            .collect::<Vec<_>>()
            != r_bad.corrupt_objects
        || p_bad.stray_paths.len() != r_bad.stray_paths.len()
    {
        differences += 1;
    }

    // Repair damaged blob through put_blob.
    cases += 1;
    let p_rep_blob = p_store.put_blob(port::Domain::Artifact, &extra).unwrap();
    let r_rep_blob = r_store.put_blob(rust::Domain::Artifact, &extra).unwrap();
    if p_rep_blob.newly_stored != r_rep_blob.newly_stored
        || p_store.read_blob(&pb.id).unwrap() != r_store.read_blob(&rb.id).unwrap()
    {
        differences += 1;
    }

    println!("flip-gate differential differences={differences} cases={cases}");
    assert_eq!(differences, 0);
}
