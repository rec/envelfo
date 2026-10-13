from fractions import Fraction

import pytest
from pydantic import TypeAdapter
from ufor import control, envelfo, envelfo_random

from envelfo import patch, random


def clock_patch(
    action: str = 'sample',
    delay: str = '0',
    every: int = 1,
    offset: int = 0,
    probability: float = 1,
) -> envelfo.EnveLFO:
    children: dict[str, object] = {
        'clock': {
            'kind': 'lfo',
            'shape': 'triangle',
            'rate': '1',
            'markers': [{'name': 'tick', 'position': '1/4'}],
        },
        'held': {'kind': 'sample_hold', 'minimum': 0, 'maximum': 1},
        'left': {'kind': 'latch', 'input': 'clock'},
        'scaled': {'kind': 'affine', 'input': 'held', 'scale': 2, 'offset': 1},
    }
    if action == 'start':
        children['held'] = {
            'kind': 'envelope',
            'start': 'event',
            'initial': 0,
            'segments': [{'duration': '1/2 s', 'to': 1}],
        }
    return TypeAdapter(envelfo.Generator).validate_python(
        {
            'kind': 'patch',
            'envelfos': children,
            'outputs': {'value': 'held', 'second': 'scaled', 'capture': 'left'},
            'event_outputs': {'tick': 'clock.tick'},
            'events': [
                {
                    'source': 'clock.tick',
                    'target': 'left' if action == 'capture' else 'held',
                    'action': action,
                    'delay': delay,
                    'every': every,
                    'offset': offset,
                    'probability': probability,
                }
            ],
        }
    )


def observe(
    definition: envelfo.EnveLFO, state: patch.PatchState, at: Fraction
) -> patch.PatchAdvance:
    return patch.advance_patch(
        definition, state, at, next_delivery=at + Fraction(1, 100)
    )


@pytest.mark.parametrize('every,offset', [(1, 0), (3, 0), (3, 2), (3, 5)])
@pytest.mark.parametrize('probability', [0, 0.5, 1])
def test_connection_gates_keep_first_alignment_and_independent_random_words(
    every: int, offset: int, probability: float
) -> None:
    count, word_state = None, None
    expected_state = 19 ^ envelfo_random.stream_key(0, 'identity')
    for i in range(20):
        count, word_state, forwarded = patch.connection_gate(
            count, word_state, every, offset, probability, 19, 'identity'
        )
        eligible = i >= offset and (i - offset) % every == 0
        expected = eligible and probability == 1
        if eligible and probability == 0.5:
            expected_state, word = random.random_word(expected_state)
            expected = word >> 11 < 2**52
        assert forwarded == expected
        assert word_state == (
            expected_state if probability == 0.5 and i >= offset else None
        )


def test_exact_deliveries_restore_without_mutating_earlier_snapshots() -> None:
    definition = clock_patch(delay='1/10')
    initial = patch.initial_patch(definition, Fraction(0), name='gesture', seed=81)
    first = observe(definition, initial, Fraction(1, 4))
    assert first.events[0].at == Fraction(1, 4)
    assert first.events[0].port == 'tick'
    assert first.state.pending[0].at == Fraction(7, 20)
    restored = patch.PatchState.model_validate_json(first.state.model_dump_json())
    before = observe(definition, restored, Fraction(1, 3))
    after = observe(definition, before.state, Fraction(7, 20))
    assert before.values['value'] == first.values['value']
    seed = 81 ^ envelfo_random.stream_key(0, 'patch-gesture-held:sample-hold')
    seed, _ = random.random_word(seed)
    _, word = random.random_word(seed)
    assert after.values['value'] == (word >> 11) / 2**53
    assert after.values['second'] == 1 + 2 * after.values['value']
    assert not after.state.pending
    assert not initial.pending
    assert not initial.event_counts
    assert initial.children['held'] == first.state.children['held']
    assert observe(definition, restored, Fraction(7, 20)) == after


