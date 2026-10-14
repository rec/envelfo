use envelfo_core::Error;
use envelfo_core::envelope::{Envelope, Segment, Start};
use envelfo_core::lfo::{Command, LFO, Shape};
use envelfo_core::patch::{
    Child, ChildCommand, ChildEvent, ClockMapping, Connection, ConnectionAction, EventOutput, Node,
    Output, Patch, Port, Seconds,
};
use envelfo_core::random::{random_word, stream_key};
use envelfo_core::sample_hold::SampleHold;
use envelfo_core::signal::Operator;
use envelfo_core::stages::{Action, Body, Stage, Stages, Transition};
use envelfo_core::time::Time;

#[test]
fn delayed_random_samples_and_cloned_continuation_keep_exact_deliveries() {
    let mut body = clock_patch(ConnectionAction::Sample);
    body.connections[0].delay = t(1, 10);
    let mut state = body.initial(Time::ZERO, "gesture", 81, &Seconds).unwrap();
    let initial = state.clone();
    let first = body
        .advance(&mut state, t(1, 4), t(3, 10), &Seconds)
        .unwrap();
    assert_eq!(first.events[0].at, t(1, 4));
    assert_eq!(state.next_delivery(), Some(t(7, 20)));
    let restored = state.clone();
    assert_eq!(
        observe(&body, &mut state, t(1, 3)).values["value"],
        first.values["value"]
    );
    let after = observe(&body, &mut state, t(7, 20));
    let mut random = 81 ^ stream_key(0, "patch-gesture-held:sample-hold");
    random_word(&mut random);
    let expected = (random_word(&mut random) >> 11) as f64 / (1_u64 << 53) as f64;
    assert_eq!(after.values["value"], expected);
    assert_eq!(after.values["second"], 1.0 + 2.0 * expected);
    assert_eq!(state.pending_count(), 0);
    assert_eq!(initial.pending_count(), 0);
    let mut restored = restored;
    assert_eq!(observe(&body, &mut restored, t(7, 20)), after);
}

#[test]
fn every_offset_and_chance_gates_match_independent_portable_streams() {
    for (every, offset) in [(1, 0), (3, 0), (3, 2), (3, 5)] {
        for probability in [0.0, 0.5, 1.0] {
            let mut body = clock_patch(ConnectionAction::Sample);
            let connection = &mut body.connections[0];
            connection.every = every;
            connection.offset = offset;
            connection.probability = probability;
            let mut state = body.initial(Time::ZERO, "patch", 19, &Seconds).unwrap();
            let mut sample_random = 19 ^ stream_key(0, "patch-patch-held:sample-hold");
            let mut expected =
                (random_word(&mut sample_random) >> 11) as f64 / (1_u64 << 53) as f64;
            let mut gate_random = 19 ^ stream_key(0, "patch-patch-held:start-0");
            for index in 0..20 {
                let eligible = index >= offset && (index - offset) % every == 0;
                let forward = eligible
                    && match probability {
                        0.0 => false,
                        1.0 => true,
                        _ => random_word(&mut gate_random) >> 11 < 1_u64 << 52,
                    };
                if forward {
                    expected =
                        (random_word(&mut sample_random) >> 11) as f64 / (1_u64 << 53) as f64;
                }
                let result = observe(
                    &body,
                    &mut state,
                    Time::from_integer(index as i64)
                        .checked_add(t(1, 4))
                        .unwrap(),
                );
                assert_eq!(
                    result.values["value"], expected,
                    "every={every} offset={offset} probability={probability} at {index}"
                );
            }
        }
    }
}

#[test]
fn natural_connections_are_partition_independent_without_inventing_observations() {
    for action in [
        ConnectionAction::Start,
        ConnectionAction::Sample,
        ConnectionAction::Capture,
    ] {
        let mut body = clock_patch(action.clone());
        let connection = &mut body.connections[0];
        connection.delay = t(1, 10);
        connection.every = 3;
        connection.offset = 2;
        connection.probability = 0.5;
        let mut initial = body.initial(Time::ZERO, "patch", 18, &Seconds).unwrap();
        observe(&body, &mut initial, Time::ZERO);
        let mut whole = initial.clone();
        let expected = observe(&body, &mut whole, t(9, 2));
        let mut split = initial;
        let mut events = vec![];
        let mut values = None;
        for at in [t(1, 7), t(1, 4), t(7, 20), t(3, 2), t(25, 8), t(9, 2)] {
            let result = observe(&body, &mut split, at);
            events.extend(result.events);
            values = Some(result.values);
        }
        assert_eq!(events, expected.events);
        assert_eq!(split.pending_count(), whole.pending_count());
        if action != ConnectionAction::Capture {
            assert_eq!(values.unwrap(), expected.values);
        }
    }
}

