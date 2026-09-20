import argparse
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile

from test_bouncer_binding import PINS, ROOT, run, wait_ready


def scenario(implementation, source, auto_away, setname=False, monitor=False, monitor_unavailable=False, invites=False, names=False):
    with tempfile.TemporaryDirectory(prefix="bouncer-presence-", dir="/tmp") as directory:
        temporary = Path(directory)
        processes = []
        with (temporary / "server.log").open("w+") as log:
            try:
                ready = temporary / "upstream.json"
                events = temporary / "events.jsonl"
                upstream = subprocess.Popen([
                    sys.executable, str(ROOT / "scripts/fixtures/presence-upstream.py"),
                    str(ready), str(events), *(["--setname"] if setname else []),
                    *(["--monitor"] if monitor or monitor_unavailable else []),
                    *(["--monitor-unavailable"] if monitor_unavailable else []),
                    *(["--invites"] if invites else []),
                    *(["--names"] if names else []),
                ], stdout=log, stderr=log)
                processes.append(upstream)
                wait_ready(upstream, ready.exists)
                upstream_port = json.loads(ready.read_text())["port"]
                if implementation == "lurker":
                    bouncer_ready = temporary / "bouncer.json"
                    bouncer = subprocess.Popen([
                        "node", str(source / "node_modules/tsx/dist/cli.mjs"),
                        str(ROOT / "scripts/fixtures/lurker-presence.mts"),
                        str(source), str(bouncer_ready), str(upstream_port),
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
                        f"listen irc+insecure://127.0.0.1:{port}\n"
                        f"listen unix+admin://{admin}\nmessage-store db\n"
                    )
                    run([str(source / "sojudb"), "-config", str(config), "create-user", "fixture"],
                        input="fixture-password\n", capture_output=True)
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
    parser.add_argument("--names", action="store_true")
    args = parser.parse_args()
    source = args.source.resolve()
    head = run(["git", "-C", str(source), "rev-parse", "HEAD"], capture_output=True).stdout.strip()
    if head != PINS[args.implementation]:
        raise RuntimeError("Upstream checkout does not match the audited revision")
    scenario(args.implementation, source, True, args.setname, args.monitor, args.monitor_unavailable, args.invites, args.names)
    if args.implementation == "soju" and not args.setname and not args.monitor and not args.monitor_unavailable and not args.invites and not args.names:
        scenario(args.implementation, source, False)


if __name__ == "__main__":
    main()
