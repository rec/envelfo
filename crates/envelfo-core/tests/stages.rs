use envelfo_core::contour::{Contour, Initial, Marker, Playback, Segment};
use envelfo_core::cycle::{Command, Cycle, Shape};
use envelfo_core::stages::{Action, Body, Event, Stage, StageCommand, Stages, Transition};
use envelfo_core::time::Time;
use envelfo_core::{Error, Status};

#[test]
fn irregular_partitions_and_cloned_continuations_preserve_all_events() {
    for mode in [Playback::Once, Playback::Loop, Playback::PingPong] {
        for named in [false, true] {
            if named && mode == Playback::Once {
                continue;
            }
            let body = repeated(mode, named, None);
            let initial = body.initial(Time::ZERO).unwrap();
            let mut whole = initial.clone();
            let expected = body
                .advance(&mut whole, Time::from_integer(4), None)
                .unwrap();
            let mut split = initial;
            let mut events = vec![];
            for at in [
                t(1, 7),
                t(1, 4),
                t(2, 3),
                t(3, 4),
                Time::ONE,
                t(11, 5),
                Time::from_integer(4),
            ] {
                let mut restored = split.clone();
                let advance = body.advance(&mut restored, at, None).unwrap();
                events.extend(advance.events);
                split = restored;
            }
            assert_eq!(events, expected.events, "{mode:?} named={named}");
            assert_eq!(split, whole);
        }
    }
}

#[test]
fn reverse_markers_are_ordered_before_named_boundary_events() {
    let body = repeated(Playback::Loop, true, None);
    let mut state = body.initial(Time::ZERO).unwrap();
    send(&body, &mut state, Time::ZERO, 0, Command::Seek(Time::ONE));
    send(&body, &mut state, Time::ZERO, 1, Command::Reverse);
    let advance = body.advance(&mut state, t(1, 4), None).unwrap();
    assert_eq!(ports(&advance.events), ["upper", "lower", "cycle"]);
    assert!(advance.events.iter().all(|e| e.at == t(1, 4)));
    assert_eq!(advance.value.value, 0.25);
}

#[test]
fn finite_ping_pong_emits_turn_then_cycle_then_completion_at_final_crossing() {
    for named in [false, true] {
        let body = repeated(Playback::PingPong, named, Some(2));
        let mut state = body.initial(Time::ZERO).unwrap();
        let at = if named {
            t(5, 4)
        } else {
            Time::from_integer(2)
        };
        let advance = body.advance(&mut state, at, None).unwrap();
        let final_ports = advance
            .events
            .iter()
            .filter(|e| e.at == at)
            .map(|e| e.port.as_str())
            .collect::<Vec<_>>();
        assert!(final_ports.ends_with(&["turned", "cycle", "stage.done"]));
        assert_eq!(advance.value.status, Status::Complete);
        assert_eq!(advance.value.value, if named { 0.25 } else { 0.0 });
        let later = body
            .advance(&mut state, Time::from_integer(3), None)
            .unwrap();
        assert!(later.events.is_empty());
        assert_eq!(later.value, advance.value);
    }
}

#[test]
fn finite_repeat_boundary_transition_wins_over_obsolete_completion_transition() {
    let mut body = repeated(Playback::Loop, true, Some(1));
    body.stages.push(stage("held", Body::Hold(0.5)));
    body.transitions = vec![
        transition("sweep", "stage.cycle", Action::Enter("held".into())),
        transition("sweep", "stage.done", Action::Finish),
    ];
    let mut state = body.initial(Time::ZERO).unwrap();
    let advance = body.advance(&mut state, t(3, 4), None).unwrap();
    assert_eq!(body.stage_name(&state), "held");
    assert_eq!(state.activation(), 1);
    assert_eq!(advance.value.status, Status::Held);
    assert_eq!(
        ports(&advance.events),
        ["lower", "lower", "upper", "cycle", "stage.done"]
    );
}

