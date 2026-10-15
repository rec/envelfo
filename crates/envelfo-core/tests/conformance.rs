use envelfo_core::Status;
use envelfo_core::envelope::{Envelope, EnvelopeCommand, Initial, Marker, Playback, Segment};
use envelfo_core::lfo::{Clock, Command, Driver, LFO, Reset, Shape};
use envelfo_core::random::random_word;
use envelfo_core::stages::{Action, Body, Event, Stage, StageCommand, Stages, Transition};
use envelfo_core::time::Time;
use envelfo_core::{patch, signal};
use serde_json::Value;

#[test]
fn patch_orchestration_matches_shared_observation_and_delivery_traces() {
    for text in [
        include_str!("../../../conformance/envelfo-patch.json"),
        include_str!("../../../conformance/envelfo-host.json"),
    ] {
        let corpus: Value = serde_json::from_str(text).unwrap();
        for case in corpus["cases"].as_array().unwrap() {
            let definition = patch_definition(&case["definition"]);
            let clock = HostClock {
                definition: &case["definition"],
                tempo: &case["tempo_map"],
            };
            let mut state = definition
                .initial(Time::ZERO, "patch", 0, None, &clock)
                .unwrap();
            for step in case["steps"].as_array().unwrap() {
                if step["restore"] == true {
                    state = state.clone();
                }
                let mut events = vec![];
                if let Some(commands) = step["commands"].as_array() {
                    for entry in commands {
                        let event = &entry["event"];
                        events.extend(
                            definition
                                .command(
                                    &mut state,
                                    entry["target"].as_str().unwrap(),
                                    &patch::ChildEvent {
                                        at: rational(&event["at"]),
                                        ordinal: event["ordinal"].as_u64().unwrap(),
                                        command: patch::ChildCommand::Playback(command(event)),
                                    },
                                    &clock,
                                    false,
                                )
                                .unwrap(),
                        );
                    }
                }
                let result = definition
                    .advance(
                        &mut state,
                        rational(&step["at"]),
                        rational(&step["next_delivery"]),
                        &clock,
                    )
                    .unwrap();
                let expected = step["values"].as_object().unwrap();
                assert_eq!(result.values.len(), expected.len());
                for (name, value) in expected {
                    assert!(
                        (result.values[name] - value.as_f64().unwrap()).abs() <= 1e-12,
                        "{} at {}: {:?}",
                        case["name"],
                        step["at"],
                        result.values
                    );
                }
                events.extend(result.events);
                let expected = step["events"].as_array().unwrap();
                assert_eq!(events.len(), expected.len());
                for (event, expected) in events.iter().zip(expected) {
                    assert_eq!(event.at, rational(&expected["at"]));
                    assert_eq!(event.port, expected["port"].as_str().unwrap());
                }
                if let Some(pending) = step["pending"].as_array() {
                    assert_eq!(state.pending_count(), pending.len());
                    assert_eq!(state.next_delivery(), pending.iter().map(rational).min());
                }
            }
        }
    }
}

