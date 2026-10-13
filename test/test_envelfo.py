from fractions import Fraction

import pytest
from pydantic import ValidationError
from ufor.codec import parse_score, score_toml
from ufor.envelfo import (
    LFO,
    Affine,
    Contour,
    EnterStage,
    EnvelfoEvent,
    EnvelfoScore,
    EnvelfoState,
    EnvelfoUse,
    FinishStage,
    Hold,
    Marker,
    Patch,
    PlaybackMode,
    Product,
    SampleHold,
    SampleHoldState,
    Stage,
    Stages,
    StageState,
    StageTransition,
    Sum,
    instantiate_envelfo,
)
from ufor.segments import Segment

from envelfo.runtime import (
    advance_envelfo,
    contour_envelope,
    envelfo_at,
    envelfo_event,
    initial_envelfo,
    next_stage_event,
    transform_value,
)


def staged_envelfo() -> EnvelfoUse:
    return EnvelfoUse(
        body=Stages(
            initial_stage='waiting',
            stages=[
                Stage(name='waiting', envelfo=Hold(value=0.0)),
                Stage(
                    name='attack',
                    envelfo=Contour(
                        initial='current',
                        segments=[Segment(duration=Fraction(1, 4), to=0.7)],
                    ),
                ),
                Stage(
                    name='sway',
                    envelfo=LFO(
                        rate=Fraction(2),
                        center=0.7,
                        depth=0.2,
                        markers=[Marker(name='peak', position=Fraction(1, 4))],
                    ),
                ),
                Stage(
                    name='release',
                    envelfo=Contour(
                        initial='current',
                        segments=[Segment(duration=Fraction(1, 4), to=0.0)],
                    ),
                ),
            ],
            transitions=[
                StageTransition(
                    from_stages=['waiting', 'attack', 'sway', 'release'],
                    event='note_on',
                    action=EnterStage(stage='attack'),
                ),
                StageTransition(
                    from_stages=['attack'],
                    event='stage.done',
                    action=EnterStage(stage='sway'),
                ),
                StageTransition(
                    from_stages=['attack', 'sway'],
                    event='note_off',
                    action=EnterStage(stage='release'),
                ),
                StageTransition(
                    from_stages=['release'],
                    event='stage.done',
                    action=FinishStage(),
                ),
                StageTransition(
                    from_stages=['waiting'],
                    event='cue.sway',
                    action=EnterStage(stage='sway'),
                ),
            ],
        )
    )


@pytest.mark.parametrize('kind', ['hold', 'lfo', 'contour'])
def test_next_stage_boundary_query_is_exact_and_does_not_advance_state(
    kind: str,
) -> None:
    child = (
        Hold(value=0.5)
        if kind == 'hold'
        else LFO(rate=2, markers=[Marker(name='pulse', position=Fraction(1, 2))])
        if kind == 'lfo'
        else Contour(segments=[Segment(duration=Fraction(1, 4), to=1)])
    )
    body = Stages(initial_stage='source', stages=[Stage(name='source', envelfo=child)])
    envelfo = EnvelfoUse(body=body)
    state = initial_envelfo(envelfo, Fraction(0))
    assert isinstance(state.runtime, StageState)
    saved = state.model_dump_json()
    assert next_stage_event(body, state.runtime, Fraction(1, 8)) is None
    boundary = next_stage_event(body, state.runtime, Fraction(1, 4))
    if kind == 'hold':
        assert boundary is None
    else:
        assert boundary == (Fraction(1, 4), 0, 'pulse' if kind == 'lfo' else 'done')
        assert next_stage_event(body, state.runtime, Fraction(1, 4), False) is None
        result = advance_envelfo(envelfo, state, Fraction(1, 4))
        assert result.events[0].at == boundary[0]
        assert result.events[0].port == ('pulse' if kind == 'lfo' else 'stage.done')
    assert state.model_dump_json() == saved


def test_transport_position_lfo_follows_song_beats_across_seek() -> None:
    envelfo = EnvelfoUse.model_validate(
        {
            'clock': 'beats',
            'position_driver': 'transport',
            'body': {
                'kind': 'lfo',
                'shape': 'triangle',
                'rate': '1/4',
                'reset': 'transport',
            },
        }
    )
    state = initial_envelfo(envelfo, Fraction(0))
    assert envelfo_at(envelfo, state, Fraction(0)).value == -1
    assert envelfo_at(envelfo, state, Fraction(1)).value == 0
    assert envelfo_at(envelfo, state, Fraction(2)).value == 1
    assert envelfo_at(envelfo, state, Fraction(1)).value == 0
    with pytest.raises(ValueError, match='does not accept phase commands'):
        envelfo_event(
            envelfo, state, EnvelfoEvent(at=Fraction(1), ordinal=0, action='reset')
        )
    with pytest.raises(ValidationError, match='transport position requires'):
        EnvelfoUse.model_validate(
            {
                'clock': 'beats',
                'position_driver': 'transport',
                'body': {'kind': 'lfo', 'rate': '1'},
            }
        )
    reference = EnvelfoUse.model_validate(
        {
            'clock': 'beats',
            'position_driver': 'transport',
            'score': {'selector': 'pulse'},
        }
    )
    score = EnvelfoScore(
        name='pulse',
        title='Pulse',
        body=LFO(rate=Fraction(1, 4), reset='transport'),
    )
    resolved = instantiate_envelfo(
        score, clock=reference.clock, position_driver=reference.position_driver
    )
    assert resolved.position_driver == reference.position_driver


