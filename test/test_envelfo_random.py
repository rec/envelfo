import pytest

from envelfo.random import random_word


@pytest.mark.parametrize(
    ('state', 'expected'),
    [
        (0, 0xE220A8397B1DCDAF),
        (0x9E3779B97F4A7C15, 0x6E789E6AA1B965F4),
        (0x3C6EF372FE94F82A, 0x06C45D188009454F),
    ],
)
def test_envelfo_random_words_match_portable_vectors(state: int, expected: int) -> None:
    next_state, word = random_word(state)
    assert next_state == (state + 0x9E3779B97F4A7C15) % 2**64
    assert word == expected
