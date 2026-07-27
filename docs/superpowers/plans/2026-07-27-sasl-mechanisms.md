# SASL mechanism coverage — implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `PLAIN`, `EXTERNAL`, `SCRAM-SHA-1`, `SCRAM-SHA-256`, `SCRAM-SHA-512` and
`ECDSA-NIST256P-CHALLENGE` all authenticate against a real IRC server.

**Architecture:** `src/irc/sasl_scram.rs` becomes hash-parameterised over
SHA-1/256/512; a new `src/irc/sasl_ecdsa.rs` holds P-256 key loading and
challenge signing; `src/irc/mod.rs` keeps ownership of mechanism selection and
of the `AUTHENTICATE` state machines, and gains chunk reassembly for inbound
payloads. Config grows one field, `sasl_key_path`.

**Tech Stack:** Rust 2024, `sha1` + `sha2` + `hmac::SimpleHmac` + `pbkdf2`,
`p256` (ecdsa/pem/pkcs8), `stringprep`, `base64`.

**Spec:** `docs/superpowers/specs/2026-07-27-sasl-mechanisms-design.md`

## Global Constraints

- All builds through `make` targets — never raw `cargo`/`trunk`.
- `make clippy` and `make test` must both be clean: pedantic + nursery = warn,
  perf + `redundant_clone` = deny, **0 warnings**.
- Binary/app name always via `constants::APP_NAME`; paths via the `constants`
  accessors (`certs_dir()` = `~/.repartee/certs`).
- `color-eyre` for errors, `tracing` for logging, `thiserror` for library errors.
- No ratatui imports in `state/`.
- Key material and passwords never appear in a log line, an error message, or
  `config.toml`.
- Every task ends green (`make test` + `make clippy`) and commits.

---

### Task 1: SCRAM over any hash

**Files:**
- Modify: `Cargo.toml` (add `sha1`, `stringprep`)
- Modify: `src/irc/sasl_scram.rs`
- Modify: `src/irc/mod.rs` (call sites gain a `ScramHash` argument)

**Interfaces produced:**
```rust
pub enum ScramHash { Sha1, Sha256, Sha512 }
impl ScramHash {
    pub fn mechanism(self) -> &'static str;      // "SCRAM-SHA-1" | -256 | -512
    pub fn from_mechanism(name: &str) -> Option<Self>;  // case-insensitive
    pub fn output_len(self) -> usize;            // 20 | 32 | 64
}
pub fn saslprep(value: &str) -> String;
pub fn client_first(hash: ScramHash, username: &str) -> (String, String, String);
pub fn client_final(hash: ScramHash, server_first: &str, client_first_bare: &str,
                    client_nonce: &str, password: &str) -> Result<(String, Vec<u8>)>;
pub fn verify_server(server_final: &str, expected_signature: &[u8]) -> bool;  // unchanged
```

- [ ] **Step 1: Write the failing RFC-vector tests**

RFC 5802 §5 (SCRAM-SHA-1) publishes the complete exchange for `user`/`pencil`:

```
C: n,,n=user,r=fyko+d2lbbFgONRv9qkxdawL
S: r=fyko+d2lbbFgONRv9qkxdawL3rfcNHYJY1ZVvWVs7j,s=QSXCR+Q6sek8bf92,i=4096
C: c=biws,r=fyko+d2lbbFgONRv9qkxdawL3rfcNHYJY1ZVvWVs7j,p=v0X8v3Bz2T0CJGbJQyF0X+HI4Ts=
S: v=rmF9pqV8S7suAoZWja4dJRkFsKQ=
```

RFC 7677 §3 (SCRAM-SHA-256), same credentials:

```
C: n,,n=user,r=rOprNGfwEbeRWgbNEkqO
S: r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4096
C: c=biws,r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,p=dHzbZapWIk4jUhN+Ute9ytag9zjfMHgsqmmiz7AndVQ=
S: v=6rriTRBi23WpRR/wtup+mMhUZUn/dB5nLTJRsjl95G4=
```

Both tests drive `client_final` with the published client nonce and salt and
assert the exact `p=` and the exact `v=`. A SCRAM-SHA-512 test asserts a 64-byte
server signature and that a wrong password changes it.

- [ ] **Step 2: `make test` — expect failures** (`client_final` takes 4 args, no `ScramHash`)

- [ ] **Step 3: Implement**

Add to `Cargo.toml`: `sha1 = "0.10"`, `stringprep = "0.1"`.

