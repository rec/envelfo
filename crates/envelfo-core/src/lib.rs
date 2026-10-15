//! Standalone control playback. Hosts supply normalized definitions and time.
//!
//! ```
//! use envelfo_core::lfo::LFO;
//! use envelfo_core::time::Time;
//! # fn main() -> Result<(), envelfo_core::Error> {
//! let lfo = LFO::new(Time::ONE)?;
//! let state = lfo.initial(Time::ZERO)?;
//! assert_eq!(lfo.value(&state, Time::new(1, 4)?)?.value, 1.0);
//! # Ok(())
//! # }
//! ```
#![forbid(unsafe_code)]

pub mod envelope;
pub mod lfo;
pub mod patch;
mod playback;
pub mod random;
pub mod sample_hold;
pub mod signal;
#[cfg(feature = "snapshots")]
pub mod snapshot;
pub mod stages;
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
    Capacity,
    PendingCapacity,
    InvalidSignal,
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
            Self::Capacity => "Envelfo event capacity exceeded",
            Self::PendingCapacity => "Delayed Envelfo event capacity exceeded",
            Self::InvalidSignal => "Patch signal is not finite",
        })
    }
}

impl std::error::Error for Error {}
