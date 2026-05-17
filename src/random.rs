//! Process-level randomness without a dependency.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};

/// Returns a random 64-bit number.
pub(crate) fn random_u64() -> u64 {
    RandomState::new().build_hasher().finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_vary() {
        let first = random_u64();
        assert!((0..16).any(|_| random_u64() != first));
    }
}
