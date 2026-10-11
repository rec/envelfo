from math import nextafter

import pytest
from ufor.envelfo import Quantize, quantize_value

from envelfo.runtime import transform_value


@pytest.mark.parametrize(
    'value,expected',
    [(-1.25, -1), (-0.75, -0.5), (-0.25, 0), (0.25, 0.5), (0.75, 1), (1.25, 1.5)],
)
def test_quantize_half_steps_choose_the_higher_value(
    value: float, expected: float
) -> None:
    body = Quantize(input='signal', step=0.5)
    assert quantize_value(body, value) == expected
    assert transform_value(body, {'signal': value}) == expected
    assert quantize_value(body, nextafter(value, float('-inf'))) == expected - 0.5
    assert quantize_value(body, nextafter(value, float('inf'))) == expected
