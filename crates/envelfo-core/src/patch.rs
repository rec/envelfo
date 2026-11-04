//! Host-observed signal graphs and exact, bounded connection scheduling.
//!
//! ```
//! use envelfo_core::lfo::{LFO, Shape};
//! use envelfo_core::patch::{Child, Node, Output, Patch, Seconds};
//! use envelfo_core::time::Time;
//! # fn main() -> Result<(), envelfo_core::Error> {
//! let mut lfo = LFO::new(Time::ONE)?;
//! lfo.shape = Shape::Triangle;
//! let patch = Patch {
//!     children: vec![Child {
//!         name: "wave".into(), node: Node::LFO { body: lfo, markers: vec![] },
//!     }],
//!     outputs: vec![Output { name: "value".into(), source: "wave".into() }],
//!     event_outputs: vec![], connections: vec![],
//! };
//! let mut state = patch.initial(Time::ZERO, "gesture", 0, None, &Seconds)?;
//! let result = patch.advance(&mut state, Time::new(1, 4)?, Time::new(3, 10)?, &Seconds)?;
//! assert_eq!(result.values["value"], 0.0);
//! # Ok(())
//! # }
//! ```

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::Error;
use crate::envelope::{
    Envelope, EnvelopeCommand, EnvelopeState, Initial, Marker, Playback, Retrigger, Start,
    validate_markers,
};
use crate::lfo::{Command, Driver, LFO, LFOState};
use crate::random::{random_word, stream_key};
use crate::sample_hold::{SampleCommand, SampleHold, SampleState};
use crate::signal::Operator;
use crate::stages::{Body, Event, EventSink, OutputEvent, StageCommand, StageState, Stages};
use crate::time::Time;

/// Host seconds and a monotonic elapsed local clock (seconds or beats).
/// The inverse returns the first host time at which a local boundary is reached.
/// Stopped transport may leave future local boundaries unreachable.
pub trait ClockMapping {
    fn local_time(&self, child: &str, seconds: Time) -> Result<Time, Error>;
    fn host_time(&self, child: &str, local: Time) -> Result<Option<Time>, Error>;
}

#[derive(Clone, Copy, Debug)]
pub struct Seconds;

impl ClockMapping for Seconds {
    fn local_time(&self, _: &str, seconds: Time) -> Result<Time, Error> {
        Ok(seconds)
    }
    fn host_time(&self, _: &str, local: Time) -> Result<Option<Time>, Error> {
        Ok(Some(local))
    }
}

#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "snapshots", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "snapshots", serde(deny_unknown_fields))]
pub enum Node {
    LFO {
        body: LFO,
        markers: Vec<Marker>,
    },
    Envelope(Envelope),
    Stages(Stages),
    SampleHold(SampleHold),
    Signal(Operator),
    /// A host-owned signal, for example an instrument-scoped generator.
    Input,
}

/// Prepared stream identities let hosts preserve authored random streams when
/// several independently named graphs are materialized into one scheduler.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StreamKeys {
    pub samples: BTreeMap<String, u64>,
    pub connections: Vec<u64>,
}

#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "snapshots", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "snapshots", serde(deny_unknown_fields))]
pub struct Child {
    pub name: String,
    pub node: Node,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "snapshots", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "snapshots", serde(deny_unknown_fields))]
pub struct Port {
    pub child: String,
    pub port: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "snapshots", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "snapshots", serde(deny_unknown_fields))]
pub struct Output {
    pub name: String,
    pub source: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "snapshots", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "snapshots", serde(deny_unknown_fields))]
pub struct EventOutput {
    pub name: String,
    pub source: Port,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "snapshots", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "snapshots", serde(deny_unknown_fields))]
pub enum ConnectionAction {
    Start,
    Sample,
    Capture,
    Cue(String),
}

#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "snapshots", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "snapshots", serde(deny_unknown_fields))]
pub struct Connection {
    pub source: Port,
    pub target: String,
    pub action: ConnectionAction,
    pub every: u64,
    pub offset: u64,
    pub probability: f64,
    pub delay: Time,
}

