//! Non-coherent RISC-V DMA with Zicbom cache-block operations: the barrier
//! sequence, the engine selected from a devicetree, and a software cache
//! model showing the sequence is necessary and sufficient for the modelled
//! data flow.

use vesper_silicon::Arch;
use vesper_silicon::dma::{
    BarrierInst, DmaBuffer, DmaCoherence, DmaEngine, DmaError, DmaPhase, NonCoherentCache,
    StaticDmaBuf, zicbom_barriers,
};
use vesper_silicon::dtb::Platform;
use vesper_silicon::ledger::{Ledger, OwnerKind};

const BOARD: &[u8] = include_bytes!("../fixtures/dtb/vesper-rv-board.dtb");
const BASE: u64 = 0x8000_0000;

fn split(seq: &[BarrierInst]) -> (usize, usize, usize) {
    let c = seq
        .iter()
        .filter(|b| matches!(b, BarrierInst::CboClean(_)))
        .count();
    let i = seq
        .iter()
        .filter(|b| matches!(b, BarrierInst::CboInval(_)))
        .count();
    let f = seq
        .iter()
        .filter(|b| **b == BarrierInst::FenceIorwIorw)
        .count();
    (c, i, f)
}

#[test]
fn zicbom_sequences() {
    let before = zicbom_barriers(DmaPhase::BeforeDevice, BASE, 128, 64).unwrap();
    assert_eq!(
        before,
        [
            BarrierInst::CboClean(BASE),
            BarrierInst::CboClean(BASE + 64),
            BarrierInst::FenceIorwIorw
        ]
    );
    let after = zicbom_barriers(DmaPhase::AfterDeviceWrite, BASE, 128, 64).unwrap();
    assert_eq!(
        after,
        [
            BarrierInst::FenceIorwIorw,
            BarrierInst::CboInval(BASE),
            BarrierInst::CboInval(BASE + 64)
        ]
    );
    let text: Vec<String> = after.iter().map(ToString::to_string).collect();
    assert_eq!(
        text,
        [
            "fence iorw, iorw",
            "cbo.inval (0x80000000)",
            "cbo.inval (0x80000040)"
        ]
    );
    // Cleaning a straddling range is safe and covers both blocks ...
    let s = zicbom_barriers(DmaPhase::BeforeDevice, BASE + 0x30, 0x20, 64).unwrap();
    assert_eq!(split(&s), (2, 0, 1));
    // ... invalidating one is not.
    assert_eq!(
        zicbom_barriers(DmaPhase::AfterDeviceWrite, BASE + 0x30, 0x20, 64),
        Err(DmaError::PartialCacheBlock {
            phys: BASE + 0x30,
            len: 0x20,
            block: 64
        })
    );
    assert_eq!(
        zicbom_barriers(DmaPhase::AfterDeviceWrite, BASE, 96, 64),
        Err(DmaError::PartialCacheBlock {
            phys: BASE,
            len: 96,
            block: 64
        })
    );
    assert_eq!(
        zicbom_barriers(DmaPhase::BeforeDevice, BASE, 64, 48),
        Err(DmaError::BadLineSize(48))
    );
    assert_eq!(
        zicbom_barriers(DmaPhase::BeforeDevice, u64::MAX - 8, 64, 64),
        Err(DmaError::RegionOverflow)
    );
}

