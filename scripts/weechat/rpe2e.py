# -*- coding: utf-8 -*-
#
# rpe2e.py — RPE2E v1.0 end-to-end encryption for WeeChat
#
# Copyright (c) 2026 repartee authors. MIT licensed.
#
# Wire-compatible with the native repartee implementation and the irssi
# rpe2e.pl script. See docs/plans/2026-04-10-e2e-encryption-architecture.md
# for the protocol specification.
#
# Dependencies:
#   pip install pynacl
#
# Install:
#   cp scripts/weechat/rpe2e.py ~/.weechat/python/autoload/
#   /python load rpe2e.py
#   /e2e fingerprint      # show your SAS
#   /e2e on               # enable on the current channel
#

from __future__ import annotations

import base64
import hashlib
import hmac as hmac_mod
import json
import os
import sqlite3
import struct
import time

try:
    import weechat  # type: ignore
except ImportError:
    # Allow running under plain python3 for syntax-check.
    weechat = None  # type: ignore

from nacl.signing import SigningKey, VerifyKey
from nacl.bindings import (
    crypto_aead_xchacha20poly1305_ietf_encrypt,
    crypto_aead_xchacha20poly1305_ietf_decrypt,
    crypto_aead_xchacha20poly1305_ietf_NPUBBYTES,
    crypto_aead_xchacha20poly1305_ietf_KEYBYTES,
    crypto_sign_BYTES,
)
from nacl.public import PrivateKey as X25519Priv, PublicKey as X25519Pub
from nacl.utils import random as nacl_random

SCRIPT_NAME = "rpe2e"
SCRIPT_AUTHOR = "repartee"
SCRIPT_VERSION = "0.1.0"
SCRIPT_LICENSE = "MIT"
SCRIPT_DESC = "RPE2E v1.0 end-to-end encryption (wire-compatible with repartee/irssi)"

# ── Protocol constants ────────────────────────────────────────────────────────
PROTO = "RPE2E01"
WIRE_PREFIX = "+RPE2E01"
CTCP_TAG = "RPEE2E"
MAX_CHUNKS = 16
MAX_PT_PER_CHUNK = 180
TS_TOLERANCE = 300
KEYREQ_MIN_INTERVAL = 30
HKDF_SALT = b"RPE2E01-WRAP"
NONCE_LEN = crypto_aead_xchacha20poly1305_ietf_NPUBBYTES
KEY_LEN = crypto_aead_xchacha20poly1305_ietf_KEYBYTES

# ── DB path ───────────────────────────────────────────────────────────────────

if weechat is not None:
    DB_DIR = weechat.info_get("weechat_dir", "") or os.path.expanduser("~/.weechat")
    DB_PATH = os.path.join(DB_DIR, "rpe2e.db")
else:
    DB_PATH = os.path.expanduser("~/.weechat/rpe2e.db")

_rate_limit_sent: dict[str, float] = {}


def db_conn() -> sqlite3.Connection:
    conn = sqlite3.connect(DB_PATH)
    conn.execute("PRAGMA journal_mode=WAL")
    return conn


SCHEMA_SQL = """
CREATE TABLE IF NOT EXISTS identity (
    id          INTEGER PRIMARY KEY CHECK (id = 1),
    pk          BLOB NOT NULL,
    sk          BLOB NOT NULL,
    fp          BLOB NOT NULL,
    created_at  INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS peers (
    fp           BLOB PRIMARY KEY,
    pk           BLOB NOT NULL,
    last_handle  TEXT,
    first_seen   INTEGER,
    last_seen    INTEGER,
    status       TEXT DEFAULT 'pending'
);
CREATE TABLE IF NOT EXISTS outgoing (
    channel           TEXT PRIMARY KEY,
    sk                BLOB NOT NULL,
    created_at        INTEGER NOT NULL,
    pending_rotation  INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS incoming (
    handle      TEXT NOT NULL,
    channel     TEXT NOT NULL,
    fp          BLOB NOT NULL,
    sk          BLOB NOT NULL,
    status      TEXT NOT NULL DEFAULT 'pending',
    created_at  INTEGER NOT NULL,
    PRIMARY KEY (handle, channel)
);
CREATE TABLE IF NOT EXISTS channels (
    channel TEXT PRIMARY KEY,
    enabled INTEGER NOT NULL DEFAULT 0,
    mode    TEXT NOT NULL DEFAULT 'normal'
);
"""


