import json
from fractions import Fraction
from pathlib import Path

import pytest
import tomlkit
from pydantic import ValidationError
from ufor import envelfo, envelope, lfo, segments
from ufor.base import Model
from ufor.codec import parse_score, score_toml
from ufor.envelfo import LFO, EnvelfoScore, Envelope
from ufor.oscillator import Waveform, shape_value

from envelfo import runtime


class EnvelopeObservation(envelfo.EnvelfoValue):
    at: Fraction


class EnvelopeCase(Model):
    name: str
    definition: envelfo.Envelope
    events: list[envelfo.EnvelfoEvent]
    observations: list[EnvelopeObservation]


class LFOObservation(lfo.LFOValue):
    at: Fraction


class LFOCase(Model):
    name: str
    definition: lfo.LFO
    events: list[lfo.LFOEvent]
    observations: list[LFOObservation]


def test_previous_voice_envelope_types_and_evaluator_are_removed() -> None:
    for name in (
        'VoiceEnvelope',
        'VoiceEnvelopeEvent',
        'VoiceEnvelopeState',
        'VoiceEnvelopeValue',
        'initial_envelope',
        'envelope_at',
        'envelope_event',
    ):
        assert not hasattr(envelope, name)


def check_envelope(case: EnvelopeCase, extra_observations: bool) -> None:
    state = runtime.initial_envelfo(case.definition, Fraction(0))
    remaining = iter(case.events)
    event = next(remaining, None)
    for probe in case.observations:
        while event is not None and event.at <= probe.at:
            if extra_observations:
                runtime.envelfo_at(
                    case.definition, state, (state.runtime.at + event.at) / 2
                )
                state = envelfo.EnvelfoState.model_validate_json(
                    state.model_dump_json()
                )
            state = runtime.envelfo_event(case.definition, state, event)
            event = next(remaining, None)
        actual = runtime.envelfo_at(case.definition, state, probe.at)
        assert actual.value == pytest.approx(probe.value, abs=TOLERANCE, rel=0)
        assert actual.status == probe.status


def check_lfo(case: LFOCase, extra_observations: bool) -> None:
    state = lfo.initial_lfo(case.definition, Fraction(0))
    remaining = iter(case.events)
    event = next(remaining, None)
    for probe in case.observations:
        while event is not None and event.at <= probe.at:
            if extra_observations:
                lfo.lfo_at(case.definition, state, (state.at + event.at) / 2)
                state = lfo.LFOState.model_validate_json(state.model_dump_json())
            state = lfo.lfo_event(case.definition, state, event)
            event = next(remaining, None)
        actual = lfo.lfo_at(case.definition, state, probe.at)
        assert actual.phase == probe.phase
        assert actual.value == pytest.approx(probe.value, abs=TOLERANCE, rel=0)
        assert actual.weight == pytest.approx(probe.weight, abs=TOLERANCE, rel=0)


def test_release_interrupts_every_stage_without_restarting() -> None:
    definition = ENVELOPES['adsr with delay and hold'].definition
    for at, level in [('1/2', 0), ('2', 0.5), ('7/2', 1), ('5', 0.75), ('7', 0.5)]:
        state = runtime.initial_envelfo(definition, Fraction(0))
        state = runtime.envelfo_event(
            definition,
            state,
            envelfo.EnvelfoEvent(at=Fraction(0), ordinal=0, action='note_on'),
        )
        release_at = Fraction(at)
        state = runtime.envelfo_event(
            definition,
            state,
            envelfo.EnvelfoEvent(at=release_at, ordinal=0, action='note_off'),
        )
        assert runtime.envelfo_at(definition, state, release_at).value == level
        state = runtime.envelfo_event(
            definition,
            state,
            envelfo.EnvelfoEvent(at=release_at + 1, ordinal=0, action='note_off'),
        )
        assert runtime.envelfo_at(definition, state, release_at + 1).value == level / 2
        assert (
            runtime.envelfo_at(definition, state, release_at + 2).status == 'complete'
        )


def test_curved_release_reaches_exact_zero() -> None:
    definition = envelfo.Envelope(
        segments=[segments.Segment(duration=Fraction(0), to=1)],
        release=[segments.Segment(duration=Fraction(1), to=0, curve=-5)],
    )
    state = runtime.initial_envelfo(definition, Fraction(0))
    for ordinal, action in enumerate(['note_on', 'note_off']):
        state = runtime.envelfo_event(
            definition,
            state,
            envelfo.EnvelfoEvent.model_validate(
                {'at': '0', 'ordinal': ordinal, 'action': action}
            ),
        )
    assert runtime.envelfo_at(definition, state, Fraction(1, 2)).value == pytest.approx(
        0.07585818002124355, abs=TOLERANCE, rel=0
    )
    assert runtime.envelfo_at(definition, state, Fraction(1)).value == 0


def test_independent_instances_and_replay_for_seeking() -> None:
    definition = ENVELOPES[
        'release interrupts attack and consumes its full duration'
    ].definition
    idle = runtime.initial_envelfo(definition, Fraction(0))
    active = runtime.envelfo_event(
        definition,
        idle,
        envelfo.EnvelfoEvent(at=Fraction(0), ordinal=0, action='note_on'),
    )
    assert runtime.envelfo_at(definition, idle, Fraction(1)).status == 'idle'
    assert runtime.envelfo_at(definition, active, Fraction(1)).value == 0.5
    released = runtime.envelfo_event(
        definition,
        active,
        envelfo.EnvelfoEvent(at=Fraction(1), ordinal=0, action='note_off'),
    )
    with pytest.raises(ValueError, match='replay'):
        runtime.envelfo_at(definition, released, Fraction(1, 2))
    assert runtime.envelfo_at(definition, active, Fraction(1, 2)).value == 0.25
    oscillator = lfo.LFO(rate=Fraction(1, 4))
    first = lfo.initial_lfo(oscillator, Fraction(0))
    second = lfo.initial_lfo(oscillator, Fraction(1))
    assert lfo.phase_at(first, Fraction(2)) == Fraction(1, 2)
    assert lfo.phase_at(second, Fraction(2)) == Fraction(1, 4)


