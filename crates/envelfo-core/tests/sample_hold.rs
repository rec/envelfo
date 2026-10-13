use envelfo_core::Error;
use envelfo_core::cycle::Command;
use envelfo_core::random::random_word;
use envelfo_core::sample_hold::{SampleCommand, SampleHold};
use envelfo_core::time::Time;

#[test]
fn only_sampling_draws_a_word_and_cloning_preserves_the_stream() {
    let body = SampleHold {
        minimum: -1.0,
        maximum: 1.0,
    };
    let mut state = body.initial(Time::ZERO, 23).unwrap();
    let mut random = 23;
    let initial = -1.0 + 2.0 * (random_word(&mut random) >> 11) as f64 / (1_u64 << 53) as f64;
    assert_eq!(body.value(&state, Time::ZERO).unwrap().value, initial);
    for (ordinal, command) in [
        Command::NoteOn,
        Command::Pause,
        Command::Reverse,
        Command::Seek(Time::ONE),
        Command::Resume,
        Command::NoteOff,
    ]
    .into_iter()
    .enumerate()
    {
        body.command(
            &mut state,
            Time::ONE,
            ordinal as u64,
            SampleCommand::Playback(command),
        )
        .unwrap();
        assert_eq!(body.value(&state, Time::ONE).unwrap().value, initial);
    }
    let saved = state.clone();
    body.command(&mut state, Time::ONE, 6, SampleCommand::Sample)
        .unwrap();
    assert_eq!(
        body.value(&state, Time::ONE).unwrap().value,
        -1.0 + 2.0 * (random_word(&mut random) >> 11) as f64 / (1_u64 << 53) as f64
    );
    let mut restored = saved;
    body.command(&mut restored, Time::ONE, 6, SampleCommand::Sample)
        .unwrap();
    assert_eq!(restored, state);
}

#[test]
fn invalid_commands_and_order_leave_the_sample_unchanged() {
    let body = SampleHold {
        minimum: 0.5,
        maximum: 0.5,
    };
    let mut state = body.initial(Time::ONE, 0).unwrap();
    let saved = state.clone();
    assert_eq!(
        body.command(&mut state, Time::ZERO, 0, SampleCommand::Sample),
        Err(Error::EventOrder)
    );
    assert!(
        body.command(
            &mut state,
            Time::ONE,
            0,
            SampleCommand::Playback(Command::Rate(Time::ONE))
        )
        .is_err()
    );
    assert_eq!(state, saved);
    assert_eq!(body.value(&state, Time::ONE).unwrap().value, 0.5);
    assert!(
        SampleHold {
            minimum: 1.0,
            maximum: 0.0
        }
        .initial(Time::ZERO, 0)
        .is_err()
    );
}
