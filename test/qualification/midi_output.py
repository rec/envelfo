"""Opt-in supervised tests using only uniquely named test-owned virtual endpoints."""

from contextlib import ExitStack
from fractions import Fraction
from os import environ, killpg
from pathlib import Path
from queue import Queue
from signal import SIGKILL
from subprocess import PIPE, Popen, TimeoutExpired
from sys import executable, platform
from time import monotonic
from uuid import uuid4

import pytest
from meedee import codecs, events, messages, midi

from envelfo.midi import CCTarget, FineCCTarget, Mapper, Mapping, NrpnTarget, RpnTarget


@pytest.mark.skipif(platform != 'darwin', reason='CoreMIDI virtual-only qualification')
@pytest.mark.parametrize('isolated', [False, True], ids=['in_process', 'isolated'])
def test_mapped_output(isolated: bool, request: pytest.FixtureRequest) -> None:
    if environ.get('ENVELFO_MIDI_CHILD') == '1':
        exercise_output(isolated)
        return
    child = Popen(
        [executable, '-m', 'pytest', '-q', '-s', '-n', '0', request.node.nodeid],
        cwd=Path(__file__).resolve().parents[2],
        env={**environ, 'ENVELFO_MIDI_CHILD': '1'},
        stdout=PIPE,
        stderr=PIPE,
        text=True,
        start_new_session=True,
    )
    try:
        stdout, stderr = child.communicate(timeout=45)
    except TimeoutExpired:
        killpg(child.pid, SIGKILL)
        stdout, stderr = child.communicate(timeout=5)
        pytest.fail(f'native flow exceeded 45 seconds\n{stdout}\n{stderr}')
    print(stdout, end='')
    assert child.returncode == 0, stderr + stdout


def exercise_output(isolated: bool) -> None:
    received: Queue[bytes] = Queue(maxsize=128)

    def receive(packet: events.ReceivedPacket) -> None:
        assert isinstance(packet.packet, bytes)
        received.put_nowait(packet.packet)

    name = f'envelfo-output-{uuid4().hex}'
    mapper = Mapper(
        mappings=[
            Mapping(output='level', target=t, input_minimum=0.0, input_maximum=1.0)
            for t in [
                CCTarget(channel=1, controller=11),
                FineCCTarget(channel=1, controller=1),
                RpnTarget(channel=1, parameter=0, resolution=14),
                NrpnTarget(channel=1, parameter=7, resolution=14),
            ]
        ],
        byte_rate=1000,
        burst_bytes=128,
    )
    with ExitStack() as stack:
        sink = stack.enter_context(
            midi.Input(isolated=isolated).create_virtual(name, receive)
        )
        output = midi.Output(isolated=isolated)
        port = next(p for p in output.ports() if p.name == name)
        connection = stack.enter_context(output.connect(port))
        expected = bytearray()
        for at, level in enumerate([0.0, 0.5, 1.0]):
            batch = mapper.prepare(at=Fraction(at), values={'level': level})
            for message in batch.messages:
                if isinstance(message, messages.ControlChange):
                    packet = codecs.encode(message)
                    expected.extend(packet)
                    connection.send(packet)
                else:
                    expected.extend(
                        b''.join(codecs.encode(p) for p in codecs.expand(message))
                    )
                    connection.send_command(message)
            mapper.confirm(batch)
        observed = bytearray()
        deadline = monotonic() + 5
        while len(observed) < len(expected) and monotonic() < deadline:
            observed.extend(received.get(timeout=max(0.001, deadline - monotonic())))
        assert observed == expected
        assert sink.dropped_packets == 0
        assert connection.worker_failure is None
        unchanged = mapper.prepare(at=Fraction(3), values={'level': 1.0})
        assert unchanged.messages == [] and unchanged.wire_bytes == 0
        mapper.confirm(unchanged)
        connection.close()
        failed = mapper.prepare(at=Fraction(4), values={'level': 0.0})
        with pytest.raises(midi.StateError):
            connection.send(codecs.encode(failed.messages[0]))
        mapper.report_loss()
        with pytest.raises(RuntimeError, match='restart'):
            mapper.prepare(at=Fraction(5), values={'level': 0.0})
        assert not sink.closed
    print(
        f'Ordered mapped MIDI and explicit send failure passed: isolated={isolated}',
        flush=True,
    )
