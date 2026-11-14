//! `dma-ranges` translation and the SiFive cache controller flush that the
//! StarFive JH7100 uses for non-coherent DMA.

use vesper_silicon::Arch;
use vesper_silicon::ccache::{CCACHE_CONFIG, CCACHE_FLUSH32, CcacheError, SifiveCcache};
use vesper_silicon::dma::{
    BarrierInst, DmaCoherence, DmaEngine, DmaError, DmaPhase, NonCoherentCache, StaticDmaBuf,
    ccache_barriers,
};
use vesper_silicon::dtb::{DmaWindow, DtbError, Platform};
use vesper_silicon::ledger::Ledger;

const VF1: &[u8] = include_bytes!("../fixtures/dtb/jh7100-starfive-visionfive-v1.dtb");
const UNMATCHED: &[u8] = include_bytes!("../fixtures/dtb/hifive-unmatched-a00.dtb");
const RV_VIRT: &[u8] = include_bytes!("../fixtures/dtb/qemu-riscv64-virt.dtb");
const EDGE: &[u8] = include_bytes!("../fixtures/dtb/vesper-aia-edge.dtb");

const VF1_MMC: &str = "/soc/mmc@10000000";
const FLUSH64: u64 = 0x0201_0200;

fn w(dma_addr: u64, cpu_addr: u64, size: u64) -> DmaWindow {
    DmaWindow {
        dma_addr,
        cpu_addr,
        size,
    }
}

#[test]
fn jh7100_dma_ranges_reach_the_dma_pool_alias() {
    let p = Platform::from_dtb(VF1).unwrap();
    assert_eq!(
        p.dma_windows(VF1_MMC).unwrap(),
        [
            w(0x8000_0000, 0x8000_0000, 0x7a00_0000),
            w(0xfa00_0000, 0x10_7a00_0000, 0x100_0000),
            w(0xfb00_0000, 0xfb00_0000, 0x7_8500_0000),
        ]
    );
    // The 16 MiB reserved pool at 0x10_7a00_0000 is seen at 0xfa00_0000.
    assert_eq!(
        p.dma_address(VF1_MMC, 0x10_7a00_0000, 0x1000).unwrap(),
        0xfa00_0000
    );
    assert_eq!(
        p.cpu_address(VF1_MMC, 0xfa00_1000, 0x10).unwrap(),
        0x10_7a00_1000
    );
    // DRAM below the window boundary is identity mapped.
    assert_eq!(
        p.dma_address(VF1_MMC, 0x9000_0000, 0x1000).unwrap(),
        0x9000_0000
    );
    // A buffer straddling two windows, and one in no window.
    assert!(matches!(
        p.dma_address(VF1_MMC, 0xf9ff_f000, 0x2000),
        Err(DtbError::NotDmaAddressable { .. })
    ));
    assert!(matches!(
        p.dma_address(VF1_MMC, 0x10_0000_0000, 0x1000),
        Err(DtbError::NotDmaAddressable { .. })
    ));
}

#[test]
fn trees_without_dma_ranges_are_identity() {
    let p = Platform::from_dtb(RV_VIRT).unwrap();
    assert_eq!(
        p.dma_windows("/soc/serial@10000000").unwrap(),
        [w(0, 0, u64::MAX)]
    );
    assert_eq!(
        p.dma_address("/soc/serial@10000000", 0x8000_0000, 64)
            .unwrap(),
        0x8000_0000
    );
}

#[test]
fn nested_dma_ranges_compose_by_intersection() {
    let p = Platform::from_dtb(EDGE).unwrap();
    // Directly under /soc: bus 0 is DRAM 0x8000_0000.
    assert_eq!(
        p.dma_windows("/soc/nic@20004000").unwrap(),
        [w(0, 0x8000_0000, 0x4000_0000)]
    );
    // Behind the inner bus: its two entries, each translated through /soc.
    assert_eq!(
        p.dma_windows("/soc/dma-bus@30000000/dma@0").unwrap(),
        [
            w(0, 0xb000_0000, 0x1000_0000),
            w(0x1000_0000, 0x8000_0000, 0x1000_0000),
        ]
    );
    assert_eq!(
        p.dma_address("/soc/dma-bus@30000000/dma@0", 0xb000_0040, 64)
            .unwrap(),
        0x40
    );
    // DRAM above 0xc000_0000 is outside both windows of the inner bus.
    assert!(matches!(
        p.dma_address("/soc/dma-bus@30000000/dma@0", 0x9000_0000, 64),
        Err(DtbError::NotDmaAddressable { .. })
    ));
}