#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "snapshots", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "snapshots", serde(deny_unknown_fields))]
pub struct Patch {
    pub children: Vec<Child>,
    pub outputs: Vec<Output>,
    pub event_outputs: Vec<EventOutput>,
    pub connections: Vec<Connection>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ChildCommand<T = String> {
    Playback(Command),
    Start,
    Advance,
    Sample,
    Cue(T),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChildEvent<T = String> {
    pub at: Time,
    pub ordinal: u64,
    pub command: ChildCommand<T>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PatchAdvance {
    pub values: BTreeMap<String, f64>,
    pub events: Vec<OutputEvent>,
}

/// Caller-owned buffers prepared for one unchanged Patch and its state lineage.
/// Preparation and snapshot restoration belong outside the render callback.
pub struct PatchStorage {
    next: PatchState,
    scratch: Scratch,
    events: Vec<OutputEvent<usize>>,
    values: Vec<f64>,
}

impl Clone for PatchStorage {
    fn clone(&self) -> Self {
        Self {
            next: self.next.clone(),
            scratch: self.scratch.clone(),
            events: clone_buffer(&self.events),
            values: clone_buffer(&self.values),
        }
    }
}

impl PatchStorage {
    /// Values follow the Patch's declared output order. Empty after failure or
    /// a command/settle call, which does not observe signals.
    pub fn values(&self) -> impl ExactSizeIterator<Item = f64> + '_ {
        self.values.iter().copied()
    }

    pub fn events(&self) -> impl ExactSizeIterator<Item = OutputEvent<&str>> + '_ {
        self.events.iter().map(|e| OutputEvent {
            at: e.at,
            port: self.scratch.names[e.port].as_str(),
            stage: self.scratch.names[e.stage].as_str(),
            activation: e.activation,
        })
    }

    fn begin(&mut self, state: &PatchState) -> Result<(), Error> {
        self.events.clear();
        self.values.clear();
        if state.pending.len() > self.scratch.pending_limit.unwrap_or(4096) {
            return Err(Error::PendingCapacity);
        }
        self.next.at = state.at;
        self.next.cursor = state.cursor;
        self.next.children.clone_from(&state.children);
        self.next.retained.copy_from_slice(&state.retained);
        self.next.inputs.copy_from_slice(&state.inputs);
        self.next.gates.clone_from(&state.gates);
        self.next.pending.clone_from(&state.pending);
        self.next.observed_at = state.observed_at;
        Ok(())
    }

    fn finish(&mut self, state: &mut PatchState, result: Result<(), Error>) -> Result<(), Error> {
        if result.is_ok() {
            std::mem::swap(state, &mut self.next);
        } else {
            self.events.clear();
            self.values.clear();
        }
        result
    }
}

#[derive(Debug, PartialEq)]
pub struct PatchState {
    at: Time,
    cursor: Time,
    name: String,
    seed: u64,
    children: Vec<Option<ChildState>>,
    order: Vec<usize>,
    retained: Vec<Option<f64>>,
    inputs: Vec<Option<f64>>,
    gates: Vec<Gate>,
    pending: Vec<Pending>,
    observed_at: Option<Time>,
}

impl Clone for PatchState {
    fn clone(&self) -> Self {
        Self {
            at: self.at,
            cursor: self.cursor,
            name: self.name.clone(),
            seed: self.seed,
            children: self.children.clone(),
            order: self.order.clone(),
            retained: self.retained.clone(),
            inputs: self.inputs.clone(),
            gates: self.gates.clone(),
            pending: clone_buffer(&self.pending),
            observed_at: self.observed_at,
        }
    }
}

#[cfg(feature = "snapshots")]
pub(crate) mod snapshot {
    use super::*;

    #[derive(serde::Serialize, serde::Deserialize)]
    #[serde(remote = "PatchState", deny_unknown_fields)]
    pub(crate) struct State {
        at: Time,
        cursor: Time,
        name: String,
        seed: u64,
        children: Vec<Option<ChildState>>,
        order: Vec<usize>,
        retained: Vec<Option<f64>>,
        inputs: Vec<Option<f64>>,
        gates: Vec<Gate>,
        pending: Vec<Pending>,
        observed_at: Option<Time>,
    }

    pub(crate) fn validate(body: &Patch, state: &PatchState) -> Result<(), Error> {
        let order = body.validate()?;
        let invalid = Error::InvalidDefinition("invalid Patch snapshot state");
        let size = body.children.len();
        if state.order != order
            || state.children.len() != size
            || state.retained.len() != size
            || state.inputs.len() != size
            || state.gates.len() != body.connections.len()
            || state.pending.len() > 4096
            || state.cursor > state.at
            || state.observed_at.is_some_and(|t| t > state.at)
        {
            return Err(invalid);
        }
        for (i, child) in body.children.iter().enumerate() {
            match (&child.node, &state.children[i]) {
                (Node::LFO { body, .. }, Some(ChildState::Lfo(s))) => {
                    crate::lfo::snapshot::validate(body, s)?
                }
                (Node::Envelope(body), Some(ChildState::Envelope(s))) => {
                    crate::envelope::snapshot::validate(body, s)?
                }
                (Node::Stages(body), Some(ChildState::Stages(s))) => {
                    crate::stages::snapshot::validate(body, s)?
                }
                (Node::SampleHold(body), Some(ChildState::Sample(s))) => {
                    crate::sample_hold::snapshot::validate(body, s)?
                }
                (Node::Signal(_) | Node::Input, None) => {}
                _ => return Err(invalid),
            }
            if state.retained[i].is_some_and(|v| !v.is_finite())
                || state.inputs[i].is_some_and(|v| !v.is_finite())
                || (state.inputs[i].is_some() && child.node != Node::Input)
                || (state.retained[i].is_some()
                    && !matches!(&child.node, Node::Signal(o) if o.retained()))
            {
                return Err(invalid);
            }
        }
        for (gate, connection) in state.gates.iter().zip(&body.connections) {
            if gate
                .count
                .is_some_and(|n| n >= connection.every.max(connection.offset))
            {
                return Err(invalid);
            }
        }
        for pending in &state.pending {
            let connection = body.connections.get(pending.order).ok_or(invalid)?;
            if pending.target != body.index(&connection.target)?
                || *pending.action != connection.action
                || pending.at < state.at
            {
                return Err(invalid);
            }
        }
        Ok(())
    }
}

impl PatchState {
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }
    pub fn next_delivery(&self) -> Option<Time> {
        self.pending.iter().map(|e| e.at).min()
    }
    pub fn observed_at(&self) -> Option<Time> {
        self.observed_at
    }
}

impl Patch {
    /// Allocate reusable transaction buffers. Pending capacity may be 0..=4096;
    /// emitted-event capacity includes output fan-out. Existing pending events
    /// must fit. Use only with this unchanged definition and state lineage;
    /// prepare again after restoring a snapshot or using an allocating execution
    /// method. Between preparations, use the bounded methods and `set_input`.
    pub fn prepare_storage(
        &self,
        state: &mut PatchState,
        pending_capacity: usize,
        event_capacity: usize,
    ) -> Result<PatchStorage, Error> {
        if pending_capacity > 4096 || state.pending.len() > pending_capacity {
            return Err(Error::PendingCapacity);
        }
        let mut scratch = Scratch::new(self, state);
        scratch.pending_limit = Some(pending_capacity);
        for pending in &mut state.pending {
            pending.action = Arc::clone(&scratch.actions[pending.order]);
        }
        state
            .pending
            .reserve(pending_capacity - state.pending.len());
        let mut next = state.clone();
        next.pending.reserve(pending_capacity - next.pending.len());
        Ok(PatchStorage {
            next,
            scratch,
            events: Vec::with_capacity(event_capacity),
            values: Vec::with_capacity(self.outputs.len()),
        })
    }

