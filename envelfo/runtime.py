"""NumPy-free scalar and staged Envelfo execution."""

from fractions import Fraction
from math import ceil, floor

from ufor import control, envelfo, envelope, lfo, oscillator, segments

from . import random


def latch_value(value: float, previous: float | None) -> float:
    """Capture on activation or when delivery has cleared the previous value."""
    return value if previous is None else previous


def slew_value(
    body: envelfo.Slew, value: float, previous: float | None, seconds: Fraction
) -> float:
    """Limit one observation in signal units per second, without overshoot."""
    if previous is None:
        return value if body.initial is None else body.initial
    elapsed = float(seconds)
    if value >= previous:
        return min(value, previous + body.rise * elapsed)
    return max(value, previous - body.fall * elapsed)


def threshold_value(
    body: envelfo.Threshold, value: float, previous: float | None
) -> tuple[float, str | None]:
    """Observe one grid sample; initialization is silent and deadband holds state."""
    output = float(value >= body.upper) if previous is None else previous
    if previous == 0 and value >= body.upper:
        return 1.0, 'rising'
    if previous == 1 and value <= body.lower:
        return 0.0, 'falling'
    return output, None


def transform_value(
    body: envelfo.Sum | envelfo.Product | envelfo.Affine | envelfo.Quantize,
    signals: dict[str, float],
) -> float:
    if isinstance(body, envelfo.Quantize):
        return envelfo.quantize_value(body, signals[body.input])
    if isinstance(body, envelfo.Affine):
        return body.offset + body.scale * signals[body.input]
    value = 0.0 if isinstance(body, envelfo.Sum) else 1.0
    for name in body.inputs:
        if isinstance(body, envelfo.Sum):
            value += signals[name]
        else:
            value *= signals[name]
    return value


def cycle_lfo(definition: envelfo.EnvelfoUse) -> lfo.LFO:
    body = definition.body
    if not isinstance(body, envelfo.Cycle):
        raise ValueError('envelfo is not a cycle')
    if isinstance(body.rate, envelfo.ParameterReference):
        raise ValueError('cycle rate has an unresolved public parameter')
    return lfo.LFO(
        clock=definition.clock,
        scope=definition.scope,
        rate=body.rate,
        phase=body.phase,
        reset=body.reset,
        waveform=body.shape,
        duty_cycle=body.duty_cycle,
        delay=body.delay,
        fade_in=body.fade_in,
    )


def contour_envelope(definition: envelfo.EnvelfoUse) -> envelope.Envelope:
    body = definition.body
    if not isinstance(body, envelfo.Contour):
        raise ValueError('envelfo is not a contour')
    if body.playback != envelfo.PlaybackMode.once:
        raise ValueError('looping contours cannot be converted to envelopes')
    if body.initial == 'current':
        raise ValueError('contour current value needs stage entry')
    return envelope.Envelope(
        clock=definition.clock,
        scope=definition.scope,
        polarity=body.polarity,
        initial=body.initial,
        segments=body.segments,
        release=body.release
        or [segments.Segment(duration=Fraction(0), to=body.segments[-1].to)],
        hold=body.hold if body.release else False,
        retrigger=body.retrigger,
    )


def initial_envelfo(
    definition: envelfo.EnvelfoUse, at: Fraction, *, seed: int = 0
) -> envelfo.EnvelfoState:
    if isinstance(definition.body, envelfo.SampleHold):
        random_state, word = random.random_word(seed)
        return envelfo.EnvelfoState(
            runtime=envelfo.SampleHoldState(
                at=at,
                value=definition.body.minimum
                + (definition.body.maximum - definition.body.minimum)
                * ((word >> 11) / 2**53),
                random_state=random_state,
            )
        )
    if isinstance(definition.body, envelfo.Cycle):
        assert isinstance(definition.body.rate, Fraction)
        return envelfo.EnvelfoState(
            runtime=envelfo.CycleState(
                at=at,
                position=envelfo.EnvelfoPosition(
                    at=at, coordinate=definition.body.phase, rate=definition.body.rate
                ),
            )
        )
    if isinstance(definition.body, envelfo.Stages):
        stage = next(
            s for s in definition.body.stages if s.name == definition.body.initial_stage
        )
        initial = (
            stage.envelfo.initial if isinstance(stage.envelfo, envelfo.Contour) else 0.0
        )
        assert isinstance(initial, float)
        return envelfo.EnvelfoState(
            runtime=envelfo.StageState(
                at=at,
                stage=stage.name,
                entry_value=initial,
                position=_initial_position(stage.envelfo, at),
            )
        )
    if not isinstance(definition.body, envelfo.Contour):
        raise ValueError('envelfo reference must be materialized before use')
    assert isinstance(definition.body.initial, float)
    return envelfo.EnvelfoState(
        runtime=envelfo.ContourState(
            at=at,
            phase=(
                'idle'
                if definition.body.release or definition.body.start == 'event'
                else 'on'
            ),
            start_value=definition.body.initial,
            position=_initial_position(definition.body, at).model_copy(
                update={
                    'coordinate': Fraction(1)
                    if not sum(s.duration for s in definition.body.segments)
                    else Fraction(0)
                }
            ),
        )
    )