@pytest.mark.parametrize('action', ['start', 'sample', 'capture'])
def test_natural_connections_are_partition_independent(action: str) -> None:
    definition = clock_patch(action, delay='1/10', every=3, offset=2, probability=0.5)
    initial = observe(
        definition, patch.initial_patch(definition, Fraction(0), seed=18), Fraction(0)
    ).state
    whole = observe(definition, initial, Fraction(9, 2))
    state, events = initial, []
    for at in [
        Fraction(1, 7),
        Fraction(1, 4),
        Fraction(7, 20),
        Fraction(3, 2),
        Fraction(25, 8),
        Fraction(9, 2),
    ]:
        result = observe(definition, state, at)
        state = result.state
        events.extend(result.events)
    assert events == whole.events
    assert state.children == whole.state.children
    assert state.pending == whole.state.pending
    assert state.event_counts == whole.state.event_counts
    assert state.event_random == whole.state.event_random
    # Captures intentionally depend on the host's observation schedule.
    if action != 'capture':
        assert result.values == whole.values


def test_latch_reads_delivery_observation_not_emission_time() -> None:
    definition = clock_patch('capture', delay='1/10')
    initial = patch.initial_patch(definition, Fraction(0))
    first = observe(definition, initial, Fraction(0))
    event = observe(definition, first.state, Fraction(1, 4))
    assert event.values['capture'] == -1
    delivered = observe(definition, event.state, Fraction(2, 5))
    assert delivered.values['capture'] == pytest.approx(0.6)
    assert (
        observe(definition, delivered.state, Fraction(1, 2)).values['capture']
        == delivered.values['capture']
    )


def test_threshold_waits_for_explicit_next_delivery_then_applies_seconds_delay() -> (
    None
):
    raw = clock_patch().model_dump(mode='json')
    body = raw
    body['envelfos']['edge'] = {
        'kind': 'threshold',
        'input': 'clock',
        'lower': -0.25,
        'upper': 0.25,
    }
    body['event_outputs'] = {'rise': 'edge.rising'}
    body['events'][0].update(source='edge.rising', delay='1/10')
    definition = TypeAdapter(envelfo.Generator).validate_python(raw)
    initial = patch.initial_patch(definition, Fraction(0))
    first = observe(definition, initial, Fraction(0))
    assert not first.events
    result = patch.advance_patch(
        definition, first.state, Fraction(2, 5), next_delivery=Fraction(1, 2)
    )
    assert result.events[0].port == 'rise'
    assert result.events[0].at == Fraction(1, 2)
    assert result.state.pending[0].at == Fraction(3, 5)
    before = observe(definition, result.state, Fraction(11, 20))
    after = observe(definition, before.state, Fraction(3, 5))
    assert before.values['value'] == result.values['value']
    assert after.values['value'] != before.values['value']


def test_slew_uses_host_seconds_even_when_local_beats_are_stopped() -> None:
    definition = TypeAdapter(envelfo.Generator).validate_python(
        {
            'kind': 'patch',
            'envelfos': {
                'constant': {'kind': 'lfo', 'rate': '0', 'center': 1, 'depth': 0},
                'slew': {
                    'kind': 'slew',
                    'input': 'constant',
                    'rise': '2/s',
                    'fall': '1/s',
                    'initial': 0,
                },
            },
            'outputs': {'value': 'slew'},
            'clock': 'beats',
        }
    )
    tempo = control.TempoMap(
        points=[
            control.TempoPoint(
                at_seconds=Fraction(0),
                beat=Fraction(0),
                bpm=Fraction(120),
                running=False,
            )
        ]
    )
    state = patch.initial_patch(definition, Fraction(0), tempo_map=tempo)
    first = patch.advance_patch(
        definition, state, Fraction(0), next_delivery=Fraction(1, 10), tempo_map=tempo
    )
    second = patch.advance_patch(
        definition,
        first.state,
        Fraction(1, 4),
        next_delivery=Fraction(1, 2),
        tempo_map=tempo,
    )
    assert first.values['value'] == 0
    assert second.values['value'] == 0.5
    restored = patch.PatchState.model_validate_json(second.state.model_dump_json())
    assert (
        patch.advance_patch(
            definition,
            restored,
            Fraction(1, 2),
            next_delivery=Fraction(3, 4),
            tempo_map=tempo,
        ).values['value']
        == 1
    )


