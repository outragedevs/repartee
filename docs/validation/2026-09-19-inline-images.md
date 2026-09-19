# Inline terminal image validation

The opt-in `image_preview.inline` setting reserves a fixed thumbnail slot below the first direct-image URL in each message. Downloads are limited to visible slots, four concurrent requests, and 32 cached previews. Public-address validation, redirect checks and a dedicated DNS resolver protect automatic requests; the client does not inherit proxy environment settings.

Validation includes:

- `make clippy` without project warnings;
- `make test`: 2288 native tests and 138 web tests;
- native `make build`;
- renderer tests preserving message positions before and after delayed image completion;
- correct link selection on thumbnail rows and messages below them;
- buffer switching and protocol invalidation;
- oldest-message access and the backlog trigger across 100 long image-bearing messages;
- pixel-level cropping at the viewport boundary;
- allocation/dimension limits and invalid-image rejection;
- bounded pending requests, cache eviction and stale-result rejection;
- private-address rejection before network access;
- oversized chunked responses rejected before EOF;
- automatic downloads leave no disk-cache entries;
- Kitty and iTerm2 tmux passthrough bytes, coordinates and retransmission after cleanup;
- 20 unchanged frames without direct-image retransmission, plus layout invalidation;
- change detection limited to the rendered history window in a 1000-message buffer;
- runtime protocol refresh and cleanup using the previous graphics protocol;
- HTML rejection without a second Open Graph request in automatic mode.

The renderer exercise uses Ratatui's test backend and the half-block path. Protocol tests inspect emitted control sequences. Physical Kitty, iTerm2, Sixel and tmux sessions have not been exercised, so these tests do not establish a full terminal compatibility matrix. Graphics cleanup uses the active terminal writer, and local tmux image writes share the existing popup implementation.
