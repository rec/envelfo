# enveLFO

Independent Python and Rust control libraries for envelopes, LFOs, Stages,
composition and events. Extraction from uFor and enge is in progress.

uFor owns the portable definitions. enveLFO owns their execution. The Python
library requires neither NumPy nor an audio engine. The Rust core now
supports exact LFO, Envelope, Stages and Patch execution, including signal
operators, connection scheduling and portable random streams. Consumer adapter
integration is still in progress.

The periodic Envelfo body is `LFO`, with discriminator `kind = "lfo"` and state
type `LFOState`. This replaces `Cycle` without compatibility aliases. Rust uses
`envelfo_core::lfo`. The `cycle` event port still names a completed loop traversal;
it is not a body type. Oscillator duty cycles and graph dependency cycles also
keep their existing meaning.

The envelope-like Envelfo body is `ufor.envelfo.Envelope`, with `kind = "envelope"`
and state type `EnvelopeState`. It replaces `Contour` without aliases. Rust uses
`envelfo_core::envelope`. uFor's lower-level `ufor.envelope.VoiceEnvelope` is a
distinct renderer envelope, with related `VoiceEnvelopeEvent`,
`VoiceEnvelopeState`, and `VoiceEnvelopeValue` types. There are no old-name aliases.

## Python

Install from this repository with `uv sync`. The reference library uses uFor's
Pydantic definitions and reccy's unit normalization; it has no NumPy, enge,
audio-device, or compiled-extension dependency.

```python
from fractions import Fraction

from envelfo.runtime import envelfo_at, initial_envelfo
from ufor.envelfo import LFO, EnveLFO

definition = EnveLFO(body=LFO(rate=Fraction(1)))
state = initial_envelfo(definition, Fraction(0))
value = envelfo_at(definition, state, Fraction(1, 4))
assert value.value == 1.0
```

Execution functions live in `envelfo.runtime`; authored definitions, typed
commands, values, and snapshot records remain in `ufor.envelfo`. Use
`envelfo_event` for timestamped LFO/Envelope/SampleHold commands and
`advance_envelfo` for Stages, retaining its returned state and ordered events.
`next_stage_event` exposes the next natural staged boundary. Snapshot records
support Pydantic JSON serialization and restoration without native bindings.

Times are exact `Fraction` positions in the definition's clock. The host supplies
seconds or beats and transport observations; the library does not acquire time.
It also exposes the existing Latch, Slew, Threshold, and transform helpers.
Portable random words live in `envelfo.random`; stream identities remain in
`ufor.envelfo_random` because document traces also use them.

## Standalone Patches

`envelfo.patch.initial_patch` accepts a materialized uFor `EnveLFO` whose body
is a Patch. `advance_patch` processes exact natural boundaries and pending
connections, then observes the named outputs once. Supply `at` and
`next_delivery` as exact host seconds, with `next_delivery > at`. There is no
implicit sample rate or observation grid.

```python
from envelfo.patch import advance_patch, initial_patch
from ufor.envelfo import Affine, Patch

definition = EnveLFO(
    body=Patch(
        envelfos={
            'wave': LFO(rate=Fraction(1)),
            'level': Affine(input='wave', scale=0.5, offset=0.5),
        },
        outputs={'expression': 'level'},
    )
)
state = initial_patch(definition, Fraction(0), name='expression')
result = advance_patch(
    definition,
    state,
    Fraction(1, 4),
    next_delivery=Fraction(1, 2),
)
assert result.values['expression'] == 1.0
state = result.state
```

`patch_event` accepts a child name and an existing `EnvelfoEvent` in host seconds.
It returns state and exported events without adding an observation. Initializing
a Patch does not manufacture note-on commands; the host delivers lifecycle
commands to the appropriate children. A command precedes obsolete same-time
natural completion, as in scalar Stages.

Natural events retain exact boundary times. Threshold initialization is silent;
edges export at `next_delivery` and connection delays start there. Delays always
use seconds. Latch captures read the first host observation after delivery, and
Slew rates use elapsed host seconds, even while a beat clock is stopped. Such
operators require the same observation schedule for equivalent continuation;
adding observations is not merely splitting a processing span.

For beat-clock Patches, supply the host's `TempoMap` to initialization, commands,
and advancement. Local running beats exclude transport jumps. Resupply the same
accepted map after restoring `PatchState.model_validate_json(...)`; snapshots
contain child state, retained values, counters, random streams, queued deliveries,
and the previous observation time, not an independently acquired clock.

Connections execute in declaration order at a shared time. Division is first
aligned with explicit offset; chance draws only on eligible events and not for
probabilities zero or one. The host supplies the instance name and unsigned
64-bit seed. SampleHold streams use `patch-{name}-{child}:sample-hold`; connection
streams use target identity and declaration index, with the existing capture,
cue and start suffixes. enge supplies its existing flattened host identities to
the same gate function, so audio snapshots and seeded results remain unchanged.
Standalone and audio-host traces must use matching identities when comparing
chance streams, rather than assuming a global host connection index.

The pending queue and emitted/delivered event work per advancement are bounded
at 4096. Capacity errors are explicit and leave the supplied snapshot unchanged.
NumPy block acceleration, voice ownership, and external cross-instance routing
remain enge adapter responsibilities.