#[test]
fn capture_reads_the_next_observation_after_delivery_not_the_emission_value() {
    let mut body = clock_patch(ConnectionAction::Capture);
    body.connections[0].delay = t(1, 10);
    let mut state = body.initial(Time::ZERO, "patch", 0, &Seconds).unwrap();
    assert_eq!(
        observe(&body, &mut state, Time::ZERO).values["capture"],
        -1.0
    );
    assert_eq!(observe(&body, &mut state, t(1, 4)).values["capture"], -1.0);
    let delivered = observe(&body, &mut state, t(2, 5));
    assert!((delivered.values["capture"] - 0.6).abs() < 1e-12);
    assert_eq!(
        observe(&body, &mut state, t(1, 2)).values["capture"],
        delivered.values["capture"]
    );
}

#[test]
fn threshold_edges_wait_for_host_delivery_and_then_seconds_delay() {
    let mut body = clock_patch(ConnectionAction::Sample);
    body.children.push(child(
        "edge",
        Node::Signal(Operator::Threshold {
            input: "clock".into(),
            lower: -0.25,
            upper: 0.25,
        }),
    ));
    body.event_outputs[0] = EventOutput {
        name: "rise".into(),
        source: port("edge", "rising"),
    };
    body.connections[0].source = port("edge", "rising");
    body.connections[0].delay = t(1, 10);
    let mut state = body.initial(Time::ZERO, "patch", 0, &Seconds).unwrap();
    assert!(observe(&body, &mut state, Time::ZERO).events.is_empty());
    let result = body
        .advance(&mut state, t(2, 5), t(1, 2), &Seconds)
        .unwrap();
    assert_eq!(result.events[0].at, t(1, 2));
    assert_eq!(state.next_delivery(), Some(t(3, 5)));
    assert_eq!(
        observe(&body, &mut state, t(11, 20)).values["value"],
        result.values["value"]
    );
    assert_ne!(
        observe(&body, &mut state, t(3, 5)).values["value"],
        result.values["value"]
    );
}

#[test]
fn external_seek_commands_do_not_add_observations_or_replay_lfo_markers() {
    let body = clock_patch(ConnectionAction::Sample);
    let mut state = body.initial(Time::ZERO, "patch", 0, &Seconds).unwrap();
    assert!(
        body.command(
            &mut state,
            "clock",
            &event(t(1, 8), 0, ChildCommand::Playback(Command::Seek(t(1, 2)))),
            &Seconds
        )
        .unwrap()
        .is_empty()
    );
    assert_eq!(state.observed_at(), None);
    assert!(observe(&body, &mut state, t(1, 4)).events.is_empty());
}

#[test]
fn staged_cues_route_completion_and_preserve_partitioned_continuation() {
    let body = staged_patch();
    let initial = body.initial(Time::ZERO, "patch", 0, &Seconds).unwrap();
    let mut split = initial.clone();
    let first = observe(&body, &mut split, t(1, 2));
    assert_eq!(first.events[0].at, t(1, 2));
    assert_eq!(first.events[0].port, "finished");
    assert_eq!(split.next_delivery(), Some(t(3, 5)));
    let mut split = split.clone();
    let second = observe(&body, &mut split, Time::ONE);
    assert_eq!(second.values["value"], 1.0);
    assert_eq!(second.events[0].at, t(7, 10));
    let mut whole = initial;
    let expected = observe(&body, &mut whole, Time::ONE);
    let mut events = first.events;
    events.extend(second.events);
    assert_eq!(events, expected.events);
    assert_eq!(split, whole);
}

#[test]
fn child_command_at_completion_preempts_obsolete_routed_event() {
    let body = staged_patch();
    let mut state = body.initial(Time::ZERO, "patch", 0, &Seconds).unwrap();
    let emitted = body
        .command(
            &mut state,
            "attack",
            &event(t(1, 2), 0, ChildCommand::Playback(Command::NoteOff)),
            &Seconds,
        )
        .unwrap();
    assert!(emitted.is_empty());
    assert_eq!(state.pending_count(), 0);
    assert_eq!(observe(&body, &mut state, Time::ONE).values["value"], 0.0);
}

