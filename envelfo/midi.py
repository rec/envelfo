"""Optional MIDI realization of already observed named control values.

The host owns Patch advancement, observation cadence, transport and delivery.
"""

from fractions import Fraction
from math import isfinite
from typing import Literal, Self

from meedee import codecs, messages
from pydantic import BaseModel, Field, model_validator


class CCTarget(BaseModel, frozen=True):
    channel: int = Field(ge=1, le=16)
    controller: int = Field(ge=0, le=127)
    kind: Literal['cc'] = 'cc'

    model_config = {'extra': 'forbid', 'strict': True}


class FineCCTarget(BaseModel, frozen=True):
    channel: int = Field(ge=1, le=16)
    controller: int = Field(ge=0, le=31)
    kind: Literal['cc14'] = 'cc14'

    model_config = {'extra': 'forbid', 'strict': True}


class RpnTarget(BaseModel, frozen=True):
    channel: int = Field(ge=1, le=16)
    parameter: int = Field(ge=0, le=16382)
    resolution: int = Field(ge=7, le=14, multiple_of=7)
    kind: Literal['rpn'] = 'rpn'

    model_config = {'extra': 'forbid', 'strict': True}


class NrpnTarget(BaseModel, frozen=True):
    channel: int = Field(ge=1, le=16)
    parameter: int = Field(ge=0, le=16383)
    resolution: int = Field(ge=7, le=14, multiple_of=7)
    kind: Literal['nrpn'] = 'nrpn'

    model_config = {'extra': 'forbid', 'strict': True}


class Mapping(BaseModel, frozen=True):
    output: str = Field(min_length=1)
    target: CCTarget | FineCCTarget | RpnTarget | NrpnTarget = Field(
        discriminator='kind'
    )
    input_minimum: float
    input_maximum: float
    minimum: int = 0
    maximum: int | None = None

    @model_validator(mode='after')
    def check_ranges(self) -> Self:
        if not (
            isfinite(self.input_minimum)
            and isfinite(self.input_maximum)
            and self.input_minimum < self.input_maximum
            and isfinite(self.input_maximum - self.input_minimum)
        ):
            raise ValueError('input bounds must define a finite increasing range')
        limit = _value_limit(self.target)
        maximum = limit if self.maximum is None else self.maximum
        if not 0 <= self.minimum <= maximum <= limit:
            raise ValueError('output bounds must lie within the target range')
        return self

    model_config = {'extra': 'forbid', 'strict': True}


class Batch(BaseModel, frozen=True):
    """Ordered primitives/commands. Confirmation accepts only the original batch."""

    at: Fraction
    messages: list[
        messages.ControlChange
        | messages.ControlChange14
        | messages.RpnSet
        | messages.NrpnSet
    ]
    wire_bytes: int

    model_config = {'extra': 'forbid', 'strict': True, 'arbitrary_types_allowed': True}


