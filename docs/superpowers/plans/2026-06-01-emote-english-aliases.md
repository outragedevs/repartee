# Emote English Aliases + Picker Language Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `:name:` accept both the Polish stem and an English alias for every emote, and add an `emotes.lang` (en|pl, default en) setting controlling picker/insert language.

**Architecture:** A single source of truth `assets/emotes/aliases.tsv` (`polish<TAB>english`) is parsed at runtime in the native crate (`include_str!`) and codegen'd by the web crate's `build.rs`. `emote_index` keeps indexing Polish stems (= file names), so the animator/renderer are unchanged; resolution helpers map either-language names to that index. The server keeps serving Polish-stem GIFs; the web frontend resolves English→stem before building `<img src>`.

**Tech Stack:** Rust 2024, rust-embed, image, axum, Leptos (WASM).

**Spec:** `docs/superpowers/specs/2026-06-01-emote-english-aliases-design.md`

**Conventions:** color-eyre, tracing, `crate::constants::APP_NAME`, clippy pedantic+nursery=warn / 0-warnings for new code, build via `make`. Commit per task.

---

## File Structure

| File | Responsibility | Create/Modify |
|---|---|---|
| `assets/emotes/aliases.tsv` | Source-of-truth PL→EN map (183 rows) | Create |
| `src/emotes/mod.rs` | Parse aliases; `resolve`/`english_label`/`display_name`/`tag_names`; update `contains`/`bytes` | Modify |
| `src/ui/message_line.rs` | `emotify_message_text` uses `resolve` (accept EN) | Modify |
| `src/config/mod.rs` | `EmoteLang` enum + `EmotesConfig.lang` | Modify |
| `src/commands/settings.rs` | `emotes.lang` get/set + settable path | Modify |
| `src/app/input.rs` | `insert_emote_by_index` lang-aware; picker key insert | Modify |
| `src/ui/emote_picker.rs` | Label per lang; filter matches either language | Modify |
| `src/ui/input.rs` | `emote_completions` offers PL+EN | Modify |
| `src/commands/handlers_ui.rs` | `cmd_emote` resolves either language | Modify |
| `web-ui/build.rs` | Parse `aliases.tsv` → union name list + EN→stem map | Modify |
| `web-ui/src/emotes.rs` | `is_emote` (either lang) + `stem_for` | Modify |
| `web-ui/src/format.rs` | `emotify_spans` stores the stem in `emote_name` | Modify |
| `web-ui/src/components/chat_view.rs` | `<img src>` already uses `emote_name` (now a stem) — verify | Modify (verify) |

---

## Task 1: Add the alias source file + integrity test

**Files:**
- Create: `assets/emotes/aliases.tsv`
- Test: inline in `src/emotes/mod.rs`

- [ ] **Step 1: Create the TSV**

Write `assets/emotes/aliases.tsv` with exactly the 183 `polish<TAB>english` rows
from the spec's §7 table (tab-separated, one per line, sorted by Polish stem).
The validated content is in the spec; copy it verbatim. First/last lines:
```
3m_sie	take_care
...
zygi	puking
```

- [ ] **Step 2: Verify integrity from the shell**

```bash
cd /home/projekt/dev/repartee
test "$(wc -l < assets/emotes/aliases.tsv)" = 183 && echo "183 OK"
# every gif stem has a row, and vice-versa (empty output = match):
comm -3 <(for f in assets/emotes/*.gif; do basename "$f" .gif; done | sort) <(cut -f1 assets/emotes/aliases.tsv | sort)
# unique English aliases (empty = unique):
cut -f2 assets/emotes/aliases.tsv | sort | uniq -d
# English alias charset (empty = all valid):
cut -f2 assets/emotes/aliases.tsv | grep -vE '^[a-z0-9_]+$'
```
Expected: "183 OK", and three empty outputs.

- [ ] **Step 3: Commit**

```bash
git add assets/emotes/aliases.tsv
git commit -m "assets: add PL->EN emote alias table (183 rows)"
```

---

## Task 2: Native registry — parse aliases + resolution helpers

