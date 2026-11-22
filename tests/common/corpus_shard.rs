//! Bazel corpus sharding. Without Bazel's shard variables, Cargo runs everything.
use std::{env, fs, path::PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shard {
    pub index: usize,
    pub total: usize,
}

impl Shard {
    fn parse(index: Option<&str>, total: Option<&str>) -> Self {
        match (index, total) {
            (None, None) => Self { index: 0, total: 1 },
            (Some(index), Some(total)) => {
                let index = index.parse().expect("TEST_SHARD_INDEX must be an integer");
                let total = total.parse().expect("TEST_TOTAL_SHARDS must be an integer");
                assert!(
                    total > 0 && index < total,
                    "invalid corpus shard {index}/{total}"
                );
                Self { index, total }
            }
            _ => panic!("TEST_SHARD_INDEX and TEST_TOTAL_SHARDS must be set together"),
        }
    }

    pub fn from_environment() -> Self {
        let index = env::var("TEST_SHARD_INDEX").ok();
        let total = env::var("TEST_TOTAL_SHARDS").ok();
        let shard = Self::parse(index.as_deref(), total.as_deref());
        if let Some(status) = env::var_os("TEST_SHARD_STATUS_FILE") {
            fs::File::create(PathBuf::from(status)).expect("acknowledge Bazel sharding support");
        }
        shard
    }

    /// The items of `items` this shard owns, by position (four-item blocks,
    /// as `contains`).
    #[allow(dead_code)]
    pub fn keep<T>(self, items: impl IntoIterator<Item = T>) -> impl Iterator<Item = T> {
        items
            .into_iter()
            .enumerate()
            .filter(move |(i, _)| self.contains(*i))
            .map(|(_, item)| item)
    }

    /// A lower bound `n` on a count over the whole corpus, as this shard's
    /// share of it.
    #[allow(dead_code)]
    pub fn share(self, n: usize) -> usize {
        n / self.total
    }

    pub fn contains(self, corpus_index: usize) -> bool {
        // The corpus JIT-samples every fourth index. Assign four-case blocks
        // so each shard receives those samples, rather than concentrating all
        // of them on shard zero when total == 4.
        (corpus_index / 4) % self.total == self.index
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsharded_runs_every_case() {
        let shard = Shard::parse(None, None);
        assert!((0..465).all(|i| shard.contains(i)));
    }

    #[test]
    fn shards_partition_corpus_exactly_once() {
        for total in [1, 2, 4, 7, 16] {
            let mut visits = vec![0; 465];
            for index in 0..total {
                let shard = Shard::parse(Some(&index.to_string()), Some(&total.to_string()));
                for (i, count) in visits.iter_mut().enumerate() {
                    if shard.contains(i) {
                        *count += 1;
                    }
                }
            }
            assert!(visits.iter().all(|count| *count == 1));
        }
    }

    #[test]
    fn jit_samples_are_balanced_across_shards() {
        for total in [2, 3, 4, 7] {
            let counts: Vec<_> = (0..total)
                .map(|index| {
                    let shard = Shard { index, total };
                    (0..465).step_by(4).filter(|i| shard.contains(*i)).count()
                })
                .collect();
            assert!(counts.iter().max().unwrap() - counts.iter().min().unwrap() <= 1);
        }
    }

    #[test]
    fn keep_partitions_items_exactly_once() {
        let mut seen = vec![];
        for index in 0..3 {
            seen.extend(Shard { index, total: 3 }.keep(0..50));
        }
        seen.sort();
        assert_eq!(seen, (0..50).collect::<Vec<_>>());
        assert_eq!(Shard { index: 0, total: 1 }.keep(0..7).count(), 7);
        assert_eq!(Shard { index: 1, total: 4 }.share(21), 5);
    }

    #[test]
    #[should_panic(expected = "invalid corpus shard")]
    fn rejects_out_of_range_index() {
        Shard::parse(Some("4"), Some("4"));
    }

    #[test]
    #[should_panic(expected = "invalid corpus shard")]
    fn rejects_zero_shards() {
        Shard::parse(Some("0"), Some("0"));
    }

    #[test]
    #[should_panic(expected = "must be set together")]
    fn rejects_partial_configuration() {
        Shard::parse(None, Some("4"));
    }
}
