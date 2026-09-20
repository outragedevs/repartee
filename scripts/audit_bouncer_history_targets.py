"""Reproduce the pinned Soju TARGETS timestamp-tie acceptance gap."""

import argparse
import json
import socket
import sqlite3
import subprocess
import tempfile
from pathlib import Path

from test_bouncer_binding import PINS, run, wait_ready


def seed(database_path):
    with sqlite3.connect(database_path) as database:
        for index in range(1001):
            target = f"peer-{index:04}"
            cursor = database.execute(
                "INSERT INTO MessageTarget(network, target) VALUES (1, ?)", (target,)
            )
            timestamp = "2024-01-01T00:00:00.000Z"
            raw = f"@time={timestamp} :{target}!u@fixture PRIVMSG fixture :tie"
            database.execute(
                "INSERT INTO Message(target, raw, time, sender, text) VALUES (?, ?, ?, ?, ?)",
                (cursor.lastrowid, raw, timestamp, target, "tie"),
            )


def probe(port):
    with socket.create_connection(("127.0.0.1", port), timeout=10) as connection:
        with connection.makefile("r", newline="\n") as stream:
            def send(line):
                connection.sendall((line + "\r\n").encode())

            def receive_until(predicate):
                rows = []
                while True:
                    line = stream.readline()
                    if not line:
                        raise RuntimeError("Bouncer closed the fixture connection")
                    line = line.strip()
                    rows.append(line)
                    if line.startswith("PING "):
                        send("PONG " + line[5:])
                    if " FAIL " in line or " 464 " in line:
                        raise RuntimeError(f"Fixture protocol rejection: {line}")
                    if predicate(line):
                        return rows

            send("CAP LS 302")
            send("PASS fixture-password")
            send("NICK fixture")
            send("USER fixture/fixture 0 * :Fixture")
            receive_until(lambda line: " CAP " in line and " LS " in line)
            send("CAP REQ :batch server-time draft/chathistory")
            replies = receive_until(lambda line: " ACK " in line or " NAK " in line)
            assert " ACK " in replies[-1], replies[-1]
            send("CAP END")
            registration = receive_until(lambda line: " 376 " in line or " 422 " in line)
            assert any("CHATHISTORY=1000" in line for line in registration)
            assert any("BOUNCER_NETID=1" in line for line in registration)
            seen = set()
            pages = []
            for upper in (
                "2025-01-01T00:00:00.000Z",
                "2024-01-01T00:00:00.001Z",
                "2024-01-01T00:00:00.000Z",
            ):
                send(f"CHATHISTORY TARGETS timestamp={upper} timestamp=1970-01-01T00:00:00.000Z 1000")
                replies = receive_until(
                    lambda line: line.startswith("BATCH -") or " BATCH -" in line
                )
                targets = [
                    line.split(" CHATHISTORY TARGETS ", 1)[1].split()[0]
                    for line in replies if " CHATHISTORY TARGETS " in line
                ]
                seen.update(targets)
                pages.append({"upper": upper, "rows": len(targets), "unique_so_far": len(seen)})
            assert [page["rows"] for page in pages] == [1000, 1000, 0], pages
            assert len(seen) == 1000, len(seen)
            return {"provider": "soju", "seeded_targets": 1001, "pages": pages, "missing": 1}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path)
    args = parser.parse_args()
    source = args.source.resolve()
    head = run(["git", "-C", str(source), "rev-parse", "HEAD"], capture_output=True).stdout.strip()
    if head != PINS["soju"]:
        raise RuntimeError("Soju checkout does not match the audited revision")
    with tempfile.TemporaryDirectory(prefix="bouncer-target-tie-", dir="/tmp") as directory:
        temporary = Path(directory)
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
        with (temporary / "server.log").open("w+") as log:
            process = subprocess.Popen([str(source / "soju"), "-config", str(config)],
                                       stdout=log, stderr=log)
            try:
                wait_ready(process, admin.exists)
                run([str(source / "sojuctl"), "-config", str(config), "user", "run", "fixture",
                     "network", "create", "-name", "fixture", "-addr", "irc+insecure://127.0.0.1:1",
                     "-enabled", "false"], capture_output=True)
                seed(temporary / "main.db")
                print(json.dumps(probe(port), indent=2))
            finally:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)


if __name__ == "__main__":
    main()
