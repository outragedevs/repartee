# /spellcheck

Spell checker status and control.

## Usage

```
/spellcheck [status|reload]
```

## Subcommands

### status

Show spell checker status: enabled/disabled, active languages, dictionary directory, and number of loaded dictionaries.

### reload

Reload dictionaries from disk. Useful after adding new `.dic`/`.aff` files or changing `spellcheck.languages`.

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

Place Hunspell `.dic` and `.aff` files in `~/.repartee/dicts/`:

```
~/.repartee/dicts/en_US.dic
~/.repartee/dicts/en_US.aff
~/.repartee/dicts/pl_PL.dic
~/.repartee/dicts/pl_PL.aff
```

Dictionaries are available from the LibreOffice project:
https://github.com/LibreOffice/dictionaries

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
