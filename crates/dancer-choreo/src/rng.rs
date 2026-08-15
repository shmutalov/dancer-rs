//! A tiny seeded PRNG.
//!
//! Move selection is random, and random plus untestable is a bad combination for
//! the one component whose output is judged by eye. A seeded generator makes every
//! choreography reproducible: a test can assert "these inputs produce this sequence
//! of moves", and a bug report can carry its seed.
//!
//! This is xorshift64*, which is fine for picking between eight sprite rows and is
//! not fit for anything else.

pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        // Zero is a fixed point of xorshift, so it must not survive as state.
        Self(if seed == 0 { 0x9E37_79B9_7F4A_7C15 } else { seed })
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in `0.0..1.0`.
    pub fn next_f32(&mut self) -> f32 {
        // Top 24 bits: exactly the mantissa an f32 can represent.
        (self.next_u64() >> 40) as f32 / (1u32 << 24) as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_gives_the_same_sequence() {
        let a: Vec<u64> = (0..8).map(|_| Rng::new(42).next_u64()).collect();
        let mut r = Rng::new(42);
        assert_eq!(r.next_u64(), a[0]);
        // Different seeds diverge.
        assert_ne!(Rng::new(1).next_u64(), Rng::new(2).next_u64());
    }

    #[test]
    fn zero_seed_does_not_collapse() {
        let mut r = Rng::new(0);
        let a = r.next_u64();
        let b = r.next_u64();
        assert_ne!(a, 0);
        assert_ne!(a, b);
    }

    #[test]
    fn floats_stay_in_range_and_spread_out() {
        let mut r = Rng::new(7);
        let mut buckets = [0usize; 4];
        for _ in 0..4000 {
            let v = r.next_f32();
            assert!((0.0..1.0).contains(&v), "{v}");
            buckets[(v * 4.0) as usize] += 1;
        }
        // Not a statistical test — just catches a generator stuck in one corner.
        assert!(buckets.iter().all(|&n| n > 500), "{buckets:?}");
    }
}