def test_sample_hold_draws_only_on_sample_events_and_restores_its_stream() -> None:
    envelfo = EnvelfoUse(body=SampleHold(minimum=0.2, maximum=0.8))
    state = initial_envelfo(envelfo, Fraction(0), seed=123)
    assert isinstance(state.runtime, SampleHoldState)
    first = envelfo_at(envelfo, state, Fraction(10))
    assert 0.2 <= first.value < 0.8
    assert first.weight == 1
    for i, fields in enumerate(
        [
            {'action': 'note_on'},
            {'action': 'note_off'},
            {'action': 'reverse'},
            {'action': 'seek', 'position': '1/4'},
            {'action': 'pause'},
            {'action': 'resume'},
        ]
    ):
        state = envelfo_event(
            envelfo,
            state,
            EnvelfoEvent.model_validate({'at': str(i), 'ordinal': 0, **fields}),
        )
        assert envelfo_at(envelfo, state, Fraction(i)).value == first.value
    snapshot = EnvelfoState.model_validate_json(state.model_dump_json())
    event = EnvelfoEvent(at=Fraction(6), ordinal=0, action='sample')
    second = envelfo_event(envelfo, state, event)
    assert envelfo_at(envelfo, second, Fraction(6)).value != first.value
    assert envelfo_event(envelfo, snapshot, event) == second
    assert initial_envelfo(envelfo, Fraction(0), seed=123) != initial_envelfo(
        envelfo, Fraction(0), seed=124
    )


def test_sample_hold_with_equal_bounds_is_constant_but_advances_draws() -> None:
    envelfo = EnvelfoUse(body=SampleHold(minimum=0.5, maximum=0.5))
    initial = initial_envelfo(envelfo, Fraction(0))
    sampled = envelfo_event(
        envelfo, initial, EnvelfoEvent(at=Fraction(1), ordinal=0, action='sample')
    )
    assert initial.runtime != sampled.runtime
    assert envelfo_at(envelfo, sampled, Fraction(1)).value == 0.5


def test_patch_transforms_follow_dependencies_without_clipping() -> None:
    patch = Patch(
        envelfos={
            'output': Affine(input='combined', scale=-0.5, offset=0.25),
            'combined': Sum(inputs=['shaped', 'carrier']),
            'shaped': Product(inputs=['carrier', 'envelope']),
            'carrier': LFO(rate=1),
            'envelope': Contour(segments=[Segment(duration=1, to=1)]),
        },
        outputs={'signal': 'output'},
    )
    values = {'carrier': 1.0, 'envelope': 0.5}
    for name in patch.signal_order:
        if isinstance(body := patch.envelfos[name], (Sum, Product, Affine)):
            values[name] = transform_value(body, values)
    assert values['combined'] == 1.5
    assert values['output'] == -0.5
    assert patch.signal_ranges['output'] == (-0.75, 1.25)
    assert Patch.model_validate_json(patch.model_dump_json()) == patch


