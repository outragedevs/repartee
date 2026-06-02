# GG7 Emotes — Plan 1: Foundation + Web UI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Embed the 183-emote GG7 set in the binary and make `:name:` shortcodes render as inline images in the web UI, with the tokens carried verbatim as plain text over IRC.

**Architecture:** A new UI-agnostic `src/emotes/` module embeds the curated GIFs via `rust-embed` and exposes a name whitelist + a tokenizer. The axum web server serves the bytes and a JSON name manifest. The Leptos `web-ui` crate fetches the manifest once and rewrites known `:name:` tokens into `<img class="emote">` during message formatting. No changes to message storage, session, or snapshot paths — messages stay plain text.

**Tech Stack:** Rust 2024, rust-embed 8, axum 0.8, serde, Leptos (WASM), `image` 0.25 (validation only here).

**Spec:** `docs/superpowers/specs/2026-06-01-builtin-gg-emotes-design.md`

**Conventions (from CLAUDE.md):** `color-eyre` for errors, `tracing` for logs, reference `crate::constants::APP_NAME` never hardcode the name, clippy pedantic+nursery=warn / perf=deny / redundant_clone=deny (0 warnings). Build via `make` targets, never raw cargo/trunk. Commit frequently.

---

## File Structure

| File | Responsibility | Create/Modify |
|---|---|---|
| `assets/emotes/*.gif` | The 183 curated emote images (committed) | Create |
| `src/emotes/mod.rs` | Embedded registry: bytes, sorted names, `contains` | Create |
| `src/emotes/parse.rs` | UI-agnostic `:name:` tokenizer → segments | Create |
| `src/lib.rs` or `src/main.rs` | Declare `mod emotes;` | Modify |
| `src/config/mod.rs` | `EmotesConfig` + `RenderMode` enum, add to `AppConfig` | Modify |
| `src/web/server.rs` | `GET /emotes/{name}` + `GET /emotes/manifest.json` routes/handlers | Modify |
| `web-ui/src/format.rs` | `StyledSpan.emote_name` field + `emotify_spans()` | Modify |
| `web-ui/src/state.rs` | Hold fetched emote-name set | Modify |
| `web-ui/src/components/chat_view.rs` | `<img>` branch in `render_styled_text` | Modify |
| `web-ui/index.html` or CSS | `.emote` style (inline-aligned, height ~1em) | Modify |

> **Module location note:** Confirm whether the root crate is a binary-only crate (`main.rs`) or has a `lib.rs`. Web server code (`src/web/`) and config already compile in the same crate, so `crate::emotes` is reachable from `src/web/server.rs` regardless. Declare `mod emotes;` next to the other top-level `mod` declarations (same file that declares `mod web;`, `mod config;`).

---

## Task 1: Curate the 183-emote asset set

**Files:**
- Create: `assets/emotes/<name>.gif` (183 files)
- Create: `assets/emotes/SOURCE.md` (provenance + curation rule)

- [ ] **Step 1: Run the curation command**

The raw scrape lives at `/home/projekt/emots-yetihehe-scrape/{1,2,3}/`. Select one file per
base name with precedence `3 > 2 > 1` (dir 3 = most classic). Run from the repo root:

```bash
SRC=/home/projekt/emots-yetihehe-scrape
DEST=assets/emotes
mkdir -p "$DEST"
for name in $(for d in 1 2 3; do for f in "$SRC"/$d/*.gif; do basename "$f" .gif; done; done | sort -u); do
  for d in 3 2 1; do
    if [ -f "$SRC/$d/$name.gif" ]; then cp "$SRC/$d/$name.gif" "$DEST/$name.gif"; break; fi
  done
done
```

- [ ] **Step 2: Verify the invariants**

Run:
```bash
ls assets/emotes/*.gif | wc -l                      # Expected: 183
file assets/emotes/*.gif | grep -vci 'GIF image'    # Expected: 0
find assets/emotes -name '*.gif' -size 0 | wc -l    # Expected: 0
du -sh assets/emotes                                # Expected: ~655K
# Spot-check the dir-3 precedence picked the classic variants:
file assets/emotes/usmiech.gif assets/emotes/smutny.gif   # Expected: GIF image, non-empty
```
Expected: 183 files, 0 non-GIF, 0 empty, ~655K.

- [ ] **Step 3: Write provenance doc**

