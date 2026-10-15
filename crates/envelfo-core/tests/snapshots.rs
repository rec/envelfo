#![cfg(feature = "snapshots")]

use envelfo_core::envelope::{Envelope, EnvelopeCommand, Marker, Playback, Segment};
use envelfo_core::lfo::{Command, LFO};
use envelfo_core::patch::{
    Child, Connection, ConnectionAction, Node, Output, Patch, Port, Seconds, StreamKeys,
};
use envelfo_core::sample_hold::{SampleCommand, SampleHold};
use envelfo_core::signal::Operator;
use envelfo_core::snapshot::{Error, Snapshot};
use envelfo_core::stages::{Action, Body, Event, Stage, StageCommand, Stages, Transition};
use envelfo_core::time::Time;
use serde_json::{Value, json};

#[test]
fn lfo_continuation_preserves_large_times_ordinals_and_playback_commands() {
    let body = LFO::new(t(1, 3)).unwrap();
    let at = Time::from_integer(9_007_199_254_740_993);
    let mut state = body.initial(at).unwrap();
    for (i, command) in [Command::Shift(t(1, 7)), Command::Reverse, Command::Pause]
        .into_iter()
        .enumerate()
    {
        body.command(&mut state, at, u64::MAX - 2 + i as u64, command)
            .unwrap();
    }
    let json = Snapshot::LFO {
        definition: body.clone(),
        state: state.clone(),
    }
    .to_json()
    .unwrap();
    assert!(json.contains("9007199254740993/1"));
    assert!(json.contains("18446744073709551615"));
    let Snapshot::LFO {
        definition,
        state: mut restored,
    } = Snapshot::from_json(&json).unwrap()
    else {
        unreachable!()
    };
    let next = at.checked_add(t(1, 5)).unwrap();
    assert_eq!(definition.value(&restored, next), body.value(&state, next));
    definition
        .command(&mut restored, next, 0, Command::Resume)
        .unwrap();
    body.command(&mut state, next, 0, Command::Resume).unwrap();
    assert_eq!(restored, state);
    assert_eq!(
        definition.value(&restored, next.checked_add(Time::ONE).unwrap()),
        body.value(&state, next.checked_add(Time::ONE).unwrap())
    );
}

#[test]
fn envelope_continuation_preserves_waits_release_and_segment_cursor() {
    let mut body = Envelope::new(
        0.0,
        vec![
            segment(Some(Time::ONE), 0.8),
            segment(None, 0.0),
            segment(Some(Time::ONE), 0.5),
        ],
    )
    .unwrap();
    body.release = vec![segment(None, 0.0), segment(Some(Time::ONE), 0.0)];
    let mut state = body.initial(Time::ZERO).unwrap();
    for (at, command) in [
        (Time::ZERO, EnvelopeCommand::Playback(Command::NoteOn)),
        (t(3, 1), EnvelopeCommand::Advance),
        (t(7, 2), EnvelopeCommand::Playback(Command::NoteOff)),
        (t(4, 1), EnvelopeCommand::Advance),
        (t(9, 2), EnvelopeCommand::Playback(Command::Pause)),
        (t(5, 1), EnvelopeCommand::Playback(Command::Resume)),
    ] {
        let Snapshot::Envelope {
            definition,
            state: mut restored,
        } = roundtrip(Snapshot::Envelope {
            definition: body.clone(),
            state: state.clone(),
        })
        else {
            unreachable!()
        };
        body.command(&mut state, at, 0, command).unwrap();
        definition.command(&mut restored, at, 0, command).unwrap();
        assert_eq!(restored, state);
        assert_eq!(definition.value(&restored, at), body.value(&state, at));
        assert_eq!(definition.complete_at(&restored), body.complete_at(&state));
    }
    roundtrip(Snapshot::Envelope {
        definition: body,
        state,
    });
}

