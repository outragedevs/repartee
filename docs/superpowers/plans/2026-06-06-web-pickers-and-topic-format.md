# Web Pickers + Topic Format Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix IRC formatting in the web topic bar and add two emote pickers (GG `:name:` GIFs and UTF-8 Unicode) to the Leptos web UI, with command/button/tab triggers.

**Architecture:** All work is in the `web-ui/` WASM crate plus its CSS. Pure logic (palette, strip, filters, insertion math) lives in tested free functions in `format.rs` / new helper modules; Leptos components are thin wrappers. The topic reuses the chat span-renderer via an extracted shared function.

**Tech Stack:** Rust 2024, Leptos 0.7 (CSR), Trunk, the `emojis` crate, existing `web-ui/src/format.rs` parser.

**Build/verify commands** (web crate has no bin; run from repo root):
- Tests: `cargo test -p repartee-web-ui` (the web-ui package) — see Task 0 for exact package name.
- WASM build: `make wasm`
- Clippy: `cd web-ui && cargo clippy --target wasm32-unknown-unknown` (or `make clippy` if it covers the workspace).

---

## File Structure

- `web-ui/src/format.rs` — add `strip_format()`, replace `mirc_color` 0–15 with 0–98 array. (pure, tested)
- `web-ui/src/components/styled.rs` *(new)* — `render_message_text`, `render_topic_text`, `render_spans` (extracted from chat_view). Plus `insert_at_caret` helper logic if pure.
- `web-ui/src/components/chat_view.rs` — call shared `render_message_text`.
- `web-ui/src/components/topic_bar.rs` — call `render_topic_text`.
- `web-ui/src/components/layout.rs` — mobile mini-topic uses `strip_format`; mount `<EmotePicker/>` + `<EmojiPicker/>`.
- `web-ui/src/components/emote_picker.rs` *(new)* — GG modal.
- `web-ui/src/components/emoji_picker.rs` *(new)* — UTF-8 modal.
- `web-ui/src/emoji.rs` *(new)* — emoji grouping/filter over the `emojis` crate (pure, tested).
- `web-ui/src/components/input.rs` — `[GG]`/`[😀]` buttons, `/emoji` intercept, Ctrl+G, emote tab-completion.
- `web-ui/src/components/mod.rs` — export new modules.
- `web-ui/src/state.rs` — `emote_picker_open`, `emoji_picker_open`, plus a way to inject `:name:`/emoji into the input.
- `web-ui/Cargo.toml` — add `emojis`.
- `web-ui/styles/base.css` — `.emote-picker-*`, `.emoji-picker-*`, `.input-emote-btn`, `desktop-only` button.

**Cross-component insertion:** The pickers must insert text into the input textarea. Two viable mechanisms (decide in Task 0 by reading `input.rs`): (a) a shared `RwSignal<Option<String>>` "pending insert" in `AppState` that the input observes and applies at the caret; or (b) the input exposes its `NodeRef<Textarea>` via context. **Plan uses (a)** — simpler, no ref plumbing: pickers push a token, the input's effect inserts it at the caret and clears the signal.

---

## Task 0: Orient (no code)

- [ ] **Step 1:** Read `web-ui/Cargo.toml` for the exact `[package] name` (use it in `cargo test -p <name>`). Read `web-ui/src/components/input.rs` fully — note the textarea `NodeRef`, the send path (`/`→RunCommand), `build_tab_matches`, the `tab_*` signals, and the `❯` prompt span. Read `web-ui/src/state.rs` (signal patterns) and `web-ui/src/components/wizard.rs` + `.wizard-*` CSS (modal pattern). Confirm insertion mechanism (a) is feasible (input can `create_effect` on a state signal).

---

## Task 1: Full mIRC palette + `strip_format` (format.rs)

**Files:** Modify `web-ui/src/format.rs` (incl. its `#[cfg(test)] mod tests`).

- [ ] **Step 1: Write failing tests** (append to `mod tests`):

```rust
#[test]
fn mirc_color_covers_extended_palette() {
    assert_eq!(mirc_color(4), Some("#ff0000"));   // base red
    assert_eq!(mirc_color(16), Some("#470000"));  // extended
    assert_eq!(mirc_color(98), Some("#ffffff"));  // last
    assert_eq!(mirc_color(99), None);             // out of range
}

#[test]
fn strip_format_removes_all_control_codes() {
    let raw = "\x02bold\x03 \x034red\x0f \x1ditalic\x1f \x04ff8800hex %Zaabbcc%N done";
    assert_eq!(strip_format(raw), "bold red italic hex hex done".replace("hex hex", "hex done").to_string().replace(" done done"," done"));
}
```