Create `assets/emotes/SOURCE.md`:
```markdown
# GG7 Emote Set

Source: https://emots.yetihehe.com/ (Gadu-Gadu 7 emoticons, dirs 1/2/3).
Curation: one file per base name, precedence 3 > 2 > 1 (dir 3 = most classic variant).
14 of 16 variant names resolve to dir 3; `dobani` and `kwiatek` fall back to dir 2.
Shoutbox (`sb/`) intentionally excluded. 183 emotes, ~655 KB.
Reproduce with the command in docs/superpowers/plans/2026-06-01-gg-emotes-foundation-web.md (Task 1).
```

- [ ] **Step 4: Commit**

```bash
git add assets/emotes
git commit -m "assets: add curated GG7 emote set (183 GIFs, precedence 3>2>1)"
```

---

## Task 2: Emote registry module

**Files:**
- Create: `src/emotes/mod.rs`
- Test: inline `#[cfg(test)]` in `src/emotes/mod.rs`
- Modify: top-level module declaration file (add `mod emotes;`)

- [ ] **Step 1: Declare the module**

In the file that holds the other top-level `mod` declarations (the one with `mod web;` / `mod config;`), add:
```rust
mod emotes;
```

- [ ] **Step 2: Write the failing tests**

In `src/emotes/mod.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_sorted_and_nonempty() {
        let names = names();
        assert!(names.len() >= 180, "expected the full GG7 set, got {}", names.len());
        assert!(names.windows(2).all(|w| w[0] <= w[1]), "names must be sorted");
        assert!(names.iter().all(|n| !n.is_empty() && !n.contains('.')));
    }

    #[test]
    fn known_emote_resolves_to_gif_bytes() {
        assert!(contains("usmiech"));
        let bytes = bytes("usmiech").expect("usmiech must exist");
        assert!(bytes.starts_with(b"GIF"), "embedded asset must be a GIF");
    }

    #[test]
    fn unknown_emote_is_absent() {
        assert!(!contains("definitely_not_an_emote"));
        assert!(bytes("definitely_not_an_emote").is_none());
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p repartee emotes::tests -- --nocapture`
Expected: FAIL — `cannot find function names` (module not implemented yet).

- [ ] **Step 4: Implement the registry**

In `src/emotes/mod.rs` (above the test module):
```rust
//! Built-in GG7 emote registry: embedded GIF assets + name whitelist.
//!
//! UI-agnostic. The 183 curated GIFs in `assets/emotes/` are embedded at compile
//! time via `rust-embed`. Names are the file stems (`usmiech.gif` -> `usmiech`).

use std::borrow::Cow;
use std::sync::LazyLock;

use rust_embed::Embed;

pub mod parse;

#[derive(Embed)]
#[folder = "assets/emotes/"]
struct EmoteAssets;

/// Sorted list of emote names (file stems, `.gif` removed).
static NAMES: LazyLock<Vec<String>> = LazyLock::new(|| {
    let mut v: Vec<String> = EmoteAssets::iter()
        .filter_map(|f| f.strip_suffix(".gif").map(ToOwned::to_owned))
        .collect();
    v.sort_unstable();
    v
});

/// All known emote names, sorted ascending.
#[must_use]
pub fn names() -> &'static [String] {
    &NAMES
}

/// Whether `name` (without `.gif`) is a known emote. Used as the tokenizer whitelist.
#[must_use]
pub fn contains(name: &str) -> bool {
    NAMES.binary_search_by(|n| n.as_str().cmp(name)).is_ok()
}

/// Raw GIF bytes for `name` (without `.gif`), or `None` if unknown.
#[must_use]
pub fn bytes(name: &str) -> Option<Cow<'static, [u8]>> {
    if !contains(name) {
        return None;
    }
    EmoteAssets::get(&format!("{name}.gif")).map(|f| f.data)
}
```

> Note: `rust_embed::Embed` resolves `#[folder = "assets/emotes/"]` relative to `CARGO_MANIFEST_DIR` (repo root). `frames()` is added in Plan 2 — do not add it here.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p repartee emotes::tests -- --nocapture`
Expected: PASS (3 tests).

- [ ] **Step 6: Lint**

Run: `make clippy`
Expected: 0 warnings.

- [ ] **Step 7: Commit**

```bash
git add src/emotes/mod.rs src/main.rs   # adjust path of the mod-decl file
git commit -m "feat(emotes): embedded GG7 registry (names, contains, bytes)"
```

---

## Task 3: `:name:` tokenizer (UI-agnostic)

**Files:**
- Create: `src/emotes/parse.rs`
- Test: inline `#[cfg(test)]` in `src/emotes/parse.rs`