#[test]
fn stages_continuation_preserves_boundary_order_completion_and_cue_reentry() {
    let mut envelope = Envelope::new(0.0, vec![segment(Some(Time::ONE), 1.0)]).unwrap();
    envelope.playback = Playback::PingPong;
    envelope.repeat_count = Some(3);
    envelope.markers = vec![Marker {
        name: "middle".into(),
        position: t(1, 2),
    }];
    let body = Stages {
        initial_stage: "repeat".into(),
        stages: vec![Stage {
            name: "repeat".into(),
            body: Body::Envelope(envelope),
            markers: vec![],
        }],
        transitions: vec![Transition {
            from: vec!["repeat".into()],
            event: "cue.again".into(),
            action: Action::Enter("repeat".into()),
        }],
    };
    let mut state = body.initial(Time::ZERO).unwrap();
    for at in [t(1, 2), Time::ONE, t(2, 1), t(3, 1), t(4, 1)] {
        let Snapshot::Stages {
            definition,
            state: mut restored,
        } = roundtrip(Snapshot::Stages {
            definition: body.clone(),
            state: state.clone(),
        })
        else {
            unreachable!()
        };
        assert_eq!(
            definition.advance(&mut restored, at, None),
            body.advance(&mut state, at, None)
        );
        assert_eq!(restored, state);
    }
    assert!(state.completed());
    let Snapshot::Stages {
        definition,
        state: mut restored,
    } = roundtrip(Snapshot::Stages {
        definition: body.clone(),
        state: state.clone(),
    })
    else {
        unreachable!()
    };
    let event = Event {
        at: t(4, 1),
        ordinal: 0,
        command: StageCommand::Cue("again".into()),
    };
    assert_eq!(
        definition.advance(&mut restored, event.at, Some(&event)),
        body.advance(&mut state, event.at, Some(&event))
    );
}

#[test]
fn random_continuation_restores_every_word_and_float_without_redrawing() {
    let body = SampleHold {
        minimum: -0.83,
        maximum: 0.79,
    };
    let mut state = body.initial(Time::ZERO, u64::MAX).unwrap();
    for ordinal in 0..1024 {
        let Snapshot::SampleHold {
            definition,
            state: mut restored,
        } = roundtrip(Snapshot::SampleHold {
            definition: body,
            state: state.clone(),
        })
        else {
            unreachable!()
        };
        definition
            .command(&mut restored, Time::ZERO, ordinal, SampleCommand::Sample)
            .unwrap();
        body.command(&mut state, Time::ZERO, ordinal, SampleCommand::Sample)
            .unwrap();
        assert_eq!(restored, state);
    }
}

#[test]
fn patch_continuation_retains_pending_deliveries_gates_inputs_and_signal_memory() {
    let body = patch();
    let keys = StreamKeys {
        samples: [("random".into(), u64::MAX)].into(),
        connections: vec![u64::MAX, 123, 456],
    };
    let mut state = body
        .initial(Time::ZERO, "portable", u64::MAX, Some(&keys), &Seconds)
        .unwrap();
    let mut saw_pending = false;
    for i in 0..48 {
        let Snapshot::Patch {
            definition,
            state: mut restored,
        } = roundtrip(Snapshot::Patch {
            definition: body.clone(),
            state: state.clone(),
        })
        else {
            unreachable!()
        };
        let at = t(i, 4);
        let input = if i % 8 < 4 { -0.8 } else { 0.8 };
        body.set_input(&mut state, "input", input).unwrap();
        definition.set_input(&mut restored, "input", input).unwrap();
        assert_eq!(
            definition
                .advance(
                    &mut restored,
                    at,
                    at.checked_add(t(1, 8)).unwrap(),
                    &Seconds
                )
                .unwrap(),
            body.advance(&mut state, at, at.checked_add(t(1, 8)).unwrap(), &Seconds)
                .unwrap()
        );
        assert_eq!(restored, state);
        saw_pending |= state.pending_count() > 0;
        roundtrip(Snapshot::Patch {
            definition: body.clone(),
            state: state.clone(),
        });
        // Save between host phases, without creating an extra signal observation.
        body.settle(
            &mut state,
            at.checked_add(t(1, 8)).unwrap(),
            false,
            &Seconds,
        )
        .unwrap();
    }
    assert!(saw_pending);
}

