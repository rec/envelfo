# Extraction closeout audit

Audit date: 2026-10-15. This records implementation ownership and verification,
not an allocation-free audio callback guarantee or a promise of additional APIs.

## Capability and evidence matrix

| Capability | Python owner | Rust owner | Verification |
| --- | --- | --- | --- |
| Flat LFO/Envelope/Stages/Patch definitions, references, Pint normalization | uFor | Normalized native definitions supplied by the host | uFor schema/codec/unit tests; Rust definition validation |
| LFO shapes, phase, rate, delay/fade, reset and playback commands | `envelfo.runtime` | `lfo` / `playback` | Shared runtime traces; `tests/lfo.rs`; Python scalar traces |
| Envelope attacks, captured release, curves, loops, repeats | `envelfo.runtime` | `envelope` | Shared runtime/host traces; `tests/envelope.rs` |
| Indefinite segments and explicit advance | `envelfo.runtime` | `envelope` / `stages` | Shared indefinite runtime traces; Python/Rust indefinite tests |
| Stages, marker ordering, cues, transitions and entry capture | `envelfo.runtime` | `stages` | Shared runtime traces; `tests/stages.rs`; Python partition tests |
| Named Patch signal/event outputs and graph ordering | `envelfo.patch` | `patch` | Shared Patch traces, including dependency order |
| Sum/Product/Affine/Quantize/Latch/Threshold/Slew | `envelfo.patch` / `runtime` | `signal` / `patch` | Shared Patch/host traces; focused operator tests in both languages |
| SampleHold, portable words and stream identities | `envelfo.random` / `runtime`; identity bytes in uFor | `random` / `sample_hold` | Shared word/SHA-256 vectors and host sample trace; independent draw/gate tests |
| Event division, offset, chance, cascades, delays and capacities | `envelfo.patch` | `patch` | Shared host division/offset/delivery traces; focused random/cascade/capacity rollback tests |
| Per-child clocks, stop/seek observations, direct song position | Host `TempoMap` supplied to `patch` | Host `ClockMapping` supplied to `patch` | One shared host trace across stops, tempo changes and forward/backward song seeks |
| Same-time commands and explicit observation/delivery phases | `patch_event` / `settle_patch` | `Patch::command` / `settle` | Shared addressed command/release trace; preemption and boundary tests |
| Continuation with retained state and pending events | Pydantic JSON snapshots | State clones; optional versioned JSON | Shared traces restore JSON at every step with `snapshots`; exact random, pending-queue and malformed-state tests |
| Audio integration | enge sampling, routes, scope and voice lifecycle | enge adapters over core state | Full enge suite and native/Python Patch regressions |
| Expressive arpeg integration | Optional `arpeg.envelfo` bridge | Separate `arpeg-envelfo` crate | Shared arpeg composition wire trace; host clock and pending-delivery continuation tests |

The core's shared fixtures are `envelfo-runtime.json`, `envelfo-patch.json`,
`envelfo-host.json` and `envelfo-streams.json`. Expected timestamps, pending
deliveries, event order and random words are exact. Floating signals have
absolute tolerance `1e-12`. Host expectations are handwritten from elapsed-time,
waveform and ordering rules, not generated from either implementation.
The fixture Rust host is test-only; the core does not acquire or estimate tempo.
Legacy invalid-document fixtures now use the canonical score discriminator and
assert a body validation error, rather than passing because an obsolete score
kind was rejected before its generator was checked.

## Ownership and duplicate-path review

- uFor contains authored definitions, validation, portable state records,
  stream identities, and stateless mathematics. It does not import enveLFO.
- The older `ufor.lfo` definition/state/event/value classes and executable
  initializer/query/command functions have been removed, without aliases.
  Lighting uses `ufor.envelfo.LFO` with `shape`, resolved rates, seconds,
  part scope and free reset. Its stateless score calculations share waveform
  and activation mathematics; they do not maintain a second playback state.
- enge block sampling accepts canonical `LFO` and `EnvelfoState`, without
  translating through a second LFO model or runtime. NumPy interpolation,
  vectorized spans, waveform kernels and sample-frame mapping remain audio
  acceleration, not alternate command/event schedulers.
- Native named Envelopes, LFOs, Stages and retained Patch scheduling use
  `envelfo-core`. Python Patch scheduling uses `envelfo.patch`. Audio hosts
  retain scope admission, trigger addresses, minimum hold and retirement.
