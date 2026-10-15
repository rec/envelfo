"""NumPy-free Patch observations and exact, host-clock connection delivery."""

from fractions import Fraction
from functools import cached_property
from graphlib import TopologicalSorter
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


class PreparedPatch(Model, frozen=True):
    """Materialized host graph; executable children retain their own clocks.

    Authored Patches still inherit clock/scope in uFor. This execution boundary
    also joins independently authored generators without changing their models.
    """

    envelfos: dict[
        str,
        envelfo.LFO
        | envelfo.Envelope
        | envelfo.Stages
        | envelfo.SampleHold
        | envelfo.Sum
        | envelfo.Product
        | envelfo.Affine
        | envelfo.Quantize
        | envelfo.Latch
        | envelfo.Threshold
        | envelfo.Slew,
    ]
    outputs: dict[str, str]
    events: list[envelfo.PatchEventConnection] = Field(default_factory=list)
    event_outputs: dict[str, str] = Field(default_factory=dict)
    inputs: list[str] = Field(default_factory=list)
    sample_keys: dict[str, int] = Field(default_factory=dict)
    connection_identities: list[str] = Field(default_factory=list)

    @cached_property
    def signal_order(self) -> list[str]:
        graph = {
            n: b.inputs
            if isinstance(b, (envelfo.Sum, envelfo.Product))
            else [b.input]
            if isinstance(
                b,
                (
                    envelfo.Affine,
                    envelfo.Quantize,
                    envelfo.Latch,
                    envelfo.Threshold,
                    envelfo.Slew,
                ),
            )
            else []
            for n, b in self.envelfos.items()
        }
        return list(TopologicalSorter(graph).static_order())


class PatchState(Model, frozen=True):
    at: control.Rational
    cursor: control.Rational
    name: str
    seed: int = Field(ge=0, lt=2**64)
    children: dict[str, envelfo.EnvelfoState]
    retained: dict[str, float] = Field(default_factory=dict)
    event_counts: dict[str, int] = Field(default_factory=dict)
    event_random: dict[str, int] = Field(default_factory=dict)
    pending: list[PendingEvent] = Field(default_factory=list)
    observed_at: control.Rational | None = None
    inputs: dict[str, float] = Field(default_factory=dict)


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
    body: envelfo.Patch | PreparedPatch,
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
    definition: envelfo.EnveLFO | PreparedPatch,
    at: Fraction,
    *,
    name: str = 'patch',
    seed: int = 0,
    tempo_map: control.TempoMap | None = None,
) -> PatchState:
    """Initialize child state without inventing note-on events or observations."""
    body = _body(definition)
    children = {
        n: runtime.initial_envelfo(
            _child(definition, n),
            _clock(definition, at, tempo_map, n),
            seed=seed
            ^ (
                definition.sample_keys.get(
                    n, envelfo_random.stream_key(0, f'patch-{name}-{n}:sample-hold')
                )
                if isinstance(definition, PreparedPatch)
                else envelfo_random.stream_key(0, f'patch-{name}-{n}:sample-hold')
            )
            if isinstance(b, envelfo.SampleHold)
            else 0,
        )
        for n, b in body.envelfos.items()
        if isinstance(
            b, (envelfo.LFO, envelfo.Envelope, envelfo.Stages, envelfo.SampleHold)
        )
        and not (isinstance(definition, PreparedPatch) and n in definition.inputs)
    }
    return PatchState(at=at, cursor=at, name=name, seed=seed, children=children)


def advance_patch(
    definition: envelfo.EnveLFO | PreparedPatch,
    state: PatchState,
    at: Fraction,
    *,
    next_delivery: Fraction,
    tempo_map: control.TempoMap | None = None,
    inputs: dict[str, float] | None = None,
) -> PatchAdvance:
    """Process boundaries/deliveries, then observe once on the host's schedule."""
    if next_delivery <= at:
        raise ValueError('next delivery must follow the observation')
    control.elapsed(at, state.at)
    state = _copy_state(state)
    if inputs is not None:
        state = state.model_copy(update={'inputs': dict(inputs)})
    state, emitted = _run(definition, state, at, tempo_map, True)
    state = state.model_copy(update={'at': at})
    signals = dict(state.inputs)
    for name, child_state in state.children.items():
        value = runtime.envelfo_at(
            _child(definition, name),
            child_state,
            _clock(definition, at, tempo_map, name),
        )
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
    definition: envelfo.EnveLFO | PreparedPatch,
    state: PatchState,
    target: str,
    event: envelfo.EnvelfoEvent,
    *,
    tempo_map: control.TempoMap | None = None,
    inclusive: bool = True,
) -> tuple[PatchState, list[envelfo.EnvelfoOutputEvent]]:
    """Deliver a child command in host seconds, without observing the graph."""
    control.elapsed(event.at, state.at)
    state = _copy_state(state)
    state, emitted = _run(definition, state, event.at, tempo_map, False)
    emitted.extend(_command(definition, state, target, event, tempo_map))
    if inclusive:
        state, output = _run(definition, state, event.at, tempo_map, True)
        emitted.extend(output)
    return state.model_copy(update={'at': event.at}), emitted


