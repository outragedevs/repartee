# GG7 Emote Set

Source: https://emots.yetihehe.com/ (Gadu-Gadu 7 emoticons, dirs 1/2/3).

Curation: one file per base name, precedence 3 > 2 > 1 (dir 3 = most classic variant).
14 of 16 variant names resolve to dir 3; `dobani` and `kwiatek` fall back to dir 2.
Shoutbox (`sb/`) intentionally excluded. 183 emotes.

All names are lowercased to `[a-z0-9_]` so the `:name:` tokenizer is unambiguous
(the only rename was `8P` -> `8p`).

Reproduce with the command in
docs/superpowers/plans/2026-06-01-gg-emotes-foundation-web.md (Task 1),
then `mv assets/emotes/8P.gif assets/emotes/8p.gif`.
