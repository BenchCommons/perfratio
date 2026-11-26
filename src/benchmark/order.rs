//! Order of the commands within one round of an interleaved schedule (#324).
//!
//! Pure functions, so the policies are testable without spawning processes.
//! A *round* runs every command once; `round` counts from 0.

use crate::options::ScheduleMode;

/// Which part of the benchmark a round belongs to. Each stream has its own
/// random sequence, so the warmup order does not shift the measured order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    Warmup,
    Measure,
}

/// SplitMix64: a small generator with a fixed definition, so a seed gives the
/// same order on every platform and perfratio version.
struct SplitMix64(u64);

impl SplitMix64 {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `0..bound` (rejection sampling, no modulo bias)
    fn below(&mut self, bound: u64) -> u64 {
        let zone = u64::MAX - u64::MAX % bound;
        loop {
            let x = self.next();
            if x < zone {
                return x % bound;
            }
        }
    }
}

/// The order of the `n` commands (indices `0..n`) in `round`.
///
/// - `Grouped` and `RoundRobin`: `0, 1, ..., n-1` every round.
/// - `Randomized`: an independent uniform shuffle per round, derived from
///   `seed`, the stream and the round number only.
/// - `Balanced`: a cyclic Latin square. Round `r` starts at command `r mod n`,
///   so over any `n` consecutive rounds every command takes every position
///   exactly once; over any number of rounds the position counts differ by at
///   most one.
pub fn round_order(
    mode: ScheduleMode,
    seed: u64,
    stream: Stream,
    round: u64,
    n: usize,
) -> Vec<usize> {
    match mode {
        ScheduleMode::Grouped | ScheduleMode::RoundRobin => (0..n).collect(),
        ScheduleMode::Balanced => {
            let start = (round % n.max(1) as u64) as usize;
            (0..n).map(|k| (start + k) % n).collect()
        }
        ScheduleMode::Randomized => {
            let stream_id = match stream {
                Stream::Warmup => 1u64,
                Stream::Measure => 2u64,
            };
            // Mix the three inputs through the generator itself
            let mut mixer = SplitMix64(seed);
            let a = mixer.next() ^ stream_id.wrapping_mul(0xD6E8_FEB8_6659_FD93);
            let mut mixer = SplitMix64(a ^ round);
            let mut rng = SplitMix64(mixer.next());
            let mut order: Vec<usize> = (0..n).collect();
            for i in (1..n).rev() {
                let j = rng.below(i as u64 + 1) as usize;
                order.swap(i, j);
            }
            order
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rounds(mode: ScheduleMode, seed: u64, n: usize, count: u64) -> Vec<Vec<usize>> {
        (0..count)
            .map(|r| round_order(mode, seed, Stream::Measure, r, n))
            .collect()
    }

    #[test]
    fn same_seed_same_order_different_seed_differs() {
        let a = rounds(ScheduleMode::Randomized, 7, 5, 20);
        assert_eq!(a, rounds(ScheduleMode::Randomized, 7, 5, 20));
        assert_ne!(a, rounds(ScheduleMode::Randomized, 8, 5, 20));
    }

    #[test]
    fn randomized_rounds_are_permutations_and_vary() {
        let all = rounds(ScheduleMode::Randomized, 1, 4, 50);
        for order in &all {
            let mut sorted = order.clone();
            sorted.sort_unstable();
            assert_eq!(sorted, vec![0, 1, 2, 3]);
        }
        assert!(all.iter().any(|o| o != &all[0]));
    }

    #[test]
    fn randomized_is_roughly_uniform_in_the_first_position() {
        let all = rounds(ScheduleMode::Randomized, 3, 3, 3000);
        let mut firsts = [0usize; 3];
        for order in &all {
            firsts[order[0]] += 1;
        }
        for count in firsts {
            assert!((900..1100).contains(&count), "{firsts:?}");
        }
    }

    #[test]
    fn warmup_and_measure_streams_differ() {
        let w: Vec<_> = (0..20)
            .map(|r| round_order(ScheduleMode::Randomized, 1, Stream::Warmup, r, 5))
            .collect();
        assert_ne!(w, rounds(ScheduleMode::Randomized, 1, 5, 20));
    }

    #[test]
    fn single_command_and_empty() {
        for mode in [ScheduleMode::Randomized, ScheduleMode::Balanced] {
            assert_eq!(round_order(mode, 1, Stream::Measure, 9, 1), vec![0]);
            assert!(round_order(mode, 1, Stream::Measure, 9, 0).is_empty());
        }
    }

    #[test]
    fn round_robin_order_is_fixed() {
        for order in rounds(ScheduleMode::RoundRobin, 1, 3, 4) {
            assert_eq!(order, vec![0, 1, 2]);
        }
    }

    #[test]
    fn balanced_position_counts_differ_by_at_most_one() {
        for n in 1..=6usize {
            for count in 1..=(3 * n as u64 + 2) {
                let mut table = vec![vec![0u64; n]; n]; // [command][position]
                for order in rounds(ScheduleMode::Balanced, 0, n, count) {
                    for (position, &command) in order.iter().enumerate() {
                        table[command][position] += 1;
                    }
                }
                for position in 0..n {
                    let column: Vec<u64> = table.iter().map(|row| row[position]).collect();
                    let (min, max) = (column.iter().min().unwrap(), column.iter().max().unwrap());
                    assert!(max - min <= 1, "n={n} rounds={count} position={position}");
                }
                if count % n as u64 == 0 {
                    assert!(table.iter().flatten().all(|&c| c == count / n as u64));
                }
            }
        }
    }

    #[test]
    fn each_command_runs_once_per_round() {
        for mode in [ScheduleMode::Randomized, ScheduleMode::Balanced] {
            let flat: Vec<usize> = rounds(mode, 5, 4, 10).concat();
            for c in 0..4 {
                assert_eq!(flat.iter().filter(|&&x| x == c).count(), 10);
            }
        }
    }
}
