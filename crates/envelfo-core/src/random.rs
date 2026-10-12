//! SplitMix64 words, unchanged from enge's native Envelfo runtime.

pub fn random_word(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9e3779b97f4a7c15);
    let mut word = (*state ^ (*state >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    word = (word ^ (word >> 27)).wrapping_mul(0x94d049bb133111eb);
    word ^ (word >> 31)
}
