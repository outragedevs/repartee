---
category: Connection
description: Register or verify an upstream IRC account
---

# /account

## Syntax

    /account register [-allow-insecure-upstream] <account|*> <email|*> <password-env>
    /account verify [-allow-insecure-upstream] <account|*> <code-env>

## Description

Create or verify an IRC account through a connected bouncer network advertising
`draft/account-registration`. This concerns the IRC network account, not the
bouncer login. The capability must be acknowledged before the request is sent.
Soju forwards these operations when the upstream supports them; Lurker does not.

Use `*` as the account to choose your current nickname, or as the email when the
server permits registration without an email address. Server-advertised account
naming, required-email and password byte-length constraints are checked locally.
The password and verification code are read from the named variables in `.env`;
do not enter the secret values as command arguments.

Both operations require verified TLS to the bouncer and a discovered TLS-enabled
upstream. The optional `-allow-insecure-upstream` explicitly accepts exposure of
the password or verification code on a plaintext or unknown upstream transport;
it does not disable downstream TLS checks.

Replies appear in the originating connection's server buffer. Registration can
succeed immediately or require a subsequent verification step; follow the server's
instructions and then use `verify`. Soju can retain the registration password
for reconnects even while verification is pending. A timeout has an unknown
outcome, so no automatic retry is attempted. Wait for the response or reconnect
and inspect the account before trying again.

## Examples

    /account register * user@example.org IRC_NEW_PASSWORD
    /account verify * IRC_VERIFICATION_CODE

## See Also

/auth, /bouncer, /server
