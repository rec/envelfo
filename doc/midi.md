# MIDI output from named observations

The optional Python `envelfo.midi` module and Rust `envelfo-midi` crate map already
observed named values to meedee MIDI 1.0 messages. They do not acquire time,
advance a Patch, send MIDI, own connections, or start threads. Observation times
and delivery remain host responsibilities. Core enveLFO imports and the default
Rust workspace member remain free of meedee and native MIDI build requirements.

Install Python support with `uv sync --extra midi`. The optional Rust crate uses
meedee from HTTPS/main, requiring Rust 1.87, CMake and a C++20 compiler. Linux
source builds additionally require ALSA headers and pkg-config. For example,
Debian/Ubuntu uses `libasound2-dev` and `pkg-config`. The standalone core retains
Rust 1.85. No CLI or audio-device host is added.

## Mapping contract

Python mappings are frozen Pydantic models; Rust mappings are checked immutable
values. Each mapping names one output, an explicit increasing finite input range,
a MIDI target and optional integer destination bounds. Multiple targets can read
the same output. All mapped outputs must be present in each observation; other
outputs are ignored. Mappings execute in declaration order. Duplicate logical
destinations are rejected, including duplicate parameter IDs with different
resolutions. The host must also avoid conflicting writes through other mappings,
clients or applications; CC and fine-CC targets can overlap on the wire.

| Python target | Rust target | MIDI value range |
| --- | --- | --- |
| `CCTarget` | `Target::ControlChange` | 0–127 |
| `FineCCTarget` | `Target::ControlChange14` | 0–16383 |
| `RpnTarget` | `Target::Rpn` | 0–127 or 0–16383, explicit resolution 7 or 14 |
| `NrpnTarget` | `Target::Nrpn` | 0–127 or 0–16383, explicit resolution 7 or 14 |

Channels are 1–16. Ordinary controller IDs are 0–127; fine-CC base controllers
are 0–31. RPN parameter 16383 is reserved for null selection; NRPN 16383 remains
usable. RPN/NRPN Set commands select afresh and release with RPN null, using
meedee's existing ordered expansion. No interpretation of vendor NRPN meanings
is attempted. UMP and increment/decrement targets are outside this slice.

Destination `minimum` defaults to zero; `maximum=None` uses the target's full
range. Explicit bounds must satisfy `0 <= minimum <= maximum <= native maximum`.
A constant destination range is allowed. Input ranges must have a positive finite
width. Values outside those ranges, non-finite numbers and missing mapped outputs
fail before admitting any batch. No clamping occurs. Python also rejects boolean
and string observations. Scaling uses double-precision arithmetic; conversion to
MIDI integers uses nearest rounding with ties to even. Input endpoints reach the
configured integer endpoints exactly. Only identical quantized values are
suppressed, and only after the host confirms a successful send.

## Python host boundary

```python
from fractions import Fraction
from meedee import codecs, messages
from envelfo.midi import CCTarget, Mapper, Mapping

mapper = Mapper(
    mappings=[Mapping(
        output="expression",
        target=CCTarget(channel=1, controller=11),
        input_minimum=0.0,
        input_maximum=1.0,
    )],
    byte_rate=1000,
    burst_bytes=64,
)

# values may be result.values from advance_patch(), or from an existing bridge.
# at is host-supplied monotonic admission time in exact Fraction seconds.
batch = mapper.prepare(at=Fraction(1, 4), values={"expression": 0.5})
completed = False
try:
    for message in batch.messages:
        if isinstance(message, messages.ControlChange):
            connection.send(codecs.encode(message))  # caller-owned meedee connection
        else:
            connection.send_command(message)
    mapper.confirm(batch)
    completed = True
finally:
    if not completed:
        mapper.report_loss()
```

Serialize all calls to one mapper, and dedicate it to one output connection.
Pass the original batch to `confirm`. A batch with no changed messages still
requires confirmation, advancing the accepted admission time without byte cost.
Do not alter the batch's list of messages. Confirmation rejects an altered list,
a copied/foreign batch, or a stale batch. It does not prove instrument receipt:
a successful backend send means acceptance under meedee's transport contract.

