"""NumPy-free Patch observations and exact, host-clock connection delivery."""

from fractions import Fraction
from math import ceil, floor
from typing import Literal

from pydantic import Field
from ufor import control, envelfo, envelfo_random
from ufor.base import Model

from . import random, runtime


class PendingEvent(Model, frozen=True):
    at: control.Rational
    target: str
    order: int
    action: Literal['start', 'sample', 'capture', 'cue']
    cue: str | None = None


class PatchState(Model, frozen=True):
    at: control.Rational
    name: str
    seed: int = Field(ge=0, lt=2**64)
    children: dict[str, envelfo.EnvelfoState]
    retained: dict[str, float] = Field(default_factory=dict)
    event_counts: dict[str, int] = Field(default_factory=dict)
    event_random: dict[str, int] = Field(default_factory=dict)
    pending: list[PendingEvent] = Field(default_factory=list)
    observed_at: control.Rational | None = None


class PatchAdvance(Model, frozen=True):
    state: PatchState
    values: dict[str, float]
    events: list[envelfo.EnvelfoOutputEvent]


def connection_gate(
    count: int | None,
    random_state: int | None,
    every: int,
    offset: int,
    probability: float,
    seed: int,
    identity: str,
) -> tuple[int, int | None, bool]:
    """First-aligned division; draw only for eligible, nonconstant chance gates."""
    count = offset if count is None else count
    remaining = every - 1 if count == 0 else count - 1
    threshold = int(probability * 2**53)
    if count != 0 or threshold == 0:
        return remaining, random_state, False
    if threshold == 2**53:
        return remaining, random_state, True
    if random_state is None:
        random_state = seed ^ envelfo_random.stream_key(0, identity)
    random_state, word = random.random_word(random_state)
    return remaining, random_state, word >> 11 < threshold


def observe_signals(
    body: envelfo.Patch,
    signals: dict[str, float],
    retained: dict[str, float],
    seconds: Fraction,
) -> tuple[dict[str, float], dict[str, float], list[tuple[str, str]]]:
    """Evaluate one host observation; missing pruned inputs stay absent."""
    signals, retained = dict(signals), dict(retained)
    emitted: list[tuple[str, str]] = []
    for name in body.signal_order:
        child = body.envelfos[name]
        if isinstance(child, (envelfo.Sum, envelfo.Product)):
            if all(n in signals for n in child.inputs):
                signals[name] = runtime.transform_value(child, signals)
        elif isinstance(child, (envelfo.Affine, envelfo.Quantize)):
            if child.input in signals:
                signals[name] = runtime.transform_value(child, signals)
        elif isinstance(child, (envelfo.Latch, envelfo.Threshold, envelfo.Slew)):
            if child.input not in signals:
                continue
            value = signals[child.input]
            if isinstance(child, envelfo.Latch):
                value = runtime.latch_value(value, retained.get(name))
            elif isinstance(child, envelfo.Slew):
                value = runtime.slew_value(child, value, retained.get(name), seconds)
            else:
                value, port = runtime.threshold_value(child, value, retained.get(name))
                if port is not None:
                    emitted.append((name, port))
            retained[name] = signals[name] = value
    return signals, retained, emitted


def initial_patch(
    definition: envelfo.EnveLFO,
    at: Fraction,
    *,
    name: str = 'patch',
    seed: int = 0,
    tempo_map: control.TempoMap | None = None,
) -> PatchState:
    """Initialize child state without inventing note-on events or observations."""
    body = _body(definition)
    clock = _clock(definition, at, tempo_map)
    children = {
        n: runtime.initial_envelfo(
            _child(definition, n),
            clock,
            seed=seed ^ envelfo_random.stream_key(0, f'patch-{name}-{n}:sample-hold')
            if isinstance(b, envelfo.SampleHold)
            else 0,
        )
        for n, b in body.envelfos.items()
        if isinstance(
            b, (envelfo.LFO, envelfo.Envelope, envelfo.Stages, envelfo.SampleHold)
        )
    }
    return PatchState(at=at, name=name, seed=seed, children=children)


