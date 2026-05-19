//! Process-level randomness without a dependency.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};

/// Returns a random 64-bit number.
pub(crate) fn random_u64() -> u64 {
    RandomState::new().build_hasher().finish()
}

/// Returns a random number below `bound`, which must not be zero.
pub(crate) fn below(bound: usize) -> usize {
    let bound = u64::try_from(bound).unwrap_or(u64::MAX).max(1);
    usize::try_from(random_u64() % bound).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stays_below_the_bound() {
        for _ in 0..1000 {
            assert!(below(7) < 7);
        }
        assert_eq!(below(1), 0);
    }

    #[test]
    fn numbers_vary() {
        let first = random_u64();
        assert!((0..16).any(|_| random_u64() != first));
    }
}