def settle_patch(
    definition: envelfo.EnveLFO | PreparedPatch,
    state: PatchState,
    at: Fraction,
    *,
    tempo_map: control.TempoMap | None = None,
    inclusive: bool = True,
) -> tuple[PatchState, list[envelfo.EnvelfoOutputEvent]]:
    """Process event phases without adding a host signal observation."""
    control.elapsed(at, state.at)
    state, events = _run(definition, _copy_state(state), at, tempo_map, inclusive)
    return state.model_copy(update={'at': at}), events


def next_patch_event(
    definition: envelfo.EnveLFO | PreparedPatch,
    state: PatchState,
    limit: Fraction,
    *,
    tempo_map: control.TempoMap | None = None,
) -> Fraction | None:
    """Query the next exact host boundary without advancing any child."""
    candidates, _ = _boundaries(definition, state, limit, tempo_map, True)
    return min(candidates, default=None)


def observe_block(
    definition: PreparedPatch, signals: dict[str, list[float]]
) -> dict[str, list[float]]:
    """Observe stateless operators over host-sampled event-free leaf spans."""
    if any(
        isinstance(b, (envelfo.Latch, envelfo.Threshold, envelfo.Slew))
        for b in definition.envelfos.values()
    ):
        raise ValueError('retained Patch operators require individual observations')
    if not signals:
        return {}
    output = {n: [] for n in definition.outputs}
    for i in range(len(next(iter(signals.values())))):
        values, _, _ = observe_signals(
            definition, {n: v[i] for n, v in signals.items()}, {}, Fraction(0)
        )
        for name, child in definition.outputs.items():
            output[name].append(values[child])
    return output


def _copy_state(state: PatchState) -> PatchState:
    # Scalar child runtimes are immutable. Only these scheduler-owned containers
    # are mutated, so copying them preserves rollback without copying each tree.
    return state.model_copy(
        update={
            'children': dict(state.children),
            'retained': dict(state.retained),
            'event_counts': dict(state.event_counts),
            'event_random': dict(state.event_random),
            'pending': list(state.pending),
            'inputs': dict(state.inputs),
        }
    )


def _body(definition: envelfo.EnveLFO | PreparedPatch) -> envelfo.Patch | PreparedPatch:
    if not isinstance(definition, (envelfo.Patch, PreparedPatch)):
        raise ValueError('Patch execution requires a materialized Patch')
    return definition


def _child(definition: envelfo.EnveLFO | PreparedPatch, name: str) -> envelfo.EnveLFO:
    body = _body(definition).envelfos[name]
    if not isinstance(
        body, (envelfo.LFO, envelfo.Envelope, envelfo.Stages, envelfo.SampleHold)
    ):
        raise ValueError('commands require an executable Patch child')
    return body


def _clock(
    definition: envelfo.EnveLFO | PreparedPatch,
    seconds: Fraction,
    tempo_map: control.TempoMap | None,
    child: str,
) -> Fraction:
    body = (
        _child(definition, child)
        if isinstance(definition, PreparedPatch)
        else definition
    )
    if body.clock == control.Clock.seconds:
        return seconds
    if tempo_map is None:
        raise ValueError('Beat-clock Patch requires a host tempo map')
    return (
        tempo_map.beat_at(seconds)
        if body.position_driver == 'transport'
        else tempo_map.elapsed_beats(Fraction(0), seconds)
    )


def _seconds(
    definition: envelfo.EnveLFO | PreparedPatch,
    at: Fraction,
    tempo_map: control.TempoMap | None,
    child: str,
) -> Fraction:
    body = (
        _child(definition, child)
        if isinstance(definition, PreparedPatch)
        else definition
    )
    if body.clock == control.Clock.seconds:
        return at
    assert tempo_map is not None
    seconds = tempo_map.time_for_elapsed_beat(at)
    if seconds is None:
        raise ValueError('Beat-clock Patch event has no host time')
    return seconds


