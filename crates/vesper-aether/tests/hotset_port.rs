//! The Vesper hot-set decoder and run builder against the Rust ones (both
//! builds are in the crate under `vesper-port`).
#![cfg(feature = "vesper-port")]

use vesper_aether::hotset::HotSet;

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

fn random_set(rng: &mut Rng) -> HotSet {
    let count = 1 + rng.below(4096) as usize;
    let n = rng.below(300) as usize;
    // Clustered indices so runs are long and split at the batch size.
    let mut at = rng.below(count as u64) as usize;
    let indices: Vec<usize> = (0..n)
        .map(|_| {
            at = if rng.below(8) == 0 {
                rng.below(count as u64) as usize
            } else {
                (at + 1) % count
            };
            at
        })
        .collect();
    HotSet::new(count, &indices).unwrap()
}

#[test]
fn runs_match() {
    let mut rng = Rng(0x0ddb_1a5e_5ca1_ab1d);
    for case in 0..5_000 {
        let h = random_set(&mut rng);
        let batch = [0usize, 1, 2, 3, 7, 64, 512][rng.below(7) as usize];
        assert_eq!(h.runs(batch), h.runs_reference(batch), "case {case}");
    }
}

#[test]
fn decode_matches_on_valid_and_mutated_encodings() {
    let mut rng = Rng(0x005e_ed0f_ca5e);
    for case in 0..5_000 {
        let h = random_set(&mut rng);
        let mut e = h.encode();
        let expected = if rng.below(10) == 0 {
            h.page_count() + 1
        } else {
            h.page_count()
        };
        match rng.below(6) {
            0 => e.truncate(rng.below(e.len() as u64 + 1) as usize),
            1 => e.push(rng.next() as u8),
            2 | 3 => {
                let i = rng.below(e.len() as u64) as usize;
                e[i] ^= 1 << rng.below(8);
            }
            // Duplicate or reorder two indices.
            4 if e.len() >= 24 => {
                let a = 16 + 4 * rng.below(((e.len() - 16) / 4) as u64) as usize;
                let b = 16 + 4 * rng.below(((e.len() - 16) / 4) as u64) as usize;
                let v: [u8; 4] = e[a..a + 4].try_into().unwrap();
                e[b..b + 4].copy_from_slice(&v);
            }
            _ => {}
        }
        assert_eq!(
            HotSet::decode(&e, expected),
            HotSet::decode_reference(&e, expected),
            "case {case}"
        );
    }
}