#[test]
fn elapsed_beat_boundaries_ignore_song_seeks_and_delays_remain_host_seconds() {
    let body = staged_patch();
    let mut state = body.initial(Time::ZERO, "patch", 0, &PausedBeats).unwrap();
    let result = body
        .advance(&mut state, t(7, 8), Time::ONE, &PausedBeats)
        .unwrap();
    assert_eq!(result.events[0].at, t(7, 8));
    assert_eq!(state.next_delivery(), Some(t(39, 40)));
    assert_eq!(result.values["value"], 0.0);
    assert_eq!(
        body.advance(&mut state, Time::ONE, t(11, 10), &PausedBeats)
            .unwrap()
            .values["value"],
        1.0
    );
    assert_eq!(state.pending_count(), 0);
}

#[test]
fn slew_uses_seconds_while_transport_is_stopped_and_cues_keep_host_timestamps() {
    let body = Patch {
        children: vec![
            child(
                "constant",
                Node::LFO {
                    body: LFO {
                        center: 1.0,
                        depth: 0.0,
                        ..LFO::new(Time::ZERO).unwrap()
                    },
                    markers: vec![],
                },
            ),
            child(
                "slew",
                Node::Signal(Operator::Slew {
                    input: "constant".into(),
                    rise: 2.0,
                    fall: 1.0,
                    initial: Some(0.0),
                }),
            ),
        ],
        outputs: vec![output("value", "slew")],
        event_outputs: vec![],
        connections: vec![],
    };
    let mut state = body.initial(Time::ZERO, "patch", 0, &Stopped).unwrap();
    assert_eq!(
        body.advance(&mut state, Time::ZERO, t(1, 10), &Stopped)
            .unwrap()
            .values["value"],
        0.0
    );
    assert_eq!(
        body.advance(&mut state, t(1, 4), t(1, 2), &Stopped)
            .unwrap()
            .values["value"],
        0.5
    );
    let mut body = staged_patch();
    if let Node::Stages(stages) = &mut body.children[0].node {
        stages.transitions.push(Transition {
            from: vec!["on".into()],
            event: "cue.finish".into(),
            action: Action::Finish,
        });
    }
    body.connections[0].source = port("attack", "done");
    body.event_outputs[0].source = port("attack", "done");
    let mut state = body.initial(Time::ZERO, "patch", 0, &Stopped).unwrap();
    let emitted = body
        .command(
            &mut state,
            "attack",
            &event(Time::ONE, 0, ChildCommand::Cue("finish".into())),
            &Stopped,
        )
        .unwrap();
    assert_eq!(emitted[0].at, Time::ONE);
    assert_eq!(state.next_delivery(), Some(t(11, 10)));
    assert_eq!(
        body.advance(&mut state, t(11, 10), t(6, 5), &Stopped)
            .unwrap()
            .values["value"],
        1.0
    );
}

#[test]
fn same_time_connections_follow_declaration_order_not_target_name_order() {
    let mut body = clock_patch(ConnectionAction::Start);
    body.children
        .push(child("another", Node::Envelope(attack())));
    body.connections.push(Connection {
        target: "another".into(),
        ..body.connections[0].clone()
    });
    body.connections.push(body.connections[0].clone());
    let mut state = body.initial(Time::ZERO, "patch", 0, &Seconds).unwrap();
    observe(&body, &mut state, t(1, 4));
    let saved = state.clone();
    assert_eq!(
        body.command(
            &mut state,
            "held",
            &event(t(1, 4), 1, ChildCommand::Start),
            &Seconds
        ),
        Err(Error::EventOrder)
    );
    assert_eq!(state, saved);
    assert!(
        body.command(
            &mut state,
            "held",
            &event(t(1, 4), 2, ChildCommand::Start),
            &Seconds
        )
        .is_ok()
    );
}

