import argparse
import json
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
                    settings = {"port": port, "network": 1, "user": "fixture"}
                environment = os.environ.copy()
                environment.update({
                    "REPARTEE_BOUNCER_TEST_PORT": str(settings["port"]),
                    "REPARTEE_BOUNCER_TEST_NETID": str(settings["network"]),
                    "REPARTEE_BOUNCER_TEST_USER": settings["user"],
                })
                run(["make", "test", "TEST_ARGS=pinned_bouncer_registration -- --ignored"],
                    cwd=ROOT, env=environment)
                print(f"{args.implementation}: explicit network binding and reconnect passed")
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
