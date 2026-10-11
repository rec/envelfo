from fractions import Fraction

import pytest
from pydantic import ValidationError
from ufor.control import TempoMap
from ufor.envelfo import Contour, EnvelfoEvent, EnvelfoUse
from ufor.segments import Segment

from envelfo.runtime import envelfo_at, envelfo_event, initial_envelfo


def test_beat_contour_follows_host_tempo_and_preserves_authored_unit() -> None:
    clock = TempoMap.model_validate(
        {
            'points': [
                {'at_seconds': '0', 'beat': '0', 'bpm': '120'},
                {'at_seconds': '1', 'beat': '2', 'bpm': '60'},
            ]
        }
    )
    envelfo = EnvelfoUse.model_validate(
        {
            'clock': 'beats',
            'body': {
                'kind': 'contour',
                'segments': [{'duration': '4 beat', 'to': 1}],
            },
        }
    )
    assert (
        envelfo.model_dump(mode='json')['body']['segments'][0]['duration'] == '4 beat'
    )
    state = envelfo_event(
        envelfo,
        initial_envelfo(envelfo, Fraction(0)),
        EnvelfoEvent(at=Fraction(0), ordinal=0, action='note_on'),
    )
    assert envelfo_at(envelfo, state, clock.beat_at(Fraction(1))).value == 0.5
    assert envelfo_at(envelfo, state, clock.beat_at(Fraction(2))).value == 0.75
    assert isinstance(envelfo.body, Contour)
    with pytest.raises(ValidationError, match='must match its clock'):
        EnvelfoUse.model_validate(
            {
                'clock': 'beats',
                'body': {
                    'kind': 'contour',
                    'segments': [{'duration': '4 s', 'to': 1}],
                },
            }
        )
    with pytest.raises(ValidationError, match='conflicts with its suffix'):
        Segment.model_validate({'duration': '4 s', 'duration_unit': 'beats', 'to': 1})
