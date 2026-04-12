---
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

Accept a changed fingerprint after manual verification.

    /e2e reverify <nick>

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
