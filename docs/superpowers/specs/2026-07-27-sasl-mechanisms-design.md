# Complete SASL mechanism coverage

**Date:** 2026-07-27
**Branch:** `feat/sasl-mechanisms`
**Scope:** `src/irc/sasl_scram.rs`, `src/irc/sasl_ecdsa.rs` (new), `src/irc/mod.rs`,
`src/irc/cap.rs`, `src/config/mod.rs`, `src/config/env.rs`, `src/commands/settings.rs`,
`src/ui/wizard/server.rs`, `web-ui/src/components/wizard.rs`, `docs/`

## Goal

Every one of these SASL mechanisms works end to end:

| Mechanism | Today | After |
|---|---|---|
| `PLAIN` | works | works (+ SASLprep) |
| `EXTERNAL` | works | works |
| `SCRAM-SHA-1` | **absent** | works |
| `SCRAM-SHA-256` | works | works (+ SASLprep, chunk reassembly) |
| `SCRAM-SHA-512` | **absent** | works |
| `ECDSA-NIST256P-CHALLENGE` | **absent** | works |

## What exists today

`src/irc/mod.rs` owns the negotiation: `select_sasl_mechanism` picks one of
`SaslMechanism::{Plain, External, ScramSha256}`, and `negotiate_caps` runs
`run_sasl_plain` / `run_sasl_external` / `run_sasl_scram` after `CAP ACK sasl`.
`src/irc/sasl_scram.rs` is a SHA-256-only SCRAM client: `client_first`,
`client_final`, `verify_server`, `chunk_authenticate`.

Four gaps beyond the three missing mechanisms:

1. **Inbound `AUTHENTICATE` is never reassembled.** `chunk_authenticate` splits
   *outgoing* payloads at 400 bytes, but the SCRAM reader takes the first
   `AUTHENTICATE <param>` as the entire payload. A server-first message split
   across two frames decodes as truncated base64 and the exchange dies. Real
   SCRAM-SHA-512 server-first messages with a long salt get close to the limit,
   so this stops being theoretical as soon as SHA-512 is on.
2. **`sasl` advertised without a value is guessed as `PLAIN`.**
   `ServerCaps::sasl_mechanisms()` returns `["PLAIN"]` when the server sends a
   bare `sasl` cap, which is indistinguishable from a server that genuinely
   offers only `PLAIN`. A user who configures `SCRAM-SHA-512` against such a
   server gets SASL silently dropped.
3. **No SASLprep.** RFC 5802 §5.1 and RFC 4616 require SASLprep on username and
   password. Identity for printable ASCII, so nothing shipped is affected — but
   a non-ASCII password computes a different `SaltedPassword` than the server's.
4. **`client_cert_path` is the only key-ish field.** It is handed to the `irc`
   crate for the TLS handshake (`EXTERNAL`/CertFP). ECDSA needs a *separate*
   private key that is never presented to TLS.

## Design

### 1. SCRAM parameterised by hash