#[test]
fn cue_reenters_with_current_level_and_preserves_pause() {
    let mut body = repeated(Playback::Once, false, None);
    let mut contour = ramp();
    contour.initial = Initial::Current;
    body.stages.push(stage("release", Body::Contour(contour)));
    body.transitions.push(transition(
        "sweep",
        "cue.release",
        Action::Enter("release".into()),
    ));
    let mut state = body.initial(Time::ZERO).unwrap();
    send(&body, &mut state, t(1, 2), 0, Command::Pause);
    let event = Event {
        at: Time::ONE,
        ordinal: 0,
        command: StageCommand::Cue("release".into()),
    };
    let advance = body.advance(&mut state, Time::ONE, Some(&event)).unwrap();
    assert_eq!(advance.value.value, 0.5);
    assert_eq!(
        body.advance(&mut state, t(3, 2), None).unwrap().value.value,
        0.5
    );
    send(&body, &mut state, t(3, 2), 0, Command::Resume);
    assert_eq!(
        body.advance(&mut state, Time::from_integer(2), None)
            .unwrap()
            .value
            .value,
        0.75
    );
}

#[test]
fn stage_cycle_fade_saturates_without_unnecessary_rational_overflow() {
    let mut cycle = Cycle::new(Time::ZERO).unwrap();
    cycle.shape = Shape::Square;
    cycle.fade = t(1, i64::MAX);
    let body = Stages {
        initial_stage: "wave".into(),
        stages: vec![stage("wave", Body::Cycle(cycle))],
        transitions: vec![],
    };
    let state = body.initial(Time::ZERO).unwrap();
    assert_eq!(
        body.value(&state, Time::from_integer(i64::MAX))
            .unwrap()
            .value,
        1.0
    );
}

#[test]
fn exact_marker_order_is_preserved_near_float_precision_limit() {
    let mut contour = ramp();
    contour.markers = vec![
        marker("earlier", t(i64::MAX - 2, i64::MAX)),
        marker("later", t(i64::MAX - 1, i64::MAX)),
    ];
    let body = Stages {
        initial_stage: "sweep".into(),
        stages: vec![stage("sweep", Body::Contour(contour))],
        transitions: vec![],
    };
    let mut state = body.initial(Time::ZERO).unwrap();
    let advance = body.advance(&mut state, Time::ONE, None).unwrap();
    assert_eq!(ports(&advance.events), ["earlier", "later", "stage.done"]);
    assert!(advance.events[0].at < advance.events[1].at);
    assert_eq!(advance.events[0].at.as_f64(), advance.events[1].at.as_f64());
}

#[test]
fn capacity_overflow_and_invalid_commands_roll_back_the_whole_advance() {
    let body = repeated(Playback::Loop, false, None);
    let mut state = body.initial(Time::ZERO).unwrap();
    let saved = state.clone();
    assert_eq!(
        body.advance(&mut state, Time::from_integer(4097), None),
        Err(Error::Capacity)
    );
    assert_eq!(state, saved);
    let invalid = Event {
        at: Time::ONE,
        ordinal: 0,
        command: StageCommand::Playback(Command::Rate(Time::from_integer(-1))),
    };
    assert!(body.advance(&mut state, Time::ONE, Some(&invalid)).is_err());
    assert_eq!(state, saved);
    let invalid = Event {
        at: Time::ONE,
        ordinal: 0,
        command: StageCommand::Playback(Command::Reset),
    };
    assert!(body.advance(&mut state, Time::ONE, Some(&invalid)).is_err());
    assert_eq!(state, saved);
    let mut body = repeated(Playback::Once, false, None);
    if let Body::Contour(c) = &mut body.stages[0].body {
        c.segments[0].duration = t(1, 2);
    }
    let mut state = body.initial(Time::ZERO).unwrap();
    let saved = state.clone();
    assert_eq!(
        body.advance(&mut state, Time::from_integer(i64::MAX), None),
        Err(Error::Overflow)
    );
    assert_eq!(state, saved);
}

