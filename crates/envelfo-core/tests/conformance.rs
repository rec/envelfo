use envelfo_core::Status;
use envelfo_core::contour::{Contour, ContourCommand, Initial, Marker, Playback, Segment};
use envelfo_core::cycle::{Clock, Command, Cycle, Driver, Reset, Shape};
use envelfo_core::random::random_word;
use envelfo_core::stages::{Action, Body, Event, Stage, StageCommand, Stages, Transition};
use envelfo_core::time::Time;
use serde_json::Value;

#[test]
fn triangle_quarter_turns_match_shared_trace() {
    check_cycle("triangle-quarter-turns");
}
#[test]
fn activation_weight_matches_shared_trace() {
    check_cycle("cycle-delay-and-fade-are-weight");
}
#[test]
fn rate_pause_and_resume_match_shared_trace() {
    check_cycle("cycle-rate-pause-and-resume");
}
#[test]
fn transport_position_matches_shared_trace() {
    check_cycle("transport-cycle-queries-song-position-directly");
}

#[test]
fn contours_and_stages_match_every_shared_trace() {
    for case in corpus()["cases"].as_array().unwrap() {
        let body = &case["definition"]["body"];
        match body["kind"].as_str().unwrap() {
            "contour" => {
                let contour = contour(body);
                let mut state = contour.initial(Time::ZERO).unwrap();
                for step in case["steps"].as_array().unwrap() {
                    let at = rational(&step["at"]);
                    if step["restore"] == true {
                        state = state.clone();
                    }
                    if !step["event"].is_null() {
                        let event = &step["event"];
                        assert_eq!(at, rational(&event["at"]));
                        contour
                            .command(
                                &mut state,
                                at,
                                event["ordinal"].as_u64().unwrap(),
                                ContourCommand::Playback(command(event)),
                            )
                            .unwrap();
                    }
                    check_value(contour.value(&state, at).unwrap(), step);
                    assert!(step["events"].is_null());
                    if !step["runtime"].is_null() {
                        assert_eq!(step["runtime"], serde_json::json!({"phase": "release"}));
                        assert_eq!(state.phase(), envelfo_core::contour::Phase::Release);
                    }
                }
            }
            "stages" => {
                let stages = stages(body);
                let mut state = stages.initial(Time::ZERO).unwrap();
                for step in case["steps"].as_array().unwrap() {
                    let at = rational(&step["at"]);
                    if step["restore"] == true {
                        state = state.clone();
                    }
                    let event = if step["event"].is_null() {
                        None
                    } else {
                        let event = &step["event"];
                        Some(Event {
                            at: rational(&event["at"]),
                            ordinal: event["ordinal"].as_u64().unwrap(),
                            command: StageCommand::Playback(command(event)),
                        })
                    };
                    let advance = stages.advance(&mut state, at, event.as_ref()).unwrap();
                    check_value(advance.value, step);
                    let expected = step["events"].as_array().map(Vec::as_slice).unwrap_or(&[]);
                    assert_eq!(
                        advance.events.len(),
                        expected.len(),
                        "{} at {at:?}: {:?}",
                        case["name"],
                        advance.events
                    );
                    for (actual, expected) in advance.events.iter().zip(expected) {
                        assert_eq!(actual.at, rational(&expected["at"]));
                        assert_eq!(actual.port, expected["port"].as_str().unwrap());
                        assert_eq!(actual.stage, expected["stage"].as_str().unwrap());
                        assert_eq!(actual.activation, expected["activation"].as_u64().unwrap());
                    }
                    if let Some(runtime) = step["runtime"].as_object() {
                        for (key, value) in runtime {
                            match key.as_str() {
                                "stage" => {
                                    assert_eq!(stages.stage_name(&state), value.as_str().unwrap())
                                }
                                "activation" => {
                                    assert_eq!(state.activation(), value.as_u64().unwrap())
                                }
                                "completed" => {
                                    assert_eq!(state.completed(), value.as_bool().unwrap())
                                }
                                "traversals" => {
                                    assert_eq!(state.traversals(), value.as_u64().unwrap())
                                }
                                _ => panic!("unverified runtime field {key}"),
                            }
                        }
                    }
                }
            }
            "cycle" => {}
            _ => panic!("unverified body"),
        }
    }
}

