"""Portable extraction fixtures, without audio or NumPy in the runner."""

import json
from fractions import Fraction
from pathlib import Path

import pytest
from ufor import envelfo
from ufor.base import Model
from ufor.control import Rational

from envelfo import random, runtime


class Observation(Model, frozen=True):
    at: Rational
    expected: envelfo.EnvelfoValue
    event: envelfo.EnvelfoEvent | None = None
    events: list[envelfo.EnvelfoOutputEvent] = []
    runtime: dict[str, object] = {}
    restore: bool = False


class ContractCase(Model, frozen=True):
    name: str
    definition: envelfo.EnveLFO
    steps: list[Observation]


CONFORMANCE = json.loads(
    (Path(__file__).parents[1] / 'conformance/envelfo-runtime.json').read_text()
)
CASES = [ContractCase.model_validate(c) for c in CONFORMANCE['cases']]


@pytest.mark.parametrize('case', CASES, ids=[c.name for c in CASES])
def test_envelfo_runtime_matches_portable_contract(case: ContractCase) -> None:
    state = runtime.initial_envelfo(case.definition, Fraction(0))
    for step in case.steps:
        if step.restore:
            state = envelfo.EnvelfoState.model_validate_json(state.model_dump_json())
        if isinstance(case.definition.body, envelfo.Stages):
            result = runtime.advance_envelfo(
                case.definition, state, step.at, step.event
            )
            state, value, events = result.state, result.value, result.events
        else:
            if step.event is not None:
                assert step.event.at == step.at
                state = runtime.envelfo_event(case.definition, state, step.event)
            value = runtime.envelfo_at(case.definition, state, step.at)
            events = []
        assert value.value == pytest.approx(step.expected.value, abs=1e-12, rel=0)
        assert value.weight == pytest.approx(step.expected.weight, abs=1e-12, rel=0)
        assert value.status == step.expected.status
        assert events == step.events
        runtime_fields = state.runtime.model_dump(mode='json')
        assert {k: runtime_fields[k] for k in step.runtime} == step.runtime


@pytest.mark.parametrize(
    'case',
    [c for c in CASES if isinstance(c.definition.body, envelfo.Stages)],
    ids=lambda c: c.name,
)
def test_staged_contract_is_independent_of_advance_partitions(
    case: ContractCase,
) -> None:
    initial = runtime.initial_envelfo(case.definition, Fraction(0))
    whole_state = initial
    whole_events: list[envelfo.EnvelfoOutputEvent] = []
    for step in case.steps:
        if step.event is not None:
            result = runtime.advance_envelfo(
                case.definition, whole_state, step.at, step.event
            )
            whole_state = result.state
            whole_events.extend(result.events)
    end = case.steps[-1].at
    result = runtime.advance_envelfo(case.definition, whole_state, end)
    whole_state = result.state
    whole_events.extend(result.events)

    state = initial
    events: list[envelfo.EnvelfoOutputEvent] = []
    for step in case.steps:
        result = runtime.advance_envelfo(case.definition, state, step.at, step.event)
        state = result.state
        events.extend(result.events)
    assert events == whole_events
    assert state == whole_state
    assert runtime.envelfo_at(case.definition, state, end) == runtime.envelfo_at(
        case.definition, whole_state, end
    )


@pytest.mark.parametrize('vector', CONFORMANCE['random_vectors'])
def test_envelfo_random_matches_shared_words(vector: dict[str, str]) -> None:
    state, word = random.random_word(int(vector['state'], 0))
    assert state == int(vector['next_state'], 0)
    assert word == int(vector['word'], 0)