def test_patch_marker_can_start_waiting_contour() -> None:
    body = {
        'kind': 'patch',
        'envelfos': {
            'clock': {
                'kind': 'lfo',
                'rate': '2',
                'markers': [{'name': 'peak', 'position': '1/4'}],
            },
            'accent': {
                'kind': 'contour',
                'start': 'event',
                'segments': [{'duration': '1/4 s', 'to': 1}],
            },
        },
        'events': [{'source': 'clock.peak', 'target': 'accent', 'action': 'start'}],
        'outputs': {'value': 'accent'},
    }
    score = EnvelfoScore.model_validate(
        {'name': 'accent', 'title': 'Accent', 'body': body}
    )
    assert parse_score(score_toml(score)) == score
    contour = EnvelfoUse(body=score.body.envelfos['accent'])
    state = initial_envelfo(contour, Fraction(0))
    assert envelfo_at(contour, state, Fraction(1)).value == 0
    state = envelfo_event(
        contour,
        state,
        EnvelfoEvent(at=Fraction(1, 8), ordinal=0, action='start'),
    )
    assert envelfo_at(contour, state, Fraction(1, 4)).value == 0.5
    body['events'][0]['every'] = 3
    body['events'][0]['offset'] = 2
    body['events'][0]['probability'] = 0.5
    body['events'][0]['delay'] = '1/64'
    divided = EnvelfoScore.model_validate(
        {'name': 'accent', 'title': 'Accent', 'body': body}
    )
    assert parse_score(score_toml(divided)) == divided
    for every in (0, -1, 1.5, True):
        body['events'][0]['every'] = every
        with pytest.raises(ValidationError):
            EnvelfoScore.model_validate(
                {'name': 'accent', 'title': 'Accent', 'body': body}
            )
    body['events'][0]['every'] = 1
    for offset in (-1, 1.5, True):
        body['events'][0]['offset'] = offset
        with pytest.raises(ValidationError):
            EnvelfoScore.model_validate(
                {'name': 'accent', 'title': 'Accent', 'body': body}
            )
    body['events'][0]['offset'] = 4
    EnvelfoScore.model_validate({'name': 'accent', 'title': 'Accent', 'body': body})
    body['events'][0]['offset'] = 0
    body['events'][0]['source'] = 'clock.missing'
    with pytest.raises(ValidationError, match='unknown marker'):
        EnvelfoScore.model_validate({'name': 'accent', 'title': 'Accent', 'body': body})
    body['events'][0]['source'] = 'clock.peak'
    body['envelfos']['accent']['start'] = 'activation'
    with pytest.raises(ValidationError, match='event-start Contour'):
        EnvelfoScore.model_validate({'name': 'accent', 'title': 'Accent', 'body': body})
    body['envelfos']['accent']['start'] = 'event'
    body['envelfos']['accent']['retrigger'] = 'reset'
    with pytest.raises(ValidationError, match='event-start Contour'):
        EnvelfoScore.model_validate({'name': 'accent', 'title': 'Accent', 'body': body})
    with pytest.raises(ValidationError, match='stage contours start on stage entry'):
        Stages.model_validate(
            {
                'initial_stage': 'accent',
                'stages': [
                    {
                        'name': 'accent',
                        'envelfo': {
                            'kind': 'contour',
                            'start': 'event',
                            'segments': [{'duration': '1 s', 'to': 1}],
                        },
                    }
                ],
            }
        )


def test_autonomous_contour_completes_without_note_off() -> None:
    envelfo = EnvelfoUse(body=Contour(segments=[Segment(duration=Fraction(1), to=1)]))
    state = initial_envelfo(envelfo, Fraction(2))
    assert envelfo_at(envelfo, state, Fraction(2)).status == 'running'
    assert envelfo_at(envelfo, state, Fraction(5, 2)).value == 0.5
    state = envelfo_event(
        envelfo,
        state,
        EnvelfoEvent(at=Fraction(5, 2), ordinal=0, action='note_off'),
    )
    assert envelfo_at(envelfo, state, Fraction(3)).status == 'complete'
    assert envelfo_at(envelfo, state, Fraction(3)).value == 1
    restored = EnvelfoState.model_validate_json(state.model_dump_json())
    assert envelfo_at(envelfo, restored, Fraction(3)) == envelfo_at(
        envelfo, state, Fraction(3)
    )


def test_triggered_contour_holds_then_releases_from_current_value() -> None:
    envelfo = EnvelfoUse(
        body=Contour(
            segments=[Segment(duration=Fraction(1), to=1)],
            release=[Segment(duration=Fraction(1), to=0)],
        )
    )
    state = initial_envelfo(envelfo, Fraction(0))
    assert envelfo_at(envelfo, state, Fraction(0)).status == 'idle'
    state = envelfo_event(
        envelfo, state, EnvelfoEvent(at=Fraction(0), ordinal=0, action='note_on')
    )
    assert envelfo_at(envelfo, state, Fraction(1)).status == 'held'
    state = envelfo_event(
        envelfo, state, EnvelfoEvent(at=Fraction(3, 2), ordinal=0, action='note_off')
    )
    assert envelfo_at(envelfo, state, Fraction(2)).value == 0.5
    assert envelfo_at(envelfo, state, Fraction(5, 2)).status == 'complete'


def test_lfo_uses_same_value_and_event_contract() -> None:
    envelfo = EnvelfoUse(body=LFO(rate=Fraction(1)))
    state = initial_envelfo(envelfo, Fraction(0))
    state = envelfo_event(
        envelfo,
        state,
        EnvelfoEvent(at=Fraction(1, 4), ordinal=0, action='rate', rate=Fraction(2)),
    )
    assert envelfo_at(envelfo, state, Fraction(1, 2)).value == pytest.approx(-1)
    assert envelfo_at(envelfo, state, Fraction(1, 2)).status == 'running'
    state = envelfo_event(
        envelfo, state, EnvelfoEvent(at=Fraction(1, 2), ordinal=0, action='note_off')
    )
    assert envelfo_at(envelfo, state, Fraction(3, 4)).value == pytest.approx(1)
    restored = EnvelfoState.model_validate_json(state.model_dump_json())
    assert envelfo_at(envelfo, restored, Fraction(3, 4)) == envelfo_at(
        envelfo, state, Fraction(3, 4)
    )


