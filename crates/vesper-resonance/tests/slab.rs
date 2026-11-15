//! TiledSeq slab packing of the compiled graph's block buffers.

use vesper_resonance::pdc::{
    CompiledGraph, DspProcessor, Input, Inputs, MixGain, NodeId, NodeSpec,
};
use vesper_resonance::slab::{DelaySlab, SlabError, TILE_BYTES, TILE_SAMPLES, TiledSlab};

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

#[test]
fn every_delay_line_starts_on_its_own_cache_line_in_one_allocation() {
    let delays = [0u32, 1, 7, 16, 37, 64, 129];
    for block in [1usize, 7, 16, 64] {
        let slab = DelaySlab::new(&delays, block).unwrap();
        assert_eq!(slab.lines(), delays.len());
        assert_eq!(slab.block(), block);
        let addrs: Vec<usize> = slab.line_addresses().collect();
        assert_eq!(addrs.len(), delays.len());
        let mut expected_total_stride = 0usize;
        for (i, (&addr, &delay)) in addrs.iter().zip(&delays).enumerate() {
            assert_eq!(addr % TILE_BYTES, 0, "line {i} misaligned");
            assert_eq!(slab.line_delay(i), Some(delay as usize));
            let stride = slab.line_stride(i).unwrap();
            assert_eq!(stride % TILE_SAMPLES, 0);
            assert!(stride >= delay as usize + block);
            expected_total_stride += stride;
            if i + 1 < addrs.len() {
                assert_eq!(addrs[i + 1] - addr, stride * 4);
            }
        }
        assert_eq!(slab.working_set_bytes(), expected_total_stride * 4);
    }
}

#[test]
fn delay_slab_read_write_wraps_and_clones_cleanly() {
    let delays = [0u32, 3, 16, 75];
    let block = 17usize;
    let mut slab = DelaySlab::new(&delays, block).unwrap();
    let mut out = vec![0.0f32; block];
    let mut clone_opt: Option<DelaySlab> = None;

    for blk in 0..12usize {
        if blk == 5 {
            clone_opt = Some(slab.clone());
        }
        let input: Vec<f32> = (0..block).map(|k| (blk * block + k + 1) as f32).collect();
        for (line_idx, &delay) in delays.iter().enumerate() {
            slab.write(line_idx, &input);
            slab.read(line_idx, &mut out);
            for (k, &actual) in out.iter().enumerate() {
                let t = blk * block + k;
                let expected = if t >= delay as usize {
                    (t - delay as usize + 1) as f32
                } else {
                    0.0
                };
                assert_eq!(actual, expected, "line {line_idx} delay {delay} sample {t}");
            }
        }
        slab.advance_all(block);
    }

    let mut cloned = clone_opt.unwrap();
    assert!(cloned.line_addresses().all(|a| a % TILE_BYTES == 0));
    let next_input: Vec<f32> = (0..block).map(|k| (5 * block + k + 1) as f32).collect();
    for (line_idx, &delay) in delays.iter().enumerate() {
        cloned.write(line_idx, &next_input);
        cloned.read(line_idx, &mut out);
        for (k, &actual) in out.iter().enumerate() {
            let t = 5 * block + k;
            let expected = if t >= delay as usize {
                (t - delay as usize + 1) as f32
            } else {
                0.0
            };
            assert_eq!(actual, expected, "cloned line {line_idx} sample {t}");
        }
    }
}

#[test]
fn oversized_and_invalid_delay_slabs_are_typed_errors() {
    assert!(matches!(
        DelaySlab::new(&[10], 0),
        Err(SlabError::TooLarge {
            buffers: 1,
            block: 0
        })
    ));
    assert!(matches!(
        DelaySlab::new(&[100], usize::MAX),
        Err(SlabError::TooLarge { .. })
    ));
    assert!(matches!(
        DelaySlab::new(&[100], (isize::MAX as usize / 4) + 1),
        Err(SlabError::TooLarge { .. })
    ));
    let empty = DelaySlab::new(&[], 64).unwrap();
    assert_eq!(empty.lines(), 0);
    assert_eq!(empty.working_set_bytes(), 0);
    assert!(empty.fits(0));
}

#[test]
fn compiled_graph_delay_lines_pack_into_delay_slab() {
    let block = 32;
    let nodes = vec![
        NodeSpec {
            id: NodeId(0),
            latency: 0,
            inputs: vec![],
        },
        NodeSpec {
            id: NodeId(1),
            latency: 19,
            inputs: vec![Input::Forward(NodeId(0))],
        },
        NodeSpec {
            id: NodeId(2),
            latency: 0,
            inputs: vec![
                Input::Forward(NodeId(0)),
                Input::Forward(NodeId(1)),
                Input::Feedback {
                    from: NodeId(2),
                    delay: 48,
                },
            ],
        },
    ];
    let procs: Vec<Box<dyn DspProcessor>> = vec![
        Box::new(Tone(1.0)),
        Box::new(MixGain(1.0)),
        Box::new(MixGain(0.5)),
    ];
    let g = CompiledGraph::new(&nodes, procs, block).unwrap();
    let dslab = g.delay_slab();
    assert_eq!(dslab.lines(), 2);
    assert_eq!(dslab.block(), block);
    assert!(dslab.line_addresses().all(|a| a % TILE_BYTES == 0));
    assert!(dslab.fits(32 * 1024));
}