class Mapper:
    """One serialized caller per output connection, with explicit send completion.

    prepare() reserves one batch; confirm() commits its values and byte budget.
    On any partial/uncertain send or known output loss, call report_loss().
    """

    def __init__(
        self, *, mappings: list[Mapping], byte_rate: int, burst_bytes: int
    ) -> None:
        if not mappings:
            raise ValueError('at least one MIDI mapping is required')
        for value in [byte_rate, burst_bytes]:
            if type(value) is not int or not 0 < value <= 2**63 - 1:
                raise ValueError(
                    'byte rate and burst must be positive signed-64-bit integers'
                )
        destinations = [_destination(m.target) for m in mappings]
        if len(set(destinations)) != len(destinations):
            raise ValueError('MIDI destinations must be distinct')
        initial_bytes = sum(_wire_bytes(_message(m.target, 0)) for m in mappings)
        if initial_bytes > burst_bytes:
            raise ValueError('burst must accommodate the complete initial batch')
        self._mappings = mappings.copy()
        self._rate = byte_rate
        self._burst = burst_bytes
        self._tokens = Fraction(burst_bytes)
        self._at: Fraction | None = None
        self._values: list[int] | None = None
        self._pending: (
            tuple[Batch, list[messages.Message], list[int], Fraction] | None
        ) = None
        self._failed = False

    def prepare(self, *, at: Fraction, values: dict[str, float]) -> Batch:
        """Quantize a complete observation, without advancing a graph or sending."""
        if self._failed:
            raise RuntimeError('MIDI mapper requires explicit restart')
        if self._pending is not None:
            raise RuntimeError('confirm the pending batch before preparing another')
        if not isinstance(at, Fraction):
            raise TypeError('at must be exact Fraction host seconds')
        if self._at is not None and at < self._at:
            raise ValueError('MIDI observation time must not move backward')
        quantized: list[int] = []
        outgoing: list[
            messages.ControlChange
            | messages.ControlChange14
            | messages.RpnSet
            | messages.NrpnSet
        ] = []
        for i, mapping in enumerate(self._mappings):
            if mapping.output not in values:
                raise ValueError(f'missing mapped output: {mapping.output}')
            value = values[mapping.output]
            if type(value) not in (int, float) or not (
                mapping.input_minimum <= value <= mapping.input_maximum
            ):
                raise ValueError(
                    f'output outside its finite input range: {mapping.output}'
                )
            maximum = (
                _value_limit(mapping.target)
                if mapping.maximum is None
                else mapping.maximum
            )
            normalized = (value - mapping.input_minimum) / (
                mapping.input_maximum - mapping.input_minimum
            )
            rounded = round(mapping.minimum + normalized * (maximum - mapping.minimum))
            quantized.append(rounded)
            if self._values is None or self._values[i] != rounded:
                outgoing.append(_message(mapping.target, rounded))
        size = sum(_wire_bytes(m) for m in outgoing)
        tokens = self._tokens
        if self._at is not None:
            tokens = min(Fraction(self._burst), tokens + (at - self._at) * self._rate)
        if size > tokens:
            raise OverflowError('MIDI wire-byte budget exhausted; no batch admitted')
        batch = Batch(at=at, messages=outgoing, wire_bytes=size)
        self._pending = batch, list(outgoing), quantized, tokens - size
        return batch

    def confirm(self, batch: Batch) -> None:
        """Confirm every message was sent successfully, not merely queued by a host."""
        if self._pending is None or batch is not self._pending[0]:
            raise ValueError("batch is not this mapper's pending batch")
        if batch.messages != self._pending[1]:
            raise ValueError('pending batch messages were altered')
        self._values = self._pending[2]
        self._tokens = self._pending[3]
        self._at = batch.at
        self._pending = None

    def report_loss(self) -> None:
        """Invalidate suppression and pending work; send no cleanup or retry."""
        self._pending = None
        self._values = None
        self._failed = True

    def restart(self) -> None:
        """Start an explicitly recovered output session with a fresh full budget."""
        self._pending = None
        self._values = None
        self._tokens = Fraction(self._burst)
        self._at = None
        self._failed = False


def _value_limit(target: CCTarget | FineCCTarget | RpnTarget | NrpnTarget) -> int:
    return (
        127
        if isinstance(target, CCTarget)
        or (isinstance(target, RpnTarget | NrpnTarget) and target.resolution == 7)
        else 16383
    )


def _destination(
    target: CCTarget | FineCCTarget | RpnTarget | NrpnTarget,
) -> tuple[str, int, int]:
    number = (
        target.controller
        if isinstance(target, CCTarget | FineCCTarget)
        else target.parameter
    )
    return target.kind, target.channel, number


def _message(
    target: CCTarget | FineCCTarget | RpnTarget | NrpnTarget, value: int
) -> (
    messages.ControlChange
    | messages.ControlChange14
    | messages.RpnSet
    | messages.NrpnSet
):
    if isinstance(target, CCTarget):
        return messages.ControlChange(
            channel=target.channel, controller=target.controller, value=value
        )
    if isinstance(target, FineCCTarget):
        return messages.ControlChange14(
            channel=target.channel, controller=target.controller, value=value
        )
    if isinstance(target, RpnTarget):
        return messages.RpnSet(
            channel=target.channel,
            parameter=target.parameter,
            value=value,
            resolution=target.resolution,
        )
    return messages.NrpnSet(
        channel=target.channel,
        parameter=target.parameter,
        value=value,
        resolution=target.resolution,
    )


def _wire_bytes(
    message: messages.ControlChange
    | messages.ControlChange14
    | messages.RpnSet
    | messages.NrpnSet,
) -> int:
    if isinstance(message, messages.ControlChange):
        return len(codecs.encode(message))
    return sum(len(codecs.encode(m)) for m in codecs.expand(message))
