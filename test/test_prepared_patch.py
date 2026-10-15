from fractions import Fraction

import pytest
from ufor import control, envelfo

from envelfo import patch, random


def test_prepared_graph_keeps_child_clocks_and_same_time_boundaries() -> None:
    definition = patch.PreparedPatch(
        envelfos={
            'clock': envelfo.LFO(
                clock='beats',
                rate=1,
                markers=[envelfo.Marker(name='tick', position=Fraction(1, 4))],
            ),
            'held': envelfo.SampleHold(minimum=0, maximum=1),
        },
        outputs={'value': 'held'},
        events=[
            envelfo.PatchEventConnection(
                source='clock.tick',
                target='held',
                action='sample',
                delay=Fraction(1, 10),
            )
        ],
        connection_identities=['held:sample'],
        sample_keys={'held': 123},
    )
    tempo = control.TempoMap.model_validate(
        {'points': [{'at_seconds': 0, 'beat': 0, 'bpm': 120}]}
    )
    state = patch.initial_patch(definition, Fraction(0), seed=17, tempo_map=tempo)
    before = state.model_dump_json()
    state, _ = patch.settle_patch(
        definition, state, Fraction(1, 8), tempo_map=tempo, inclusive=False
    )
    first = patch.advance_patch(
        definition, state, Fraction(1, 8), next_delivery=Fraction(1, 4), tempo_map=tempo
    )
    assert first.state.pending[0].at == Fraction(9, 40)
    snapshot = patch.PatchState.model_validate_json(first.state.model_dump_json())
    after = patch.advance_patch(
        definition,
        snapshot,
        Fraction(9, 40),
        next_delivery=Fraction(1, 4),
        tempo_map=tempo,
    )
    word_state, _ = random.random_word(17 ^ 123)
    _, word = random.random_word(word_state)
    assert after.values['value'] == (word >> 11) / 2**53
    assert not after.state.pending
    assert first.state.pending
    assert state.model_dump_json() != before


def test_host_inputs_and_stateless_blocks_use_the_same_signal_evaluator() -> None:
    definition = patch.PreparedPatch(
        envelfos={
            'host': envelfo.LFO(rate=1),
            'level': envelfo.Affine(input='host', scale=0.5, offset=0.5),
        },
        inputs=['host'],
        outputs={'value': 'level'},
    )
    state = patch.initial_patch(definition, Fraction(0))
    assert not state.children
    block = patch.observe_block(definition, {'host': [-1.0, 0.0, 1.0]})
    for i, value in enumerate((-1.0, 0.0, 1.0)):
        result = patch.advance_patch(
            definition,
            state,
            Fraction(i),
            next_delivery=Fraction(i + 1),
            inputs={'host': value},
        )
        assert result.values['value'] == block['value'][i]
        state = result.state
    assert state.inputs['host'] == 1


def test_failed_prepared_command_does_not_change_snapshot_containers() -> None:
    definition = patch.PreparedPatch(
        envelfos={'wave': envelfo.LFO(rate=1)}, outputs={'value': 'wave'}
    )
    state = patch.initial_patch(definition, Fraction(0))
    before = state.model_dump_json()
    with pytest.raises(ValueError):
        patch.patch_event(
            definition,
            state,
            'wave',
            envelfo.EnvelfoEvent(at=Fraction(1), ordinal=0, action='sample'),
        )
    assert state.model_dump_json() == before