(The second assertion is fiddly; replace it with the simpler explicit form below in Step 1 — use this instead:)

```rust
#[test]
fn strip_format_removes_all_control_codes() {
    assert_eq!(strip_format("\x02bold\x0f end"), "bold end");
    assert_eq!(strip_format("\x034,2red\x03 plain"), "red plain");
    assert_eq!(strip_format("\x04ff8800hex"), "hex");
    assert_eq!(strip_format("%Zaabbcc%_x%N y"), "x y");
    assert_eq!(strip_format("plain text"), "plain text");
}
```

- [ ] **Step 2: Run, expect FAIL** (`strip_format` undefined; `mirc_color(16)` returns None).
  Run: `cargo test -p <web-ui-pkg> format::tests`

- [ ] **Step 3: Replace `mirc_color` with the full palette array:**

```rust
/// Full mIRC colour palette (indices 0–98), matching the TUI `MIRC_COLORS`.
const MIRC_COLORS: [&str; 99] = [
    "#ffffff", "#000000", "#00007f", "#009300", "#ff0000", "#7f0000", "#9c009c", "#fc7f00",
    "#ffff00", "#00fc00", "#009393", "#00ffff", "#0000fc", "#ff00ff", "#7f7f7f", "#d2d2d2",
    "#470000", "#472100", "#474700", "#324700", "#004700", "#00472c", "#004747", "#002747",
    "#000047", "#2e0047", "#470047", "#47002a", "#740000", "#743a00", "#747400", "#517400",
    "#007400", "#007449", "#007474", "#004074", "#000074", "#4b0074", "#740074", "#740045",
    "#b50000", "#b56300", "#b5b500", "#7db500", "#00b500", "#00b571", "#00b5b5", "#0063b5",
    "#0000b5", "#7500b5", "#b500b5", "#b5006b", "#ff0000", "#ff8c00", "#ffff00", "#b2ff00",
    "#00ff00", "#00ffa0", "#00ffff", "#008cff", "#0000ff", "#a500ff", "#ff00ff", "#ff0098",
    "#ff5959", "#ffb459", "#ffff71", "#cfff60", "#6fff6f", "#65ffc9", "#6dffff", "#59b4ff",
    "#5959ff", "#c459ff", "#ff66ff", "#ff59bc", "#ff9c9c", "#ffd39c", "#ffff9c", "#e2ff9c",
    "#9cff9c", "#9cffdb", "#9cffff", "#9cd3ff", "#9c9cff", "#dc9cff", "#ff9cff", "#ff94d3",
    "#000000", "#131313", "#282828", "#363636", "#4d4d4d", "#656565", "#818181", "#9f9f9f",
    "#bcbcbc", "#e2e2e2", "#ffffff",
];

/// Convert a mIRC colour code (0-98) to a CSS hex colour.
fn mirc_color(code: u8) -> Option<&'static str> {
    MIRC_COLORS.get(code as usize).copied()
}
```

- [ ] **Step 4: Add `strip_format`** (mirror `parse_format` control handling but emit only text). Place after `parse_format`:

```rust
/// Remove all mIRC/irssi formatting control codes, returning the visible text.
/// Used for places that need plain text (e.g. the mobile topic breadcrumb).
pub fn strip_format(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < len {
        match chars[i] {
            '%' if i + 1 < len => match chars[i + 1] {
                'Z' | 'z' if i + 8 <= len => i += 8,
                'N' | 'n' | '_' | 'u' | 'U' | 'i' | 'I' | 'd' => i += 2,
                '%' => { out.push('%'); i += 2; }
                c if irssi_color(c).is_some() => i += 2,
                c => { out.push('%'); out.push(c); i += 2; }
            },
            '\x02' | '\x0F' | '\x16' | '\x1D' | '\x1E' | '\x1F' => i += 1,
            '\x03' => {
                i += 1;
                let mut digits = 0;
                while i < len && chars[i].is_ascii_digit() && digits < 2 { i += 1; digits += 1; }
                if digits > 0 && i < len && chars[i] == ',' {
                    i += 1;
                    let mut d2 = 0;
                    while i < len && chars[i].is_ascii_digit() && d2 < 2 { i += 1; d2 += 1; }
                }
            }
            '\x04' => {
                i += 1;
                if i + 6 <= len && chars[i..i + 6].iter().all(|c| c.is_ascii_hexdigit()) { i += 6; }
            }
            ch => { out.push(ch); i += 1; }
        }
    }
    out
}
```