#[test]
fn zero_delay_cues_execute_in_connection_order_and_route_cascaded_done_once() {
    for finish_first in [false, true] {
        let mut body = clock_patch(ConnectionAction::Sample);
        let receiver = staged_patch().children.remove(1);
        body.children.push(receiver);
        if let Node::Stages(stages) = &mut body.children.last_mut().unwrap().node {
            stages.transitions.push(Transition {
                from: vec!["waiting".into(), "high".into()],
                event: "cue.finish".into(),
                action: Action::Finish,
            });
        }
        let first = Connection {
            source: port("clock", "tick"),
            target: "receiver".into(),
            action: ConnectionAction::Cue("go".into()),
            every: 1,
            offset: 0,
            probability: 1.0,
            delay: Time::ZERO,
        };
        let second = Connection {
            action: ConnectionAction::Cue("finish".into()),
            ..first.clone()
        };
        body.connections = if finish_first {
            vec![second, first]
        } else {
            vec![first, second]
        };
        body.connections.push(Connection {
            source: port("receiver", "done"),
            target: "held".into(),
            action: ConnectionAction::Sample,
            every: 1,
            offset: 0,
            probability: 1.0,
            delay: Time::ZERO,
        });
        body.outputs.push(output("receiver", "receiver"));
        body.event_outputs.push(EventOutput {
            name: "done".into(),
            source: port("receiver", "done"),
        });
        let mut state = body.initial(Time::ZERO, "patch", 0, &Seconds).unwrap();
        let initial = observe(&body, &mut state, Time::ZERO);
        let result = observe(&body, &mut state, t(1, 4));
        assert_eq!(
            result.values["receiver"],
            if finish_first { 0.0 } else { 1.0 }
        );
        assert_eq!(
            result
                .events
                .iter()
                .map(|e| e.port.as_str())
                .collect::<Vec<_>>(),
            ["tick", "done"]
        );
        assert_eq!(result.events[1].activation, u64::from(!finish_first));
        assert_ne!(result.values["value"], initial.values["value"]);
        assert_eq!(state.pending_count(), 0);
    }
}

#[test]
fn host_clock_failure_rolls_back_deliveries_and_random_draws() {
    struct Failing;
    impl ClockMapping for Failing {
        fn local_time(&self, at: Time) -> Result<Time, Error> {
            Ok(at)
        }
        fn host_time(&self, _: Time) -> Result<Option<Time>, Error> {
            Err(Error::Overflow)
        }
    }
    let body = clock_patch(ConnectionAction::Sample);
    let mut state = body.initial(Time::ZERO, "patch", 0, &Seconds).unwrap();
    let saved = state.clone();
    assert_eq!(
        body.advance(&mut state, Time::ONE, Time::from_integer(2), &Failing),
        Err(Error::Overflow)
    );
    assert_eq!(state, saved);
}

#[test]
fn lfo_pause_reverse_and_rate_commands_retime_markers_exactly() {
    let body = clock_patch(ConnectionAction::Sample);
    let mut state = body.initial(Time::ZERO, "patch", 0, &Seconds).unwrap();
    body.command(
        &mut state,
        "clock",
        &event(t(1, 8), 0, ChildCommand::Playback(Command::Pause)),
        &Seconds,
    )
    .unwrap();
    assert!(observe(&body, &mut state, t(1, 2)).events.is_empty());
    body.command(
        &mut state,
        "clock",
        &event(t(1, 2), 0, ChildCommand::Playback(Command::Resume)),
        &Seconds,
    )
    .unwrap();
    body.command(
        &mut state,
        "clock",
        &event(
            t(1, 2),
            1,
            ChildCommand::Playback(Command::Rate(Time::from_integer(2))),
        ),
        &Seconds,
    )
    .unwrap();
    assert_eq!(observe(&body, &mut state, t(9, 16)).events[0].at, t(9, 16));
    body.command(
        &mut state,
        "clock",
        &event(t(5, 8), 0, ChildCommand::Playback(Command::Reverse)),
        &Seconds,
    )
    .unwrap();
    assert_eq!(
        observe(&body, &mut state, t(11, 16)).events[0].at,
        t(11, 16)
    );
}