- [ ] **Step 1: Write the failing tests**

In `src/emotes/parse.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    // Test helper: a fixed whitelist so tests don't depend on the embedded set.
    fn known(name: &str) -> bool {
        matches!(name, "usmiech" | "smutny" | "lol")
    }

    fn seg(text: &str) -> Vec<Segment> {
        tokenize_with(text, known)
    }

    #[test]
    fn plain_text_has_no_emotes() {
        assert_eq!(seg("hello world"), vec![Segment::Text(0..11)]);
    }

    #[test]
    fn single_known_emote() {
        // "a :lol: b"
        assert_eq!(
            seg("a :lol: b"),
            vec![Segment::Text(0..2), Segment::Emote("lol".into()), Segment::Text(7..9)]
        );
    }

    #[test]
    fn unknown_token_stays_text() {
        assert_eq!(seg(":nope:"), vec![Segment::Text(0..6)]);
    }

    #[test]
    fn smiley_does_not_trigger() {
        // ":)" and ":D" must remain plain text.
        assert_eq!(seg(":) :D"), vec![Segment::Text(0..5)]);
    }

    #[test]
    fn adjacent_emotes() {
        assert_eq!(
            seg(":lol::smutny:"),
            vec![Segment::Emote("lol".into()), Segment::Emote("smutny".into())]
        );
    }

    #[test]
    fn emote_at_start_and_end() {
        assert_eq!(
            seg(":usmiech:x:lol:"),
            vec![
                Segment::Emote("usmiech".into()),
                Segment::Text(9..10),
                Segment::Emote("lol".into())
            ]
        );
    }

    #[test]
    fn lone_colons() {
        assert_eq!(seg("::"), vec![Segment::Text(0..2)]);
        assert_eq!(seg("a::b"), vec![Segment::Text(0..4)]);
    }

    #[test]
    fn names_with_underscores_and_digits() {
        fn known2(n: &str) -> bool { n == "je_pizze" || n == "8p" }
        assert_eq!(
            tokenize_with(":je_pizze:", known2),
            vec![Segment::Emote("je_pizze".into())]
        );
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p repartee emotes::parse -- --nocapture`
Expected: FAIL — `cannot find type Segment`.

- [ ] **Step 3: Implement the tokenizer**

In `src/emotes/parse.rs` (above tests):
```rust
//! UI-agnostic tokenizer that splits a message into text runs and `:name:` emotes.
//!
//! Matching rule: a colon, then a run of `[a-z0-9_]`, then a colon, where the inner
//! name is a known emote. `:)`, `:D`, and unknown `:foo:` stay as plain text.

use std::ops::Range;

/// A contiguous slice of the input: either literal text or a resolved emote name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    /// Byte range into the source string.
    Text(Range<usize>),
    /// A known emote name (without colons or `.gif`).
    Emote(String),
}

/// Tokenize using the embedded registry as the whitelist.
#[must_use]
pub fn tokenize(text: &str) -> Vec<Segment> {
    tokenize_with(text, super::contains)
}

/// Tokenize using a caller-supplied whitelist predicate (used in tests).
pub fn tokenize_with(text: &str, is_known: impl Fn(&str) -> bool) -> Vec<Segment> {
    let bytes = text.as_bytes();
    let mut out: Vec<Segment> = Vec::new();
    let mut text_start = 0usize;
    let mut i = 0usize;

    while i < bytes.len() {
        if bytes[i] == b':' {
            // Find the closing colon of a candidate name.
            let name_start = i + 1;
            let mut j = name_start;
            while j < bytes.len() && is_name_byte(bytes[j]) {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b':' && j > name_start {
                let name = &text[name_start..j];
                if is_known(name) {
                    if text_start < i {
                        out.push(Segment::Text(text_start..i));
                    }
                    out.push(Segment::Emote(name.to_owned()));
                    i = j + 1;
                    text_start = i;
                    continue;
                }
            }
        }
        i += 1;
    }
    if text_start < bytes.len() {
        out.push(Segment::Text(text_start..bytes.len()));
    }
    out
}

const fn is_name_byte(b: u8) -> bool {
    b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'
}
```

