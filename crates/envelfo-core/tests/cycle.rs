use envelfo_core::Error;
use envelfo_core::cycle::{Clock, Command, Cycle, Driver, Reset, Shape};
use envelfo_core::time::Time;

#[test]
fn reverse_seek_shift_and_clone_preserve_exact_position() {
    let mut definition = Cycle::new(Time::ONE).unwrap();
    definition.shape = Shape::Triangle;
    let mut state = definition.initial(Time::ZERO).unwrap();
    definition
        .command(&mut state, Time::new(1, 4).unwrap(), 0, Command::Reverse)
        .unwrap();
    assert_eq!(
        definition
            .value(&state, Time::new(1, 2).unwrap())
            .unwrap()
            .value,
        -1.0
    );
    definition
        .command(
            &mut state,
            Time::new(1, 2).unwrap(),
            0,
            Command::Seek(Time::new(3, 4).unwrap()),
        )
        .unwrap();
    definition
        .command(
            &mut state,
            Time::new(1, 2).unwrap(),
            1,
            Command::Shift(Time::new(1, 4).unwrap()),
        )
        .unwrap();
    let restored = state.clone();
    assert_eq!(
        definition
            .value(&restored, Time::new(1, 2).unwrap())
            .unwrap()
            .value,
        -1.0
    );
    assert_eq!(
        definition
            .value(&state, Time::new(3, 4).unwrap())
            .unwrap()
            .value,
        0.0
    );
}

#[test]
fn failed_commands_leave_the_complete_snapshot_unchanged() {
    let mut definition = Cycle::new(Time::from_integer(i64::MAX)).unwrap();
    let mut state = definition.initial(Time::ZERO).unwrap();
    let before = state.clone();
    assert_eq!(
        definition.command(&mut state, Time::from_integer(2), 0, Command::Pause),
        Err(Error::Overflow)
    );
    assert_eq!(state, before);
    definition.rate = Time::ONE;
    for command in [
        Command::Rate(Time::from_integer(-1)),
        Command::Seek(Time::from_integer(2)),
    ] {
        assert!(
            definition
                .command(&mut state, Time::ZERO, 0, command)
                .is_err()
        );
        assert_eq!(state, before);
    }
    definition
        .command(&mut state, Time::ZERO, 0, Command::Pause)
        .unwrap();
    let paused = state.clone();
    assert_eq!(
        definition.command(&mut state, Time::ZERO, 0, Command::Resume),
        Err(Error::EventOrder)
    );
    assert_eq!(state, paused);
    assert_eq!(
        definition.value(&state, Time::from_integer(-1)),
        Err(Error::TimeReversed)
    );
}

#[test]
fn pause_freezes_activation_age_and_reset_restarts_it() {
    let mut definition = Cycle::new(Time::ONE).unwrap();
    definition.shape = Shape::Square;
    definition.fade = Time::ONE;
    let mut state = definition.initial(Time::ZERO).unwrap();
    definition
        .command(&mut state, Time::new(1, 4).unwrap(), 0, Command::Pause)
        .unwrap();
    assert_eq!(
        definition
            .value(&state, Time::from_integer(2))
            .unwrap()
            .weight,
        0.25
    );
    definition
        .command(&mut state, Time::from_integer(2), 0, Command::Reset)
        .unwrap();
    assert_eq!(
        definition
            .value(&state, Time::from_integer(3))
            .unwrap()
            .weight,
        0.0
    );
    definition
        .command(&mut state, Time::from_integer(3), 0, Command::Resume)
        .unwrap();
    assert_eq!(
        definition
            .value(&state, Time::new(7, 2).unwrap())
            .unwrap()
            .weight,
        0.5
    );
}

#[test]
fn waveform_decisions_remain_exact_near_i64_limits() {
    let mut definition = Cycle::new(Time::ZERO).unwrap();
    definition.phase = Time::new(i64::MAX - 1, i64::MAX).unwrap();
    definition.duty = definition.phase;
    let state = definition.initial(Time::ZERO).unwrap();
    definition.shape = Shape::Square;
    assert_eq!(definition.value(&state, Time::ZERO).unwrap().value, -1.0);
    definition.shape = Shape::Triangle;
    assert_eq!(definition.value(&state, Time::ZERO).unwrap().value, 1.0);
    definition.duty = Time::ZERO;
    assert!(
        definition
            .value(&state, Time::ZERO)
            .unwrap()
            .value
            .is_finite()
    );
    definition.duty = Time::ONE;
    assert!(
        definition
            .value(&state, Time::ZERO)
            .unwrap()
            .value
            .is_finite()
    );
}

#[test]
fn reset_policies_match_trigger_transport_and_free_playback() {
    for reset in [Reset::Trigger, Reset::Transport, Reset::Free] {
        for command in [Command::NoteOn, Command::NoteOff, Command::Transport] {
            let mut definition = Cycle::new(Time::ONE).unwrap();
            definition.shape = Shape::Triangle;
            definition.reset = reset;
            let mut state = definition.initial(Time::ZERO).unwrap();
            definition
                .command(&mut state, Time::new(1, 4).unwrap(), 0, command)
                .unwrap();
            let resets = (reset == Reset::Trigger && command == Command::NoteOn)
                || (reset == Reset::Transport && command == Command::Transport);
            assert_eq!(
                definition
                    .value(&state, Time::new(1, 4).unwrap())
                    .unwrap()
                    .value,
                if resets { -1.0 } else { 0.0 }
            );
        }
    }
}

#[test]
fn transport_driver_validates_its_clock_and_rejects_state_commands() {
    let mut definition = Cycle::new(Time::ONE).unwrap();
    definition.driver = Driver::Transport;
    assert!(definition.initial(Time::ZERO).is_err());
    definition.clock = Clock::Beats;
    definition.reset = Reset::Transport;
    let mut state = definition.initial(Time::ZERO).unwrap();
    let before = state.clone();
    assert!(
        definition
            .command(&mut state, Time::ZERO, 0, Command::Pause)
            .is_err()
    );
    assert_eq!(state, before);
    definition.fade = Time::ONE;
    assert!(definition.initial(Time::ZERO).is_err());
}

#[test]
fn saturated_fade_does_not_overflow_signal_only_arithmetic() {
    let mut definition = Cycle::new(Time::ZERO).unwrap();
    definition.fade = Time::new(1, i64::MAX).unwrap();
    let state = definition.initial(Time::ZERO).unwrap();
    assert_eq!(
        definition
            .value(&state, Time::from_integer(i64::MAX))
            .unwrap()
            .weight,
        1.0
    );
}
