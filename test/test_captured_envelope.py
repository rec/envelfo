import json
from fractions import Fraction
from pathlib import Path

import pytest
from ufor.approach import ExponentialApproach
from ufor.envelfo import EnvelfoEvent, EnvelfoState, Envelope, EnvelopeState
from ufor.segments import Segment

from envelfo.runtime import (
    envelfo_at,
    envelfo_event,
    envelope_complete_at,
    initial_envelfo,
)


@pytest.mark.parametrize(
    'case', json.loads(Path('conformance/captured-envelope.json').read_text())['cases']
)
def test_captured_release_matches_shared_vectors_and_json_restore(
    case: dict[str, object],
) -> None:
    stage = ExponentialApproach.model_validate(
        json.loads(Path('conformance/captured-envelope.json').read_text())['stage']
    )
    initial = float(case['initial'])
    body = Envelope(
        initial=initial, segments=[Segment(duration=1, to=initial)], release=[stage]
    )
    state = initial_envelfo(body, Fraction(0))
    state = envelfo_event(
        body, state, EnvelfoEvent(at=Fraction(0), ordinal=0, action='note_on')
    )
    state = envelfo_event(
        body, state, EnvelfoEvent(at=Fraction(0), ordinal=1, action='note_off')
    )
    assert isinstance(state.runtime, EnvelopeState)
    assert state.runtime.resolved_segments is not None
    assert [
        s.duration * 1_000_000_000 for s in state.runtime.resolved_segments
    ] == case['durations_ns']
    first = state.runtime.resolved_segments[0].duration
    assert envelfo_at(body, state, first / 2).value == pytest.approx(
        case['midpoint'], abs=1e-15
    )
    restored = EnvelfoState.model_validate_json(state.model_dump_json())
    end = envelope_complete_at(body, restored)
    assert end == sum(
        (s.duration for s in state.runtime.resolved_segments), Fraction(0)
    )
    for i in range(101):
        at = Fraction(i, 100)
        assert envelfo_at(body, state, at) == envelfo_at(body, restored, at)


def test_note_off_captures_attack_and_bypasses_onset_delay() -> None:
    stage = ExponentialApproach(
        asymptote=0.0,
        time_constant=1.0,
        threshold=0.5,
        transition_to=0.0,
        transition_seconds=0.05,
        minimum_transition_distance=0.5,
    )
    body = Envelope(
        initial=0.0,
        segments=[
            Segment(duration=Fraction(1, 10), to=0.0),
            Segment(duration=1, to=1.0),
        ],
        release=[stage],
    )
    state = envelfo_event(
        body,
        initial_envelfo(body, Fraction(0)),
        EnvelfoEvent(at=Fraction(0), ordinal=0, action='note_on'),
    )
    for at, value in [
        (Fraction(1, 20), 0.0),
        (Fraction(7, 20), 0.25),
        (Fraction(3, 5), 0.5),
    ]:
        released = envelfo_event(
            body, state, EnvelfoEvent(at=at, ordinal=1, action='note_off')
        )
        assert envelfo_at(body, released, at).value == pytest.approx(value)
        assert envelfo_at(body, released, at + Fraction(1, 10)).status == 'complete'
