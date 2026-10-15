//! Shared checked position state; body-specific lifecycle remains with its body.

use crate::Error;
use crate::lfo::{Command, Direction};
use crate::time::Time;

#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "snapshots", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "snapshots", serde(deny_unknown_fields))]
pub(crate) struct Position {
    pub at: Time,
    pub coordinate: Time,
    pub rate: Time,
    pub age: Time,
    pub direction: Direction,
    pub paused: bool,
}

impl Position {
    #[cfg(feature = "snapshots")]
    pub(crate) fn validate_snapshot(&self) -> Result<(), Error> {
        if self.rate < Time::ZERO || self.age < Time::ZERO {
            return Err(Error::InvalidDefinition("invalid snapshot position"));
        }
        Ok(())
    }

    pub fn new(at: Time, coordinate: Time, rate: Time) -> Self {
        Self {
            at,
            coordinate,
            rate,
            age: Time::ZERO,
            direction: Direction::Forward,
            paused: false,
        }
    }

    pub fn advanced(&self, at: Time) -> Result<Self, Error> {
        if at < self.at {
            return Err(Error::TimeReversed);
        }
        if at == self.at {
            return Ok(self.clone());
        }
        let elapsed = at.checked_sub(self.at)?;
        let mut next = self.clone();
        if !self.paused {
            let offset = self.rate.checked_mul(elapsed)?;
            next.coordinate = if self.direction == Direction::Forward {
                self.coordinate.checked_add(offset)?
            } else {
                self.coordinate.checked_sub(offset)?
            };
            next.age = self.age.checked_add(elapsed)?;
        }
        next.at = at;
        Ok(next)
    }

    pub fn command(&mut self, command: Command) -> Result<(), Error> {
        match command {
            Command::Pause => self.paused = true,
            Command::Resume => self.paused = false,
            Command::Reverse => {
                self.direction = if self.direction == Direction::Forward {
                    Direction::Reverse
                } else {
                    Direction::Forward
                }
            }
            Command::Seek(position) => {
                if position < Time::ZERO || position > Time::ONE {
                    return Err(Error::InvalidDefinition("seek position must be in [0, 1]"));
                }
                self.coordinate = position;
            }
            Command::Shift(offset) => self.coordinate = self.coordinate.checked_add(offset)?,
            Command::Rate(rate) => {
                if rate < Time::ZERO {
                    return Err(Error::InvalidDefinition("rate must be nonnegative"));
                }
                self.rate = rate;
            }
            _ => {
                return Err(Error::Unsupported(
                    "command is not a playback position command",
                ));
            }
        }
        Ok(())
    }
}

pub(crate) fn check_order(
    at: Time,
    ordinal: Option<u64>,
    event_at: Time,
    event_ordinal: u64,
) -> Result<(), Error> {
    if event_at < at || (event_at == at && ordinal.is_some_and(|o| event_ordinal <= o)) {
        Err(Error::EventOrder)
    } else {
        Ok(())
    }
}
