//! Normalized envelope definitions and exact attack/release playback.

use crate::lfo::{Command, Direction, Value};
use crate::playback::{Position, check_order};
use crate::time::Time;
use crate::{Error, Status};

#[derive(Clone, Debug, PartialEq)]
pub struct Segment {
    /// None holds the incoming value until Advance; to and curve are unused.
    pub duration: Option<Time>,
    pub to: f64,
    pub curve: f64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Marker {
    pub name: String,
    pub position: Time,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Initial {
    Value(f64),
    Current,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Playback {
    Once,
    Loop,
    PingPong,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Retrigger {
    Current,
    Initial,
    Ignore,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Start {
    Activation,
    Event,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Envelope {
    pub initial: Initial,
    pub segments: Vec<Segment>,
    pub release: Vec<Segment>,
    pub bipolar: bool,
    pub hold: bool,
    pub retrigger: Retrigger,
    pub start: Start,
    pub playback: Playback,
    pub markers: Vec<Marker>,
    /// Normalized positions resolved from named markers by the host.
    pub loop_bounds: Option<(Time, Time)>,
    pub repeat_count: Option<u64>,
}

impl Envelope {
    pub fn new(initial: f64, segments: Vec<Segment>) -> Result<Self, Error> {
        let envelope = Self {
            initial: Initial::Value(initial),
            segments,
            release: vec![],
            bipolar: false,
            hold: true,
            retrigger: Retrigger::Current,
            start: Start::Activation,
            playback: Playback::Once,
            markers: vec![],
            loop_bounds: None,
            repeat_count: None,
        };
        envelope.validate()?;
        Ok(envelope)
    }

    pub fn validate(&self) -> Result<(), Error> {
        let valid_level =
            |v: f64| v.is_finite() && v <= 1.0 && v >= if self.bipolar { -1.0 } else { 0.0 };
        if self.segments.is_empty()
            || matches!(self.initial, Initial::Value(v) if !valid_level(v))
            || self.segments.iter().chain(&self.release).any(|s| {
                s.duration.is_some_and(|d| d < Time::ZERO)
                    || !valid_level(s.to)
                    || !s.curve.is_finite()
                    || s.duration.is_none() && s.curve != 0.0
            })
        {
            return Err(Error::InvalidDefinition(
                "invalid Envelope levels or segments",
            ));
        }
        validate_markers(&self.markers, true)?;
        if self.indefinite() && (!self.markers.is_empty() || self.playback != Playback::Once) {
            return Err(Error::InvalidDefinition(
                "indefinite Envelope cannot use markers or repeated playback",
            ));
        }
        if self.repeat_count == Some(0)
            || (self.playback == Playback::Once
                && (self.repeat_count.is_some() || self.loop_bounds.is_some()))
        {
            return Err(Error::InvalidDefinition(
                "repeat count and bounds require repeated playback",
            ));
        }
        if let Some((start, end)) = self.loop_bounds {
            if start < Time::ZERO
                || start >= end
                || end > Time::ONE
                || !self.markers.iter().any(|m| m.position == start)
                || !self.markers.iter().any(|m| m.position == end)
                || self.markers.iter().any(|m| m.position > end)
            {
                return Err(Error::InvalidDefinition(
                    "invalid named Envelope loop bounds",
                ));
            }
        }
        duration(&self.segments)?;
        duration(&self.release)?;
        Ok(())
    }

    pub fn initial(&self, at: Time) -> Result<EnvelopeState, Error> {
        self.validate()?;
        let Initial::Value(value) = self.initial else {
            return Err(Error::InvalidDefinition(
                "current initial level requires a Stage entry",
            ));
        };
        let rate = if self.indefinite() {
            Time::ONE
        } else {
            rate(&self.segments)?
        };
        Ok(EnvelopeState {
            position: Position::new(
                at,
                if rate == Time::ZERO {
                    Time::ONE
                } else {
                    Time::ZERO
                },
                rate,
            ),
            ordinal: None,
            phase: if !self.release.is_empty() || self.start == Start::Event {
                Phase::Idle
            } else {
                Phase::On
            },
            start_value: value,
            traversals: 0,
            complete_coordinate: None,
            segment_index: 0,
        })
    }

    pub fn value(&self, state: &EnvelopeState, at: Time) -> Result<Value, Error> {
        if state.phase == Phase::Idle {
            return Ok(Value {
                value: state.start_value,
                weight: 1.0,
                status: Status::Idle,
            });
        }
        let segments = if state.phase == Phase::Release {
            &self.release
        } else {
            &self.segments
        };
        let position = state.position.advanced(at)?;
        if self.indefinite() {
            let (value, mut status, _) = segment_value(
                state.start_value,
                &segments[state.segment_index..],
                position.coordinate.max(Time::ZERO),
            )?;
            if status == Status::Complete
                && state.phase == Phase::On
                && self.hold
                && !self.release.is_empty()
            {
                status = Status::Held;
            }
            return Ok(Value {
                value,
                weight: 1.0,
                status,
            });
        }
        let complete = if state.phase == Phase::On {
            state.complete_coordinate.or(self.completion(state, at)?.1)
        } else {
            None
        };
        let total = duration(segments)?;
        let coordinate = if let Some(boundary) = complete {
            boundary
        } else if total == Time::ZERO {
            Time::ONE
        } else {
            self.coordinate(position.coordinate, state.phase == Phase::Release)?
        };
        let status = if complete.is_some() {
            Status::Complete
        } else if state.phase == Phase::On && self.playback != Playback::Once
            || coordinate < Time::ONE
        {
            Status::Running
        } else if state.phase == Phase::On && self.hold && !self.release.is_empty() {
            Status::Held
        } else {
            Status::Complete
        };
        Ok(Value {
            value: curve_at(state.start_value, segments, coordinate.checked_mul(total)?)?,
            weight: 1.0,
            status,
        })
    }

    /// Clone-based updates are transactional: an error never partially changes state.
    pub fn command(
        &self,
        state: &mut EnvelopeState,
        at: Time,
        ordinal: u64,
        command: EnvelopeCommand,
    ) -> Result<(), Error> {
        check_order(state.position.at, state.ordinal, at, ordinal)?;
        let observed = self.value(state, at)?;
        let mut next = state.clone();
        if state.phase == Phase::On && next.complete_coordinate.is_none() {
            let (crossed, complete) = self.completion(state, at)?;
            next.traversals = next
                .traversals
                .checked_add(crossed)
                .ok_or(Error::Overflow)?;
            next.complete_coordinate = complete;
        }
        next.position = state.position.advanced(at)?;
        match command {
            EnvelopeCommand::Start | EnvelopeCommand::Playback(Command::NoteOn)
                if command == EnvelopeCommand::Start || self.start == Start::Activation =>
            {
                if !matches!(observed.status, Status::Running | Status::Held)
                    || self.retrigger != Retrigger::Ignore
                {
                    next.phase = Phase::On;
                    next.start_value = if self.retrigger == Retrigger::Current {
                        observed.value
                    } else {
                        match self.initial {
                            Initial::Value(v) => v,
                            Initial::Current => {
                                return Err(Error::InvalidDefinition(
                                    "current initial requires Stage entry",
                                ));
                            }
                        }
                    };
                    next.traversals = 0;
                    next.complete_coordinate = None;
                    next.segment_index = 0;
                    next.position.coordinate = Time::ZERO;
                    next.position.age = Time::ZERO;
                    next.position.rate = if self.indefinite() {
                        Time::ONE
                    } else {
                        rate(&self.segments)?
                    };
                }
            }
            EnvelopeCommand::Playback(Command::NoteOff) => {
                if !self.release.is_empty()
                    && next.phase == Phase::On
                    && observed.status != Status::Complete
                {
                    next.phase = Phase::Release;
                    next.start_value = observed.value;
                    next.segment_index = 0;
                    next.position.coordinate = Time::ZERO;
                    next.position.age = Time::ZERO;
                    next.position.rate = if self.indefinite() {
                        Time::ONE
                    } else {
                        rate(&self.release)?
                    };
                }
            }
            EnvelopeCommand::Playback(Command::NoteOn) => {}
            EnvelopeCommand::Advance => {
                if next.phase == Phase::Idle || !self.indefinite() {
                    return Err(Error::Unsupported(
                        "advance requires an active indefinite segment",
                    ));
                }
                let parts = if next.phase == Phase::Release {
                    &self.release
                } else {
                    &self.segments
                };
                let (index, value) = advance_segment(
                    next.start_value,
                    &parts[next.segment_index..],
                    next.position.coordinate,
                )?;
                next.segment_index += index;
                next.start_value = value;
                next.position.coordinate = Time::ZERO;
            }
            EnvelopeCommand::Playback(
                c @ (Command::Pause
                | Command::Resume
                | Command::Reverse
                | Command::Seek(_)
                | Command::Shift(_)),
            ) => {
                if self.indefinite() && matches!(c, Command::Seek(_) | Command::Shift(_)) {
                    return Err(Error::Unsupported(
                        "indefinite Envelope cannot use normalized positions",
                    ));
                }
                next.position.command(c)?;
                if matches!(c, Command::Shift(_))
                    && (next.phase == Phase::Release || self.playback == Playback::Once)
                {
                    next.position.coordinate =
                        next.position.coordinate.clamp(Time::ZERO, Time::ONE);
                }
            }
            _ => return Err(Error::Unsupported("unsupported Envelope command")),
        }
        next.ordinal = Some(ordinal);
        *state = next;
        Ok(())
    }

    pub(crate) fn bounds(&self) -> (Time, Time) {
        self.loop_bounds.unwrap_or((Time::ZERO, Time::ONE))
    }

    pub fn indefinite(&self) -> bool {
        self.segments
            .iter()
            .chain(&self.release)
            .any(|s| s.duration.is_none())
    }

    /// Release completion in the owning clock; None while a wait or pause prevents it.
    pub fn complete_at(&self, state: &EnvelopeState) -> Result<Option<Time>, Error> {
        if state.phase != Phase::Release {
            return Ok(None);
        }
        let parts = &self.release[state.segment_index..];
        if parts.iter().any(|s| s.duration.is_none()) {
            return Ok(None);
        }
        let total = duration(parts)?;
        let position = &state.position;
        let target = if self.indefinite() { total } else { Time::ONE };
        if total == Time::ZERO || position.coordinate >= target {
            return Ok(Some(position.at));
        }
        if position.paused
            || position.direction == Direction::Reverse
            || position.rate == Time::ZERO
        {
            return Ok(None);
        }
        Ok(Some(
            position.at.checked_add(
                target
                    .checked_sub(position.coordinate)?
                    .checked_div(position.rate)?,
            )?,
        ))
    }

    pub(crate) fn coordinate(&self, raw: Time, release: bool) -> Result<Time, Error> {
        if release || self.playback == Playback::Once {
            return Ok(raw.clamp(Time::ZERO, Time::ONE));
        }
        let (start, end) = self.bounds();
        let width = end.checked_sub(start)?;
        let named = start != Time::ZERO || end != Time::ONE;
        if named && raw < end {
            return Ok(raw.max(Time::ZERO));
        }
        let raw = if named { raw.checked_sub(end)? } else { raw };
        if self.playback == Playback::Loop {
            return start.checked_add(raw.checked_mod(width)?);
        }
        let phase = raw.checked_mod(width.checked_mul(Time::from_integer(2))?)?;
        if phase <= width {
            if named {
                end.checked_sub(phase)
            } else {
                start.checked_add(phase)
            }
        } else if named {
            start.checked_add(phase.checked_sub(width)?)
        } else {
            end.checked_sub(phase.checked_sub(width)?)
        }
    }

    pub(crate) fn boundary_coordinate(
        &self,
        raw: Time,
        direction: Direction,
    ) -> Result<Time, Error> {
        if self.playback == Playback::Loop {
            let (start, end) = self.bounds();
            Ok(if direction == Direction::Forward {
                end
            } else {
                start
            })
        } else {
            self.coordinate(raw, false)
        }
    }

    fn completion(&self, state: &EnvelopeState, at: Time) -> Result<(u64, Option<Time>), Error> {
        let Some(count) = self.repeat_count else {
            return Ok((0, None));
        };
        if state.complete_coordinate.is_some() {
            return Ok((0, state.complete_coordinate));
        }
        let target = state.position.advanced(at)?.coordinate;
        let coordinate = state.position.coordinate;
        if target == coordinate {
            return Ok((0, None));
        }
        let remaining = count - state.traversals;
        let (start, end) = self.bounds();
        let width = end.checked_sub(start)?;
        let named = self.loop_bounds.is_some();
        let forward = state.position.direction == Direction::Forward;
        let index = if named {
            let offset = coordinate.checked_sub(end)?.checked_div(width)?;
            if forward {
                offset.floor().checked_add(1).ok_or(Error::Overflow)?.max(0)
            } else {
                offset.ceil().checked_sub(1).ok_or(Error::Overflow)?
            }
        } else if forward {
            coordinate.floor().checked_add(1).ok_or(Error::Overflow)?
        } else {
            coordinate.ceil().checked_sub(1).ok_or(Error::Overflow)?
        };
        let first = if named {
            end.checked_add(width.checked_mul(Time::from_integer(index))?)?
        } else {
            Time::from_integer(index)
        };
        if !forward && named && first < end {
            return Ok((0, None));
        }
        let distance = if forward {
            target.checked_sub(first)?
        } else {
            first.checked_sub(target)?
        };
        let crossed = distance
            .checked_div(width)?
            .floor()
            .checked_add(1)
            .ok_or(Error::Overflow)?
            .max(0) as u64;
        if crossed < remaining {
            return Ok((crossed, None));
        }
        let offset = width.checked_mul(Time::from_integer(
            i64::try_from(remaining - 1).map_err(|_| Error::Overflow)?,
        ))?;
        let boundary = if forward {
            first.checked_add(offset)?
        } else {
            first.checked_sub(offset)?
        };
        Ok((
            remaining,
            Some(self.boundary_coordinate(boundary, state.position.direction)?),
        ))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EnvelopeCommand {
    Start,
    Advance,
    Playback(Command),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Phase {
    Idle,
    On,
    Release,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EnvelopeState {
    position: Position,
    pub(crate) ordinal: Option<u64>,
    phase: Phase,
    start_value: f64,
    traversals: u64,
    complete_coordinate: Option<Time>,
    segment_index: usize,
}

impl EnvelopeState {
    pub fn phase(&self) -> Phase {
        self.phase
    }
}

pub(crate) fn validate_markers(markers: &[Marker], include_end: bool) -> Result<(), Error> {
    for (index, marker) in markers.iter().enumerate() {
        if marker.name.is_empty()
            || marker.position < Time::ZERO
            || marker.position > Time::ONE
            || (!include_end && marker.position == Time::ONE)
            || markers[..index].iter().any(|m| m.name == marker.name)
        {
            return Err(Error::InvalidDefinition("invalid or duplicate marker"));
        }
    }
    Ok(())
}

pub(crate) fn duration(segments: &[Segment]) -> Result<Time, Error> {
    segments.iter().try_fold(Time::ZERO, |t, s| {
        t.checked_add(s.duration.unwrap_or(Time::ZERO))
    })
}

pub(crate) fn rate(segments: &[Segment]) -> Result<Time, Error> {
    if segments.iter().any(|s| s.duration.is_none()) {
        return Ok(Time::ONE);
    }
    let total = duration(segments)?;
    if total == Time::ZERO {
        Ok(Time::ZERO)
    } else {
        Time::ONE.checked_div(total)
    }
}

pub(crate) fn curve_at(initial: f64, segments: &[Segment], mut at: Time) -> Result<f64, Error> {
    let mut value = initial;
    for segment in segments {
        let Some(duration) = segment.duration else {
            return Ok(value);
        };
        if at < duration {
            let progress = at.checked_div(duration)?.as_f64();
            let curve = segment.curve;
            let progress = if progress == 0.0 || progress == 1.0 || curve == 0.0 {
                progress
            } else if curve > 0.0 {
                (curve * (progress - 1.0)).exp() * (-curve * progress).exp_m1() / (-curve).exp_m1()
            } else {
                (curve * progress).exp_m1() / curve.exp_m1()
            };
            return Ok(value + (segment.to - value) * progress);
        }
        at = at.checked_sub(duration)?;
        value = segment.to;
    }
    Ok(value)
}

pub(crate) fn segment_value(
    initial: f64,
    segments: &[Segment],
    mut at: Time,
) -> Result<(f64, Status, usize), Error> {
    let mut value = initial;
    at = at.max(Time::ZERO);
    for (index, segment) in segments.iter().enumerate() {
        let Some(duration) = segment.duration else {
            return Ok((value, Status::Held, index));
        };
        if at < duration {
            return Ok((
                curve_at(value, std::slice::from_ref(segment), at)?,
                Status::Running,
                index,
            ));
        }
        at = at.checked_sub(duration)?;
        value = segment.to;
    }
    Ok((value, Status::Complete, segments.len()))
}

pub(crate) fn advance_segment(
    initial: f64,
    segments: &[Segment],
    at: Time,
) -> Result<(usize, f64), Error> {
    let (value, status, index) = segment_value(initial, segments, at)?;
    if status != Status::Held {
        return Err(Error::Unsupported(
            "advance requires an active indefinite segment",
        ));
    }
    Ok((index + 1, value))
}
