import json
from pathlib import Path

import pytest
from ufor.envelfo_random import stream_key

from envelfo.random import random_word


def test_stream_identities_match_shared_sha256_vectors() -> None:
    vectors = json.loads(
        (Path(__file__).parents[1] / 'conformance/envelfo-streams.json').read_text()
    )['vectors']
    for v in vectors:
        assert stream_key(v['seed'], v['identity']) == int(v['key'], 16)


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