- [ ] **Step 5: Run, expect PASS.** `cargo test -p <web-ui-pkg> format::tests`
- [ ] **Step 6: Commit** `fix(web-ui): full mIRC palette (0-98) + strip_format helper`

---

## Task 2: Extract shared span renderer (styled.rs)

**Files:** Create `web-ui/src/components/styled.rs`; modify `web-ui/src/components/mod.rs`, `chat_view.rs`.

- [ ] **Step 1:** Read `chat_view.rs::render_styled_text` (≈665-706) to copy its exact body.
- [ ] **Step 2: Create `styled.rs`** with the extracted logic:

```rust
use leptos::prelude::*;
use crate::format;

/// Render already-parsed spans to views (span / link / emote <img>).
/// Mirrors the original chat_view::render_styled_text body.
pub fn render_spans(spans: Vec<format::StyledSpan>) -> Vec<AnyView> {
    spans.into_iter().map(|span| {
        // <COPY the exact match arms from chat_view: emote_name => <img>, link => <a>, styled => <span style>, plain => <span>>
        todo!("paste exact arms from chat_view::render_styled_text")
    }).collect()
}

/// Full message pipeline: parse → linkify → optional emotify → render.
pub fn render_message_text(text: &str, emotes_on: bool) -> Vec<AnyView> {
    let base = format::linkify_spans(format::parse_format(text));
    let spans = if emotes_on { format::emotify_spans(base) } else { base };
    render_spans(spans)
}

/// Topic pipeline: parse → linkify (NO emote expansion).
pub fn render_topic_text(text: &str) -> Vec<AnyView> {
    render_spans(format::linkify_spans(format::parse_format(text)))
}
```

> Implementation note: replace the `todo!` by moving the real closure body from `chat_view::render_styled_text`. Keep behaviour identical.

- [ ] **Step 3:** In `chat_view.rs`, replace the body of `render_styled_text` with `crate::components::styled::render_message_text(text, emotes_on)` (or delete it and call the shared fn at call sites). In `mod.rs` add `pub mod styled;`.
- [ ] **Step 4: Build check** `make wasm` (no behaviour change expected). Visually unchanged.
- [ ] **Step 5: Commit** `refactor(web-ui): extract shared styled-text renderer`

---

## Task 3: Topic formatting (topic_bar.rs + mobile)

**Files:** Modify `web-ui/src/components/topic_bar.rs`, `web-ui/src/components/layout.rs`.

- [ ] **Step 1:** In `topic_bar.rs`, replace `<span>{topic}</span>` (line ~22) with:

```rust
<span>{crate::components::styled::render_topic_text(&topic)}</span>
```

- [ ] **Step 2:** In `layout.rs` mobile mini-topic (~159-167), wrap the topic in `strip_format` before truncation:

```rust
let topic_full = crate::format::strip_format(b.topic.as_deref().unwrap_or(""));
let topic = topic_full.as_str();
let topic_end = topic.char_indices().nth(40).map_or(topic.len(), |(i, _)| i);
let topic_short = &topic[..topic_end];
```

(Adjust to match the existing local names/limit; keep the char-boundary slice.)

- [ ] **Step 3: Build** `make wasm`. Manually verify a coloured/bold topic renders styled in the desktop bar and clean (no control bytes) in the mobile breadcrumb.
- [ ] **Step 4: Commit** `fix(web-ui): render IRC formatting in channel topic`

---

## Task 4: State signals + input insertion channel

**Files:** Modify `web-ui/src/state.rs`, `web-ui/src/components/input.rs`.

- [ ] **Step 1:** In `AppState` (state.rs), add fields next to `wizard_open`:

```rust
pub emote_picker_open: RwSignal<bool>,
pub emoji_picker_open: RwSignal<bool>,
/// A token to insert into the input at the caret (`:name:` or a Unicode emoji).
/// The input component consumes and clears it.
pub pending_insert: RwSignal<Option<String>>,
```

