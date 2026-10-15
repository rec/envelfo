//! Dimensionless signal operators evaluated on an explicit host observation grid.

use crate::Error;
use crate::time::Time;
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "snapshots", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "snapshots", serde(deny_unknown_fields))]
pub enum Operator {
    Sum(Vec<String>),
    Product(Vec<String>),
    Affine {
        input: String,
        scale: f64,
        offset: f64,
    },
    Quantize {
        input: String,
        step: f64,
        origin: f64,
    },
    Latch(String),
    Threshold {
        input: String,
        lower: f64,
        upper: f64,
    },
    Slew {
        input: String,
        rise: f64,
        fall: f64,
        initial: Option<f64>,
    },
}

impl Operator {
    pub fn inputs(&self) -> &[String] {
        match self {
            Self::Sum(inputs) | Self::Product(inputs) => inputs,
            Self::Affine { input, .. }
            | Self::Quantize { input, .. }
            | Self::Latch(input)
            | Self::Threshold { input, .. }
            | Self::Slew { input, .. } => std::slice::from_ref(input),
        }
    }

    pub fn validate(&self) -> Result<(), Error> {
        let valid = match self {
            Self::Sum(v) | Self::Product(v) => v.len() >= 2,
            Self::Affine { scale, offset, .. } => scale.is_finite() && offset.is_finite(),
            Self::Quantize { step, origin, .. } => {
                step.is_finite() && *step > 0.0 && origin.is_finite()
            }
            Self::Threshold { lower, upper, .. } => {
                lower.is_finite() && upper.is_finite() && lower < upper
            }
            Self::Slew {
                rise,
                fall,
                initial,
                ..
            } => {
                rise.is_finite()
                    && fall.is_finite()
                    && *rise >= 0.0
                    && *fall >= 0.0
                    && initial.is_none_or(f64::is_finite)
            }
            Self::Latch(_) => true,
        };
        if !valid || self.inputs().iter().any(String::is_empty) {
            return Err(Error::InvalidDefinition("invalid Patch signal operator"));
        }
        Ok(())
    }

    /// Retained operators return their new value and an optional Threshold edge.
    pub fn observe(
        &self,
        signals: &BTreeMap<String, f64>,
        previous: Option<f64>,
        seconds: Time,
    ) -> Result<(f64, Option<&'static str>), Error> {
        if seconds < Time::ZERO {
            return Err(Error::TimeReversed);
        }
        let input = |name: &str| {
            signals
                .get(name)
                .copied()
                .ok_or(Error::InvalidDefinition("missing Patch signal input"))
        };
        let mut edge = None;
        let value = match self {
            Self::Sum(inputs) => inputs
                .iter()
                .try_fold(0.0, |v, n| Ok::<_, Error>(v + input(n)?))?,
            Self::Product(inputs) => inputs
                .iter()
                .try_fold(1.0, |v, n| Ok::<_, Error>(v * input(n)?))?,
            Self::Affine {
                input: name,
                scale,
                offset,
            } => offset + scale * input(name)?,
            Self::Quantize {
                input: name,
                step,
                origin,
            } => quantize(input(name)?, *step, *origin)?,
            Self::Latch(name) => previous.unwrap_or(input(name)?),
            Self::Slew {
                input: name,
                rise,
                fall,
                initial,
            } => {
                let target = input(name)?;
                match previous {
                    None => initial.unwrap_or(target),
                    Some(previous) if target >= previous => {
                        target.min(previous + rise * seconds.as_f64())
                    }
                    Some(previous) => target.max(previous - fall * seconds.as_f64()),
                }
            }
            Self::Threshold {
                input: name,
                lower,
                upper,
            } => {
                let target = input(name)?;
                match previous {
                    None => f64::from(target >= *upper),
                    Some(0.0) if target >= *upper => {
                        edge = Some("rising");
                        1.0
                    }
                    Some(1.0) if target <= *lower => {
                        edge = Some("falling");
                        0.0
                    }
                    Some(previous) => previous,
                }
            }
        };
        if !value.is_finite() {
            return Err(Error::InvalidSignal);
        }
        Ok((value, edge))
    }

    pub(crate) fn retained(&self) -> bool {
        matches!(
            self,
            Self::Latch(_) | Self::Threshold { .. } | Self::Slew { .. }
        )
    }

    pub(crate) fn range(&self, ranges: &BTreeMap<String, (f64, f64)>) -> Result<(f64, f64), Error> {
        let range = match self {
            Self::Sum(inputs) => inputs.iter().fold((0.0, 0.0), |(lo, hi), n| {
                (lo + ranges[n].0, hi + ranges[n].1)
            }),
            Self::Product(inputs) => inputs.iter().fold((1.0, 1.0), |(lo, hi), n| {
                let (a, b) = ranges[n];
                let values = [lo * a, lo * b, hi * a, hi * b];
                (
                    values.into_iter().fold(f64::INFINITY, f64::min),
                    values.into_iter().fold(f64::NEG_INFINITY, f64::max),
                )
            }),
            Self::Affine {
                input,
                scale,
                offset,
            } => {
                let (lo, hi) = ranges[input];
                let a = offset + scale * lo;
                let b = offset + scale * hi;
                (a.min(b), a.max(b))
            }
            Self::Quantize {
                input,
                step,
                origin,
            } => {
                let (lo, hi) = ranges[input];
                (quantize(lo, *step, *origin)?, quantize(hi, *step, *origin)?)
            }
            Self::Latch(input) => ranges[input],
            Self::Threshold { .. } => (0.0, 1.0),
            Self::Slew { input, initial, .. } => {
                let (lo, hi) = ranges[input];
                if let Some(initial) = initial {
                    (lo.min(*initial), hi.max(*initial))
                } else {
                    (lo, hi)
                }
            }
        };
        if !range.0.is_finite() || !range.1.is_finite() {
            return Err(Error::InvalidSignal);
        }
        Ok(range)
    }
}

fn quantize(value: f64, step: f64, origin: f64) -> Result<f64, Error> {
    let position = (value - origin) / step;
    if !position.is_finite() {
        return Err(Error::InvalidSignal);
    }
    let lower = position.floor();
    let value = origin + (lower + f64::from(position - lower >= 0.5)) * step;
    if !value.is_finite() {
        return Err(Error::InvalidSignal);
    }
    Ok(value)
}
