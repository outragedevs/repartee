import argparse
import json
import http.server
import threading
import urllib.parse
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile

from test_bouncer_binding import PINS, ROOT, run, wait_ready


def scenario(implementation, source, auto_away, setname=False, monitor=False, monitor_unavailable=False, invites=False, names=False, redaction=False, filehost=False, oauth=False, upstream_auth=False, account_registration=False, server_search=False, metadata=False, certificates=False, channel_context=False, network_icon=False):
    with tempfile.TemporaryDirectory(prefix="bouncer-presence-", dir="/tmp") as directory:
        temporary = Path(directory)
        processes = []
        oauth_server = None
        with (temporary / "server.log").open("w+") as log:
            try:
                http_port = None
                if filehost or oauth or (certificates and implementation == "soju"):
                    with socket.socket() as reservation:
                        reservation.bind(("127.0.0.1", 0))
                        http_port = reservation.getsockname()[1]
                    run(["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes",
                         "-keyout", str(temporary / "ca-key.pem"), "-out", str(temporary / "ca.pem"),
                         "-days", "1", "-subj", "/CN=Disposable fixture CA",
                         "-addext", "basicConstraints=critical,CA:TRUE"], capture_output=True)
                    run(["openssl", "req", "-new", "-newkey", "rsa:2048", "-nodes",
                         "-keyout", str(temporary / "key.pem"), "-out", str(temporary / "server.csr"),
                         "-subj", "/CN=localhost"], capture_output=True)
                    (temporary / "extensions").write_text("basicConstraints=critical,CA:FALSE\nsubjectAltName=DNS:localhost,IP:127.0.0.1\nextendedKeyUsage=serverAuth\n")
                    run(["openssl", "x509", "-req", "-in", str(temporary / "server.csr"),
                         "-CA", str(temporary / "ca.pem"), "-CAkey", str(temporary / "ca-key.pem"),
                         "-CAcreateserial", "-out", str(temporary / "cert.pem"), "-days", "1",
                         "-extfile", str(temporary / "extensions")], capture_output=True)
                ready = temporary / "upstream.json"
                events = temporary / "events.jsonl"
                upstream = subprocess.Popen([
                    sys.executable, str(ROOT / "scripts/fixtures/presence-upstream.py"),
                    str(ready), str(events), *(["--setname"] if setname else []),
                    *(["--monitor"] if monitor or monitor_unavailable else []),
                    *(["--monitor-unavailable"] if monitor_unavailable else []),
                    *(["--invites"] if invites else []),
                    *(["--names"] if names or channel_context else []),
                    *(["--channel-context"] if channel_context else []),
                    *(["--network-icon"] if network_icon else []),
                    *(["--redaction"] if redaction or server_search or metadata else []),
                    *(["--upstream-auth"] if upstream_auth else []),
                    *(["--account-registration"] if account_registration else []),
                ], stdout=log, stderr=log)
                processes.append(upstream)
                wait_ready(upstream, ready.exists)
                upstream_port = json.loads(ready.read_text())["port"]
                if implementation == "lurker":
                    bouncer_ready = temporary / "bouncer.json"
                    bouncer = subprocess.Popen([
                        "node", str(source / "node_modules/tsx/dist/cli.mjs"),
                        str(ROOT / "scripts/fixtures/lurker-presence.mts"),
                        str(source), str(bouncer_ready), str(upstream_port), *([str(http_port)] if filehost else []),
                    ], cwd=source, stdout=log, stderr=log)
                    processes.append(bouncer)
                    wait_ready(bouncer, bouncer_ready.exists)
                    settings = json.loads(bouncer_ready.read_text())
                else:
                    with socket.socket() as reservation:
                        reservation.bind(("127.0.0.1", 0))
                        port = reservation.getsockname()[1]
                    config = temporary / "config"
                    admin = temporary / "admin"
                    config.write_text(
                        f"hostname fixture.local\ndb sqlite3 {temporary}/main.db\n"
                        f"listen {'ircs' if oauth or certificates else 'irc+insecure'}://127.0.0.1:{port}\n"
                        f"listen unix+admin://{admin}\nmessage-store db\n"
                        + (f"listen https://127.0.0.1:{http_port}\ntls {temporary}/cert.pem {temporary}/key.pem\n"
                           f"http-ingress https://127.0.0.1:{http_port}\nfile-upload fs {temporary}/uploads\n" if filehost else "")
                    )
                    run([str(source / "sojudb"), "-config", str(config), "create-user", "fixture"],
                        input="fixture-password\n", capture_output=True)
                    if oauth:
                        class OAuthHandler(http.server.BaseHTTPRequestHandler):
                            def log_message(self, *args):
                                pass

                            def respond(self, value):
                                body = json.dumps(value).encode()
                                self.send_response(200)
                                self.send_header("Content-Type", "application/json")
                                self.send_header("Content-Length", str(len(body)))
                                self.end_headers()
                                self.wfile.write(body)

                            def do_GET(self):
                                self.respond({"issuer": f"http://127.0.0.1:{self.server.server_port}", "introspection_endpoint": f"http://127.0.0.1:{self.server.server_port}/introspect",
                                              "introspection_endpoint_auth_methods_supported": ["none"]})

                            def do_POST(self):
                                form = urllib.parse.parse_qs(self.rfile.read(int(self.headers["Content-Length"])).decode())
                                self.respond({"active": form.get("token") == ["fixture-token"], "username": "fixture"})

                        oauth_server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), OAuthHandler)
                        threading.Thread(target=oauth_server.serve_forever, daemon=True).start()
                        with config.open("a") as output:
                            output.write(f"auth oauth2 http://127.0.0.1:{oauth_server.server_port}\n")
                            if not filehost:
                                output.write(f"tls {temporary}/cert.pem {temporary}/key.pem\n")
                    if certificates:
                        with config.open("a") as output:
                            output.write(f"tls {temporary}/cert.pem {temporary}/key.pem\nclient-cert-auth true\n")
                        run(["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes",
                             "-keyout", str(temporary / "client-key.pem"), "-out", str(temporary / "client-cert.pem"),
                             "-days", "1", "-subj", "/CN=Disposable test client"], capture_output=True)
                        client_pem = temporary / "client.pem"
                        client_pem.write_bytes((temporary / "client-cert.pem").read_bytes() + (temporary / "client-key.pem").read_bytes())
                        client_pem.chmod(0o600)
                    bouncer = subprocess.Popen([str(source / "soju"), "-config", str(config)], stdout=log, stderr=log)
                    processes.append(bouncer)
                    wait_ready(bouncer, admin.exists)
                    run([str(source / "sojuctl"), "-config", str(config), "user", "run", "fixture", "network", "create",
                         "-name", "fixture", "-addr", f"irc+insecure://127.0.0.1:{upstream_port}",
                         "-auto-away", str(auto_away).lower(), "-enabled", "true"], capture_output=True)
                    settings = {"port": port, "user": "fixture"}
                wait_ready(bouncer, events.exists)
                environment = os.environ.copy()
                environment.update({
                    "REPARTEE_BOUNCER_TEST_PORT": str(settings["port"]),
                    "REPARTEE_BOUNCER_TEST_USER": settings["user"],
                    "REPARTEE_PRESENCE_EVENTS": str(events),
                    "REPARTEE_PRESENCE_PROVIDER": implementation,
                    "REPARTEE_PRESENCE_AUTO_AWAY": str(auto_away).lower(),
                })
                test_filter = "pinned_bouncer_presence"
                if filehost:
                    environment["REPARTEE_FILEHOST_TEST_CA"] = str(temporary / "ca.pem")
                    test_filter = "pinned_bouncer_filehost"
                if oauth:
                    environment["REPARTEE_OAUTH_TEST_CA"] = str(temporary / "ca.pem")
                    test_filter = "pinned_bouncer_oauthbearer"
                if upstream_auth or account_registration:
                    secrets = temporary / "auth.env"
                    secrets.write_text("UPSTREAM_PASSWORD=disposable\u00a0password\nWRONG_PASSWORD=invalid password\nACCOUNT_PASSWORD=registration-password\nVERIFY_CODE=fixture-code\nWRONG_CODE=wrong-code\n")
                    secrets.chmod(0o600)
                    environment["REPARTEE_UPSTREAM_AUTH_ENV"] = str(secrets)
                    environment["REPARTEE_SOJU_TEST_DB"] = str(temporary / "main.db")
                    environment["REPARTEE_SOJU_TEST_CLI"] = str(source / "sojuctl")
                    environment["REPARTEE_SOJU_TEST_CONFIG"] = str(temporary / "config")
                    test_filter = "pinned_bouncer_account_registration" if account_registration else "pinned_bouncer_upstream_auth"
                if setname:
                    environment["REPARTEE_SETNAME_BOUND"] = "1"
                    environment["REPARTEE_BOUNCER_TEST_PROVIDER"] = implementation
                    test_filter = "pinned_bouncer_setname"
                if monitor:
                    test_filter = "pinned_bouncer_monitor"
                if monitor_unavailable:
                    test_filter = "pinned_bouncer_no_monitor"
                if invites:
                    test_filter = "pinned_bouncer_invites"
                if names:
                    test_filter = "pinned_bouncer_names"
                if redaction:
                    test_filter = "pinned_bouncer_redaction"
                if server_search:
                    environment["REPARTEE_SEARCH_LABELS"] = str(int(names))
                    test_filter = "pinned_bouncer_server_search"
                if metadata:
                    test_filter = "pinned_bouncer_metadata"
                if network_icon:
                    test_filter = "pinned_bouncer_network_icon"
                if channel_context:
                    test_filter = "pinned_bouncer_channel_context"
                if certificates:
                    test_filter = "pinned_bouncer_certificates"
                    if implementation == "soju":
                        environment["REPARTEE_OAUTH_TEST_CA"] = str(temporary / "ca.pem")
                        environment["REPARTEE_CLIENT_CERT_TEST_PEM"] = str(temporary / "client.pem")
                run(["make", "test", f"TEST_ARGS={test_filter} -- --ignored --nocapture"],
                    cwd=ROOT, env=environment)
                if setname and implementation == "soju":
                    rows = [json.loads(line) for line in events.read_text().splitlines()]
                    if not any(row.get("realname") == "Fixture changed name %" for row in rows):
                        raise AssertionError("SETNAME never reached the IRC upstream")
                print(f"{implementation}: {test_filter} real-upstream fixture passed (AutoAway={auto_away})")
            except Exception:
                if events.exists():
                    controls = [row["control"] for line in events.read_text().splitlines()
                                if "control" in (row := json.loads(line))]
                    print("Fixture control steps:", controls)
                log.flush()
                log.seek(0)
                print(log.read())
                raise
            finally:
                if oauth_server is not None:
                    oauth_server.shutdown()
                    oauth_server.server_close()
                for process in reversed(processes):
                    if process.poll() is None:
                        process.terminate()
                        try:
                            process.wait(timeout=5)
                        except subprocess.TimeoutExpired:
                            process.kill()
                            process.wait(timeout=5)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("implementation", choices=PINS)
    parser.add_argument("source", type=Path)
    parser.add_argument("--setname", action="store_true")
    parser.add_argument("--monitor", action="store_true")
    parser.add_argument("--monitor-unavailable", action="store_true")
    parser.add_argument("--invites", action="store_true")
    parser.add_argument("--network-icon", action="store_true")
    parser.add_argument("--channel-context", action="store_true")
    parser.add_argument("--names", action="store_true")
    parser.add_argument("--redaction", action="store_true")
    parser.add_argument("--server-search", action="store_true")
    parser.add_argument("--metadata", action="store_true")
    parser.add_argument("--certificates", action="store_true")
    parser.add_argument("--filehost", action="store_true")
    parser.add_argument("--oauth", action="store_true")
    parser.add_argument("--upstream-auth", action="store_true")
    parser.add_argument("--account-registration", action="store_true")
    args = parser.parse_args()
    if (args.upstream_auth or args.account_registration) and args.implementation == "soju" and not args.oauth:
        parser.error("--upstream-auth requires --oauth for its verified TLS fixture")
    if args.oauth and args.implementation != "soju":
        parser.error("OAUTHBEARER is only advertised by Soju")
    source = args.source.resolve()
    head = run(["git", "-C", str(source), "rev-parse", "HEAD"], capture_output=True).stdout.strip()
    if head != PINS[args.implementation]:
        raise RuntimeError("Upstream checkout does not match the audited revision")
    scenario(args.implementation, source, True, args.setname, args.monitor, args.monitor_unavailable, args.invites, args.names, args.redaction, args.filehost, args.oauth, args.upstream_auth, args.account_registration, args.server_search, args.metadata, args.certificates, args.channel_context, args.network_icon)
    if args.implementation == "soju" and not args.setname and not args.monitor and not args.monitor_unavailable and not args.invites and not args.names and not args.redaction and not args.filehost and not args.oauth and not args.upstream_auth and not args.account_registration and not args.server_search and not args.metadata and not args.certificates and not args.channel_context and not args.network_icon:
        scenario(args.implementation, source, False)


if __name__ == "__main__":
    main()