`sasl_scram.rs` gains

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScramHash { Sha1, Sha256, Sha512 }
```

and every public function takes it as its first argument. The maths is written
once, generically:

```rust
fn client_final_with<D>(...) -> Result<(String, Vec<u8>)>
where D: Digest + BlockSizeUser + Clone + Sync
```

using `hmac::SimpleHmac<D>` (bounds `D: Digest + BlockSizeUser`, unlike
`Hmac<D>`'s `CoreProxy` chain) and `pbkdf2::pbkdf2::<SimpleHmac<D>>` rather than
`pbkdf2_hmac::<D>`, so the whole where-clause is those four traits. `ScramHash`
dispatches to `client_final_with::<Sha1 | Sha256 | Sha512>`.

The three hashes differ only in `SaltedPassword`/`ClientKey`/`StoredKey` width
(20 / 32 / 64 bytes); the wire format is identical, so no per-hash message code
exists.

`MAX_ITERATIONS` stays 100 000 for every hash. It is a denial-of-service bound
on work the *server* asks us to do, not a security parameter, and SHA-512 at
100 000 iterations is already the slowest case.

**`-PLUS` variants are out of scope.** We do not implement channel binding, so
we must never select `SCRAM-SHA-256-PLUS` merely because it looks like a match.
Mechanism comparison is whole-token `eq_ignore_ascii_case`, which already
rejects it; a test pins that.

### 2. SASLprep

A `saslprep` helper in `sasl_scram.rs`, used by SCRAM (username + password) and
by `run_sasl_plain` (both):

```rust
pub fn saslprep(value: &str) -> String
```

It applies `stringprep::saslprep` and, **on failure, returns the input
unchanged with a `tracing::warn!`**. Erroring instead would turn a credential
that authenticates today into a hard failure on a server that does not prep
either. Prepping is done before SCRAM's `=`/`,` escaping.

### 3. Inbound `AUTHENTICATE` reassembly

One reader for all challenge-response mechanisms:

```rust
async fn await_authenticate_payload(
    stream: &mut irc::client::ClientStream,
) -> Result<Vec<u8>>
```

It accumulates `AUTHENTICATE <chunk>` frames until a chunk shorter than 400
bytes arrives (per the IRCv3 SASL spec, a 400-byte chunk means "more follows";
a bare `+` terminates an exchange whose last chunk was exactly 400 bytes), then
base64-decodes the concatenation. `+` alone decodes to an empty payload. It
carries the same 30 s timeout and 904/905/906/907 handling as
`await_authenticate_plus`, plus a cap on total accumulated length so a hostile
server cannot stream chunks forever.

`await_authenticate_plus` is kept for the mechanisms that only need the empty
challenge (`PLAIN`, `EXTERNAL`) — those want to reject a non-`+` payload rather
than decode it.

### 4. ECDSA-NIST256P-CHALLENGE

New module `src/irc/sasl_ecdsa.rs`, and a matching `run_sasl_ecdsa` in
`src/irc/mod.rs`. The exchange, as implemented by atheme's
`ecdsa-nist256p-challenge` module and WeeChat's client:

1. `AUTHENTICATE ECDSA-NIST256P-CHALLENGE`
2. server → `AUTHENTICATE +`
3. client → base64(`<authzid>\0<authcid>`), both set to `sasl_user` — the same
   shape `PLAIN` already sends, minus the password
4. server → `AUTHENTICATE <base64 challenge>` (32 random bytes)
5. client → base64(DER ECDSA signature over those bytes)
6. server → `903` / `904`

The signature is computed **over the challenge bytes as a prehash** — no extra
hashing. Both reference implementations call `ECDSA_sign(0, challenge, len, …)`
/ `ECDSA_verify(0, challenge, len, …)`, which treat the input as an already-
computed digest. Signing with `p256::ecdsa::SigningKey` means
`signature::hazmat::PrehashSigner`, and the output must be DER
(`Signature::to_der()`), because OpenSSL's `ECDSA_sign` emits DER and the server
parses it as such. The fixed 64-byte form would be rejected.

Key loading accepts both PEM encodings, because `ecdsatool` emits SEC1 and
OpenSSL 3 emits PKCS#8:

```rust
pub fn load_key(pem: &str) -> Result<SigningKey>   // SEC1 "EC PRIVATE KEY", then PKCS#8
```

Errors name the file and say which formats are accepted; the key material never
reaches a log line or an error message.

A P-256 challenge shorter than 16 bytes is rejected with a clear error rather
than left to `bits2field`'s opaque failure.

### 5. Configuration

One new field on `ServerConfig`:

```rust
/// Path to a PEM ECDSA (NIST P-256) private key for
/// SASL ECDSA-NIST256P-CHALLENGE. Distinct from `client_cert_path`, which is
/// the TLS client certificate used by EXTERNAL/CertFP.
#[serde(default, skip_serializing_if = "Option::is_none")]
pub sasl_key_path: Option<String>,
```

It holds a *path*, not key material, so it is written to `config.toml` like
`client_cert_path` and unlike `sasl_pass`. Relative paths resolve against
`constants::certs_dir()` (`~/.repartee/certs`), so `sasl_key_path = "libera.pem"`
works without an absolute path.

`sasl_mechanism` accepts the six mechanism names case-insensitively, plus unset
for auto-detect.

### 6. Mechanism selection

```rust
pub enum SaslMechanism {
    Plain, External, ScramSha1, ScramSha256, ScramSha512, EcdsaNist256pChallenge,
}
```

`ServerCaps` grows `sasl_mechanisms_advertised() -> Option<Vec<String>>`:
`None` when `sasl` is advertised bare (we do not know the list), `Some(list)`
when it carries a value. `sasl_mechanisms()` stays as the "flattened" accessor.

`select_sasl_mechanism(advertised: Option<&[String]>, override, has_client_cert,
has_sasl_key, has_credentials)`:

- **Explicit override** — selected if its prerequisite is present (cert / key /
  user+pass) **and** either the server advertises it or the advertised list is
  unknown (`None`). Attempting a configured mechanism against a server that
  refused to enumerate is strictly better than silently authenticating with
  nothing.
- **Auto-detect**, in order, each requiring both the prerequisite and an
  explicit advertisement: `EXTERNAL` → `ECDSA-NIST256P-CHALLENGE` →
  `SCRAM-SHA-512` → `SCRAM-SHA-256` → `SCRAM-SHA-1` → `PLAIN`. With an unknown
  list, auto-detect falls back to `PLAIN` when credentials exist — today's
  behaviour, and the only mechanism a server that predates mechanism
  advertisement is likely to speak.

Strongest-first ordering: certificate and key mechanisms never put a secret on
the wire at all, and SCRAM never sends the password. `SCRAM-SHA-1` still ranks
above `PLAIN` — SHA-1's collision weakness does not touch its use inside
HMAC/PBKDF2 here, and it beats sending a cleartext password.

An override that cannot be satisfied keeps today's behaviour: no SASL at all,
with a diagnostic naming what was configured and what the server offered. Never
a silent downgrade to a weaker mechanism.

### 7. Surfaces

- TUI wizard `SASL_MECHS` and the web-UI wizard's list both become
  `Auto, PLAIN, SCRAM-SHA-256, SCRAM-SHA-512, SCRAM-SHA-1, EXTERNAL,
  ECDSA-NIST256P-CHALLENGE`, plus a `sasl_key_path` text field.
  `mech_index`/`mech_from_choice` stop hardcoding indices and look the name up
  in `SASL_MECHS`, so the two cannot drift when the list changes again.
- `/set` gains `sasl_key_path`; `sasl_mechanism` gets validated against the
  mechanism list instead of accepting any string.
- `.env` support is unchanged — the ECDSA key is a file path, not a secret to
  put in `.env`.
- Docs: `configuration.md`, `first-connection.md`, `architecture.md`, and a
  README changelog entry.

## Testing

Unit tests, no network:

1. **RFC vectors.** SCRAM-SHA-1 against RFC 5802 §5 (`user`/`pencil`, salt
   `QSXCR+Q6sek8bf92`, i=4096) — the published `ClientProof` and
   `ServerSignature` must match byte for byte. SCRAM-SHA-256 against RFC 7677
   §3 with the same credentials. These are the tests that prove the generic
   rewrite computes real SCRAM and not merely self-consistent SCRAM.
2. SCRAM-SHA-512 roundtrip: derived key widths are 64 bytes, and a wrong
   password yields a different server signature.
3. Each `ScramHash` reports its own mechanism name, and every name round-trips
   through `SaslMechanism`'s parser and `Display`.
4. Inbound reassembly: two 400-byte chunks plus a short tail concatenate before
   decoding; a single short chunk stands alone; `+` is an empty payload; total
   length beyond the cap errors.
5. ECDSA: a key generated in the test signs a 32-byte challenge, and the
   signature verifies against the public key with the same prehash convention.
   A fixed SEC1 PEM and a fixed PKCS#8 PEM both load. Garbage PEM errors without
   echoing its content. A 4-byte challenge errors.
6. `select_sasl_mechanism`: the full auto-detect ladder, each prerequisite gate,
   `-PLUS` never selected, override honoured when the advertised list is
   unknown, override rejected (→ `None`) when the server advertises without it.
7. `sasl_key_path` survives a config save/load round-trip and a wizard
   build/edit round-trip.

Verification: `make test` and `make clippy` clean — 0 warnings under pedantic +
nursery + perf=deny + redundant_clone=deny.

## Dependencies

Three additions, all already present transitively in `Cargo.lock`, all from the
RustCrypto org already used for `sha2`/`hmac`/`pbkdf2`:

- `sha1` — SCRAM-SHA-1
- `p256` (features `ecdsa`, `pem`, `pkcs8`) — ECDSA-NIST256P-CHALLENGE
- `stringprep` — SASLprep

`sha2` already provides SHA-512.

## Out of scope

- `SCRAM-*-PLUS` / channel binding (needs the TLS exporter, which the `irc`
  crate does not expose).
- `EXTERNAL` with an authzid, `ANONYMOUS`, `OAUTHBEARER`.
- Generating ECDSA keys from inside repartee — `ecdsatool` and `openssl` both
  do it, and a key-generation UI is a separate feature.
- `RPL_SASLMECHS` (908) driven retry with a second mechanism.