**Files:**
- Modify: `src/emotes/mod.rs`

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module in `src/emotes/mod.rs`:
```rust
#[test]
fn aliases_cover_every_emote() {
    // One alias row per emote name; both directions resolve to the same index.
    for (i, n) in names().iter().enumerate() {
        let idx = u32::try_from(i).unwrap();
        assert_eq!(resolve(n), Some(idx), "PL stem {n} must resolve to its own index");
        let en = english_label(idx).expect("every emote has an English label");
        assert_eq!(resolve(en), Some(idx), "EN alias {en} must resolve to {n}'s index");
    }
}

#[test]
fn resolve_both_languages_and_unknown() {
    let smile = resolve("usmiech").expect("usmiech");
    assert_eq!(resolve("smile"), Some(smile), ":smile: and :usmiech: are the same emote");
    assert!(resolve("definitely_not_an_emote").is_none());
}

#[test]
fn bytes_resolves_english_to_gif() {
    assert!(bytes("smile").unwrap().starts_with(b"GIF"));
    assert!(bytes("usmiech").unwrap().starts_with(b"GIF"));
}

#[test]
fn display_name_follows_lang() {
    let i = resolve("usmiech").unwrap();
    assert_eq!(display_name(i, Lang::En), "smile");
    assert_eq!(display_name(i, Lang::Pl), "usmiech");
}

#[test]
fn tag_names_includes_both_languages() {
    let tags = tag_names();
    assert!(tags.contains(&"smile"));
    assert!(tags.contains(&"usmiech"));
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p repartee emotes::tests -- --nocapture`
Expected: FAIL — `resolve`, `english_label`, `display_name`, `Lang`, `tag_names` undefined.

- [ ] **Step 3: Implement parsing + helpers**

In `src/emotes/mod.rs`, after the `NAMES` `LazyLock`:
```rust
/// Picker/insert language (mirrors `config::EmoteLang` but lives here so the
/// registry stays UI/config-agnostic).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    En,
    Pl,
}

/// `english_alias[i]` is the English alias for `names()[i]` (may equal the stem
/// for loanword emotes like `lol`, `ok`, `8p`). Parsed from `aliases.tsv`.
static ENGLISH: LazyLock<Vec<&'static str>> = LazyLock::new(|| {
    let raw = include_str!("../../assets/emotes/aliases.tsv");
    // stem -> english, then align to NAMES order so english[i] matches names()[i].
    let mut map: std::collections::HashMap<&str, &str> = std::collections::HashMap::new();
    for line in raw.lines() {
        if let Some((pl, en)) = line.split_once('\t') {
            map.insert(pl.trim(), en.trim());
        }
    }
    NAMES.iter().map(|n| *map.get(n.as_str()).unwrap_or(&n.as_str())).collect()
});

/// English alias -> index into `NAMES`, for resolving English tags.
static EN_TO_INDEX: LazyLock<std::collections::HashMap<&'static str, u32>> = LazyLock::new(|| {
    ENGLISH
        .iter()
        .enumerate()
        .map(|(i, en)| (*en, u32::try_from(i).unwrap_or(0)))
        .collect()
});

/// All valid `:name:` tags (Polish stems + English aliases), sorted, deduped.
static TAG_NAMES: LazyLock<Vec<&'static str>> = LazyLock::new(|| {
    let mut v: Vec<&'static str> = NAMES.iter().map(String::as_str).collect();
    v.extend(ENGLISH.iter().copied());
    v.sort_unstable();
    v.dedup();
    v
});

/// English alias for the emote at `index` (equals the stem when there is no
/// distinct alias).
#[must_use]
pub fn english_label(index: u32) -> Option<&'static str> {
    ENGLISH.get(index as usize).copied()
}

/// Resolve a tag name (Polish stem OR English alias) to its canonical index.
#[must_use]
pub fn resolve(name: &str) -> Option<u32> {
    if let Ok(i) = NAMES.binary_search_by(|n| n.as_str().cmp(name)) {
        return u32::try_from(i).ok();
    }
    EN_TO_INDEX.get(name).copied()
}

/// The name to display/insert for `index` in the given language.
#[must_use]
pub fn display_name(index: u32, lang: Lang) -> &'static str {
    match lang {
        Lang::En => english_label(index).unwrap_or("?"),
        Lang::Pl => names().get(index as usize).map_or("?", String::as_str),
    }
}

/// All valid tag names (both languages), sorted.
#[must_use]
pub fn tag_names() -> &'static [&'static str] {
    &TAG_NAMES
}
```