def envelfo_at(
    definition: envelfo.EnvelfoUse, state: envelfo.EnvelfoState, at: Fraction
) -> envelfo.EnvelfoValue:
    if isinstance(definition.body, envelfo.SampleHold) and isinstance(
        state.runtime, envelfo.SampleHoldState
    ):
        if at < state.runtime.at:
            raise ValueError('sample-and-hold observation precedes its state')
        return envelfo.EnvelfoValue(
            value=state.runtime.value, weight=1.0, status='held'
        )
    if isinstance(definition.body, envelfo.Cycle) and isinstance(
        state.runtime, envelfo.CycleState
    ):
        position = state.runtime.position
        if definition.position_driver == envelfo.PositionDriver.transport:
            assert isinstance(definition.body.rate, Fraction)
            return envelfo.EnvelfoValue(
                value=definition.body.center
                + definition.body.depth
                * oscillator.shape_value(
                    definition.body.shape,
                    (definition.body.phase + definition.body.rate * at) % 1,
                    definition.body.duty_cycle,
                ),
                weight=1.0,
                status='running',
            )
        coordinate = _coordinate_at(position, at)
        age = _age_at(position, at)
        if age < definition.body.delay:
            weight = 0.0
        elif definition.body.fade_in:
            weight = float(
                min(
                    Fraction(1), (age - definition.body.delay) / definition.body.fade_in
                )
            )
        else:
            weight = 1.0
        return envelfo.EnvelfoValue(
            value=definition.body.center
            + definition.body.depth
            * oscillator.shape_value(
                definition.body.shape, coordinate % 1, definition.body.duty_cycle
            ),
            weight=weight,
            status='running',
        )
    if isinstance(definition.body, envelfo.Stages) and isinstance(
        state.runtime, envelfo.StageState
    ):
        return advance_envelfo(definition, state, at).value
    if isinstance(definition.body, envelfo.Contour) and isinstance(
        state.runtime, envelfo.ContourState
    ):
        return _contour_value(definition.body, state.runtime, at)
    raise ValueError('envelfo state does not match its definition')