> **Name charset note:** Emote names are ASCII lowercase/digits/`_` (verified across the 183 files — they match `[a-z0-9_]`, e.g. `8p`, `je_pizze`, `3m_sie`). `is_name_byte` enforces this so adjacent emotes (`:lol::smutny:`) and lone colons parse correctly.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p repartee emotes::parse -- --nocapture`
Expected: PASS (8 tests).

- [ ] **Step 5: Verify embedded names match the tokenizer charset**

Add this test to `src/emotes/mod.rs` tests and run it:
```rust
#[test]
fn all_embedded_names_are_valid_tokens() {
    for n in names() {
        assert!(
            n.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'),
            "name {n:?} contains a byte outside [a-z0-9_]"
        );
    }
}
```
Run: `cargo test -p repartee emotes -- --nocapture`
Expected: PASS. If it fails, the curated set has an out-of-charset name — rename or extend `is_name_byte` and update tokenizer tests accordingly.

- [ ] **Step 6: Commit**

```bash
git add src/emotes/parse.rs src/emotes/mod.rs
git commit -m "feat(emotes): UI-agnostic :name: tokenizer with whitelist"
```

---

## Task 4: `[emotes]` config section

**Files:**
- Modify: `src/config/mod.rs` (struct list ~line 61–79; add `EmotesConfig` near `WebConfig` ~line 514)

- [ ] **Step 1: Write the failing test**

In the `#[cfg(test)]` module of `src/config/mod.rs` (tests live around lines 662–923):
```rust
#[test]
fn emotes_config_defaults_and_roundtrip() {
    let cfg = AppConfig::default();
    assert!(cfg.emotes.enabled);
    assert_eq!(cfg.emotes.render, RenderMode::Graphical);

    // TOML round-trip preserves the section.
    let toml_str = toml::to_string(&cfg).expect("serialize");
    let back: AppConfig = toml::from_str(&toml_str).expect("deserialize");
    assert_eq!(back.emotes.render, RenderMode::Graphical);

    // Parsing an explicit section.
    let parsed: AppConfig = toml::from_str("[emotes]\nenabled = false\nrender = \"text\"\n").unwrap();
    assert!(!parsed.emotes.enabled);
    assert_eq!(parsed.emotes.render, RenderMode::Text);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p repartee emotes_config_defaults_and_roundtrip`
Expected: FAIL — `no field emotes on AppConfig`.

- [ ] **Step 3: Add the config types**

Near `WebConfig` in `src/config/mod.rs`:
```rust
/// How `:name:` emote tokens are rendered.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RenderMode {
    /// Render as an inline image where the surface supports it; fall back to text.
    #[default]
    Graphical,
    /// Always render the literal `:name:` text.
    Text,
    /// Do not treat `:name:` as an emote at all.
    Off,
}

/// `[emotes]` configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EmotesConfig {
    pub enabled: bool,
    pub render: RenderMode,
}

impl Default for EmotesConfig {
    fn default() -> Self {
        Self { enabled: true, render: RenderMode::Graphical }
    }
}
```

Add the field to `AppConfig` (after `pub web: WebConfig,`):
```rust
    pub emotes: EmotesConfig,
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p repartee emotes_config_defaults_and_roundtrip`
Expected: PASS.

- [ ] **Step 5: Lint + commit**

```bash
make clippy   # 0 warnings
git add src/config/mod.rs
git commit -m "feat(config): add [emotes] section (enabled, render mode)"
```

---

## Task 5: Web server — serve emote bytes + manifest

**Files:**
- Modify: `src/web/server.rs` (`build_router` ~line 254–266; add handlers; reuse `mime_from_path` ~line 231)
- Test: inline `#[cfg(test)]` in `src/web/server.rs`

- [ ] **Step 1: Write the failing test (manifest content)**

In `src/web/server.rs` tests:
```rust
#[test]
fn emote_manifest_lists_known_names() {
    let json = emote_manifest_json();
    let names: Vec<String> = serde_json::from_str(&json).expect("valid JSON array");
    assert!(names.iter().any(|n| n == "usmiech"));
    assert!(names.windows(2).all(|w| w[0] <= w[1]), "manifest must be sorted");
    assert_eq!(names.len(), crate::emotes::names().len());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p repartee emote_manifest_lists_known_names`
Expected: FAIL — `cannot find function emote_manifest_json`.

