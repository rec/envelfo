use engelfo::approach::ExponentialApproach;
use engelfo::envelope::{Envelope, EnvelopeCommand, Segment};
use engelfo::lfo::Command;
use engelfo::time::Time;
#[path = "support/allocations.rs"]
mod allocations;
use allocations::ALLOCATIONS;

#[test]
fn captured_release_matches_shared_vectors() {
    let vectors: serde_json::Value =
        serde_json::from_str(include_str!("../conformance/captured-envelope.json")).unwrap();
    let source = &vectors["stage"];
    let approach = ExponentialApproach {
        clock_units_per_second: Time::ONE,
        asymptote: source["asymptote"].as_f64().unwrap(),
        time_constant: source["time_constant"].as_f64().unwrap(),
        minimum_time_constant: 0.0,
        threshold: source["threshold"].as_f64().unwrap(),
        transition_to: source["transition_to"].as_f64().unwrap(),
        transition_seconds: source["transition_seconds"].as_f64().unwrap(),
        minimum_transition_distance: source["minimum_transition_distance"].as_f64().unwrap(),
    };
    for case in vectors["cases"].as_array().unwrap() {
        let initial = case["initial"].as_f64().unwrap();
        let parts = approach.resolve(initial).unwrap();
        let expected: Vec<_> = case["durations_ns"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| Time::new(v.as_i64().unwrap(), 1_000_000_000).unwrap())
            .collect();
        assert_eq!(
            parts
                .iter()
                .map(|s| s.duration.unwrap())
                .collect::<Vec<_>>(),
            expected
        );
        let mut body = Envelope::new(
            initial,
            vec![Segment {
                duration: Some(Time::ONE),
                to: initial,
                curve: 0.0,
                approach: None,
            }],
        )
        .unwrap();
        body.release = vec![Segment {
            duration: Some(Time::ZERO),
            to: 0.0,
            curve: 0.0,
            approach: Some(approach.clone()),
        }];
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
            Time::ZERO,
            1,
            EnvelopeCommand::Playback(Command::NoteOff),
        )
        .unwrap();
        let midpoint = expected[0].checked_div(Time::from_integer(2)).unwrap();
        assert!(
            (body.value(&state, midpoint).unwrap().value - case["midpoint"].as_f64().unwrap())
                .abs()
                < 1e-15
        );
        #[cfg(feature = "snapshots")]
        {
            use engelfo::snapshot::Snapshot;
            let saved = Snapshot::Envelope {
                definition: body.clone(),
                state: state.clone(),
            }
            .to_json()
            .unwrap();
            let Snapshot::Envelope {
                definition,
                state: restored,
            } = Snapshot::from_json(&saved).unwrap()
            else {
                panic!()
            };
            for i in 0..101 {
                let at = Time::new(i, 100).unwrap();
                assert_eq!(
                    body.value(&state, at).unwrap(),
                    definition.value(&restored, at).unwrap()
                );
            }
        }
    }
}

#[test]
fn positive_floor_does_not_turn_zero_time_into_an_approach() {
    let mut stage = ExponentialApproach {
        clock_units_per_second: Time::ONE,
        asymptote: 0.0,
        time_constant: 0.001,
        minimum_time_constant: 0.001,
        threshold: 0.0001,
        transition_to: 0.0,
        transition_seconds: 0.05,
        minimum_transition_distance: 0.0001,
    };
    let expected = stage.resolve(1.0).unwrap();
    stage.time_constant = 0.00001;
    assert_eq!(stage.resolve(1.0).unwrap(), expected);
    stage.time_constant = 0.0;
    let parts = stage.resolve(1.0).unwrap();
    assert_eq!(parts.len(), 1);
    assert_eq!(parts[0].duration, Some(Time::new(1, 20).unwrap()));
}

#[test]
fn prepared_commands_preserve_capture_clones_and_transactionality_without_heap_operations() {
    let mut body = Envelope::new(
        1.0,
        vec![Segment {
            duration: Some(Time::ZERO),
            to: 1.0,
            curve: 0.0,
            approach: None,
        }],
    )
    .unwrap();
    body.release = vec![Segment {
        duration: Some(Time::ZERO),
        to: 0.0,
        curve: 0.0,
        approach: Some(ExponentialApproach {
            clock_units_per_second: Time::ONE,
            asymptote: 0.0,
            time_constant: 0.1 / 9.0,
            minimum_time_constant: 0.006 / 9.0,
            threshold: 0.0001,
            transition_to: 0.0,
            transition_seconds: 0.05,
            minimum_transition_distance: 0.0001,
        }),
    }];
    let mut actual = body.initial(Time::ZERO).unwrap();
    let mut storage = body.prepare_commands(&mut actual).unwrap();
    let initial = actual.clone();
    let mut expected = actual.clone();
    for (ordinal, (at, command)) in [
        (Time::ZERO, Command::NoteOn),
        (Time::new(1, 2).unwrap(), Command::NoteOff),
        (Time::new(51, 100).unwrap(), Command::Pause),
        (Time::new(52, 100).unwrap(), Command::Resume),
    ]
    .into_iter()
    .enumerate()
    {
        body.command(
            &mut expected,
            at,
            ordinal as u64,
            EnvelopeCommand::Playback(command),
        )
        .unwrap();
        ALLOCATIONS.set(Some(0));
        let result = body.command_with_storage(
            &mut actual,
            &mut storage,
            at,
            ordinal as u64,
            EnvelopeCommand::Playback(command),
        );
        let operations = ALLOCATIONS.replace(None).unwrap();
        assert_eq!(operations, 0);
        result.unwrap();
        assert_eq!(actual, expected);
        // Opaque host snapshots clone continuation and scratch outside processing.
        actual = actual.clone();
        storage = storage.clone();
    }
    let before = actual.clone();
    ALLOCATIONS.set(Some(0));
    let failed = body.command_with_storage(
        &mut actual,
        &mut storage,
        Time::ZERO,
        4,
        EnvelopeCommand::Playback(Command::NoteOff),
    );
    let operations = ALLOCATIONS.replace(None).unwrap();
    assert_eq!(operations, 0);
    assert!(failed.is_err());
    assert_eq!(actual, before);
    ALLOCATIONS.set(Some(0));
    actual.clone_from(&initial);
    let result = body.command_with_storage(
        &mut actual,
        &mut storage,
        Time::ZERO,
        0,
        EnvelopeCommand::Playback(Command::NoteOn),
    );
    let operations = ALLOCATIONS.replace(None).unwrap();
    assert_eq!(operations, 0);
    result.unwrap();
    let before = actual.clone();
    body.release[0].approach.as_mut().unwrap().time_constant = 1e300;
    ALLOCATIONS.set(Some(0));
    let result = body.command_with_storage(
        &mut actual,
        &mut storage,
        Time::ONE,
        1,
        EnvelopeCommand::Playback(Command::NoteOff),
    );
    let operations = ALLOCATIONS.replace(None).unwrap();
    assert_eq!(operations, 0);
    assert!(result.is_err());
    assert_eq!(actual, before);
    body.release.resize(32, body.release[0].clone());
    ALLOCATIONS.set(Some(0));
    let result = body.command_with_storage(
        &mut actual,
        &mut storage,
        Time::ONE,
        1,
        EnvelopeCommand::Playback(Command::NoteOff),
    );
    let operations = ALLOCATIONS.replace(None).unwrap();
    assert_eq!(operations, 0);
    assert_eq!(result, Err(engelfo::Error::Capacity));
    assert_eq!(actual, before);
}
