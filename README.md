# enveLFO

Independent Python and Rust control libraries for envelopes, LFOs, Stages,
composition and events. Extraction from uFor and enge is in progress.

uFor owns the portable definitions. enveLFO owns their execution. The Python
library requires neither NumPy nor an audio engine. Rust extraction is a
subsequent milestone; no standalone Rust library is available yet.

## Python

Install from this repository with `uv sync`. The reference library uses uFor's
Pydantic definitions and reccy's unit normalization; it has no NumPy, enge,
audio-device, or compiled-extension dependency.

```python
from fractions import Fraction

from envelfo.runtime import envelfo_at, initial_envelfo
from ufor.envelfo import Cycle, EnvelfoUse

definition = EnvelfoUse(body=Cycle(rate=Fraction(1)))
state = initial_envelfo(definition, Fraction(0))
value = envelfo_at(definition, state, Fraction(1, 4))
assert value.value == 1.0
```

Execution functions live in `envelfo.runtime`; authored definitions, typed
commands, values, and snapshot records remain in `ufor.envelfo`. Use
`envelfo_event` for timestamped Cycle/Contour/SampleHold commands and
`advance_envelfo` for Stages, retaining its returned state and ordered events.
`next_stage_event` exposes the next natural staged boundary. Snapshot records
support Pydantic JSON serialization and restoration without native bindings.

Times are exact `Fraction` positions in the definition's clock. The host supplies
seconds or beats and transport observations; the library does not acquire time.
It also exposes the existing Latch, Slew, Threshold, and transform helpers.
Portable random words live in `envelfo.random`; stream identities remain in
`ufor.envelfo_random` because document traces also use them.

## Standalone Patches

`envelfo.patch.initial_patch` accepts a materialized uFor `EnvelfoUse` whose body
is a Patch. `advance_patch` processes exact natural boundaries and pending
connections, then observes the named outputs once. Supply `at` and
`next_delivery` as exact host seconds, with `next_delivery > at`. There is no
implicit sample rate or observation grid.

```python
from envelfo.patch import advance_patch, initial_patch
from ufor.envelfo import Affine, Patch

definition = EnvelfoUse(
    body=Patch(
        envelfos={
            'wave': Cycle(rate=Fraction(1)),
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
remain enge adapter responsibilities. No standalone Rust library is available
yet.

## Verification

Run `uv run pytest` for the reference tests and the language-neutral cases in
`conformance/envelfo-runtime.json` and `conformance/envelfo-patch.json`. These
preserve exact event ordering and
random words, with explicit tolerances for floating-point signal values.
Linux, Windows, and macOS checks are release gates, not per-commit jobs.