def test_commands_do_not_add_observations_and_do_not_replay_markers_on_seek() -> None:
    definition = clock_patch()
    state = patch.initial_patch(definition, Fraction(0))
    state, events = patch.patch_event(
        definition,
        state,
        'clock',
        envelfo.EnvelfoEvent(
            at=Fraction(1, 8), ordinal=0, action='seek', position=Fraction(1, 2)
        ),
    )
    assert not events
    assert state.observed_at is None
    result = observe(definition, state, Fraction(1, 4))
    assert not result.events
    assert result.state.observed_at == Fraction(1, 4)


def test_invalid_host_schedule_and_unmaterialized_patch_are_rejected() -> None:
    definition = clock_patch()
    state = patch.initial_patch(definition, Fraction(1))
    with pytest.raises(ValueError, match='next delivery'):
        patch.advance_patch(definition, state, Fraction(1), next_delivery=Fraction(1))
    with pytest.raises(ValueError, match='precedes current state'):
        observe(definition, state, Fraction(0))
    with pytest.raises(ValueError, match='host tempo map'):
        patch.initial_patch(
            definition.model_copy(update={'clock': control.Clock.beats}), Fraction(0)
        )
    with pytest.raises(ValueError, match='materialized Patch'):
        patch.initial_patch(envelfo.LFO(rate=Fraction(1)), Fraction(0))


def staged_patch(clock: str = 'seconds') -> envelfo.EnveLFO:
    unit = 's' if clock == 'seconds' else 'beat'
    return TypeAdapter(envelfo.Generator).validate_python(
        {
            'kind': 'patch',
            'envelfos': {
                'attack': {
                    'kind': 'stages',
                    'initial_stage': 'on',
                    'stages': [
                        {
                            'name': 'on',
                            'envelfo': {
                                'kind': 'envelope',
                                'segments': [{'duration': f'1/2 {unit}', 'to': 1}],
                            },
                        },
                        {'name': 'off', 'envelfo': {'kind': 'hold', 'value': 0}},
                    ],
                    'transitions': [
                        {
                            'from': ['on'],
                            'event': 'note_off',
                            'action': {'kind': 'enter', 'stage': 'off'},
                        }
                    ],
                },
                'receiver': {
                    'kind': 'stages',
                    'initial_stage': 'waiting',
                    'stages': [
                        {'name': 'waiting', 'envelfo': {'kind': 'hold', 'value': 0}},
                        {
                            'name': 'high',
                            'envelfo': {
                                'kind': 'envelope',
                                'initial': 1,
                                'segments': [{'duration': f'1/10 {unit}', 'to': 1}],
                            },
                        },
                    ],
                    'transitions': [
                        {
                            'from': ['waiting', 'high'],
                            'event': 'cue.go',
                            'action': {'kind': 'enter', 'stage': 'high'},
                        }
                    ],
                },
            },
            'outputs': {'value': 'receiver'},
            'event_outputs': {
                'finished': 'attack.stage.done',
                'received': 'receiver.stage.done',
            },
            'events': [
                {
                    'source': 'attack.stage.done',
                    'target': 'receiver',
                    'action': 'cue',
                    'cue': 'go',
                    'delay': '1/10',
                }
            ],
            'clock': clock,
        }
    )


def test_stage_cues_keep_boundary_order_and_survive_restore() -> None:
    definition = staged_patch()
    initial = patch.initial_patch(definition, Fraction(0))
    first = observe(definition, initial, Fraction(1, 2))
    assert [(e.at, e.port) for e in first.events] == [(Fraction(1, 2), 'finished')]
    assert first.values['value'] == 0
    restored = patch.PatchState.model_validate_json(first.state.model_dump_json())
    result = observe(definition, restored, Fraction(1))
    assert result.values['value'] == 1
    assert [(e.at, e.port) for e in result.events] == [(Fraction(7, 10), 'received')]
    whole = observe(definition, initial, Fraction(1))
    assert first.events + result.events == whole.events
    assert whole.state == result.state


def test_child_command_preempts_obsolete_same_time_completion() -> None:
    definition = staged_patch()
    state = patch.initial_patch(definition, Fraction(0))
    state, events = patch.patch_event(
        definition,
        state,
        'attack',
        envelfo.EnvelfoEvent(at=Fraction(1, 2), ordinal=0, action='note_off'),
    )
    assert not events
    assert not state.pending
    assert observe(definition, state, Fraction(1)).values['value'] == 0