Initialise them (`RwSignal::new(false)` / `RwSignal::new(None)`) wherever `wizard_open` is initialised.

- [ ] **Step 2:** In `input.rs`, after the textarea `NodeRef` is set up, add an effect that applies a pending insert at the caret:

```rust
Effect::new(move |_| {
    if let Some(token) = state.pending_insert.get() {
        if let Some(ta) = input_ref.get() {
            // read selectionStart, splice token into value, restore caret after token, refocus
            let el: web_sys::HtmlTextAreaElement = ta;
            let val = el.value();
            let pos = el.selection_start().ok().flatten().unwrap_or(val.len() as u32) as usize;
            let pos = val.char_indices().map(|(i,_)| i).chain([val.len()]).min_by_key(|&i| (i as i64 - pos as i64).abs()).unwrap_or(val.len());
            let mut new_val = String::with_capacity(val.len() + token.len());
            new_val.push_str(&val[..pos]); new_val.push_str(&token); new_val.push_str(&val[pos..]);
            el.set_value(&new_val);
            let caret = (pos + token.len()) as u32;
            let _ = el.set_selection_start(Some(caret));
            let _ = el.set_selection_end(Some(caret));
            let _ = el.focus();
        }
        state.pending_insert.set(None);
    }
});
```

> Note: exact `web_sys` calls may need feature flags already enabled (textarea is used elsewhere). Adjust types to the existing `input_ref` type. If `selection_start` byte/char index mismatch is a concern, the nearest-char-boundary clamp above guards `str` slicing.

- [ ] **Step 3: Build** `make wasm` (no visible change yet).
- [ ] **Step 4: Commit** `feat(web-ui): picker state + input insertion channel`

---

## Task 5: GG emote picker component + CSS

**Files:** Create `web-ui/src/components/emote_picker.rs`; modify `mod.rs`, `layout.rs`, `styles/base.css`.

- [ ] **Step 1: Pure filter test** — create a `filter_emotes` free fn and test it. In `emote_picker.rs`:

```rust
/// Indices into EMOTE_NAMES whose name contains the (lowercased) needle.
pub fn filter_emotes(needle: &str) -> Vec<usize> {
    let n = needle.to_ascii_lowercase();
    crate::emotes::EMOTE_NAMES.iter().enumerate()
        .filter(|(_, name)| n.is_empty() || name.to_ascii_lowercase().contains(&n))
        .map(|(i, _)| i).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn filter_empty_returns_all() {
        assert_eq!(filter_emotes("").len(), crate::emotes::EMOTE_NAMES.len());
    }
    #[test]
    fn filter_substring_narrows() {
        let all = filter_emotes("");
        let some = filter_emotes("usm");
        assert!(some.len() <= all.len());
    }
}
```

- [ ] **Step 2: Run, expect FAIL→PASS** after adding the fn. `cargo test -p <web-ui-pkg> emote_picker`
- [ ] **Step 3: Component** (modal, reuse `.wizard-backdrop`/`.wizard-modal`):

```rust
#[component]
pub fn EmotePicker() -> impl IntoView {
    let state = use_context::<AppState>().unwrap();
    let open = state.emote_picker_open;
    let filter = RwSignal::new(String::new());
    let pick = move |idx: usize| {
        let name = crate::emotes::EMOTE_NAMES[idx];
        state.pending_insert.set(Some(format!(":{name}: ")));
        open.set(false);
        filter.set(String::new());
    };
    view! {
        <Show when=move || open.get() fallback=|| ()>
            <div class="wizard-backdrop" on:click=move |_| open.set(false)></div>
            <div class="emote-picker-modal">
                <input class="emote-picker-filter" placeholder="filter emotes…"
                    prop:value=move || filter.get()
                    on:input=move |ev| filter.set(event_target_value(&ev))
                    on:keydown=move |ev| if ev.key()=="Escape" { open.set(false); } />
                <div class="emote-picker-grid">
                    {move || filter_emotes(&filter.get()).into_iter().map(|idx| {
                        let stem = crate::emotes::stem_for(crate::emotes::EMOTE_NAMES[idx]).unwrap_or(crate::emotes::EMOTE_NAMES[idx]);
                        let name = crate::emotes::EMOTE_NAMES[idx];
                        view! {
                            <button class="emote-picker-cell" title=name on:click=move |_| pick(idx)>
                                <img src=format!("/emotes/{stem}.gif") loading="lazy" alt=name />
                                <span class="emote-picker-name">{name}</span>
                            </button>
                        }
                    }).collect_view()}
                </div>
            </div>
        </Show>
    }
}
```