#[test]
fn next_boundary_queries_are_non_mutating_and_honor_exclusive_limit() {
    let body = repeated(Playback::Once, false, None);
    let state = body.initial(Time::ZERO).unwrap();
    let saved = state.clone();
    assert!(body.next_event(&state, t(1, 4), false).unwrap().is_none());
    let event = body.next_event(&state, t(1, 4), true).unwrap().unwrap();
    assert_eq!(event.at, t(1, 4));
    assert_eq!(event.port, "lower");
    assert_eq!(state, saved);
}

#[test]
fn rate_changes_and_pauses_retime_future_markers_without_replaying_past_ones() {
    let body = repeated(Playback::Once, false, None);
    let mut state = body.initial(Time::ZERO).unwrap();
    send(
        &body,
        &mut state,
        t(1, 8),
        0,
        Command::Rate(Time::from_integer(2)),
    );
    assert_eq!(
        body.next_event(&state, Time::ONE, true)
            .unwrap()
            .unwrap()
            .at,
        t(3, 16)
    );
    send(&body, &mut state, t(1, 8), 1, Command::Pause);
    assert!(body.next_event(&state, Time::ONE, true).unwrap().is_none());
    send(&body, &mut state, t(1, 2), 0, Command::Resume);
    assert_eq!(
        body.next_event(&state, Time::ONE, true)
            .unwrap()
            .unwrap()
            .at,
        t(9, 16)
    );
}

#[test]
fn stage_definitions_reject_unknown_transitions_and_reserved_markers() {
    let mut body = repeated(Playback::Once, false, None);
    body.transitions
        .push(transition("sweep", "stage.unknown", Action::Finish));
    assert!(body.initial(Time::ZERO).is_err());
    body.transitions.clear();
    if let Body::Contour(c) = &mut body.stages[0].body {
        c.markers[0].name = "done".into();
    }
    assert!(body.initial(Time::ZERO).is_err());
    let body = Stages {
        initial_stage: "held".into(),
        stages: vec![stage("held", Body::Hold(0.25))],
        transitions: vec![],
    };
    let mut state = body.initial(Time::ZERO).unwrap();
    let event = Event {
        at: Time::ZERO,
        ordinal: 0,
        command: StageCommand::Playback(Command::Seek(Time::ZERO)),
    };
    assert!(body.advance(&mut state, Time::ZERO, Some(&event)).is_err());
}

fn send(
    body: &Stages,
    state: &mut envelfo_core::stages::StageState,
    at: Time,
    ordinal: u64,
    command: Command,
) {
    body.advance(
        state,
        at,
        Some(&Event {
            at,
            ordinal,
            command: StageCommand::Playback(command),
        }),
    )
    .unwrap();
}
fn ports(events: &[envelfo_core::stages::OutputEvent]) -> Vec<&str> {
    events.iter().map(|e| e.port.as_str()).collect()
}
fn repeated(playback: Playback, named: bool, repeat_count: Option<u64>) -> Stages {
    let mut contour = ramp();
    contour.playback = playback;
    contour.repeat_count = repeat_count;
    contour.markers = vec![marker("lower", t(1, 4)), marker("upper", t(3, 4))];
    if named {
        contour.loop_bounds = Some((t(1, 4), t(3, 4)));
    }
    Stages {
        initial_stage: "sweep".into(),
        stages: vec![stage("sweep", Body::Contour(contour))],
        transitions: vec![],
    }
}
fn stage(name: &str, body: Body) -> Stage {
    Stage {
        name: name.into(),
        body,
        markers: vec![],
    }
}
fn transition(source: &str, event: &str, action: Action) -> Transition {
    Transition {
        from: vec![source.into()],
        event: event.into(),
        action,
    }
}
fn ramp() -> Contour {
    Contour::new(
        0.0,
        vec![Segment {
            duration: Time::ONE,
            to: 1.0,
            curve: 0.0,
        }],
    )
    .unwrap()
}
fn marker(name: &str, position: Time) -> Marker {
    Marker {
        name: name.into(),
        position,
    }
}
fn t(n: i64, d: i64) -> Time {
    Time::new(n, d).unwrap()
}
