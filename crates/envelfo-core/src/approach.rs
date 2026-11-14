//! Captured exponential stages resolved to the existing interpolation contract.

use crate::Error;
use crate::envelope::Segment;
use crate::time::Time;

#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "snapshots", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "snapshots", serde(deny_unknown_fields))]
pub struct ExponentialApproach {
    /// Host clock units per second; rounding precedes conversion to that clock.
    pub clock_units_per_second: Time,
    pub asymptote: f64,
    pub time_constant: f64,
    #[cfg_attr(feature = "snapshots", serde(default))]
    pub minimum_time_constant: f64,
    pub threshold: f64,
    pub transition_to: f64,
    pub transition_seconds: f64,
    pub minimum_transition_distance: f64,
}

impl ExponentialApproach {
    pub fn validate(&self) -> Result<(), Error> {
        if [
            self.asymptote,
            self.time_constant,
            self.minimum_time_constant,
            self.threshold,
            self.transition_to,
            self.transition_seconds,
            self.minimum_transition_distance,
        ]
        .iter()
        .any(|v| !v.is_finite())
            || self.threshold == self.asymptote
            || self.time_constant < 0.0
            || self.minimum_time_constant < 0.0
            || self.transition_seconds < 0.0
            || self.minimum_transition_distance < 0.0
            || self.clock_units_per_second <= Time::ZERO
        {
            return Err(Error::InvalidDefinition("invalid exponential approach"));
        }
        Ok(())
    }

    pub fn resolve(&self, initial: f64) -> Result<Vec<Segment>, Error> {
        let mut result = Vec::with_capacity(2);
        self.resolve_into(initial, &mut result)?;
        Ok(result)
    }

    fn resolve_into(&self, initial: f64, result: &mut Vec<Segment>) -> Result<(), Error> {
        self.validate()?;
        if !initial.is_finite() {
            return Err(Error::InvalidDefinition("nonfinite captured level"));
        }
        if result.capacity() - result.len() < 2 {
            return Err(Error::Capacity);
        }
        let mut start = initial;
        let distance = self.threshold - self.asymptote;
        if self.time_constant > 0.0 && (initial - self.threshold) * distance > 0.0 {
            let curve = (distance / (initial - self.asymptote)).ln();
            result.push(Segment {
                duration: Some(
                    seconds(-self.time_constant.max(self.minimum_time_constant) * curve)?
                        .checked_mul(self.clock_units_per_second)?,
                ),
                to: self.threshold,
                curve,
                approach: None,
            });
            start = self.threshold;
        }
        let remaining = (start - self.transition_to).abs();
        let transition = if remaining == 0.0 {
            0.0
        } else {
            self.transition_seconds * remaining / remaining.max(self.minimum_transition_distance)
        };
        result.push(Segment {
            duration: Some(seconds(transition)?.checked_mul(self.clock_units_per_second)?),
            to: self.transition_to,
            curve: 0.0,
            approach: None,
        });
        Ok(())
    }
}

pub fn resolve_segments(parts: &[Segment], initial: f64) -> Result<Vec<Segment>, Error> {
    let mut result = Vec::with_capacity(resolved_capacity(parts)?);
    resolve_segments_into(parts, initial, &mut result)?;
    Ok(result)
}

pub(crate) fn resolved_capacity(parts: &[Segment]) -> Result<usize, Error> {
    parts.iter().try_fold(0_usize, |n, p| {
        n.checked_add(if p.approach.is_some() { 2 } else { 1 })
            .ok_or(Error::Overflow)
    })
}

pub(crate) fn resolve_segments_into(
    parts: &[Segment],
    initial: f64,
    result: &mut Vec<Segment>,
) -> Result<(), Error> {
    if result.capacity() < resolved_capacity(parts)? {
        return Err(Error::Capacity);
    }
    result.clear();
    let mut entry = initial;
    for part in parts {
        if let Some(approach) = &part.approach {
            approach.resolve_into(entry, result)?;
            entry = approach.transition_to;
        } else {
            result.push(part.clone());
            entry = part.to;
        }
    }
    Ok(())
}

fn seconds(value: f64) -> Result<Time, Error> {
    let nanos = (value * 1_000_000_000.0).round_ties_even();
    if !nanos.is_finite() || nanos < 0.0 || nanos >= i64::MAX as f64 {
        return Err(Error::Overflow);
    }
    Time::new(nanos as i64, 1_000_000_000)
}