    /// Advance using prepared storage, without allocating. Errors restore the
    /// full state (including RNGs and pending deliveries) and clear outputs.
    /// The supplied clock must itself be allocation-free for callback use.
    pub fn advance_into(
        &self,
        state: &mut PatchState,
        at: Time,
        next_delivery: Time,
        clock: &impl ClockMapping,
        storage: &mut PatchStorage,
    ) -> Result<(), Error> {
        storage.begin(state)?;
        let names = Arc::clone(&storage.scratch.names);
        let result = (|| {
            if at < state.at {
                return Err(Error::TimeReversed);
            }
            if next_delivery <= at {
                return Err(Error::InvalidDefinition(
                    "next delivery must follow the observation",
                ));
            }
            let mut events = IndexedEvents {
                events: &mut storage.events,
                names: &names,
            };
            self.run(
                &mut storage.next,
                at,
                true,
                clock,
                &mut storage.scratch,
                &mut events,
            )?;
            self.observe(
                &mut storage.next,
                at,
                next_delivery,
                clock,
                &mut storage.scratch,
                &mut events,
            )?;
            for output in &self.outputs {
                storage.values.push(
                    storage.scratch.signals[self.index(&output.source)?]
                        .ok_or(Error::InvalidSignal)?,
                );
            }
            Ok(())
        })();
        storage.finish(state, result)
    }

    /// Deliver a borrowed command using prepared transaction and event storage.
    pub fn command_into(
        &self,
        state: &mut PatchState,
        target: &str,
        event: &ChildEvent<&str>,
        clock: &impl ClockMapping,
        inclusive: bool,
        storage: &mut PatchStorage,
    ) -> Result<(), Error> {
        storage.begin(state)?;
        let names = Arc::clone(&storage.scratch.names);
        let result = (|| {
            if event.at < state.at {
                return Err(Error::TimeReversed);
            }
            let target = self.index(target)?;
            let mut events = IndexedEvents {
                events: &mut storage.events,
                names: &names,
            };
            self.run(
                &mut storage.next,
                event.at,
                false,
                clock,
                &mut storage.scratch,
                &mut events,
            )?;
            self.deliver(
                &mut storage.next,
                target,
                event,
                clock,
                &mut events,
                &mut storage.scratch,
            )?;
            if inclusive {
                self.run(
                    &mut storage.next,
                    event.at,
                    true,
                    clock,
                    &mut storage.scratch,
                    &mut events,
                )?;
            }
            storage.next.at = event.at;
            Ok(())
        })();
        storage.finish(state, result)
    }

    /// Settle boundaries using prepared storage without observing signals.
    pub fn settle_into(
        &self,
        state: &mut PatchState,
        at: Time,
        inclusive: bool,
        clock: &impl ClockMapping,
        storage: &mut PatchStorage,
    ) -> Result<(), Error> {
        storage.begin(state)?;
        let names = Arc::clone(&storage.scratch.names);
        let result = (|| {
            if at < state.at {
                return Err(Error::TimeReversed);
            }
            let mut events = IndexedEvents {
                events: &mut storage.events,
                names: &names,
            };
            self.run(
                &mut storage.next,
                at,
                inclusive,
                clock,
                &mut storage.scratch,
                &mut events,
            )?;
            storage.next.at = at;
            Ok(())
        })();
        storage.finish(state, result)
    }

    /// Validate normalized definitions once and initialize without adding an observation.
    pub fn initial(
        &self,
        at: Time,
        name: &str,
        seed: u64,
        keys: Option<&StreamKeys>,
        clock: &impl ClockMapping,
    ) -> Result<PatchState, Error> {
        let order = self.validate()?;
        if keys.is_some_and(|k| k.connections.len() != self.connections.len()) {
            return Err(Error::InvalidDefinition("connection stream count mismatch"));
        }
        let children = self
            .children
            .iter()
            .map(|child| {
                let local = clock.local_time(&child.name, at)?;
                Ok(match &child.node {
                    Node::LFO { body, .. } => Some(ChildState::Lfo(body.initial(local)?)),
                    Node::Envelope(body) => Some(ChildState::Envelope(body.initial(local)?)),
                    Node::Stages(body) => Some(ChildState::Stages(body.initial(local)?)),
                    Node::SampleHold(body) => Some(ChildState::Sample(
                        body.initial(
                            local,
                            seed ^ keys
                                .and_then(|k| k.samples.get(&child.name))
                                .copied()
                                .unwrap_or_else(|| {
                                    stream_key(
                                        0,
                                        &format!("patch-{name}-{}:sample-hold", child.name),
                                    )
                                }),
                        )?,
                    )),
                    Node::Signal(_) | Node::Input => None,
                })
            })
            .collect::<Result<_, Error>>()?;
        Ok(PatchState {
            at,
            cursor: at,
            name: name.into(),
            seed,
            children,
            order,
            retained: vec![None; self.children.len()],
            inputs: vec![None; self.children.len()],
            gates: self
                .connections
                .iter()
                .enumerate()
                .map(|(i, _)| Gate {
                    count: None,
                    random: keys.map(|k| seed ^ k.connections[i]),
                })
                .collect(),
            pending: vec![],
            observed_at: None,
        })
    }