#[test]
fn engine_descriptors_carry_bus_addresses() {
    let p = Platform::from_dtb(EDGE).unwrap();
    let ledger = Ledger::new();
    let mut e = DmaEngine::new(Arch::Riscv64, 2, 4, 64, &ledger).unwrap();
    e.set_dma_windows(p.dma_windows("/soc/dma-bus@30000000/dma@0").unwrap())
        .unwrap();
    let xfer = e
        .submit_tx(0, StaticDmaBuf::<256>::new(0x8000_1000))
        .unwrap();
    assert_eq!(
        e.descriptor(xfer.desc_slot()).unwrap().phys_addr,
        0x1000_1000
    );
    // The windows are fixed while a transfer is outstanding.
    assert_eq!(
        e.set_dma_windows(Vec::new()),
        Err(DmaError::TransfersInFlight(1))
    );
    // A buffer the device cannot reach is handed back.
    let (_buf, err) = e
        .submit_rx(1, StaticDmaBuf::<256>::new(0x9000_0000))
        .unwrap_err();
    assert_eq!(
        err,
        DmaError::NotDmaAddressable {
            phys: 0x9000_0000,
            len: 256
        }
    );
    e.device_complete(0, 256, None).unwrap();
    let (res, _buf) = e.poll_complete(xfer).unwrap();
    assert_eq!(res, Ok(256));
}

#[test]
fn only_the_jh7100_ccache_is_used_for_dma() {
    let p = Platform::from_dtb(VF1).unwrap();
    assert_eq!(p.ccaches.len(), 1);
    let c = &p.ccaches[0];
    assert_eq!(
        (c.base, c.line_bytes, c.sets, c.cache_bytes, c.dma_flush64),
        (0x0201_0000, 64, Some(2048), Some(0x20_0000), Some(FLUSH64))
    );
    assert_eq!(
        p.dma_coherence(VF1_MMC).unwrap(),
        DmaCoherence::SifiveCcache {
            flush64: FLUSH64,
            line_bytes: 64
        }
    );
    // The FU740's controller is not used for DMA by Linux (its DMA is
    // coherent), so it carries no flush register.
    let u = Platform::from_dtb(UNMATCHED).unwrap();
    assert_eq!(u.ccaches.len(), 1);
    assert_eq!(u.ccaches[0].dma_flush64, None);
    let e = Platform::from_dtb(EDGE).unwrap();
    assert_eq!(e.ccaches[0].dma_flush64, None);
}

#[test]
fn ccache_flush_sequence_matches_linux() {
    let seq = ccache_barriers(DmaPhase::BeforeDevice, 0x8000_0030, 0x50, FLUSH64, 64).unwrap();
    assert_eq!(
        seq,
        [
            BarrierInst::FenceIorwIorw,
            BarrierInst::CcacheFlush64 {
                reg: FLUSH64,
                line: 0x8000_0000
            },
            BarrierInst::FenceIorwIorw,
            BarrierInst::CcacheFlush64 {
                reg: FLUSH64,
                line: 0x8000_0040
            },
            BarrierInst::FenceIorwIorw,
        ]
    );
    assert_eq!(seq[1].to_string(), "sd 0x80000000, (0x2010200)");
    assert_eq!(
        ccache_barriers(DmaPhase::AfterDeviceWrite, 0x8000_0030, 0x50, FLUSH64, 64),
        Err(DmaError::PartialCacheBlock {
            phys: 0x8000_0030,
            len: 0x50,
            block: 64
        })
    );
}

/// One JH7100-style round trip: transmit then receive through the engine,
/// with the engine's barrier sequences run through the controller's
/// register on a cache model, and with each flush left out.
#[test]
fn jh7100_transfers_stay_consistent_only_with_the_flushes() {
    let p = Platform::from_dtb(VF1).unwrap();
    let coherence = p.dma_coherence(VF1_MMC).unwrap();
    let ledger = Ledger::new();
    let base = 0x8000_0000u64;
    let run = |skip_tx: bool, skip_rx: bool| -> (Vec<u8>, Vec<u8>) {
        let mut cache = NonCoherentCache::new(base, 0x4000, 64).unwrap();
        let mut cc = SifiveCcache::new(&p.ccaches[0], 16);
        let mut e = DmaEngine::with_coherence(Arch::Riscv64, coherence, 1, 4, 64, &ledger).unwrap();
        // Transmit: the CPU fills the buffer (dirty in the cache).
        let tx = base + 0x1000;
        cache.cpu_write(tx, &[0xa5; 128]).unwrap();
        let before = e.barriers().len();
        let x = e.submit_tx(0, StaticDmaBuf::<128>::new(tx)).unwrap();
        let seq = e.barriers()[before..].to_vec();
        if !skip_tx {
            cc.run(&mut cache, &seq).unwrap();
        }
        let seen_by_device = cache.device_read(tx, 128).unwrap();
        e.device_complete(0, 128, None).unwrap();
        let _ = e.poll_complete(x).unwrap();
        // Receive: the CPU had read the buffer before (lines cached).
        let rx = base + 0x2000;
        let _ = cache.cpu_read(rx, 128).unwrap();
        let before = e.barriers().len();
        let x = e.submit_rx(0, StaticDmaBuf::<128>::new(rx)).unwrap();
        cc.run(&mut cache, &e.barriers()[before..]).unwrap();
        // A speculative fill while the device owns the buffer (what the
        // post-receive flush exists for).
        let _ = cache.cpu_read(rx, 128).unwrap();
        cache.device_write(rx, &[0x5a; 128]).unwrap();
        e.device_complete(0, 128, None).unwrap();
        let before = e.barriers().len();
        let _ = e.poll_complete(x).unwrap();
        if !skip_rx {
            cc.run(&mut cache, &e.barriers()[before..]).unwrap();
        }
        let seen_by_cpu = cache.cpu_read(rx, 128).unwrap();
        (seen_by_device, seen_by_cpu)
    };
    let (dev, cpu) = run(false, false);
    assert_eq!((dev, cpu), (vec![0xa5; 128], vec![0x5a; 128]));
    let (dev, _) = run(true, false);
    assert_eq!(dev, vec![0; 128]);
    let (_, cpu) = run(false, true);
    assert_eq!(cpu, vec![0; 128]);
}