> Verify `crate::emotes::stem_for` / `EMOTE_NAMES` signatures against `web-ui/src/emotes.rs` (Task 0). Adjust if `stem_for` returns `Option<&str>` differently.

- [ ] **Step 4:** `mod.rs`: `pub mod emote_picker;`. `layout.rs`: add `<EmotePicker/>` near `<ServerWizard/>` (both desktop and mobile trees, or once at root — match wizard placement).
- [ ] **Step 5: CSS** in `base.css`:

```css
.emote-picker-modal{position:fixed;left:50%;top:50%;transform:translate(-50%,-50%);z-index:41;
  width:min(560px,92vw);max-height:70vh;display:flex;flex-direction:column;gap:8px;padding:12px;
  background:var(--bg-alt);border:1px solid var(--fg-muted);border-radius:8px;}
.emote-picker-filter{font-family:inherit;padding:6px 8px;background:var(--bg);color:var(--fg);
  border:1px solid var(--fg-muted);border-radius:4px;}
.emote-picker-grid{overflow-y:auto;display:grid;grid-template-columns:repeat(auto-fill,minmax(72px,1fr));gap:6px;}
.emote-picker-cell{display:flex;flex-direction:column;align-items:center;gap:2px;padding:6px;cursor:pointer;
  background:transparent;border:1px solid transparent;border-radius:6px;color:var(--fg);}
.emote-picker-cell:hover{border-color:var(--accent);background:var(--bg);}
.emote-picker-cell img{width:32px;height:32px;object-fit:contain;}
.emote-picker-name{font-size:11px;color:var(--fg-muted);overflow:hidden;text-overflow:ellipsis;max-width:68px;white-space:nowrap;}
```

- [ ] **Step 6: Build** `make wasm`; verify modal opens (temporarily set signal true) and inserting works.
- [ ] **Step 7: Commit** `feat(web-ui): GG emote picker modal`

---

## Task 6: Input buttons + `/emoji` intercept + Ctrl+G

**Files:** Modify `web-ui/src/components/input.rs`, `styles/base.css`.

- [ ] **Step 1:** Add buttons left of the `❯` prompt span (match the existing markup; the GG button always, the emoji button `desktop-only`):

```rust
<button type="button" class="input-emote-btn" title="GG emotes (/emoji)"
    on:click=move |_| state.emote_picker_open.set(true)>"GG"</button>
<button type="button" class="input-emote-btn desktop-only" title="Emoji"
    on:click=move |_| state.emoji_picker_open.set(true)>"\u{1F600}"</button>
```

- [ ] **Step 2:** In the send handler, intercept emote commands before dispatching. Where `trimmed.starts_with('/')` is handled:

```rust
let lower = trimmed.to_ascii_lowercase();
if lower == "/emoji" || lower == "/emote" || lower == "/emotes" {
    state.emote_picker_open.set(true);
    // clear the input, do not send
    set_input_value_empty(); // mirror existing clear logic
    return;
}
```

(Use the same input-clearing the normal send path uses.)

- [ ] **Step 3:** In the textarea `on:keydown`, add Ctrl+G:

```rust
if ev.ctrl_key() && ev.key() == "g" {
    ev.prevent_default();
    state.emote_picker_open.set(true);
    return;
}
```

- [ ] **Step 4: CSS:**

```css
.input-emote-btn{font-family:inherit;font-size:13px;padding:0 8px;margin-right:4px;cursor:pointer;
  background:transparent;border:1px solid var(--fg-muted);border-radius:4px;color:var(--fg-muted);}
.input-emote-btn:hover{color:var(--fg);border-color:var(--accent);}
```

- [ ] **Step 5: Build** `make wasm`; verify `[GG]`/`[😀]` show on desktop, only `[GG]` < 768px, `/emoji` opens the picker, Ctrl+G opens it.
- [ ] **Step 6: Commit** `feat(web-ui): input emote buttons, /emoji intercept, Ctrl+G`

---

## Task 7: Emote tab-completion