    /// Process all boundaries/deliveries through `at`, then observe exactly once.
    /// Threshold edges use the explicit next-delivery time, not a guessed frame/grid.
    /// Any error rolls back the entire call, including random draws and queued commands.
    pub fn advance(
        &self,
        state: &mut PatchState,
        at: Time,
        next_delivery: Time,
        clock: &impl ClockMapping,
    ) -> Result<PatchAdvance, Error> {
        if next_delivery <= at {
            return Err(Error::InvalidDefinition(
                "next delivery must follow the observation",
            ));
        }
        if at < state.at {
            return Err(Error::TimeReversed);
        }
        let mut next = state.clone();
        let mut scratch = Scratch::new(self, state);
        let mut events = OwnedEvents(Vec::new());
        self.run(&mut next, at, true, clock, &mut scratch, &mut events)?;
        self.observe(
            &mut next,
            at,
            next_delivery,
            clock,
            &mut scratch,
            &mut events,
        )?;
        let values = self
            .outputs
            .iter()
            .map(|o| {
                Ok((
                    o.name.clone(),
                    scratch.signals[self.index(&o.source)?].ok_or(Error::InvalidSignal)?,
                ))
            })
            .collect::<Result<_, Error>>()?;
        *state = next;
        Ok(PatchAdvance {
            values,
            events: events.0,
        })
    }

    fn observe(
        &self,
        next: &mut PatchState,
        at: Time,
        next_delivery: Time,
        clock: &impl ClockMapping,
        scratch: &mut Scratch,
        events: &mut impl for<'a> EventSink<'a>,
    ) -> Result<(), Error> {
        next.at = at;
        scratch.signals.fill(None);
        for (index, child) in self.children.iter().enumerate() {
            if let Some(current) = &next.children[index] {
                let local = clock.local_time(&child.name, at)?;
                scratch.signals[index] = Some(self.value(index, current, local)?);
            } else if child.node == Node::Input {
                scratch.signals[index] = Some(next.inputs[index].ok_or(
                    Error::InvalidDefinition("host signal has not been supplied"),
                )?);
            }
        }
        let seconds = match next.observed_at {
            Some(previous) => at.checked_sub(previous)?,
            None => Time::ZERO,
        };
        scratch.edges.clear();
        for &index in &next.order {
            if let Node::Signal(operator) = &self.children[index].node {
                let (value, edge) = operator.observe_with(
                    |name| {
                        scratch.signals[self.index(name)?]
                            .ok_or(Error::InvalidDefinition("missing Patch signal input"))
                    },
                    next.retained[index],
                    seconds,
                )?;
                scratch.signals[index] = Some(value);
                if operator.retained() {
                    next.retained[index] = Some(value);
                }
                if let Some(port) = edge {
                    scratch.edges.push((index, port));
                }
            }
        }
        next.observed_at = Some(at);
        for &(index, port) in &scratch.edges {
            self.route(
                next,
                index,
                OutputEvent {
                    at: next_delivery,
                    port,
                    stage: &self.children[index].name,
                    activation: 0,
                },
                next_delivery,
                events,
                scratch,
            )?;
        }
        Ok(())
    }

    /// Deliver a child command in host seconds, without observing the graph.
    pub fn command(
        &self,
        state: &mut PatchState,
        target: &str,
        event: &ChildEvent,
        clock: &impl ClockMapping,
        inclusive: bool,
    ) -> Result<Vec<OutputEvent>, Error> {
        if event.at < state.at {
            return Err(Error::TimeReversed);
        }
        let target = self.index(target)?;
        let mut next = state.clone();
        let mut scratch = Scratch::new(self, state);
        let mut emitted = OwnedEvents(Vec::new());
        self.run(
            &mut next,
            event.at,
            false,
            clock,
            &mut scratch,
            &mut emitted,
        )?;
        self.deliver(&mut next, target, event, clock, &mut emitted, &mut scratch)?;
        if inclusive {
            self.run(&mut next, event.at, true, clock, &mut scratch, &mut emitted)?;
        }
        next.at = event.at;
        *state = next;
        Ok(emitted.0)
    }

    /// Settle events without adding a signal observation. Hosts use an exclusive
    /// limit before a same-time command batch and an inclusive limit afterwards.
    pub fn settle(
        &self,
        state: &mut PatchState,
        at: Time,
        inclusive: bool,
        clock: &impl ClockMapping,
    ) -> Result<Vec<OutputEvent>, Error> {
        if at < state.at {
            return Err(Error::TimeReversed);
        }
        let mut next = state.clone();
        let mut scratch = Scratch::new(self, state);
        let mut events = OwnedEvents(Vec::new());
        self.run(&mut next, at, inclusive, clock, &mut scratch, &mut events)?;
        next.at = at;
        *state = next;
        Ok(events.0)
    }

    pub fn set_input(&self, state: &mut PatchState, child: &str, value: f64) -> Result<(), Error> {
        let index = self.index(child)?;
        if self.children[index].node != Node::Input || !value.is_finite() {
            return Err(Error::InvalidSignal);
        }
        state.inputs[index] = Some(value);
        Ok(())
    }

    /// Observe an executable child without changing retained operators or edges.
    pub fn child_value(
        &self,
        state: &PatchState,
        child: &str,
        at: Time,
        clock: &impl ClockMapping,
    ) -> Result<crate::lfo::Value, Error> {
        let index = self.index(child)?;
        let local = clock.local_time(child, at)?;
        match (&self.children[index].node, &state.children[index]) {
            (Node::LFO { body, .. }, Some(ChildState::Lfo(s))) => body.value(s, local),
            (Node::Envelope(body), Some(ChildState::Envelope(s))) => body.value(s, local),
            (Node::Stages(body), Some(ChildState::Stages(s))) => body.value(s, local),
            (Node::SampleHold(body), Some(ChildState::Sample(s))) => body.value(s, local),
            _ => Err(Error::Unsupported("child is not an executable generator")),
        }
    }

    pub fn next_ordinal(&self, state: &PatchState, child: &str) -> Result<u64, Error> {
        state.children[self.index(child)?]
            .as_ref()
            .ok_or(Error::Unsupported("child is not an executable generator"))?
            .next_ordinal()
    }