def envelfo_event(
    definition: envelfo.EnvelfoUse,
    state: envelfo.EnvelfoState,
    event: envelfo.EnvelfoEvent,
) -> envelfo.EnvelfoState:
    if isinstance(definition.body, envelfo.SampleHold) and isinstance(
        state.runtime, envelfo.SampleHoldState
    ):
        control.check_order(state.runtime.at, state.runtime.ordinal, event)
        if event.action not in (
            'sample',
            'note_on',
            'note_off',
            'pause',
            'resume',
            'seek',
            'shift',
            'reverse',
        ):
            raise ValueError('sample-and-hold does not accept this event')
        runtime = state.runtime
        if event.action == 'sample':
            random_state, word = random.random_word(runtime.random_state)
            runtime = runtime.model_copy(
                update={
                    'value': definition.body.minimum
                    + (definition.body.maximum - definition.body.minimum)
                    * ((word >> 11) / 2**53),
                    'random_state': random_state,
                }
            )
        return envelfo.EnvelfoState(
            runtime=runtime.model_copy(
                update={'at': event.at, 'ordinal': event.ordinal}
            )
        )
    if definition.position_driver == envelfo.PositionDriver.transport:
        raise ValueError('transport-position Cycle does not accept phase commands')
    if isinstance(definition.body, envelfo.Stages) and isinstance(
        state.runtime, envelfo.StageState
    ):
        return advance_envelfo(definition, state, event.at, event).state
    if isinstance(definition.body, envelfo.Cycle) and isinstance(
        state.runtime, envelfo.CycleState
    ):
        if event.action == 'cue':
            raise ValueError('cycles do not accept cue events')
        if event.action == 'sample':
            raise ValueError('cycles do not accept sample events')
        control.check_order(state.runtime.at, state.runtime.ordinal, event)
        position = _position_at(state.runtime.position, event.at)
        if event.action in ('note_on', 'reset', 'transport'):
            if (
                event.action == 'reset'
                or (
                    event.action == 'note_on'
                    and definition.body.reset == lfo.Reset.trigger
                )
                or (
                    event.action == 'transport'
                    and definition.body.reset == lfo.Reset.transport
                )
            ):
                position = position.model_copy(
                    update={'coordinate': definition.body.phase, 'age': Fraction(0)}
                )
        else:
            position = _position_command(position, event)
        return envelfo.EnvelfoState(
            runtime=envelfo.CycleState(
                at=event.at, ordinal=event.ordinal, position=position
            )
        )
    if isinstance(definition.body, envelfo.Contour) and isinstance(
        state.runtime, envelfo.ContourState
    ):
        if event.action in ('cue', 'reset', 'transport', 'rate', 'sample'):
            raise ValueError('contours do not accept this event')
        control.check_order(state.runtime.at, state.runtime.ordinal, event)
        runtime = state.runtime
        position = _position_at(runtime.position, event.at)
        phase = runtime.phase
        start_value = runtime.start_value
        traversals = runtime.traversals
        complete_coordinate = runtime.complete_coordinate
        if (
            phase == 'on'
            and complete_coordinate is None
            and definition.body.repeat_count is not None
        ):
            crossed, boundary = _repeat_crossings(
                definition.body,
                runtime.position,
                event.at,
                definition.body.repeat_count - traversals,
            )
            traversals += crossed
            if boundary is not None:
                position = position.model_copy(update={'coordinate': boundary})
                complete_coordinate = _repeat_boundary_coordinate(
                    definition.body, boundary, runtime.position.direction
                )
        observed = _contour_value(definition.body, runtime, event.at)
        if event.action == 'start' or (
            event.action == 'note_on' and definition.body.start == 'activation'
        ):
            active = observed.status in ('running', 'held')
            if not active or definition.body.retrigger != envelope.Retrigger.ignore:
                assert isinstance(definition.body.initial, float)
                start_value = (
                    observed.value
                    if definition.body.retrigger == envelope.Retrigger.current
                    else definition.body.initial
                )
                phase = 'on'
                traversals = 0
                complete_coordinate = None
                position = position.model_copy(
                    update={
                        'coordinate': Fraction(0),
                        'age': Fraction(0),
                        'rate': _contour_rate(definition.body.segments),
                    }
                )
        elif event.action == 'note_off':
            if (
                definition.body.release
                and phase == 'on'
                and observed.status != 'complete'
            ):
                phase = 'release'
                start_value = observed.value
                position = position.model_copy(
                    update={
                        'coordinate': Fraction(0),
                        'age': Fraction(0),
                        'rate': _contour_rate(definition.body.release),
                    }
                )
        else:
            position = _position_command(position, event)
            if event.action == 'shift' and (
                phase == 'release'
                or definition.body.playback == envelfo.PlaybackMode.once
            ):
                position = position.model_copy(
                    update={
                        'coordinate': max(
                            Fraction(0), min(Fraction(1), position.coordinate)
                        )
                    }
                )
        return envelfo.EnvelfoState(
            runtime=envelfo.ContourState(
                at=event.at,
                ordinal=event.ordinal,
                phase=phase,
                start_value=start_value,
                position=position,
                traversals=traversals,
                complete_coordinate=complete_coordinate,
            )
        )
    raise ValueError('envelfo state does not match its definition')


