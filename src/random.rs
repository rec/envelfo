//! SplitMix64 words, unchanged from enge's native Envelfo runtime.

use sha2::{Digest, Sha256};

pub fn random_word(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9e3779b97f4a7c15);
    let mut word = (*state ^ (*state >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    word = (word ^ (word >> 27)).wrapping_mul(0x94d049bb133111eb);
    word ^ (word >> 31)
}

/// SHA-256 identity shared with uFor. Renaming the library must not change this prefix.
pub fn stream_key(seed: u64, identity: &str) -> u64 {
    let mut hash = Sha256::new();
    hash.update(b"ufor-envelfo-v1\0");
    hash.update(seed.to_be_bytes());
    hash.update(identity.as_bytes());
    let digest = hash.finalize();
    u64::from_be_bytes(
        digest[..8]
            .try_into()
            .expect("SHA-256 has at least eight bytes"),
    )
}