    fn validate(&self) -> Result<Vec<usize>, Error> {
        if self.children.is_empty() || self.outputs.is_empty() {
            return Err(Error::InvalidDefinition(
                "Patch requires children and named outputs",
            ));
        }
        for (index, child) in self.children.iter().enumerate() {
            if child.name.is_empty() || self.children[..index].iter().any(|c| c.name == child.name)
            {
                return Err(Error::InvalidDefinition("invalid or duplicate Patch child"));
            }
            match &child.node {
                Node::LFO { body, markers } => {
                    body.validate()?;
                    validate_markers(markers, false)?;
                    if body.driver != Driver::Elapsed && !markers.is_empty() {
                        return Err(Error::InvalidDefinition(
                            "Patch LFO requires elapsed position",
                        ));
                    }
                }
                Node::Envelope(body) => {
                    body.validate()?;
                    if body.initial == Initial::Current {
                        return Err(Error::InvalidDefinition(
                            "Patch Envelope needs explicit initial level",
                        ));
                    }
                }
                Node::Stages(body) => body.validate()?,
                Node::SampleHold(body) => body.validate()?,
                Node::Signal(body) => body.validate()?,
                Node::Input => {}
            }
        }
        let mut remaining = vec![0_usize; self.children.len()];
        let mut successors = vec![vec![]; self.children.len()];
        let mut encountered = vec![];
        for (index, child) in self.children.iter().enumerate() {
            if !encountered.contains(&index) {
                encountered.push(index);
            }
            if let Node::Signal(operator) = &child.node {
                for input in operator.inputs() {
                    let input = self.index(input)?;
                    if !encountered.contains(&input) {
                        encountered.push(input);
                    }
                    successors[input].push(index);
                    remaining[index] += 1;
                }
            }
        }
        // Match graphlib.TopologicalSorter: prerequisites are recorded when
        // first encountered, even before their own declaration is visited.
        let mut ready: Vec<usize> = encountered
            .into_iter()
            .filter(|i| remaining[*i] == 0)
            .collect();
        let mut order = vec![];
        while !ready.is_empty() {
            let batch = std::mem::take(&mut ready);
            for index in batch {
                order.push(index);
                for &successor in &successors[index] {
                    remaining[successor] -= 1;
                    if remaining[successor] == 0 {
                        ready.push(successor);
                    }
                }
            }
        }
        if order.len() != self.children.len() {
            return Err(Error::InvalidDefinition(
                "Patch signal dependencies contain a cycle",
            ));
        }
        let mut ranges = BTreeMap::new();
        for &index in &order {
            let child = &self.children[index];
            let range = match &child.node {
                Node::Signal(operator) => operator.range(&ranges)?,
                Node::Envelope(c) if !c.bipolar => (0.0, 1.0),
                _ => (-1.0, 1.0),
            };
            ranges.insert(child.name.clone(), range);
        }
        for (index, output) in self.outputs.iter().enumerate() {
            self.index(&output.source)?;
            if output.name.is_empty() || self.outputs[..index].iter().any(|o| o.name == output.name)
            {
                return Err(Error::InvalidDefinition(
                    "invalid or duplicate Patch output",
                ));
            }
        }
        for (index, output) in self.event_outputs.iter().enumerate() {
            self.validate_port(&output.source)?;
            if output.name.is_empty()
                || self.event_outputs[..index]
                    .iter()
                    .any(|o| o.name == output.name)
            {
                return Err(Error::InvalidDefinition(
                    "invalid or duplicate Patch event output",
                ));
            }
        }
        for connection in &self.connections {
            self.validate_port(&connection.source)?;
            if connection.every == 0
                || !connection.probability.is_finite()
                || !(0.0..=1.0).contains(&connection.probability)
                || connection.delay < Time::ZERO
            {
                return Err(Error::InvalidDefinition("invalid connection gate or delay"));
            }
            let target = &self.children[self.index(&connection.target)?].node;
            let valid = match (&connection.action, target) {
                (ConnectionAction::Capture, Node::Signal(Operator::Latch(_)))
                | (ConnectionAction::Sample, Node::SampleHold(_)) => true,
                (ConnectionAction::Start, Node::Envelope(c)) => {
                    c.start == Start::Event
                        && c.release.is_empty()
                        && c.playback == Playback::Once
                        && c.retrigger == Retrigger::Current
                }
                (ConnectionAction::Cue(cue), Node::Stages(s)) => {
                    !cue.is_empty()
                        && s.transitions
                            .iter()
                            .any(|t| t.event == format!("cue.{cue}"))
                }
                _ => false,
            };
            if !valid {
                return Err(Error::InvalidDefinition(
                    "connection action does not match its target",
                ));
            }
        }
        Ok(order)
    }

    fn index(&self, name: &str) -> Result<usize, Error> {
        self.children
            .iter()
            .position(|c| c.name == name)
            .ok_or(Error::InvalidDefinition("unknown Patch child"))
    }

    fn validate_port(&self, port: &Port) -> Result<(), Error> {
        let node = &self.children[self.index(&port.child)?].node;
        let valid = match node {
            Node::LFO { markers, .. } => markers.iter().any(|m| m.name == port.port),
            Node::Signal(Operator::Threshold { .. }) => {
                matches!(port.port.as_str(), "rising" | "falling")
            }
            Node::Stages(s) => {
                matches!(port.port.as_str(), "done" | "stage.done")
                    || s.stages.iter().any(|stage| {
                        let (markers, playback) = match &stage.body {
                            Body::Envelope(c) => (&c.markers, c.playback),
                            _ => (&stage.markers, Playback::Once),
                        };
                        markers.iter().any(|m| m.name == port.port)
                            || playback != Playback::Once
                                && (port.port == "cycle"
                                    || playback == Playback::PingPong && port.port == "turned")
                    })
            }
            _ => false,
        };
        if valid {
            Ok(())
        } else {
            Err(Error::InvalidDefinition("unknown Patch child event port"))
        }
    }

