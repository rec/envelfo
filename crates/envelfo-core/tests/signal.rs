use envelfo_core::Error;
use envelfo_core::signal::Operator;
use envelfo_core::time::Time;
use std::collections::BTreeMap;

#[test]
fn quantizer_ties_select_higher_point_including_negative_values() {
    let operator = Operator::Quantize {
        input: "x".into(),
        step: 1.0,
        origin: 0.0,
    };
    for (value, expected) in [(-1.5, -1.0), (-0.5, 0.0), (0.5, 1.0), (1.5, 2.0)] {
        assert_eq!(
            operator
                .observe(&signal(value), None, Time::ZERO)
                .unwrap()
                .0,
            expected
        );
    }
}

#[test]
fn threshold_initializes_silently_and_holds_inside_deadband() {
    let operator = Operator::Threshold {
        input: "x".into(),
        lower: -0.25,
        upper: 0.25,
    };
    assert_eq!(
        operator.observe(&signal(1.0), None, Time::ZERO).unwrap(),
        (1.0, None)
    );
    assert_eq!(
        operator
            .observe(&signal(0.0), Some(1.0), Time::ZERO)
            .unwrap(),
        (1.0, None)
    );
    assert_eq!(
        operator
            .observe(&signal(-0.25), Some(1.0), Time::ZERO)
            .unwrap(),
        (0.0, Some("falling"))
    );
    assert_eq!(
        operator
            .observe(&signal(0.0), Some(0.0), Time::ZERO)
            .unwrap(),
        (0.0, None)
    );
    assert_eq!(
        operator
            .observe(&signal(0.25), Some(0.0), Time::ZERO)
            .unwrap(),
        (1.0, Some("rising"))
    );
}

#[test]
fn slew_uses_asymmetric_rates_without_overshoot_and_zero_rates_hold() {
    let operator = Operator::Slew {
        input: "x".into(),
        rise: 2.0,
        fall: 1.0,
        initial: Some(-1.0),
    };
    assert_eq!(
        operator.observe(&signal(1.0), None, Time::ONE).unwrap().0,
        -1.0
    );
    assert_eq!(
        operator
            .observe(&signal(1.0), Some(-1.0), t(1, 4))
            .unwrap()
            .0,
        -0.5
    );
    assert_eq!(
        operator
            .observe(&signal(-1.0), Some(0.0), t(1, 4))
            .unwrap()
            .0,
        -0.25
    );
    assert_eq!(
        operator
            .observe(&signal(0.2), Some(0.0), Time::ONE)
            .unwrap()
            .0,
        0.2
    );
    let operator = Operator::Slew {
        input: "x".into(),
        rise: 0.0,
        fall: 0.0,
        initial: None,
    };
    assert_eq!(
        operator.observe(&signal(0.5), None, Time::ZERO).unwrap().0,
        0.5
    );
    assert_eq!(
        operator
            .observe(&signal(1.0), Some(0.5), Time::ONE)
            .unwrap()
            .0,
        0.5
    );
}

#[test]
fn operators_reject_nonfinite_grid_coordinates_and_missing_inputs() {
    let operator = Operator::Quantize {
        input: "x".into(),
        step: f64::MIN_POSITIVE,
        origin: 0.0,
    };
    assert_eq!(
        operator.observe(&signal(f64::MAX), None, Time::ZERO),
        Err(Error::InvalidSignal)
    );
    let operator = Operator::Affine {
        input: "x".into(),
        scale: 1.0,
        offset: 0.0,
    };
    assert!(
        operator
            .observe(&BTreeMap::new(), None, Time::ZERO)
            .is_err()
    );
    assert_eq!(
        operator.observe(&signal(0.0), None, Time::from_integer(-1)),
        Err(Error::TimeReversed)
    );
}

fn signal(value: f64) -> BTreeMap<String, f64> {
    BTreeMap::from([("x".into(), value)])
}
fn t(n: i64, d: i64) -> Time {
    Time::new(n, d).unwrap()
}
