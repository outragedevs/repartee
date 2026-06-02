# Built-in GG7 Emotes (`:name:`) for TUI and Web UI — Design

- **Date:** 2026-06-01
- **Status:** Approved (design); pending implementation plan
- **Topic:** Embed the Gadu-Gadu 7 animated emoticon set as built-in emotes, sendable and displayable inline in both the TUI and the web UI, using `:name:` shortcodes.

## 1. Goal & user intent

Ship the Gadu-Gadu 7 emoticon set **built into the binary**. A user can type a `:name:`
shortcode, have it travel over IRC as plain text, and see it rendered inline as an
(animated) image both in the TUI (graphics-capable terminals) and in the web UI. On
clients/terminals without graphics support, the shortcode degrades cleanly to readable
`:name:` text.

This is the Slack/Discord/Matrix shortcode model with graceful degradation — the
professional standard for "emoji-like" custom emotes over a plain-text transport.

## 2. Fundamental constraint (why this shape)

- **UTF-8 emoji** are codepoints in the text stream: they reflow, wrap, scroll, and copy
  for free because the terminal font renders them in cells.
- **GIF emotes** are raster graphics placed via a terminal graphics protocol
  (Kitty / iTerm2 / Sixel) at cell positions. They do **not** belong to the text stream and
  do not reflow on their own. A raster GIF cannot literally behave like a font glyph in an
  arbitrary terminal.
- **IRC carries plain text.** A GIF cannot be embedded in a `PRIVMSG`. Therefore "sending an
  emote" means sending the `:name:` text token. A repartee peer renders the image; any other
  IRC client shows the literal `:name:`.

ratatui is a full-screen immediate-mode renderer that repaints every frame and knows the
exact cell rectangle of every element. This is what makes inline graphical compositing
feasible: reserve cells for the emote in the text line, then composite the current animation
frame into that cell rect on each render pass.

## 3. Decisions (locked during brainstorming)

| Decision | Choice |
|---|---|
| Wire format / trigger syntax | `:name:` (paired colons), whitelist-matched against the known set. Mid-message `:` is not IRC-protocol-special; paired colons + whitelist mean `:)`/`:D`/unknown `:foo:` never trigger. |
| Emote set | **GG7 only** (scrape dirs `1/`, `2/`, `3/`). Shoutbox (`sb/`) is **excluded**. |
| Dedup / variants | One image per name. Precedence **`3 > 2 > 1`** ("dir 3 = most classic"). 14 of 16 variant names exist in `3/`; `dobani` and `kwiatek` fall back to `2/`. |
| Final set | **183 emotes, ~655 KB**, embedded in the binary. |
| Animation (TUI) | **Full animation from v1** (frame timer re-encodes frames at GIF delays). Web animates natively. |
| Insertion UX (TUI) | All of: (a) `:usm`→Tab autocomplete, (b) picker/palette popup with keyboard nav **and mouse click** inserting the `:name:` token, (c) raw `:name:` text always works. |
| TUI render architecture | **Approach A: overlay compositing** now. **Approach C: Kitty Unicode placeholders** possibly later. |

### Approaches considered

- **A — Overlay compositing (chosen).** Messages stay plain text with `:name:` tokens.
  Tokenize at render; reserve K placeholder cells; composite the current animation frame via
  `ratatui-image` in a post-render pass; a shared animation tick selects frames. Pro: storage,
  session-detach, web-snapshot, and the text fallback all work unchanged; clean degradation.
  Con: must compute emote screen rects after wrap/scroll; per-frame compositing cost.
- **B — Decode into the message model (rejected).** Attaching decoded frames to `Message`
  violates the UI-agnostic `state/` rule, bloats memory, and complicates session/snapshot
  serialization.
- **C — Kitty Unicode placeholders (future).** Images become real placeholder glyphs the
  terminal positions/scrolls itself. Most "truly inline", but Kitty-only and uncertain in
  `ratatui-image` v10. Tracked as a later Kitty-specific optimization, not the base.

## 4. Architecture

### 4.1 Asset layer — `src/emotes/`

- **Curation (one-time, reproducible).** From scrape dirs `1/2/3`, select one file per base
  name by precedence `3 > 2 > 1`, write as `assets/emotes/<name>.gif` (183 files), commit to
  the repo. The precedence rule is documented so the set is reproducible from the raw scrape.
- **Embedding.** Use the existing `rust-embed = "8"` dependency (already used for `static/web/`)
  with `#[derive(Embed)] #[folder = "assets/emotes/"]`. No new `build.rs`; matches the codebase
  convention. The sorted name list (for autocomplete/picker/manifest) is derived at runtime from
  `EmoteAssets::iter()` behind a `LazyLock`.
- **Registry API (`src/emotes/mod.rs`):**
  - `names() -> &'static [String]` — sorted, `.gif` stripped (LazyLock).
  - `contains(name) -> bool` — whitelist check used by the tokenizer.
  - `bytes(name) -> Option<Cow<'static, [u8]>>` — raw GIF bytes via `EmoteAssets::get`.
  - `frames(name) -> Option<&'static [(image::RgbaImage, u32 /* delay_ms */)]>` — lazy
    decode + cache (used only by the TUI animator in Plan 2).

### 4.2 Tokenizer (UI-agnostic) — `src/emotes/parse.rs`