def test_beat_boundaries_use_elapsed_transport_but_delays_remain_seconds() -> None:
    definition = staged_patch('beats')
    tempo = control.TempoMap(
        points=[
            control.TempoPoint(
                at_seconds=Fraction(0), beat=Fraction(0), bpm=Fraction(60)
            ),
            control.TempoPoint(
                at_seconds=Fraction(1, 4),
                beat=Fraction(10),
                bpm=Fraction(120),
                running=False,
            ),
            control.TempoPoint(
                at_seconds=Fraction(3, 4), beat=Fraction(4), bpm=Fraction(120)
            ),
        ]
    )
    initial = patch.initial_patch(definition, Fraction(0), tempo_map=tempo)
    result = patch.advance_patch(
        definition, initial, Fraction(7, 8), next_delivery=Fraction(1), tempo_map=tempo
    )
    assert result.events[0].at == Fraction(7, 8)
    assert result.state.pending[0].at == Fraction(39, 40)
    assert result.values['value'] == 0
    delivered = patch.advance_patch(
        definition,
        result.state,
        Fraction(1),
        next_delivery=Fraction(11, 10),
        tempo_map=tempo,
    )
    assert delivered.values['value'] == 1
    assert not delivered.state.pending


def test_same_time_connections_follow_declaration_order() -> None:
    raw = clock_patch('start').model_dump(mode='json')
    raw['events'] = [raw['events'][0], raw['events'][0]]
    definition = TypeAdapter(envelfo.Generator).validate_python(raw)
    initial = patch.initial_patch(definition, Fraction(0))
    result = observe(definition, initial, Fraction(1, 4))
    assert result.state.children['held'].runtime.ordinal == 1
    assert result.values['value'] == 0


def test_event_processing_and_pending_delivery_capacities_are_explicit() -> None:
    definition = clock_patch()
    state = patch.initial_patch(definition, Fraction(0))
    with pytest.raises(ValueError, match='event capacity exceeded'):
        observe(definition, state, Fraction(4097))
    pending = patch.PendingEvent(
        at=Fraction(10), target='held', order=0, action='sample'
    )
    full = state.model_copy(update={'pending': [pending] * 4096})
    with pytest.raises(ValueError, match='Delayed Envelfo event capacity exceeded'):
        observe(definition, full, Fraction(1, 4))
    assert not state.pending


def test_command_events_keep_their_host_timestamp_while_beats_are_stopped() -> None:
    definition = TypeAdapter(envelfo.Generator).validate_python(
        {
            'kind': 'patch',
            'envelfos': {
                'source': {
                    'kind': 'stages',
                    'initial_stage': 'waiting',
                    'stages': [
                        {'name': 'waiting', 'envelfo': {'kind': 'hold', 'value': 0}}
                    ],
                    'transitions': [
                        {
                            'from': ['waiting'],
                            'event': 'cue.finish',
                            'action': {'kind': 'finish'},
                        }
                    ],
                },
                'held': {'kind': 'sample_hold'},
            },
            'outputs': {'value': 'held'},
            'event_outputs': {'done': 'source.done'},
            'events': [
                {
                    'source': 'source.done',
                    'target': 'held',
                    'action': 'sample',
                    'delay': '1/10',
                }
            ],
            'clock': 'beats',
        }
    )
    tempo = control.TempoMap(
        points=[
            control.TempoPoint(
                at_seconds=Fraction(0),
                beat=Fraction(0),
                bpm=Fraction(120),
                running=False,
            )
        ]
    )
    state = patch.initial_patch(definition, Fraction(0), tempo_map=tempo)
    state, emitted = patch.patch_event(
        definition,
        state,
        'source',
        envelfo.EnvelfoEvent(at=Fraction(1), ordinal=0, action='cue', cue='finish'),
        tempo_map=tempo,
    )
    assert emitted[0].at == Fraction(1)
    assert state.pending[0].at == Fraction(11, 10)
    before = patch.advance_patch(
        definition, state, Fraction(1), next_delivery=Fraction(21, 20), tempo_map=tempo
    )
    after = patch.advance_patch(
        definition,
        before.state,
        Fraction(11, 10),
        next_delivery=Fraction(6, 5),
        tempo_map=tempo,
    )
    assert after.values['value'] != before.values['value']
    assert not after.state.pending
