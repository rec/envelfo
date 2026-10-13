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


class Observation(Model, frozen=True):
    at: control.Rational
    next_delivery: control.Rational
    values: dict[str, float]
    events: list[ExpectedEvent]
    restore: bool = False


class ContractCase(Model, frozen=True):
    name: str
    definition: envelfo.EnveLFO
    steps: list[Observation]


CASES = [
    ContractCase.model_validate(c)
    for c in json.loads(
        (Path(__file__).parents[1] / 'conformance/envelfo-patch.json').read_text()
    )['cases']
]


@pytest.mark.parametrize('case', CASES, ids=[c.name for c in CASES])
def test_patch_matches_shared_observation_and_delivery_contract(
    case: ContractCase,
) -> None:
    state = patch.initial_patch(case.definition, Fraction(0))
    for step in case.steps:
        if step.restore:
            state = patch.PatchState.model_validate_json(state.model_dump_json())
        result = patch.advance_patch(
            case.definition, state, step.at, next_delivery=step.next_delivery
        )
        assert result.values == pytest.approx(step.values, abs=1e-12, rel=0)
        assert [
            ExpectedEvent(at=e.at, port=e.port) for e in result.events
        ] == step.events
        state = result.state