## Standalone Rust core

`crates/envelfo-core` is a normal Rust library, with no Python, PyO3, NumPy or
audio dependency. Build and test it directly with `cargo test --workspace`;
also check optimized overflow behavior with `cargo test --workspace --release`.
`cargo clippy --workspace --all-targets -- -D warnings` checks the Rust source.
The tested API example lives in the crate's documentation.

`LFO::new(rate)` uses the existing LFO defaults. Pass exact positions in
the LFO's declared seconds/beat clock; a transport-position LFO receives
song beats directly. Hosts normalize authored quantities and supply clocks.
The core does not parse Pint quantities, acquire tempo or interpret frame counts.
Commands preserve phase, activation age, pause/reverse and reset behavior;
failed commands leave the supplied state untouched. `LFOState::clone` provides
continuation snapshots. A serialized Rust snapshot format is not provided yet.

`Time` holds a reduced signed i64 numerator and positive i64 denominator. It
accepts integer or rational literals, not floating-point or unit expressions.
Construction and arithmetic return explicit errors for zero denominators,
division by zero and values outside the normalized 64-bit component range.
Intermediate products use i128 and reduce before checking the final components;
the API never exposes unchecked arithmetic operators. Ordering and phase wraps
stay exact, even when adjacent positions round to the same floating-point value.
Conversions to and from arpeg's `num_rational::Ratio<i64>` are provided; incoming
ratios are checked before normalization. Signal observations use floating point.
The runtime dependency disables num-rational's default BigInt feature.

`Envelope::new(initial, segments)` supplies ordinary attack defaults. Segment
durations are exact times in the host's chosen seconds or beat clock; target
levels and curve shapes are floating point. The host resolves named loop bounds
to marker positions. Release starts at the captured current level, and retrigger,
pause/reverse, seek/shift, looping and finite repeats follow the Python contract.
Zero-duration segments apply immediately. Nonzero curves are supported here,
unlike enge's existing persistent native preparation.

`Stages::initial(at)` validates normalized Stage bodies and transitions.
`Stages::advance(state, at, event)` processes natural boundaries and returns a
scalar value plus ordered output events. `Stages::value` only observes state;
it does not run transitions. `Stages::next_event` inspects the next boundary
without advancing. Commands at a boundary preempt obsolete natural completion;
markers precede loop boundaries, and finite repeats emit the boundary then
`stage.done`. A finish transition emits `done`. LFO markers are stored on their
Stage; Envelope markers are part of the Envelope definition. Envelope/Stage states
can be cloned for continuation, and failed advances leave the supplied state
unchanged. One advance is limited to 4096 emitted events, with an explicit
capacity error rather than dropped events. Public Rust definition fields are
normalized host inputs, not an authored-document/Pint parser; initialize state
again after changing a definition.

`Patch::initial(at, name, seed, clock)` validates the materialized graph, signal
dependencies, event ports, output names, action targets and finite signal ranges.
Child, connection and event-output vectors preserve declaration order. Signal
ordering matches Python's first-encountered dependency order. Nodes include
LFOs with markers, Envelopes, Stages, SampleHold, and Sum/Product/Affine/Quantize/
Latch/Threshold/Slew operators. Signals remain dimensionless and are not clipped.

`Patch::advance(state, at, next_delivery, clock)` processes exact natural events
and queued commands, then observes the graph once. Times `at`, `next_delivery`
and connection delays are host seconds. `Seconds` supplies the identity clock;
beat-clock hosts implement `ClockMapping` with monotonic elapsed beats and their
inverse host times, not seekable song positions. No tempo-map implementation,
MIDI clock acquisition, implicit observation grid or audio rate is imposed.
Slew always measures host seconds, including when the beat clock is stopped.
Threshold initializes silently; later edges are timestamped at `next_delivery`,
and any connection delay is added to that time. Capture clears Latch state at
delivery so the next observation captures the current input.

`Patch::command` delivers an addressed child command without adding a signal
observation. Same-time external commands can preempt Stage completion. Natural
events are routed before pending deliveries; connections run in declaration
order and zero-delay cascades are processed at the same exact host time.
Every/offset gates are first-aligned; probability zero/one consumes no random
word. `random::stream_key` preserves uFor's SHA-256 identity bytes. The core uses
RustCrypto's SHA-256 implementation, with no Python or audio dependency.

Patch state can be cloned for independent continuation. Failed calls roll back
all child state, retained observations, gates, random draws and pending events.
Pending queues and processed events retain their separate 4096 limits. This
allocating API does not promise an allocation-free real-time callback. Public
normalized definitions must remain unchanged while using initialized state.

All ten body traces, four Patch traces and portable random/identity vectors now
pass directly in both languages. Built-in host tempo maps, serialized snapshots,
optional Python bindings and consumer adapter replacement remain extraction work.
The existing enge native playback still has floating-point event positions;
using the extracted random-word kernel does not claim exact native timing.

## Verification

Run `uv run pytest` for the reference tests and the language-neutral cases in
`conformance/envelfo-runtime.json`, `conformance/envelfo-patch.json` and
`conformance/envelfo-streams.json`. These
preserve exact event ordering and
random words, with explicit tolerances for floating-point signal values.
Linux, Windows, and macOS checks are release gates, not per-commit jobs.
