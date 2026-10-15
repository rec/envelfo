# Rust performance and allocation audit

Measured on macOS arm64 with Homebrew Rust 1.99.0. These are local steady-state
microbenchmarks, not portable latency promises or allocation-free callback claims.

## Reproduction

```sh
cargo bench -p envelfo-core --bench patch --locked
```

The dependency-free harness uses 256 warmup operations, then five timed runs of
4000 operations. It reports median/min/max nanoseconds per operation. Allocations
are counted in a separate run, including reallocations and requested bytes, not
peak live memory. Preparation and warmup are excluded. Returned results and state
clones are consumed with `black_box` and dropped inside the measured operation.
The counting System allocator is confined to the benchmark executable; the
library retains `forbid(unsafe_code)`.

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

## Additional work beyond the prompt

None.