    fn value(&self, index: usize, state: &ChildState, local: Time) -> Result<f64, Error> {
        let value = match (&self.children[index].node, state) {
            (Node::LFO { body, .. }, ChildState::Lfo(state)) => body.value(state, local)?,
            (Node::Envelope(body), ChildState::Envelope(state)) => body.value(state, local)?,
            (Node::Stages(body), ChildState::Stages(state)) => body.value(state, local)?,
            (Node::SampleHold(body), ChildState::Sample(state)) => body.value(state, local)?,
            _ => {
                return Err(Error::InvalidDefinition(
                    "Patch state does not match its definition",
                ));
            }
        };
        Ok(value.value * value.weight)
    }

    fn route(
        &self,
        state: &mut PatchState,
        source: usize,
        event: OutputEvent<&str>,
        seconds: Time,
        emitted: &mut impl for<'a> EventSink<'a>,
        scratch: &Scratch,
    ) -> Result<(), Error> {
        let source = &self.children[source].name;
        let port = event.port;
        for output in &self.event_outputs {
            if output.source.child == *source && output.source.port == port {
                emitted.emit(OutputEvent {
                    at: seconds,
                    port: output.name.as_str(),
                    stage: event.stage,
                    activation: event.activation,
                })?;
            }
        }
        for (order, connection) in self.connections.iter().enumerate() {
            if connection.source.child != *source || connection.source.port != port {
                continue;
            }
            if state.gates[order].forward(connection, state.seed, scratch.keys[order]) {
                if state.pending.len() == scratch.pending_limit.unwrap_or(4096) {
                    return Err(Error::PendingCapacity);
                }
                state.pending.push(Pending {
                    at: seconds.checked_add(connection.delay)?,
                    order,
                    target: self.index(&connection.target)?,
                    action: Arc::clone(&scratch.actions[order]),
                });
            }
        }
        Ok(())
    }

    fn deliver<T: AsRef<str>>(
        &self,
        state: &mut PatchState,
        target: usize,
        event: &ChildEvent<T>,
        clock: &impl ClockMapping,
        emitted: &mut impl for<'a> EventSink<'a>,
        scratch: &mut Scratch,
    ) -> Result<(), Error> {
        let local = clock.local_time(&self.children[target].name, event.at)?;
        let current = state.children[target].as_mut().ok_or(Error::Unsupported(
            "commands require an executable Patch child",
        ))?;
        scratch.stages.clear();
        match (&self.children[target].node, current, &event.command) {
            (Node::LFO { body, .. }, ChildState::Lfo(current), ChildCommand::Playback(command)) => {
                body.command(current, local, event.ordinal, *command)?;
            }
            (Node::Envelope(body), ChildState::Envelope(current), command) => {
                let command = match command {
                    ChildCommand::Start => EnvelopeCommand::Start,
                    ChildCommand::Advance => EnvelopeCommand::Advance,
                    ChildCommand::Playback(c) => EnvelopeCommand::Playback(*c),
                    _ => return Err(Error::Unsupported("unsupported Envelope command")),
                };
                body.command(current, local, event.ordinal, command)?;
            }
            (Node::SampleHold(body), ChildState::Sample(current), command) => {
                let command = match command {
                    ChildCommand::Sample => SampleCommand::Sample,
                    ChildCommand::Playback(c) => SampleCommand::Playback(*c),
                    _ => return Err(Error::Unsupported("unsupported SampleHold command")),
                };
                body.command(current, local, event.ordinal, command)?;
            }
            (Node::Stages(body), ChildState::Stages(current), command) => {
                let command = match command {
                    ChildCommand::Playback(c) => StageCommand::Playback(*c),
                    ChildCommand::Cue(c) => StageCommand::Cue(c.as_ref()),
                    ChildCommand::Advance => StageCommand::Advance,
                    _ => return Err(Error::Unsupported("unsupported Stages command")),
                };
                body.advance_with(
                    current,
                    local,
                    Some(&Event {
                        at: local,
                        ordinal: event.ordinal,
                        command,
                    }),
                    &mut IndexedEvents {
                        events: &mut scratch.stages,
                        names: &scratch.names,
                    },
                )?;
            }
            _ => return Err(Error::Unsupported("unsupported Patch child command")),
        };
        for indexed in &scratch.stages {
            let output = OutputEvent {
                at: indexed.at,
                port: scratch.names[indexed.port].as_str(),
                stage: scratch.names[indexed.stage].as_str(),
                activation: indexed.activation,
            };
            let seconds = if output.at == local {
                event.at
            } else {
                clock
                    .host_time(&self.children[target].name, output.at)?
                    .ok_or(Error::InvalidDefinition("local event has no host time"))?
            };
            self.route(state, target, output, seconds, emitted, scratch)?;
        }
        Ok(())
    }

