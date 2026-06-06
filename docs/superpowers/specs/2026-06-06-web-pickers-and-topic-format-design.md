# Web UI: topic formatting fix + GG/UTF-8 emote pickers

**Date:** 2026-06-06
**Status:** approved
**Scope:** `web-ui/` (Leptos WASM) + minor `web-ui/styles/`. No changes to the native TUI.

## Problem

Three web-UI issues reported:

1. **Font** — "ensure the web uses our FiraCode font."
2. **IRC formatting not rendered** — colours/bold from mIRC/irssi appear unformatted.
3. **No emote/emoji picker on web**, no tab-completion for `:name:`, and `/emoji` does not open a picker.

## Investigation outcome (ground truth)

- **#1 Font is already correct.** FiraCode Nerd Font Mono is bundled (`web-ui/fonts/FiraCodeNerdFontMono-Regular.ttf`), embedded into `static/web/fonts/` via `rust-embed`, served at `/fonts/...` (route `/{*path}` → `serve_embedded`, unhashed → long cache), and set as the primary `font-family` in `web-ui/styles/base.css:28`; chat and input inherit it. **No change required — verify only.**

- **#2 The chat parser already works; the bug is the topic.** `web-ui/src/format.rs::parse_format` fully parses mIRC + irssi codes into CSS spans and is used by `chat_view.rs::render_styled_text`. The **channel topic** bypasses it: `web-ui/src/components/topic_bar.rs:22` renders `<span>{topic}</span>` raw, and the mobile mini-topic (`web-ui/src/components/layout.rs:167`) interpolates raw topic text. A secondary latent bug: `format.rs::mirc_color` only maps indices 0–15; the TUI palette (`src/theme/parser.rs`) covers 0–98, so extended-colour text is dropped uncoloured.

- **#3 Genuinely missing.** No picker component, no emote tab-completion in `web-ui/src/components/input.rs::build_tab_matches`, no `/emoji` client-side intercept. The `emojis` crate is **not** used anywhere — `/emoji` in the TUI is just an alias for `/emote` (GG emotes). A UTF-8 Unicode emoji picker is new and needs its own data source.

## Design

### 1. Font — verify only
No code change. During implementation, sanity-check that a running instance serves `/fonts/FiraCodeNerdFontMono-Regular.ttf` (HTTP 200, `font/ttf`).

### 2. Topic formatting fix

- **Extract a shared renderer.** Move the span→view logic out of `chat_view.rs::render_styled_text` into a reusable free function so both chat and topic use one path. Proposed home: a small `web-ui/src/components/styled.rs` (or a `pub` fn in `format.rs` returning views). Signature roughly:
  `fn render_spans(spans: Vec<format::StyledSpan>) -> Vec<AnyView>`
  with helpers `render_message_text(text, emotes_on)` (parse → linkify → optional emotify → render) and `render_topic_text(text)` (parse → linkify, **no emotify**).
- **`TopicBar`** (`topic_bar.rs:22`): replace `<span>{topic}</span>` with `render_topic_text(&topic)`. URLs in the topic become clickable; emotes are **not** expanded in the topic (kept clean).
- **Mobile mini-topic** (`layout.rs:167`): pass the topic through a new `format::strip_format(&str) -> String` (drops all mIRC/irssi control codes) before truncation, so no raw control bytes leak into the breadcrumb. Truncation must remain grapheme-safe (it already slices on a char boundary; keep that).
- **Extend `mirc_color`** in `format.rs` from 0–15 to the full **0–98** mIRC palette, matching `src/theme/parser.rs::MIRC_COLORS`. Benefits chat and topic.

### 3a. GG emote picker (modal)

