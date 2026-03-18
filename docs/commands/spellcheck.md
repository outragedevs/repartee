---
category: Configuration
description: Spell checker status and control
---

# /spellcheck

Spell checker status and control.

## Usage

```
/spellcheck [status|reload|list|get <lang>]
```

## Subcommands

### status

Show spell checker status: enabled/disabled, active languages, dictionary directory, and number of loaded dictionaries.

### reload

Reload dictionaries from disk. Useful after adding new `.dic`/`.aff` files or changing `spellcheck.languages`.

### list

Fetch the list of available dictionaries from the Repartee dictionary repository. Shows each language with its install status.

### get &lt;lang&gt;

Download a dictionary by language code (e.g. `en_US`, `pl_PL`, `de_DE`). Files are saved to `~/.repartee/dicts/` and the spell checker is automatically reloaded.

## Configuration

```toml
[spellcheck]
enabled = true
computing = true           # computing/IT dictionary (7,400+ terms)
mode = "replace"           # or "highlight"
languages = ["en_US", "pl_PL", "de_DE"]
dictionary_dir = ""        # default: ~/.repartee/dicts
```

Runtime settings:

```
/set spellcheck.enabled true
/set spellcheck.computing true
/set spellcheck.mode replace
/set spellcheck.languages en_US,pl_PL,de_DE
/set spellcheck.dictionary_dir /path/to/dicts
```

## Computing dictionary

A bundled 7,400-word dictionary of computing/IT terms — programming languages, frameworks, tools, protocols, IRC vocabulary, and more. Prevents false positives on words like `kubectl`, `PRIVMSG`, `IRCnet`, `tokio`, `rustfmt`.

Install from the repartee-dicts repository:

```
/spellcheck get computing
```

Controlled via `spellcheck.computing` (default: `true`). The computing dictionary is checked **before** language dictionaries — if a word is a known computing term, it's accepted immediately.

## Modes

### replace (default)

Misspelled words are immediately replaced with the first suggestion. Tab cycles through alternatives, Space accepts, Escape reverts.

### highlight

Misspelled words stay as-is but are marked with red underline. A popup shows suggestions for reference. Any keystroke dismisses the popup without changing your text. Non-aggressive — useful if you prefer to fix typos manually.

## Dictionary setup

The easiest way is to use the built-in download command:

```
/spellcheck list          # see available dictionaries
/spellcheck get en_US     # download English (US)
/spellcheck get pl_PL     # download Polish
```

Dictionaries are downloaded from the [outragedevs/repartee-dicts](https://github.com/outragedevs/repartee-dicts) repository, which provides UTF-8 Hunspell dictionaries sourced from [wooorm/dictionaries](https://github.com/wooorm/dictionaries).

You can also place `.dic`/`.aff` files manually in `~/.repartee/dicts/`:

```
~/.repartee/dicts/en_US.dic
~/.repartee/dicts/en_US.aff
```

For languages not included in our repository, you can find additional UTF-8 Hunspell dictionaries at [wooorm/dictionaries](https://github.com/wooorm/dictionaries) (90+ languages).

## Inline correction

When spell checking is active:

1. Type a word and press **Space** — the word is checked
2. Check order: skip filters (URLs, numbers) → nick list → computing dict → language dicts
3. A word is correct if **any** source accepts it

### Replace mode (default)

- Misspelled word is replaced with the first suggestion
- **Tab** cycles through alternatives (replaces inline)
- **Space** or typing accepts the current correction
- **Escape** reverts to the original word
- **Backspace** dismisses and lets you edit manually

### Highlight mode

- Misspelled word stays as-is, marked **red + underlined**
- Popup shows suggestions for reference only
- **Any keystroke** dismisses the popup without changing text
- Non-aggressive — fix typos in your own time

## Aliases

`/spell`

## See also

`/set spellcheck.*`
