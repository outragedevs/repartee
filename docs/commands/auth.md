---
category: Connection
description: Authenticate an upstream IRC account using a password secret reference
---

# /auth

## Syntax

    /auth login [-allow-insecure-upstream] <IRC-account> <password-env-name>
    /auth clear -YES

## Description

Authenticate the active bound bouncer network to its upstream IRC account.
This requires a verified TLS connection and upstream SASL advertised by the
bouncer after registration. It does not replace your bouncer login credentials.
The pinned Soju implementation supports these operations; Lurker does not.

The bouncer's upstream network must advertise `tls=1` through network discovery.
If the upstream uses plaintext or its TLS state is unknown (including manually
bound connections without discovery metadata), login is refused before loading
the secret. `/auth login -allow-insecure-upstream <account> <password-env-name>`
explicitly overrides this upstream check: SASL PLAIN can then expose the IRC
password between the bouncer and IRC server. This does not bypass the verified
TLS requirement between this client and the bouncer. Clearing saved credentials
does not transmit a password upstream and does not need this override.


Store the IRC account password in the application's `.env` file and pass the
name of that variable, not its value. For example, with an `IRC_ACCOUNT_PASSWORD`
entry, use `/auth login my-account IRC_ACCOUNT_PASSWORD`. No password is entered
into the native or web command history. The result appears in that connection's
server buffer, even if you switch to another network while waiting.

On successful login, Soju saves the upstream credentials for reconnects.
`/auth clear -YES` removes the network's saved SASL credentials on the bouncer,
including any SASL EXTERNAL certificate and private key. This is not a command
to log out of the bouncer, and it does not guarantee a live upstream logout.

Only one unresolved authentication operation is allowed per connection.
Timeouts report an unknown outcome; wait for a terminal reply or reconnect and
check the account state before retrying. This command does not create or verify
an IRC account; those operations are separate.

## Examples

    /auth login my-account IRC_ACCOUNT_PASSWORD
    /auth clear -YES

## See Also

/bouncer, /server, /set