**Files:** Modify `web-ui/src/components/input.rs` (`build_tab_matches` + helpers).

- [ ] **Step 1: Pure test** for an emote-completion helper. Add a free fn `emote_tab_matches(word: &str) -> Vec<String>`:

```rust
/// `:usm` → [":usmiech: ", ":usmialy: ", ...]. Empty unless word is a single
/// leading colon followed by a non-empty prefix with no closing colon.
pub fn emote_tab_matches(word: &str) -> Vec<String> {
    let Some(rest) = word.strip_prefix(':') else { return Vec::new(); };
    if rest.is_empty() || rest.contains(':') { return Vec::new(); }
    let p = rest.to_ascii_lowercase();
    crate::emotes::EMOTE_NAMES.iter()
        .filter(|n| n.to_ascii_lowercase().starts_with(&p))
        .map(|n| format!(":{n}: "))
        .collect()
}

#[cfg(test)]
mod emote_tab_tests {
    use super::*;
    #[test] fn no_colon_no_matches() { assert!(emote_tab_matches("usm").is_empty()); }
    #[test] fn closing_colon_no_matches() { assert!(emote_tab_matches(":usm:").is_empty()); }
    #[test] fn prefix_matches_have_trailing_space() {
        let m = emote_tab_matches(":usm");
        assert!(m.iter().all(|s| s.starts_with(':') && s.ends_with(": ")));
    }
}
```

- [ ] **Step 2: Run FAIL→PASS.** `cargo test -p <web-ui-pkg> emote_tab`
- [ ] **Step 3:** Wire into `build_tab_matches`: before the nick-completion branch, if `emotes_enabled` and the current word yields `emote_tab_matches(word)` non-empty, return those. Keep cycling behaviour identical to other matches.
- [ ] **Step 4: Build** `make wasm`; verify `:usm`+Tab cycles `:usmiech: ` etc.
- [ ] **Step 5: Commit** `feat(web-ui): :name: emote tab-completion`

---

## Task 8: UTF-8 emoji picker

**Files:** `web-ui/Cargo.toml`; create `web-ui/src/emoji.rs`, `web-ui/src/components/emoji_picker.rs`; modify `mod.rs` (top-level + components), `layout.rs`.

- [ ] **Step 1:** Add to `web-ui/Cargo.toml` `[dependencies]`: `emojis = "0.6"` (verify latest on crates.io at impl time; pin like other deps).
- [ ] **Step 2: emoji.rs helper + tests:**

```rust
//! Thin helpers over the `emojis` crate for the web picker.

/// A category shown as a tab in the picker.
pub const GROUPS: &[emojis::Group] = &[
    emojis::Group::SmileysAndEmotion,
    emojis::Group::PeopleAndBody,
    emojis::Group::AnimalsAndNature,
    emojis::Group::FoodAndDrink,
    emojis::Group::TravelAndPlaces,
    emojis::Group::Activities,
    emojis::Group::Objects,
    emojis::Group::Symbols,
    emojis::Group::Flags,
];

/// Emoji chars in a group.
pub fn in_group(g: emojis::Group) -> Vec<&'static str> {
    g.emojis().map(|e| e.as_str()).collect()
}

/// Filter all emoji by a name/shortcode substring (lowercased).
pub fn search(needle: &str) -> Vec<&'static str> {
    let n = needle.to_ascii_lowercase();
    emojis::iter()
        .filter(|e| e.name().to_ascii_lowercase().contains(&n)
            || e.shortcodes().any(|s| s.to_ascii_lowercase().contains(&n)))
        .map(|e| e.as_str())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn search_finds_grinning() { assert!(search("grinning").iter().any(|&e| e == "\u{1F600}")); }
    #[test] fn groups_nonempty() { assert!(!in_group(emojis::Group::FoodAndDrink).is_empty()); }
}
```

- [ ] **Step 3: Run FAIL→PASS.** `cargo test -p <web-ui-pkg> emoji::`. Add `pub mod emoji;` to the crate root (`web-ui/src/main.rs` or `lib.rs` — match where modules are declared).
- [ ] **Step 4: Component** `emoji_picker.rs` (modal; selected group signal + filter):