#[test]
fn invalid_schedules_commands_overflow_and_capacity_leave_snapshot_unchanged() {
    let body = clock_patch(ConnectionAction::Sample);
    let mut state = body.initial(Time::ZERO, "patch", 0, &Seconds).unwrap();
    let saved = state.clone();
    assert!(
        body.advance(&mut state, Time::ZERO, Time::ZERO, &Seconds)
            .is_err()
    );
    assert_eq!(state, saved);
    assert!(
        body.command(
            &mut state,
            "missing",
            &event(Time::ONE, 0, ChildCommand::Sample),
            &Seconds
        )
        .is_err()
    );
    assert_eq!(state, saved);
    assert!(
        body.command(
            &mut state,
            "clock",
            &event(Time::ONE, 0, ChildCommand::Sample),
            &Seconds
        )
        .is_err()
    );
    assert_eq!(state, saved);
    assert_eq!(
        body.advance(
            &mut state,
            Time::from_integer(4097),
            Time::from_integer(4098),
            &Seconds
        ),
        Err(Error::Capacity)
    );
    assert_eq!(state, saved);
    let mut body = body;
    body.connections[0].delay = Time::from_integer(i64::MAX);
    let mut state = body.initial(Time::ZERO, "patch", 0, &Seconds).unwrap();
    let saved = state.clone();
    assert_eq!(
        body.advance(&mut state, t(1, 4), t(1, 2), &Seconds),
        Err(Error::Overflow)
    );
    assert_eq!(state, saved);
    let mut body = clock_patch(ConnectionAction::Sample);
    body.connections[0].delay = Time::from_integer(10000);
    let mut state = body.initial(Time::ZERO, "patch", 0, &Seconds).unwrap();
    let saved = state.clone();
    assert_eq!(
        body.advance(
            &mut state,
            Time::from_integer(4097),
            Time::from_integer(4098),
            &Seconds
        ),
        Err(Error::Capacity)
    );
    assert_eq!(state, saved);
}

#[test]
fn normalized_graphs_reject_lfos_unknown_ports_invalid_targets_and_nonfinite_ranges() {
    let mut body = clock_patch(ConnectionAction::Sample);
    body.children
        .push(child("self", Node::Signal(Operator::Latch("self".into()))));
    assert!(body.initial(Time::ZERO, "patch", 0, &Seconds).is_err());
    body.children.pop();
    body.connections[0].source.port = "unknown".into();
    assert!(body.initial(Time::ZERO, "patch", 0, &Seconds).is_err());
    body.connections[0].source.port = "tick".into();
    body.connections[0].target = "clock".into();
    assert!(body.initial(Time::ZERO, "patch", 0, &Seconds).is_err());
    body.connections.clear();
    body.children.push(child(
        "infinite",
        Node::Signal(Operator::Affine {
            input: "clock".into(),
            scale: f64::MAX,
            offset: f64::MAX,
        }),
    ));
    assert_eq!(
        body.initial(Time::ZERO, "patch", 0, &Seconds),
        Err(Error::InvalidSignal)
    );
}

