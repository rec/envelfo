//! Optional MIDI realization of already observed enveLFO named outputs.
//!
//! The host advances graphs, chooses observation times and sends returned values
//! in declaration order. Confirm only after every send succeeds; report_loss on
//! any partial/uncertain send. No transport, clock, retry or thread is owned here.
//! ```
//! use std::collections::BTreeMap;
//! use envelfo_core::time::Time;
//! use envelfo_midi::{Mapper, Mapping, Target};
//! # fn main() -> Result<(), envelfo_midi::Error> {
//! let mapping = Mapping::new("expression".into(),
//!     Target::ControlChange { channel: 1, controller: 11 },
//!     0.0, 1.0, 0, None)?;
//! let mut mapper = Mapper::new(vec![mapping], 1000, 64)?;
//! let observed = BTreeMap::from([("expression".into(), 0.5)]);
//! let batch = mapper.prepare(Time::ZERO, &observed)?;
//! assert_eq!(batch.wire_bytes(), 3);
//! // A real host sends all batch.messages() successfully before confirming.
//! mapper.confirm(&batch)?;
//! # Ok(())
//! # }
//! ```
#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use envelfo_core::time::Time;
use meedee::messages::{self, Command, PrimitiveMessage};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    ControlChange {
        channel: u8,
        controller: u8,
    },
    ControlChange14 {
        channel: u8,
        controller: u8,
    },
    Rpn {
        channel: u8,
        parameter: u16,
        resolution: u8,
    },
    Nrpn {
        channel: u8,
        parameter: u16,
        resolution: u8,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Mapping {
    output: String,
    target: Target,
    input_minimum: f64,
    input_maximum: f64,
    minimum: u16,
    maximum: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MidiMessage {
    ControlChange(messages::ControlChange),
    ControlChange14(messages::ControlChange14),
    RpnSet(messages::RpnSet),
    NrpnSet(messages::NrpnSet),
}

#[derive(Debug)]
pub struct Batch {
    owner: Arc<()>,
    serial: u64,
    at: Time,
    messages: Vec<MidiMessage>,
    wire_bytes: usize,
}

#[derive(Debug)]
pub struct Mapper {
    owner: Arc<()>,
    mappings: Vec<Mapping>,
    byte_rate: Time,
    burst: Time,
    tokens: Time,
    at: Option<Time>,
    values: Option<Vec<u16>>,
    pending: Option<Pending>,
    failed: bool,
    serial: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Invalid(&'static str),
    MissingOutput(String),
    InvalidValue(String),
    Budget,
    Pending,
    Failed,
    WrongBatch,
    Time(envelfo_core::Error),
    Message(messages::ValueError),
}

impl Mapping {
    pub fn new(
        output: String,
        target: Target,
        input_minimum: f64,
        input_maximum: f64,
        minimum: u16,
        maximum: Option<u16>,
    ) -> Result<Self, Error> {
        target.message(0)?;
        let maximum = maximum.unwrap_or(target.limit());
        if output.is_empty()
            || !input_minimum.is_finite()
            || !input_maximum.is_finite()
            || input_minimum >= input_maximum
            || !(input_maximum - input_minimum).is_finite()
            || minimum > maximum
            || maximum > target.limit()
        {
            return Err(Error::Invalid("invalid MIDI mapping ranges or output name"));
        }
        Ok(Self {
            output,
            target,
            input_minimum,
            input_maximum,
            minimum,
            maximum,
        })
    }

    fn quantize(&self, value: f64) -> Result<u16, Error> {
        if !value.is_finite() || value < self.input_minimum || value > self.input_maximum {
            return Err(Error::InvalidValue(self.output.clone()));
        }
        let normalized = (value - self.input_minimum) / (self.input_maximum - self.input_minimum);
        Ok(
            (f64::from(self.minimum) + normalized * f64::from(self.maximum - self.minimum))
                .round_ties_even() as u16,
        )
    }
}

impl Mapper {
    pub fn new(mappings: Vec<Mapping>, byte_rate: i64, burst_bytes: i64) -> Result<Self, Error> {
        if mappings.is_empty() || byte_rate <= 0 || burst_bytes <= 0 {
            return Err(Error::Invalid(
                "mappings, byte rate and burst must be positive",
            ));
        }
        let mut initial_bytes = 0usize;
        for (i, mapping) in mappings.iter().enumerate() {
            if mappings[..i]
                .iter()
                .any(|m| m.target.destination() == mapping.target.destination())
            {
                return Err(Error::Invalid("MIDI destinations must be distinct"));
            }
            initial_bytes = initial_bytes
                .checked_add(mapping.target.message(0)?.wire_bytes())
                .ok_or(Error::Invalid("initial batch is too large"))?;
        }
        if i64::try_from(initial_bytes).map_err(|_| Error::Budget)? > burst_bytes {
            return Err(Error::Invalid(
                "burst must accommodate the complete initial batch",
            ));
        }
        let burst = Time::from_integer(burst_bytes);
        Ok(Self {
            owner: Arc::new(()),
            mappings,
            byte_rate: Time::from_integer(byte_rate),
            burst,
            tokens: burst,
            at: None,
            values: None,
            pending: None,
            failed: false,
            serial: 0,
        })
    }

    pub fn prepare(&mut self, at: Time, values: &BTreeMap<String, f64>) -> Result<Batch, Error> {
        if self.failed {
            return Err(Error::Failed);
        }
        if self.pending.is_some() {
            return Err(Error::Pending);
        }
        let tokens = if let Some(previous) = self.at {
            if at < previous {
                return Err(Error::Time(envelfo_core::Error::TimeReversed));
            }
            // Cap the elapsed interval before multiplication, so a long silence
            // cannot overflow merely while refilling an already bounded bucket.
            let needed = self.burst.checked_sub(self.tokens)?;
            let refill_time = needed.checked_div(self.byte_rate)?;
            let elapsed = at.checked_sub(previous)?;
            if elapsed >= refill_time {
                self.burst
            } else {
                self.tokens
                    .checked_add(elapsed.checked_mul(self.byte_rate)?)?
            }
        } else {
            self.tokens
        };
        let mut quantized = Vec::with_capacity(self.mappings.len());
        let mut outgoing = Vec::new();
        let mut size = 0usize;
        for (i, mapping) in self.mappings.iter().enumerate() {
            let value = mapping.quantize(
                *values
                    .get(&mapping.output)
                    .ok_or_else(|| Error::MissingOutput(mapping.output.clone()))?,
            )?;
            quantized.push(value);
            if self.values.as_ref().is_none_or(|v| v[i] != value) {
                let message = mapping.target.message(value)?;
                size = size
                    .checked_add(message.wire_bytes())
                    .ok_or(Error::Budget)?;
                outgoing.push(message);
            }
        }
        let cost = Time::from_integer(i64::try_from(size).map_err(|_| Error::Budget)?);
        if cost > tokens {
            return Err(Error::Budget);
        }
        let serial = self
            .serial
            .checked_add(1)
            .ok_or(Error::Invalid("batch counter exhausted"))?;
        let remaining = tokens.checked_sub(cost)?;
        let batch = Batch {
            owner: Arc::clone(&self.owner),
            serial,
            at,
            messages: outgoing,
            wire_bytes: size,
        };
        self.pending = Some(Pending {
            serial,
            at,
            values: quantized,
            tokens: remaining,
        });
        self.serial = serial;
        Ok(batch)
    }

    pub fn confirm(&mut self, batch: &Batch) -> Result<(), Error> {
        if !Arc::ptr_eq(&self.owner, &batch.owner)
            || self
                .pending
                .as_ref()
                .is_none_or(|p| p.serial != batch.serial || p.at != batch.at)
        {
            return Err(Error::WrongBatch);
        }
        let pending = self.pending.take().ok_or(Error::WrongBatch)?;
        self.at = Some(pending.at);
        self.values = Some(pending.values);
        self.tokens = pending.tokens;
        Ok(())
    }

    pub fn report_loss(&mut self) {
        self.pending = None;
        self.values = None;
        self.failed = true;
    }

    pub fn restart(&mut self) {
        self.pending = None;
        self.values = None;
        self.tokens = self.burst;
        self.at = None;
        self.failed = false;
    }
}

impl Batch {
    pub fn at(&self) -> Time {
        self.at
    }
    pub fn messages(&self) -> &[MidiMessage] {
        &self.messages
    }
    pub fn wire_bytes(&self) -> usize {
        self.wire_bytes
    }
}

impl MidiMessage {
    /// Ordered primitive messages for testing, accounting or device-free hosts.
    /// A transport host should send compound variants with send_command(),
    /// retaining meedee's connection-local serialization and failure progress.
    pub fn expand(&self) -> Vec<messages::AnyMessage> {
        match self {
            Self::ControlChange(m) => vec![messages::AnyMessage::ControlChange(m.clone())],
            Self::ControlChange14(m) => m.expand(),
            Self::RpnSet(m) => m.expand(),
            Self::NrpnSet(m) => m.expand(),
        }
    }

    fn wire_bytes(&self) -> usize {
        self.expand().iter().map(|m| m.encode().len()).sum()
    }
}

impl Target {
    fn limit(self) -> u16 {
        match self {
            Self::ControlChange { .. }
            | Self::Rpn { resolution: 7, .. }
            | Self::Nrpn { resolution: 7, .. } => 127,
            _ => 16383,
        }
    }

    fn destination(self) -> (u8, u8, u16) {
        match self {
            Self::ControlChange {
                channel,
                controller,
            } => (0, channel, u16::from(controller)),
            Self::ControlChange14 {
                channel,
                controller,
            } => (1, channel, u16::from(controller)),
            Self::Rpn {
                channel, parameter, ..
            } => (2, channel, parameter),
            Self::Nrpn {
                channel, parameter, ..
            } => (3, channel, parameter),
        }
    }

    fn message(self, value: u16) -> Result<MidiMessage, Error> {
        let value = i64::from(value);
        Ok(match self {
            Self::ControlChange {
                channel,
                controller,
            } => MidiMessage::ControlChange(messages::ControlChange::new(
                i64::from(channel),
                i64::from(controller),
                value,
            )?),
            Self::ControlChange14 {
                channel,
                controller,
            } => MidiMessage::ControlChange14(messages::ControlChange14::new(
                i64::from(channel),
                i64::from(controller),
                value,
            )?),
            Self::Rpn {
                channel,
                parameter,
                resolution,
            } => MidiMessage::RpnSet(messages::RpnSet::new(
                i64::from(channel),
                i64::from(parameter),
                value,
                i64::from(resolution),
            )?),
            Self::Nrpn {
                channel,
                parameter,
                resolution,
            } => MidiMessage::NrpnSet(messages::NrpnSet::new(
                i64::from(channel),
                i64::from(parameter),
                value,
                i64::from(resolution),
            )?),
        })
    }
}

impl From<envelfo_core::Error> for Error {
    fn from(error: envelfo_core::Error) -> Self {
        Self::Time(error)
    }
}
impl From<messages::ValueError> for Error {
    fn from(error: messages::ValueError) -> Self {
        Self::Message(error)
    }
}
impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(reason) => formatter.write_str(reason),
            Self::MissingOutput(name) => write!(formatter, "missing mapped output: {name}"),
            Self::InvalidValue(name) => {
                write!(formatter, "output outside its finite input range: {name}")
            }
            Self::Budget => {
                formatter.write_str("MIDI wire-byte budget exhausted; no batch admitted")
            }
            Self::Pending => {
                formatter.write_str("confirm the pending batch before preparing another")
            }
            Self::Failed => formatter.write_str("MIDI mapper requires explicit restart"),
            Self::WrongBatch => formatter.write_str("batch is not this mapper's pending batch"),
            Self::Time(error) => error.fmt(formatter),
            Self::Message(error) => error.fmt(formatter),
        }
    }
}
impl std::error::Error for Error {}

#[derive(Debug)]
struct Pending {
    serial: u64,
    at: Time,
    values: Vec<u16>,
    tokens: Time,
}