    fn run(
        &self,
        state: &mut PatchState,
        limit: Time,
        inclusive: bool,
        clock: &impl ClockMapping,
        scratch: &mut Scratch,
        emitted: &mut impl for<'a> EventSink<'a>,
    ) -> Result<(), Error> {
        let mut processed = 0_usize;
        loop {
            let within = |at: Time| if inclusive { at <= limit } else { at < limit };
            let mut next = state
                .pending
                .iter()
                .filter(|e| within(e.at))
                .map(|e| e.at)
                .min();
            scratch.lfos.clear();
            for (index, child) in self.children.iter().enumerate() {
                let local_limit = clock.local_time(&child.name, limit)?;
                match (&child.node, &state.children[index]) {
                    (Node::Stages(body), Some(ChildState::Stages(current))) => {
                        if let Some(event) = body.next_event(current, local_limit, true)? {
                            let seconds = clock
                                .host_time(&child.name, event.at)?
                                .ok_or(Error::InvalidDefinition("local event has no host time"))?;
                            if seconds >= state.at && within(seconds) {
                                next = Some(next.map_or(seconds, |n| n.min(seconds)));
                            }
                        }
                    }
                    (Node::LFO { body, markers }, Some(ChildState::Lfo(current)))
                        if !markers.is_empty() =>
                    {
                        let previous = clock.local_time(&child.name, state.cursor)?;
                        for (marker_index, marker) in markers.iter().enumerate() {
                            // Host-time inclusion is checked below, including stopped clocks.
                            let Some(local) = body.next_marker(
                                current,
                                marker.position,
                                previous,
                                local_limit,
                                true,
                            )?
                            else {
                                continue;
                            };
                            let seconds = clock
                                .host_time(&child.name, local)?
                                .ok_or(Error::InvalidDefinition("local marker has no host time"))?;
                            if within(seconds) {
                                scratch.lfos.push((seconds, index, marker_index, local));
                                next = Some(next.map_or(seconds, |n| n.min(seconds)));
                            }
                        }
                    }
                    _ => {}
                }
            }
            let Some(seconds) = next else {
                break;
            };
            for (index, child) in self.children.iter().enumerate() {
                if let (Node::Stages(body), Some(ChildState::Stages(current))) =
                    (&child.node, &mut state.children[index])
                {
                    let local = clock.local_time(&child.name, seconds)?;
                    scratch.stages.clear();
                    body.advance_with(
                        current,
                        local,
                        None::<&Event<&str>>,
                        &mut IndexedEvents {
                            events: &mut scratch.stages,
                            names: &scratch.names,
                        },
                    )?;
                    processed += scratch.stages.len();
                    for indexed in &scratch.stages {
                        let event = OutputEvent {
                            at: indexed.at,
                            port: scratch.names[indexed.port].as_str(),
                            stage: scratch.names[indexed.stage].as_str(),
                            activation: indexed.activation,
                        };
                        self.route(state, index, event, seconds, emitted, scratch)?;
                    }
                }
            }
            for &(at, index, marker, local) in &scratch.lfos {
                if at == seconds {
                    processed += 1;
                    let Node::LFO { markers, .. } = &self.children[index].node else {
                        unreachable!()
                    };
                    self.route(
                        state,
                        index,
                        OutputEvent {
                            at: local,
                            port: &markers[marker].name,
                            stage: &self.children[index].name,
                            activation: 0,
                        },
                        seconds,
                        emitted,
                        scratch,
                    )?;
                }
            }
            // Newly queued zero-delay connections join this phase. Newly emitted
            // commands from those deliveries are considered on the next pass.
            scratch.due.clear();
            scratch
                .due
                .extend(state.pending.iter().filter(|e| e.at == seconds).cloned());
            // Equal-order entries at this time have identical commands, targets
            // and timestamps, so their relative order cannot change delivery.
            scratch.due.sort_unstable_by_key(|e| e.order);
            for due_index in 0..scratch.due.len() {
                let event = scratch.due[due_index].clone();
                let index = state
                    .pending
                    .iter()
                    .position(|e| e == &event)
                    .expect("due delivery remains queued");
                state.pending.remove(index);
                processed += 1;
                if *event.action == ConnectionAction::Capture {
                    state.retained[event.target] = None;
                } else {
                    let ordinal = state.children[event.target]
                        .as_ref()
                        .ok_or(Error::Unsupported("connection target is not executable"))?
                        .next_ordinal()?;
                    let command = match event.action.as_ref() {
                        ConnectionAction::Start => ChildCommand::Start,
                        ConnectionAction::Sample => ChildCommand::Sample,
                        ConnectionAction::Cue(cue) => ChildCommand::Cue(cue.as_str()),
                        ConnectionAction::Capture => unreachable!(),
                    };
                    self.deliver(
                        state,
                        event.target,
                        &ChildEvent {
                            at: seconds,
                            ordinal,
                            command,
                        },
                        clock,
                        emitted,
                        scratch,
                    )?;
                }
            }
            if processed > 4096 {
                return Err(Error::Capacity);
            }
            state.at = seconds;
            state.cursor = seconds;
        }
        if inclusive {
            for (index, child) in self.children.iter().enumerate() {
                if let (Node::Stages(body), Some(ChildState::Stages(current))) =
                    (&child.node, &mut state.children[index])
                {
                    let local = clock.local_time(&child.name, limit)?;
                    scratch.stages.clear();
                    body.advance_with(
                        current,
                        local,
                        None::<&Event<&str>>,
                        &mut IndexedEvents {
                            events: &mut scratch.stages,
                            names: &scratch.names,
                        },
                    )?;
                    debug_assert!(
                        scratch.stages.is_empty(),
                        "scheduler must consume all Stage boundaries"
                    );
                }
            }
            state.cursor = limit;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "snapshots", derive(serde::Serialize, serde::Deserialize))]
enum ChildState {
    Lfo(#[cfg_attr(feature = "snapshots", serde(with = "crate::lfo::snapshot::State"))] LFOState),
    Envelope(
        #[cfg_attr(
            feature = "snapshots",
            serde(with = "crate::envelope::snapshot::State")
        )]
        EnvelopeState,
    ),
    Stages(
        #[cfg_attr(feature = "snapshots", serde(with = "crate::stages::snapshot::State"))]
        StageState,
    ),
    Sample(
        #[cfg_attr(
            feature = "snapshots",
            serde(with = "crate::sample_hold::snapshot::State")
        )]
        SampleState,
    ),
}

