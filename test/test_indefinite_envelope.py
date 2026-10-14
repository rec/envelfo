from fractions import Fraction

import pytest
from ufor import envelfo

from envelfo import patch
from envelfo.runtime import (
    advance_envelfo,
    envelfo_at,
    envelfo_event,
    envelope_complete_at,
    initial_envelfo,
)


@pytest.mark.parametrize('waiting', [False, True])
def test_release_completion_tracks_waits_pause_resume_and_advance(
    waiting: bool,
) -> None:
    body = envelfo.Envelope.model_validate(
        {
            'segments': [{'duration': None}],
            'release': ([{'duration': None}] if waiting else [])
            + [{'duration': '1/2 s', 'to': 0}],
        }
    )
    state = initial_envelfo(body, Fraction(0))
    assert envelope_complete_at(body, state) is None
    state = envelfo_event(
        body, state, envelfo.EnvelfoEvent(at=Fraction(0), ordinal=0, action='note_on')
    )
    state = envelfo_event(
        body, state, envelfo.EnvelfoEvent(at=Fraction(1), ordinal=0, action='note_off')
    )
    assert envelope_complete_at(body, state) == (None if waiting else Fraction(3, 2))
    if waiting:
        state = envelfo_event(
            body,
            state,
            envelfo.EnvelfoEvent(at=Fraction(2), ordinal=0, action='advance'),
        )
    state = envelfo_event(
        body,
        state,
        envelfo.EnvelfoEvent(
            at=Fraction(2 if waiting else 1), ordinal=1, action='pause'
        ),
    )
    assert envelope_complete_at(body, state) is None
    state = envelfo_event(
        body, state, envelfo.EnvelfoEvent(at=Fraction(3), ordinal=0, action='resume')
    )
    assert envelope_complete_at(body, state) == Fraction(7, 2)


def definition() -> envelfo.Envelope:
    return envelfo.Envelope.model_validate(
        {
            'segments': [
                {'duration': '1/4 s', 'to': 1},
                {'duration': None},
                {'duration': '1/2 s', 'to': 0.5},
                {'duration': None},
                {'duration': '1/4 s', 'to': 0},
            ],
        }
    )


def test_multiple_holds_advance_from_captured_levels_and_restore() -> None:
    body = definition()
    state = initial_envelfo(body, Fraction(0))
    with pytest.raises(ValueError, match='active indefinite'):
        envelfo_event(
            body,
            state,
            envelfo.EnvelfoEvent(at=Fraction(1, 8), ordinal=0, action='advance'),
        )
    assert envelfo_at(body, state, Fraction(100)).value == 1
    assert envelfo_at(body, state, Fraction(100)).status == 'held'
    state = envelfo_event(
        body, state, envelfo.EnvelfoEvent(at=Fraction(100), ordinal=0, action='advance')
    )
    assert envelfo_at(body, state, Fraction(401, 4)).value == 0.75
    state = envelfo.EnvelfoState.model_validate_json(state.model_dump_json())
    assert envelfo_at(body, state, Fraction(101)).value == 0.5
    state = envelfo_event(
        body, state, envelfo.EnvelfoEvent(at=Fraction(101), ordinal=0, action='advance')
    )
    assert envelfo_at(body, state, Fraction(405, 4)).status == 'complete'


def test_note_off_bypasses_holds_and_release_can_itself_wait() -> None:
    body = definition().model_copy(update={'release': definition().segments})
    state = initial_envelfo(body, Fraction(0))
    state = envelfo_event(
        body, state, envelfo.EnvelfoEvent(at=Fraction(0), ordinal=0, action='note_on')
    )
    state = envelfo_event(
        body,
        state,
        envelfo.EnvelfoEvent(at=Fraction(1, 8), ordinal=0, action='note_off'),
    )
    assert envelfo_at(body, state, Fraction(1, 8)).value == 0.5
    assert envelfo_at(body, state, Fraction(10)).status == 'held'
    state = envelfo_event(
        body, state, envelfo.EnvelfoEvent(at=Fraction(10), ordinal=0, action='advance')
    )
    assert envelfo_at(body, state, Fraction(41, 4)).value == 0.75


@pytest.mark.parametrize(
    'action,payload', [('seek', {'position': '1/2'}), ('shift', {'offset': '1/4'})]
)
def test_indefinite_position_commands_are_rejected(
    action: str, payload: dict[str, str]
) -> None:
    body = definition()
    state = initial_envelfo(body, Fraction(0))
    with pytest.raises(ValueError, match='normalized position'):
        envelfo_event(
            body,
            state,
            envelfo.EnvelfoEvent.model_validate(
                {'at': '1', 'ordinal': 0, 'action': action, **payload}
            ),
        )


