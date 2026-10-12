//! Exact Cycle position/command state and floating-point signal observations.

use std::f64::consts::TAU;

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
pub struct Cycle {
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

impl Cycle {
    pub fn new(rate: Time) -> Result<Self, Error> {
        let cycle = Self {
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
        cycle.validate()?;
        Ok(cycle)
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
                "invalid Cycle range, phase, rate or activation",
            ));
        }
        if self.driver == Driver::Transport
            && (self.clock != Clock::Beats
                || self.reset != Reset::Transport
                || self.delay != Time::ZERO
                || self.fade != Time::ZERO)
        {
            return Err(Error::InvalidDefinition(
                "transport-position Cycle requires beat clock, transport reset and no delay/fade",
            ));
        }
        Ok(())
    }

    pub fn initial(&self, at: Time) -> Result<CycleState, Error> {
        self.validate()?;
        Ok(CycleState {
            at,
            ordinal: None,
            coordinate: self.phase,
            rate: self.rate,
            age: Time::ZERO,
            direction: Direction::Forward,
            paused: false,
        })
    }

    pub fn value(&self, state: &CycleState, at: Time) -> Result<Value, Error> {
        self.validate()?;
        let (coordinate, weight) = if self.driver == Driver::Transport {
            (self.phase.checked_add(self.rate.checked_mul(at)?)?, 1.0)
        } else {
            let position = state.position_at(at)?;
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
            value: self.center + self.depth * shape,
            weight,
            status: Status::Running,
        })
    }

    /// Errors leave state untouched, including overflow while advancing position.
    pub fn command(
        &self,
        state: &mut CycleState,
        at: Time,
        ordinal: u64,
        command: Command,
    ) -> Result<(), Error> {
        self.validate()?;
        if self.driver == Driver::Transport {
            return Err(Error::Unsupported(
                "transport-position Cycle does not accept phase commands",
            ));
        }
        if at < state.at
            || (at == state.at && state.ordinal.is_some_and(|previous| ordinal <= previous))
        {
            return Err(Error::EventOrder);
        }
        let mut next = state.position_at(at)?;
        match command {
            Command::Pause => next.paused = true,
            Command::Resume => next.paused = false,
            Command::Reverse => {
                next.direction = match next.direction {
                    Direction::Forward => Direction::Reverse,
                    Direction::Reverse => Direction::Forward,
                }
            }
            Command::Seek(position) => {
                if position < Time::ZERO || position > Time::ONE {
                    return Err(Error::InvalidDefinition("seek position must be in [0, 1]"));
                }
                next.coordinate = position;
            }
            Command::Shift(offset) => next.coordinate = next.coordinate.checked_add(offset)?,
            Command::Rate(rate) => {
                if rate < Time::ZERO {
                    return Err(Error::InvalidDefinition("rate must be nonnegative"));
                }
                next.rate = rate;
            }
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
        next.ordinal = Some(ordinal);
        *state = next;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Direction {
    Forward,
    Reverse,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CycleState {
    at: Time,
    ordinal: Option<u64>,
    coordinate: Time,
    rate: Time,
    age: Time,
    direction: Direction,
    paused: bool,
}

impl CycleState {
    fn position_at(&self, at: Time) -> Result<Self, Error> {
        if at < self.at {
            return Err(Error::TimeReversed);
        }
        let elapsed = at.checked_sub(self.at)?;
        let mut next = self.clone();
        if !self.paused {
            let offset = self.rate.checked_mul(elapsed)?;
            next.coordinate = match self.direction {
                Direction::Forward => self.coordinate.checked_add(offset)?,
                Direction::Reverse => self.coordinate.checked_sub(offset)?,
            };
            next.age = self.age.checked_add(elapsed)?;
        }
        next.at = at;
        Ok(next)
    }
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