Rewrite the maths generically:

```rust
fn hmac<D>(key: &[u8], data: &[u8]) -> Vec<u8>
where D: Digest + BlockSizeUser + Clone + Sync
{
    let mut mac = SimpleHmac::<D>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}
```

`client_final_with::<D>` does PBKDF2 via
`pbkdf2::pbkdf2::<SimpleHmac<D>>(password.as_bytes(), &salt, iters, &mut salted)`
into a `vec![0u8; <D as Digest>::output_size()]`, then ClientKey / StoredKey /
ClientSignature / ClientProof / ServerKey / ServerSignature exactly as today.
`ScramHash` matches to `client_final_with::<Sha1>` / `<Sha256>` / `<Sha512>`.

`client_first` applies `saslprep` to the username **before** escaping `=`→`=3D`
and `,`→`=2C`; `client_final` applies it to the password. `saslprep` wraps
`stringprep::saslprep` and returns the input unchanged on error with a
`tracing::warn!` that names neither the value nor its length.

Update the two `sasl_scram::` call sites in `src/irc/mod.rs` to pass
`ScramHash::Sha256` (behaviour-preserving until Task 3 widens the enum).

- [ ] **Step 4: `make test` + `make clippy` — expect green, 0 warnings**

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock src/irc/sasl_scram.rs src/irc/mod.rs
git commit -m "feat(sasl): parameterise SCRAM over SHA-1/256/512"
```

---

### Task 2: Reassemble inbound AUTHENTICATE, widen the mechanism set

**Files:**
- Modify: `src/irc/cap.rs` (`sasl_mechanisms_advertised`)
- Modify: `src/irc/mod.rs` (`await_authenticate_payload`, `SaslMechanism`,
  `select_sasl_mechanism`, `run_sasl_scram`)

**Interfaces consumed:** `ScramHash` from Task 1.
**Interfaces produced:**
```rust
impl ServerCaps { pub fn sasl_mechanisms_advertised(&self) -> Option<Vec<String>>; }
pub enum SaslMechanism { Plain, External, ScramSha1, ScramSha256, ScramSha512, EcdsaNist256pChallenge }
impl SaslMechanism {
    pub fn from_name(name: &str) -> Option<Self>;   // case-insensitive, whole token
    pub const ALL: &'static [SaslMechanism];
}
pub fn select_sasl_mechanism(advertised: Option<&[String]>, override_: Option<&str>,
                             has_client_cert: bool, has_sasl_key: bool,
                             has_credentials: bool) -> Option<SaslMechanism>;
