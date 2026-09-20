import contextlib
import argparse
import json
import datetime
import sqlite3
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import time


PINS = {
    "soju": "82e8b7adfb2ab64ec3b88807d29b8b6940236008",
    "lurker": "be42a04e73d6f337e76734684deb457cb5dcdb5f",
}
ROOT = Path(__file__).resolve().parents[1]


def run(command, **kwargs):
    return subprocess.run(command, check=True, text=True, timeout=kwargs.pop("timeout", 180), **kwargs)


def wait_ready(process, predicate):
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise RuntimeError("Bouncer fixture exited before becoming ready")
        if predicate():
            return
        time.sleep(0.05)
    raise TimeoutError("Bouncer fixture did not become ready")


def tls_material(directory, case):
    for name in ("trusted", "unrelated"):
        run(["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes",
             "-keyout", str(directory / f"{name}-key.pem"), "-out", str(directory / f"{name}-ca.pem"),
             "-days", "1", "-subj", f"/CN=Disposable {name} fixture CA",
             "-addext", "basicConstraints=critical,CA:TRUE"], capture_output=True)
    run(["openssl", "req", "-new", "-newkey", "rsa:2048", "-nodes",
         "-keyout", str(directory / "server-key.pem"), "-out", str(directory / "server.csr"),
         "-subj", "/CN=Disposable bouncer"], capture_output=True)
    address = "192.0.2.1" if case == "wrong-host" else "127.0.0.1"
    (directory / "extensions").write_text(
        f"basicConstraints=critical,CA:FALSE\nsubjectAltName=IP:{address}\nextendedKeyUsage=serverAuth\n")
    run(["openssl", "x509", "-req", "-in", str(directory / "server.csr"),
         "-CA", str(directory / "trusted-ca.pem"), "-CAkey", str(directory / "trusted-key.pem"),
         "-CAcreateserial", "-out", str(directory / "server-cert.pem"), "-days", "1",
         "-extfile", str(directory / "extensions")], capture_output=True)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("implementation", choices=PINS)
    parser.add_argument("source", type=Path)
    parser.add_argument("--test-filter", default="pinned_bouncer_")
    parser.add_argument("--daemon-image", help="Run the real daemon/browser lifecycle fixture using this container image")
    parser.add_argument("--history-stall", choices=("timeout", "cancel", "batch-expiry", "request-expiry"), help="Hold a partial history response, then deliver it late")
    parser.add_argument("--partial-history", action="store_true", help="Cut an actual history batch after its first message")
    parser.add_argument("--history-range", action="store_true", help="Probe BETWEEN against the real provider")
    parser.add_argument("--tls-case", choices=("valid", "untrusted", "wrong-host"))
    parser.add_argument("--pass-auth", nargs="?", const="user", choices=("user", "combined"))
    parser.add_argument("--account-matrix", action="store_true", help="Exercise concurrent history across two accounts and four networks")
    parser.add_argument("--target-tie", action="store_true", help="Seed 1001 conversations sharing a timestamp")
    args = parser.parse_args()
    if args.account_matrix:
        if any((args.partial_history, args.history_stall, args.history_range, args.tls_case, args.pass_auth, args.daemon_image, args.target_tie)) or args.test_filter != "pinned_bouncer_":
            parser.error("--account-matrix is a standalone scenario")
        args.test_filter = "pinned_bouncer_account_matrix"
    if args.partial_history or args.history_stall:
        if (args.partial_history and args.history_stall) or args.history_range or args.tls_case or args.pass_auth or args.daemon_image or args.target_tie or args.test_filter != "pinned_bouncer_":
            parser.error("--partial-history is a standalone partial-batch scenario")
        args.test_filter = "pinned_bouncer_stalled_history" if args.history_stall else "pinned_bouncer_partial_history"
    if args.history_range and (args.tls_case or args.pass_auth or args.daemon_image or args.target_tie or args.test_filter != "pinned_bouncer_"):
        parser.error("--history-range is a standalone provider protocol scenario")
    if args.tls_case:
        if args.pass_auth or args.daemon_image or args.target_tie or args.test_filter not in ("pinned_bouncer_", "pinned_bouncer_tls_validation"):
            parser.error("--tls-case is a standalone TLS validation scenario")
        args.test_filter = "pinned_bouncer_tls_validation"
    if args.pass_auth and (args.implementation != "lurker" or args.test_filter != "pinned_bouncer_persistent_history" or args.daemon_image or args.target_tie):
        parser.error("--pass-auth requires Lurker and --test-filter pinned_bouncer_persistent_history")
    source = args.source.resolve()
    head = run(["git", "-C", str(source), "rev-parse", "HEAD"], capture_output=True).stdout.strip()
    if head != PINS[args.implementation]:
        raise RuntimeError("Upstream checkout does not match the audited revision")
    with tempfile.TemporaryDirectory(prefix="bouncer-binding-", dir="/tmp") as directory:
        temporary = Path(directory)
        process = None
        matrix_upstream = None
        if args.tls_case:
            tls_material(temporary, args.tls_case)
        with contextlib.ExitStack() as faults, (temporary / "server.log").open("w+") as log:
            try:
                if args.implementation == "lurker":
                    ready = temporary / "ready.json"
                    process = subprocess.Popen([
                        "node", str(source / "node_modules/tsx/dist/cli.mjs"),
                        str(ROOT / "scripts/fixtures/lurker-binding.mts"), str(source), str(ready),
                    ], cwd=source, stdout=log, stderr=log,
                        env=dict(os.environ, REPARTEE_BOUNCER_ACCOUNT_MATRIX="1" if args.account_matrix else "0",
                                 REPARTEE_BOUNCER_DAEMON_FIXTURE="1" if args.daemon_image else "0",
                                 REPARTEE_BOUNCER_TARGET_TIE="1" if args.target_tie else "0",
                                 REPARTEE_BOUNCER_TLS_FIXTURE="1" if args.tls_case else "0",
                                 LURKER_BOUNCER_TLS_CERT=str(temporary / "server-cert.pem") if args.tls_case else "",
                                 LURKER_BOUNCER_TLS_KEY=str(temporary / "server-key.pem") if args.tls_case else ""))
                    wait_ready(process, ready.exists)
                    settings = json.loads(ready.read_text())
                else:
                    with socket.socket() as reservation:
                        reservation.bind(("127.0.0.1", 0))
                        port = reservation.getsockname()[1]
                    config = temporary / "config"
                    admin = temporary / "admin"
                    config.write_text(
                        f"hostname fixture.local\ndb sqlite3 {temporary}/main.db\n"
                        f"listen {'ircs' if args.tls_case else 'irc+insecure'}://127.0.0.1:{port}\n"
                        f"listen unix+admin://{admin}\nmessage-store db\n"
                        + (f"tls {temporary}/server-cert.pem {temporary}/server-key.pem\n" if args.tls_case else "")
                    )
                    run([str(source / "sojudb"), "-config", str(config), "create-user", "fixture"],
                        input="fixture-password\n", capture_output=True)
                    if args.account_matrix:
                        run([str(source / "sojudb"), "-config", str(config), "create-user", "other"],
                            input="fixture-password\n", capture_output=True)
                    process = subprocess.Popen([str(source / "soju"), "-config", str(config)],
                                               stdout=log, stderr=log)
                    wait_ready(process, admin.exists)
                    run([str(source / "sojuctl"), "-config", str(config), "user", "run", "fixture",
                         "network", "create", "-name", "fixture", "-addr", "irc+insecure://127.0.0.1:1",
                         "-enabled", "false"], capture_output=True)
                    with sqlite3.connect(temporary / "main.db") as database:
                        for target in ("history-peer", "#history-channel"):
                            database.execute("INSERT INTO MessageTarget(network, target) VALUES (1, ?)", (target,))
                            target_id = database.execute("SELECT id FROM MessageTarget WHERE network=1 AND target=?", (target,)).fetchone()[0]
                            for index in range(300):
                                timestamp = (datetime.datetime(2024, 1, 1, tzinfo=datetime.timezone.utc) + datetime.timedelta(seconds=index)).isoformat(timespec="milliseconds").replace("+00:00", "Z")
                                body = f"fixture-history-{index}"
                                recipient = target if target.startswith("#") else "fixture"
                                raw = f"@time={timestamp} :history-peer!user@fixture.local PRIVMSG {recipient} :{body}"
                                database.execute("INSERT INTO Message(target, raw, time, sender, text) VALUES (?, ?, ?, ?, ?)",
                                                 (target_id, raw, timestamp, "history-peer", body))
                    if args.account_matrix:
                        from bouncer_account_matrix import seed_soju
                        matrix_accounts = seed_soju(source, config, temporary / "main.db")
                    if args.target_tie:
                        from audit_bouncer_history_targets import seed
                        with sqlite3.connect(temporary / "main.db") as database:
                            database.execute("DELETE FROM Message")
                            database.execute("DELETE FROM MessageTarget")
                        seed(temporary / "main.db")
                    settings = {"port": port, "network": 1, "user": "fixture"}
                environment = os.environ.copy()
                if args.account_matrix:
                    upstream_ready = temporary / 'matrix-upstream.json'
                    events = temporary / 'matrix-events.jsonl'
                    matrix_upstream = subprocess.Popen([
                        'python3', str(ROOT / 'scripts/fixtures/presence-upstream.py'), str(upstream_ready), str(events),
                        '--history-traffic', '--history-control', str(temporary / 'unused-history-trigger'), '--record-joins',
                    ], stdout=log, stderr=log)
                    wait_ready(matrix_upstream, upstream_ready.exists)
                    environment['REPARTEE_MATRIX_UPSTREAM_PORT'] = str(json.loads(upstream_ready.read_text())['port'])
                    environment['REPARTEE_MATRIX_UPSTREAM_EVENTS'] = str(events)
                    environment['REPARTEE_MATRIX_PROVIDER'] = args.implementation
                    control = temporary / "matrix-control.json"
                    environment["REPARTEE_BOUNCER_MATRIX_CONTROL"] = str(control)
                    if args.implementation == "soju":
                        from bouncer_account_matrix import SojuController
                        faults.enter_context(SojuController(source, config, temporary / "main.db", control, matrix_accounts))
                    environment["REPARTEE_BOUNCER_MATRIX_ACCOUNTS"] = json.dumps(settings["accounts"] if args.implementation == "lurker" else matrix_accounts)
                    def restart_provider():
                        nonlocal process
                        process.terminate()
                        process.wait(timeout=10)
                        if args.implementation == "lurker":
                            process = subprocess.Popen([
                                "node", str(source / "node_modules/tsx/dist/cli.mjs"),
                                str(ROOT / "scripts/fixtures/lurker-matrix-resume.mts"), str(source), str(ready),
                            ], cwd=source, stdout=log, stderr=log)
                            wait_ready(process, (temporary / "resume-ready.json").exists)
                        else:
                            admin.unlink(missing_ok=True)
                            process = subprocess.Popen([str(source / "soju"), "-config", str(config)], stdout=log, stderr=log)
                            wait_ready(process, admin.exists)
                    from bouncer_account_matrix import restart_controller
                    faults.enter_context(restart_controller(control.with_suffix(".restart"), restart_provider))
                if args.partial_history or args.history_stall:
                    from bouncer_partial_batch_proxy import PartialBatchProxy
                    fault = faults.enter_context(PartialBatchProxy(settings["port"], stall=bool(args.history_stall)))
                    settings["port"] = fault.port
                    environment["REPARTEE_BOUNCER_FAULT_CONTROL"] = fault.control_url
                    if args.history_stall:
                        environment["REPARTEE_BOUNCER_STALL_MODE"] = args.history_stall
                environment.update({
                    "REPARTEE_BOUNCER_TEST_PORT": str(settings["port"]),
                    "REPARTEE_BOUNCER_TEST_NETID": str(settings["network"]),
                    "REPARTEE_BOUNCER_TEST_USER": settings["user"],
                    "REPARTEE_BOUNCER_TEST_PROVIDER": args.implementation,
                    "REPARTEE_BOUNCER_TEST_PASS": args.pass_auth or "0",
                })
                if args.tls_case:
                    environment["REPARTEE_BOUNCER_TLS_CASE"] = args.tls_case
                    authority = "unrelated" if args.tls_case == "untrusted" else "trusted"
                    environment["REPARTEE_OAUTH_TEST_CA"] = str(temporary / f"{authority}-ca.pem")
                test_args = f"{args.test_filter} -- --ignored"
                if args.test_filter == "pinned_bouncer_":
                    test_args += " --skip pinned_bouncer_account_matrix --skip pinned_bouncer_stalled_history --skip pinned_bouncer_partial_history --skip pinned_bouncer_bounded_history --skip pinned_bouncer_tls_validation --skip pinned_bouncer_discovery_limit --skip pinned_bouncer_service --skip pinned_bouncer_presence --skip pinned_bouncer_network_management --skip pinned_bouncer_setname --skip pinned_bouncer_monitor --skip pinned_bouncer_no_monitor --skip pinned_bouncer_invites --skip pinned_bouncer_names --skip pinned_bouncer_channel_context --skip pinned_bouncer_network_icon"
                if args.history_range:
                    from probe_bouncer_history_ranges import probe
                    print(json.dumps(probe(settings, args.implementation), indent=2))
                elif args.daemon_image:
                    run(["python3", str(ROOT / "scripts/test_bouncer_daemon_history.py"), args.daemon_image],
                        cwd=ROOT, env=environment)
                else:
                    run(["make", "test", f"TEST_ARGS={test_args}"],
                        cwd=ROOT, env=environment,
                        timeout=600 if args.history_stall in ("batch-expiry", "request-expiry") else 180)
                print(f"{args.implementation}: {'history-range' if args.history_range else args.test_filter} fixture passed")
            except Exception:
                if args.account_matrix and (temporary / 'matrix-events.jsonl').exists():
                    print((temporary / 'matrix-events.jsonl').read_text())
                log.flush()
                log.seek(0)
                print(log.read())
                raise
            finally:
                if matrix_upstream is not None:
                    matrix_upstream.terminate()
                    try:
                        matrix_upstream.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        matrix_upstream.kill()
                        matrix_upstream.wait(timeout=5)
                if process is not None:
                    process.terminate()
                    try:
                        process.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait(timeout=5)


if __name__ == "__main__":
    main()
