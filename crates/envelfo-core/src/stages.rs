//! Exact natural boundaries, transitions and externally commanded Stage playback.

use crate::contour::{
    Contour, Initial, Marker, Playback, Start, curve_at, duration, rate, validate_markers,
};
use crate::cycle::{Command, Cycle, Direction, Driver, Value};
use crate::playback::{Position, check_order};
use crate::time::Time;
use crate::{Error, Status};

#[derive(Clone, Debug, PartialEq)]
pub enum Body {
    Hold(f64),
    Cycle(Cycle),
    Contour(Contour),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Stage {
    pub name: String,
    pub body: Body,
    pub markers: Vec<Marker>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Action {
    Enter(String),
    Finish,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Transition {
    pub from: Vec<String>,
    pub event: String,
    pub action: Action,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Stages {
    pub initial_stage: String,
    pub stages: Vec<Stage>,
    pub transitions: Vec<Transition>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StageCommand {
    Playback(Command),
    Cue(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Event {
    pub at: Time,
    pub ordinal: u64,
    pub command: StageCommand,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OutputEvent {
    pub at: Time,
    pub port: String,
    pub stage: String,
    pub activation: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Advance {
    pub value: Value,
    pub events: Vec<OutputEvent>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StageState {
    at: Time,
    pub(crate) ordinal: Option<u64>,
    stage: usize,
    entry_value: f64,
    position: Position,
    activation: u64,
    cursor: Option<usize>,
    completed: bool,
    complete_value: Option<f64>,
    traversals: u64,
}

impl StageState {
    pub fn activation(&self) -> u64 {
        self.activation
    }
    pub fn completed(&self) -> bool {
        self.completed
    }
    pub fn traversals(&self) -> u64 {
        self.traversals
    }
}

impl Stages {
    pub fn validate(&self) -> Result<(), Error> {
        let initial = self.index(&self.initial_stage)?;
        for (index, stage) in self.stages.iter().enumerate() {
            if stage.name.is_empty() || self.stages[..index].iter().any(|s| s.name == stage.name) {
                return Err(Error::InvalidDefinition("invalid or duplicate Stage name"));
            }
            match &stage.body {
                Body::Hold(v) if !v.is_finite() || !(-1.0..=1.0).contains(v) => {
                    return Err(Error::InvalidDefinition("invalid Hold level"));
                }
                Body::Hold(_) if !stage.markers.is_empty() => {
                    return Err(Error::InvalidDefinition("Hold cannot have markers"));
                }
                Body::Hold(_) => {}
                Body::Cycle(c) => {
                    c.validate()?;
                    if c.driver != Driver::Elapsed {
                        return Err(Error::InvalidDefinition(
                            "Stage Cycle requires elapsed position",
                        ));
                    }
                    validate_markers(&stage.markers, false)?;
                }
                Body::Contour(c) => {
                    c.validate()?;
                    if !stage.markers.is_empty()
                        || !c.release.is_empty()
                        || c.start == Start::Event
                        || duration(&c.segments)? == Time::ZERO
                        || (index == initial && c.initial == Initial::Current)
                    {
                        return Err(Error::InvalidDefinition(
                            "invalid staged Contour release, start, duration or initial level",
                        ));
                    }
                    if c.playback != Playback::Once
                        && c.markers.iter().any(|m| {
                            m.position <= Time::ZERO
                                || m.position >= Time::ONE
                                || matches!(m.name.as_str(), "cycle" | "turned")
                        })
                    {
                        return Err(Error::InvalidDefinition("invalid repeated Stage marker"));
                    }
                }
            }
            if markers(stage).iter().any(|m| m.name == "done") {
                return Err(Error::InvalidDefinition("done is a reserved Stage marker"));
            }
        }
        for transition in &self.transitions {
            if transition.from.is_empty() {
                return Err(Error::InvalidDefinition("transition needs a source Stage"));
            }
            if let Action::Enter(name) = &transition.action {
                self.index(name)?;
            }
            for source in &transition.from {
                let stage = &self.stages[self.index(source)?];
                let event = transition.event.as_str();
                let allowed = matches!(event, "note_on" | "note_off" | "stage.done") || event.strip_prefix("cue.").is_some_and(|s| !s.is_empty())
                    || event.strip_prefix("stage.").is_some_and(|s| markers(stage).iter().any(|m| m.name == s)
                        || matches!(&stage.body, Body::Contour(c) if c.playback != Playback::Once && (s == "cycle" || c.playback == Playback::PingPong && s == "turned")));
                if !allowed {
                    return Err(Error::InvalidDefinition(
                        "transition references an unavailable Stage event",
                    ));
                }
            }
        }
        Ok(())
    }

    pub fn initial(&self, at: Time) -> Result<StageState, Error> {
        self.validate()?;
        let stage = self.index(&self.initial_stage)?;
        let entry_value = match &self.stages[stage].body {
            Body::Contour(c) => match c.initial {
                Initial::Value(v) => v,
                Initial::Current => unreachable!(),
            },
            _ => 0.0,
        };
        Ok(StageState {
            at,
            ordinal: None,
            stage,
            entry_value,
            position: initial_position(&self.stages[stage].body, at)?,
            activation: 0,
            cursor: None,
            completed: false,
            complete_value: None,
            traversals: 0,
        })
    }

    pub fn stage_name<'a>(&'a self, state: &StageState) -> &'a str {
        &self.stages[state.stage].name
    }

    pub fn value(&self, state: &StageState, at: Time) -> Result<Value, Error> {
        if at < state.at {
            return Err(Error::TimeReversed);
        }
        if let Some(value) = state.complete_value {
            return Ok(Value {
                value,
                weight: 1.0,
                status: Status::Complete,
            });
        }
        let position = state.position.advanced(at)?;
        match &self.stages[state.stage].body {
            Body::Hold(value) => Ok(Value {
                value: *value,
                weight: 1.0,
                status: Status::Held,
            }),
            Body::Cycle(c) => c.staged_value(&position),
            Body::Contour(c) => {
                let complete = c.repeat_count.is_some_and(|n| state.traversals >= n);
                let coordinate = if complete {
                    c.boundary_coordinate(position.coordinate, position.direction)?
                } else {
                    c.coordinate(position.coordinate, false)?
                };
                Ok(Value {
                    value: curve_at(
                        state.entry_value,
                        &c.segments,
                        coordinate.checked_mul(duration(&c.segments)?)?,
                    )?,
                    weight: 1.0,
                    status: if complete {
                        Status::Complete
                    } else if c.playback != Playback::Once || coordinate < Time::ONE {
                        Status::Running
                    } else {
                        Status::Held
                    },
                })
            }
        }
    }

    /// Process natural boundaries and optional command at `at`. Errors leave state untouched.
    /// Use this rather than `value` when transitions must be processed.
    pub fn advance(
        &self,
        state: &mut StageState,
        at: Time,
        event: Option<&Event>,
    ) -> Result<Advance, Error> {
        if at < state.at {
            return Err(Error::TimeReversed);
        }
        if let Some(event) = event {
            if event.at != at {
                return Err(Error::InvalidDefinition(
                    "command time must equal advance time",
                ));
            }
            check_order(state.at, state.ordinal, at, event.ordinal)?;
        }
        let mut next = state.clone();
        let mut events = vec![];
        self.natural(&mut next, at, event.is_none(), &mut events)?;
        if let Some(event) = event {
            match &event.command {
                StageCommand::Playback(
                    c @ (Command::Pause
                    | Command::Resume
                    | Command::Reverse
                    | Command::Seek(_)
                    | Command::Shift(_)
                    | Command::Rate(_)),
                ) => {
                    if matches!(self.stages[next.stage].body, Body::Hold(_))
                        && matches!(c, Command::Seek(_) | Command::Shift(_))
                    {
                        return Err(Error::Unsupported("Hold cannot seek or shift"));
                    }
                    next.position = next.position.advanced(at)?;
                    next.position.command(*c)?;
                    if matches!(c, Command::Shift(_))
                        && matches!(&self.stages[next.stage].body, Body::Contour(c) if c.playback == Playback::Once)
                    {
                        next.position.coordinate =
                            next.position.coordinate.clamp(Time::ZERO, Time::ONE);
                    }
                    next.cursor = None;
                }
                StageCommand::Playback(Command::NoteOn | Command::NoteOff)
                | StageCommand::Cue(_) => {
                    let key = match &event.command {
                        StageCommand::Cue(s) => format!("cue.{s}"),
                        StageCommand::Playback(Command::NoteOn) => "note_on".into(),
                        _ => "note_off".into(),
                    };
                    let activation = next.activation;
                    self.transition(&mut next, at, &key, &mut events)?;
                    if next.activation == activation && !next.completed {
                        self.natural(&mut next, at, true, &mut events)?;
                    }
                }
                _ => return Err(Error::Unsupported("unsupported Stages command")),
            }
            next.at = at;
            next.ordinal = Some(event.ordinal);
        }
        let value = self.value(&next, at)?;
        *state = next;
        Ok(Advance { value, events })
    }

    pub fn next_event(
        &self,
        state: &StageState,
        limit: Time,
        inclusive: bool,
    ) -> Result<Option<OutputEvent>, Error> {
        Ok(self.candidate(state, limit, inclusive)?.map(|c| {
            self.output(
                state,
                c.at,
                if c.port == "done" {
                    "stage.done"
                } else {
                    &c.port
                },
            )
        }))
    }

    fn index(&self, name: &str) -> Result<usize, Error> {
        self.stages
            .iter()
            .position(|s| s.name == name)
            .ok_or(Error::InvalidDefinition("unknown Stage name"))
    }

    fn output(&self, state: &StageState, at: Time, port: &str) -> OutputEvent {
        OutputEvent {
            at,
            port: port.into(),
            stage: self.stage_name(state).into(),
            activation: state.activation,
        }
    }

    fn transition(
        &self,
        state: &mut StageState,
        at: Time,
        key: &str,
        events: &mut Vec<OutputEvent>,
    ) -> Result<(), Error> {
        if state.completed {
            return Ok(());
        }
        let Some(transition) = self
            .transitions
            .iter()
            .find(|t| t.event == key && t.from.iter().any(|s| s == self.stage_name(state)))
        else {
            return Ok(());
        };
        let value = self.value(state, at)?.value;
        match &transition.action {
            Action::Finish => {
                state.at = at;
                state.completed = true;
                state.complete_value = Some(value);
                emit(events, self.output(state, at, "done"))?;
            }
            Action::Enter(name) => {
                let stage = self.index(name)?;
                let body = &self.stages[stage].body;
                let entry_value = match body {
                    Body::Contour(c) => match c.initial {
                        Initial::Value(v) => v,
                        Initial::Current => value,
                    },
                    _ => value,
                };
                let mut position = initial_position(body, at)?;
                position.paused = state.position.paused;
                position.direction = state.position.direction;
                *state = StageState {
                    at,
                    ordinal: state.ordinal,
                    stage,
                    entry_value,
                    position,
                    activation: state.activation.checked_add(1).ok_or(Error::Overflow)?,
                    cursor: None,
                    completed: false,
                    complete_value: None,
                    traversals: 0,
                };
            }
        }
        Ok(())
    }

    fn natural(
        &self,
        state: &mut StageState,
        at: Time,
        inclusive: bool,
        events: &mut Vec<OutputEvent>,
    ) -> Result<(), Error> {
        while let Some(candidate) = self.candidate(state, at, inclusive)? {
            let stage = state.stage;
            let activation = state.activation;
            if state.at != candidate.at {
                state.ordinal = None;
            }
            state.at = candidate.at;
            state.cursor = Some(candidate.order);
            state.position = state.position.advanced(candidate.at)?;
            let mut final_repeat = false;
            let mut final_cycle = false;
            if let Body::Contour(c) = &self.stages[stage].body {
                if let Some(count) = c.repeat_count {
                    if candidate.port
                        == if c.playback == Playback::Loop {
                            "cycle"
                        } else {
                            "turned"
                        }
                    {
                        state.traversals =
                            state.traversals.checked_add(1).ok_or(Error::Overflow)?;
                        final_repeat = state.traversals == count;
                        let (start, end) = c.bounds();
                        final_cycle = final_repeat
                            && c.playback == Playback::PingPong
                            && state
                                .position
                                .coordinate
                                .checked_sub(end)?
                                .checked_div(end.checked_sub(start)?)?
                                .checked_mod(Time::from_integer(2))?
                                == Time::ONE;
                    }
                }
            }
            emit(
                events,
                self.output(
                    state,
                    candidate.at,
                    if candidate.port == "done" {
                        "stage.done"
                    } else {
                        &candidate.port
                    },
                ),
            )?;
            if final_repeat {
                if final_cycle {
                    emit(events, self.output(state, candidate.at, "cycle"))?;
                }
                emit(events, self.output(state, candidate.at, "stage.done"))?;
            }
            self.transition(
                state,
                candidate.at,
                &format!("stage.{}", candidate.port),
                events,
            )?;
            if final_repeat && state.stage == stage && state.activation == activation {
                self.transition(state, candidate.at, "stage.done", events)?;
                if state.stage == stage && state.activation == activation {
                    state.complete_value = Some(self.value(state, candidate.at)?.value);
                    state.completed = true;
                }
            }
        }
        if inclusive {
            if state.at != at {
                state.ordinal = None;
                state.cursor = None;
            }
            state.at = at;
            state.position = state.position.advanced(at)?;
        }
        Ok(())
    }

    fn candidate(
        &self,
        state: &StageState,
        limit: Time,
        inclusive: bool,
    ) -> Result<Option<Candidate>, Error> {
        if limit < state.at {
            return Err(Error::TimeReversed);
        }
        if state.completed || state.position.paused || state.position.rate == Time::ZERO {
            return Ok(None);
        }
        let stage = &self.stages[state.stage];
        let coordinate = state.position.coordinate;
        let forward = state.position.direction == Direction::Forward;
        let mut candidates = vec![];
        let mut add = |target: Time, order: usize, port: &str, same: bool| -> Result<(), Error> {
            if (if forward {
                target > coordinate
            } else {
                target < coordinate
            }) || target == coordinate && same
            {
                let at = state.at.checked_add(
                    target
                        .checked_sub(coordinate)?
                        .checked_abs()?
                        .checked_div(state.position.rate)?,
                )?;
                if if inclusive { at <= limit } else { at < limit } {
                    candidates.push(Candidate {
                        at,
                        order,
                        port: port.into(),
                    });
                }
            }
            Ok(())
        };
        let after = |order: usize| {
            state.cursor.is_some_and(|cursor| {
                if forward {
                    order > cursor
                } else {
                    order < cursor
                }
            })
        };
        match &stage.body {
            Body::Hold(_) => {}
            Body::Cycle(_) => {
                for (order, marker) in stage.markers.iter().enumerate() {
                    let offset = coordinate.checked_sub(marker.position)?;
                    let turn = next_integer(offset, forward)?;
                    add(
                        Time::from_integer(turn).checked_add(marker.position)?,
                        order,
                        &marker.name,
                        false,
                    )?;
                    if offset.modulo_one() == Time::ZERO {
                        add(coordinate, order, &marker.name, after(order))?;
                    }
                }
            }
            Body::Contour(c) if c.playback == Playback::Once => {
                for (order, marker) in c.markers.iter().enumerate() {
                    add(marker.position, order, &marker.name, after(order))?;
                }
                if forward {
                    add(Time::ONE, c.markers.len(), "done", after(c.markers.len()))?;
                }
            }
            Body::Contour(c) => {
                let (start, end) = c.bounds();
                let width = end.checked_sub(start)?;
                let period = if c.playback == Playback::Loop {
                    width
                } else {
                    width.checked_mul(Time::from_integer(2))?
                };
                let named = c.loop_bounds.is_some();
                let same = |order: usize| {
                    state.cursor.is_some_and(|cursor| {
                        if forward {
                            order > cursor
                        } else if named {
                            cursor < c.markers.len() && (order < cursor || order == c.markers.len())
                        } else {
                            order < cursor
                        }
                    })
                };
                for (order, marker) in c.markers.iter().enumerate() {
                    let bases = if named {
                        add(marker.position, order, &marker.name, same(order))?;
                        if marker.position < start {
                            continue;
                        }
                        if c.playback == Playback::Loop {
                            vec![end.checked_add(marker.position.checked_sub(start)?)?]
                        } else {
                            let mut bases = vec![];
                            if marker.position < end {
                                bases.push(end.checked_add(end)?.checked_sub(marker.position)?);
                            }
                            if marker.position > start {
                                bases.push(
                                    end.checked_add(width)?
                                        .checked_add(marker.position.checked_sub(start)?)?,
                                );
                            }
                            bases
                        }
                    } else if c.playback == Playback::Loop {
                        vec![marker.position]
                    } else {
                        vec![
                            marker.position,
                            Time::from_integer(2).checked_sub(marker.position)?,
                        ]
                    };
                    for base in bases {
                        let offset = coordinate.checked_sub(base)?;
                        let mut turn = next_integer(offset.checked_div(period)?, forward)?;
                        if named && forward {
                            turn = turn.max(0);
                        }
                        if !named || turn >= 0 {
                            add(
                                base.checked_add(period.checked_mul(Time::from_integer(turn))?)?,
                                order,
                                &marker.name,
                                false,
                            )?;
                        }
                        if (!named || coordinate >= base)
                            && offset.checked_mod(period)? == Time::ZERO
                        {
                            add(coordinate, order, &marker.name, same(order))?;
                        }
                    }
                }
                let base = if named { end } else { Time::ZERO };
                let offset = coordinate.checked_sub(base)?.checked_div(width)?;
                let mut boundary = next_integer(offset, forward)?;
                if named && forward {
                    boundary = boundary.max(0);
                }
                let order = c.markers.len();
                let port = if c.playback == Playback::Loop {
                    "cycle"
                } else {
                    "turned"
                };
                let cycle_boundary = |index: i64| {
                    c.playback == Playback::PingPong
                        && index.rem_euclid(2) == if named { 1 } else { 0 }
                };
                if !named || boundary >= 0 {
                    let target =
                        base.checked_add(width.checked_mul(Time::from_integer(boundary))?)?;
                    add(target, order, port, false)?;
                    if cycle_boundary(boundary) {
                        add(target, order + 1, "cycle", false)?;
                    }
                }
                if (!named || coordinate >= end) && offset.modulo_one() == Time::ZERO {
                    if named {
                        add(coordinate, order, port, same(order))?;
                    }
                    if cycle_boundary(offset.floor()) && state.cursor == Some(order) {
                        add(coordinate, order + 1, "cycle", true)?;
                    }
                }
            }
        }
        candidates.sort_by(|a, b| {
            a.at.cmp(&b.at).then_with(|| {
                let rank = |c: &Candidate| {
                    if forward || matches!(c.port.as_str(), "cycle" | "turned") {
                        c.order as i128
                    } else {
                        -(c.order as i128)
                    }
                };
                rank(a).cmp(&rank(b)).then_with(|| a.port.cmp(&b.port))
            })
        });
        Ok(candidates.into_iter().next())
    }
}

#[derive(Clone, Debug)]
struct Candidate {
    at: Time,
    order: usize,
    port: String,
}

fn markers(stage: &Stage) -> &[Marker] {
    match &stage.body {
        Body::Contour(c) => &c.markers,
        _ => &stage.markers,
    }
}

fn initial_position(body: &Body, at: Time) -> Result<Position, Error> {
    let (coordinate, rate) = match body {
        Body::Hold(_) => (Time::ZERO, Time::ZERO),
        Body::Cycle(c) => (c.phase, c.rate),
        Body::Contour(c) => (Time::ZERO, rate(&c.segments)?),
    };
    Ok(Position::new(at, coordinate, rate))
}

fn next_integer(value: Time, forward: bool) -> Result<i64, Error> {
    if forward {
        value.floor().checked_add(1)
    } else {
        value.ceil().checked_sub(1)
    }
    .ok_or(Error::Overflow)
}

fn emit(events: &mut Vec<OutputEvent>, event: OutputEvent) -> Result<(), Error> {
    if events.len() == 4096 {
        return Err(Error::Capacity);
    }
    events.push(event);
    Ok(())
}