Replace `contains` and `bytes` to go through `resolve`:
```rust
#[must_use]
pub fn contains(name: &str) -> bool {
    resolve(name).is_some()
}

#[must_use]
pub fn bytes(name: &str) -> Option<Cow<'static, [u8]>> {
    let idx = resolve(name)?;
    let stem = NAMES.get(idx as usize)?;
    EmoteAssets::get(&format!("{stem}.gif")).map(|f| f.data)
}
```

> `NAMES` must be referenced by `ENGLISH`/`EN_TO_INDEX`/`TAG_NAMES`; they're all
> `LazyLock` so ordering is fine. The `unwrap_or(&n.as_str())` makes a missing
> alias fall back to the stem (defensive; the Task 1 integrity test guarantees
> full coverage).

- [ ] **Step 4: Run tests**

Run: `cargo test -p repartee emotes -- --nocapture`
Expected: PASS (existing + 5 new).

- [ ] **Step 5: Lint + commit**

```bash
make clippy   # 0 warnings in src/emotes/mod.rs
git add src/emotes/mod.rs
git commit -m "feat(emotes): parse PL->EN aliases; resolve/english_label/display_name/tag_names"
```

---

## Task 3: TUI rendering accepts English tags

**Files:**
- Modify: `src/ui/message_line.rs`
- Test: inline

`emotify_message_text` currently does `names().binary_search_by(...)` to get the
placeholder index; switch it to `emotes::resolve` so English tags also become
placeholders. (`emotes::parse::tokenize` already calls `emotes::contains`, which
now resolves both, so the tokenizer needs no change.)

- [ ] **Step 1: Write the failing test**

In `message_line.rs` `emote_tests`:
```rust
#[test]
fn english_alias_becomes_placeholder() {
    use crate::ui::emote_layout::{EMOTE_COLS, decode_placeholder_index};
    let out = emotify_message_text("hi :smile: x", true);
    let placeholders = out.chars().filter(|c| decode_placeholder_index(*c).is_some()).count();
    assert_eq!(placeholders, EMOTE_COLS, ":smile: must become a placeholder");
    assert!(!out.contains(":smile:"));
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p repartee message_line::emote_tests::english_alias_becomes_placeholder`
Expected: FAIL — `:smile:` left as text (binary_search over Polish stems misses it).

- [ ] **Step 3: Implement**

In `emotify_message_text`, replace the index lookup:
```rust
Segment::Emote(name) => {
    if let Some(idx) = emotes::resolve(&name) {
        out.push_str(&placeholder_for_index(idx));
    } else {
        out.push(':');
        out.push_str(&name);
        out.push(':');
    }
}
```
(Remove the now-unused `let names = emotes::names();` binding if present.)

- [ ] **Step 4: Run tests**

Run: `cargo test -p repartee message_line`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/ui/message_line.rs
git commit -m "feat(emotes): render English-alias :tags: inline in the TUI"
```

---

## Task 4: `emotes.lang` config

**Files:**
- Modify: `src/config/mod.rs`

- [ ] **Step 1: Write the failing test**

In `config` tests:
```rust
#[test]
fn emotes_lang_default_and_parse() {
    assert_eq!(AppConfig::default().emotes.lang, EmoteLang::En);
    let p: AppConfig = toml::from_str("[emotes]\nlang = \"pl\"\n").unwrap();
    assert_eq!(p.emotes.lang, EmoteLang::Pl);
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p repartee emotes_lang_default_and_parse`
Expected: FAIL — no `lang` / `EmoteLang`.

- [ ] **Step 3: Implement**

Add near `RenderMode` in `src/config/mod.rs`:
```rust
/// Picker / autocomplete-insert preview language for emotes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EmoteLang {
    #[default]
    En,
    Pl,
}

