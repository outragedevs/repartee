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
/e2e reverify <nick>
```

If a peer changes identity or appears under a different `ident@host`, repartee can warn and require you to decide whether to trust the new state.

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