def init_db() -> None:
    with db_conn() as c:
        c.executescript(SCHEMA_SQL)


# ── Crypto helpers ────────────────────────────────────────────────────────────


def fingerprint(pk: bytes) -> bytes:
    """SHA-256('RPE2E01-FP:' + pk)[:16]."""
    return hashlib.sha256(b"RPE2E01-FP:" + pk).digest()[:16]


def hkdf_sha256(salt: bytes, ikm: bytes, info: bytes, length: int) -> bytes:
    """RFC 5869 HKDF-SHA256."""
    prk = hmac_mod.new(salt, ikm, hashlib.sha256).digest()
    out = b""
    prev = b""
    counter = 1
    while len(out) < length:
        prev = hmac_mod.new(prk, prev + info + bytes([counter]), hashlib.sha256).digest()
        out += prev
        counter += 1
    return out[:length]


def build_aad(sender: str, channel: str, msgid: bytes, ts: int, part: int, total: int) -> bytes:
    return (
        PROTO.encode()
        + b":"
        + sender.encode()
        + b":"
        + channel.encode()
        + b":"
        + msgid
        + b":"
        + struct.pack(">q", ts)
        + b":"
        + bytes([part])
        + b":"
        + bytes([total])
    )


def aead_encrypt(key: bytes, aad: bytes, pt: bytes) -> tuple[bytes, bytes]:
    nonce = nacl_random(NONCE_LEN)
    ct = crypto_aead_xchacha20poly1305_ietf_encrypt(pt, aad, nonce, key)
    return nonce, ct


def aead_decrypt(key: bytes, nonce: bytes, aad: bytes, ct: bytes) -> bytes | None:
    try:
        return crypto_aead_xchacha20poly1305_ietf_decrypt(ct, aad, nonce, key)
    except Exception:
        return None


def ensure_identity() -> tuple[bytes, bytes, bytes]:
    with db_conn() as c:
        row = c.execute("SELECT pk, sk, fp FROM identity WHERE id = 1").fetchone()
        if row is not None:
            return row[0], row[1], row[2]
        sk_obj = SigningKey.generate()
        pk = bytes(sk_obj.verify_key)
        sk = bytes(sk_obj)
        fp = fingerprint(pk)
        c.execute(
            "INSERT INTO identity VALUES (1, ?, ?, ?, ?)",
            (pk, sk, fp, int(time.time())),
        )
        return pk, sk, fp


# ── Wire format ───────────────────────────────────────────────────────────────


def parse_wire(line: str) -> dict | None:
    if not line.startswith(WIRE_PREFIX):
        return None
    try:
        parts = line.split(" ", 4)
        if len(parts) != 5 or parts[0] != WIRE_PREFIX:
            return None
        msgid_hex, ts_s, parttot, body = parts[1], parts[2], parts[3], parts[4]
        if len(msgid_hex) != 16:
            return None
        part_s, total_s = parttot.split("/", 1)
        part, total = int(part_s), int(total_s)
        if total < 1 or total > MAX_CHUNKS or part < 1 or part > total:
            return None
        nonce_b64, ct_b64 = body.split(":", 1)
        nonce = base64.b64decode(nonce_b64)
        if len(nonce) != NONCE_LEN:
            return None
        ct = base64.b64decode(ct_b64)
        return {
            "msgid": bytes.fromhex(msgid_hex),
            "ts": int(ts_s),
            "part": part,
            "total": total,
            "nonce": nonce,
            "ct": ct,
        }
    except Exception:
        return None


def encode_wire(msgid: bytes, ts: int, part: int, total: int, nonce: bytes, ct: bytes) -> str:
    return (
        f"{WIRE_PREFIX} {msgid.hex()} {ts} {part}/{total} "
        f"{base64.b64encode(nonce).decode()}:{base64.b64encode(ct).decode()}"
    )


