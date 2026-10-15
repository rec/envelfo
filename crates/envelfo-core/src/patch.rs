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

use crate::Error;
use crate::envelope::{
    Envelope, EnvelopeCommand, EnvelopeState, Initial, Marker, Playback, Retrigger, Start,
    validate_markers,
};
use crate::lfo::{Command, Driver, LFO, LFOState};
use crate::random::{random_word, stream_key};
use crate::sample_hold::{SampleCommand, SampleHold, SampleState};
use crate::signal::Operator;
use crate::stages::{Body, Event, OutputEvent, StageCommand, StageState, Stages};
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
pub enum ChildCommand {
    Playback(Command),
    Start,
    Advance,
    Sample,
    Cue(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChildEvent {
    pub at: Time,
    pub ordinal: u64,
    pub command: ChildCommand,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PatchAdvance {
    pub values: BTreeMap<String, f64>,
    pub events: Vec<OutputEvent>,
}

#[derive(Clone, Debug, PartialEq)]
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
                || pending.action != connection.action
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
        let mut events = self.run(&mut next, at, true, clock)?;
        next.at = at;
        let mut signals = BTreeMap::new();
        for (index, child) in self.children.iter().enumerate() {
            if let Some(current) = &next.children[index] {
                let local = clock.local_time(&child.name, at)?;
                signals.insert(child.name.clone(), self.value(index, current, local)?);
            } else if child.node == Node::Input {
                signals.insert(
                    child.name.clone(),
                    next.inputs[index].ok_or(Error::InvalidDefinition(
                        "host signal has not been supplied",
                    ))?,
                );
            }
        }
        let seconds = match next.observed_at {
            Some(previous) => at.checked_sub(previous)?,
            None => Time::ZERO,
        };
        let mut edges = vec![];
        for &index in &next.order {
            if let Node::Signal(operator) = &self.children[index].node {
                let (value, edge) = operator.observe(&signals, next.retained[index], seconds)?;
                signals.insert(self.children[index].name.clone(), value);
                if operator.retained() {
                    next.retained[index] = Some(value);
                }
                if let Some(port) = edge {
                    edges.push((index, port));
                }
            }
        }
        next.observed_at = Some(at);
        for (index, port) in edges {
            self.route(
                &mut next,
                index,
                OutputEvent {
                    at: next_delivery,
                    port: port.into(),
                    stage: self.children[index].name.clone(),
                    activation: 0,
                },
                next_delivery,
                &mut events,
            )?;
        }
        let values = self
            .outputs
            .iter()
            .map(|o| (o.name.clone(), signals[&o.source]))
            .collect();
        *state = next;
        Ok(PatchAdvance { values, events })
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
        let mut emitted = self.run(&mut next, event.at, false, clock)?;
        self.deliver(&mut next, target, event, clock, &mut emitted)?;
        if inclusive {
            emitted.extend(self.run(&mut next, event.at, true, clock)?);
        }
        next.at = event.at;
        *state = next;
        Ok(emitted)
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
        let events = self.run(&mut next, at, inclusive, clock)?;
        next.at = at;
        *state = next;
        Ok(events)
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
        mut event: OutputEvent,
        seconds: Time,
        emitted: &mut Vec<OutputEvent>,
    ) -> Result<(), Error> {
        let source = &self.children[source].name;
        let port = event.port.clone();
        event.at = seconds;
        for output in &self.event_outputs {
            if output.source.child == *source && output.source.port == port {
                let mut output_event = event.clone();
                output_event.port.clone_from(&output.name);
                emitted.push(output_event);
            }
        }
        for (order, connection) in self.connections.iter().enumerate() {
            if connection.source.child != *source || connection.source.port != port {
                continue;
            }
            let capture = connection.action == ConnectionAction::Capture;
            let command = match &connection.action {
                ConnectionAction::Capture => format!("capture-{}-{order}", connection.target),
                ConnectionAction::Cue(_) => format!("cue-{order}"),
                _ => format!("start-{order}"),
            };
            let target_name = if capture {
                state.name.clone()
            } else {
                format!("patch-{}-{}", state.name, connection.target)
            };
            let identity = format!("{target_name}:{command}");
            if state.gates[order].forward(connection, state.seed, &identity) {
                if state.pending.len() == 4096 {
                    return Err(Error::PendingCapacity);
                }
                state.pending.push(Pending {
                    at: seconds.checked_add(connection.delay)?,
                    order,
                    target: self.index(&connection.target)?,
                    action: connection.action.clone(),
                });
            }
        }
        Ok(())
    }

    fn deliver(
        &self,
        state: &mut PatchState,
        target: usize,
        event: &ChildEvent,
        clock: &impl ClockMapping,
        emitted: &mut Vec<OutputEvent>,
    ) -> Result<(), Error> {
        let local = clock.local_time(&self.children[target].name, event.at)?;
        let current = state.children[target].as_mut().ok_or(Error::Unsupported(
            "commands require an executable Patch child",
        ))?;
        let outputs = match (&self.children[target].node, current, &event.command) {
            (Node::LFO { body, .. }, ChildState::Lfo(current), ChildCommand::Playback(command)) => {
                body.command(current, local, event.ordinal, *command)?;
                vec![]
            }
            (Node::Envelope(body), ChildState::Envelope(current), command) => {
                let command = match command {
                    ChildCommand::Start => EnvelopeCommand::Start,
                    ChildCommand::Advance => EnvelopeCommand::Advance,
                    ChildCommand::Playback(c) => EnvelopeCommand::Playback(*c),
                    _ => return Err(Error::Unsupported("unsupported Envelope command")),
                };
                body.command(current, local, event.ordinal, command)?;
                vec![]
            }
            (Node::SampleHold(body), ChildState::Sample(current), command) => {
                let command = match command {
                    ChildCommand::Sample => SampleCommand::Sample,
                    ChildCommand::Playback(c) => SampleCommand::Playback(*c),
                    _ => return Err(Error::Unsupported("unsupported SampleHold command")),
                };
                body.command(current, local, event.ordinal, command)?;
                vec![]
            }
            (Node::Stages(body), ChildState::Stages(current), command) => {
                let command = match command {
                    ChildCommand::Playback(c) => StageCommand::Playback(*c),
                    ChildCommand::Cue(c) => StageCommand::Cue(c.clone()),
                    ChildCommand::Advance => StageCommand::Advance,
                    _ => return Err(Error::Unsupported("unsupported Stages command")),
                };
                body.advance(
                    current,
                    local,
                    Some(&Event {
                        at: local,
                        ordinal: event.ordinal,
                        command,
                    }),
                )?
                .events
            }
            _ => return Err(Error::Unsupported("unsupported Patch child command")),
        };
        for output in outputs {
            let seconds = if output.at == local {
                event.at
            } else {
                clock
                    .host_time(&self.children[target].name, output.at)?
                    .ok_or(Error::InvalidDefinition("local event has no host time"))?
            };
            self.route(state, target, output, seconds, emitted)?;
        }
        Ok(())
    }

    fn run(
        &self,
        state: &mut PatchState,
        limit: Time,
        inclusive: bool,
        clock: &impl ClockMapping,
    ) -> Result<Vec<OutputEvent>, Error> {
        let mut emitted = vec![];
        let mut processed = 0_usize;
        loop {
            let within = |at: Time| if inclusive { at <= limit } else { at < limit };
            let mut next = state
                .pending
                .iter()
                .filter(|e| within(e.at))
                .map(|e| e.at)
                .min();
            let mut lfos = vec![];
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
                        for marker in markers {
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
                                lfos.push((seconds, index, marker.name.clone(), local));
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
                    let result = body.advance(current, local, None)?;
                    processed += result.events.len();
                    for event in result.events {
                        self.route(state, index, event, seconds, &mut emitted)?;
                    }
                }
            }
            for (at, index, port, local) in lfos {
                if at == seconds {
                    processed += 1;
                    self.route(
                        state,
                        index,
                        OutputEvent {
                            at: local,
                            port,
                            stage: self.children[index].name.clone(),
                            activation: 0,
                        },
                        seconds,
                        &mut emitted,
                    )?;
                }
            }
            // Newly queued zero-delay connections join this phase. Newly emitted
            // commands from those deliveries are considered on the next pass.
            let mut due: Vec<Pending> = state
                .pending
                .iter()
                .filter(|e| e.at == seconds)
                .cloned()
                .collect();
            due.sort_by_key(|e| e.order);
            for event in due {
                let index = state
                    .pending
                    .iter()
                    .position(|e| e == &event)
                    .expect("due delivery remains queued");
                state.pending.remove(index);
                processed += 1;
                if event.action == ConnectionAction::Capture {
                    state.retained[event.target] = None;
                } else {
                    let ordinal = state.children[event.target]
                        .as_ref()
                        .ok_or(Error::Unsupported("connection target is not executable"))?
                        .next_ordinal()?;
                    let command = match event.action {
                        ConnectionAction::Start => ChildCommand::Start,
                        ConnectionAction::Sample => ChildCommand::Sample,
                        ConnectionAction::Cue(cue) => ChildCommand::Cue(cue),
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
                        &mut emitted,
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
                    let result = body.advance(current, local, None)?;
                    debug_assert!(
                        result.events.is_empty(),
                        "scheduler must consume all Stage boundaries"
                    );
                }
            }
            state.cursor = limit;
        }
        Ok(emitted)
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
    action: ConnectionAction,
}

#[derive(Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "snapshots", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "snapshots", serde(deny_unknown_fields))]
struct Gate {
    count: Option<u64>,
    random: Option<u64>,
}

impl Gate {
    fn forward(&mut self, connection: &Connection, seed: u64, identity: &str) -> bool {
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
        let state = self
            .random
            .get_or_insert_with(|| seed ^ stream_key(0, identity));
        random_word(state) >> 11 < threshold
    }
}