def test_lfo_pause_reverse_and_seek_preserve_seconds_position() -> None:
    envelfo = EnvelfoUse(body=LFO(rate=Fraction(1), delay=Fraction(1, 4)))
    state = initial_envelfo(envelfo, Fraction(0))
    state = envelfo_event(
        envelfo, state, EnvelfoEvent(at=Fraction(1, 4), ordinal=0, action='pause')
    )
    assert envelfo_at(envelfo, state, Fraction(2)).value == pytest.approx(1)
    assert envelfo_at(envelfo, state, Fraction(2)).weight == 1
    state = envelfo_event(
        envelfo, state, EnvelfoEvent(at=Fraction(2), ordinal=0, action='reverse')
    )
    state = envelfo_event(
        envelfo, state, EnvelfoEvent(at=Fraction(3), ordinal=0, action='resume')
    )
    assert envelfo_at(envelfo, state, Fraction(13, 4)).value == pytest.approx(0)
    state = envelfo_event(
        envelfo,
        state,
        EnvelfoEvent(
            at=Fraction(13, 4), ordinal=0, action='seek', position=Fraction(1, 2)
        ),
    )
    assert envelfo_at(envelfo, state, Fraction(7, 2)).value == pytest.approx(1)
    restored = EnvelfoState.model_validate_json(state.model_dump_json())
    assert envelfo_at(envelfo, restored, Fraction(7, 2)) == envelfo_at(
        envelfo, state, Fraction(7, 2)
    )


def test_relative_shift_wraps_lfos_and_clamps_contours() -> None:
    lfo = EnvelfoUse(body=LFO(rate=Fraction(1)))
    lfo_state = envelfo_event(
        lfo,
        initial_envelfo(lfo, Fraction(0)),
        EnvelfoEvent(
            at=Fraction(1, 4),
            ordinal=0,
            action='shift',
            offset=Fraction(-1, 2),
        ),
    )
    assert envelfo_at(lfo, lfo_state, Fraction(1, 4)).value == pytest.approx(-1)

    contour = EnvelfoUse(body=Contour(segments=[Segment(duration=Fraction(1), to=1)]))
    state = envelfo_event(
        contour,
        initial_envelfo(contour, Fraction(0)),
        EnvelfoEvent(
            at=Fraction(1, 2),
            ordinal=0,
            action='shift',
            offset=Fraction(3, 4),
        ),
    )
    assert envelfo_at(contour, state, Fraction(1, 2)).value == 1
    state = envelfo_event(
        contour,
        state,
        EnvelfoEvent(
            at=Fraction(1, 2),
            ordinal=1,
            action='shift',
            offset=Fraction(-1, 2),
        ),
    )
    assert envelfo_at(contour, state, Fraction(1, 2)).value == pytest.approx(0.5)


@pytest.mark.parametrize(
    ('mode', 'at_end', 'after_end'),
    [
        (PlaybackMode.loop, 0, 0.25),
        (PlaybackMode.ping_pong, 1, 0.75),
    ],
)
def test_contour_playback_loops_and_releases_from_current_value(
    mode: PlaybackMode, at_end: float, after_end: float
) -> None:
    envelfo = EnvelfoUse(
        body=Contour(
            segments=[Segment(duration=Fraction(1), to=1)],
            release=[Segment(duration=Fraction(1), to=0)],
            playback=mode,
        )
    )
    state = envelfo_event(
        envelfo,
        initial_envelfo(envelfo, Fraction(0)),
        EnvelfoEvent(at=Fraction(0), ordinal=0, action='note_on'),
    )
    assert envelfo_at(envelfo, state, Fraction(1)).value == at_end
    assert envelfo_at(envelfo, state, Fraction(5, 4)).value == after_end
    assert envelfo_at(envelfo, state, Fraction(5, 4)).status == 'running'
    state = envelfo_event(
        envelfo,
        state,
        EnvelfoEvent(at=Fraction(5, 4), ordinal=0, action='note_off'),
    )
    assert envelfo_at(envelfo, state, Fraction(5, 4)).value == after_end
    assert envelfo_at(envelfo, state, Fraction(7, 4)).value == pytest.approx(
        after_end / 2
    )
    assert envelfo_at(envelfo, state, Fraction(9, 4)).status == 'complete'
    restored = EnvelfoState.model_validate_json(state.model_dump_json())
    assert envelfo_at(envelfo, restored, Fraction(7, 4)) == envelfo_at(
        envelfo, state, Fraction(7, 4)
    )
    with pytest.raises(ValueError, match='cannot be converted'):
        contour_envelope(envelfo)