#[test]
fn splitmix_words_match_shared_vectors() {
    for vector in corpus()["random_vectors"].as_array().unwrap() {
        let mut state = u64::from_str_radix(
            vector["state"].as_str().unwrap().trim_start_matches("0x"),
            16,
        )
        .unwrap();
        let word = random_word(&mut state);
        assert_eq!(
            word,
            u64::from_str_radix(
                vector["word"].as_str().unwrap().trim_start_matches("0x"),
                16
            )
            .unwrap()
        );
        assert_eq!(
            state,
            u64::from_str_radix(
                vector["next_state"]
                    .as_str()
                    .unwrap()
                    .trim_start_matches("0x"),
                16
            )
            .unwrap()
        );
    }
}

#[test]
fn random_stream_counter_wraps_as_specified() {
    let mut state = u64::MAX;
    random_word(&mut state);
    assert_eq!(state, 0x9e3779b97f4a7c14);
}

fn corpus() -> Value {
    serde_json::from_str(include_str!("../../../conformance/envelfo-runtime.json")).unwrap()
}

fn rational(value: &Value) -> Time {
    if let Some(text) = value.as_str() {
        text.parse().unwrap()
    } else {
        Time::from_integer(value.as_i64().unwrap())
    }
}

fn check_cycle(name: &str) {
    let corpus = corpus();
    let case = corpus["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == name)
        .unwrap();
    let definition = &case["definition"];
    let body = &definition["body"];
    assert_eq!(body["kind"], "cycle");
    assert!(
        body["markers"].is_null() || body["markers"].as_array().unwrap().is_empty(),
        "Cycle marker emission is not extracted yet"
    );
    let mut cycle = cycle(body);
    if definition["clock"] == "beats" {
        cycle.clock = Clock::Beats;
    }
    if definition["position_driver"] == "transport" {
        cycle.driver = Driver::Transport;
    }
    let mut state = cycle.initial(Time::ZERO).unwrap();
    for step in case["steps"].as_array().unwrap() {
        assert!(step["events"].is_null() || step["events"].as_array().unwrap().is_empty());
        assert!(step["runtime"].is_null() || step["runtime"].as_object().unwrap().is_empty());
        if step["restore"] == true {
            state = state.clone();
        }
        let at = rational(&step["at"]);
        let event = &step["event"];
        if !event.is_null() {
            assert_eq!(at, rational(&event["at"]));
            cycle
                .command(
                    &mut state,
                    at,
                    event["ordinal"].as_u64().unwrap(),
                    command(event),
                )
                .unwrap();
        }
        check_value(cycle.value(&state, at).unwrap(), step);
    }
}

fn cycle(body: &Value) -> Cycle {
    let mut cycle = Cycle::new(rational(&body["rate"])).unwrap();
    cycle.shape = match body["shape"].as_str().unwrap_or("sine") {
        "sine" => Shape::Sine,
        "square" => Shape::Square,
        "triangle" => Shape::Triangle,
        _ => panic!("unsupported shape"),
    };
    if let Some(center) = body["center"].as_f64() {
        cycle.center = center;
    }
    if let Some(depth) = body["depth"].as_f64() {
        cycle.depth = depth;
    }
    for (key, destination) in [
        ("phase", &mut cycle.phase),
        ("duty_cycle", &mut cycle.duty),
        ("delay", &mut cycle.delay),
        ("fade_in", &mut cycle.fade),
    ] {
        if !body[key].is_null() {
            *destination = rational(&body[key]);
        }
    }
    cycle.reset = match body["reset"].as_str().unwrap_or("trigger") {
        "trigger" => Reset::Trigger,
        "transport" => Reset::Transport,
        "free" => Reset::Free,
        _ => panic!("unsupported reset"),
    };
    cycle
}

fn command(event: &Value) -> Command {
    match event["action"].as_str().unwrap() {
        "rate" => Command::Rate(rational(&event["rate"])),
        "pause" => Command::Pause,
        "resume" => Command::Resume,
        "note_on" => Command::NoteOn,
        "note_off" => Command::NoteOff,
        _ => panic!("unsupported fixture command"),
    }
}

fn check_value(value: envelfo_core::cycle::Value, step: &Value) {
    assert!(
        (value.value - step["expected"]["value"].as_f64().unwrap()).abs() <= 1e-12,
        "{value:?} at {}",
        step["at"]
    );
    assert!((value.weight - step["expected"]["weight"].as_f64().unwrap()).abs() <= 1e-12);
    assert_eq!(
        value.status,
        match step["expected"]["status"].as_str().unwrap() {
            "idle" => Status::Idle,
            "running" => Status::Running,
            "held" => Status::Held,
            "complete" => Status::Complete,
            _ => panic!("unknown status"),
        }
    );
}

fn markers(body: &Value) -> Vec<Marker> {
    body["markers"]
        .as_array()
        .map(|v| {
            v.iter()
                .map(|m| Marker {
                    name: m["name"].as_str().unwrap().into(),
                    position: rational(&m["position"]),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn contour(body: &Value) -> Contour {
    // This decoder accepts only the corpus's canonical seconds serialization.
    // Authored quantities are normalized by the Python/Pint host, never by the core.
    let segments = |key: &str| {
        body[key]
            .as_array()
            .map(|v| {
                v.iter()
                    .map(|s| Segment {
                        duration: s["duration"]
                            .as_str()
                            .unwrap()
                            .strip_suffix(" s")
                            .unwrap()
                            .parse()
                            .unwrap(),
                        to: s["to"].as_f64().unwrap(),
                        curve: s["curve"].as_f64().unwrap_or(0.0),
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut contour = Contour::new(0.0, segments("segments")).unwrap();
    contour.initial = if body["initial"] == "current" {
        Initial::Current
    } else {
        Initial::Value(body["initial"].as_f64().unwrap_or(0.0))
    };
    contour.release = segments("release");
    contour.markers = markers(body);
    contour.playback = match body["playback"].as_str().unwrap_or("once") {
        "once" => Playback::Once,
        "loop" => Playback::Loop,
        "ping_pong" => Playback::PingPong,
        _ => panic!("unknown playback"),
    };
    if let Some(start) = body["loop_start"].as_str() {
        let end = body["loop_end"].as_str().unwrap();
        contour.loop_bounds = Some((
            contour
                .markers
                .iter()
                .find(|m| m.name == start)
                .unwrap()
                .position,
            contour
                .markers
                .iter()
                .find(|m| m.name == end)
                .unwrap()
                .position,
        ));
    }
    contour.repeat_count = body["repeat_count"].as_u64();
    contour
}

fn stages(body: &Value) -> Stages {
    Stages {
        initial_stage: body["initial_stage"].as_str().unwrap().into(),
        stages: body["stages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| {
                let body = &s["envelfo"];
                Stage {
                    name: s["name"].as_str().unwrap().into(),
                    markers: if body["kind"] == "cycle" {
                        markers(body)
                    } else {
                        vec![]
                    },
                    body: match body["kind"].as_str().unwrap() {
                        "hold" => Body::Hold(body["value"].as_f64().unwrap()),
                        "cycle" => Body::Cycle(cycle(body)),
                        "contour" => Body::Contour(contour(body)),
                        _ => panic!("unsupported stage"),
                    },
                }
            })
            .collect(),
        transitions: body["transitions"]
            .as_array()
            .map(|v| {
                v.iter()
                    .map(|t| Transition {
                        from: t["from"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|v| v.as_str().unwrap().into())
                            .collect(),
                        event: t["event"].as_str().unwrap().into(),
                        action: match t["action"]["kind"].as_str().unwrap() {
                            "enter" => Action::Enter(t["action"]["stage"].as_str().unwrap().into()),
                            "finish" => Action::Finish,
                            _ => panic!("unknown action"),
                        },
                    })
                    .collect()
            })
            .unwrap_or_default(),
    }
}
