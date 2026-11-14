use envelfo_core::approach::ExponentialApproach;
use envelfo_core::envelope::{Envelope, EnvelopeCommand, Segment};
use envelfo_core::lfo::Command;
use envelfo_core::time::Time;

#[test]
fn captured_release_matches_shared_vectors() {
    let vectors: serde_json::Value =
        serde_json::from_str(include_str!("../../../conformance/captured-envelope.json")).unwrap();
    let source = &vectors["stage"];
    let approach = ExponentialApproach {
        clock_units_per_second: Time::ONE,
        asymptote: source["asymptote"].as_f64().unwrap(),
        time_constant: source["time_constant"].as_f64().unwrap(),
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
            use envelfo_core::snapshot::Snapshot;
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
