use envelfo_core::envelope::{
    Envelope, EnvelopeCommand, Initial, Marker, Playback, Retrigger, Segment, Start,
};
use envelfo_core::lfo::{Command, Direction};
use envelfo_core::time::Time;
use envelfo_core::{Error, Status};

#[test]
fn release_captures_the_in_flight_level_and_retrigger_policy_is_explicit() {
    for (policy, expected) in [
        (Retrigger::Current, 0.25),
        (Retrigger::Initial, 0.0),
        (Retrigger::Ignore, 0.25),
    ] {
        let mut body = ramp();
        body.release = vec![segment(Time::ONE, 0.0)];
        body.retrigger = policy;
        let mut state = body.initial(Time::ZERO).unwrap();
        body.command(
            &mut state,
            Time::ZERO,
            0,
            EnvelopeCommand::Playback(Command::NoteOn),
        )
        .unwrap();
        body.command(
            &mut state,
            t(1, 2),
            0,
            EnvelopeCommand::Playback(Command::NoteOff),
        )
        .unwrap();
        assert_eq!(body.value(&state, t(3, 4)).unwrap().value, 0.375);
        body.command(
            &mut state,
            Time::ONE,
            0,
            EnvelopeCommand::Playback(Command::NoteOn),
        )
        .unwrap();
        assert_eq!(body.value(&state, Time::ONE).unwrap().value, expected);
        assert_eq!(
            body.value(&state, t(3, 2)).unwrap().value,
            if policy == Retrigger::Ignore {
                0.0
            } else {
                (expected + 1.0) / 2.0
            }
        );
    }
}

#[test]
fn event_started_envelope_ignores_note_on_until_explicit_start() {
    let mut body = ramp();
    body.start = Start::Event;
    let mut state = body.initial(Time::ZERO).unwrap();
    body.command(
        &mut state,
        Time::ONE,
        0,
        EnvelopeCommand::Playback(Command::NoteOn),
    )
    .unwrap();
    assert_eq!(body.value(&state, Time::ONE).unwrap().status, Status::Idle);
    body.command(&mut state, Time::ONE, 1, EnvelopeCommand::Start)
        .unwrap();
    assert_eq!(body.value(&state, t(3, 2)).unwrap().value, 0.5);
}

#[test]
fn zero_length_segments_apply_immediately_and_curves_stay_finite() {
    for curve in [-1000.0, -4.0, 0.0, 4.0, 1000.0] {
        let body = Envelope::new(
            0.0,
            vec![
                segment(Time::ZERO, 0.25),
                Segment {
                    duration: Some(Time::ONE),
                    to: 1.0,
                    curve,
                },
            ],
        )
        .unwrap();
        let state = body.initial(Time::ZERO).unwrap();
        assert_eq!(body.value(&state, Time::ZERO).unwrap().value, 0.25);
        let value = body.value(&state, t(1, 2)).unwrap().value;
        let expected = if curve < 0.0 {
            0.25 + 0.75 / (1.0 + (curve / 2.0).exp())
        } else {
            0.25 + 0.75 * (-curve / 2.0).exp() / (1.0 + (-curve / 2.0).exp())
        };
        assert!((value - expected).abs() < 1e-12);
        assert_eq!(body.value(&state, Time::ONE).unwrap().value, 1.0);
    }
    let body = Envelope::new(0.0, vec![segment(Time::ZERO, 1.0)]).unwrap();
    let state = body.initial(Time::ZERO).unwrap();
    assert_eq!(
        body.value(&state, Time::ZERO).unwrap().status,
        Status::Complete
    );
}

#[test]
fn pause_reverse_seek_and_shift_preserve_exact_playback() {
    let body = ramp();
    let mut state = body.initial(Time::ZERO).unwrap();
    body.command(
        &mut state,
        t(1, 4),
        0,
        EnvelopeCommand::Playback(Command::Pause),
    )
    .unwrap();
    assert_eq!(body.value(&state, Time::ONE).unwrap().value, 0.25);
    body.command(
        &mut state,
        Time::ONE,
        0,
        EnvelopeCommand::Playback(Command::Resume),
    )
    .unwrap();
    body.command(
        &mut state,
        Time::ONE,
        1,
        EnvelopeCommand::Playback(Command::Reverse),
    )
    .unwrap();
    assert_eq!(body.value(&state, t(9, 8)).unwrap().value, 0.125);
    body.command(
        &mut state,
        t(9, 8),
        0,
        EnvelopeCommand::Playback(Command::Seek(t(3, 4))),
    )
    .unwrap();
    body.command(
        &mut state,
        t(9, 8),
        1,
        EnvelopeCommand::Playback(Command::Shift(Time::ONE)),
    )
    .unwrap();
    assert_eq!(body.value(&state, t(9, 8)).unwrap().value, 1.0);
}

