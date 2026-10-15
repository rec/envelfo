import json
from fractions import Fraction
from pathlib import Path

import pytest
from ufor import control, envelfo
from ufor.base import Model

from envelfo import patch


class ExpectedEvent(Model, frozen=True):
    at: control.Rational
    port: str


class Command(Model, frozen=True):
    target: str
    event: envelfo.EnvelfoEvent


class Observation(Model, frozen=True):
    at: control.Rational
    next_delivery: control.Rational
    values: dict[str, float]
    events: list[ExpectedEvent]
    restore: bool = False
    commands: list[Command] = []
    pending: list[control.Rational] | None = None


class ContractCase(Model, frozen=True):
    name: str
    definition: envelfo.Generator | patch.PreparedPatch
    steps: list[Observation]
    tempo_map: control.TempoMap | None = None


CASES = [
    ContractCase.model_validate(c)
    for n in ['envelfo-patch.json', 'envelfo-host.json']
    for c in json.loads((Path(__file__).parents[1] / 'conformance' / n).read_text())[
        'cases'
    ]
]


@pytest.mark.parametrize('case', CASES, ids=[c.name for c in CASES])
def test_patch_matches_shared_observation_and_delivery_contract(
    case: ContractCase,
) -> None:
    state = patch.initial_patch(case.definition, Fraction(0), tempo_map=case.tempo_map)
    for step in case.steps:
        if step.restore:
            state = patch.PatchState.model_validate_json(state.model_dump_json())
        events: list[envelfo.EnvelfoOutputEvent] = []
        for command in step.commands:
            state, emitted = patch.patch_event(
                case.definition,
                state,
                command.target,
                command.event,
                tempo_map=case.tempo_map,
                inclusive=False,
            )
            events.extend(emitted)
        result = patch.advance_patch(
            case.definition,
            state,
            step.at,
            next_delivery=step.next_delivery,
            tempo_map=case.tempo_map,
        )
        assert result.values == pytest.approx(step.values, abs=1e-12, rel=0)
        assert [
            ExpectedEvent(at=e.at, port=e.port) for e in [*events, *result.events]
        ] == step.events
        if step.pending is not None:
            assert [e.at for e in result.state.pending] == step.pending
        state = result.state