impl EmoteLang {
    /// Map to the registry's language enum.
    #[must_use]
    pub fn to_registry(self) -> crate::emotes::Lang {
        match self {
            Self::En => crate::emotes::Lang::En,
            Self::Pl => crate::emotes::Lang::Pl,
        }
    }
}
```
Add `pub lang: EmoteLang,` to `EmotesConfig` and `lang: EmoteLang::En` to its
`Default`.

- [ ] **Step 4: Run tests + commit**

Run: `cargo test -p repartee emotes_lang_default_and_parse` → PASS
```bash
make clippy
git add src/config/mod.rs
git commit -m "feat(config): emotes.lang (en|pl, default en)"
```

---

## Task 5: `/set emotes.lang`

**Files:**
- Modify: `src/commands/settings.rs`

- [ ] **Step 1: Write the failing test**

In `settings` tests, extend `get_set_emotes` (or add):
```rust
#[test]
fn get_set_emotes_lang() {
    let mut config = default_config();
    assert_eq!(get_config_value(&config, "emotes.lang").unwrap().value, "en");
    set_config_value(&mut config, "emotes.lang", "pl").unwrap();
    assert_eq!(config.emotes.lang, crate::config::EmoteLang::Pl);
    assert!(set_config_value(&mut config, "emotes.lang", "fr").is_err());
    assert!(BASE_PATHS.contains(&"emotes.lang"));
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p repartee get_set_emotes_lang`
Expected: FAIL — `emotes.lang` not gettable/settable.

- [ ] **Step 3: Implement**

In the `"emotes"` arm of `get_config_value`:
```rust
"lang" => format!("{:?}", config.emotes.lang).to_lowercase(),
```
In the `"emotes"` arm of `set_config_value`:
```rust
"lang" => {
    config.emotes.lang = match raw.to_ascii_lowercase().as_str() {
        "en" => crate::config::EmoteLang::En,
        "pl" => crate::config::EmoteLang::Pl,
        _ => return Err("Expected en or pl".to_string()),
    };
}
```
Add `"emotes.lang"` to `BASE_PATHS` and `"lang"` to the `("emotes", &[...])`
metadata entry.

- [ ] **Step 4: Run tests + commit**

Run: `cargo test -p repartee get_set_emotes` → PASS
```bash
make clippy
git add src/commands/settings.rs
git commit -m "feat(settings): /set emotes.lang en|pl"
```

---

## Task 6: Picker — language-aware label, filter, insert

**Files:**
- Modify: `src/app/input.rs` (`insert_emote_by_index`)
- Modify: `src/ui/emote_picker.rs` (`filtered_indices`, label)

- [ ] **Step 1: lang-aware insert**

In `src/app/input.rs::insert_emote_by_index`, insert the current-language name:
```rust
pub(crate) fn insert_emote_by_index(&mut self, index: u32) {
    let lang = self.config.emotes.lang.to_registry();
    let name = crate::emotes::display_name(index, lang);
    let token = format!(":{name}:");
    let at = self.input.cursor_pos;
    self.input.value.insert_str(at, &token);
    self.input.cursor_pos = at + token.len();
    self.input.tab_state = None;
}
```

- [ ] **Step 2: filter matches either language**

In `src/ui/emote_picker.rs::filtered_indices`, match Polish stem OR English alias:
```rust
#[must_use]
pub fn filtered_indices(filter: &str) -> Vec<u32> {
    let needle = filter.to_ascii_lowercase();
    crate::emotes::names()
        .iter()
        .enumerate()
        .filter(|(i, n)| {
            needle.is_empty()
                || n.contains(&needle)
                || crate::emotes::english_label(u32::try_from(*i).unwrap_or(0))
                    .is_some_and(|e| e.contains(&needle))
        })
        .map(|(i, _)| u32::try_from(i).unwrap_or(0))
        .collect()
}
```

- [ ] **Step 3: label per lang**

`render` takes `&mut App`, so read `app.config.emotes.lang` once and use it for
the displayed label. Replace the cell `name`:
```rust
let lang = app.config.emotes.lang.to_registry();
// ... inside the loop:
let name = crate::emotes::display_name(reg_idx, lang).to_owned();
```
(The `:name:` text-fallback and the thumbnail+name branches both use `name`.)

- [ ] **Step 4: Write a filter test**

In `emote_picker.rs` tests:
```rust
#[test]
fn filter_matches_english_alias() {
    let by_en = EmotePickerState::filtered_indices("smile");
    let by_pl = EmotePickerState::filtered_indices("usmiech");
    assert!(!by_en.is_empty());
    assert!(by_en.iter().any(|i| by_pl.contains(i)), "EN and PL filters hit the same emote");
}
```

- [ ] **Step 5: Run + commit**

Run: `cargo test -p repartee 'emote_picker'` → PASS; `cargo build -p repartee`
```bash
make clippy
git add src/app/input.rs src/ui/emote_picker.rs
git commit -m "feat(emotes): language-aware picker label/filter/insert"
```

---

## Task 7: Tab-complete offers both languages

**Files:**
- Modify: `src/ui/input.rs` (`emote_completions`)
- Test: inline

- [ ] **Step 1: Write the failing test**

In `ui/input.rs` tests:
```rust
#[test]
fn tab_completes_english_alias() {
    let mut input = InputState::new();
    input.value = "hey :smi".to_owned();
    input.cursor_pos = input.value.len();
    input.tab_complete(&[], &[], &[], &[]);
    assert!(input.value.contains(":smile:"), "got {:?}", input.value);
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p repartee tab_completes_english_alias`
Expected: FAIL — only Polish stems completed.

- [ ] **Step 3: Implement**

Change `emote_completions` to match across both languages using `tag_names()`:
```rust
fn emote_completions(word: &str) -> Option<Vec<String>> {
    let ep = word
        .strip_prefix(':')
        .filter(|p| !p.is_empty() && !p.contains(':'))?
        .to_ascii_lowercase();
    Some(
        crate::emotes::tag_names()
            .iter()
            .filter(|n| n.starts_with(&ep))
            .map(|n| format!(":{n}:"))
            .collect(),
    )
}
```

- [ ] **Step 4: Run tests + commit**

Run: `cargo test -p repartee 'tab_complete'` → PASS
```bash
make clippy
git add src/ui/input.rs
git commit -m "feat(input): tab-complete offers both PL and EN emote names"
```

---

## Task 8: `/emote` accepts either language

**Files:**
- Modify: `src/commands/handlers_ui.rs`

- [ ] **Step 1: Implement (resolve + lang-aware insert)**

In `cmd_emote`, replace the `binary_search`/insert block:
```rust
let names = crate::emotes::names();
if let Some(idx) = crate::emotes::resolve(&query) {
    // Insert in the configured language via the shared path.
    app.insert_emote_by_index(idx);
} else {
    let hits: Vec<&str> = crate::emotes::tag_names()
        .iter()
        .filter(|n| n.contains(query.as_str()))
        .take(10)
        .copied()
        .collect();
    let msg = if hits.is_empty() {
        format!("No emote matches \"{query}\"")
    } else {
        format!("Emotes matching \"{query}\": {}", hits.join(", "))
    };
    add_local_event(app, &msg);
}
let _ = names; // remove the old `names` binding entirely if it is now unused
```
> Remove the now-unused `let names = crate::emotes::names();` if `cmd_emote` no
> longer uses it elsewhere (the suggestion list now uses `tag_names()`).

- [ ] **Step 2: Build + the registry presence test still passes**

Run: `cargo test -p repartee emote_command_registered && cargo build -p repartee`
Expected: PASS / compiles.

- [ ] **Step 3: Lint + commit**

```bash
make clippy
git add src/commands/handlers_ui.rs
git commit -m "feat(commands): /emote accepts PL or EN names (case-insensitive)"
```

---

## Task 9: Web build.rs — union names + EN→stem map

**Files:**
- Modify: `web-ui/build.rs`

- [ ] **Step 1: Implement codegen from aliases.tsv**

Replace the body of `web-ui/build.rs::main` to parse `aliases.tsv` and emit BOTH
the union whitelist (`EMOTE_NAMES`, sorted) and an `EN_TO_STEM` lookup:
```rust
use std::fmt::Write as _;
use std::path::Path;

fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let aliases = Path::new(&manifest_dir).join("..").join("assets").join("emotes").join("aliases.tsv");
    println!("cargo:rerun-if-changed={}", aliases.display());

    let raw = std::fs::read_to_string(&aliases)
        .unwrap_or_else(|e| panic!("read {}: {e}", aliases.display()));

    // (polish_stem, english_alias) pairs.
    let mut pairs: Vec<(String, String)> = raw
        .lines()
        .filter_map(|l| l.split_once('\t'))
        .map(|(pl, en)| (pl.trim().to_owned(), en.trim().to_owned()))
        .collect();
    pairs.sort();
    assert!(!pairs.is_empty(), "no rows in {}", aliases.display());

    // Union of both names, sorted + deduped → the whitelist.
    let mut names: Vec<String> = pairs.iter().flat_map(|(p, e)| [p.clone(), e.clone()]).collect();
    names.sort();
    names.dedup();

    let mut out = String::from("// @generated by build.rs — do not edit.\n");
    out.push_str("pub static EMOTE_NAMES: &[&str] = &[\n");
    for n in &names {
        writeln!(out, "    {n:?},").expect("write");
    }
    out.push_str("];\n\n");
    // english/stem → polish stem (every name maps; stems map to themselves).
    out.push_str("pub static EMOTE_STEM: &[(&str, &str)] = &[\n");
    let mut stem_rows: Vec<(String, String)> = Vec::new();
    for (pl, en) in &pairs {
        stem_rows.push((pl.clone(), pl.clone()));
        if en != pl {
            stem_rows.push((en.clone(), pl.clone()));
        }
    }
    stem_rows.sort();
    for (name, stem) in &stem_rows {
        writeln!(out, "    ({name:?}, {stem:?}),").expect("write");
    }
    out.push_str("];\n");

    let dest = Path::new(&std::env::var("OUT_DIR").expect("OUT_DIR")).join("emote_names.rs");
    std::fs::write(&dest, out).unwrap_or_else(|e| panic!("write {}: {e}", dest.display()));
}
```

- [ ] **Step 2: Build the web crate**

Run: `cargo build -p repartee-web`
Expected: compiles (the generated file now also defines `EMOTE_STEM`).

- [ ] **Step 3: Commit**

```bash
git add web-ui/build.rs
git commit -m "feat(web-ui): codegen union name whitelist + EN->stem map from aliases.tsv"
```

---

## Task 10: Web — resolve English in `is_emote` + `stem_for`, use stem in `<img src>`

**Files:**
- Modify: `web-ui/src/emotes.rs`
- Modify: `web-ui/src/format.rs` (`emotify_spans_with`)
- Modify: `web-ui/src/components/chat_view.rs` (verify `<img src>` uses the stem)

- [ ] **Step 1: emotes.rs — add `stem_for`, keep `is_emote`**

```rust
use std::collections::HashMap;
use std::sync::LazyLock;

include!(concat!(env!("OUT_DIR"), "/emote_names.rs"));

static EMOTE_SET: LazyLock<HashSet<&'static str>> = LazyLock::new(|| EMOTE_NAMES.iter().copied().collect());
static STEM_MAP: LazyLock<HashMap<&'static str, &'static str>> =
    LazyLock::new(|| EMOTE_STEM.iter().copied().collect());

#[must_use]
pub fn is_emote(name: &str) -> bool {
    EMOTE_SET.contains(name)
}

/// The Polish stem (= GIF file name) for a tag in either language.
#[must_use]
pub fn stem_for(name: &str) -> &'static str {
    STEM_MAP.get(name).copied().unwrap_or("")
}
```
(Keep/adjust the existing `tests` module: assert `is_emote("smile")` and
`is_emote("usmiech")` both true, and `stem_for("smile") == "usmiech"`.)

- [ ] **Step 2: format.rs — store the stem in `emote_name`**

In `emotify_spans_with`, when a token matches, store the **stem** (so the `<img>`
src is the served file) while keeping the original token as `text` (alt/copy):
```rust
if is_known(name) {
    // ... push preceding text run ...
    out.push(StyledSpan {
        text: span.text[i..=j].to_owned(),         // ":matched:" (alt / copy)
        emote_name: Some(crate::emotes::stem_for(name).to_owned()),  // GIF stem for src
        ..StyledSpan::default()
    });
    // ...
}
```
> `emotify_spans` (the zero-arg entry) already passes `crate::emotes::is_emote`
> as the predicate; that now accepts both languages via the union whitelist.

- [ ] **Step 3: chat_view.rs — confirm src uses `emote_name`**

`render_styled_text` builds `format!("/emotes/{name}.gif")` from `span.emote_name`,
which is now always a stem → the served file resolves. No change needed beyond
verifying. The visually-hidden `.emote-code` and `alt` use `span.text` (the typed
token) — correct.

- [ ] **Step 4: Update web format tests**

In `format.rs` `emote_tests`, adjust the `known` helper and add an English case:
```rust
fn known(name: &str) -> bool {
    matches!(name, "usmiech" | "smile" | "lol")
}