def advance_patch(
    definition: envelfo.EnveLFO,
    state: PatchState,
    at: Fraction,
    *,
    next_delivery: Fraction,
    tempo_map: control.TempoMap | None = None,
) -> PatchAdvance:
    """Process boundaries/deliveries, then observe once on the host's schedule."""
    if next_delivery <= at:
        raise ValueError('next delivery must follow the observation')
    control.elapsed(at, state.at)
    state = state.model_copy(deep=True)
    state, emitted = _run(definition, state, at, tempo_map, True)
    state = state.model_copy(update={'at': at})
    clock = _clock(definition, at, tempo_map)
    signals: dict[str, float] = {}
    for name, child_state in state.children.items():
        value = runtime.envelfo_at(_child(definition, name), child_state, clock)
        signals[name] = value.value * value.weight
    values, retained, edges = observe_signals(
        _body(definition),
        signals,
        state.retained,
        Fraction(0) if state.observed_at is None else at - state.observed_at,
    )
    state = state.model_copy(update={'retained': retained, 'observed_at': at})
    for name, port in edges:
        emitted.extend(
            _route(
                definition,
                state,
                name,
                envelfo.EnvelfoOutputEvent(
                    at=next_delivery,
                    port=port,
                    stage=name,
                    activation=0,
                ),
                next_delivery,
            )
        )
    return PatchAdvance(
        state=state,
        values={n: values[c] for n, c in _body(definition).outputs.items()},
        events=emitted,
    )


def patch_event(
    definition: envelfo.EnveLFO,
    state: PatchState,
    target: str,
    event: envelfo.EnvelfoEvent,
    *,
    tempo_map: control.TempoMap | None = None,
) -> tuple[PatchState, list[envelfo.EnvelfoOutputEvent]]:
    """Deliver a child command in host seconds, without observing the graph."""
    control.elapsed(event.at, state.at)
    state = state.model_copy(deep=True)
    state, emitted = _run(definition, state, event.at, tempo_map, False)
    emitted.extend(_command(definition, state, target, event, tempo_map))
    state, output = _run(definition, state, event.at, tempo_map, True)
    emitted.extend(output)
    return state.model_copy(update={'at': event.at}), emitted


def _body(definition: envelfo.EnveLFO) -> envelfo.Patch:
    if not isinstance(definition.body, envelfo.Patch):
        raise ValueError('Patch execution requires a materialized Patch')
    return definition.body


def _child(definition: envelfo.EnveLFO, name: str) -> envelfo.EnveLFO:
    body = _body(definition).envelfos[name]
    if not isinstance(
        body, (envelfo.LFO, envelfo.Envelope, envelfo.Stages, envelfo.SampleHold)
    ):
        raise ValueError('commands require an executable Patch child')
    return envelfo.EnveLFO(
        scope=definition.scope,
        clock=definition.clock,
        body=body,
    )


def _clock(
    definition: envelfo.EnveLFO,
    seconds: Fraction,
    tempo_map: control.TempoMap | None,
) -> Fraction:
    if definition.clock == control.Clock.seconds:
        return seconds
    if tempo_map is None:
        raise ValueError('Beat-clock Patch requires a host tempo map')
    return tempo_map.elapsed_beats(Fraction(0), seconds)


def _seconds(
    definition: envelfo.EnveLFO, at: Fraction, tempo_map: control.TempoMap | None
) -> Fraction:
    if definition.clock == control.Clock.seconds:
        return at
    assert tempo_map is not None
    seconds = tempo_map.time_for_elapsed_beat(at)
    if seconds is None:
        raise ValueError('Beat-clock Patch event has no host time')
    return seconds


def _route(
    definition: envelfo.EnveLFO,
    state: PatchState,
    source: str,
    event: envelfo.EnvelfoOutputEvent,
    seconds: Fraction,
) -> list[envelfo.EnvelfoOutputEvent]:
    body = _body(definition)
    output = [
        event.model_copy(update={'at': seconds, 'port': n})
        for n, p in body.event_outputs.items()
        if p == f'{source}.{event.port}'
    ]
    for order, connection in enumerate(body.events):
        if connection.source != f'{source}.{event.port}':
            continue
        capture = connection.action == 'capture'
        command = (
            f'capture-{connection.target}-{order}'
            if capture
            else f'cue-{order}'
            if connection.action == 'cue'
            else f'start-{order}'
        )
        target_name = (
            state.name if capture else f'patch-{state.name}-{connection.target}'
        )
        identity = f'{target_name}:{command}'
        count, word_state, forward = connection_gate(
            state.event_counts.get(identity),
            state.event_random.get(identity),
            connection.every,
            connection.offset,
            connection.probability,
            state.seed,
            identity,
        )
        state.event_counts[identity] = count
        if word_state is not None:
            state.event_random[identity] = word_state
        if forward:
            if len(state.pending) >= 4096:
                raise ValueError('Delayed Envelfo event capacity exceeded')
            state.pending.append(
                PendingEvent(
                    at=seconds + connection.delay,
                    target=connection.target,
                    order=order,
                    action=connection.action,
                    cue=connection.cue,
                )
            )
    return output


