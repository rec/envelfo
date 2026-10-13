//! Event-sampled random controls with a portable, independently owned stream.

use crate::lfo::{Command, Value};
use crate::playback::check_order;
use crate::random::random_word;
use crate::time::Time;
use crate::{Error, Status};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SampleHold {
    pub minimum: f64,
    pub maximum: f64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SampleCommand {
    Sample,
    Playback(Command),
}

#[derive(Clone, Debug, PartialEq)]
pub struct SampleState {
    at: Time,
    pub(crate) ordinal: Option<u64>,
    value: f64,
    random: u64,
}

impl SampleHold {
    pub fn validate(&self) -> Result<(), Error> {
        if !self.minimum.is_finite()
            || !self.maximum.is_finite()
            || self.minimum < -1.0
            || self.maximum > 1.0
            || self.minimum > self.maximum
        {
            return Err(Error::InvalidDefinition("invalid SampleHold range"));
        }
        Ok(())
    }

    pub fn initial(&self, at: Time, seed: u64) -> Result<SampleState, Error> {
        self.validate()?;
        let mut random = seed;
        let value = self.draw(&mut random);
        Ok(SampleState {
            at,
            ordinal: None,
            value,
            random,
        })
    }

    pub fn value(&self, state: &SampleState, at: Time) -> Result<Value, Error> {
        if at < state.at {
            return Err(Error::TimeReversed);
        }
        Ok(Value {
            value: state.value,
            weight: 1.0,
            status: Status::Held,
        })
    }

    pub fn command(
        &self,
        state: &mut SampleState,
        at: Time,
        ordinal: u64,
        command: SampleCommand,
    ) -> Result<(), Error> {
        check_order(state.at, state.ordinal, at, ordinal)?;
        match command {
            SampleCommand::Sample => state.value = self.draw(&mut state.random),
            SampleCommand::Playback(
                Command::NoteOn
                | Command::NoteOff
                | Command::Pause
                | Command::Resume
                | Command::Seek(_)
                | Command::Shift(_)
                | Command::Reverse,
            ) => {}
            _ => return Err(Error::Unsupported("unsupported SampleHold command")),
        }
        state.at = at;
        state.ordinal = Some(ordinal);
        Ok(())
    }

    fn draw(&self, random: &mut u64) -> f64 {
        self.minimum
            + (self.maximum - self.minimum)
                * ((random_word(random) >> 11) as f64 / (1_u64 << 53) as f64)
    }
}