#[test]
fn english_alias_becomes_emote_span() {
    // With the real stem map, :smile: → emote span whose src-stem is usmiech.
    let out = emotify_spans(vec![plain(":smile:")]);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].emote_name.as_deref(), Some("usmiech"));
    assert_eq!(out[0].text, ":smile:");
}
```
> The `_with` unit tests use the injected `known` predicate and assert
> `emote_name == stem_for(name)`; since `stem_for` reads the embedded map, prefer
> testing the embedded path via `emotify_spans` for the stem assertion.

- [ ] **Step 5: Build + test web**

Run: `cargo build -p repartee-web && cargo test -p repartee-web 'format::' 'emotes::'`
Expected: compiles, tests pass.

- [ ] **Step 6: Commit**

```bash
git add web-ui/src/emotes.rs web-ui/src/format.rs web-ui/src/components/chat_view.rs
git commit -m "feat(web-ui): render English-alias :tags:; <img> src uses the GIF stem"
```

---

## Task 11: Full verification + finish

- [ ] **Step 1: Gate**

Run: `make clippy && make test`
Expected: 0 new warnings, all tests pass.

- [ ] **Step 2: Full build**

Run: `make wasm && make release`
Expected: builds.

- [ ] **Step 3: Manual verify (graphics terminal)**

1. Send `:smile: :usmiech:` — both render the same emote inline.
2. `/set emotes.lang pl`; open picker — labels show Polish; picking inserts `:usmiech:`.
3. `/set emotes.lang en`; picker labels English; picking inserts `:smile:`.
4. Tab: `:smi`→`:smile:`, `:usm`→`:usmiech:`.
5. Web UI: a message with `:smile:` shows the emote (img src `/emotes/usmiech.gif`).

- [ ] **Step 4: Finish branch**

Use `superpowers:finishing-a-development-branch`.

---

## Self-Review

- **Spec coverage:** §3 single-source TSV → Task 1; registry resolve/labels/tags →
  Task 2; TUI English render → Task 3; §4 config lang → Task 4; /set → Task 5;
  picker label/filter/insert → Task 6; autocomplete both → Task 7; /emote → Task 8;
  web build/resolve/src → Tasks 9–10; §6 testing → tests across tasks + §11 manual.
- **Type consistency:** `resolve`/`english_label`/`display_name`/`tag_names`/`Lang`
  (Task 2) are used identically in Tasks 3,6,7,8; `EmoteLang`/`to_registry` (Task 4)
  used in Tasks 5,6; web `is_emote`/`stem_for` + `EMOTE_NAMES`/`EMOTE_STEM` (Tasks
  9,10) consistent. `emote_index` still = index into `names()` everywhere.
- **Placeholders:** none — full code per step; the only verify-only step (Task 10
  Step 3) is explicitly a no-op confirmation.