def _command(
    definition: envelfo.EnveLFO,
    state: PatchState,
    target: str,
    event: envelfo.EnvelfoEvent,
    tempo_map: control.TempoMap | None,
) -> list[envelfo.EnvelfoOutputEvent]:
    child = _child(definition, target)
    current = state.children[target]
    local = event.model_copy(update={'at': _clock(definition, event.at, tempo_map)})
    emitted: list[envelfo.EnvelfoOutputEvent] = []
    if isinstance(child.body, envelfo.Stages):
        result = runtime.advance_envelfo(child, current, local.at, local)
        state.children[target] = result.state
        for output in result.events:
            emitted.extend(
                _route(
                    definition,
                    state,
                    target,
                    output,
                    event.at
                    if output.at == local.at
                    else _seconds(definition, output.at, tempo_map),
                )
            )
    else:
        state.children[target] = runtime.envelfo_event(child, current, local)
    return emitted


def _run(
    definition: envelfo.EnveLFO,
    state: PatchState,
    limit: Fraction,
    tempo_map: control.TempoMap | None,
    inclusive: bool,
) -> tuple[PatchState, list[envelfo.EnvelfoOutputEvent]]:
    emitted: list[envelfo.EnvelfoOutputEvent] = []
    processed = 0
    while True:
        candidates: list[Fraction] = [
            e.at for e in state.pending if e.at <= limit if inclusive or e.at < limit
        ]
        lfos: list[tuple[Fraction, str, str]] = []
        clock_limit = _clock(definition, limit, tempo_map)
        for name, current in state.children.items():
            body = _body(definition).envelfos[name]
            if isinstance(body, envelfo.Stages):
                assert isinstance(current.runtime, envelfo.StageState)
                boundary = runtime.next_stage_event(
                    body, current.runtime, clock_limit, inclusive
                )
                if boundary is not None:
                    seconds = _seconds(definition, boundary[0], tempo_map)
                    if seconds >= state.at:
                        candidates.append(seconds)
            elif isinstance(body, envelfo.LFO) and body.markers:
                assert isinstance(current.runtime, envelfo.LFOState)
                position = current.runtime.position
                if position.paused or not position.rate:
                    continue
                previous = max(position.at, _clock(definition, state.at, tempo_map))
                coordinate = (
                    position.coordinate
                    + position.direction * position.rate * (previous - position.at)
                )
                for marker in body.markers:
                    turn = (
                        floor(coordinate - marker.position) + 1
                        if position.direction == 1
                        else ceil(coordinate - marker.position) - 1
                    )
                    clock = position.at + (
                        turn + marker.position - position.coordinate
                    ) / (position.direction * position.rate)
                    seconds = _seconds(definition, clock, tempo_map)
                    if seconds <= limit and (inclusive or seconds < limit):
                        lfos.append((seconds, name, marker.name))
                        candidates.append(seconds)
        if not candidates:
            break
        seconds = min(candidates)
        clock = _clock(definition, seconds, tempo_map)
        for name, current in list(state.children.items()):
            body = _body(definition).envelfos[name]
            if isinstance(body, envelfo.Stages):
                result = runtime.advance_envelfo(
                    _child(definition, name), current, clock
                )
                state.children[name] = result.state
                processed += len(result.events)
                for event in result.events:
                    emitted.extend(_route(definition, state, name, event, seconds))
        for at, name, port in lfos:
            if at == seconds:
                processed += 1
                emitted.extend(
                    _route(
                        definition,
                        state,
                        name,
                        envelfo.EnvelfoOutputEvent(
                            at=clock, port=port, stage=name, activation=0
                        ),
                        seconds,
                    )
                )
        due = sorted(
            [e for e in state.pending if e.at == seconds], key=lambda e: e.order
        )
        for event in due:
            state.pending.remove(event)
            processed += 1
            if event.action == 'capture':
                state.retained.pop(event.target, None)
            else:
                current = state.children[event.target].runtime
                command = envelfo.EnvelfoEvent(
                    at=seconds,
                    ordinal=current.ordinal + 1,
                    action=event.action,
                    cue=event.cue,
                )
                emitted.extend(
                    _command(definition, state, event.target, command, tempo_map)
                )
        if processed > 4096:
            raise ValueError('Envelfo event capacity exceeded')
        state = state.model_copy(update={'at': seconds})
    for name, current in list(state.children.items()):
        body = _body(definition).envelfos[name]
        if isinstance(body, envelfo.Stages):
            # Exclusive advancement leaves same-time transitions to the command.
            if inclusive:
                result = runtime.advance_envelfo(
                    _child(definition, name),
                    current,
                    _clock(definition, limit, tempo_map),
                )
                state.children[name] = result.state
                assert not result.events
    return state, emitted