- `tokenize(&str) -> Vec<Segment>` where `Segment = Text(Range) | Emote(&'static str)`.
- Matches only `:` + known-name + `:` (whitelist via registry). `:)`, `:D`, and unknown
  `:foo:` remain plain text. **No ratatui imports** (respects the `state/` UI-agnostic rule);
  shared by both TUI render and web serialization logic.

### 4.3 TUI inline render — `src/emotes/anim.rs` + `ui/message_line.rs` / `ui/chat_view.rs`

- **Footprint.** Emote scaled to **1 row tall** (≈ cell height), width proportional → typically
  **2 columns**. One-row height preserves line layout (no overlap with adjacent lines). At the
  token position reserve K columns of placeholder spaces and record the position.
- **Position resolver.** During wrap/scroll layout, compute each visible emote's screen
  `(x, y, cols)`.
- **Compositing.** After the text line renders, overlay the current frame via `ratatui-image`
  (same path as `image_overlay.rs`, including tmux passthrough).
- **Animation tick.** New arm in the `main.rs` `select!` loop, interval matched to the minimum
  `delay` among visible emotes. Frame index derived from `std::time::Instant` elapsed +
  cumulative per-frame delays (looping). Only **visible** emotes animate.
- **Caches.** Decoded frames per name; encoded-protocol cache keyed by
  `(name, cols, rows, frame_idx)` — bounded LRU.
- **Gating.** `Picker::protocol_type()`: Halfblocks / no graphics → render the literal
  `:name:` token (styled). Controlled by config.

### 4.4 Insertion UX (TUI)

- **Autocomplete.** `:` + partial at the cursor → name suggestions; Tab completes to `:name:`
  (hook into the existing completion infrastructure used for nicks).
- **Picker / palette.** New overlay `ui/emote_picker.rs` + state, opened via a keybinding and
  via a `/emote` command. Grid of emotes (graphical if the terminal supports it, else names);
  keyboard navigation **and mouse click** insert the `:name:` token at the cursor position.
- **Raw text.** Typing `:name:` always works regardless of UI affordances.

### 4.5 Web UI

- **Serving.** axum route `GET /emotes/{name}.gif` → embedded bytes (content-type +
  cache headers), served alongside the (public) static WASM assets.
- **Render.** `web-ui` `format.rs::emotify_spans` replaces known `:name:` tokens with
  `<img class="emote" alt="">` + a visually-hidden `:name:` span (so copy/paste and screen
  readers keep the shortcode); the browser animates natively.
- **Known-name list.** Embedded into the WASM at build time via `web-ui/build.rs` (reads the
  shared `assets/emotes/`), so the whitelist is available synchronously on the first render —
  no manifest fetch, no race against backlog rendering.
- **Config parity.** The server pushes `emotes_enabled` (`enabled && render==Graphical`) on the
  existing `SettingsChanged` event; the web UI gates emote `<img>` rendering on it so the
  `[emotes]` config governs both surfaces.

### 4.6 Storage / session / snapshot

- **No changes.** Messages are plain text containing `:name:` tokens, so SQLite storage,
  session-detach (postcard), and web-snapshots carry them for free. Rendering happens only at
  display time.

### 4.7 Configuration

```toml
[emotes]
enabled = true
render  = "graphical"   # graphical | text | off
```

Both keys are runtime-settable via `/set emotes.enabled` / `/set emotes.render` and listed in
`/set`. Semantics:
- `enabled = false` → no emotes anywhere (no rendering, no insertion affordances), tokens stay
  literal text on the wire and on screen.
- `render = graphical` → inline images in graphics-capable terminals and the web UI; literal
  `:name:` fallback on Halfblocks/no-graphics terminals.
- `render = text` → always literal `:name:`, but insertion affordances (tab-complete, picker,
  `/emote`) still work (you can insert tokens; they render as text).
- `render = off` → tokens are inert: no images **and** no insertion affordances.

The emote picker opens with **Ctrl+G** or `/emote`.

## 5. Testing

- `parse.rs`: tokenizer — matches, `:)`/`:D` ignored, unknown `:foo:` left as text, adjacent
  emotes, emotes at line start/end, `::`, partial tokens.
- Registry: all 183 names resolve to non-empty GIF bytes; name list is sorted; lookup works.
- `anim.rs`: frame index from elapsed time is deterministic given a delay list.
- Web `format.rs`: known `:name:` → `<img>`; unknown left unchanged.
- TUI: position resolver tested in isolation (placeholder coords given a wrapped buffer).

## 6. Risks

- **Performance** with many animated emotes on screen. Mitigation: animate only visible
  emotes; cache decoded and encoded frames; cap concurrent animations if needed.
- **Position resolver after wrap.** The hardest element; depends on whether `chat_view` wraps
  manually (to be confirmed during planning — if ratatui owns wrapping we may need to take
  over wrapping for buffers containing emotes).
- **tmux / protocol quirks.** Reuse the existing `image_overlay.rs` tmux-passthrough handling
  rather than reinventing it.

## 7. Out of scope (YAGNI)

- Shoutbox (`sb/`) set.
- Custom user-supplied emote packs / per-server emotes.
- Approach C (Kitty Unicode placeholders) — deferred.
- Sending actual image data over IRC (CTCP/URL rich payloads).
