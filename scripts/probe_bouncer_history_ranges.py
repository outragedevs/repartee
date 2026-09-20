"""Verify BETWEEN wire semantics against disposable, seeded bouncer instances."""

import base64
import datetime
import socket


def parse(line):
    tags = {}
    if line.startswith("@"):
        raw_tags, line = line[1:].split(" ", 1)
        tags = dict(tag.partition("=")[::2] for tag in raw_tags.split(";"))
    if line.startswith(":"):
        _, line = line.split(" ", 1)
    head, separator, trailing = line.partition(" :")
    parts = head.split()
    if separator:
        parts.append(trailing)
    return tags, parts[0], parts[1:]


def timestamp(second):
    instant = datetime.datetime(2024, 1, 1, tzinfo=datetime.timezone.utc)
    instant += datetime.timedelta(seconds=second)
    return "timestamp=" + instant.isoformat(timespec="milliseconds").replace("+00:00", "Z")


def probe(settings, provider):
    results = []
    with socket.create_connection(("127.0.0.1", settings["port"]), timeout=15) as connection:
        with connection.makefile("r", newline="\n") as stream:
            def send(line):
                connection.sendall((line + "\r\n").encode())

            def receive():
                while True:
                    line = stream.readline()
                    if not line:
                        raise RuntimeError("Bouncer closed the range fixture connection")
                    message = parse(line.rstrip("\r\n"))
                    if message[1] == "PING":
                        send("PONG :" + message[2][-1])
                    else:
                        return message

            def until(command):
                while True:
                    message = receive()
                    if message[1] in ("FAIL", "ERROR", "464", "904", "905", "906"):
                        raise AssertionError(message)
                    if message[1] == command:
                        return message

            send("CAP LS 302")
            send("NICK fixture")
            send(f"USER {settings['user']} 0 * :Fixture")
            while True:
                _, _, params = until("CAP")
                if params[1] == "LS" and params[2] != "*":
                    break
            send("CAP REQ :sasl message-tags batch server-time draft/chathistory soju.im/bouncer-networks")
            assert until("CAP")[2][1] == "ACK"
            send("AUTHENTICATE PLAIN")
            assert until("AUTHENTICATE")[2] == ["+"]
            credentials = f"\0{settings['user']}\0fixture-password".encode()
            send("AUTHENTICATE " + base64.b64encode(credentials).decode())
            until("903")
            send(f"BOUNCER BIND {settings['network']}")
            send("CAP END")
            registered_network = False
            reference_types = None
            while True:
                _, command, params = receive()
                assert command not in ("FAIL", "ERROR", "464"), (command, params)
                if command == "005":
                    for token in params:
                        if token.startswith("MSGREFTYPES="):
                            reference_types = token.split("=", 1)[1]
                    registered_network |= f"BOUNCER_NETID={settings['network']}" in params
                if command in ("376", "422"):
                    break
            assert registered_network
            assert reference_types == "timestamp", reference_types

            def query(target, first, last, limit, rejection=None):
                send(f"CHATHISTORY BETWEEN {target} {first} {last} {limit}")
                batch = None
                rows = []
                refs = []
                while True:
                    tags, command, params = receive()
                    if command == "FAIL":
                        assert batch is None and not rows
                        assert rejection is not None, params
                        assert params[:3] == ["CHATHISTORY", rejection, "BETWEEN"], params
                        return None, []
                    assert command not in ("ERROR", "421", "461"), (command, params)
                    if command == "BATCH" and params[0].startswith("+"):
                        assert batch is None and params[1:] == ["chathistory", target], params
                        batch = params[0][1:]
                    elif command == "BATCH" and params[0].startswith("-"):
                        assert batch is not None and params == ["-" + batch], params
                        assert rejection is None, ("Expected rejection", rejection, rows)
                        return rows, refs
                    elif tags.get("batch") == batch and batch is not None:
                        assert command == "PRIVMSG", (command, params)
                        assert params[-1].startswith("fixture-history-"), params
                        rows.append(int(params[-1].removeprefix("fixture-history-")))
                        refs.append(tags.get("msgid"))

            cases = [
                ("ascending", 10, 20, 3, [11, 12, 13]),
                ("descending", 20, 10, 3, [17, 18, 19]),
                ("complete", 10, 20, 100, list(range(11, 20))),
                ("equal", 10, 10, 100, []),
                ("empty", 400, 500, 100, []),
            ]
            for target in ("history-peer", "#history-channel"):
                for name, first, last, limit, expected in cases:
                    rows, _ = query(target, timestamp(first), timestamp(last), limit)
                    assert rows == expected, (provider, target, name, rows, expected)
                    results.append({"target": target, "case": name, "rows": rows})
                rows, refs = query(target, timestamp(9), timestamp(21), 100)
                assert rows == list(range(10, 21)), rows
                if provider == "lurker":
                    assert all(refs), refs
                else:
                    refs = ["fixture-first", "fixture-last"]
                rejection = "INVALID_MSGREFTYPE" if provider == "lurker" else "INVALID_PARAMS"
                rows, _ = query(target, "msgid=" + refs[0], "msgid=" + refs[-1], 3, rejection)
                assert rows is None
                results.append({"target": target, "case": "msgid", "rejection": rejection})
                for first, last in (("*", timestamp(20)), (timestamp(10), "*")):
                    rows, _ = query(target, first, last, 3, "INVALID_PARAMS")
                    assert rows is None, (provider, "invalid-bound", rows)
                results.append({"target": target, "case": "invalid-bounds", "rejected": 2})
    return {"provider": provider, "cases": results}
