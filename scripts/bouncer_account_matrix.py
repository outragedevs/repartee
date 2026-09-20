import datetime
import sqlite3

from test_bouncer_binding import run


def seed_soju(source, config, database_path):
    for user, name in [("fixture", "second"), ("other", "fixture"), ("other", "second")]:
        run([str(source / "sojuctl"), "-config", str(config), "user", "run", user,
             "network", "create", "-name", name, "-addr", "irc+insecure://127.0.0.1:1",
             "-enabled", "false"], capture_output=True)
    accounts = [{"user": "fixture", "networks": [1, 2]}, {"user": "other", "networks": [3, 4]}]
    with sqlite3.connect(database_path) as database:
        database.execute("DELETE FROM Message")
        database.execute("DELETE FROM MessageTarget")
        for account, owner in enumerate(accounts):
            for network in owner["networks"]:
                for target in ("history-peer", "#history-channel"):
                    cursor = database.execute("INSERT INTO MessageTarget(network, target) VALUES (?, ?)", (network, target))
                    for index in range(300):
                        timestamp = (datetime.datetime(2024, 1, 1, tzinfo=datetime.timezone.utc) + datetime.timedelta(seconds=index)).isoformat(timespec="milliseconds").replace("+00:00", "Z")
                        body = f"matrix-{account}-{network}-{index}"
                        recipient = target if target.startswith("#") else owner["user"]
                        raw = f"@time={timestamp} :history-peer!user@fixture.local PRIVMSG {recipient} :{body}"
                        database.execute("INSERT INTO Message(target, raw, time, sender, text) VALUES (?, ?, ?, ?, ?)",
                                         (cursor.lastrowid, raw, timestamp, "history-peer", body))
    return accounts


class SojuController:
    def __init__(self, source, config, database, control, accounts):
        import threading
        self.source, self.config, self.database, self.control, self.accounts = source, config, database, control, accounts
        self.stop = threading.Event()
        self.thread = threading.Thread(target=self.loop, daemon=True)

    def __enter__(self):
        self.thread.start()
        return self

    def __exit__(self, *args):
        self.stop.set()
        self.thread.join(timeout=5)
        if self.thread.is_alive():
            raise RuntimeError("Matrix controller did not stop")

    def loop(self):
        import json
        while not self.stop.wait(0.02):
            if not self.control.exists():
                continue
            try:
                request = json.loads(self.control.read_text())
                self.control.unlink()
                user = self.accounts[request["account"]]["user"]
                action = request["action"]
                if action == "online":
                    command = ["network", "update", request["name"], "-addr", f"irc+insecure://127.0.0.1:{request['port']}",
                               "-nick", request["nick"], "-auto-away", "true", "-enabled", "true"]
                elif action == "offline":
                    command = ["network", "update", request["name"], "-enabled", "false"]
                elif action == "rename":
                    command = ["network", "update", request["name"], "-name", "renamed"]
                elif action == "delete":
                    command = ["network", "delete", "renamed"]
                elif action == "create":
                    command = ["network", "create", "-name", "renamed", "-addr", f"irc+insecure://127.0.0.1:{request['port']}",
                               "-nick", "matrix-recreated", "-auto-away", "true", "-enabled", "true"]
                else:
                    raise ValueError("Unknown matrix action")
                run([str(self.source / "sojuctl"), "-config", str(self.config), "user", "run", user, *command], capture_output=True, timeout=10)
                with sqlite3.connect(self.database) as database:
                    network = database.execute("SELECT MAX(id) FROM Network").fetchone()[0]
                response = {"network": network}
            except Exception as error:
                response = {"error": str(error)}
            output = self.control.with_suffix(".response")
            pending = output.with_suffix(".pending")
            pending.write_text(json.dumps(response))
            pending.replace(output)


from contextlib import contextmanager


@contextmanager
def restart_controller(control, restart):
    import json
    import threading
    stop = threading.Event()

    def loop():
        while not stop.wait(0.02):
            if not control.exists():
                continue
            control.unlink()
            try:
                restart()
                result = {"ok": True}
            except Exception as error:
                result = {"error": str(error)}
            pending = control.with_suffix(".pending")
            pending.write_text(json.dumps(result))
            pending.replace(control.with_suffix(".restarted"))

    thread = threading.Thread(target=loop, daemon=True)
    thread.start()
    try:
        yield
    finally:
        stop.set()
        thread.join(timeout=20)
        if thread.is_alive():
            raise RuntimeError("Provider restart did not finish")