def advance_envelfo(
    definition: envelfo.EnvelfoUse,
    state: envelfo.EnvelfoState,
    at: Fraction,
    event: envelfo.EnvelfoEvent | None = None,
) -> envelfo.EnvelfoAdvance:
    """Advance a staged Envelfo, returning its state and ordered output events."""
    body = definition.body
    runtime = state.runtime
    if not isinstance(body, envelfo.Stages) or not isinstance(
        runtime, envelfo.StageState
    ):
        raise ValueError('advance_envelfo requires staged Envelfo state')
    control.elapsed(at, runtime.at)
    if event is not None:
        if event.at != at:
            raise ValueError('Envelfo event time must equal the advance boundary')
        control.check_order(runtime.at, runtime.ordinal, event)
    current, emitted = _advance_stages(body, runtime, at, event is None)
    if event is not None:
        if event.action in ('pause', 'resume', 'reverse', 'seek', 'shift', 'rate'):
            stage = next(s.envelfo for s in body.stages if s.name == current.stage)
            if event.action in ('seek', 'shift') and isinstance(stage, envelfo.Hold):
                raise ValueError('hold stages do not accept position events')
            position = _position_command(_position_at(current.position, at), event)
            if (
                event.action == 'shift'
                and isinstance(stage, envelfo.Contour)
                and stage.playback == envelfo.PlaybackMode.once
            ):
                position = position.model_copy(
                    update={
                        'coordinate': max(
                            Fraction(0), min(Fraction(1), position.coordinate)
                        )
                    }
                )
            current = current.model_copy(
                update={
                    'position': position,
                    'cursor_order': -1,
                }
            )
        else:
            key = f'cue.{event.cue}' if event.action == 'cue' else event.action
            activation = current.activation
            current, output = _stage_transition(body, current, at, key)
            emitted.extend(output)
            if current.activation == activation and not current.completed:
                current, output = _advance_stages(body, current, at, True)
                emitted.extend(output)
        current = current.model_copy(
            update={
                'at': at,
                'ordinal': event.ordinal,
                'cursor_order': current.cursor_order,
            }
        )
    value = _stage_value(body, current, at)
    return envelfo.EnvelfoAdvance(
        state=envelfo.EnvelfoState(runtime=current), value=value, events=emitted
    )


def next_stage_event(
    body: envelfo.Stages,
    state: envelfo.StageState,
    limit: Fraction,
    inclusive: bool = True,
) -> tuple[Fraction, int, str] | None:
    """Find the next natural boundary without advancing state or emitting events."""
    if state.completed:
        return None
    stage = next(s.envelfo for s in body.stages if s.name == state.stage)
    if isinstance(stage, envelfo.Hold):
        return None
    candidates: list[tuple[Fraction, int, str]] = []
    position = state.position
    if position.paused or not position.rate:
        return None
    start = position.coordinate
    forward = position.direction == 1
    if isinstance(stage, envelfo.Contour):
        if stage.playback == envelfo.PlaybackMode.once:
            for index, marker in enumerate(stage.markers):
                target = marker.position
                if (target > start if forward else target < start) or (
                    target == start
                    and state.cursor_order >= 0
                    and (
                        index > state.cursor_order
                        if forward
                        else index < state.cursor_order
                    )
                ):
                    time = state.at + abs(target - start) / position.rate
                    candidates.append((time, index, marker.name))
            if forward and (
                start < 1
                or (start == 1 and 0 <= state.cursor_order < len(stage.markers))
            ):
                candidates.append(
                    (state.at + (1 - start) / position.rate, len(stage.markers), 'done')
                )
        elif stage.loop_start is not None:
            candidates.extend(_named_loop_candidates(stage, state))
        else:
            period = 1 if stage.playback == envelfo.PlaybackMode.loop else 2
            for index, marker in enumerate(stage.markers):
                offsets = (
                    [marker.position]
                    if period == 1
                    else [marker.position, 2 - marker.position]
                )
                for offset in offsets:
                    turn = (
                        floor((start - offset) / period) + 1
                        if forward
                        else ceil((start - offset) / period) - 1
                    )
                    target = turn * period + offset
                    candidates.append(
                        (
                            state.at + abs(target - start) / position.rate,
                            index,
                            marker.name,
                        )
                    )
                    if (
                        state.cursor_order >= 0
                        and (start - offset) % period == 0
                        and (
                            index > state.cursor_order
                            if forward
                            else index < state.cursor_order
                        )
                    ):
                        candidates.append((state.at, index, marker.name))
            boundary = floor(start) + 1 if forward else ceil(start) - 1
            order = len(stage.markers)
            candidates.append(
                (
                    state.at + abs(boundary - start) / position.rate,
                    order,
                    'cycle' if period == 1 else 'turned',
                )
            )
            if period == 2 and boundary % 2 == 0:
                candidates.append(
                    (
                        state.at + abs(boundary - start) / position.rate,
                        order + 1,
                        'cycle',
                    )
                )
            if period == 2 and start.denominator == 1 and start % 2 == 0:
                if state.cursor_order == order:
                    candidates.append((state.at, order + 1, 'cycle'))
    else:
        assert isinstance(stage, envelfo.Cycle)
        for index, marker in enumerate(stage.markers):
            offset = start - marker.position
            turn = floor(offset) + 1 if forward else ceil(offset) - 1
            target = turn + marker.position
            time = state.at + abs(target - start) / position.rate
            candidates.append((time, index, marker.name))
            if (
                state.cursor_order >= 0
                and offset.denominator == 1
                and (
                    index > state.cursor_order
                    if forward
                    else index < state.cursor_order
                )
            ):
                candidates.append((state.at, index, marker.name))
    available = [
        c for c in candidates if (c[0] <= limit if inclusive else c[0] < limit)
    ]
    if not available:
        return None
    return (
        min(available)
        if forward
        else min(
            available,
            key=lambda c: (
                c[0],
                c[1] if c[2] in ('turned', 'cycle') else -c[1],
            ),
        )
    )


