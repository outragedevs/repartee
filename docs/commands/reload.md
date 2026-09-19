---
category: Configuration
description: Reload config, .env credentials, and theme
---

# /reload

## Syntax

    /reload

## Description

Reload `~/.repartee/config.toml`, `~/.repartee/.env` credentials, and the
current theme from disk.

Existing IRC connections keep credentials already used to authenticate. New
or rotated service keys are applied where the running component supports a
safe live refresh; otherwise Repartee reports that a restart is required.

## See Also

/set, /items