- [ ] **Step 3: Implement the manifest builder + handlers**

In `src/web/server.rs`:
```rust
/// JSON array of all known emote names (sorted). Cached after first build.
fn emote_manifest_json() -> &'static str {
    use std::sync::LazyLock;
    static MANIFEST: LazyLock<String> = LazyLock::new(|| {
        serde_json::to_string(crate::emotes::names()).unwrap_or_else(|_| "[]".to_owned())
    });
    &MANIFEST
}

async fn emotes_manifest_handler() -> Response {
    (
        StatusCode::OK,
        [
            (axum::http::header::CONTENT_TYPE, "application/json; charset=utf-8"),
            (axum::http::header::CACHE_CONTROL, "public, max-age=3600"),
        ],
        emote_manifest_json().to_owned(),
    )
        .into_response()
}

async fn emote_handler(axum::extract::Path(file): axum::extract::Path<String>) -> Response {
    // `file` is e.g. "usmiech.gif". Strip extension, validate against the registry.
    let name = file.strip_suffix(".gif").unwrap_or(&file);
    match crate::emotes::bytes(name) {
        Some(data) => (
            StatusCode::OK,
            [
                (axum::http::header::CONTENT_TYPE, "image/gif"),
                (axum::http::header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
            ],
            data.into_owned(),
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}
```

- [ ] **Step 4: Register the routes**

In `build_router`, add these BEFORE the catch-all `/{*path}` route:
```rust
        .route("/emotes/manifest.json", get(emotes_manifest_handler))
        .route("/emotes/{file}", get(emote_handler))
```
(matchit prioritizes the static `manifest.json` segment over the `{file}` param, so they coexist.)

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test -p repartee emote_manifest_lists_known_names`
Expected: PASS.

- [ ] **Step 6: Manual route smoke test**

Build and run with web enabled (per existing web docs), then:
```bash
curl -sk https://127.0.0.1:8443/emotes/manifest.json | head -c 200      # JSON array incl. "usmiech"
curl -sk https://127.0.0.1:8443/emotes/usmiech.gif | head -c 3 | xxd    # "GIF"
curl -sko /dev/null -w "%{http_code}\n" https://127.0.0.1:8443/emotes/nope.gif   # 404
```
Expected: manifest JSON, `GIF` magic, `404` for unknown.

> Note: if these routes sit behind auth middleware, decide whether emote assets are public or session-gated. Default: keep them on the same layer as `static_handler` (the WASM assets) so the browser fetches them with the session cookie. If `static_handler` is public, place emote routes alongside it; if gated, gate them too. Mirror whatever `/{*path}` does.

- [ ] **Step 7: Lint + commit**

```bash
make clippy   # 0 warnings
git add src/web/server.rs
git commit -m "feat(web): serve /emotes/{name}.gif and /emotes/manifest.json"
```

---

## Task 6: web-ui — fetch manifest into app state

**Files:**
- Modify: `web-ui/src/state.rs` (add an emote-names signal)
- Modify: `web-ui/src/app.rs` (or wherever startup effects live) to fetch the manifest once

- [ ] **Step 1: Add emote-names state**

In `web-ui/src/state.rs`, add a field to the app state struct holding known emote names, e.g. a
`RwSignal<Vec<String>>` (match the existing signal style in that file):
```rust
    /// Known emote names fetched from /emotes/manifest.json (empty until loaded).
    pub emote_names: RwSignal<Vec<String>>,
