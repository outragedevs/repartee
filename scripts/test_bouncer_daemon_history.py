"""Check history exclusion across real daemon processes in disposable containers."""

import argparse
from contextlib import ExitStack
import json
import os
import re
import sqlite3
import subprocess
import tempfile
import time
from pathlib import Path

from bouncer_fault_proxy import FaultProxy
from bouncer_partial_batch_proxy import PartialBatchProxy
from bouncer_native_attach import verify_native_attach

ROOT = Path(__file__).resolve().parents[1]
APP_NAME = re.search(r'pub const APP_NAME: &str = "([^"]+)"', (ROOT / 'src/constants.rs').read_text()).group(1)


def run(command, **kwargs):
    return subprocess.run(command, check=True, text=True, timeout=120, **kwargs)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('image')
    args = parser.parse_args()
    if os.environ.get('REPARTEE_DAEMON_NATIVE_ATTACH') == '1' and (
        not os.environ.get('REPARTEE_DAEMON_LIVE_HISTORY')
        or os.environ.get('REPARTEE_PRESENCE_AUTO_AWAY') != 'true'
    ):
        parser.error('Native attach acceptance requires the live-history fixture with AutoAway enabled')
    with tempfile.TemporaryDirectory(prefix='bouncer-daemon-', dir='/tmp') as directory, ExitStack() as resources:
        provider_port = int(os.environ['REPARTEE_BOUNCER_TEST_PORT'])
        partial = os.environ.get('REPARTEE_DAEMON_PARTIAL_HISTORY') == '1'
        history_proxy = resources.enter_context(PartialBatchProxy(provider_port)) if partial else None
        search_proxy = resources.enter_context(PartialBatchProxy(history_proxy.port, search=True)) if partial and os.environ.get('REPARTEE_BOUNCER_TEST_PROVIDER') == 'soju' else None
        proxy = search_proxy or history_proxy or (resources.enter_context(FaultProxy(provider_port)) if os.environ.get('REPARTEE_DAEMON_LIVE_HISTORY') else None)
        data = Path(directory)
        (data / 'config.toml').write_text(f'''
[general]
nick = 'fixture'
flood_protection = false
[display]
backlog_lines = 200
[logging]
enabled = true
encrypt = false
retention_days = 0
event_retention_hours = 0
[web]
enabled = true
bind_address = '0.0.0.0'
port = 8443
[servers.fixture]
label = 'fixture'
address = 'host.docker.internal'
port = {proxy.port if proxy else provider_port}
tls = false
autoconnect = true
reconnect_delay = 1
channels = []
bouncer_network_id = '{os.environ['REPARTEE_BOUNCER_TEST_NETID']}'
''')
        (data / '.env').write_text(
            f"FIXTURE_SASL_USER={os.environ['REPARTEE_BOUNCER_TEST_USER']}\n"
            'FIXTURE_SASL_PASS=fixture-password\nWEB_PASSWORD=fixture-web-password\n'
        )
        for cycle in range(2):
            container = run([
                'docker', 'run', '-d', '-p', '127.0.0.1::8443',
                '--mount', f'type=bind,src={data},dst=/root/.{APP_NAME}',
                '-e', 'RUST_LOG=trace', args.image, f'/usr/local/bin/{APP_NAME}', '-d',
            ], capture_output=True).stdout.strip()
            try:
                port = run(['docker', 'port', container, '8443/tcp'], capture_output=True).stdout.strip().rsplit(':', 1)[1]
                environment = dict(os.environ, REPARTEE_DAEMON_TEST_URL=f'https://127.0.0.1:{port}',
                                   REPARTEE_DAEMON_TEST_CYCLE=str(cycle))
                if proxy:
                    environment["REPARTEE_FAULT_CONTROL_URL"] = proxy.control_url
                if partial and cycle == 0:
                    environment["REPARTEE_HISTORY_FAULT_CONTROL"] = history_proxy.control_url
                    if search_proxy:
                        environment["REPARTEE_SEARCH_FAULT_CONTROL"] = search_proxy.control_url
                deadline = time.monotonic() + 30
                while time.monotonic() < deadline:
                    if run(['docker', 'inspect', '-f', '{{.State.Running}}', container], capture_output=True).stdout.strip() != 'true':
                        raise RuntimeError('Disposable daemon exited before readiness')
                    health = subprocess.run(['curl', '-skf', '--max-time', '1', environment['REPARTEE_DAEMON_TEST_URL'] + '/api/health'], capture_output=True)
                    if health.returncode == 0:
                        break
                    time.sleep(.1)
                else:
                    raise TimeoutError('Disposable daemon HTTPS readiness timed out')
                run(['node', str(ROOT / ('scripts/fixtures/daemon-live-browser.cjs' if os.environ.get('REPARTEE_DAEMON_LIVE_HISTORY') else 'scripts/fixtures/daemon-history-browser.cjs'))], env=environment)
                if os.environ.get('REPARTEE_DAEMON_NATIVE_ATTACH') == '1':
                    verify_native_attach(container, APP_NAME, data / f'{APP_NAME}.log')
                run(['docker', 'stop', '-t', '20', container], capture_output=True)
                exit_code = run(['docker', 'inspect', '-f', '{{.State.ExitCode}}', container], capture_output=True).stdout.strip()
                assert exit_code == '0', f'Unclean daemon exit: {exit_code}'
                snapshot = data / f'inspection-{cycle}'
                snapshot.mkdir()
                run(['docker', 'cp', f'{container}:/root/.{APP_NAME}/logs/.', str(snapshot)], capture_output=True)
                with sqlite3.connect(str(snapshot / 'messages.db')) as database:
                    rows = database.execute('SELECT network, buffer, type, text FROM messages ORDER BY id').fetchall()
                    welcome = ('Status', 'status', 'event', f'Welcome to {APP_NAME}! Use /connect <server> to connect.')
                    assert rows == [welcome] * (cycle + 1), f'Unexpected persistent messages after cycle {cycle}: {rows}'
                diagnostic = (data / f'{APP_NAME}.log').read_text()
                assert 'TRACE ' in diagnostic, 'Diagnostic TRACE output was not enabled'
                assert 'web command received' in diagnostic, 'Command receipt diagnostics were not exercised'
                assert 'fixture-memory-' not in diagnostic, 'Live or replayed message body entered diagnostics'
                assert 'fixture-history-' not in diagnostic, 'History body entered diagnostics'
                assert 'fixture-browser-outgoing' not in diagnostic, 'Outgoing body entered diagnostics'
                if cycle == 0 and os.environ.get('REPARTEE_DAEMON_LIVE_HISTORY'):
                    Path(os.environ['REPARTEE_HISTORY_CONTROL']).touch()
                    deadline = time.monotonic() + 30
                    while time.monotonic() < deadline:
                        records = Path(os.environ['REPARTEE_PRESENCE_EVENTS']).read_text().splitlines()
                        if any(json.loads(line).get('offline_ack') for line in records):
                            break
                        time.sleep(.02)
                    else:
                        raise TimeoutError('Bouncer did not acknowledge the offline upstream message')
                print(f'PASS: daemon cycle {cycle + 1}: browser history, clean stop, only local startup events in SQLite, no history bodies in diagnostics')
            except Exception:
                run(['docker', 'logs', container])
                raise
            finally:
                run(['docker', 'rm', '-f', container], capture_output=True)


if __name__ == '__main__':
    main()