- **New component** `web-ui/src/components/emote_picker.rs`, rendered in `layout.rs` next to `<ServerWizard/>`. Reuses `.wizard-backdrop` / `.wizard-modal` patterns with new `.emote-picker-*` classes in `base.css`.
- **State:** `emote_picker_open: RwSignal<bool>` in `state.rs`. Filter/selected are component-local signals.
- **Data:** existing build-time `EMOTE_NAMES` / stem map (`web-ui/src/emotes.rs`). Grid cells render the animated thumbnail `<img src="/emotes/{stem}.gif" loading="lazy">` plus the Polish name label.
- **Filtering:** live case-insensitive substring against emote names (mirror `EmotePickerState::filtered_indices`).
- **Interaction:** type → filter; ←/→/↑/↓ → move selection; Enter → insert; Esc or backdrop click → close; click a cell → insert. On insert, write `:name:` at the textarea caret and return focus to the input.
- **Triggers:**
  - **Command intercept** in `input.rs`: before sending, if the trimmed text is `/emoji`, `/emote`, or `/emotes` (no extra args), open the picker instead of dispatching (same pattern as the existing `/wizard server` intercept). `/emote <name>` still goes to the server.
  - **`[GG]` button** in the input line, left of `❯`.
  - **Tab-completion** for `:name:`: extend `input.rs::build_tab_matches` with an emote case (single leading `:`, no closing `:`, non-empty prefix → `:name:` + trailing space), gated on `emotes_enabled`. Mirrors TUI `ui/input.rs::emote_completions`.
  - **Ctrl+G** keybind (parity with TUI), in the input `on:keydown`.

### 3b. UTF-8 emoji picker (modal, desktop-only)

- **New component** `web-ui/src/components/emoji_picker.rs`, same modal scaffold.
- **State:** `emoji_picker_open: RwSignal<bool>` in `state.rs`.
- **Data:** add the **`emojis`** crate to `web-ui/Cargo.toml` (pure-Rust, WASM-compatible). Build a grid grouped by `emojis::Group` (Smileys & Emotion, People & Body, Animals & Nature, Food & Drink, Travel & Places, Activities, Objects, Symbols, Flags) with a filter over emoji name / shortcode.
- **Interaction:** type → filter; click or Enter → insert the **literal Unicode emoji** at the caret; Esc/backdrop → close.
- **Trigger:** a `[😀]` button in the input line with class `desktop-only` (hidden < 768px — phones already provide a system emoji keyboard). No command/keybind trigger.
- **Tradeoff:** the `emojis` crate embeds Unicode data, adding to the WASM bundle (~hundreds of KB). Accepted.

### Input line affordances
- Desktop: `[GG] [😀] ❯ …` (both buttons left of the prompt).
- Mobile: `[GG] ❯ …` (the `[😀]` button is `desktop-only`).
- Buttons are styled to match the input row; clicking focuses neither textarea nor steals it after the picker closes (focus returns to the textarea).

## Testing
Pure logic is extracted into free functions and unit-tested in the WASM crate's `#[cfg(test)]` modules:
- `format::strip_format` — drops every supported control code, leaves text.
- `format::mirc_color` — spot-check new indices (e.g. 16, 50, 98 map; 99 → None).
- Emote filter + the `:name:` insertion-string/caret computation.
- Emoji filter + grouping selection.
- `build_tab_matches` emote case (prefix → expected `:name:` completions).

Leptos components themselves are not unit-tested (no DOM harness); their logic lives in the tested free functions. Manual/visual verification of the rendered pickers is done by building (`make wasm`) and loading the web UI.

## Out of scope
- Native TUI changes.
- Server-side protocol changes (emote names already embedded client-side; GIFs already served).
- A UTF-8 emoji picker on mobile.
- Emote expansion inside topics.

## Key files
- `web-ui/src/format.rs` — `strip_format`, `mirc_color` 0–98, (maybe) shared render fn.
- `web-ui/src/components/styled.rs` *(new)* — shared span renderer (or `chat_view.rs` refactor).
- `web-ui/src/components/topic_bar.rs` — use shared renderer.
- `web-ui/src/components/layout.rs` — mobile mini-topic strip; mount both pickers.
- `web-ui/src/components/emote_picker.rs` *(new)*, `emoji_picker.rs` *(new)*.
- `web-ui/src/components/input.rs` — buttons, `/emoji` intercept, Ctrl+G, emote tab-completion.
- `web-ui/src/components/mod.rs` — module exports.
- `web-ui/src/state.rs` — two `RwSignal<bool>` flags.
- `web-ui/src/emoji.rs` *(new, optional)* — emoji grouping/filter helpers over the `emojis` crate.
- `web-ui/Cargo.toml` — add `emojis`.
- `web-ui/styles/base.css` — `.emote-picker-*`, `.emoji-picker-*`, input-button styles, `desktop-only` button.