#[test]
fn engine_from_the_devicetree_maintains_frames_with_zicbom() {
    let p = Platform::from_dtb(BOARD).unwrap();
    let coherence = p.dma_coherence("/soc/dma@10010000").unwrap();
    assert_eq!(coherence, DmaCoherence::Zicbom { block_bytes: 64 });
    let ledger = Ledger::new();
    let mut allocs = p.frame_allocators(&ledger).unwrap();
    let mut dma = DmaEngine::with_coherence(p.arch, coherence, 2, 4, 0, &ledger).unwrap();
    let frame = allocs[0].alloc().unwrap();
    let pa = frame.phys_addr();
    let rx = dma.submit_rx(0, frame).unwrap();
    // 4096 / 64 = 64 cleans, then the fence before the doorbell.
    assert_eq!(split(dma.barriers()), (64, 0, 1));
    assert_eq!(dma.barriers()[0], BarrierInst::CboClean(pa));
    dma.device_complete(0, 4096, None).unwrap();
    let (res, frame) = dma.poll_complete(rx).unwrap();
    assert_eq!(res, Ok(4096));
    assert_eq!(split(dma.barriers()), (64, 64, 2));
    assert_eq!(dma.barriers()[65], BarrierInst::FenceIorwIorw);
    assert_eq!(dma.barriers()[66], BarrierInst::CboInval(pa));
    allocs[0].free(frame).unwrap();

    // A receive buffer that shares cache blocks with other data is refused
    // before the device owns it, and the buffer comes back.
    let (back, e) = dma
        .submit_rx(1, StaticDmaBuf::<100>::new(0x8030_0010))
        .unwrap_err();
    assert_eq!(
        e,
        DmaError::PartialCacheBlock {
            phys: 0x8030_0010,
            len: 100,
            block: 64
        }
    );
    assert_eq!(back.dma_region(), (0x8030_0010, 100));
    // Transmitting from the same buffer is fine (cleaning is harmless).
    let tx = dma.submit_tx(1, back).unwrap();
    dma.device_complete(1, 100, None).unwrap();
    let (res, _buf) = dma.poll_complete(tx).unwrap();
    assert_eq!(res, Ok(100));
    assert_eq!(ledger.live(OwnerKind::DmaTransfer), 0);
    assert_eq!(ledger.total_leaked(), 0);

    // The coherent NIC gets fences only.
    let nic = p.dma_coherence("/soc/nic@10020000").unwrap();
    let mut dma = DmaEngine::with_coherence(p.arch, nic, 1, 1, 64, &ledger).unwrap();
    let x = dma
        .submit_rx(0, StaticDmaBuf::<100>::new(0x8030_0010))
        .unwrap();
    dma.device_complete(0, 1, None).unwrap();
    let _ = dma.poll_complete(x).unwrap();
    assert_eq!(
        dma.barriers(),
        [BarrierInst::FenceIorwIorw, BarrierInst::FenceIorwIorw]
    );
}

#[test]
fn coherence_must_exist_on_the_architecture() {
    let l = Ledger::new();
    let z = DmaCoherence::Zicbom { block_bytes: 64 };
    assert!(matches!(
        DmaEngine::with_coherence(Arch::Aarch64, z, 1, 1, 64, &l),
        Err(DmaError::CoherenceMismatch { .. })
    ));
    assert!(matches!(
        DmaEngine::with_coherence(Arch::Riscv64, DmaCoherence::ArmNonCoherent, 1, 1, 64, &l),
        Err(DmaError::CoherenceMismatch { .. })
    ));
    assert!(matches!(
        DmaEngine::with_coherence(
            Arch::Riscv64,
            DmaCoherence::Zicbom { block_bytes: 0 },
            1,
            1,
            64,
            &l
        ),
        Err(DmaError::BadLineSize(0))
    ));
}

#[test]
fn transmit_needs_the_clean() {
    let payload: Vec<u8> = (0..128u32).map(|i| i as u8 ^ 0x5a).collect();
    // Without maintenance the device reads stale memory.
    let mut c = NonCoherentCache::new(BASE, 4096, 64).unwrap();
    c.cpu_write(BASE, &payload).unwrap();
    assert_ne!(c.device_read(BASE, 128).unwrap(), payload);
    // With the Zicbom sequence it reads what the CPU wrote.
    let seq = zicbom_barriers(DmaPhase::BeforeDevice, BASE, 128, 64).unwrap();
    c.execute_all(&seq).unwrap();
    assert_eq!(c.device_read(BASE, 128).unwrap(), payload);
}

#[test]
fn receive_needs_the_clean_and_the_invalidate() {
    let data: Vec<u8> = (0..128u32).map(|i| (i * 7) as u8).collect();
    let before = zicbom_barriers(DmaPhase::BeforeDevice, BASE, 128, 64).unwrap();
    let after = zicbom_barriers(DmaPhase::AfterDeviceWrite, BASE, 128, 64).unwrap();

    // Leaving out the invalidate: the CPU reads its stale cached copy.
    let mut c = NonCoherentCache::new(BASE, 4096, 64).unwrap();
    let _ = c.cpu_read(BASE, 128).unwrap();
    c.execute_all(&before).unwrap();
    c.device_write(BASE, &data).unwrap();
    assert_ne!(c.cpu_read(BASE, 128).unwrap(), data);

    // Leaving out the clean: a dirty line written before the transfer is
    // evicted after it and overwrites the device's data in memory.
    let mut c = NonCoherentCache::new(BASE, 4096, 64).unwrap();
    c.cpu_write(BASE, &[0xee; 128]).unwrap();
    c.device_write(BASE, &data).unwrap();
    c.evict_all();
    assert_ne!(c.device_read(BASE, 128).unwrap(), data);

    // The full sequence: correct even under arbitrary eviction.
    let mut c = NonCoherentCache::new(BASE, 4096, 64).unwrap();
    c.cpu_write(BASE, &[0xee; 128]).unwrap();
    c.execute_all(&before).unwrap();
    c.device_write(BASE, &data).unwrap();
    c.evict_all();
    c.execute_all(&after).unwrap();
    assert_eq!(c.cpu_read(BASE, 128).unwrap(), data);
    assert_eq!(c.device_read(BASE, 128).unwrap(), data);
}