def test_looping_contour_shift_keeps_unwrapped_position() -> None:
    envelfo = EnvelfoUse(
        body=Contour(
            segments=[Segment(duration=Fraction(1), to=1)],
            playback=PlaybackMode.ping_pong,
        )
    )
    state = envelfo_event(
        envelfo,
        initial_envelfo(envelfo, Fraction(0)),
        EnvelfoEvent(
            at=Fraction(1, 4),
            ordinal=0,
            action='shift',
            offset=Fraction(3, 2),
        ),
    )
    assert envelfo_at(envelfo, state, Fraction(1, 4)).value == 0.25
    assert envelfo_at(envelfo, state, Fraction(1, 2)).value == 0


@pytest.mark.parametrize(
    ('mode', 'ports'),
    [
        (
            PlaybackMode.loop,
            [
                (Fraction(1, 4), 'quarter'),
                (Fraction(1), 'cycle'),
                (Fraction(5, 4), 'quarter'),
                (Fraction(2), 'cycle'),
                (Fraction(9, 4), 'quarter'),
            ],
        ),
        (
            PlaybackMode.ping_pong,
            [
                (Fraction(1, 4), 'quarter'),
                (Fraction(1), 'turned'),
                (Fraction(7, 4), 'quarter'),
                (Fraction(2), 'turned'),
                (Fraction(2), 'cycle'),
                (Fraction(9, 4), 'quarter'),
            ],
        ),
    ],
)
def test_staged_contour_playback_orders_markers_and_boundaries(
    mode: PlaybackMode, ports: list[tuple[Fraction, str]]
) -> None:
    envelfo = EnvelfoUse(
        body=Stages(
            initial_stage='sweep',
            stages=[
                Stage(
                    name='sweep',
                    envelfo=Contour(
                        segments=[Segment(duration=Fraction(1), to=1)],
                        playback=mode,
                        markers=[Marker(name='quarter', position=Fraction(1, 4))],
                    ),
                )
            ],
        )
    )
    result = advance_envelfo(
        envelfo, initial_envelfo(envelfo, Fraction(0)), Fraction(9, 4)
    )
    assert [(e.at, e.port) for e in result.events] == ports
    assert result.value.value == 0.25
    assert result.value.status == 'running'
    assert advance_envelfo(envelfo, result.state, Fraction(5, 2)).events == []


@pytest.mark.parametrize(
    ('mode', 'captured'),
    [(PlaybackMode.loop, 0.25), (PlaybackMode.ping_pong, 0.75)],
)
def test_staged_loop_releases_immediately_from_current_value(
    mode: PlaybackMode, captured: float
) -> None:
    envelfo = EnvelfoUse(
        body=Stages(
            initial_stage='sweep',
            stages=[
                Stage(
                    name='sweep',
                    envelfo=Contour(
                        segments=[Segment(duration=Fraction(1), to=1)],
                        playback=mode,
                    ),
                ),
                Stage(
                    name='release',
                    envelfo=Contour(
                        initial='current',
                        segments=[Segment(duration=Fraction(1), to=0)],
                    ),
                ),
            ],
            transitions=[
                StageTransition(
                    **{
                        'from': ['sweep'],
                        'event': 'note_off',
                        'action': EnterStage(stage='release'),
                    }
                )
            ],
        )
    )
    release = EnvelfoEvent(at=Fraction(5, 4), ordinal=0, action='note_off')
    result = advance_envelfo(
        envelfo, initial_envelfo(envelfo, Fraction(0)), release.at, release
    )
    assert result.state.runtime.stage == 'release'
    assert result.value.value == captured
    assert advance_envelfo(
        envelfo, result.state, Fraction(7, 4)
    ).value.value == pytest.approx(captured / 2)


def test_staged_loop_cycle_port_can_enter_another_stage() -> None:
    envelfo = EnvelfoUse(
        body=Stages(
            initial_stage='sweep',
            stages=[
                Stage(
                    name='sweep',
                    envelfo=Contour(
                        segments=[Segment(duration=Fraction(1), to=1)],
                        playback=PlaybackMode.loop,
                    ),
                ),
                Stage(name='held', envelfo=Hold(value=0.75)),
            ],
            transitions=[
                StageTransition(
                    **{
                        'from': ['sweep'],
                        'event': 'stage.cycle',
                        'action': EnterStage(stage='held'),
                    }
                )
            ],
        )
    )
    result = advance_envelfo(
        envelfo, initial_envelfo(envelfo, Fraction(0)), Fraction(2)
    )
    assert [(e.at, e.port) for e in result.events] == [(Fraction(1), 'cycle')]
    assert result.state.runtime.stage == 'held'
    assert result.value.value == 0.75


