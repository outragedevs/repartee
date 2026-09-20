import base64
import http.server
import json
import os
from pathlib import Path
import signal
import ssl
import struct
import subprocess
import sys
import threading
import time
from urllib.parse import urlsplit

from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import ec, utils
from cryptography.hazmat.primitives.ciphers.aead import AESGCM
from cryptography.hazmat.primitives.kdf.hkdf import HKDF

root = Path('/fixture')
address = sys.argv[1]


def run(args, **kwargs):
    return subprocess.run(args, check=True, capture_output=True, **kwargs)


def b64(value):
    return base64.urlsafe_b64encode(value).decode().rstrip('=')


def unb64(value):
    return base64.urlsafe_b64decode(value + '=' * (-len(value) % 4))


run(['openssl', 'req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-keyout', str(root / 'ca-key.pem'),
     '-out', str(root / 'ca.pem'), '-days', '1', '-subj', '/CN=Disposable fixture CA', '-addext', 'basicConstraints=critical,CA:TRUE'])
run(['openssl', 'req', '-new', '-newkey', 'rsa:2048', '-nodes', '-keyout', str(root / 'key.pem'),
     '-out', str(root / 'server.csr'), '-subj', '/CN=localhost'])
(root / 'extensions').write_text(f'basicConstraints=critical,CA:FALSE\nsubjectAltName=DNS:localhost,IP:127.0.0.1,IP:{address}\nextendedKeyUsage=serverAuth\n')
run(['openssl', 'x509', '-req', '-in', str(root / 'server.csr'), '-CA', str(root / 'ca.pem'), '-CAkey', str(root / 'ca-key.pem'),
     '-CAcreateserial', '-out', str(root / 'cert.pem'), '-days', '1', '-extfile', str(root / 'extensions')])
private = ec.generate_private_key(ec.SECP256R1())
public = private.public_key().public_bytes(serialization.Encoding.X962, serialization.PublicFormat.UncompressedPoint)
auth = os.urandom(16)
endpoint = f'https://{address}:9443/subscription'
(root / 'subscription.json').write_text(json.dumps({'endpoint': endpoint, 'p256dh': b64(public), 'auth': b64(auth)}))
(root / 'subscription.json').chmod(0o600)


class Receiver(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_POST(self):
        try:
            if self.path != '/subscription':
                self.send_response(404)
                self.end_headers()
                return
            assert self.headers['Content-Encoding'] == 'aes128gcm'
            authorization = self.headers['Authorization']
            scheme, params = authorization.split(' ', 1)
            assert scheme.lower() == 'vapid'
            fields = dict(part.strip().split('=', 1) for part in params.split(','))
            expected = (root / 'expected-vapid').read_text()
            assert fields['k'] == expected
            token_header, token_body, token_sig = fields['t'].split('.')
            claims = json.loads(unb64(token_body))
            assert claims['aud'] == f'https://{address}:9443'
            assert time.time() < claims['exp'] <= time.time() + 86401
            signature = unb64(token_sig)
            assert len(signature) == 64
            signature = utils.encode_dss_signature(int.from_bytes(signature[:32], 'big'), int.from_bytes(signature[32:], 'big'))
            verifier = ec.EllipticCurvePublicKey.from_encoded_point(ec.SECP256R1(), unb64(fields['k']))
            verifier.verify(signature, f'{token_header}.{token_body}'.encode(), ec.ECDSA(hashes.SHA256()))
            body = self.rfile.read(int(self.headers['Content-Length']))
            salt, record_size, key_size = body[:16], struct.unpack('!I', body[16:20])[0], body[20]
            assert record_size == 2048 and key_size == 65
            server_public = body[21:21 + key_size]
            shared = private.exchange(ec.ECDH(), ec.EllipticCurvePublicKey.from_encoded_point(ec.SECP256R1(), server_public))
            ikm = HKDF(algorithm=hashes.SHA256(), length=32, salt=auth, info=b'WebPush: info\0' + public + server_public).derive(shared)
            key = HKDF(algorithm=hashes.SHA256(), length=16, salt=salt, info=b'Content-Encoding: aes128gcm\0').derive(ikm)
            nonce = HKDF(algorithm=hashes.SHA256(), length=12, salt=salt, info=b'Content-Encoding: nonce\0').derive(ikm)
            plaintext = AESGCM(key).decrypt(nonce, body[21 + key_size:], None).rstrip(b'\0')
            assert plaintext.endswith(b'\x02')
            payload = plaintext[:-1].decode()
            with (root / 'received.jsonl').open('a') as output:
                output.write(json.dumps({'payload': payload, 'verified_vapid': True}) + '\n')
            self.send_response(201)
            self.end_headers()
        except Exception:
            (root / 'receiver-error').write_text('Push authentication or decryption failed')
            self.send_response(400)
            self.end_headers()


server = http.server.ThreadingHTTPServer(('0.0.0.0', 9443), Receiver)
context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
context.load_cert_chain(root / 'cert.pem', root / 'key.pem')
server.socket = context.wrap_socket(server.socket, server_side=True)
threading.Thread(target=server.serve_forever, daemon=True).start()
upstream = subprocess.Popen(['python3', '/upstream.py', str(root / 'upstream-ready'), str(root / 'upstream-events'), '--redaction'])
while not (root / 'upstream-ready').exists():
    if upstream.poll() is not None:
        raise RuntimeError('Disposable upstream exited')
    time.sleep(0.02)
upstream_port = json.loads((root / 'upstream-ready').read_text())['port']
config = root / 'config'
config.write_text('hostname fixture.local\ndb sqlite3 /fixture/main.db\nlisten ircs://0.0.0.0:6667\n'
                  'listen unix+admin:///fixture/admin\ntls /fixture/cert.pem /fixture/key.pem\nmessage-store db\n')
run(['/soju/sojudb', '-config', str(config), 'create-user', 'fixture'], input=b'fixture-password\n')
environment = os.environ.copy()
environment['SSL_CERT_FILE'] = str(root / 'ca.pem')
with (root / 'server.log').open('w') as log:
    process = subprocess.Popen(['/soju/soju', '-config', str(config)], stdout=log, stderr=log, env=environment)
    signal.signal(signal.SIGTERM, lambda *_: process.terminate())
    while not (root / 'admin').exists():
        if process.poll() is not None:
            raise RuntimeError('Soju exited before creating admin socket')
        time.sleep(0.02)
    run(['/soju/sojuctl', '-config', str(config), 'user', 'run', 'fixture', 'network', 'create',
         '-name', 'fixture', '-addr', f'irc+insecure://127.0.0.1:{upstream_port}', '-enabled', 'true'])
    while not (root / 'upstream-events').exists():
        if process.poll() is not None:
            raise RuntimeError('Soju exited before upstream registration')
        time.sleep(0.02)
    (root / 'ready').touch()
    process.wait()
upstream.terminate()
upstream.wait()
server.shutdown()
