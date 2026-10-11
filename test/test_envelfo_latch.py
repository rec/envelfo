from envelfo.runtime import latch_value


def test_latch_captures_on_activation_and_holds_until_cleared() -> None:
    assert latch_value(-0.25, None) == -0.25
    assert latch_value(0.75, -0.25) == -0.25
    assert latch_value(0.75, None) == 0.75
    assert latch_value(-0.5, 0.75) == 0.75