/// Working storage shared by every scheduler phase in one transaction. Preparing
/// it allocates; processing Stage events and queued cues reuses these buffers.
struct Scratch {
    pending_limit: Option<usize>,
    signals: Vec<Option<f64>>,
    edges: Vec<(usize, &'static str)>,
    stages: Vec<OutputEvent<usize>>,
    names: Arc<[String]>,
    lfos: Vec<(Time, usize, usize, Time)>,
    due: Vec<Pending>,
    actions: Vec<Arc<ConnectionAction>>,
    keys: Vec<u64>,
}

impl Clone for Scratch {
    fn clone(&self) -> Self {
        Self {
            pending_limit: self.pending_limit,
            signals: self.signals.clone(),
            edges: clone_buffer(&self.edges),
            stages: clone_buffer(&self.stages),
            names: Arc::clone(&self.names),
            lfos: clone_buffer(&self.lfos),
            due: clone_buffer(&self.due),
            actions: self.actions.clone(),
            keys: self.keys.clone(),
        }
    }
}

fn clone_buffer<T: Clone>(source: &Vec<T>) -> Vec<T> {
    let mut result = Vec::with_capacity(source.capacity());
    result.extend_from_slice(source);
    result
}

impl Scratch {
    fn new(patch: &Patch, state: &PatchState) -> Self {
        let mut names: Vec<String> = ["done", "stage.done", "cycle", "turned"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        for child in &patch.children {
            names.push(child.name.clone());
            match &child.node {
                Node::LFO { markers, .. } => names.extend(markers.iter().map(|m| m.name.clone())),
                Node::Stages(body) => {
                    for stage in &body.stages {
                        names.push(stage.name.clone());
                        let markers = match &stage.body {
                            Body::Envelope(e) => &e.markers,
                            Body::LFO(_) => &stage.markers,
                        };
                        names.extend(markers.iter().map(|m| m.name.clone()));
                    }
                }
                _ => {}
            }
        }
        names.extend(patch.event_outputs.iter().map(|o| o.name.clone()));
        names.sort();
        names.dedup();
        let markers = patch
            .children
            .iter()
            .map(|c| match &c.node {
                Node::LFO { markers, .. } => markers.len(),
                _ => 0,
            })
            .sum();
        let keys = patch
            .connections
            .iter()
            .enumerate()
            .map(|(i, c)| {
                if state.gates[i].random.is_some() || matches!(c.probability, 0.0 | 1.0) {
                    return 0;
                }
                let command = match &c.action {
                    ConnectionAction::Capture => format!("capture-{}-{i}", c.target),
                    ConnectionAction::Cue(_) => format!("cue-{i}"),
                    _ => format!("start-{i}"),
                };
                let target = if c.action == ConnectionAction::Capture {
                    state.name.clone()
                } else {
                    format!("patch-{}-{}", state.name, c.target)
                };
                stream_key(0, &format!("{target}:{command}"))
            })
            .collect();
        Self {
            pending_limit: None,
            signals: vec![None; patch.children.len()],
            edges: Vec::with_capacity(patch.children.len()),
            names: names.into(),
            stages: Vec::with_capacity(
                if patch
                    .children
                    .iter()
                    .any(|c| matches!(c.node, Node::Stages(_)))
                {
                    4096
                } else {
                    0
                },
            ),
            lfos: Vec::with_capacity(markers),
            due: Vec::with_capacity(if patch.connections.is_empty() {
                0
            } else {
                4096
            }),
            actions: patch
                .connections
                .iter()
                .map(|c| Arc::new(c.action.clone()))
                .collect(),
            keys,
        }
    }
}

struct IndexedEvents<'a> {
    events: &'a mut Vec<OutputEvent<usize>>,
    names: &'a [String],
}

impl<'a> EventSink<'a> for IndexedEvents<'_> {
    fn emit(&mut self, event: OutputEvent<&'a str>) -> Result<(), Error> {
        if self.events.len() == self.events.capacity() {
            return Err(Error::Capacity);
        }
        let index = |name: &str| {
            self.names
                .binary_search_by(|n| n.as_str().cmp(name))
                .map_err(|_| Error::InvalidDefinition("unprepared event name"))
        };
        self.events.push(OutputEvent {
            at: event.at,
            port: index(event.port)?,
            stage: index(event.stage)?,
            activation: event.activation,
        });
        Ok(())
    }
}

struct OwnedEvents(Vec<OutputEvent>);

impl<'a> EventSink<'a> for OwnedEvents {
    fn emit(&mut self, event: OutputEvent<&'a str>) -> Result<(), Error> {
        self.0.push(OutputEvent {
            at: event.at,
            port: event.port.into(),
            stage: event.stage.into(),
            activation: event.activation,
        });
        Ok(())
    }
}

impl ChildState {
    fn next_ordinal(&self) -> Result<u64, Error> {
        let ordinal = match self {
            Self::Lfo(s) => s.ordinal,
            Self::Envelope(s) => s.ordinal,
            Self::Stages(s) => s.ordinal,
            Self::Sample(s) => s.ordinal,
        };
        match ordinal {
            None => Ok(0),
            Some(o) => o.checked_add(1).ok_or(Error::Overflow),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "snapshots", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "snapshots", serde(deny_unknown_fields))]
struct Pending {
    at: Time,
    order: usize,
    target: usize,
    action: Arc<ConnectionAction>,
}

#[derive(Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "snapshots", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "snapshots", serde(deny_unknown_fields))]
struct Gate {
    count: Option<u64>,
    random: Option<u64>,
}

impl Gate {
    fn forward(&mut self, connection: &Connection, seed: u64, key: u64) -> bool {
        let count = self.count.unwrap_or(connection.offset);
        self.count = Some(if count == 0 {
            connection.every - 1
        } else {
            count - 1
        });
        let threshold = (connection.probability * (1_u64 << 53) as f64) as u64;
        if count != 0 || threshold == 0 {
            return false;
        }
        if threshold == 1_u64 << 53 {
            return true;
        }
        let state = self.random.get_or_insert(seed ^ key);
        random_word(state) >> 11 < threshold
    }
}
