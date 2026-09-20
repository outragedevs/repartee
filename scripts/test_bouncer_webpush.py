import argparse
import ipaddress
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time
import uuid

from test_bouncer_binding import PINS, ROOT, run


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('source', type=Path)
    args = parser.parse_args()
    source = args.source.resolve()
    head = run(['git', '-C', str(source), 'rev-parse', 'HEAD'], capture_output=True).stdout.strip()
    if head != PINS['soju']:
        raise RuntimeError('Soju checkout does not match the audited revision')
    image = 'repartee-soju-webpush-fixture:82e8b7a'
    with tempfile.TemporaryDirectory(prefix='bouncer-webpush-', dir='/tmp') as directory:
        temporary = Path(directory)
        context = temporary / 'build'
        context.mkdir()
        (context / 'source').mkdir()
        archive = subprocess.run(['git', '-C', str(source), 'archive', 'HEAD'], check=True, capture_output=True).stdout
        subprocess.run(['tar', '-x', '-C', str(context / 'source')], input=archive, check=True)
        shutil.copy(ROOT / 'scripts/fixtures/soju-webpush.Dockerfile', context / 'Dockerfile')
        run(['docker', 'build', '-q', '-t', image, str(context)])
        networks = [json.loads(line) for line in run(['docker', 'network', 'ls', '--format', 'json'], capture_output=True).stdout.splitlines()]
        used = []
        for network in networks:
            details = json.loads(run(['docker', 'network', 'inspect', network['ID']], capture_output=True).stdout)[0]
            used.extend(ipaddress.ip_network(item['Subnet']) for item in (details.get('IPAM', {}).get('Config') or []) if item.get('Subnet'))
        subnet = next(ipaddress.ip_network(f'198.18.{n}.0/24') for n in range(240, 254)
                      if all(not ipaddress.ip_network(f'198.18.{n}.0/24').overlaps(item) for item in used if item.version == 4))
        address = str(subnet.network_address + 2)
        identifier = uuid.uuid4().hex[:12]
        network, container = f'repartee-push-{identifier}', f'repartee-push-{identifier}'
        created = False
        try:
            run(['docker', 'network', 'create', '--subnet', str(subnet), network], capture_output=True)
            created = True
            run(['docker', 'run', '-d', '--name', container, '--network', network, '--ip', address,
                 '-p', '127.0.0.1::6667', '-v', f'{temporary}:/fixture', '-v', f'{ROOT / "scripts/fixtures/webpush-receiver.py"}:/receiver.py:ro',
                 '-v', f'{ROOT / "scripts/fixtures/presence-upstream.py"}:/upstream.py:ro',
                 image, 'python3', '/receiver.py', address], capture_output=True)
            deadline = time.monotonic() + 30
            while not (temporary / 'ready').exists():
                if time.monotonic() > deadline:
                    raise RuntimeError('Disposable WebPush provider did not become ready')
                time.sleep(0.05)
            port = run(['docker', 'port', container, '6667/tcp'], capture_output=True).stdout.strip().rsplit(':', 1)[1]
            environment = os.environ.copy()
            environment.update({'REPARTEE_WEBPUSH_FIXTURE': str(temporary), 'REPARTEE_BOUNCER_TEST_PORT': port,
                                'REPARTEE_OAUTH_TEST_CA': str(temporary / 'ca.pem')})
            run(['make', 'test', 'TEST_ARGS=pinned_bouncer_webpush -- --ignored --nocapture'], cwd=ROOT, env=environment)
            if (temporary / 'receiver-error').exists():
                raise RuntimeError('Receiver reported push authentication/decryption failure')
            print('Soju: encrypted WebPush receiver fixture passed with VAPID verification')
        finally:
            subprocess.run(['docker', 'rm', '-f', container], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            if created:
                subprocess.run(['docker', 'network', 'rm', network], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


if __name__ == '__main__':
    main()
