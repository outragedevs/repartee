"""Exercise the real daemon shim using an owned PTY, not a desktop terminal."""

import errno
import fcntl
import json
import os
import pty
import select
import struct
import subprocess
import termios
import time
from pathlib import Path


def verify_native_attach(container, app_name, diagnostic):
    events = Path(os.environ['REPARTEE_PRESENCE_EVENTS'])

    def away():
        rows = [json.loads(line) for line in events.read_text().splitlines()]
        rows = [row for row in rows if 'away' in row]
        return bool(rows) and rows[-1]['away'] is not None

    def wait(predicate, label, drain=lambda: None):
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline:
            drain()
            if predicate():
                return
            time.sleep(.02)
        raise TimeoutError(label)

    wait(away, 'Browser disconnect did not retire presence')
    for detach in (b'\x1c', b'/detach\r'):
        attached = diagnostic.read_text().count('shim attached')
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 35, 120, 0, 0))
        process = None
        output = bytearray()

        def drain():
            while select.select([master], [], [], 0)[0]:
                try:
                    data = os.read(master, 65536)
                except OSError as error:
                    if error.errno == errno.EIO:
                        return
                    raise
                if not data:
                    return
                output.extend(data)

        try:
            process = subprocess.Popen(
                ['docker', 'exec', '-it', '-e', 'TERM=xterm-256color', container,
                 f'/usr/local/bin/{app_name}', 'a', '1'],
                stdin=slave, stdout=slave, stderr=slave,
            )
            os.close(slave)
            slave = None
            wait(lambda: diagnostic.read_text().count('shim attached') > attached,
                 'Native shim did not attach', drain)
            wait(lambda: b'\x1b[?1004h' in output, 'Terminal focus reporting was not enabled', drain)
            assert away(), 'Attaching without activity must not announce presence'
            os.write(master, b'/help\r')
            wait(lambda: not away(), 'Native keyboard activity did not announce presence', drain)
            os.write(master, b'\x1b[O')
            wait(away, 'Terminal focus-loss report did not retire presence', drain)
            os.write(master, b'\x1b[I')
            wait(lambda: not away(), 'Terminal focus-gain report did not announce presence', drain)
            os.write(master, detach)
            wait(lambda: process.poll() is not None, 'Native detach did not exit the shim', drain)
            drain()
            assert process.returncode == 0, f'Shim exit code: {process.returncode}'
            assert b'Detached from ' in output, 'Daemon did not confirm detach'
            wait(away, 'Native detach did not retire presence')
            state = subprocess.run(['docker', 'inspect', '-f', '{{.State.Running}}', container],
                                   check=True, text=True, capture_output=True, timeout=10)
            assert state.stdout.strip() == 'true', 'Native detach stopped the daemon'
        finally:
            if process is not None and process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)
            os.close(master)
            if slave is not None:
                os.close(slave)
    print('PASS: real PTY shim attach, keyboard activity, injected CSI focus reports, '
          'chord/command detach, reattach and upstream AWAY; daemon remains running')
