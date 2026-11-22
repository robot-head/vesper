//! The Vesper PVH ELF parser against the Rust one on truncations, bit
//! flips and random field values (both builds are in the crate under
//! `vesper-port`).
#![cfg(feature = "vesper-port")]

use vesper_aether::guest_mem::GuestMemory;
use vesper_aether::pvh::{build_test_pvh_elf, parse_pvh_elf, parse_pvh_elf_reference};

const CODE: [u8; 10] = [0x8B, 0x03, 0xE6, 0x10, 0x8B, 0x43, 0x04, 0xE6, 0x10, 0xF4];

fn mem() -> GuestMemory {
    GuestMemory::new(&[(0, 2 << 20), (0x1000_0000, 1 << 20)]).unwrap()
}

fn same(elf: &[u8], m: &GuestMemory, what: &str) -> bool {
    let port = parse_pvh_elf(elf, m);
    let reference = parse_pvh_elf_reference(elf, m);
    assert_eq!(port, reference, "{what}");
    port.is_ok()
}

#[test]
fn truncations_and_bit_flips_match() {
    let m = mem();
    let good = build_test_pvh_elf(0x10_0000, &CODE);
    assert!(same(&good, &m, "good"));
    for n in 0..good.len() {
        same(&good[..n], &m, &format!("truncated to {n}"));
    }
    for i in 0..good.len() {
        for bit in 0..8 {
            let mut e = good.clone();
            e[i] ^= 1 << bit;
            same(&e, &m, &format!("byte {i} bit {bit}"));
        }
    }
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// Random program-header and note fields over a valid image: segment
/// kinds, sizes, addresses (inside, across and outside RAM and the boot
/// areas), extra load segments and note descriptors.
#[test]
fn random_headers_match() {
    let m = mem();
    let mut rng = Rng(0x1234_5678_9abc_def1);
    let mut accepted = 0;
    for case in 0..20_000 {
        let paddr =
            [0x10_0000u32, 0x1000_0000, 0x6000, 0x1f_f000, 0x30_0000][rng.below(5) as usize];
        let mut e = build_test_pvh_elf(paddr, &CODE);
        // Add up to two more program headers by rewriting e_phnum and
        // appending entries past the code (the table is then too short or
        // points at garbage unless phoff moves, which the cases cover).
        let flips = rng.below(4);
        for _ in 0..flips {
            let at = match rng.below(6) {
                // A field of either program header (64 + 56 * k + field).
                0..=2 => {
                    64 + 56 * rng.below(2) as usize + [0, 4, 8, 24, 32, 40][rng.below(6) as usize]
                }
                // A note header word or the descriptor.
                3 => 176 + 4 * rng.below(5) as usize,
                // e_phnum, e_phoff, e_phentsize.
                4 => [56, 32, 54][rng.below(3) as usize],
                _ => rng.below(e.len() as u64) as usize,
            };
            let width = [1usize, 4, 8][rng.below(3) as usize];
            let value = match rng.below(5) {
                0 => 0,
                1 => u64::MAX,
                2 => rng.below(8),
                3 => 0x10_0000 + rng.below(0x20_0000),
                _ => rng.next(),
            };
            for k in 0..width {
                if let Some(b) = e.get_mut(at + k) {
                    *b = (value >> (8 * k)) as u8;
                }
            }
        }
        if rng.below(4) == 0 {
            e.extend((0..rng.below(200)).map(|_| rng.next() as u8));
        }
        accepted += usize::from(same(&e, &m, &format!("case {case}")));
    }
    assert!(accepted > 1_000, "too few accepted images: {accepted}");
}
