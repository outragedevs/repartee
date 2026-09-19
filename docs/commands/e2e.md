---
category: Other
description: End-to-end encryption commands and key management
---

# /e2e

## Syntax
    /e2e <subcommand> [args]

## Description
Manage RPE2E encryption, trust, remembered peers, and keyring state.

## Subcommands

### on

Enable E2E on the current channel.

    /e2e on

### off

Disable E2E on the current channel.

    /e2e off

### mode

Set the channel mode.

    /e2e mode <auto-accept|normal|quiet>

### handshake

Send a manual KEYREQ to a peer.

    /e2e handshake <nick>

### accept

Accept a pending peer and send KEYRSP.

    /e2e accept <nick>

### decline

Decline a pending peer.

    /e2e decline <nick>

### revoke

Revoke a peer on the current channel.

    /e2e revoke <nick>

### unrevoke

Restore a revoked peer.

    /e2e unrevoke <nick>

### forget

Forget channel-local or global peer state.

    /e2e forget <nick|handle>
    /e2e forget -all <nick|handle>

### list

List trusted peers on the current channel or all remembered state.

    /e2e list
    /e2e list -all

### status

Show current identity and channel status.

    /e2e status

### fingerprint

Show the local fingerprint and SAS.

    /e2e fingerprint

### verify

Show side-by-side SAS for a peer.

    /e2e verify <nick>

### reverify

Accept a peer's changed state after manual verification — either a new
identity key, or a known key that has re-appeared under a different
`ident@host`.

    /e2e reverify <nick|handle> [fingerprint]

The warning prints the handle to pass, so the argument can be a full
`ident@host` as well as a nick. Accepting a handle change keeps the
existing fingerprint and trust and re-binds them to the new handle;
accepting a fingerprint change replaces the key.

Reverify acts on a warning repartee is currently holding. If the warning
was raised in an earlier session, run `/e2e handshake <nick>` first to
raise it again.

If more than one unresolved identity change is waiting on the peer you
named, reverify refuses and lists them instead of picking one — it cannot
tell which fingerprint you compared. Nothing is changed and no warning is
discarded.

Resolve it by passing the fingerprint of the key you verified, which is
what each line of the list starts with:

    /e2e reverify ~bob@b.host c37c65c773314a48

Any unique prefix works, and case does not matter. Accepting one key
settles the contest at that `ident@host`: the other claims are dropped,
and their peers can re-handshake to raise a fresh warning. A fingerprint
that matches nothing changes nothing.

### rotate

Schedule outgoing key rotation.

    /e2e rotate

### autotrust

Manage autotrust rules.

    /e2e autotrust <list|add|remove> [args...]

### export

Export the keyring.

    /e2e export <file>

### import

Import a keyring.

    /e2e import <file>

### help

Show built-in `/e2e` help.

    /e2e help

## Examples
    /e2e on
    /e2e mode normal
    /e2e list -all
    /e2e forget -all k2
    /e2e forget -all ~k@f7a48125c050.cloak.irc.al