def split_plaintext(pt: str) -> list[bytes]:
    """Stateless chunker: splits plaintext into <= MAX_CHUNKS UTF-8-safe pieces."""
    if not pt:
        return [b""]
    b = pt.encode("utf-8")
    chunks: list[bytes] = []
    i = 0
    while i < len(b):
        j = min(i + MAX_PT_PER_CHUNK, len(b))
        # Walk back to a UTF-8 start byte.
        while j > i and (b[j - 1] & 0xC0) == 0x80:
            j -= 1
        if j == i:
            # Single codepoint exceeds chunk budget — should not happen for
            # valid input (max UTF-8 codepoint is 4 bytes, budget is 180).
            raise ValueError("cannot split: UTF-8 codepoint too large")
        chunks.append(b[i:j])
        i = j
        if len(chunks) > MAX_CHUNKS:
            raise ValueError(f"chunk limit: {len(chunks)} > {MAX_CHUNKS}")
    return chunks


# ── Session key management ───────────────────────────────────────────────────


def get_or_generate_outgoing_key(channel: str) -> bytes:
    with db_conn() as c:
        row = c.execute(
            "SELECT sk, pending_rotation FROM outgoing WHERE channel = ?", (channel,)
        ).fetchone()
        if row is not None and not row[1]:
            return row[0]
        fresh = nacl_random(KEY_LEN)
        c.execute(
            "INSERT OR REPLACE INTO outgoing VALUES (?, ?, ?, 0)",
            (channel, fresh, int(time.time())),
        )
        return fresh


# ── WeeChat hooks ─────────────────────────────────────────────────────────────


def hook_irc_in_privmsg(data, modifier, server, msg):
    """Decrypt incoming PRIVMSG before WeeChat renders it."""
    try:
        if not msg.startswith(":"):
            return msg
        prefix_end = msg.index(" ")
        prefix = msg[1:prefix_end]
        rest = msg[prefix_end + 1 :]
        if "!" not in prefix or "@" not in prefix:
            return msg
        nick, userhost = prefix.split("!", 1)
        handle = userhost  # ident@host
        rest_parts = rest.split(" ", 2)
        if len(rest_parts) < 3 or rest_parts[0] != "PRIVMSG":
            return msg
        target = rest_parts[1]
        text = rest_parts[2][1:] if rest_parts[2].startswith(":") else rest_parts[2]

        wire = parse_wire(text)
        if wire is None:
            return msg
        # Replay window check
        skew = abs(int(time.time()) - wire["ts"])
        if skew > TS_TOLERANCE:
            return ""  # drop silently
        with db_conn() as c:
            row = c.execute(
                "SELECT sk, status FROM incoming WHERE handle = ? AND channel = ?",
                (handle, target),
            ).fetchone()
        if row is None or row[1] != "trusted":
            return ""  # unknown peer → drop
        sk = row[0]
        aad = build_aad(handle, target, wire["msgid"], wire["ts"], wire["part"], wire["total"])
        pt = aead_decrypt(sk, wire["nonce"], aad, wire["ct"])
        if pt is None:
            return ""
        try:
            pt_str = pt.decode("utf-8")
        except UnicodeDecodeError:
            pt_str = pt.decode("utf-8", errors="replace")
        return f":{prefix} PRIVMSG {target} :{pt_str}"
    except Exception:
        return msg