```rust
#[component]
pub fn EmojiPicker() -> impl IntoView {
    let state = use_context::<AppState>().unwrap();
    let open = state.emoji_picker_open;
    let filter = RwSignal::new(String::new());
    let group = RwSignal::new(0usize);
    let pick = move |ch: &'static str| { state.pending_insert.set(Some(ch.to_string())); open.set(false); filter.set(String::new()); };
    view! {
        <Show when=move || open.get() fallback=|| ()>
            <div class="wizard-backdrop" on:click=move |_| open.set(false)></div>
            <div class="emoji-picker-modal">
                <input class="emote-picker-filter" placeholder="search emoji…"
                    prop:value=move || filter.get()
                    on:input=move |ev| filter.set(event_target_value(&ev))
                    on:keydown=move |ev| if ev.key()=="Escape" { open.set(false); } />
                <div class="emoji-picker-tabs">
                    {crate::emoji::GROUPS.iter().enumerate().map(|(i,_g)| view!{
                        <button class="emoji-tab" on:click=move |_| group.set(i)>{i+1}</button>
                    }).collect_view()}
                </div>
                <div class="emoji-picker-grid">
                    {move || {
                        let f = filter.get();
                        let items = if f.is_empty() { crate::emoji::in_group(crate::emoji::GROUPS[group.get()]) } else { crate::emoji::search(&f) };
                        items.into_iter().map(|ch| view!{ <button class="emoji-cell" on:click=move |_| pick(ch)>{ch}</button> }).collect_view()
                    }}
                </div>
            </div>
        </Show>
    }
}
```

- [ ] **Step 5:** `components/mod.rs`: `pub mod emoji_picker;`. `layout.rs`: mount `<EmojiPicker/>` next to `<EmotePicker/>`.
- [ ] **Step 6: CSS:**

```css
.emoji-picker-modal{position:fixed;left:50%;top:50%;transform:translate(-50%,-50%);z-index:41;
  width:min(420px,92vw);max-height:70vh;display:flex;flex-direction:column;gap:8px;padding:12px;
  background:var(--bg-alt);border:1px solid var(--fg-muted);border-radius:8px;}
.emoji-picker-tabs{display:flex;gap:4px;flex-wrap:wrap;}
.emoji-tab{cursor:pointer;background:var(--bg);border:1px solid var(--fg-muted);border-radius:4px;color:var(--fg-muted);padding:2px 8px;}
.emoji-picker-grid{overflow-y:auto;display:grid;grid-template-columns:repeat(auto-fill,minmax(34px,1fr));gap:4px;}
.emoji-cell{cursor:pointer;background:transparent;border:none;font-size:22px;line-height:1;padding:4px;border-radius:4px;}
.emoji-cell:hover{background:var(--bg);}
```

- [ ] **Step 7: Build** `make wasm`; verify `[😀]` (desktop) opens picker, tabs switch groups, search filters, click inserts the literal emoji.
- [ ] **Step 8: Commit** `feat(web-ui): UTF-8 emoji picker (desktop)`

---

## Task 9: Full verification

- [ ] **Step 1:** `cargo test -p <web-ui-pkg>` — all green. Also `make test` (workspace) green.
- [ ] **Step 2:** Clippy on the web crate (wasm target) — 0 warnings. `make clippy` if it covers web-ui.
- [ ] **Step 3:** `make clean && make wasm && make release` — clean full build; confirm `static/web` regenerated and committed if changed (commit regenerated assets).
- [ ] **Step 4:** Runtime sanity: start the web server, `curl -I https://<host>/fonts/FiraCodeNerdFontMono-Regular.ttf` → 200 `font/ttf` (font verification for #1).
- [ ] **Step 5: Commit** any regenerated `static/web/` assets: `chore(web): rebuild web UI assets`.

---

## Self-review notes
- Spec #1 (font) → Task 9 Step 4 (verify only). ✓
- Spec #2 (topic + palette) → Tasks 1, 2, 3. ✓
- Spec #3a (GG picker + triggers) → Tasks 4, 5, 6, 7. ✓
- Spec #3b (UTF-8 picker, desktop-only) → Tasks 4, 8. ✓
- Insertion mechanism consistent (`pending_insert` signal) across Tasks 4/5/8. ✓
- `emote_tab_matches` / `filter_emotes` / `strip_format` / `mirc_color` names used consistently. ✓
- Open risk flagged: exact `web_sys` textarea caret API and `emotes::stem_for` signature must be confirmed in Task 0 against real code; `emojis` crate version/Group variant names verified at impl time.
