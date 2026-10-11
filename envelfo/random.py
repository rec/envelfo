"""Portable SplitMix64 execution for Envelfo signals and event gates."""


def random_word(state: int) -> tuple[int, int]:
    state = (state + 0x9E3779B97F4A7C15) & 0xFFFF_FFFF_FFFF_FFFF
    word = ((state ^ (state >> 30)) * 0xBF58476D1CE4E5B9) & 0xFFFF_FFFF_FFFF_FFFF
    word = ((word ^ (word >> 27)) * 0x94D049BB133111EB) & 0xFFFF_FFFF_FFFF_FFFF
    return state, word ^ (word >> 31)