def hook_input_text_display(data, modifier, modifier_data, text):
    """Encrypt outbound PRIVMSG before WeeChat sends it.

    We hook `irc_out_privmsg` modifier. The `modifier_data` carries the
    server name. The full line looks like `PRIVMSG #chan :text`.
    """
    try:
        if not text.startswith("PRIVMSG "):
            return text
        # Parse: PRIVMSG <target> :<text>
        _, rest = text.split(" ", 1)
        target, payload = rest.split(" ", 1)
        if not payload.startswith(":"):
            return text
        plain = payload[1:]

        # Only encrypt if the channel is enabled.
        with db_conn() as c:
            row = c.execute(
                "SELECT enabled FROM channels WHERE channel = ?", (target,)
            ).fetchone()
        if row is None or not row[0]:
            return text

        sk = get_or_generate_outgoing_key(target)
        chunks = split_plaintext(plain)
        total = len(chunks)
        msgid = nacl_random(8)
        ts = int(time.time())

        # We need to resolve our own handle (ident@host) from the server info.
        # WeeChat exposes it via /info "irc_nick_modes" but not directly —
        # fall back to $nick!unknown@unknown so AAD is still well-formed.
        server = modifier_data
        my_nick = weechat.info_get("irc_nick", server) if weechat else "me"
        my_handle = f"{my_nick}!unknown@unknown"

        # Send each chunk as its own PRIVMSG line.
        wire_lines = []
        for idx, chunk in enumerate(chunks, start=1):
            aad = build_aad(my_handle, target, msgid, ts, idx, total)
            nonce, ct = aead_encrypt(sk, aad, chunk)
            wire_lines.append(encode_wire(msgid, ts, idx, total, nonce, ct))

        # Replace the single PRIVMSG with the first chunk; queue the rest.
        first = f"PRIVMSG {target} :{wire_lines[0]}"
        for extra in wire_lines[1:]:
            if weechat is not None:
                weechat.command(
                    weechat.buffer_search("irc", f"{server}.{target}"),
                    f"/quote PRIVMSG {target} :{extra}",
                )
        return first
    except Exception:
        return text


# ── Commands ──────────────────────────────────────────────────────────────────


