//! A small seedable generator (splitmix64) so a soak run can be repeated from its seed.

pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed)
    }

    /// A seed from the operating system.
    pub fn random_seed() -> u64 {
        let mut b = [0u8; 8];
        getrandom::fill(&mut b).expect("system randomness");
        u64::from_le_bytes(b)
    }

    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// 0..n (n > 0).
    pub fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    /// True with probability `percent` / 100.
    pub fn chance(&mut self, percent: f64) -> bool {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64 * 100.0 < percent
    }
}