def _contour_rate(segments: list[segments.Segment]) -> Fraction:
    duration = sum((s.duration for s in segments), Fraction(0))
    return Fraction(1, 1) / duration if duration else Fraction(0)


def _loop_bounds(body: envelfo.Contour) -> tuple[Fraction, Fraction]:
    if body.loop_start is None or body.loop_end is None:
        return Fraction(0), Fraction(1)
    positions = {m.name: m.position for m in body.markers}
    return positions[body.loop_start], positions[body.loop_end]


def _contour_coordinate(
    coordinate: Fraction, body: envelfo.Contour, release: bool = False
) -> Fraction:
    mode = envelfo.PlaybackMode.once if release else body.playback
    start, end = _loop_bounds(body)
    if start or end != 1:
        if coordinate < end:
            return max(Fraction(0), coordinate)
        width = end - start
        if mode == envelfo.PlaybackMode.loop:
            return start + (coordinate - end) % width
        if mode == envelfo.PlaybackMode.ping_pong:
            position = (coordinate - end) % (2 * width)
            return end - position if position <= width else start + position - width
    if mode == envelfo.PlaybackMode.loop:
        return coordinate % 1
    if mode == envelfo.PlaybackMode.ping_pong:
        position = coordinate % 2
        return position if position <= 1 else 2 - position
    return max(Fraction(0), min(Fraction(1), coordinate))


def _repeat_crossings(
    body: envelfo.Contour,
    position: envelfo.EnvelfoPosition,
    at: Fraction,
    remaining: int,
) -> tuple[int, Fraction | None]:
    start, end = _loop_bounds(body)
    width = end - start
    coordinate = position.coordinate
    target = _coordinate_at(position, at)
    if position.paused or not position.rate or target == coordinate:
        return 0, None
    if position.direction == 1:
        first = (
            end + max(0, floor((coordinate - end) / width) + 1) * width
            if body.loop_start is not None
            else floor(coordinate) + 1
        )
        crossed = max(0, floor((target - first) / width) + 1)
        boundary = first + (remaining - 1) * width
    else:
        first = (
            end + (ceil((coordinate - end) / width) - 1) * width
            if body.loop_start is not None
            else ceil(coordinate) - 1
        )
        crossed = (
            max(0, floor((first - target) / width) + 1)
            if body.loop_start is None or first >= end
            else 0
        )
        boundary = first - (remaining - 1) * width
    return min(crossed, remaining), boundary if crossed >= remaining else None