def cmd_e2e(data, buffer, args):
    parts = args.split()
    sub = parts[0] if parts else ""
    rest = parts[1:]

    if sub in ("", "help"):
        if weechat:
            weechat.prnt(
                "",
                "Usage: /e2e <on|off|mode|fingerprint|list|status|accept|revoke|forget|rotate>",
            )
    elif sub == "fingerprint":
        pk, sk, fp = ensure_identity()
        if weechat:
            weechat.prnt("", f"[E2E] fingerprint: {fp.hex()}")
    elif sub == "status":
        with db_conn() as c:
            n = c.execute("SELECT COUNT(*) FROM incoming").fetchone()[0]
            m = c.execute("SELECT COUNT(*) FROM channels WHERE enabled=1").fetchone()[0]
            id_row = c.execute("SELECT fp FROM identity WHERE id=1").fetchone()
        fp = id_row[0].hex() if id_row else "(none)"
        if weechat:
            weechat.prnt("", f"[E2E] identity={fp} peers={n} enabled_channels={m}")
    elif sub == "on":
        channel = weechat.buffer_get_string(buffer, "localvar_channel") if weechat else ""
        if not channel:
            if weechat:
                weechat.prnt("", "/e2e on: no active channel")
            return weechat.WEECHAT_RC_OK if weechat else 0
        with db_conn() as c:
            c.execute(
                "INSERT OR REPLACE INTO channels VALUES (?, 1, 'normal')", (channel,)
            )
        if weechat:
            weechat.prnt(buffer, f"[E2E] enabled on {channel}")
    elif sub == "off":
        channel = weechat.buffer_get_string(buffer, "localvar_channel") if weechat else ""
        if channel:
            with db_conn() as c:
                c.execute("UPDATE channels SET enabled=0 WHERE channel=?", (channel,))
            if weechat:
                weechat.prnt(buffer, f"[E2E] disabled on {channel}")
    elif sub == "mode":
        channel = weechat.buffer_get_string(buffer, "localvar_channel") if weechat else ""
        if not channel or not rest:
            if weechat:
                weechat.prnt("", "Usage: /e2e mode <auto-accept|normal|quiet>")
            return weechat.WEECHAT_RC_OK if weechat else 0
        mode = rest[0]
        if mode not in ("auto-accept", "normal", "quiet"):
            if weechat:
                weechat.prnt("", f"[E2E] invalid mode: {mode}")
            return weechat.WEECHAT_RC_OK if weechat else 0
        with db_conn() as c:
            c.execute(
                "INSERT OR REPLACE INTO channels VALUES (?, 1, ?)", (channel, mode)
            )
        if weechat:
            weechat.prnt(buffer, f"[E2E] mode={mode} on {channel}")
    elif sub == "rotate":
        channel = weechat.buffer_get_string(buffer, "localvar_channel") if weechat else ""
        if channel:
            with db_conn() as c:
                c.execute(
                    "UPDATE outgoing SET pending_rotation=1 WHERE channel=?", (channel,)
                )
            if weechat:
                weechat.prnt(buffer, f"[E2E] rotation scheduled for {channel}")
    elif sub == "list":
        with db_conn() as c:
            rows = c.execute(
                "SELECT handle, channel, fp, status FROM incoming"
            ).fetchall()
        if not rows and weechat:
            weechat.prnt("", "[E2E] no peers")
        else:
            for r in rows:
                if weechat:
                    weechat.prnt(
                        "",
                        f"[E2E] {r[0]} on {r[1]}  fp={r[2].hex()[:16]}  status={r[3]}",
                    )
    elif sub == "accept":
        if not rest:
            if weechat:
                weechat.prnt("", "Usage: /e2e accept <nick>")
            return weechat.WEECHAT_RC_OK if weechat else 0
        nick = rest[0]
        channel = weechat.buffer_get_string(buffer, "localvar_channel") if weechat else ""
        with db_conn() as c:
            c.execute(
                "UPDATE incoming SET status='trusted' WHERE handle LIKE ? AND channel = ?",
                (f"{nick}%", channel),
            )
        if weechat:
            weechat.prnt(buffer, f"[E2E] accepted {nick} on {channel}")
    elif sub == "revoke":
        if not rest:
            return weechat.WEECHAT_RC_OK if weechat else 0
        nick = rest[0]
        channel = weechat.buffer_get_string(buffer, "localvar_channel") if weechat else ""
        with db_conn() as c:
            c.execute(
                "UPDATE incoming SET status='revoked' WHERE handle LIKE ? AND channel = ?",
                (f"{nick}%", channel),
            )
            c.execute(
                "UPDATE outgoing SET pending_rotation=1 WHERE channel=?", (channel,)
            )
        if weechat:
            weechat.prnt(buffer, f"[E2E] revoked {nick} on {channel} — key will rotate")
    elif sub == "forget":
        if not rest:
            return weechat.WEECHAT_RC_OK if weechat else 0
        nick = rest[0]
        with db_conn() as c:
            c.execute("DELETE FROM incoming WHERE handle LIKE ?", (f"{nick}%",))
        if weechat:
            weechat.prnt("", f"[E2E] forgot {nick}")

    return weechat.WEECHAT_RC_OK if weechat else 0


# ── Registration ──────────────────────────────────────────────────────────────


def main() -> None:
    if weechat is None:
        return
    weechat.register(
        SCRIPT_NAME,
        SCRIPT_AUTHOR,
        SCRIPT_VERSION,
        SCRIPT_LICENSE,
        SCRIPT_DESC,
        "",
        "",
    )
    init_db()
    ensure_identity()
    weechat.hook_modifier("irc_in_privmsg", "hook_irc_in_privmsg", "")
    weechat.hook_modifier("irc_out_privmsg", "hook_input_text_display", "")
    weechat.hook_command(
        "e2e",
        SCRIPT_DESC,
        "<on|off|mode|fingerprint|list|status|accept|revoke|forget|rotate> [args]",
        "Manage RPE2E end-to-end encryption",
        "on || off || mode auto-accept|normal|quiet || fingerprint || list || status"
        " || accept %(irc_channel_nicks) || revoke %(irc_channel_nicks)"
        " || forget %(irc_channel_nicks) || rotate",
        "cmd_e2e",
        "",
    )
    weechat.prnt("", f"[rpe2e] loaded v{SCRIPT_VERSION}. /e2e fingerprint to view your SAS.")


if __name__ == "__main__" or weechat is not None:
    main()
