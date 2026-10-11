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

Standalone Patch orchestration, delayed connections, and host observation/delivery
scheduling have not yet been extracted from enge. The current library does not
claim those capabilities or standalone Rust support.

## Verification

Run `uv run pytest` for the reference tests and the language-neutral cases in
`conformance/envelfo-runtime.json`. These preserve exact event ordering and
random words, with explicit tolerances for floating-point signal values.
Linux, Windows, and macOS checks are release gates, not per-commit jobs.
