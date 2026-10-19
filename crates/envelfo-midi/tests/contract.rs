use std::collections::BTreeMap;

use envelfo_core::time::Time;
use envelfo_midi::{Error, Mapper, Mapping, Target};
use meedee::messages::PrimitiveMessage;
use serde_json::Value;

fn integer(value: &Value, name: &str) -> u16 {
    u16::try_from(value[name].as_u64().unwrap()).unwrap()
}

fn target(value: &Value) -> Target {
    let channel = u8::try_from(integer(value, "channel")).unwrap();
    match value["kind"].as_str().unwrap() {
        "cc" => Target::ControlChange {
            channel,
            controller: u8::try_from(integer(value, "controller")).unwrap(),
        },
        "cc14" => Target::ControlChange14 {
            channel,
            controller: u8::try_from(integer(value, "controller")).unwrap(),
        },
        "rpn" => Target::Rpn {
            channel,
            parameter: integer(value, "parameter"),
            resolution: u8::try_from(integer(value, "resolution")).unwrap(),
        },
        "nrpn" => Target::Nrpn {
            channel,
            parameter: integer(value, "parameter"),
            resolution: u8::try_from(integer(value, "resolution")).unwrap(),
        },
        _ => panic!("unknown target"),
    }
}

fn values(value: &Value) -> BTreeMap<String, f64> {
    value
        .as_object()
        .unwrap()
        .iter()
        .map(|(n, v)| (n.clone(), v.as_f64().unwrap()))
        .collect()
}

#[test]
fn shared_wire_vectors_cover_order_quantization_budget_and_delivery_recovery() {
    let document: Value =
        serde_json::from_str(include_str!("../../../conformance/envelfo-midi.json")).unwrap();
    for case in document["cases"].as_array().unwrap() {
        let mappings = case["mappings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| {
                Mapping::new(
                    m["output"].as_str().unwrap().into(),
                    target(&m["target"]),
                    m["input_minimum"].as_f64().unwrap(),
                    m["input_maximum"].as_f64().unwrap(),
                    m.get("minimum").map_or(0, |v| v.as_u64().unwrap() as u16),
                    m.get("maximum").map(|v| v.as_u64().unwrap() as u16),
                )
                .unwrap()
            })
            .collect();
        let mut mapper = Mapper::new(
            mappings,
            case["byte_rate"].as_i64().unwrap(),
            case["burst_bytes"].as_i64().unwrap(),
        )
        .unwrap();
        for step in case["steps"].as_array().unwrap() {
            if step.get("report_loss").is_some() {
                mapper.report_loss();
                continue;
            }
            if step.get("restart").is_some() {
                mapper.restart();
                continue;
            }
            let at = step["at"].as_str().unwrap().parse().unwrap();
            let result = mapper.prepare(at, &values(&step["values"]));
            if let Some(expected) = step.get("error") {
                let error = result.unwrap_err();
                let matches = match expected.as_str().unwrap() {
                    "budget" => error == Error::Budget,
                    "pending" => error == Error::Pending,
                    "failed" => error == Error::Failed,
                    "reversed" => error == Error::Time(envelfo_core::Error::TimeReversed),
                    "missing" => matches!(error, Error::MissingOutput(_)),
                    "range" => matches!(error, Error::InvalidValue(_)),
                    _ => false,
                };
                assert!(matches, "case {}: {error:?}", case["name"]);
                continue;
            }
            let batch = result.unwrap();
            let wire: Vec<Vec<u8>> = batch
                .messages()
                .iter()
                .flat_map(|m| m.expand())
                .map(|m| m.encode())
                .collect();
            assert_eq!(
                serde_json::to_value(&wire).unwrap(),
                step["wire"],
                "{}",
                case["name"]
            );
            assert_eq!(batch.wire_bytes(), wire.iter().map(Vec::len).sum::<usize>());
            assert_eq!(batch.at(), at);
            if step.get("confirm").and_then(Value::as_bool).unwrap_or(true) {
                mapper.confirm(&batch).unwrap();
            }
        }
    }
}

fn mapping() -> Mapping {
    Mapping::new(
        "x".into(),
        Target::ControlChange {
            channel: 1,
            controller: 11,
        },
        0.0,
        1.0,
        0,
        None,
    )
    .unwrap()
}

#[test]
fn only_the_owning_mapper_can_confirm_a_current_batch() {
    let mut first = Mapper::new(vec![mapping()], 100, 3).unwrap();
    let mut second = Mapper::new(vec![mapping()], 100, 3).unwrap();
    let observed = BTreeMap::from([("x".into(), 0.5)]);
    let batch = first.prepare(Time::ZERO, &observed).unwrap();
    let foreign = second.prepare(Time::ZERO, &observed).unwrap();
    assert_eq!(first.confirm(&foreign), Err(Error::WrongBatch));
    first.confirm(&batch).unwrap();
    assert_eq!(first.confirm(&batch), Err(Error::WrongBatch));
    first.restart();
    let fresh = first.prepare(Time::ZERO, &observed).unwrap();
    assert_eq!(fresh.wire_bytes(), 3);
    assert_eq!(first.confirm(&batch), Err(Error::WrongBatch));
    first.confirm(&fresh).unwrap();
    second.confirm(&foreign).unwrap();
}

#[test]
fn bad_values_do_not_admit_a_batch_or_change_the_delivery_cache() {
    let mut mapper = Mapper::new(vec![mapping()], 100, 3).unwrap();
    for value in [f64::NAN, f64::INFINITY, -0.01, 1.01] {
        assert!(matches!(
            mapper.prepare(Time::ZERO, &BTreeMap::from([("x".into(), value)])),
            Err(Error::InvalidValue(_))
        ));
    }
    let batch = mapper
        .prepare(Time::ZERO, &BTreeMap::from([("x".into(), 0.5)]))
        .unwrap();
    assert_eq!(batch.messages()[0].expand()[0].encode(), vec![176, 11, 64]);
    mapper.confirm(&batch).unwrap();
}

#[test]
fn configuration_rejects_reserved_targets_invalid_ranges_and_unusable_budgets() {
    for target in [
        Target::ControlChange {
            channel: 0,
            controller: 11,
        },
        Target::ControlChange14 {
            channel: 1,
            controller: 32,
        },
        Target::Rpn {
            channel: 1,
            parameter: 16383,
            resolution: 14,
        },
        Target::Nrpn {
            channel: 1,
            parameter: 0,
            resolution: 8,
        },
    ] {
        assert!(Mapping::new("x".into(), target, 0.0, 1.0, 0, None).is_err());
    }
    for (low, high, minimum, maximum) in [
        (1.0, 1.0, 0, None),
        (0.0, f64::INFINITY, 0, None),
        (0.0, 1.0, 100, Some(99)),
        (0.0, 1.0, 0, Some(128)),
    ] {
        assert!(
            Mapping::new(
                "x".into(),
                Target::ControlChange {
                    channel: 1,
                    controller: 11
                },
                low,
                high,
                minimum,
                maximum
            )
            .is_err()
        );
    }
    assert!(Mapper::new(vec![mapping()], 0, 3).is_err());
    assert!(Mapper::new(vec![mapping()], 100, 2).is_err());
    assert!(Mapper::new(vec![mapping(), mapping()], 100, 6).is_err());
}