- arpeg retains note/rhythm decisions and captured controller ownership. Its
  bridge links the libraries without making either event core depend on the other.

## Deliberate limits, not missing extraction

- Rust time has checked signed 64-bit rational components, i128 intermediates
  and explicit overflow errors. Python Fractions are not limited to that range.
- Rust accepts normalized definitions and host clock mappings, not Pint strings,
  authored document parsing or MIDI clock acquisition.
- Python snapshots serialize to JSON. Rust's optional `snapshots` feature bundles
  normalized definitions and complete state in versioned JSON, including Patch
  pending connections and RNG streams. Cross-language interchange is not provided.
  Hosts save clock mappings and external delivery queues themselves; combined
  enge/arpeg application snapshots remain host-owned.
- Stateful operators require the same observation schedule for equivalent
  continuation. Additional observations can legitimately change their output.
- Scalar LFO onset is a separate activation weight; staged LFO onset scales
  depth while preserving center. These existing contracts remain distinct.
- enge's linear-only preparation restrictions, voice-amplitude profile policy
  and event-free audio accelerators are host restrictions, not absent core curves.
- Standalone Python bindings to Rust remain optional and are not implemented.
- arpeg currently returns newly triggered expression after its note messages at
  the same timestamp. Revisit pre-note-on expression separately; the bridge does
  not guarantee it and does not introduce unbounded same-time feedback.

## Distribution and licenses

Python's runtime dependency is uFor, bringing Pydantic, reccy's Pint utilities
and their declared dependencies. Neither NumPy nor enge is required. The wheel
and source distribution build independently of enge and native bindings.
Dependency revisions come from `uv.lock` for reproducible repository installs;
publishing packages still requires their dependencies to be available.

The Rust runtime tree contains num-rational/num-integer/num-traits and RustCrypto
SHA-256 dependencies, without num-rational's default BigInt feature. It contains
no Python, PyO3, MIDI device host or audio engine. The crate packages and verifies
independently. Its packaged `LICENSE` carries the project's MIT notices.
The root and crate copies of that notice must stay identical.
Enabling `snapshots` adds optional Serde/serde_json dependencies; the default
runtime dependency tree is unchanged. JSON parsing enables exact float round trips.

The reviewed runtime manifests identify enveLFO, uFor and reccy as MIT;
Rust runtime dependencies are MIT or dual MIT/Apache-2.0. Retain dependency
notices when redistributing. This inventory is not a legal opinion or a
statement that an application's unrelated optional audio dependencies share
these licenses.

## Verification and release boundary

Local audit verification covers Python 3.13 and 3.15 on macOS, Rust debug/release
tests and strict lint, wheel/source-distribution builds, Cargo package verification,
and an installed-wheel test run outside the checkout with no NumPy or enge.
Consumer verification covers uFor, enge and arpeg. These automated tests do not
claim hardware, allocation-free callback, or physical MIDI timing validation.

Reproduce the source checks with:

```sh
uv sync --frozen
uv run --no-sync pytest
uv run --no-sync ruff check --select B,E,F,I envelfo test
uv run --no-sync ruff format --check envelfo test
uv run --no-sync ty check envelfo
cargo test --workspace --locked
cargo test --workspace --locked --release
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked --features snapshots
cargo test --workspace --locked --release --features snapshots
cargo clippy --workspace --all-targets --locked --features snapshots -- -D warnings
uv build
cargo package --manifest-path crates/envelfo-core/Cargo.toml --locked
cargo package --manifest-path crates/envelfo-core/Cargo.toml --locked --features snapshots
```

`.github/workflows/test.yml` runs only when a release is published. It gates
Linux, Windows and macOS, Python 3.13/3.15, tests against a wheel installed in a
fresh environment outside the checkout, and standalone Rust package verification.
Rust checks run with snapshots disabled and enabled, including debug/release
tests and independent package compilation. The enabled shared conformance runner
serializes and reloads every continuation step, not just explicit restore points.
The full repository tests retain access to the shared fixtures; the published
crate's build verification is independent of those repository-level test assets.
Cross-platform results are not claimed until that release workflow runs.
This audit does not create a tag, publish packages or trigger a release.

## Additional work beyond the prompt

None.
