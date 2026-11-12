//! Self-checking host-only temp-CAS example; does not register VM memory.
use vesper_aether::*;
use vesper_loom_cas::{Domain, Store};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let store = Store::open(temp.path())?;
    let bytes = [0xa7; PAGE_BYTES];
    let id = store.put_blob(Domain::Artifact, &bytes)?.id;
    let identity = CasPage {
        object: id,
        object_len: PAGE_SIZE_4K,
        offset: 0,
    };
    let entries = [
        SlotSnapshot {
            slot: MemorySlot {
                vm_id: 1,
                slot_id: 0,
                guest_start: 0,
                host_virtual_start: 0x10000,
                byte_len: PAGE_SIZE_4K,
                permission: Permission::ReadOnlyCas {
                    object: id,
                    object_len: PAGE_SIZE_4K,
                    offset: 0,
                },
            },
            pages: vec![PageMapping::ReadOnlyCas(identity)],
        },
        SlotSnapshot {
            slot: MemorySlot {
                vm_id: 1,
                slot_id: 1,
                guest_start: PAGE_SIZE_4K,
                host_virtual_start: 0x20000,
                byte_len: PAGE_SIZE_4K * 2,
                permission: Permission::Private,
            },
            pages: vec![PageMapping::PrivateCow(identity), PageMapping::Zero],
        },
    ];
    let map = SnapshotMap::validate(&entries, Limits::default())?;
    #[cfg(any(unix, windows))]
    {
        let shared = map.resolve(&store, 1, 0)?;
        let cow = map.resolve(&store, 1, PAGE_SIZE_4K + 17)?;
        assert_eq!(shared.bytes(), &bytes);
        assert_eq!(cow.bytes(), &bytes);
        assert_eq!(
            shared.action(),
            UffdPageAction::ContinueSharedCas { identity }
        );
        assert_eq!(cow.action(), UffdPageAction::CopyPrivateCow { identity });
        println!("verified exact 4096-byte readonly/COW output from artifact CAS {id}");
    }
    #[cfg(not(any(unix, windows)))]
    {
        assert_eq!(
            map.resolve(&store, 1, 0).unwrap_err(),
            Error::UnsupportedPlatform
        );
        println!(
            "CAS resolution explicitly unsupported on this platform; layout validation passed"
        );
    }
    let zero = map.resolve(&store, 1, PAGE_SIZE_4K * 2)?;
    assert_eq!(zero.bytes(), &[0; PAGE_BYTES]);
    assert_eq!(zero.action(), UffdPageAction::ZeroPage4K);
    assert_eq!(
        map.resolve(&store, 1, PAGE_SIZE_4K * 3).unwrap_err(),
        Error::UnknownFault
    );
    println!("explicit zero and exclusive-end fault checks passed; actions are plans only");
    Ok(())
}