def test_staged_final_advance_emits_completion_once_at_command_time() -> None:
    body = envelfo.Stages.model_validate(
        {
            'initial_stage': 'wait',
            'stages': [
                {
                    'name': 'wait',
                    'envelfo': {
                        'kind': 'envelope',
                        'initial': 0.7,
                        'segments': [{'duration': None}],
                    },
                }
            ],
            'transitions': [
                {'from': ['wait'], 'event': 'stage.done', 'action': {'kind': 'finish'}}
            ],
        }
    )
    state = initial_envelfo(body, Fraction(0))
    assert not advance_envelfo(body, state, Fraction(10)).events
    result = advance_envelfo(
        body,
        state,
        Fraction(10),
        envelfo.EnvelfoEvent(at=Fraction(10), ordinal=0, action='advance'),
    )
    assert [e.port for e in result.events] == ['stage.done', 'done']
    assert all(e.at == 10 for e in result.events)
    assert result.value.value == 0.7
    assert result.value.status == 'complete'
    assert not advance_envelfo(body, result.state, Fraction(11)).events


def test_paused_wait_can_advance_without_resuming_the_following_ramp() -> None:
    body = definition()
    state = initial_envelfo(body, Fraction(0))
    state = envelfo_event(
        body, state, envelfo.EnvelfoEvent(at=Fraction(1), ordinal=0, action='pause')
    )
    state = envelfo_event(
        body, state, envelfo.EnvelfoEvent(at=Fraction(2), ordinal=0, action='advance')
    )
    assert envelfo_at(body, state, Fraction(10)).value == 1
    state = envelfo_event(
        body, state, envelfo.EnvelfoEvent(at=Fraction(10), ordinal=0, action='resume')
    )
    assert envelfo_at(body, state, Fraction(41, 4)).value == 0.75


@pytest.mark.parametrize('staged', [False, True])
def test_patch_host_can_advance_a_waiting_child(staged: bool) -> None:
    child = {
        'kind': 'envelope',
        'initial': 1,
        'segments': [{'duration': None}, {'duration': '1/2 s', 'to': 0}],
    }
    if staged:
        child = {
            'kind': 'stages',
            'initial_stage': 'sustain',
            'stages': [{'name': 'sustain', 'envelfo': child}],
        }
    body = envelfo.Patch.model_validate(
        {'envelfos': {'child': child}, 'outputs': {'value': 'child'}}
    )
    state = patch.initial_patch(body, Fraction(0))
    state, events = patch.patch_event(
        body,
        state,
        'child',
        envelfo.EnvelfoEvent(at=Fraction(10), ordinal=0, action='advance'),
    )
    assert not events
    state = patch.PatchState.model_validate_json(state.model_dump_json())
    observed = patch.advance_patch(
        body, state, Fraction(41, 4), next_delivery=Fraction(11)
    )
    assert observed.values['value'] == 0.5


@pytest.mark.parametrize('paused', [False, True])
def test_final_wait_completion_is_not_repeated_by_pause_and_resume(
    paused: bool,
) -> None:
    body = envelfo.Stages.model_validate(
        {
            'initial_stage': 'wait',
            'stages': [
                {
                    'name': 'wait',
                    'envelfo': {'kind': 'envelope', 'segments': [{'duration': None}]},
                }
            ],
        }
    )
    state = initial_envelfo(body, Fraction(0))
    if paused:
        state = envelfo_event(
            body, state, envelfo.EnvelfoEvent(at=Fraction(0), ordinal=0, action='pause')
        )
    result = advance_envelfo(
        body,
        state,
        Fraction(1),
        envelfo.EnvelfoEvent(at=Fraction(1), ordinal=0, action='advance'),
    )
    assert [e.port for e in result.events] == ['stage.done']
    result = result.model_copy(
        update={
            'state': envelfo.EnvelfoState.model_validate_json(
                result.state.model_dump_json()
            )
        }
    )
    for at, action in [(Fraction(1), 'pause'), (Fraction(2), 'resume')]:
        result = advance_envelfo(
            body,
            result.state,
            at,
            envelfo.EnvelfoEvent(at=at, ordinal=1, action=action),
        )
        assert not result.events
