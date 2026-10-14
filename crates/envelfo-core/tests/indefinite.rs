use envelfo_core::Status;
use envelfo_core::envelope::{Envelope, EnvelopeCommand, Segment};
use envelfo_core::lfo::Command;
use envelfo_core::patch::{Child, ChildCommand, ChildEvent, Node, Output, Patch, Seconds};
use envelfo_core::stages::{Action, Body, Event, Stage, StageCommand, Stages, Transition};
use envelfo_core::time::Time;

fn waiting() -> Envelope {
    Envelope::new(
        0.7,
        vec![Segment {
            duration: None,
            to: 0.0,
            curve: 0.0,
        }],
    )
    .unwrap()
}

#[test]
fn release_completion_tracks_waits_and_playback_commands() {
    let mut body = waiting();
    body.release = vec![
        Segment {
            duration: None,
            to: 0.0,
            curve: 0.0,
        },
        Segment {
            duration: Some(Time::ONE),
            to: 0.0,
            curve: 0.0,
        },
    ];
    let mut state = body.initial(Time::ZERO).unwrap();
    assert_eq!(body.complete_at(&state).unwrap(), None);
    body.command(
        &mut state,
        Time::ZERO,
        0,
        EnvelopeCommand::Playback(Command::NoteOn),
    )
    .unwrap();
    body.command(
        &mut state,
        Time::ONE,
        0,
        EnvelopeCommand::Playback(Command::NoteOff),
    )
    .unwrap();
    assert_eq!(body.complete_at(&state).unwrap(), None);
    body.command(
        &mut state,
        Time::from_integer(2),
        0,
        EnvelopeCommand::Advance,
    )
    .unwrap();
    assert_eq!(
        body.complete_at(&state).unwrap(),
        Some(Time::from_integer(3))
    );
    body.command(
        &mut state,
        Time::from_integer(2),
        1,
        EnvelopeCommand::Playback(Command::Pause),
    )
    .unwrap();
    assert_eq!(body.complete_at(&state).unwrap(), None);
    body.command(
        &mut state,
        Time::from_integer(4),
        0,
        EnvelopeCommand::Playback(Command::Resume),
    )
    .unwrap();
    assert_eq!(
        body.complete_at(&state).unwrap(),
        Some(Time::from_integer(5))
    );
}

#[test]
fn release_bypasses_wait_and_invalid_commands_preserve_state() {
    let mut body = waiting();
    body.release = vec![Segment {
        duration: Some(Time::ONE),
        to: 0.0,
        curve: 0.0,
    }];
    let mut state = body.initial(Time::ZERO).unwrap();
    body.command(
        &mut state,
        Time::ZERO,
        0,
        EnvelopeCommand::Playback(Command::NoteOn),
    )
    .unwrap();
    assert_eq!(
        body.value(&state, Time::from_integer(100)).unwrap().status,
        Status::Held
    );
    let saved = state.clone();
    assert!(
        body.command(
            &mut state,
            Time::ONE,
            0,
            EnvelopeCommand::Playback(Command::Seek(Time::ONE))
        )
        .is_err()
    );
    assert_eq!(state, saved);
    body.command(
        &mut state,
        Time::ONE,
        0,
        EnvelopeCommand::Playback(Command::NoteOff),
    )
    .unwrap();
    assert_eq!(
        body.value(&state, Time::from_integer(2)).unwrap().status,
        Status::Complete
    );
    let saved = state.clone();
    assert!(
        body.command(
            &mut state,
            Time::from_integer(2),
            0,
            EnvelopeCommand::Advance
        )
        .is_err()
    );
    assert_eq!(state, saved);
}

#[test]
fn final_stage_wait_advances_and_finishes_once() {
    let body = Stages {
        initial_stage: "wait".into(),
        stages: vec![Stage {
            name: "wait".into(),
            body: Body::Envelope(waiting()),
            markers: vec![],
        }],
        transitions: vec![Transition {
            from: vec!["wait".into()],
            event: "stage.done".into(),
            action: Action::Finish,
        }],
    };
    let mut state = body.initial(Time::ZERO).unwrap();
    assert!(
        body.advance(&mut state, Time::from_integer(100), None)
            .unwrap()
            .events
            .is_empty()
    );
    let mut restored = state.clone();
    let result = body
        .advance(
            &mut restored,
            Time::from_integer(100),
            Some(&Event {
                at: Time::from_integer(100),
                ordinal: 0,
                command: StageCommand::Advance,
            }),
        )
        .unwrap();
    assert_eq!(
        result
            .events
            .iter()
            .map(|e| e.port.as_str())
            .collect::<Vec<_>>(),
        vec!["stage.done", "done"]
    );
    assert_eq!(result.value.status, Status::Complete);
    assert!(
        body.advance(&mut restored, Time::from_integer(101), None)
            .unwrap()
            .events
            .is_empty()
    );
    assert!(!state.completed());
    let mut body = body;
    body.transitions.clear();
    let mut state = body.initial(Time::ZERO).unwrap();
    body.advance(
        &mut state,
        Time::ZERO,
        Some(&Event {
            at: Time::ZERO,
            ordinal: 0,
            command: StageCommand::Playback(Command::Pause),
        }),
    )
    .unwrap();
    let result = body
        .advance(
            &mut state,
            Time::ONE,
            Some(&Event {
                at: Time::ONE,
                ordinal: 0,
                command: StageCommand::Advance,
            }),
        )
        .unwrap();
    assert_eq!(result.events.len(), 1);
    let mut state = state.clone();
    let result = body
        .advance(
            &mut state,
            Time::from_integer(2),
            Some(&Event {
                at: Time::from_integer(2),
                ordinal: 0,
                command: StageCommand::Playback(Command::Resume),
            }),
        )
        .unwrap();
    assert!(result.events.is_empty());
}

#[test]
fn patch_host_can_advance_a_waiting_child() {
    for staged in [false, true] {
        let mut envelope = waiting();
        envelope.segments.push(Segment {
            duration: Some(Time::ONE),
            to: 0.0,
            curve: 0.0,
        });
        let node = if staged {
            Node::Stages(Stages {
                initial_stage: "sustain".into(),
                stages: vec![Stage {
                    name: "sustain".into(),
                    body: Body::Envelope(envelope),
                    markers: vec![],
                }],
                transitions: vec![],
            })
        } else {
            Node::Envelope(envelope)
        };
        let body = Patch {
            children: vec![Child {
                name: "child".into(),
                node,
            }],
            outputs: vec![Output {
                name: "value".into(),
                source: "child".into(),
            }],
            event_outputs: vec![],
            connections: vec![],
        };
        let mut state = body.initial(Time::ZERO, "gesture", 0, &Seconds).unwrap();
        body.command(
            &mut state,
            "child",
            &ChildEvent {
                at: Time::from_integer(10),
                ordinal: 0,
                command: ChildCommand::Advance,
            },
            &Seconds,
        )
        .unwrap();
        let mut state = state.clone();
        let result = body
            .advance(
                &mut state,
                Time::new(21, 2).unwrap(),
                Time::from_integer(11),
                &Seconds,
            )
            .unwrap();
        assert_eq!(result.values["value"], 0.35);
    }
}
