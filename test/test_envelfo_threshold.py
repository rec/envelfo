from ufor.envelfo import Threshold

from envelfo.runtime import threshold_value


def test_threshold_initializes_silently_and_holds_inside_hysteresis() -> None:
    body = Threshold(input='pressure', lower=0.3, upper=0.7)
    assert threshold_value(body, 1.0, None) == (1.0, None)
    assert threshold_value(body, 0.5, None) == (0.0, None)
    assert threshold_value(body, 0.7, 0.0) == (1.0, 'rising')
    assert threshold_value(body, 0.31, 1.0) == (1.0, None)
    assert threshold_value(body, 0.3, 1.0) == (0.0, 'falling')
    assert threshold_value(body, 0.69, 0.0) == (0.0, None)