def _repeat_boundary_coordinate(
    body: envelfo.Contour, boundary: Fraction, direction: int
) -> Fraction:
    if body.playback == envelfo.PlaybackMode.loop:
        start, end = _loop_bounds(body)
        return end if direction == 1 else start
    return _contour_coordinate(boundary, body)


def _initial_position(
    body: envelfo.Hold | envelfo.Cycle | envelfo.Contour, at: Fraction
) -> envelfo.EnvelfoPosition:
    if isinstance(body, envelfo.Cycle):
        assert isinstance(body.rate, Fraction)
        return envelfo.EnvelfoPosition(at=at, coordinate=body.phase, rate=body.rate)
    if isinstance(body, envelfo.Contour):
        return envelfo.EnvelfoPosition(
            at=at, coordinate=Fraction(0), rate=_contour_rate(body.segments)
        )
    return envelfo.EnvelfoPosition(at=at, coordinate=Fraction(0), rate=Fraction(0))


def _coordinate_at(position: envelfo.EnvelfoPosition, at: Fraction) -> Fraction:
    elapsed = control.elapsed(at, position.at)
    if position.paused:
        return position.coordinate
    return position.coordinate + position.direction * position.rate * elapsed


def _age_at(position: envelfo.EnvelfoPosition, at: Fraction) -> Fraction:
    elapsed = control.elapsed(at, position.at)
    return position.age if position.paused else position.age + elapsed


def _position_at(
    position: envelfo.EnvelfoPosition, at: Fraction
) -> envelfo.EnvelfoPosition:
    return position.model_copy(
        update={
            'at': at,
            'coordinate': _coordinate_at(position, at),
            'age': _age_at(position, at),
        }
    )


def _position_command(
    position: envelfo.EnvelfoPosition, event: envelfo.EnvelfoEvent
) -> envelfo.EnvelfoPosition:
    if event.action == 'pause':
        return position.model_copy(update={'paused': True})
    if event.action == 'resume':
        return position.model_copy(update={'paused': False})
    if event.action == 'reverse':
        return position.model_copy(update={'direction': -position.direction})
    if event.action == 'seek':
        assert event.position is not None
        return position.model_copy(update={'coordinate': event.position})
    if event.action == 'shift':
        assert event.offset is not None
        return position.model_copy(
            update={'coordinate': position.coordinate + event.offset}
        )
    if event.action == 'rate':
        assert event.rate is not None
        return position.model_copy(update={'rate': event.rate})
    return position


def _contour_value(
    body: envelfo.Contour, state: envelfo.ContourState, at: Fraction
) -> envelfo.EnvelfoValue:
    if state.phase == 'idle':
        assert isinstance(body.initial, float)
        return envelfo.EnvelfoValue(value=body.initial, weight=1, status='idle')
    segments = body.release if state.phase == 'release' else body.segments
    duration = sum((s.duration for s in segments), Fraction(0))
    complete_coordinate = state.complete_coordinate
    if (
        complete_coordinate is None
        and state.phase == 'on'
        and body.repeat_count is not None
    ):
        _, boundary = _repeat_crossings(
            body, state.position, at, body.repeat_count - state.traversals
        )
        if boundary is not None:
            complete_coordinate = _repeat_boundary_coordinate(
                body, boundary, state.position.direction
            )
    coordinate = (
        complete_coordinate
        if complete_coordinate is not None
        else _contour_coordinate(
            _coordinate_at(state.position, at), body, state.phase != 'on'
        )
        if duration
        else Fraction(1)
    )
    value = envelope.curve_at(
        envelope.Curve(initial=state.start_value, segments=segments),
        coordinate * duration,
    )
    if complete_coordinate is not None:
        status = 'complete'
    elif state.phase == 'on' and body.playback != envelfo.PlaybackMode.once:
        status = 'running'
    elif coordinate < 1:
        status = 'running'
    elif state.phase == 'on' and body.hold and body.release:
        status = 'held'
    else:
        status = 'complete'
    return envelfo.EnvelfoValue(value=value, weight=1, status=status)


