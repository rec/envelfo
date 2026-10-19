"""Shared wire vectors and host-owned delivery lifecycle, without MIDI devices."""

import json
from fractions import Fraction
from pathlib import Path
from typing import cast

import pytest

pytest.importorskip(
    'meedee', reason='install enveLFO[midi] for the optional MIDI tests'
)

from meedee import codecs, messages
from pydantic import BaseModel, ValidationError
from ufor import envelfo

from envelfo import midi, patch


class Step(BaseModel, frozen=True):
    at: str = '0'
    values: dict[str, float] = {}
    wire: list[list[int]] = []
    error: str | None = None
    confirm: bool = True
    report_loss: bool = False
    restart: bool = False


class Case(BaseModel, frozen=True):
    name: str
    mappings: list[midi.Mapping]
    byte_rate: int
    burst_bytes: int
    steps: list[Step]


@pytest.mark.parametrize(
    'case',
    [
        Case.model_validate(c)
        for c in json.loads(
            (Path(__file__).parents[1] / 'conformance/envelfo-midi.json').read_text()
        )['cases']
    ],
    ids=lambda c: c.name,
)
def test_mapper_matches_shared_wire_and_delivery_contract(case: Case) -> None:
    mapper = midi.Mapper(
        mappings=case.mappings,
        byte_rate=case.byte_rate,
        burst_bytes=case.burst_bytes,
    )
    errors = {
        'budget': (OverflowError, 'budget'),
        'pending': (RuntimeError, 'pending'),
        'failed': (RuntimeError, 'restart'),
        'reversed': (ValueError, 'backward'),
        'missing': (ValueError, 'missing'),
        'range': (ValueError, 'range'),
    }
    for step in case.steps:
        if step.report_loss:
            mapper.report_loss()
            continue
        if step.restart:
            mapper.restart()
            continue
        at = Fraction(step.at)
        if step.error is not None:
            exception, match = errors[step.error]
            with pytest.raises(exception, match=match):
                mapper.prepare(at=at, values=step.values)
            continue
        batch = mapper.prepare(at=at, values=step.values)
        wire = [
            list(codecs.encode(p))
            for m in batch.messages
            for p in (
                [m] if isinstance(m, messages.ControlChange) else codecs.expand(m)
            )
        ]
        assert wire == step.wire
        assert batch.wire_bytes == sum(len(m) for m in wire)
        assert batch.at == at
        if step.confirm:
            mapper.confirm(batch)


def mapping() -> midi.Mapping:
    return midi.Mapping(
        output='level',
        target=midi.CCTarget(channel=1, controller=11),
        input_minimum=0.0,
        input_maximum=1.0,
    )


def test_foreign_stale_and_altered_batches_cannot_commit_delivery() -> None:
    first = midi.Mapper(mappings=[mapping()], byte_rate=100, burst_bytes=3)
    second = midi.Mapper(mappings=[mapping()], byte_rate=100, burst_bytes=3)
    batch = first.prepare(at=Fraction(0), values={'level': 0.5})
    foreign = second.prepare(at=Fraction(0), values={'level': 0.5})
    with pytest.raises(ValueError, match='pending batch'):
        first.confirm(foreign)
    original = batch.messages.copy()
    batch.messages.clear()
    with pytest.raises(ValueError, match='altered'):
        first.confirm(batch)
    batch.messages.extend(original)
    first.confirm(batch)
    with pytest.raises(ValueError, match='pending batch'):
        first.confirm(batch)
    first.restart()
    fresh = first.prepare(at=Fraction(0), values={'level': 0.5})
    assert fresh.messages == original
    with pytest.raises(ValueError, match='pending batch'):
        first.confirm(batch)
    first.confirm(fresh)


@pytest.mark.parametrize(
    'value', [float('nan'), float('inf'), -0.01, 1.01, True, '0.5']
)
def test_invalid_observation_leaves_mapper_available_for_correction(
    value: object,
) -> None:
    mapper = midi.Mapper(mappings=[mapping()], byte_rate=100, burst_bytes=3)
    with pytest.raises(ValueError):
        mapper.prepare(at=Fraction(0), values={'level': cast(float, value)})
    batch = mapper.prepare(at=Fraction(0), values={'level': 0.5})
    assert batch.messages[0].value == 64
    mapper.confirm(batch)


def test_mapper_consumes_patch_observation_without_advancing_or_owning_it() -> None:
    definition = envelfo.Patch(
        envelfos={'wave': envelfo.LFO(rate=1, center=0.5, depth=0.5)},
        outputs={'level': 'wave'},
    )
    state = patch.initial_patch(definition, Fraction(0))
    result = patch.advance_patch(
        definition, state, Fraction(1, 4), next_delivery=Fraction(1, 2)
    )
    before = result.state.model_dump_json()
    mapper = midi.Mapper(mappings=[mapping()], byte_rate=100, burst_bytes=3)
    batch = mapper.prepare(at=Fraction(1, 4), values=result.values)
    assert batch.messages == [
        messages.ControlChange(channel=1, controller=11, value=127)
    ]
    mapper.confirm(batch)
    assert result.state.model_dump_json() == before


@pytest.mark.parametrize(
    'change',
    [
        {'input_minimum': 1.0},
        {'input_maximum': float('inf')},
        {'minimum': -1},
        {'maximum': 128},
        {'minimum': 100, 'maximum': 99},
    ],
)
def test_invalid_mapping_ranges_fail_before_observation(
    change: dict[str, float | int],
) -> None:
    with pytest.raises(ValidationError):
        midi.Mapping.model_validate({**mapping().model_dump(), **change})


@pytest.mark.parametrize('rate, burst', [(0, 3), (100, 0), (True, 3), (100, 2)])
def test_invalid_or_unusable_byte_budgets_are_rejected(rate: int, burst: int) -> None:
    with pytest.raises(ValueError):
        midi.Mapper(mappings=[mapping()], byte_rate=rate, burst_bytes=burst)


def test_duplicate_destinations_and_inexact_clock_are_rejected() -> None:
    with pytest.raises(ValueError, match='distinct'):
        midi.Mapper(mappings=[mapping(), mapping()], byte_rate=100, burst_bytes=6)
    mapper = midi.Mapper(mappings=[mapping()], byte_rate=100, burst_bytes=3)
    with pytest.raises(TypeError, match='Fraction'):
        mapper.prepare(at=cast(Fraction, 0.0), values={'level': 0.5})
    batch = mapper.prepare(at=Fraction(0), values={'level': 0.5})
    mapper.confirm(batch)


@pytest.mark.parametrize(
    'target',
    [
        {'kind': 'cc', 'channel': 0, 'controller': 11},
        {'kind': 'cc', 'channel': True, 'controller': 11},
        {'kind': 'cc14', 'channel': 1, 'controller': 32},
        {'kind': 'rpn', 'channel': 1, 'parameter': 16383, 'resolution': 14},
        {'kind': 'rpn', 'channel': 1, 'parameter': 0, 'resolution': 7.0},
        {'kind': 'nrpn', 'channel': 1, 'parameter': 0, 'resolution': 8},
    ],
)
def test_target_identifiers_and_resolutions_are_checked(
    target: dict[str, str | int | float],
) -> None:
    with pytest.raises(ValidationError):
        midi.Mapping.model_validate({**mapping().model_dump(), 'target': target})