#[test]
fn malformed_documents_and_unknown_versions_are_rejected() {
    let body = LFO::new(Time::ONE).unwrap();
    let valid = Snapshot::LFO {
        state: body.initial(Time::ZERO).unwrap(),
        definition: body,
    }
    .to_json()
    .unwrap();
    let mut document: Value = serde_json::from_str(&valid).unwrap();
    document["version"] = json!(2);
    assert!(matches!(
        Snapshot::from_json(&document.to_string()),
        Err(Error::Version(2))
    ));
    for (pointer, value) in [
        ("/version", json!(null)),
        ("/unexpected", json!(true)),
        ("/snapshot/unexpected", json!(true)),
        ("/snapshot/kind", json!("Hold")),
        ("/snapshot/definition/rate", json!("1/0")),
        ("/snapshot/definition/rate", json!("9223372036854775808/1")),
        ("/snapshot/definition/rate", json!(0.5)),
        ("/snapshot/definition/depth", json!(-1)),
        ("/snapshot/state/position/rate", json!("-1/1")),
        ("/snapshot/state/position/age", json!("-1/1")),
        ("/snapshot/state/ordinal", json!(18446744073709551616.0)),
        ("/snapshot/state/position/paused", json!(null)),
    ] {
        let mut document: Value = serde_json::from_str(&valid).unwrap();
        let (parent, field) = pointer.rsplit_once('/').unwrap();
        document
            .pointer_mut(parent)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert(field.into(), value);
        assert!(
            Snapshot::from_json(&document.to_string()).is_err(),
            "{pointer}"
        );
    }
    for text in [
        "",
        "null",
        "{}",
        &valid[..valid.len() - 1],
        &format!("{valid} trailing"),
    ] {
        assert!(Snapshot::from_json(text).is_err());
    }
}

#[test]
fn incompatible_state_and_definition_pairs_are_rejected_on_save_and_load() {
    let body = patch();
    let mut state = body.initial(Time::ZERO, "test", 0, None, &Seconds).unwrap();
    body.set_input(&mut state, "input", 0.0).unwrap();
    let valid = Snapshot::Patch {
        definition: body.clone(),
        state: state.clone(),
    }
    .to_json()
    .unwrap();
    let mut incompatible = body;
    incompatible.children[0].node = Node::SampleHold(SampleHold {
        minimum: 0.0,
        maximum: 1.0,
    });
    assert!(
        Snapshot::Patch {
            definition: incompatible,
            state
        }
        .to_json()
        .is_err()
    );
    for (pointer, value) in [
        ("/snapshot/state/children", json!([])),
        ("/snapshot/state/order", json!([999])),
        ("/snapshot/state/retained", json!([])),
        ("/snapshot/state/inputs/0", json!(0.5)),
        ("/snapshot/state/gates/0/count", json!(999)),
        ("/snapshot/state/children/0", json!(null)),
        ("/snapshot/state/observed_at", json!("1/1")),
        (
            "/snapshot/state/pending",
            json!([{"at":"0/1","order":999,"target":0,"action":"Sample"}]),
        ),
        ("/snapshot/definition/children/0/name", json!("input")),
    ] {
        let mut document: Value = serde_json::from_str(&valid).unwrap();
        *document.pointer_mut(pointer).unwrap() = value;
        assert!(
            Snapshot::from_json(&document.to_string()).is_err(),
            "{pointer}"
        );
    }
}

#[test]
fn version_one_sample_format_preserves_signed_rational_limits() {
    let body = SampleHold {
        minimum: 0.0,
        maximum: 0.0,
    };
    let state = body
        .initial(Time::new(i64::MIN, i64::MAX).unwrap(), 0)
        .unwrap();
    let expected = json!({
        "version": 1,
        "snapshot": {
            "kind": "SampleHold",
            "definition": {"minimum": 0.0, "maximum": 0.0},
            "state": {
                "at": "-9223372036854775808/9223372036854775807",
                "ordinal": null,
                "value": 0.0,
                "random": 11400714819323198485_u64,
            },
        },
    });
    let snapshot = Snapshot::SampleHold {
        definition: body,
        state,
    };
    assert_eq!(
        serde_json::from_str::<Value>(&snapshot.to_json().unwrap()).unwrap(),
        expected
    );
    assert_eq!(
        Snapshot::from_json(&expected.to_string()).unwrap(),
        snapshot
    );
}