def _advance_stages(
    body: envelfo.Stages, state: envelfo.StageState, at: Fraction, inclusive: bool
) -> tuple[envelfo.StageState, list[envelfo.EnvelfoOutputEvent]]:
    emitted: list[envelfo.EnvelfoOutputEvent] = []
    while (natural := next_stage_event(body, state, at, inclusive)) is not None:
        time, order, port = natural
        stage = state.stage
        activation = state.activation
        state = state.model_copy(
            update={
                'at': time,
                'ordinal': state.ordinal if state.at == time else -1,
                'cursor_order': order,
                'position': _position_at(state.position, time),
            }
        )
        contour = next(s.envelfo for s in body.stages if s.name == stage)
        if (
            isinstance(contour, envelfo.Contour)
            and contour.repeat_count is not None
            and port
            == ('cycle' if contour.playback == envelfo.PlaybackMode.loop else 'turned')
        ):
            state = state.model_copy(update={'traversals': state.traversals + 1})
        final_repeat = (
            isinstance(contour, envelfo.Contour)
            and contour.repeat_count is not None
            and state.traversals == contour.repeat_count
            and port
            == ('cycle' if contour.playback == envelfo.PlaybackMode.loop else 'turned')
        )
        emitted.append(
            envelfo.EnvelfoOutputEvent(
                at=time,
                port='stage.done' if port == 'done' else port,
                stage=stage,
                activation=activation,
            )
        )
        if final_repeat:
            assert isinstance(contour, envelfo.Contour)
            start, end = _loop_bounds(contour)
            if (
                contour.playback == envelfo.PlaybackMode.ping_pong
                and (state.position.coordinate - end) / (end - start) % 2 == 1
            ):
                emitted.append(
                    envelfo.EnvelfoOutputEvent(
                        at=time, port='cycle', stage=stage, activation=activation
                    )
                )
            emitted.append(
                envelfo.EnvelfoOutputEvent(
                    at=time, port='stage.done', stage=stage, activation=activation
                )
            )
        state, output = _stage_transition(body, state, time, f'stage.{port}')
        emitted.extend(output)
        if final_repeat and state.stage == stage and state.activation == activation:
            state, output = _stage_transition(body, state, time, 'stage.done')
            emitted.extend(output)
            if state.stage == stage and state.activation == activation:
                value = _stage_value(body, state, time).value
                state = state.model_copy(
                    update={'completed': True, 'complete_value': value}
                )
        if len(emitted) > 4096:
            raise ValueError('Envelfo event capacity exceeded')
    if inclusive:
        state = state.model_copy(
            update={
                'at': at,
                'ordinal': state.ordinal if state.at == at else -1,
                'cursor_order': -1 if state.at != at else state.cursor_order,
                'position': _position_at(state.position, at),
            }
        )
    return state, emitted


def _named_loop_candidates(
    stage: envelfo.Contour, state: envelfo.StageState
) -> list[tuple[Fraction, int, str]]:
    start, end = _loop_bounds(stage)
    width = end - start
    position = state.position
    coordinate = position.coordinate
    forward = position.direction == 1
    period = width if stage.playback == envelfo.PlaybackMode.loop else 2 * width
    candidates: list[tuple[Fraction, int, str]] = []

    def add(target: Fraction, order: int, port: str) -> None:
        if (target > coordinate if forward else target < coordinate) or (
            target == coordinate
            and state.cursor_order >= 0
            and (
                order > state.cursor_order
                if forward
                else (
                    state.cursor_order < len(stage.markers)
                    and (order < state.cursor_order or order == len(stage.markers))
                )
            )
        ):
            candidates.append(
                (state.at + abs(target - coordinate) / position.rate, order, port)
            )

    def repeat(base: Fraction, order: int, port: str) -> None:
        turn = (
            max(0, floor((coordinate - base) / period) + 1)
            if forward
            else ceil((coordinate - base) / period) - 1
        )
        if turn >= 0:
            add(base + turn * period, order, port)
        if coordinate >= base and (coordinate - base) % period == 0:
            add(coordinate, order, port)

    for order, marker in enumerate(stage.markers):
        add(marker.position, order, marker.name)
        if marker.position < start:
            continue
        if stage.playback == envelfo.PlaybackMode.loop:
            base = end + (marker.position - start)
            repeat(base, order, marker.name)
        else:
            if marker.position < end:
                repeat(end + end - marker.position, order, marker.name)
            if marker.position > start:
                repeat(end + width + marker.position - start, order, marker.name)

    order = len(stage.markers)
    boundary = (
        max(0, floor((coordinate - end) / width) + 1)
        if forward
        else ceil((coordinate - end) / width) - 1
    )
    if boundary >= 0:
        target = end + boundary * width
        add(
            target,
            order,
            'cycle' if stage.playback == envelfo.PlaybackMode.loop else 'turned',
        )
        if stage.playback == envelfo.PlaybackMode.ping_pong and boundary % 2 == 1:
            add(target, order + 1, 'cycle')
    if coordinate >= end and (coordinate - end) % width == 0:
        boundary = int((coordinate - end) / width)
        add(
            coordinate,
            order,
            'cycle' if stage.playback == envelfo.PlaybackMode.loop else 'turned',
        )
        if stage.playback == envelfo.PlaybackMode.ping_pong and boundary % 2 == 1:
            if state.cursor_order == order:
                candidates.append((state.at, order + 1, 'cycle'))
    return candidates