@pytest.mark.parametrize(
    ('mode', 'boundary_events', 'value'),
    [
        (
            PlaybackMode.loop,
            [
                (Fraction(1, 4), 'start'),
                (Fraction(3, 4), 'start'),
                (Fraction(3, 4), 'end'),
                (Fraction(3, 4), 'cycle'),
                (Fraction(5, 4), 'start'),
                (Fraction(5, 4), 'end'),
                (Fraction(5, 4), 'cycle'),
            ],
            0.25,
        ),
        (
            PlaybackMode.ping_pong,
            [
                (Fraction(1, 4), 'start'),
                (Fraction(3, 4), 'end'),
                (Fraction(3, 4), 'turned'),
                (Fraction(5, 4), 'start'),
                (Fraction(5, 4), 'turned'),
                (Fraction(5, 4), 'cycle'),
            ],
            0.25,
        ),
    ],
)
def test_named_contour_loop_boundaries_preserve_markers_and_events(
    mode: PlaybackMode,
    boundary_events: list[tuple[Fraction, str]],
    value: float,
) -> None:
    contour = Contour(
        segments=[Segment(duration=Fraction(1), to=1)],
        playback=mode,
        markers=[
            Marker(name='start', position=Fraction(1, 4)),
            Marker(name='end', position=Fraction(3, 4)),
        ],
        loop_start='start',
        loop_end='end',
    )
    envelfo = EnvelfoUse(
        body=Stages(
            initial_stage='sweep',
            stages=[Stage(name='sweep', envelfo=contour)],
        )
    )
    result = advance_envelfo(
        envelfo, initial_envelfo(envelfo, Fraction(0)), Fraction(5, 4)
    )
    assert [(e.at, e.port) for e in result.events] == boundary_events
    assert result.value.value == value
    standalone = EnvelfoUse(body=contour)
    started = envelfo_event(
        standalone,
        initial_envelfo(standalone, Fraction(0)),
        EnvelfoEvent(at=Fraction(0), ordinal=0, action='note_on'),
    )
    assert envelfo_at(standalone, started, Fraction(1)).value == 0.5


def test_named_loop_reverse_orders_boundary_markers_before_cycle() -> None:
    envelfo = EnvelfoUse(
        body=Stages(
            initial_stage='sweep',
            stages=[
                Stage(
                    name='sweep',
                    envelfo=Contour(
                        segments=[Segment(duration=Fraction(1), to=1)],
                        playback=PlaybackMode.loop,
                        markers=[
                            Marker(name='start', position=Fraction(1, 4)),
                            Marker(name='end', position=Fraction(3, 4)),
                        ],
                        loop_start='start',
                        loop_end='end',
                    ),
                )
            ],
        )
    )
    forward = advance_envelfo(
        envelfo, initial_envelfo(envelfo, Fraction(0)), Fraction(1)
    )
    reversed_state = advance_envelfo(
        envelfo,
        forward.state,
        Fraction(1),
        EnvelfoEvent(at=Fraction(1), ordinal=0, action='reverse'),
    ).state
    result = advance_envelfo(envelfo, reversed_state, Fraction(5, 4))
    assert [(e.at, e.port) for e in result.events] == [
        (Fraction(5, 4), 'end'),
        (Fraction(5, 4), 'start'),
        (Fraction(5, 4), 'cycle'),
    ]
    assert (
        EnvelfoState.model_validate_json(result.state.model_dump_json()) == result.state
    )


@pytest.mark.parametrize(
    ('mode', 'events', 'value'),
    [
        (PlaybackMode.loop, ['cycle', 'cycle', 'stage.done'], 1.0),
        (
            PlaybackMode.ping_pong,
            ['turned', 'turned', 'cycle', 'stage.done'],
            0.0,
        ),
    ],
)
def test_finite_contour_repeats_hold_final_boundary(
    mode: PlaybackMode, events: list[str], value: float
) -> None:
    contour = Contour(
        segments=[Segment(duration=Fraction(1), to=1)],
        playback=mode,
        repeat_count=2,
    )
    staged = EnvelfoUse(
        body=Stages(
            initial_stage='sweep',
            stages=[Stage(name='sweep', envelfo=contour)],
        )
    )
    result = advance_envelfo(staged, initial_envelfo(staged, Fraction(0)), Fraction(3))
    assert [e.port for e in result.events] == events
    assert result.value.value == value
    assert result.value.status == 'complete'
    assert advance_envelfo(staged, result.state, Fraction(4)).events == []
    assert (
        EnvelfoState.model_validate_json(result.state.model_dump_json()) == result.state
    )

    standalone = EnvelfoUse(body=contour)
    initial = initial_envelfo(standalone, Fraction(0))
    assert envelfo_at(standalone, initial, Fraction(3)).value == value
    assert envelfo_at(standalone, initial, Fraction(3)).status == 'complete'
    state = envelfo_event(
        standalone,
        initial,
        EnvelfoEvent(at=Fraction(3), ordinal=0, action='pause'),
    )
    assert envelfo_at(standalone, state, Fraction(4)).value == value


