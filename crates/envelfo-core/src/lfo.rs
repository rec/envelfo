//! Exact LFO position/command state and floating-point signal observations.

use std::f64::consts::TAU;

use crate::playback::{Position, check_order};
use crate::time::Time;
use crate::{Error, Status};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Shape {
    Sine,
    Square,
    Triangle,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Reset {
    Trigger,
    Transport,
    Free,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Clock {
    Seconds,
    Beats,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Driver {
    Elapsed,
    Transport,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LFO {
    pub shape: Shape,
    pub rate: Time,
    pub phase: Time,
    pub duty: Time,
    pub delay: Time,
    pub fade: Time,
    pub center: f64,
    pub depth: f64,
    pub reset: Reset,
    pub clock: Clock,
    pub driver: Driver,
}

impl LFO {
    pub fn new(rate: Time) -> Result<Self, Error> {
        let lfo = Self {
            shape: Shape::Sine,
            rate,
            phase: Time::ZERO,
            duty: Time::new(1, 2)?,
            delay: Time::ZERO,
            fade: Time::ZERO,
            center: 0.0,
            depth: 1.0,
            reset: Reset::Trigger,
            clock: Clock::Seconds,
            driver: Driver::Elapsed,
        };
        lfo.validate()?;
        Ok(lfo)
    }
    pub fn validate(&self) -> Result<(), Error> {
        if self.rate < Time::ZERO
            || self.phase < Time::ZERO
            || self.phase >= Time::ONE
            || self.duty < Time::ZERO
            || self.duty > Time::ONE
            || self.delay < Time::ZERO
            || self.fade < Time::ZERO
            || !self.center.is_finite()
            || !self.depth.is_finite()
            || self.depth < 0.0
            || self.center - self.depth < -1.0
            || self.center + self.depth > 1.0
        {
            return Err(Error::InvalidDefinition(
                "invalid LFO range, phase, rate or activation",
            ));
        }
        if self.driver == Driver::Transport
            && (self.clock != Clock::Beats
                || self.reset != Reset::Transport
                || self.delay != Time::ZERO
                || self.fade != Time::ZERO)
        {
            return Err(Error::InvalidDefinition(
                "transport-position LFO requires beat clock, transport reset and no delay/fade",
            ));
        }
        Ok(())
    }

    pub fn initial(&self, at: Time) -> Result<LFOState, Error> {
        self.validate()?;
        Ok(LFOState {
            ordinal: None,
            position: Position::new(at, self.phase, self.rate),
        })
    }

    pub fn value(&self, state: &LFOState, at: Time) -> Result<Value, Error> {
        self.validate()?;
        let (coordinate, weight) = if self.driver == Driver::Transport {
            (self.phase.checked_add(self.rate.checked_mul(at)?)?, 1.0)
        } else {
            let position = state.position.advanced(at)?;
            let weight = if position.age < self.delay {
                0.0
            } else if self.fade == Time::ZERO {
                1.0
            } else {
                let active = position.age.checked_sub(self.delay)?;
                if active >= self.fade {
                    1.0
                } else {
                    active.as_f64() / self.fade.as_f64()
                }
            };
            (position.coordinate, weight)
        };
        self.signal(coordinate, weight, false)
    }

    pub(crate) fn staged_value(&self, position: &Position) -> Result<Value, Error> {
        let weight = if position.age < self.delay {
            0.0
        } else if self.fade == Time::ZERO {
            1.0
        } else {
            let active = position.age.checked_sub(self.delay)?;
            if active >= self.fade {
                1.0
            } else {
                active.as_f64() / self.fade.as_f64()
            }
        };
        self.signal(position.coordinate, weight, true)
    }

    fn signal(&self, coordinate: Time, weight: f64, staged: bool) -> Result<Value, Error> {
        let phase = coordinate.modulo_one();
        let shape = match self.shape {
            Shape::Sine => (TAU * phase.as_f64()).sin(),
            Shape::Square => {
                if phase < self.duty {
                    1.0
                } else {
                    -1.0
                }
            }
            Shape::Triangle if phase < self.duty => {
                2.0 * (phase.as_f64() / self.duty.as_f64()) - 1.0
            }
            Shape::Triangle => {
                2.0 * (Time::ONE.checked_sub(phase)?.as_f64()
                    / Time::ONE.checked_sub(self.duty)?.as_f64())
                    - 1.0
            }
        };
        Ok(Value {
            value: self.center + self.depth * shape * if staged { weight } else { 1.0 },
            weight: if staged { 1.0 } else { weight },
            status: Status::Running,
        })
    }

    /// Errors leave state untouched, including overflow while advancing position.
    pub fn command(
        &self,
        state: &mut LFOState,
        at: Time,
        ordinal: u64,
        command: Command,
    ) -> Result<(), Error> {
        self.validate()?;
        if self.driver == Driver::Transport {
            return Err(Error::Unsupported(
                "transport-position LFO does not accept phase commands",
            ));
        }
        check_order(state.position.at, state.ordinal, at, ordinal)?;
        let mut next = state.position.advanced(at)?;
        match command {
            Command::Pause
            | Command::Resume
            | Command::Reverse
            | Command::Seek(_)
            | Command::Shift(_)
            | Command::Rate(_) => next.command(command)?,
            Command::Reset => {
                next.coordinate = self.phase;
                next.age = Time::ZERO;
            }
            Command::NoteOn if self.reset == Reset::Trigger => {
                next.coordinate = self.phase;
                next.age = Time::ZERO;
            }
            Command::Transport if self.reset == Reset::Transport => {
                next.coordinate = self.phase;
                next.age = Time::ZERO;
            }
            Command::NoteOn | Command::NoteOff | Command::Transport => {}
        }
        *state = LFOState {
            position: next,
            ordinal: Some(ordinal),
        };
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Direction {
    Forward,
    Reverse,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LFOState {
    pub(crate) ordinal: Option<u64>,
    pub(crate) position: Position,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Command {
    Pause,
    Resume,
    Reverse,
    Seek(Time),
    Shift(Time),
    Rate(Time),
    Reset,
    NoteOn,
    NoteOff,
    Transport,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Value {
    pub value: f64,
    pub weight: f64,
    pub status: Status,
}