def _route(
    definition: envelfo.EnveLFO | PreparedPatch,
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
        identity = (
            definition.connection_identities[order]
            if isinstance(definition, PreparedPatch)
            and definition.connection_identities
            else f'{target_name}:{command}'
        )
        key = str(order) if isinstance(definition, PreparedPatch) else identity
        count, word_state, forward = connection_gate(
            state.event_counts.get(key),
            state.event_random.get(key),
            connection.every,
            connection.offset,
            connection.probability,
            state.seed,
            identity,
        )
        state.event_counts[key] = count
        if word_state is not None:
            state.event_random[key] = word_state
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
    definition: envelfo.EnveLFO | PreparedPatch,
    state: PatchState,
    target: str,
    event: envelfo.EnvelfoEvent,
    tempo_map: control.TempoMap | None,
) -> list[envelfo.EnvelfoOutputEvent]:
    child = _child(definition, target)
    current = state.children[target]
    local = event.model_copy(
        update={'at': _clock(definition, event.at, tempo_map, target)}
    )
    emitted: list[envelfo.EnvelfoOutputEvent] = []
    if isinstance(child, envelfo.Stages):
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
                    else _seconds(definition, output.at, tempo_map, target),
                )
            )
    else:
        state.children[target] = runtime.envelfo_event(child, current, local)
    return emitted


def _run(
    definition: envelfo.EnveLFO | PreparedPatch,
    state: PatchState,
    limit: Fraction,
    tempo_map: control.TempoMap | None,
    inclusive: bool,
) -> tuple[PatchState, list[envelfo.EnvelfoOutputEvent]]:
    emitted: list[envelfo.EnvelfoOutputEvent] = []
    processed = 0
    while True:
        candidates, lfos = _boundaries(definition, state, limit, tempo_map, inclusive)
        if not candidates:
            break
        seconds = min(candidates)
        for name, current in list(state.children.items()):
            body = _body(definition).envelfos[name]
            if isinstance(body, envelfo.Stages):
                clock = _clock(definition, seconds, tempo_map, name)
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
                            at=_clock(definition, seconds, tempo_map, name),
                            port=port,
                            stage=name,
                            activation=0,
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
        state = state.model_copy(update={'at': seconds, 'cursor': seconds})
    for name, current in list(state.children.items()):
        body = _body(definition).envelfos[name]
        if isinstance(body, envelfo.Stages):
            # Exclusive advancement leaves same-time transitions to the command.
            if inclusive:
                result = runtime.advance_envelfo(
                    _child(definition, name),
                    current,
                    _clock(definition, limit, tempo_map, name),
                )
                state.children[name] = result.state
                assert not result.events
    if inclusive:
        state = state.model_copy(update={'cursor': limit})
    return state, emitted


def _boundaries(
    definition: envelfo.EnveLFO | PreparedPatch,
    state: PatchState,
    limit: Fraction,
    tempo_map: control.TempoMap | None,
    inclusive: bool,
) -> tuple[list[Fraction], list[tuple[Fraction, str, str]]]:
    candidates: list[Fraction] = [
        e.at for e in state.pending if e.at <= limit if inclusive or e.at < limit
    ]
    lfos: list[tuple[Fraction, str, str]] = []
    for name, current in state.children.items():
        body = _body(definition).envelfos[name]
        clock_limit = _clock(definition, limit, tempo_map, name)
        if isinstance(body, envelfo.Stages):
            assert isinstance(current.runtime, envelfo.StageState)
            boundary = runtime.next_stage_event(
                body, current.runtime, clock_limit, True
            )
            if boundary is not None:
                seconds = _seconds(definition, boundary[0], tempo_map, name)
                if state.cursor <= seconds <= limit and (inclusive or seconds < limit):
                    candidates.append(seconds)
        elif isinstance(body, envelfo.LFO) and body.markers:
            assert isinstance(current.runtime, envelfo.LFOState)
            position = current.runtime.position
            if position.paused or not position.rate:
                continue
            previous = max(
                position.at, _clock(definition, state.cursor, tempo_map, name)
            )
            coordinate = position.coordinate + position.direction * position.rate * (
                previous - position.at
            )
            for marker in body.markers:
                turn = (
                    floor(coordinate - marker.position) + 1
                    if position.direction == 1
                    else ceil(coordinate - marker.position) - 1
                )
                clock = position.at + (turn + marker.position - position.coordinate) / (
                    position.direction * position.rate
                )
                if clock > clock_limit:
                    continue
                seconds = _seconds(definition, clock, tempo_map, name)
                if seconds <= limit and (inclusive or seconds < limit):
                    lfos.append((seconds, name, marker.name))
                    candidates.append(seconds)
    return candidates, lfos