def test_equal_time_ordinals_are_strict_for_both_generators() -> None:
    definition = ENVELOPES['adsr with delay and hold'].definition
    state = runtime.initial_envelfo(definition, Fraction(0))
    event = envelfo.EnvelfoEvent(at=Fraction(1), ordinal=2, action='note_on')
    state = runtime.envelfo_event(definition, state, event)
    for invalid in [
        event,
        envelfo.EnvelfoEvent(at=Fraction(0), ordinal=3, action='note_off'),
    ]:
        with pytest.raises(ValueError, match='order'):
            runtime.envelfo_event(definition, state, invalid)
    oscillator = lfo.LFO(rate=Fraction(1))
    lfo_state = lfo.initial_lfo(oscillator, Fraction(0))
    change = lfo.LFOEvent(at=Fraction(1), ordinal=0, action='rate', rate=Fraction(2))
    lfo_state = lfo.lfo_event(oscillator, lfo_state, change)
    with pytest.raises(ValueError, match='order'):
        lfo.lfo_event(oscillator, lfo_state, change)
    with pytest.raises(ValueError, match='replay'):
        lfo.lfo_at(oscillator, lfo_state, Fraction(0))


def test_rate_event_payload_is_required_only_for_rate_changes() -> None:
    for payload in [{'action': 'rate'}, {'action': 'reset', 'rate': '1'}]:
        with pytest.raises(ValidationError, match='rate events'):
            lfo.LFOEvent.model_validate({'at': '0', 'ordinal': 0, **payload})


def test_beat_controls_follow_supplied_tempo_positions_without_reset() -> None:
    definition = envelfo.Envelope.model_validate(
        {
            'clock': 'beats',
            'segments': [{'duration': '4 beat', 'to': 1}],
            'release': [{'duration': '0 beat', 'to': 0}],
        }
    )
    oscillator = lfo.LFO.model_validate({'clock': 'beats', 'rate': '1/4'})
    state = runtime.initial_envelfo(definition, Fraction(0))
    state = runtime.envelfo_event(
        definition,
        state,
        envelfo.EnvelfoEvent(at=Fraction(0), ordinal=0, action='note_on'),
    )
    phase = lfo.initial_lfo(oscillator, Fraction(0))
    # Host positions for 120 BPM until second 1, then 60 BPM.
    for seconds, beats in [('0', 0), ('1/2', 1), ('1', 2), ('2', 3)]:
        at = Fraction(beats)
        expected = beats / 4
        assert runtime.envelfo_at(definition, state, at).value == expected, seconds
        assert lfo.phase_at(phase, at) == Fraction(beats, 4), seconds


CASES = json.loads(
    (Path(__file__).parents[1] / 'conformance/modulation.json').read_text()
)
TOLERANCE = CASES['absolute_tolerance']
ENVELOPES = {c['name']: EnvelopeCase.model_validate(c) for c in CASES['envelopes']}
LFOS = [LFOCase.model_validate(c) for c in CASES['lfos']]


@pytest.mark.parametrize('case', ENVELOPES.values(), ids=list(ENVELOPES))
@pytest.mark.parametrize('extra_observations', [False, True])
def test_envelope_conformance(case: EnvelopeCase, extra_observations: bool) -> None:
    check_envelope(case, extra_observations)
    document = EnvelfoScore(
        name='envelope',
        title=case.name,
        body=Envelope(
            clock=case.definition.clock,
            scope=case.definition.scope,
            initial=case.definition.initial,
            segments=case.definition.segments,
            release=case.definition.release,
            polarity=case.definition.polarity,
            hold=case.definition.hold,
            retrigger=case.definition.retrigger,
        ),
    )
    assert parse_score(score_toml(document)) == document


@pytest.mark.parametrize('case', LFOS, ids=[c.name for c in LFOS])
@pytest.mark.parametrize('extra_observations', [False, True])
def test_lfo_conformance(case: LFOCase, extra_observations: bool) -> None:
    check_lfo(case, extra_observations)
    document = EnvelfoScore(
        name='lfo',
        title=case.name,
        body=LFO(
            shape=case.definition.waveform,
            rate=case.definition.rate,
            phase=case.definition.phase,
            duty_cycle=case.definition.duty_cycle,
            reset=case.definition.reset,
            delay=case.definition.delay,
            fade_in=case.definition.fade_in,
        ),
    )
    assert parse_score(score_toml(document)) == document


@pytest.mark.parametrize('case', CASES['curves'])
def test_curve_conformance(case: dict[str, float]) -> None:
    assert envelope.curve_progress(case['progress'], case['curve']) == pytest.approx(
        case['value'], abs=TOLERANCE, rel=0
    )


@pytest.mark.parametrize('case', CASES['shapes'])
def test_shape_conformance(case: dict[str, str | float]) -> None:
    assert shape_value(
        Waveform(case['waveform']),
        Fraction(str(case['phase'])),
        Fraction(str(case['duty_cycle'])),
    ) == pytest.approx(case['value'], abs=TOLERANCE, rel=0)


@pytest.mark.parametrize('case', CASES['invalid_documents'])
def test_invalid_modulation_documents(case: dict[str, object]) -> None:
    with pytest.raises(ValidationError):
        parse_score(tomlkit.dumps({'name': 'invalid', 'title': 'Invalid', **case}))