async fn await_authenticate_payload(stream: &mut ClientStream) -> Result<Vec<u8>>;
fn reassemble_authenticate(chunks: &[String]) -> Result<Vec<u8>>;  // pure, testable
```

- [ ] **Step 1: Write the failing tests**

`reassemble_authenticate`: `["+"]` → empty; one short chunk → its decoded bytes;
`[400-byte chunk, 400-byte chunk, short]` → the concatenation decoded;
`[400-byte chunk, "+"]` → the first chunk alone; over-cap input errors.

`select_sasl_mechanism`: the six-rung auto-detect ladder in order; each rung
skipped without its prerequisite; `SCRAM-SHA-256-PLUS` alone selects nothing;
an override honoured when `advertised` is `None`; an override rejected when the
server advertises a list without it; `EcdsaNist256pChallenge` requires
`has_sasl_key`.

`SaslMechanism::from_name` round-trips every `Display` output, is
case-insensitive, and rejects `SCRAM-SHA-256-PLUS` and `SCRAM-SHA-384`.

`sasl_mechanisms_advertised`: `Some(list)` for `sasl=PLAIN,EXTERNAL`, `None` for
bare `sasl`, `None` when `sasl` is absent — with `has("sasl")` distinguishing
the last two at the call site.

- [ ] **Step 2: `make test` — expect failures**

- [ ] **Step 3: Implement**

`await_authenticate_payload` loops `stream.next()` under the existing 30 s
timeout, pushing each `AUTHENTICATE <param>` chunk; it stops on a chunk whose
length is under 400 or on a bare `+`, and returns
`reassemble_authenticate(&chunks)`. `ERR_SASLFAIL`/`ERR_SASLTOOLONG`/
`ERR_SASLABORT`/`ERR_SASLALREADY` map to errors, as in
`await_authenticate_plus`. Total accumulated base64 is capped at
`MAX_AUTHENTICATE_BYTES` (8 KiB — an order of magnitude above any real SCRAM
message).

`run_sasl_scram` takes a `ScramHash`, sends `AUTHENTICATE <mechanism>`, and
reads server-first and server-final through `await_authenticate_payload`.

`select_sasl_mechanism` takes `Option<&[String]>` and the extra `has_sasl_key`
flag; `negotiate_caps` passes `server_caps.sasl_mechanisms_advertised()`.
The `SaslMechanism` match in `negotiate_caps` covers the three SCRAM variants
by mapping each to its `ScramHash`; `EcdsaNist256pChallenge` is wired in Task 3
and until then returns an explicit "not yet wired" error rather than silently
succeeding.

- [ ] **Step 4: `make test` + `make clippy` — expect green, 0 warnings**

- [ ] **Step 5: Commit**

```bash
git add src/irc/cap.rs src/irc/mod.rs
git commit -m "feat(sasl): reassemble chunked AUTHENTICATE and widen mechanism selection"
```

---

### Task 3: ECDSA-NIST256P-CHALLENGE

**Files:**
- Modify: `Cargo.toml` (add `p256`)
- Create: `src/irc/sasl_ecdsa.rs`
- Modify: `src/irc/mod.rs` (`run_sasl_ecdsa`, `RegistrationParams.sasl_key_path`)
- Modify: `src/config/mod.rs` (`ServerConfig::sasl_key_path`)
- Modify: every `ServerConfig` literal in tests/helpers (`src/app/*.rs`,
  `src/commands/*.rs`, `src/config/env.rs`, `src/ui/*.rs`, `src/web/*.rs`)

**Interfaces consumed:** `await_authenticate_payload` from Task 2.
**Interfaces produced:**
```rust
// src/irc/sasl_ecdsa.rs
pub fn resolve_key_path(configured: &str) -> PathBuf;   // relative → certs_dir()
pub fn load_key(pem: &str) -> Result<p256::ecdsa::SigningKey>;  // SEC1 then PKCS#8
pub fn load_key_file(path: &Path) -> Result<p256::ecdsa::SigningKey>;
pub fn sign_challenge(key: &SigningKey, challenge: &[u8]) -> Result<Vec<u8>>;  // DER
pub fn authcid_payload(user: &str) -> Vec<u8>;          // "user\0user"
```

- [ ] **Step 1: Write the failing tests**

- `authcid_payload("bob")` == `b"bob\0bob"`.
- A `SigningKey` built from fixed key bytes signs a 32-byte challenge; the DER
  signature parses back and `VerifyingKey::verify_prehash` accepts it.
- `sign_challenge` on a 4-byte challenge errors and the message says the
  challenge is too short.
- `load_key` accepts a SEC1 `-----BEGIN EC PRIVATE KEY-----` PEM and a PKCS#8
  `-----BEGIN PRIVATE KEY-----` PEM holding the same key, and both yield the
  same `VerifyingKey`.
- `load_key("not a pem")` errors, and the error text contains neither the input
  nor any key bytes.
- `resolve_key_path("libera.pem")` is under `constants::certs_dir()`;
  `resolve_key_path("/abs/k.pem")` is returned unchanged.

The two PEMs are generated once with `openssl` and pasted into the test as
constants — a test-only throwaway key, never a credential.

- [ ] **Step 2: `make test` — expect failures** (module does not exist)

- [ ] **Step 3: Implement**

`Cargo.toml`: `p256 = { version = "0.13", features = ["ecdsa", "pem", "pkcs8"] }`.

`load_key` tries `SecretKey::from_sec1_pem`, then `SecretKey::from_pkcs8_pem`,
and reports "expected a PEM EC private key (SEC1 or PKCS#8)" with the path on
failure — never the file contents. `sign_challenge` rejects challenges under 16
bytes, then `PrehashSigner::sign_prehash(challenge)` and `.to_der().to_vec()`.

`run_sasl_ecdsa` in `src/irc/mod.rs`: `AUTHENTICATE
ECDSA-NIST256P-CHALLENGE` → `await_authenticate_plus` → chunked
base64(`authcid_payload`) → `await_authenticate_payload` (the challenge) →
chunked base64(signature) → 903/904. The key file is read **once, before**
`AUTHENTICATE` is sent, so an unreadable key aborts the mechanism instead of
leaving a half-open exchange.

`ServerConfig` gains `sasl_key_path`; `RegistrationParams` gains it too;
`connect_server` passes `has_sasl_key: server_config.sasl_key_path.is_some()`.

- [ ] **Step 4: `make test` + `make clippy` — expect green, 0 warnings**

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock src/irc/sasl_ecdsa.rs src/irc/mod.rs src/config/mod.rs src/
git commit -m "feat(sasl): add ECDSA-NIST256P-CHALLENGE"
```

---

### Task 4: Surfaces and docs

**Files:**
- Modify: `src/ui/wizard/server.rs`, `web-ui/src/components/wizard.rs`
- Modify: `src/commands/settings.rs`, `src/commands/handlers_irc.rs`
- Modify: `src/web/protocol.rs`, `web-ui/src/protocol.rs` (if the wizard form
  crosses the wire — check `ServerForm`)
- Modify: `docs/src/content/configuration.md`, `first-connection.md`,
  `architecture.md`, `README.md`

- [ ] **Step 1: Write the failing tests**

- The wizard's mechanism list contains all six names plus `Auto`, and
  `mech_index(mech_from_choice(name)) == name` for every entry — the property
  that used to be two hardcoded index tables.
- A wizard build with `sasl_key_path` set round-trips into `ServerConfig`.
- `/set <server> sasl_mechanism SCRAM-SHA-512` is accepted;
  `/set <server> sasl_mechanism NOPE` is rejected with a message listing the
  valid names.
- `sasl_key_path` is in the `/set` key list and survives a config save/load.

- [ ] **Step 2: `make test` — expect failures**

- [ ] **Step 3: Implement**

`SASL_MECHS` becomes `["Auto", "PLAIN", "SCRAM-SHA-256", "SCRAM-SHA-512",
"SCRAM-SHA-1", "EXTERNAL", "ECDSA-NIST256P-CHALLENGE"]`; `mech_index` and
`mech_from_choice` derive from that slice by position instead of hardcoding 1
and 2. The web-UI list is the same sequence. Both wizards gain a
`sasl_key_path` text field next to `client_cert_path`.

`settings.rs`: add `sasl_key_path` to the server key list and to the getter and
setter matches; validate `sasl_mechanism` through `SaslMechanism::from_name`
plus the literal `Auto`/empty.

Docs: document all six mechanisms, the auto-detect order, `sasl_key_path`, and
how to generate a key (`openssl ecparam -genkey -name prime256v1 -noout -out
key.pem`, then `/msg NickServ SET PUBKEY <base64 compressed public key>`).

- [ ] **Step 4: `make test` + `make clippy` + `make wasm` — expect green**

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(sasl): expose every mechanism in the wizards, /set and docs"
```

---

## What actually shipped beyond the plan

Two things surfaced during implementation and were folded in:

1. **The SCRAM acknowledgement** (spec §3a). The shipped SCRAM-SHA-256 never
   sent the empty `AUTHENTICATE +` that answers the server-final, and waited for
   `903` in an untimed loop — so it hung rather than authenticating, against
   both atheme and Ergo. Fixed, and every wait in the SASL path is now bounded.
   Also `AUTHENTICATE *` now aborts an exchange we failed locally, instead of
   leaving the server holding a half-open session across `CAP END`.
2. **`/server add`** (Task 4 named only the wizards and `/set`). It gained
   `-sasl-key=<path>` — without it there was no command-line route to an ECDSA
   key at all — and `-sasl-mechanism=` is validated through the same helper
   `/set` uses.

## Self-Review

**Spec coverage:** §1 SCRAM-over-hash → Task 1. §2 SASLprep → Task 1. §3 inbound
reassembly → Task 2. §4 ECDSA → Task 3. §5 configuration → Task 3 (field) +
Task 4 (surfaces). §6 selection → Task 2. §7 surfaces → Task 4. Testing items
1–3 → Task 1; 4 and 6 → Task 2; 5 → Task 3; 7 → Task 4. No gaps.

**Placeholders:** none — every step names the exact functions, bounds, and
constants involved.

**Type consistency:** `ScramHash` (Task 1) is consumed by `run_sasl_scram`
(Task 2). `await_authenticate_payload` (Task 2) is consumed by `run_sasl_ecdsa`
(Task 3). `select_sasl_mechanism`'s signature changes once, in Task 2, and
Task 3 only adds the argument's *source* (`sasl_key_path`) — so Task 2 must land
`has_sasl_key` even though nothing sets it yet; it is passed `false` until
Task 3, and Task 2's tests exercise it directly.