def test_finite_named_loop_counts_reverse_boundary_crossing() -> None:
    contour = Contour(
        segments=[Segment(duration=Fraction(1), to=1)],
        playback=PlaybackMode.loop,
        markers=[
            Marker(name='start', position=Fraction(1, 4)),
            Marker(name='end', position=Fraction(3, 4)),
        ],
        loop_start='start',
        loop_end='end',
        repeat_count=2,
    )
    staged = EnvelfoUse(
        body=Stages(
            initial_stage='sweep',
            stages=[Stage(name='sweep', envelfo=contour)],
        )
    )
    forward = advance_envelfo(staged, initial_envelfo(staged, Fraction(0)), Fraction(1))
    reversed_state = advance_envelfo(
        staged,
        forward.state,
        Fraction(1),
        EnvelfoEvent(at=Fraction(1), ordinal=0, action='reverse'),
    ).state
    final = advance_envelfo(staged, reversed_state, Fraction(2))
    assert [(e.at, e.port) for e in final.events] == [
        (Fraction(5, 4), 'end'),
        (Fraction(5, 4), 'start'),
        (Fraction(5, 4), 'cycle'),
        (Fraction(5, 4), 'stage.done'),
    ]
    assert final.value.value == 0.25
    assert final.value.status == 'complete'


def test_final_repeat_emits_done_before_entering_next_stage() -> None:
    envelfo = EnvelfoUse(
        body=Stages(
            initial_stage='sweep',
            stages=[
                Stage(
                    name='sweep',
                    envelfo=Contour(
                        segments=[Segment(duration=Fraction(1), to=1)],
                        playback=PlaybackMode.loop,
                        repeat_count=1,
                    ),
                ),
                Stage(name='held', envelfo=Hold(value=0.5)),
            ],
            transitions=[
                StageTransition(
                    **{
                        'from': ['sweep'],
                        'event': 'stage.cycle',
                        'action': EnterStage(stage='held'),
                    }
                )
            ],
        )
    )
    result = advance_envelfo(
        envelfo, initial_envelfo(envelfo, Fraction(0)), Fraction(2)
    )
    assert [e.port for e in result.events] == ['cycle', 'stage.done']
    assert result.state.runtime.stage == 'held'
    assert result.value.value == 0.5


def test_contour_reverses_through_segments_without_recapturing_start() -> None:
    envelfo = EnvelfoUse(
        body=Contour(
            segments=[Segment(duration=Fraction(1), to=1)],
            release=[Segment(duration=Fraction(1), to=0)],
        )
    )
    state = envelfo_event(
        envelfo,
        initial_envelfo(envelfo, Fraction(0)),
        EnvelfoEvent(at=Fraction(0), ordinal=0, action='note_on'),
    )
    state = envelfo_event(
        envelfo, state, EnvelfoEvent(at=Fraction(3, 4), ordinal=0, action='reverse')
    )
    assert envelfo_at(envelfo, state, Fraction(1)).value == pytest.approx(0.5)
    state = envelfo_event(
        envelfo, state, EnvelfoEvent(at=Fraction(1), ordinal=0, action='pause')
    )
    assert envelfo_at(envelfo, state, Fraction(3)).value == pytest.approx(0.5)
    state = envelfo_event(
        envelfo,
        state,
        EnvelfoEvent(at=Fraction(3), ordinal=0, action='seek', position=Fraction(1)),
    )
    assert envelfo_at(envelfo, state, Fraction(3)).status == 'held'
    state = envelfo_event(
        envelfo, state, EnvelfoEvent(at=Fraction(3), ordinal=1, action='note_off')
    )
    assert envelfo_at(envelfo, state, Fraction(3)).value == 1


def test_staged_reverse_markers_and_seek_do_not_undo_transitions() -> None:
    envelfo = staged_envelfo()
    cue = EnvelfoEvent(at=Fraction(0), ordinal=0, action='cue', cue='sway')
    state = advance_envelfo(
        envelfo, initial_envelfo(envelfo, cue.at), cue.at, cue
    ).state
    first = advance_envelfo(envelfo, state, Fraction(3, 8))
    assert [(e.at, e.port) for e in first.events] == [(Fraction(1, 8), 'peak')]
    reversed_at = EnvelfoEvent(at=Fraction(3, 8), ordinal=0, action='reverse')
    state = advance_envelfo(envelfo, first.state, reversed_at.at, reversed_at).state
    backward = advance_envelfo(envelfo, state, Fraction(5, 8))
    assert [(e.at, e.port) for e in backward.events] == [(Fraction(5, 8), 'peak')]
    seek = EnvelfoEvent(
        at=Fraction(5, 8), ordinal=0, action='seek', position=Fraction(3, 4)
    )
    jumped = advance_envelfo(envelfo, backward.state, seek.at, seek)
    assert jumped.events == []
    assert jumped.state.runtime.stage == 'sway'
    assert advance_envelfo(envelfo, jumped.state, Fraction(7, 8)).events == [
        backward.events[0].model_copy(update={'at': Fraction(7, 8)})
    ]