```
Initialize it to an empty `Vec` where the state struct is constructed.

- [ ] **Step 2: Fetch the manifest on startup**

In the app's startup effect (where other one-time fetches/initializations run in `app.rs`), add a
fetch of `/emotes/manifest.json` that populates `emote_names`. Use the same HTTP mechanism the
crate already uses (search for existing `fetch`/`gloo_net`/`reqwasm` usage and mirror it). Pseudostructure:
```rust
// inside an async spawn_local on mount:
if let Ok(resp) = /* GET "/emotes/manifest.json" */ {
    if let Ok(names) = resp.json::<Vec<String>>().await {
        state.emote_names.set(names);
    }
}
```

- [ ] **Step 3: Build the WASM frontend to verify it compiles**

Run: `make wasm`
Expected: builds; `static/web/` repopulated. (No unit test here — covered by Task 8 manual verify.)

- [ ] **Step 4: Commit**

```bash
git add web-ui/src/state.rs web-ui/src/app.rs
git commit -m "feat(web-ui): fetch emote name manifest into app state"
```

---

## Task 7: web-ui — render `:name:` as `<img>`

**Files:**
- Modify: `web-ui/src/format.rs` (`StyledSpan` struct; add `emotify_spans`)
- Modify: `web-ui/src/components/chat_view.rs` (`render_styled_text` ~line 645–677)
- Modify: CSS (`.emote` rule)

- [ ] **Step 1: Write the failing test**

In `web-ui/src/format.rs` `#[cfg(test)]` (add one if absent):
```rust
#[cfg(test)]
mod emote_tests {
    use super::*;

    fn known() -> std::collections::HashSet<String> {
        ["usmiech", "lol"].iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn splits_known_emote_into_emote_span() {
        let spans = vec![StyledSpan { text: "hi :lol: x".to_owned(), ..StyledSpan::default() }];
        let out = emotify_spans(spans, &known());
        // -> "hi " (text), emote(lol), " x" (text)
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].text, "hi ");
        assert_eq!(out[1].emote_name.as_deref(), Some("lol"));
        assert_eq!(out[1].text, ":lol:");      // alt/fallback text preserved
        assert_eq!(out[2].text, " x");
    }

    #[test]
    fn unknown_and_smileys_untouched() {
        let spans = vec![StyledSpan { text: ":) :nope:".to_owned(), ..StyledSpan::default() }];
        let out = emotify_spans(spans, &known());
        assert_eq!(out.len(), 1);
        assert!(out[0].emote_name.is_none());
        assert_eq!(out[0].text, ":) :nope:");
    }

    #[test]
    fn empty_name_set_is_noop() {
        let spans = vec![StyledSpan { text: ":lol:".to_owned(), ..StyledSpan::default() }];
        let out = emotify_spans(spans, &std::collections::HashSet::new());
        assert_eq!(out.len(), 1);
        assert!(out[0].emote_name.is_none());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p web-ui emote_tests`
Expected: FAIL — `no field emote_name` / `cannot find function emotify_spans`.

- [ ] **Step 3: Add `emote_name` to `StyledSpan` and implement `emotify_spans`**

In `web-ui/src/format.rs`, add the field to `StyledSpan` (ensure `Default` is derived or updated):
```rust
    /// When set, this span is an emote and should render as <img>; `text` is the
    /// original `:name:` token (used as alt text / fallback).
    pub emote_name: Option<String>,
```

Implement (preserves existing style on the split text runs):
```rust
use std::collections::HashSet;

/// Split any `:name:` tokens (whitelisted by `known`) inside text spans into emote spans.
/// Style spans that carry no plain colon text are passed through untouched.
#[must_use]
pub fn emotify_spans(spans: Vec<StyledSpan>, known: &HashSet<String>) -> Vec<StyledSpan> {
    if known.is_empty() {
        return spans;
    }
    let mut out = Vec::with_capacity(spans.len());
    for span in spans {
        if span.emote_name.is_some() || !span.text.contains(':') {
            out.push(span);
            continue;
        }
        let bytes = span.text.as_bytes();
        let mut text_start = 0usize;
        let mut i = 0usize;
        while i < bytes.len() {
            if bytes[i] == b':' {
                let name_start = i + 1;
                let mut j = name_start;
                while j < bytes.len()
                    && (bytes[j].is_ascii_lowercase() || bytes[j].is_ascii_digit() || bytes[j] == b'_')
                {
                    j += 1;
                }
                if j < bytes.len() && bytes[j] == b':' && j > name_start {
                    let name = &span.text[name_start..j];
                    if known.contains(name) {
                        if text_start < i {
                            out.push(StyledSpan { text: span.text[text_start..i].to_owned(), ..span.clone() });
                        }
                        out.push(StyledSpan {
                            text: span.text[i..=j].to_owned(),    // ":name:"
                            emote_name: Some(name.to_owned()),
                            ..StyledSpan::default()
                        });
                        i = j + 1;
                        text_start = i;
                        continue;
                    }
                }
            }
            i += 1;
        }
        if text_start < span.text.len() {
            out.push(StyledSpan { text: span.text[text_start..].to_owned(), ..span.clone() });
        }
    }
    out
}
```

