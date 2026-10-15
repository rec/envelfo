//! Self-contained, versioned JSON continuation, enabled by the `snapshots` feature.
//!
//! Snapshots contain normalized definitions, not a host's clocks or delivery queues.
//! Supply the same clock mapping when resuming a Patch. This is a Rust snapshot
//! format, not an interchange format for Python's runtime snapshots.
//!
//! ```
//! use envelfo_core::{lfo::LFO, snapshot::Snapshot, time::Time};
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let definition = LFO::new(Time::ONE)?;
//! let state = definition.initial(Time::ZERO)?;
//! let json = Snapshot::LFO { definition, state }.to_json()?;
//! let Snapshot::LFO { definition, state } = Snapshot::from_json(&json)? else {
//!     unreachable!();
//! };
//! assert_eq!(definition.value(&state, Time::new(1, 4)?)?.value, 1.0);
//! # Ok(())
//! # }
//! ```

use crate::envelope::{Envelope, EnvelopeState};
use crate::lfo::{LFO, LFOState};
use crate::patch::{Patch, PatchState};
use crate::sample_hold::{SampleHold, SampleState};
use crate::stages::{StageState, Stages};

/// A definition and its complete continuation state. Validation happens on save
/// and load; deserialization of the opaque playback states is not public.
#[derive(Clone, Debug, PartialEq)]
pub enum Snapshot {
    LFO {
        definition: LFO,
        state: LFOState,
    },
    Envelope {
        definition: Envelope,
        state: EnvelopeState,
    },
    Stages {
        definition: Stages,
        state: StageState,
    },
    SampleHold {
        definition: SampleHold,
        state: SampleState,
    },
    Patch {
        definition: Patch,
        state: PatchState,
    },
}

#[derive(Debug)]
pub enum Error {
    Json(serde_json::Error),
    Version(u32),
    Invalid(crate::Error),
}

impl Snapshot {
    /// Save version 1 JSON. Rational times are strings; counters and random words
    /// are exact JSON integers. Consumers must not round integers through f64.
    pub fn to_json(&self) -> Result<String, Error> {
        self.validate()?;
        serde_json::to_string(&Document {
            version: 1,
            snapshot: self.clone(),
        })
        .map_err(Error::Json)
    }

    /// Load a complete definition and state without advancing time or drawing
    /// random values. Unknown versions and invalid definitions/states are errors.
    pub fn from_json(json: &str) -> Result<Self, Error> {
        let document: Document = serde_json::from_str(json).map_err(Error::Json)?;
        if document.version != 1 {
            return Err(Error::Version(document.version));
        }
        document.snapshot.validate()?;
        Ok(document.snapshot)
    }

    fn validate(&self) -> Result<(), Error> {
        match self {
            Self::LFO { definition, state } => crate::lfo::snapshot::validate(definition, state),
            Self::Envelope { definition, state } => {
                crate::envelope::snapshot::validate(definition, state)
            }
            Self::Stages { definition, state } => {
                crate::stages::snapshot::validate(definition, state)
            }
            Self::SampleHold { definition, state } => {
                crate::sample_hold::snapshot::validate(definition, state)
            }
            Self::Patch { definition, state } => {
                crate::patch::snapshot::validate(definition, state)
            }
        }
        .map_err(Error::Invalid)
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Json(error) => write!(formatter, "invalid snapshot JSON: {error}"),
            Self::Version(version) => write!(formatter, "unsupported snapshot version: {version}"),
            Self::Invalid(error) => write!(formatter, "invalid snapshot: {error}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Json(error) => Some(error),
            Self::Invalid(error) => Some(error),
            Self::Version(_) => None,
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u32,
    #[serde(with = "Wire")]
    snapshot: Snapshot,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(remote = "Snapshot", tag = "kind", deny_unknown_fields)]
#[allow(clippy::upper_case_acronyms)] // Remote variants must match the public API.
enum Wire {
    LFO {
        definition: LFO,
        #[serde(with = "crate::lfo::snapshot::State")]
        state: LFOState,
    },
    Envelope {
        definition: Envelope,
        #[serde(with = "crate::envelope::snapshot::State")]
        state: EnvelopeState,
    },
    Stages {
        definition: Stages,
        #[serde(with = "crate::stages::snapshot::State")]
        state: StageState,
    },
    SampleHold {
        definition: SampleHold,
        #[serde(with = "crate::sample_hold::snapshot::State")]
        state: SampleState,
    },
    Patch {
        definition: Patch,
        #[serde(with = "crate::patch::snapshot::State")]
        state: PatchState,
    },
}
