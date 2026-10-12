//! Standalone control playback. Hosts supply normalized definitions and time.
//!
//! ```
//! use envelfo_core::cycle::Cycle;
//! use envelfo_core::time::Time;
//! # fn main() -> Result<(), envelfo_core::Error> {
//! let cycle = Cycle::new(Time::ONE)?;
//! let state = cycle.initial(Time::ZERO)?;
//! assert_eq!(cycle.value(&state, Time::new(1, 4)?)?.value, 1.0);
//! # Ok(())
//! # }
//! ```
#![forbid(unsafe_code)]

pub mod cycle;
pub mod random;
pub mod time;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Status {
    Idle,
    Running,
    Held,
    Complete,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Overflow,
    ZeroDenominator,
    DivisionByZero,
    InvalidTime,
    InvalidDefinition(&'static str),
    TimeReversed,
    EventOrder,
    Unsupported(&'static str),
}

impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Overflow => "rational time exceeds the signed 64-bit range",
            Self::ZeroDenominator => "rational denominator must not be zero",
            Self::DivisionByZero => "rational division by zero",
            Self::InvalidTime => "expected a normalized rational literal",
            Self::InvalidDefinition(message) | Self::Unsupported(message) => message,
            Self::TimeReversed => "query precedes current state; replay events to seek",
            Self::EventOrder => "commands must increase in time and ordinal order",
        })
    }
}

impl std::error::Error for Error {}
