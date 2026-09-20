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
    return subprocess.run(command, check=True, text=True, timeout=180, **kwargs)


def wait_ready(process, predicate):
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise RuntimeError("Bouncer fixture exited before becoming ready")
        if predicate():
            return
        time.sleep(0.05)
    raise TimeoutError("Bouncer fixture did not become ready")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("implementation", choices=PINS)
    parser.add_argument("source", type=Path)
    parser.add_argument("--test-filter", default="pinned_bouncer_")
    args = parser.parse_args()
    source = args.source.resolve()
    head = run(["git", "-C", str(source), "rev-parse", "HEAD"], capture_output=True).stdout.strip()
    if head != PINS[args.implementation]:
        raise RuntimeError("Upstream checkout does not match the audited revision")
    with tempfile.TemporaryDirectory(prefix="bouncer-binding-", dir="/tmp") as directory:
        temporary = Path(directory)
        process = None
        with (temporary / "server.log").open("w+") as log:
            try:
                if args.implementation == "lurker":
                    ready = temporary / "ready.json"
                    process = subprocess.Popen([
                        "node", str(source / "node_modules/tsx/dist/cli.mjs"),
                        str(ROOT / "scripts/fixtures/lurker-binding.mts"), str(source), str(ready),
                    ], cwd=source, stdout=log, stderr=log)
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
                        f"listen irc+insecure://127.0.0.1:{port}\n"
                        f"listen unix+admin://{admin}\nmessage-store db\n"
                    )
                    run([str(source / "sojudb"), "-config", str(config), "create-user", "fixture"],
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
                    settings = {"port": port, "network": 1, "user": "fixture"}
                environment = os.environ.copy()
                environment.update({
                    "REPARTEE_BOUNCER_TEST_PORT": str(settings["port"]),
                    "REPARTEE_BOUNCER_TEST_NETID": str(settings["network"]),
                    "REPARTEE_BOUNCER_TEST_USER": settings["user"],
                    "REPARTEE_BOUNCER_TEST_PROVIDER": args.implementation,
                })
                test_args = f"{args.test_filter} -- --ignored"
                if args.test_filter == "pinned_bouncer_":
                    test_args += " --skip pinned_bouncer_presence --skip pinned_bouncer_network_management --skip pinned_bouncer_setname --skip pinned_bouncer_monitor --skip pinned_bouncer_no_monitor --skip pinned_bouncer_invites --skip pinned_bouncer_names"
                run(["make", "test", f"TEST_ARGS={test_args}"],
                    cwd=ROOT, env=environment)
                print(f"{args.implementation}: {args.test_filter} fixture passed")
            except Exception:
                log.flush()
                log.seek(0)
                print(log.read())
                raise
            finally:
                if process is not None:
                    process.terminate()
                    try:
                        process.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait(timeout=5)


if __name__ == "__main__":
    main()
