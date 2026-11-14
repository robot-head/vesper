//! TiledSeq slab packing of the compiled graph's block buffers.

use vesper_resonance::pdc::{
    CompiledGraph, DspProcessor, Input, Inputs, MixGain, NodeId, NodeSpec,
};
use vesper_resonance::slab::{SlabError, TILE_BYTES, TILE_SAMPLES, TiledSlab};

#[test]
fn every_buffer_starts_on_its_own_cache_line() {
    for block in [1usize, 7, 16, 17, 64, 100, 128] {
        let slab = TiledSlab::new(9, block).unwrap();
        assert_eq!(slab.stride() % TILE_SAMPLES, 0);
        assert!(slab.stride() >= block && slab.stride() < block + TILE_SAMPLES);
        let addrs: Vec<usize> = slab.buffer_addresses().collect();
        assert_eq!(addrs.len(), 9);
        for (i, a) in addrs.iter().enumerate() {
            assert_eq!(a % TILE_BYTES, 0, "block {block} buffer {i} misaligned");
            assert_eq!(slab.buffer(i).unwrap().len(), block);
        }
        // One contiguous allocation: buffers are exactly `stride` apart.
        for w in addrs.windows(2) {
            assert_eq!(w[1] - w[0], slab.stride() * 4);
        }
        assert_eq!(slab.working_set_bytes(), 9 * slab.stride() * 4);
    }
}

#[test]
fn split_out_gives_one_writer_and_every_other_reader() {
    let mut slab = TiledSlab::new(5, 20).unwrap();
    for i in 0..5 {
        slab.buffer_mut(i).unwrap().fill(i as f32);
    }
    for out in 0..5 {
        let (read, w) = slab.split_out(out).unwrap();
        assert!(w.iter().all(|&x| x == out as f32));
        for i in 0..5 {
            match read.get(i) {
                Some(b) => {
                    assert_ne!(i, out);
                    assert_eq!(b.len(), 20);
                    assert!(
                        b.iter().all(|&x| x == i as f32),
                        "buffer {i} via split {out}"
                    );
                }
                None => assert_eq!(i, out),
            }
        }
        assert!(read.get(5).is_none());
        w.fill(-1.0);
        slab.buffer_mut(out).unwrap().fill(out as f32);
    }
    assert!(slab.split_out(5).is_none());
    assert!(slab.buffer(5).is_none());
    assert!(slab.buffer_mut(5).is_none());
    // Writes through one buffer never touch a neighbour (padding included).
    slab.buffer_mut(2).unwrap().fill(9.0);
    assert!(slab.buffer(1).unwrap().iter().all(|&x| x == 1.0));
    assert!(slab.buffer(3).unwrap().iter().all(|&x| x == 3.0));
}

#[test]
fn oversized_slabs_are_typed_errors() {
    assert_eq!(
        TiledSlab::new(usize::MAX / 4, 64).unwrap_err(),
        SlabError::TooLarge {
            buffers: usize::MAX / 4,
            block: 64
        }
    );
    assert!(matches!(
        TiledSlab::new(2, usize::MAX),
        Err(SlabError::TooLarge { .. })
    ));
    let empty = TiledSlab::new(0, 64).unwrap();
    assert_eq!(empty.working_set_bytes(), 0);
}

struct Tone(f32);
impl DspProcessor for Tone {
    fn process(&mut self, _: &Inputs<'_>, out: &mut [f32]) {
        out.fill(self.0);
    }
}

/// The §32.1 item 4 claim: a 64-channel mixer at 64 samples per block packs
/// into one slab that fits a 32 KiB L1 data cache, and mixes correctly.
#[test]
fn sixty_four_channel_mixer_fits_l1() {
    let block = 64;
    let mut nodes: Vec<NodeSpec> = (0..64)
        .map(|c| NodeSpec {
            id: NodeId(c),
            latency: 0,
            inputs: vec![],
        })
        .collect();
    nodes.push(NodeSpec {
        id: NodeId(64),
        latency: 0,
        inputs: (0..64).map(|c| Input::Forward(NodeId(c))).collect(),
    });
    let mut procs: Vec<Box<dyn DspProcessor>> = (0..64)
        .map(|c| Box::new(Tone(c as f32 / 64.0)) as Box<dyn DspProcessor>)
        .collect();
    procs.push(Box::new(MixGain(0.5)));
    let mut g = CompiledGraph::new(&nodes, procs, block).unwrap();
    let slab = g.slab();
    assert_eq!(slab.buffers(), 65, "every channel is live until the mix");
    assert_eq!(slab.working_set_bytes(), 65 * 64 * 4);
    assert!(slab.fits(32 * 1024), "{} bytes", slab.working_set_bytes());
    g.process_block();
    let expected = 0.5 * (0..64).map(|c| c as f32 / 64.0).sum::<f32>();
    let out = g.output(NodeId(64)).unwrap();
    assert_eq!(out.len(), block);
    assert!(out.iter().all(|&y| (y - expected).abs() < 1e-5), "{out:?}");
}

#[test]
fn clones_are_realigned_and_hold_the_same_samples() {
    let mut slab = TiledSlab::new(9, 64).unwrap();
    for i in 0..9 {
        slab.buffer_mut(i).unwrap().fill(i as f32 + 0.5);
    }
    // Keep every clone alive so the allocator hands out varied addresses.
    let clones: Vec<TiledSlab> = (0..64).map(|_| slab.clone()).collect();
    for c in &clones {
        assert!(c.buffer_addresses().all(|a| a % TILE_BYTES == 0));
        for i in 0..9 {
            assert_eq!(c.buffer(i).unwrap(), slab.buffer(i).unwrap());
        }
    }
}