Rust uses the same flow with `Mapper::prepare(at: Time, values: &BTreeMap<...>)`
and `Mapper::confirm(&batch)`. `batch.messages()` contains `MidiMessage` variants
wrapping meedee's immutable primitive/command values. Match those variants to
`OutputConnection::send` or `send_command`; keep each compound command intact.
`MidiMessage::expand()` exposes primitive order for device-free realization or
inspection. Time uses enveLFO's existing checked signed-64-bit rational type;
arithmetic overflow is an explicit error. Python uses exact `Fraction` seconds.
See the crate documentation for a compiled public API example.

## Admission and recovery

Both rate and burst capacity are required. The token bucket starts full and
refills in the supplied host admission clock. Admission time must not decrease.
The complete first batch must fit the configured burst. Accounting includes each
status byte and every expanded selector, data and RPN-null message; it does not
assume running-status compression. For example, a fine CC costs six bytes, a
7-bit parameter Set costs fifteen and a 14-bit Set costs eighteen.

The limit covers this mapper's admitted MIDI bytes, not unrelated traffic,
transport framing, driver buffers or a guaranteed hardware link rate. It is not a
scheduler: it does not sleep, select observation cadence, estimate backend drain
time or compensate for a host delivering an old batch late. Hosts must supply
admission times matching their actual send scheduling. Transport sends can still
block or fail after admission; select isolation and native deadlines at the host.

An over-budget observation is rejected before returning a batch, with no retained
or coalesced update. No old values are silently flushed later. Invalid input,
backward time and budget rejection leave mapper state unchanged. There is one
pending batch at most; another prepare is refused until that batch is confirmed
or the session is explicitly abandoned. Storage is proportional to configured
mappings plus that batch, not elapsed time or observation count. Caller-held
results and backend buffers are outside this bound. This is not an allocation-
free real-time audio callback interface.

On a partial or uncertain send, interruption, reconnect or known output loss,
call `report_loss()`. It invalidates suppression and abandons pending work without
sending cleanup or retrying. A compound-send error's known prefix belongs to the
host; the mapper never assumes the remaining bytes were delivered. There is no
rollback of a command that already affected an instrument.

Only after the host has recovered its output session should it call `restart()`.
The new session begins with a full budget and no suppression history; its first
observation sends every configured target. Old batches cannot confirm new work.
Restart sends no MIDI. Neither mapper runtime has a serialized continuation
contract. Save application/transport state explicitly if continuation is needed.

## Existing arpeg composition

Keep the existing `arpeg.envelfo.EnveLFOBridge` and Rust `arpeg-envelfo` bridge.
Their observations already expose named floating-point values in the shape this
mapper accepts. The mapper does not receive hits or re-observe the Patch.

Preserve the existing schedule, receive same-time hits, observe once, then
realize sequence described in [arpeg's bridge contract](../../arpeg/doc/envelfo.md).
The player's owned breath, bend and pressure lanes continue through its existing
realization path. Do not map those lanes a second time onto the same destination.
For additional named controls, the host merges the prepared messages into its
existing ordered delivery, keeping due releases and entry controls before fresh
NoteOn. Sending these controls after the note is not equivalent for instruments
that latch state at onset. This slice supplies the mapper; it does not modify
arpeg's host or introduce another scheduling/bridge path.

## Verification

Python and Rust consume the same hand-authored
[wire and lifecycle cases](../conformance/envelfo-midi.json), covering target
families, ordering, integer endpoints, ties to even, confirmed suppression,
exact refill, rejected work and explicit loss/restart. Additional unit checks
cover foreign/stale batches, malformed configuration and non-finite values.

```bash
uv run --frozen --extra midi pytest
CARGO_HOME="$PWD/target/cargo-home" cargo test -p envelfo-midi --locked
CARGO_HOME="$PWD/target/cargo-home" cargo test -p envelfo-midi --locked --release
```

For authorized macOS qualification, invoke this as a standalone command:

```bash
uv run --frozen --extra midi pytest -q -s -n 0 test/qualification/midi_output.py
```

Each case has a 45-second outer deadline. Both isolated and in-process cases
passed locally: exact ordered CC/fine-CC/RPN/NRPN delivery, confirmed suppression
and explicit send failure after caller-owned close. Endpoints are uniquely named
and test-owned. No physical devices, audio playback, tuning or reset messages are
used. Linux/Windows native delivery and physical-device qualification remain open.
Release CI adds device-free MIDI checks on Linux, Windows and macOS, without
changing the release-only workflow trigger.

## Additional work beyond the prompt

None.