#[test]
fn invalidating_a_shared_block_loses_neighbouring_writes() {
    // A counter at BASE shares its 64-byte block with a receive buffer at
    // BASE + 16: exactly the case the engine refuses.
    let mut c = NonCoherentCache::new(BASE, 4096, 64).unwrap();
    c.cpu_write(BASE, &[0x11; 8]).unwrap();
    // The before-device clean writes the counter back ...
    c.execute(&BarrierInst::CboClean(BASE)).unwrap();
    // ... but the CPU keeps updating it while the device fills the buffer.
    c.cpu_write(BASE, &[0x22; 8]).unwrap();
    c.device_write(BASE + 16, &[0xab; 48]).unwrap();
    c.execute(&BarrierInst::CboInval(BASE)).unwrap();
    assert_eq!(c.cpu_read(BASE + 16, 48).unwrap(), [0xab; 48]);
    assert_eq!(
        c.cpu_read(BASE, 8).unwrap(),
        [0x11; 8],
        "the 0x22 update is lost"
    );
    assert!(matches!(
        c.execute(&BarrierInst::CboInval(BASE + 8192)),
        Err(DmaError::OutOfWindow { .. })
    ));
}

fn rng(seed: &mut u64) -> u64 {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 7;
    *seed ^= *seed << 17;
    *seed
}

/// Random CPU traffic, evictions and transfers through the engine: after
/// every receive the CPU sees exactly what the device wrote, and after every
/// transmit the device read exactly what the CPU wrote.
#[test]
fn randomized_transfers_keep_cpu_and_device_views_consistent() {
    const BUFS: usize = 4;
    for seed0 in 1..=64u64 {
        let mut seed = seed0.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
        let ledger = Ledger::new();
        let mut cache = NonCoherentCache::new(BASE, BUFS * 256, 64).unwrap();
        let mut dma = DmaEngine::with_coherence(
            Arch::Riscv64,
            DmaCoherence::Zicbom { block_bytes: 64 },
            1,
            2,
            0,
            &ledger,
        )
        .unwrap();
        for _ in 0..200 {
            let k = (rng(&mut seed) as usize) % BUFS;
            let at = BASE + (k * 256) as u64;
            match rng(&mut seed) % 4 {
                0 => {
                    let b = rng(&mut seed) as u8;
                    let off = rng(&mut seed) % 256;
                    cache.cpu_write(at + off, &[b]).unwrap();
                }
                1 => cache.evict_all(),
                2 => {
                    // Transmit: the device must read the CPU's view.
                    let want = cache.cpu_read(at, 256).unwrap();
                    let start = dma.barriers().len();
                    let x = dma.submit_tx(0, StaticDmaBuf::<256>::new(at)).unwrap();
                    cache.execute_all(&dma.barriers()[start..]).unwrap();
                    assert_eq!(cache.device_read(at, 256).unwrap(), want, "seed {seed0}");
                    dma.device_complete(0, 256, None).unwrap();
                    let _ = dma.poll_complete(x).unwrap();
                }
                _ => {
                    // Receive: the CPU must read the device's data.
                    let data: Vec<u8> = (0..256).map(|_| rng(&mut seed) as u8).collect();
                    let start = dma.barriers().len();
                    let x = dma.submit_rx(0, StaticDmaBuf::<256>::new(at)).unwrap();
                    cache.execute_all(&dma.barriers()[start..]).unwrap();
                    if rng(&mut seed) % 2 == 0 {
                        cache.evict_all();
                    }
                    cache.device_write(at, &data).unwrap();
                    if rng(&mut seed) % 2 == 0 {
                        cache.evict_all();
                    }
                    dma.device_complete(0, 256, None).unwrap();
                    let mid = dma.barriers().len();
                    let _ = dma.poll_complete(x).unwrap();
                    cache.execute_all(&dma.barriers()[mid..]).unwrap();
                    assert_eq!(cache.cpu_read(at, 256).unwrap(), data, "seed {seed0}");
                }
            }
        }
        assert_eq!(ledger.total_leaked(), 0);
    }
}
