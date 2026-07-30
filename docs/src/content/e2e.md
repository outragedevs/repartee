# End-to-End Encryption

repartee includes built-in end-to-end encryption for IRC channels and private conversations. The IRC server still routes messages, but the plaintext stays on the participating clients.

This is designed to protect message content from passive network capture, server-side logging, and operators who can inspect IRC traffic but do not control the endpoints.

## What E2E protects

When E2E is enabled for a conversation:

- message bodies are encrypted before they leave your client
- the IRC server relays ciphertext, not plaintext
- peers decrypt messages locally after a key exchange

The server still sees metadata such as:

- nicknames and `ident@host`
- channel names or query targets
- timing and approximate message sizes
- the fact that an E2E handshake took place

## Trust model

repartee uses a trust-on-first-use model by default.

The first time a peer wants to exchange encrypted messages, repartee stores their E2E identity and asks for confirmation unless auto-accept is enabled. After that, future sessions for the same peer can be resumed automatically.

This means:

- passive observers cannot read message content
- active attackers are still relevant until you verify fingerprints out of band

If you want strong identity guarantees, verify the peer fingerprint using a second channel you trust.

## Basic flow

With E2E mode set to `normal`, the first encrypted message triggers a key exchange:

1. one client sends an encrypted message
2. the receiving client notices it cannot decrypt yet and requests a key exchange
3. repartee shows a pending request
4. you accept it with `/e2e accept <nick>`
5. both sides exchange keys and future messages decrypt automatically

Once both directions are established, both clients can send encrypted messages without repeating the setup.

## Common commands

Enable E2E in the current buffer:

```text
/e2e on
```

Disable E2E in the current buffer:

```text
/e2e off
```

Set the current buffer to normal trust mode:

```text
/e2e mode normal
```

Accept a pending key exchange:

```text
/e2e accept <nick>
```

Show trusted peers for the current channel or query:

```text
/e2e list
```

Show all remembered E2E state:

```text
/e2e list -all
```

Forget a remembered peer everywhere:

```text
/e2e forget -all <nick|ident@host>
```

For the full command reference, see [Commands](commands.html).

## Modes

### `normal`

`normal` is the safest everyday mode. Incoming key requests stay pending until you accept them. Use this when you want an explicit prompt before trusting a peer for the first time.

### `autoaccept`

`autoaccept` skips the manual approval step and accepts new requests automatically. This is more convenient, but it lowers protection against an active attacker during first contact.

Use it only if you understand that convenience is replacing explicit trust confirmation.

## Fingerprints and verification

Every peer has a fingerprint derived from their E2E identity key. This fingerprint is what you should compare out of band if you want protection against active impersonation or man-in-the-middle attacks.

Useful commands:

```text
/e2e fingerprint
/e2e verify <nick>
/e2e reverify <nick|handle> [fingerprint]
```

If a peer changes identity or appears under a different `ident@host`, repartee warns and blocks the handshake until you decide whether to trust the new state. The warning names the handle to pass to `/e2e reverify`, and that handle is accepted verbatim.

The two cases resolve differently, because they are not equally serious. A changed `ident@host` under an unchanged fingerprint is the same peer on a new connection: accepting re-binds the fingerprint you already verified to the new handle, and your trust is preserved. A changed fingerprint is a new key: accepting replaces the old one, so compare the SAS words out of band first.

Either way, the handshake that raised the warning was refused, so run `/e2e handshake <nick>` afterwards to open a session under the accepted state.

If two different keys are offered at the same `ident@host` before you decide, repartee holds both warnings and `/e2e reverify` refuses to guess between them: it lists the candidates and changes nothing. This matters — accepting the wrong one would install a key you never compared.

Two keys at one handle are indistinguishable by handle, so you resolve it by naming the fingerprint you verified. That is what the listing leads with, and it is the same value you compared out of band:

```text
/e2e reverify ~bob@b.host c37c65c773314a48
```

Any unique prefix of the fingerprint works. Accepting one key settles the contest at that handle — the competing claims are dropped, and those peers can re-handshake if they are genuine.

## Resetting state

`/e2e list` only shows trusted peers in the current conversation. It does not show every remembered identity.

If you want a true clean slate, inspect the full keyring first:

```text
/e2e list -all
```

Then remove the remembered peer globally:

```text
/e2e forget -all <nick|ident@host>
```

If you pass a nick, repartee resolves it to `ident@host` first and removes the stored peer state using that handle.

## Notes for channel use

E2E in IRC channels is negotiated per peer. In practice this means:

- you may trust some channel members and not others
- one peer may decrypt your messages before another does
- the first encrypted message can trigger the setup flow

That is expected. The channel remains an IRC channel, but the encrypted relationship is still established client to client.

## Limitations

E2E does not hide:

- who is talking to whom
- which channel is used
- when messages are sent
- approximate message size and frequency

It also does not protect against a compromised endpoint. If someone controls your client machine, your keys, or the running process, E2E cannot help.

## See also

- [First Connection](first-connection.html)
- [Configuration](configuration.html)
- [Commands](commands.html)
- [FAQ](faq.html)
