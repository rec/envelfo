//! Checked rationals: signed i64 numerator and positive i64 denominator.

use std::cmp::Ordering;
use std::str::FromStr;

use num_rational::Ratio;

use crate::Error;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Time {
    numerator: i64,
    denominator: i64,
}

impl Time {
    pub const ZERO: Self = Self::from_integer(0);
    pub const ONE: Self = Self::from_integer(1);

    pub const fn from_integer(value: i64) -> Self {
        Self {
            numerator: value,
            denominator: 1,
        }
    }

    pub fn new(numerator: i64, denominator: i64) -> Result<Self, Error> {
        Self::from_wide(i128::from(numerator), i128::from(denominator))
    }

    pub const fn numerator(self) -> i64 {
        self.numerator
    }
    pub const fn denominator(self) -> i64 {
        self.denominator
    }

    pub fn checked_add(self, other: Self) -> Result<Self, Error> {
        let left = i128::from(self.numerator) * i128::from(other.denominator);
        let right = i128::from(other.numerator) * i128::from(self.denominator);
        Self::from_wide(
            left.checked_add(right).ok_or(Error::Overflow)?,
            i128::from(self.denominator) * i128::from(other.denominator),
        )
    }

    pub fn checked_sub(self, other: Self) -> Result<Self, Error> {
        let left = i128::from(self.numerator) * i128::from(other.denominator);
        let right = i128::from(other.numerator) * i128::from(self.denominator);
        Self::from_wide(
            left.checked_sub(right).ok_or(Error::Overflow)?,
            i128::from(self.denominator) * i128::from(other.denominator),
        )
    }

    pub fn checked_mul(self, other: Self) -> Result<Self, Error> {
        Self::from_wide(
            i128::from(self.numerator) * i128::from(other.numerator),
            i128::from(self.denominator) * i128::from(other.denominator),
        )
    }

    pub fn checked_div(self, other: Self) -> Result<Self, Error> {
        if other.numerator == 0 {
            return Err(Error::DivisionByZero);
        }
        Self::from_wide(
            i128::from(self.numerator) * i128::from(other.denominator),
            i128::from(self.denominator) * i128::from(other.numerator),
        )
    }

    pub fn modulo_one(self) -> Self {
        Self {
            numerator: self.numerator.rem_euclid(self.denominator),
            denominator: self.denominator,
        }
        .reduced()
    }

    /// Convert only signal evaluation to floating point, never event decisions.
    pub fn as_f64(self) -> f64 {
        self.numerator as f64 / self.denominator as f64
    }

    fn reduced(self) -> Self {
        Self::from_wide(i128::from(self.numerator), i128::from(self.denominator))
            .expect("a fraction reduced from valid i64 components fits")
    }

    fn from_wide(mut numerator: i128, mut denominator: i128) -> Result<Self, Error> {
        if denominator == 0 {
            return Err(Error::ZeroDenominator);
        }
        let mut a = numerator.checked_abs().ok_or(Error::Overflow)?;
        let mut b = denominator.checked_abs().ok_or(Error::Overflow)?;
        while b != 0 {
            (a, b) = (b, a % b);
        }
        numerator /= a;
        denominator /= a;
        if denominator < 0 {
            numerator = numerator.checked_neg().ok_or(Error::Overflow)?;
            denominator = denominator.checked_neg().ok_or(Error::Overflow)?;
        }
        Ok(Self {
            numerator: i64::try_from(numerator).map_err(|_| Error::Overflow)?,
            denominator: i64::try_from(denominator).map_err(|_| Error::Overflow)?,
        })
    }
}

impl Ord for Time {
    fn cmp(&self, other: &Self) -> Ordering {
        (i128::from(self.numerator) * i128::from(other.denominator))
            .cmp(&(i128::from(other.numerator) * i128::from(self.denominator)))
    }
}

impl PartialOrd for Time {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl TryFrom<Ratio<i64>> for Time {
    type Error = Error;
    fn try_from(value: Ratio<i64>) -> Result<Self, Error> {
        Self::new(*value.numer(), *value.denom())
    }
}

impl From<Time> for Ratio<i64> {
    fn from(value: Time) -> Self {
        Self::new_raw(value.numerator, value.denominator)
    }
}

impl FromStr for Time {
    type Err = Error;
    fn from_str(text: &str) -> Result<Self, Error> {
        let (numerator, denominator) = text.split_once('/').unwrap_or((text, "1"));
        let numerator = parse_component(numerator)?;
        let denominator = parse_component(denominator)?;
        Self::new(numerator, denominator)
    }
}

fn parse_component(text: &str) -> Result<i64, Error> {
    text.parse()
        .map_err(|error: std::num::ParseIntError| match error.kind() {
            std::num::IntErrorKind::PosOverflow | std::num::IntErrorKind::NegOverflow => {
                Error::Overflow
            }
            _ => Error::InvalidTime,
        })
}