#[test]
fn ccache_registers() {
    let p = Platform::from_dtb(VF1).unwrap();
    let mut cc = SifiveCcache::new(&p.ccaches[0], 16);
    let base = p.ccaches[0].base;
    // Config: 1 bank, 16 ways, 2^11 sets, 2^6-byte blocks.
    assert_eq!(cc.read64(base + CCACHE_CONFIG).unwrap(), 0x060b_1001);
    let mut cache = NonCoherentCache::new(0x8000_0000, 0x1000, 64).unwrap();
    cache.cpu_write(0x8000_0100, &[7; 4]).unwrap();
    // FLUSH32 takes the address shifted right by 4.
    cc.write32(&mut cache, base + CCACHE_FLUSH32, 0x0800_0010)
        .unwrap();
    assert_eq!(cache.device_read(0x8000_0100, 4).unwrap(), [7; 4]);
    assert_eq!(cc.flushes(), 1);
    assert_eq!(
        cc.write64(&mut cache, base + 0x204, 0),
        Err(CcacheError::Misaligned { addr: base + 0x204 })
    );
    assert_eq!(
        cc.write64(&mut cache, base + 0x300, 0),
        Err(CcacheError::Unmapped { addr: base + 0x300 })
    );
    assert!(matches!(
        cc.write64(&mut cache, base + 0x200, 0x1_0000_0000),
        Err(CcacheError::Dma(DmaError::OutOfWindow { .. }))
    ));
    let mut wrong = NonCoherentCache::new(0x8000_0000, 0x1000, 32).unwrap();
    assert_eq!(
        cc.write64(&mut wrong, base + 0x200, 0x8000_0000),
        Err(CcacheError::LineMismatch {
            controller: 64,
            cache: 32
        })
    );
}

/// A window whose bus end passes 2^64 is refused when the tree is read, and
/// a hand-built one cannot make translation overflow.
#[test]
fn windows_that_pass_the_64_bit_address_space_are_refused() {
    // Patch /soc's `dma-ranges` (bus 0 -> DRAM 0x8000_0000, 1 GiB) so the
    // bus base is 0xffff_ffff_ffff_f000.
    let cells: [u32; 6] = [0, 0, 0, 0x8000_0000, 0, 0x4000_0000];
    let old: Vec<u8> = cells.iter().flat_map(|c| c.to_be_bytes()).collect();
    let mut new = old.clone();
    new[..8].copy_from_slice(&0xffff_ffff_ffff_f000u64.to_be_bytes());
    let at = EDGE
        .windows(old.len())
        .position(|x| x == old.as_slice())
        .unwrap();
    assert_eq!(
        EDGE.windows(old.len())
            .filter(|x| *x == old.as_slice())
            .count(),
        1
    );
    let mut dtb = EDGE.to_vec();
    dtb[at..at + old.len()].copy_from_slice(&new);
    let p = Platform::from_dtb(&dtb).unwrap();
    assert!(matches!(
        p.dma_windows("/soc/nic@20004000"),
        Err(DtbError::AddressOverflow { .. })
    ));
    assert!(matches!(
        p.dma_address("/soc/nic@20004000", 0x8000_2000, 64),
        Err(DtbError::AddressOverflow { .. })
    ));

    let ledger = Ledger::new();
    let mut e = DmaEngine::new(Arch::Riscv64, 2, 4, 64, &ledger).unwrap();
    e.set_dma_windows(vec![w(0xffff_ffff_ffff_f000, 0x8000_0000, 0x4000_0000)])
        .unwrap();
    assert_eq!(
        e.bus_address(0x8000_2000, 64),
        Err(DmaError::RegionOverflow)
    );
}
