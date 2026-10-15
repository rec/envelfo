# Rust performance and allocation audit

Measured on macOS arm64 with Homebrew Rust 1.99.0. These are local steady-state
microbenchmarks, not portable latency promises or allocation-free callback claims.

## Reproduction

```sh
cargo bench -p envelfo-core --bench patch --locked
```

The dependency-free harness uses 256 warmup operations, then five timed runs of
4000 operations. It reports median/min/max of the five per-run averages in
nanoseconds per operation, not individual-call tail latency. Allocations
are counted in a separate run, including reallocations and requested bytes, not
peak live memory. Preparation and warmup are excluded. Returned results and state
clones are consumed with `black_box` and dropped inside the measured operation.
The counting System allocator is confined to the benchmark executable; the
library retains `forbid(unsafe_code)`.
The inactive counting check remains in the allocator during timed runs, equally
for baseline and optimized measurements. Startup, snapshot JSON and host IO are
not measured. macOS denied native sampler attachment, so this audit uses paired
workload and clone-only measurements rather than claiming a sampled stack profile.

Patch workloads observe at 1024 Hz. The arpeg cases use two children and two
outputs, with or without an explicit Envelope start before every observation.
The signal case has 64 LFOs, 64 Slews and 64 outputs. The staged case has 16
independent marked LFO Stages. Fanout cases route a 64 Hz marker to 128 SampleHolds
with 1/16-second delays and deterministic or 50% chance gates. Clone-only cases
use separately warmed state, including queued deliveries, without advancing it.

## Baseline

Unmodified runtime at `3f7f060`, measured twice; this table records the second
run's five-sample medians. Timing variation is visible in the harness's ranges.

| Workload | ns/op | Allocation calls/op | Requested bytes/op | Clone-only ns/op |
| --- | ---: | ---: | ---: | ---: |
| Scalar LFO | 292 | 0 | 0 | n/a |
| arpeg observation | 1220 | 12 | 1275 | 224 |
| arpeg start + observation | 1764 | 17 | 1728 | 230 |
| Signals, 128 nodes | 43857 | 224 | 41991 | 815 |
| Stages, 16 nodes | 17588 | 44.18 | 6748.65 | 277 |
| Fanout, 128 connections | 29425 | 199.75 | 76463.19 | 2468 |
| Chance fanout, 128 connections | 24873 | 199.66 | 59645.13 | 1658 |

Cloning is not the dominant CPU cost in these dense cases. The runtime also
copies graph names into temporary signal maps and constructs connection stream
identities before determining whether they are needed. Optimization starts there;
transactional clones, exact rational times and event-capacity policies stay intact.

## Optimized results

The unchanged harness was rerun after optimization. This table records the final
repeat's five-sample medians; an earlier run showed transient timing outliers,
so these are reproducible local observations, not platform-independent guarantees.
Allocation counts and requested bytes were stable between runs.

| Workload | Before ns/op | After ns/op | Before calls/op | After calls/op | After bytes/op |
| --- | ---: | ---: | ---: | ---: | ---: |
| Scalar LFO | 292 | 288 | 0 | 0 | 0 |
| arpeg observation | 1220 | 1001 | 12 | 10 | 1177 |
| arpeg start + observation | 1764 | 1574 | 17 | 15 | 1630 |
| Signals, 128 nodes | 43857 | 36514 | 224 | 96 | 39443 |
| Stages, 16 nodes | 17588 | 14358 | 44.18 | 28.18 | 6366.65 |
| Fanout, 128 connections | 29425 | 19896 | 199.75 | 30.75 | 72892.81 |
| Chance fanout, 128 connections | 24873 | 15206 | 199.66 | 30.66 | 56074.75 |

Dense routing takes 32–39% less elapsed time and makes approximately 85% fewer
allocation calls. The signal graph makes 57% fewer allocation calls and takes
17% less time. Small arpeg and staged workloads also improve; scalar LFO timing
is unchanged within noise. Requested bytes fall less dramatically than allocation
counts because transactional state/queue storage is retained.

Changes are limited to:

- Borrow graph-owned names in the temporary signal map. The public owned-map
  `Operator::observe` API and the internal borrowed map use one evaluator.
- Construct a connection's stream identity only when initializing its random
  stream. Gated-out, deterministic, pre-keyed and already-seeded routes avoid
  that string construction, without changing any stream key or draw.
- Return unchanged position state when elapsed time is exactly zero. This skips
  redundant rational arithmetic in repeated staged observations, not nonzero
  advancement or overflow checks.

No public API, snapshot wire layout, capacity, event ordering, output ownership
or default dependency changed. Advance/command/settle still clone and commit state
only after success; no rollback journal or persistent scratch cache was added.
This remains an allocating API, not a hard-real-time callback interface.

## Correctness checks

The Rust suites run with snapshots disabled and enabled, in debug and release,
alongside formatting and strict Clippy checks. Shared body, host-clock, graph and
random traces remain unchanged, including JSON continuation with pending events.
Existing failure tests check state equality after arithmetic overflow,
host-clock failures, event-order errors and both capacity limits. A new late-failure
regression first executes chance routing and a random sample delivery, then fails
on a missing host input or overflowing signal after a Slew update. The entire state
must remain equal to its saved copy, and a corrected retry must match a clean run.
A focused Stage regression also verifies that repeated observations at
an already-consumed exact boundary do not replay events or mutate state, and
that backwards time still errors without mutation.

Only the standalone core changed. Consumer dependency pins were not refreshed,
and Linux/Windows testing remains a release gate rather than a per-commit action.

## Additional work beyond the prompt

None.