#[test]
fn invalid_envelope_stage_and_sample_cursors_are_rejected() {
    let mut envelope = Envelope::new(0.0, vec![segment(Some(Time::ONE), 1.0)]).unwrap();
    envelope.playback = Playback::Loop;
    envelope.repeat_count = Some(2);
    let stages = Stages {
        initial_stage: "loop".into(),
        stages: vec![Stage {
            name: "loop".into(),
            body: Body::Envelope(envelope.clone()),
            markers: vec![],
        }],
        transitions: vec![],
    };
    let sample = SampleHold {
        minimum: 0.0,
        maximum: 0.5,
    };
    for (snapshot, mutations) in [
        (
            Snapshot::Envelope {
                state: envelope.initial(Time::ZERO).unwrap(),
                definition: envelope,
            },
            vec![
                ("/snapshot/state/segment_index", json!(2)),
                ("/snapshot/state/traversals", json!(2)),
                ("/snapshot/state/complete_coordinate", json!("1/1")),
                ("/snapshot/state/phase", json!("Release")),
                ("/snapshot/state/start_value", json!(-0.5)),
            ],
        ),
        (
            Snapshot::Stages {
                state: stages.initial(Time::ZERO).unwrap(),
                definition: stages,
            },
            vec![
                ("/snapshot/state/stage", json!(999)),
                ("/snapshot/state/segment_index", json!(999)),
                ("/snapshot/state/cursor", json!(999)),
                ("/snapshot/state/entry_value", json!(2.0)),
                ("/snapshot/state/completed", json!(true)),
                ("/snapshot/state/traversals", json!(3)),
                ("/snapshot/state/position/at", json!("1/1")),
            ],
        ),
        (
            Snapshot::SampleHold {
                state: sample.initial(Time::ZERO, 0).unwrap(),
                definition: sample,
            },
            vec![
                ("/snapshot/state/value", json!(0.6)),
                ("/snapshot/state/value", json!(null)),
            ],
        ),
    ] {
        let valid = snapshot.to_json().unwrap();
        for (pointer, value) in mutations {
            let mut document: Value = serde_json::from_str(&valid).unwrap();
            *document.pointer_mut(pointer).unwrap() = value;
            assert!(
                Snapshot::from_json(&document.to_string()).is_err(),
                "{pointer}"
            );
        }
    }
}

fn roundtrip(snapshot: Snapshot) -> Snapshot {
    let restored = Snapshot::from_json(&snapshot.to_json().unwrap()).unwrap();
    assert_eq!(restored, snapshot);
    restored
}

fn t(numerator: i64, denominator: i64) -> Time {
    Time::new(numerator, denominator).unwrap()
}

fn segment(duration: Option<Time>, to: f64) -> Segment {
    Segment {
        duration,
        to,
        curve: 0.0,
    }
}

fn patch() -> Patch {
    Patch {
        children: vec![
            Child {
                name: "clock".into(),
                node: Node::LFO {
                    body: LFO::new(Time::ONE).unwrap(),
                    markers: vec![Marker {
                        name: "tick".into(),
                        position: t(1, 4),
                    }],
                },
            },
            Child {
                name: "input".into(),
                node: Node::Input,
            },
            Child {
                name: "random".into(),
                node: Node::SampleHold(SampleHold {
                    minimum: 0.0,
                    maximum: 1.0,
                }),
            },
            Child {
                name: "capture".into(),
                node: Node::Signal(Operator::Latch("random".into())),
            },
            Child {
                name: "slew".into(),
                node: Node::Signal(Operator::Slew {
                    input: "input".into(),
                    rise: 4.0,
                    fall: 4.0,
                    initial: None,
                }),
            },
            Child {
                name: "edge".into(),
                node: Node::Signal(Operator::Threshold {
                    input: "slew".into(),
                    lower: -0.25,
                    upper: 0.25,
                }),
            },
        ],
        outputs: ["random", "capture", "slew", "edge"]
            .into_iter()
            .map(|n| Output {
                name: n.into(),
                source: n.into(),
            })
            .collect(),
        event_outputs: vec![],
        connections: vec![
            Connection {
                source: Port {
                    child: "clock".into(),
                    port: "tick".into(),
                },
                target: "random".into(),
                action: ConnectionAction::Sample,
                every: 1,
                offset: 0,
                probability: 1.0,
                delay: t(1, 2),
            },
            Connection {
                source: Port {
                    child: "clock".into(),
                    port: "tick".into(),
                },
                target: "random".into(),
                action: ConnectionAction::Sample,
                every: 3,
                offset: 7,
                probability: 0.5,
                delay: t(1, 10),
            },
            Connection {
                source: Port {
                    child: "edge".into(),
                    port: "rising".into(),
                },
                target: "capture".into(),
                action: ConnectionAction::Capture,
                every: 1,
                offset: 0,
                probability: 1.0,
                delay: Time::ZERO,
            },
        ],
    }
}