def _stage_transition(
    body: envelfo.Stages, state: envelfo.StageState, at: Fraction, event: str
) -> tuple[envelfo.StageState, list[envelfo.EnvelfoOutputEvent]]:
    if state.completed:
        return state, []
    transition = next(
        (
            t
            for t in body.transitions
            if state.stage in t.from_stages and t.event == event
        ),
        None,
    )
    if transition is None:
        return state, []
    value = _stage_value(body, state, at).value
    if isinstance(transition.action, envelfo.EnterStage):
        target = next(
            s.envelfo for s in body.stages if s.name == transition.action.stage
        )
        entry = (
            value
            if isinstance(target, envelfo.Contour) and target.initial == 'current'
            else target.initial
            if isinstance(target, envelfo.Contour)
            else value
        )
        assert isinstance(entry, float)
        return (
            envelfo.StageState(
                at=at,
                ordinal=state.ordinal,
                stage=transition.action.stage,
                entry_value=entry,
                position=_initial_position(target, at).model_copy(
                    update={
                        'paused': state.position.paused,
                        'direction': state.position.direction,
                    }
                ),
                activation=state.activation + 1,
            ),
            [],
        )
    return (
        state.model_copy(update={'at': at, 'completed': True, 'complete_value': value}),
        [
            envelfo.EnvelfoOutputEvent(
                at=at, port='done', stage=state.stage, activation=state.activation
            )
        ],
    )


def _stage_value(
    body: envelfo.Stages, state: envelfo.StageState, at: Fraction
) -> envelfo.EnvelfoValue:
    if state.completed:
        assert state.complete_value is not None
        return envelfo.EnvelfoValue(
            value=state.complete_value, weight=1, status='complete'
        )
    stage = next(s.envelfo for s in body.stages if s.name == state.stage)
    if isinstance(stage, envelfo.Hold):
        return envelfo.EnvelfoValue(value=stage.value, weight=1, status='held')
    position = state.position
    coordinate = _coordinate_at(position, at)
    if isinstance(stage, envelfo.Cycle):
        phase = coordinate % 1
        age = _age_at(position, at)
        if age < stage.delay:
            weight = 0.0
        elif stage.fade_in:
            weight = float(min(Fraction(1), (age - stage.delay) / stage.fade_in))
        else:
            weight = 1.0
        value = stage.center + stage.depth * weight * oscillator.shape_value(
            stage.shape, phase, stage.duty_cycle
        )
        return envelfo.EnvelfoValue(value=value, weight=1, status='running')
    curve = envelope.Curve(initial=state.entry_value, segments=stage.segments)
    duration = sum((s.duration for s in stage.segments), Fraction(0))
    bounded = _contour_coordinate(coordinate, stage)
    complete = stage.repeat_count is not None and state.traversals >= stage.repeat_count
    if complete:
        bounded = _repeat_boundary_coordinate(stage, coordinate, position.direction)
    value = envelope.curve_at(curve, bounded * duration)
    return envelfo.EnvelfoValue(
        value=value,
        weight=1,
        status='complete'
        if complete
        else 'running'
        if stage.playback != envelfo.PlaybackMode.once or bounded < 1
        else 'held',
    )
