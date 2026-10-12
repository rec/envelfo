use envelfo_core::Status;
use envelfo_core::cycle::{Clock, Command, Cycle, Driver, Reset, Shape};
use envelfo_core::random::random_word;
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
    if definition["clock"] == "beats" {
        cycle.clock = Clock::Beats;
    }
    if definition["position_driver"] == "transport" {
        cycle.driver = Driver::Transport;
    }
    cycle.reset = match body["reset"].as_str().unwrap_or("trigger") {
        "trigger" => Reset::Trigger,
        "transport" => Reset::Transport,
        "free" => Reset::Free,
        _ => panic!("unsupported reset"),
    };
    let mut state = cycle.initial(Time::ZERO).unwrap();
    for step in case["steps"].as_array().unwrap() {
        assert!(
            step["events"].is_null() || step["events"].as_array().unwrap().is_empty(),
            "event fixture requires an unextracted capability"
        );
        assert!(
            step["runtime"].is_null() || step["runtime"].as_object().unwrap().is_empty(),
            "runtime field fixture requires additional snapshot assertions"
        );
        if step["restore"] == true {
            state = state.clone();
        }
        let at = rational(&step["at"]);
        let event = &step["event"];
        if !event.is_null() {
            assert_eq!(at, rational(&event["at"]));
            let command = match event["action"].as_str().unwrap() {
                "rate" => Command::Rate(rational(&event["rate"])),
                "pause" => Command::Pause,
                "resume" => Command::Resume,
                _ => panic!("unsupported fixture command"),
            };
            cycle
                .command(&mut state, at, event["ordinal"].as_u64().unwrap(), command)
                .unwrap();
        }
        let value = cycle.value(&state, at).unwrap();
        assert!(
            (value.value - step["expected"]["value"].as_f64().unwrap()).abs() <= 1e-12,
            "{name} at {at:?}"
        );
        assert!((value.weight - step["expected"]["weight"].as_f64().unwrap()).abs() <= 1e-12);
        assert_eq!(value.status, Status::Running);
        assert_eq!(step["expected"]["status"], "running");
    }
}