struct PausedBeats;
impl ClockMapping for PausedBeats {
    fn local_time(&self, seconds: Time) -> Result<Time, Error> {
        if seconds < t(1, 4) {
            Ok(seconds)
        } else if seconds <= t(3, 4) {
            Ok(t(1, 4))
        } else {
            t(1, 4).checked_add(
                seconds
                    .checked_sub(t(3, 4))?
                    .checked_mul(Time::from_integer(2))?,
            )
        }
    }
    fn host_time(&self, local: Time) -> Result<Option<Time>, Error> {
        Ok(Some(if local <= t(1, 4) {
            local
        } else {
            t(3, 4).checked_add(
                local
                    .checked_sub(t(1, 4))?
                    .checked_div(Time::from_integer(2))?,
            )?
        }))
    }
}
struct Stopped;
impl ClockMapping for Stopped {
    fn local_time(&self, _: Time) -> Result<Time, Error> {
        Ok(Time::ZERO)
    }
    fn host_time(&self, local: Time) -> Result<Option<Time>, Error> {
        Ok((local == Time::ZERO).then_some(Time::ZERO))
    }
}
fn observe(
    body: &Patch,
    state: &mut envelfo_core::patch::PatchState,
    at: Time,
) -> envelfo_core::patch::PatchAdvance {
    body.advance(state, at, at.checked_add(t(1, 100)).unwrap(), &Seconds)
        .unwrap()
}
fn clock_patch(action: ConnectionAction) -> Patch {
    let mut lfo = LFO::new(Time::ONE).unwrap();
    lfo.shape = Shape::Triangle;
    let held = if action == ConnectionAction::Start {
        Node::Envelope(attack())
    } else {
        Node::SampleHold(SampleHold {
            minimum: 0.0,
            maximum: 1.0,
        })
    };
    Patch {
        children: vec![
            child(
                "clock",
                Node::LFO {
                    body: lfo,
                    markers: vec![envelfo_core::envelope::Marker {
                        name: "tick".into(),
                        position: t(1, 4),
                    }],
                },
            ),
            child("held", held),
            child("left", Node::Signal(Operator::Latch("clock".into()))),
            child(
                "scaled",
                Node::Signal(Operator::Affine {
                    input: "held".into(),
                    scale: 2.0,
                    offset: 1.0,
                }),
            ),
        ],
        outputs: vec![
            output("value", "held"),
            output("second", "scaled"),
            output("capture", "left"),
        ],
        event_outputs: vec![EventOutput {
            name: "tick".into(),
            source: port("clock", "tick"),
        }],
        connections: vec![Connection {
            source: port("clock", "tick"),
            target: if action == ConnectionAction::Capture {
                "left"
            } else {
                "held"
            }
            .into(),
            action,
            every: 1,
            offset: 0,
            probability: 1.0,
            delay: Time::ZERO,
        }],
    }
}
fn staged_patch() -> Patch {
    let attack = Stages {
        initial_stage: "on".into(),
        stages: vec![
            stage(
                "on",
                Body::Envelope(
                    Envelope::new(
                        0.0,
                        vec![Segment {
                            duration: Some(t(1, 2)),
                            to: 1.0,
                            curve: 0.0,
                        }],
                    )
                    .unwrap(),
                ),
            ),
            stage(
                "off",
                Body::Envelope(
                    Envelope::new(
                        0.0,
                        vec![Segment {
                            duration: None,
                            to: 0.0,
                            curve: 0.0,
                        }],
                    )
                    .unwrap(),
                ),
            ),
        ],
        transitions: vec![Transition {
            from: vec!["on".into()],
            event: "note_off".into(),
            action: Action::Enter("off".into()),
        }],
    };
    let receiver = Stages {
        initial_stage: "waiting".into(),
        stages: vec![
            stage(
                "waiting",
                Body::Envelope(
                    Envelope::new(
                        0.0,
                        vec![Segment {
                            duration: None,
                            to: 0.0,
                            curve: 0.0,
                        }],
                    )
                    .unwrap(),
                ),
            ),
            stage(
                "high",
                Body::Envelope(
                    Envelope::new(
                        1.0,
                        vec![Segment {
                            duration: Some(t(1, 10)),
                            to: 1.0,
                            curve: 0.0,
                        }],
                    )
                    .unwrap(),
                ),
            ),
        ],
        transitions: vec![Transition {
            from: vec!["waiting".into(), "high".into()],
            event: "cue.go".into(),
            action: Action::Enter("high".into()),
        }],
    };
    Patch {
        children: vec![
            child("attack", Node::Stages(attack)),
            child("receiver", Node::Stages(receiver)),
        ],
        outputs: vec![output("value", "receiver")],
        event_outputs: vec![
            EventOutput {
                name: "finished".into(),
                source: port("attack", "stage.done"),
            },
            EventOutput {
                name: "received".into(),
                source: port("receiver", "stage.done"),
            },
        ],
        connections: vec![Connection {
            source: port("attack", "stage.done"),
            target: "receiver".into(),
            action: ConnectionAction::Cue("go".into()),
            every: 1,
            offset: 0,
            probability: 1.0,
            delay: t(1, 10),
        }],
    }
}
fn attack() -> Envelope {
    let mut envelope = Envelope::new(
        0.0,
        vec![Segment {
            duration: Some(t(1, 2)),
            to: 1.0,
            curve: 0.0,
        }],
    )
    .unwrap();
    envelope.start = Start::Event;
    envelope
}
fn stage(name: &str, body: Body) -> Stage {
    Stage {
        name: name.into(),
        body,
        markers: vec![],
    }
}
fn child(name: &str, node: Node) -> Child {
    Child {
        name: name.into(),
        node,
    }
}
fn output(name: &str, source: &str) -> Output {
    Output {
        name: name.into(),
        source: source.into(),
    }
}
fn port(child: &str, port: &str) -> Port {
    Port {
        child: child.into(),
        port: port.into(),
    }
}
fn event(at: Time, ordinal: u64, command: ChildCommand) -> ChildEvent {
    ChildEvent {
        at,
        ordinal,
        command,
    }
}
fn t(n: i64, d: i64) -> Time {
    Time::new(n, d).unwrap()
}