> If `StyledSpan` does not derive `Default`/`Clone`, add the derives. The `..span.clone()` keeps fg/bg/bold/etc. on the surrounding text runs; emote spans use `StyledSpan::default()` styling.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p web-ui emote_tests`
Expected: PASS (3 tests).

- [ ] **Step 5: Wire into the render pipeline**

In `web-ui/src/components/chat_view.rs` `render_styled_text` (~line 653), insert `emotify_spans`
after `linkify_spans`, threading the known-name set from app state (use the reactive
`emote_names` signal; convert to a `HashSet<String>` once). Then add the `<img>` branch BEFORE the
link/styled branches:
```rust
let spans = format::emotify_spans(
    format::linkify_spans(format::parse_format(text)),
    &emote_set,   // HashSet<String> derived from state.emote_names.get()
);
// ... in the per-span map:
if let Some(name) = &span.emote_name {
    let src = format!("/emotes/{name}.gif");
    view! {
        <img class="emote" src=src alt=span.text.clone() title=span.text.clone() />
    }.into_any()
} else if let Some(url) = span.link {
    /* existing link branch */
} else if span.has_style() {
    /* existing styled branch */
} else {
    /* existing plain branch */
}
```
> `render_styled_text` currently takes `&str`. Add a parameter for the emote set (e.g.
> `fn render_styled_text(text: &str, emote_set: &HashSet<String>)`) and update its call sites to
> pass `&state.emote_names.get().into_iter().collect()` (or a memoized set). If threading state is
> awkward there, derive the set from a `use_context`/signal already in scope — match the crate's
> existing state-access pattern.

- [ ] **Step 6: Add `.emote` CSS**

In the web-ui CSS (search for the existing `.msg-link` rule and add nearby):
```css
.emote {
    height: 1.4em;        /* ~one line tall, inline with text */
    width: auto;
    vertical-align: middle;
    margin: 0 1px;
}
```

- [ ] **Step 7: Build WASM**

Run: `make wasm`
Expected: builds cleanly.

- [ ] **Step 8: Commit**

```bash
git add web-ui/src/format.rs web-ui/src/components/chat_view.rs   # + CSS file
git commit -m "feat(web-ui): render :name: emotes as inline <img>"
```

---

## Task 8: End-to-end web verification

**Files:** none (verification only)

- [ ] **Step 1: Full build**

Run: `make wasm && make release`
Expected: native binary built with embedded WASM + emotes.

- [ ] **Step 2: Manual verify in the browser**

Use the `verify` skill / run the app with the web server enabled, connect to a server, and:
1. Send a message containing `:usmiech: hi :lol:` from repartee.
2. Confirm the web UI shows two inline animated GIFs aligned with the text, and that an unknown
   `:nope:` and a smiley `:)` render as plain text.
3. Confirm a non-repartee IRC client (or the raw log) shows the literal `:usmiech: hi :lol:`.
4. `curl -sk https://127.0.0.1:8443/emotes/usmiech.gif` returns GIF bytes.

Expected: emotes render inline and animated in the browser; tokens travel as plain text over IRC.

- [ ] **Step 3: Final lint/test gate**

Run: `make clippy && make test`
Expected: 0 warnings, all tests pass.

- [ ] **Step 4: Finish the branch**

Use the `superpowers:finishing-a-development-branch` skill to decide merge/PR.

---

## Self-Review

- **Spec coverage:** §4.1 assets/registry → Tasks 1–2; §4.2 tokenizer → Task 3; §4.5 web →
  Tasks 5–7; §4.6 storage/session unchanged → no task needed (verified by design); §4.7 config →
  Task 4; §5 testing → tests in Tasks 2,3,4,5,7 + manual §8. TUI render (§4.3), input UX (§4.4),
  and animation belong to **Plan 2** (out of scope here).
- **Type consistency:** `Segment`, `tokenize`/`tokenize_with`, `names()`, `contains()`, `bytes()`,
  `EmotesConfig`/`RenderMode`, `emotify_spans`, `StyledSpan.emote_name`, `emote_manifest_json` are
  defined once and referenced consistently. The tokenizer charset `[a-z0-9_]` matches in both the
  root crate (`is_name_byte`) and `web-ui` (`emotify_spans`), and is guarded by the
  `all_embedded_names_are_valid_tokens` test.
- **Placeholders:** none — every code step shows full code; the only deliberately open items are
  the exact module-declaration file path, the web-ui HTTP-fetch helper, and the CSS file path,
  each flagged with how to find the right one in-tree.
