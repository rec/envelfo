use envelfo_core::Error;
use envelfo_core::time::Time;
use num_rational::Ratio;

#[test]
fn normalized_components_cover_signed_limits_and_negative_denominators() {
    assert_eq!(Time::new(6, -8).unwrap(), Time::new(-3, 4).unwrap());
    assert_eq!(Time::new(0, i64::MIN).unwrap(), Time::ZERO);
    assert_eq!(Time::new(i64::MIN, i64::MIN).unwrap(), Time::ONE);
    assert_eq!(Time::new(i64::MIN, 1).unwrap().numerator(), i64::MIN);
    assert_eq!(Time::new(i64::MIN, -1), Err(Error::Overflow));
    assert_eq!(Time::new(1, i64::MIN), Err(Error::Overflow));
    assert_eq!(Time::new(1, 0), Err(Error::ZeroDenominator));
}

#[test]
fn arithmetic_reports_numerator_and_denominator_overflow() {
    let maximum = Time::from_integer(i64::MAX);
    let minimum = Time::from_integer(i64::MIN);
    assert_eq!(maximum.checked_add(Time::ONE), Err(Error::Overflow));
    assert_eq!(minimum.checked_sub(Time::ONE), Err(Error::Overflow));
    assert_eq!(
        maximum.checked_mul(Time::from_integer(2)),
        Err(Error::Overflow)
    );
    assert_eq!(
        minimum.checked_div(Time::from_integer(-1)),
        Err(Error::Overflow)
    );
    assert_eq!(
        Time::ONE.checked_div(Time::ZERO),
        Err(Error::DivisionByZero)
    );
    let tiny = Time::new(1, i64::MAX).unwrap();
    assert_eq!(tiny.checked_mul(tiny), Err(Error::Overflow));
    assert_eq!(
        tiny.checked_add(Time::new(1, i64::MAX - 1).unwrap()),
        Err(Error::Overflow)
    );
}

#[test]
fn wide_intermediates_reduce_before_checking_the_final_components() {
    let half = Time::new(i64::MAX, 2).unwrap();
    assert_eq!(
        half.checked_mul(Time::from_integer(2)).unwrap(),
        Time::from_integer(i64::MAX)
    );
    assert_eq!(
        half.checked_add(half).unwrap(),
        Time::from_integer(i64::MAX)
    );
    assert_eq!(
        Time::new(i64::MIN, i64::MAX)
            .unwrap()
            .checked_sub(Time::new(i64::MIN, i64::MAX).unwrap())
            .unwrap(),
        Time::ZERO
    );
    assert_eq!(
        Time::new(-1, 4).unwrap().modulo_one(),
        Time::new(3, 4).unwrap()
    );
}

#[test]
fn rational_arithmetic_matches_wider_reference_for_small_fractions() {
    for a in -8..=8 {
        for b in 1..=8 {
            for c in -8..=8 {
                for d in 1..=8 {
                    let left = Time::new(a, b).unwrap();
                    let right = Time::new(c, d).unwrap();
                    let wide_left = Ratio::new(i128::from(a), i128::from(b));
                    let wide_right = Ratio::new(i128::from(c), i128::from(d));
                    for (actual, expected) in [
                        (left.checked_add(right), wide_left + wide_right),
                        (left.checked_sub(right), wide_left - wide_right),
                        (left.checked_mul(right), wide_left * wide_right),
                    ] {
                        let actual = actual.unwrap();
                        assert_eq!(
                            Ratio::new(
                                i128::from(actual.numerator()),
                                i128::from(actual.denominator())
                            ),
                            expected
                        );
                    }
                    if c != 0 {
                        let actual = left.checked_div(right).unwrap();
                        assert_eq!(
                            Ratio::new(
                                i128::from(actual.numerator()),
                                i128::from(actual.denominator())
                            ),
                            wide_left / wide_right
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn ordering_does_not_round_large_adjacent_positions() {
    let left = Time::new(i64::MAX - 1, i64::MAX).unwrap();
    let right = Time::new(i64::MAX - 2, i64::MAX - 1).unwrap();
    assert!(left > right);
    assert!(Time::from_integer(i64::MIN) < Time::from_integer(i64::MAX));
}

#[test]
fn arpeg_ratios_convert_without_unchecked_normalization() {
    let ratio = Ratio::new_raw(-6, -8);
    let time = Time::try_from(ratio).unwrap();
    assert_eq!(Ratio::<i64>::from(time), Ratio::new(3, 4));
    assert_eq!(
        Time::try_from(Ratio::new_raw(1, 0)),
        Err(Error::ZeroDenominator)
    );
    assert_eq!(
        Time::try_from(Ratio::new_raw(i64::MIN, -1)),
        Err(Error::Overflow)
    );
}

#[test]
fn normalized_literal_parser_reports_overflow_without_accepting_units() {
    assert_eq!("3/4".parse::<Time>().unwrap(), Time::new(3, 4).unwrap());
    assert_eq!("9223372036854775808".parse::<Time>(), Err(Error::Overflow));
    assert_eq!(
        "1/9223372036854775808".parse::<Time>(),
        Err(Error::Overflow)
    );
    assert_eq!("1/0".parse::<Time>(), Err(Error::ZeroDenominator));
    for text in ["", "1 s", "1/2/3", "0.25", "NaN"] {
        assert_eq!(text.parse::<Time>(), Err(Error::InvalidTime));
    }
}