def test_unconnected_stage_completion_emits_once() -> None:
    envelfo = EnvelfoUse(
        body=Stages(
            initial_stage='rise',
            stages=[
                Stage(
                    name='rise',
                    envelfo=Contour(segments=[Segment(duration=Fraction(1, 4), to=1)]),
                )
            ],
        )
    )
    first = advance_envelfo(
        envelfo, initial_envelfo(envelfo, Fraction(0)), Fraction(1, 4)
    )
    assert [e.port for e in first.events] == ['stage.done']
    assert advance_envelfo(envelfo, first.state, Fraction(1)).events == []


def test_stages_attack_sways_releases_and_emits_completion() -> None:
    envelfo = staged_envelfo()
    assert EnvelfoUse.model_validate_json(envelfo.model_dump_json()) == envelfo
    score = EnvelfoScore(name='bloom', title='Bloom', body=envelfo.body)
    assert parse_score(score_toml(score)) == score
    state = initial_envelfo(envelfo, Fraction(0))
    assert envelfo_at(envelfo, state, Fraction(0)).value == 0
    start = advance_envelfo(
        envelfo,
        state,
        Fraction(0),
        EnvelfoEvent(at=Fraction(0), ordinal=0, action='note_on'),
    )
    attack_end = advance_envelfo(envelfo, start.state, Fraction(1, 4))
    assert attack_end.value.value == pytest.approx(0.7)
    assert [(e.at, e.port, e.stage) for e in attack_end.events] == [
        (Fraction(1, 4), 'stage.done', 'attack')
    ]
    peak = advance_envelfo(envelfo, attack_end.state, Fraction(3, 8))
    assert peak.value.value == pytest.approx(0.9)
    assert [(e.at, e.port, e.stage) for e in peak.events] == [
        (Fraction(3, 8), 'peak', 'sway')
    ]
    release = advance_envelfo(
        envelfo,
        peak.state,
        Fraction(3, 8),
        EnvelfoEvent(at=Fraction(3, 8), ordinal=0, action='note_off'),
    )
    assert release.value.value == pytest.approx(0.9)
    restored = EnvelfoState.model_validate_json(release.state.model_dump_json())
    done = advance_envelfo(envelfo, restored, Fraction(5, 8))
    assert done.value.status == 'complete'
    assert done.value.value == pytest.approx(0)
    assert [(e.port, e.stage) for e in done.events] == [
        ('stage.done', 'release'),
        ('done', 'release'),
    ]
    assert advance_envelfo(envelfo, done.state, Fraction(1)).events == []


def test_note_off_at_stage_end_discards_obsolete_completion() -> None:
    envelfo = staged_envelfo()
    start = advance_envelfo(
        envelfo,
        initial_envelfo(envelfo, Fraction(0)),
        Fraction(0),
        EnvelfoEvent(at=Fraction(0), ordinal=0, action='note_on'),
    )
    release = advance_envelfo(
        envelfo,
        start.state,
        Fraction(1, 4),
        EnvelfoEvent(at=Fraction(1, 4), ordinal=0, action='note_off'),
    )
    assert release.events == []
    assert release.value.value == pytest.approx(0.7)
    assert release.state.runtime.stage == 'release'
    done = advance_envelfo(envelfo, release.state, Fraction(1, 2))
    assert [e.port for e in done.events] == ['stage.done', 'done']


def test_stage_cue_and_marker_crossings_are_partition_independent() -> None:
    envelfo = staged_envelfo()
    cue = EnvelfoEvent(at=Fraction(0), ordinal=0, action='cue', cue='sway')
    start = advance_envelfo(envelfo, initial_envelfo(envelfo, Fraction(0)), cue.at, cue)
    whole = advance_envelfo(envelfo, start.state, Fraction(1))
    pieces = []
    state = start.state
    for end in (Fraction(1, 8), Fraction(1, 4), Fraction(3, 4), Fraction(1)):
        result = advance_envelfo(envelfo, state, end)
        pieces.extend(result.events)
        state = result.state
    assert [(e.at, e.port) for e in whole.events] == [
        (Fraction(1, 8), 'peak'),
        (Fraction(5, 8), 'peak'),
    ]
    assert pieces == whole.events
    assert envelfo_at(envelfo, state, Fraction(1)) == whole.value