#[test]
fn finite_repeats_count_actual_forward_and_reverse_crossings() {
    for mode in [Playback::Loop, Playback::PingPong] {
        for direction in [Direction::Forward, Direction::Reverse] {
            let mut body = ramp();
            body.playback = mode;
            body.repeat_count = Some(2);
            let mut state = body.initial(Time::ZERO).unwrap();
            if direction == Direction::Reverse {
                body.command(
                    &mut state,
                    Time::ZERO,
                    0,
                    EnvelopeCommand::Playback(Command::Reverse),
                )
                .unwrap();
            }
            assert_eq!(body.value(&state, t(3, 2)).unwrap().status, Status::Running);
            let value = body.value(&state, Time::from_integer(2)).unwrap();
            assert_eq!(value.status, Status::Complete);
            assert_eq!(
                value.value,
                if mode == Playback::Loop && direction == Direction::Forward {
                    1.0
                } else {
                    0.0
                }
            );
            body.command(
                &mut state,
                Time::ONE,
                1,
                EnvelopeCommand::Playback(Command::Pause),
            )
            .unwrap();
            body.command(
                &mut state,
                Time::from_integer(2),
                0,
                EnvelopeCommand::Playback(Command::Resume),
            )
            .unwrap();
            assert_eq!(body.value(&state, Time::from_integer(3)).unwrap(), value);
        }
    }
}

#[test]
fn named_ping_pong_repeats_hold_the_last_boundary_value() {
    let mut body = ramp();
    body.playback = Playback::PingPong;
    body.repeat_count = Some(2);
    body.markers = vec![marker("lower", t(1, 4)), marker("upper", t(3, 4))];
    body.loop_bounds = Some((t(1, 4), t(3, 4)));
    let state = body.initial(Time::ZERO).unwrap();
    assert_eq!(body.value(&state, Time::ONE).unwrap().value, 0.5);
    let final_value = body.value(&state, t(5, 4)).unwrap();
    assert_eq!(final_value.value, 0.25);
    assert_eq!(final_value.status, Status::Complete);
    assert_eq!(
        body.value(&state, Time::from_integer(3)).unwrap(),
        final_value
    );
}

#[test]
fn invalid_commands_and_time_overflow_leave_snapshot_unchanged() {
    let body = ramp();
    let mut state = body.initial(Time::ZERO).unwrap();
    let saved = state.clone();
    for command in [
        Command::Rate(Time::ONE),
        Command::Reset,
        Command::Seek(Time::from_integer(2)),
    ] {
        assert!(
            body.command(&mut state, t(1, 4), 0, EnvelopeCommand::Playback(command))
                .is_err()
        );
        assert_eq!(state, saved);
    }
    body.command(
        &mut state,
        t(1, 4),
        0,
        EnvelopeCommand::Playback(Command::Pause),
    )
    .unwrap();
    let saved = state.clone();
    assert_eq!(
        body.command(
            &mut state,
            t(1, 4),
            0,
            EnvelopeCommand::Playback(Command::Resume)
        ),
        Err(Error::EventOrder)
    );
    assert_eq!(state, saved);
    let mut body = ramp();
    body.segments[0].duration = Some(t(1, 2));
    let mut state = body.initial(Time::ZERO).unwrap();
    let saved = state.clone();
    assert_eq!(
        body.command(
            &mut state,
            Time::from_integer(i64::MAX),
            0,
            EnvelopeCommand::Playback(Command::Pause)
        ),
        Err(Error::Overflow)
    );
    assert_eq!(state, saved);
}

#[test]
fn current_initial_requires_stage_entry_and_definitions_validate_ranges() {
    let mut body = ramp();
    body.initial = Initial::Current;
    assert!(body.initial(Time::ZERO).is_err());
    body.initial = Initial::Value(0.0);
    body.segments[0].to = -1.0;
    assert!(body.validate().is_err());
    body.bipolar = true;
    assert!(body.validate().is_ok());
    body.playback = Playback::Loop;
    body.repeat_count = Some(0);
    assert!(body.validate().is_err());
}

fn ramp() -> Envelope {
    Envelope::new(0.0, vec![segment(Time::ONE, 1.0)]).unwrap()
}
fn segment(duration: Time, to: f64) -> Segment {
    Segment {
        duration: Some(duration),
        to,
        curve: 0.0,
    }
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
