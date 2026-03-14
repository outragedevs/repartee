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
languages = ["en_US", "pl_PL", "de_DE"]
dictionary_dir = ""   # default: ~/.repartee/dicts
```

Runtime settings:

```
/set spellcheck.enabled true
/set spellcheck.languages en_US,pl_PL,de_DE
/set spellcheck.dictionary_dir /path/to/dicts
```

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
2. Misspelled words appear **underlined in red**
3. A popup shows up to 4 suggestions
4. Press **Tab** to cycle through suggestions (replaces the word inline)
5. Press **Space** or continue typing to accept the current correction
6. Press **Escape** to revert to the original word
7. Press **Backspace** to dismiss and edit manually

A word is correct if **any** active dictionary accepts it — so with `en_US` + `pl_PL` active, both English and Polish words pass.

## Aliases

`/spell`

## See also

`/set spellcheck.*`