#[test]
fn bodies_match_every_shared_trace() {
    for case in corpus()["cases"].as_array().unwrap() {
        let body = &case["definition"];
        match body["kind"].as_str().unwrap() {
            "envelope" => {
                let envelope = envelope(body);
                let mut state = envelope.initial(Time::ZERO).unwrap();
                for step in case["steps"].as_array().unwrap() {
                    let at = rational(&step["at"]);
                    if step["restore"] == true {
                        state = state.clone();
                    }
                    if !step["event"].is_null() {
                        let event = &step["event"];
                        assert_eq!(at, rational(&event["at"]));
                        envelope
                            .command(
                                &mut state,
                                at,
                                event["ordinal"].as_u64().unwrap(),
                                if event["action"] == "advance" {
                                    EnvelopeCommand::Advance
                                } else {
                                    EnvelopeCommand::Playback(command(event))
                                },
                            )
                            .unwrap();
                    }
                    check_value(envelope.value(&state, at).unwrap(), step);
                    assert!(step["events"].is_null());
                    if !step["runtime"].is_null() {
                        assert_eq!(step["runtime"], serde_json::json!({"phase": "release"}));
                        assert_eq!(state.phase(), envelfo_core::envelope::Phase::Release);
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
                            command: if event["action"] == "advance" {
                                StageCommand::Advance
                            } else {
                                StageCommand::Playback(command(event))
                            },
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
            "lfo" => check_lfo(case),
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
fn stream_identities_match_shared_sha256_vectors() {
    let vectors: Value =
        serde_json::from_str(include_str!("../../../conformance/envelfo-streams.json")).unwrap();
    for vector in vectors["vectors"].as_array().unwrap() {
        assert_eq!(
            envelfo_core::random::stream_key(
                vector["seed"].as_u64().unwrap(),
                vector["identity"].as_str().unwrap()
            ),
            u64::from_str_radix(vector["key"].as_str().unwrap(), 16).unwrap()
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

fn check_lfo(case: &Value) {
    let definition = &case["definition"];
    let body = definition;
    assert_eq!(body["kind"], "lfo");
    assert!(
        body["markers"].is_null() || body["markers"].as_array().unwrap().is_empty(),
        "scalar LFO queries do not emit marker events; use Stages or Patch"
    );
    let mut lfo = lfo(body);
    if definition["clock"] == "beats" {
        lfo.clock = Clock::Beats;
    }
    if definition["position_driver"] == "transport" {
        lfo.driver = Driver::Transport;
    }
    let mut state = lfo.initial(Time::ZERO).unwrap();
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
            lfo.command(
                &mut state,
                at,
                event["ordinal"].as_u64().unwrap(),
                command(event),
            )
            .unwrap();
        }
        check_value(lfo.value(&state, at).unwrap(), step);
    }
}

fn lfo(body: &Value) -> LFO {
    let mut lfo = LFO::new(rational(&body["rate"])).unwrap();
    lfo.shape = match body["shape"].as_str().unwrap_or("sine") {
        "sine" => Shape::Sine,
        "square" => Shape::Square,
        "triangle" => Shape::Triangle,
        _ => panic!("unsupported shape"),
    };
    if let Some(center) = body["center"].as_f64() {
        lfo.center = center;
    }
    if let Some(depth) = body["depth"].as_f64() {
        lfo.depth = depth;
    }
    for (key, destination) in [
        ("phase", &mut lfo.phase),
        ("duty_cycle", &mut lfo.duty),
        ("delay", &mut lfo.delay),
        ("fade_in", &mut lfo.fade),
    ] {
        if !body[key].is_null() {
            *destination = rational(&body[key]);
        }
    }
    lfo.reset = match body["reset"].as_str().unwrap_or("trigger") {
        "trigger" => Reset::Trigger,
        "transport" => Reset::Transport,
        "free" => Reset::Free,
        _ => panic!("unsupported reset"),
    };
    if body["clock"] == "beats" {
        lfo.clock = Clock::Beats;
    }
    if body["position_driver"] == "transport" {
        lfo.driver = Driver::Transport;
    }
    lfo
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

fn check_value(value: envelfo_core::lfo::Value, step: &Value) {
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

fn envelope(body: &Value) -> Envelope {
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
                            .map(|v| v.strip_suffix(" s").unwrap().parse().unwrap()),
                        to: s["to"].as_f64().unwrap_or(0.0),
                        curve: s["curve"].as_f64().unwrap_or(0.0),
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut envelope = Envelope::new(0.0, segments("segments")).unwrap();
    envelope.initial = if body["initial"] == "current" {
        Initial::Current
    } else {
        Initial::Value(body["initial"].as_f64().unwrap_or(0.0))
    };
    envelope.release = segments("release");
    if body["start"] == "event" {
        envelope.start = envelfo_core::envelope::Start::Event;
    }
    envelope.markers = markers(body);
    envelope.playback = match body["playback"].as_str().unwrap_or("once") {
        "once" => Playback::Once,
        "loop" => Playback::Loop,
        "ping_pong" => Playback::PingPong,
        _ => panic!("unknown playback"),
    };
    if let Some(start) = body["loop_start"].as_str() {
        let end = body["loop_end"].as_str().unwrap();
        envelope.loop_bounds = Some((
            envelope
                .markers
                .iter()
                .find(|m| m.name == start)
                .unwrap()
                .position,
            envelope
                .markers
                .iter()
                .find(|m| m.name == end)
                .unwrap()
                .position,
        ));
    }
    envelope.repeat_count = body["repeat_count"].as_u64();
    envelope
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
                    markers: if body["kind"] == "lfo" {
                        markers(body)
                    } else {
                        vec![]
                    },
                    body: match body["kind"].as_str().unwrap() {
                        "lfo" => Body::LFO(lfo(body)),
                        "envelope" => Body::Envelope(envelope(body)),
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

fn patch_definition(body: &Value) -> patch::Patch {
    let source = |value: &Value| {
        let (child, port) = value.as_str().unwrap().split_once('.').unwrap();
        patch::Port {
            child: child.into(),
            port: port.into(),
        }
    };
    patch::Patch {
        children: body["envelfos"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(name, body)| patch::Child {
                name: name.clone(),
                node: match body["kind"].as_str().unwrap() {
                    "lfo" => patch::Node::LFO {
                        body: lfo(body),
                        markers: markers(body),
                    },
                    "envelope" => patch::Node::Envelope(envelope(body)),
                    "stages" => patch::Node::Stages(stages(body)),
                    "sample_hold" => {
                        patch::Node::SampleHold(envelfo_core::sample_hold::SampleHold {
                            minimum: body["minimum"].as_f64().unwrap(),
                            maximum: body["maximum"].as_f64().unwrap(),
                        })
                    }
                    "affine" => patch::Node::Signal(signal::Operator::Affine {
                        input: body["input"].as_str().unwrap().into(),
                        scale: body["scale"].as_f64().unwrap_or(1.0),
                        offset: body["offset"].as_f64().unwrap_or(0.0),
                    }),
                    "threshold" => patch::Node::Signal(signal::Operator::Threshold {
                        input: body["input"].as_str().unwrap().into(),
                        lower: body["lower"].as_f64().unwrap(),
                        upper: body["upper"].as_f64().unwrap(),
                    }),
                    "sum" | "product" => {
                        let inputs = body["inputs"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|v| v.as_str().unwrap().into())
                            .collect();
                        patch::Node::Signal(if body["kind"] == "sum" {
                            signal::Operator::Sum(inputs)
                        } else {
                            signal::Operator::Product(inputs)
                        })
                    }
                    "quantize" => patch::Node::Signal(signal::Operator::Quantize {
                        input: body["input"].as_str().unwrap().into(),
                        step: body["step"].as_f64().unwrap(),
                        origin: body["origin"].as_f64().unwrap_or(0.0),
                    }),
                    "latch" => patch::Node::Signal(signal::Operator::Latch(
                        body["input"].as_str().unwrap().into(),
                    )),
                    "slew" => patch::Node::Signal(signal::Operator::Slew {
                        input: body["input"].as_str().unwrap().into(),
                        rise: body["rise"].as_f64().unwrap(),
                        fall: body["fall"].as_f64().unwrap(),
                        initial: body["initial"].as_f64(),
                    }),
                    _ => panic!("unverified Patch node"),
                },
            })
            .collect(),
        outputs: body["outputs"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(name, source)| patch::Output {
                name: name.clone(),
                source: source.as_str().unwrap().into(),
            })
            .collect(),
        event_outputs: body["event_outputs"]
            .as_object()
            .map(|v| {
                v.iter()
                    .map(|(name, value)| patch::EventOutput {
                        name: name.clone(),
                        source: source(value),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        connections: body["events"]
            .as_array()
            .map(|v| {
                v.iter()
                    .map(|c| patch::Connection {
                        source: source(&c["source"]),
                        target: c["target"].as_str().unwrap().into(),
                        action: match c["action"].as_str().unwrap_or("start") {
                            "start" => patch::ConnectionAction::Start,
                            "capture" => patch::ConnectionAction::Capture,
                            "sample" => patch::ConnectionAction::Sample,
                            _ => panic!("unverified connection action"),
                        },
                        every: c["every"].as_u64().unwrap_or(1),
                        offset: c["offset"].as_u64().unwrap_or(0),
                        probability: c["probability"].as_f64().unwrap_or(1.0),
                        delay: if c["delay"].is_null() {
                            Time::ZERO
                        } else {
                            rational(&c["delay"])
                        },
                    })
                    .collect()
            })
            .unwrap_or_default(),
    }
}

// This is a fixture host, not a second tempo implementation in the library.
// It integrates the same accepted points supplied to Python's TempoMap.
struct HostClock<'a> {
    definition: &'a Value,
    tempo: &'a Value,
}

impl patch::ClockMapping for HostClock<'_> {
    fn local_time(&self, child: &str, seconds: Time) -> Result<Time, envelfo_core::Error> {
        let body = &self.definition["envelfos"][child];
        if body["clock"] != "beats" {
            return Ok(seconds);
        }
        let points = self.tempo["points"].as_array().unwrap();
        let mut elapsed = Time::ZERO;
        for (index, point) in points.iter().enumerate() {
            let start = rational(&point["at_seconds"]);
            let end = points
                .get(index + 1)
                .map(|p| rational(&p["at_seconds"]))
                .unwrap_or(seconds)
                .min(seconds);
            let running = point["running"] != false;
            if body["position_driver"] == "transport"
                && (index + 1 == points.len()
                    || rational(&points[index + 1]["at_seconds"]) > seconds)
            {
                return if running {
                    rational(&point["beat"]).checked_add(
                        seconds
                            .checked_sub(start)?
                            .checked_mul(rational(&point["bpm"]))?
                            .checked_div(Time::from_integer(60))?,
                    )
                } else {
                    Ok(rational(&point["beat"]))
                };
            }
            if running && end > start {
                elapsed = elapsed.checked_add(
                    end.checked_sub(start)?
                        .checked_mul(rational(&point["bpm"]))?
                        .checked_div(Time::from_integer(60))?,
                )?;
            }
            if end == seconds && body["position_driver"] != "transport" {
                break;
            }
        }
        Ok(elapsed)
    }

    fn host_time(&self, child: &str, local: Time) -> Result<Option<Time>, envelfo_core::Error> {
        if self.definition["envelfos"][child]["clock"] != "beats" {
            return Ok(Some(local));
        }
        let points = self.tempo["points"].as_array().unwrap();
        let mut elapsed = Time::ZERO;
        for (index, point) in points.iter().enumerate() {
            let start = rational(&point["at_seconds"]);
            if local == elapsed {
                return Ok(Some(start));
            }
            if point["running"] == false {
                continue;
            }
            let duration = points
                .get(index + 1)
                .map(|p| rational(&p["at_seconds"]).checked_sub(start))
                .transpose()?;
            let reached = local
                .checked_sub(elapsed)?
                .checked_mul(Time::from_integer(60))?
                .checked_div(rational(&point["bpm"]))?;
            if duration.is_none_or(|d| reached <= d) {
                return Ok(Some(start.checked_add(reached)?));
            }
            elapsed = elapsed.checked_add(
                duration
                    .unwrap()
                    .checked_mul(rational(&point["bpm"]))?
                    .checked_div(Time::from_integer(60))?,
            )?;
        }
        Ok(None)
    }
}
