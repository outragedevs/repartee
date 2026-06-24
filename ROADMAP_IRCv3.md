# ROADMAP IRCv3 — repartee

Stan i plan rozwoju obsługi IRCv3 w repartee. Lista uszeregowana **od najważniejszego do najmniej ważnego**, z uzasadnieniem, wpływem na fork parsera (`irc-proto-repartee` / `~/dev/irc`) i szacowanym nakładem.

> **Założenie strategiczne:** nie adaptujemy kodu Halloy (GPL-3.0 vs nasz MIT/MPL-2.0) ani nie przepisujemy parsera od zera. Rozwijamy **własne foreki** `irc-repartee` + `irc-proto-repartee` u źródła i dopisujemy logikę capów w repartee. Halloy (`squidowl/halloy`) służy wyłącznie jako **wzorzec maszyn stanu** — czytany, nie kopiowany. Specyfikacje: <https://ircv3.net/>.

---

## Status obecny (zaimplementowane)

CAP negotiation (CAP LS 302, REQ/ACK/NAK, END, NEW/DEL), oraz capy:

`multi-prefix` · `extended-join` · `server-time` · `account-tag` · `account-notify` · `away-notify` · `cap-notify` · `chghost` · `echo-message` · `invite-notify` · `batch` (NETSPLIT/NETJOIN) · `userhost-in-names` · `message-tags`

**SASL:** PLAIN · EXTERNAL (CertFP) · **SCRAM-SHA-256** (RFC 5802/7677) — *przewaga nad Halloy, który ma tylko PLAIN/EXTERNAL.*

Dodatkowo: bogaty parser ISUPPORT, parsowanie extbanów, tagi zapisywane do SQLite.

---

## Legenda

- **Proto-impact** — ile pracy w forku `irc-proto-repartee` (`~/dev/irc`):
  - 🟢 brak / już jest · 🟡 mały (1 wariant enuma) · 🔴 większy
- **Nakład** — szac. pracy w repartee (logika + UI + testy): S / M / L
- **Zależy od** — capy/infra, które warto mieć wcześniej

---

## Tier 1 — Najwyższy priorytet

### 1. `draft/chathistory` (+ `draft/event-playback`)
- **Dlaczego #1:** bezpośrednia synergia z naszą bazą SQLite historii i z niedawnymi pracami nad „web backlog viewport-fill". Pozwala pobrać historię z serwera/bouncera (soju/ergo) po reconnect i wypełnić luki w backlogu zamiast polegać tylko na lokalnym store. To kierunek, ku któremu projekt już zmierza.
- **Proto-impact:** 🟡 — dodać typowany `Command::CHATHISTORY` (dziś wpada w `Raw`). Odbiór działa już teraz: batch typu `chathistory` parsuje się jako `BatchSubCommand::CUSTOM`.
- **Nakład:** L — paging (`BEFORE`/`AFTER`/`LATEST`/`BETWEEN`/`AROUND`/`TARGETS`), wpięcie batcha w historię, deduplikacja po `msgid`, integracja z backlogiem web-ui.
- **Zależy od:** `batch` (✅ jest), warto: `labeled-response` + `msgid` (poz. 2) dla pewnej korelacji odpowiedzi.
- **Szczegóły:** osobny dokument planu — `PLAN_chathistory.md`.

---

## Tier 2 — Infrastruktura i tanie, wysokowartościowe wygrane

### 2. `labeled-response` (+ obsługa `msgid` / `draft/reply` tagów)
- **Dlaczego:** infrastruktura. Pozwala niezawodnie korelować odpowiedź serwera z naszym żądaniem — fundament pod redakcję, reply, oraz pewne dopasowanie odpowiedzi `chathistory`. `msgid` daje stabilną deduplikację (kluczowe dla chathistory).
- **Proto-impact:** 🟢 — to czyste tagi (`label`, `msgid`), brak nowych verbów. Generujemy `label` przy wysyłce, czytamy z odpowiedzi.
- **Nakład:** S–M (mapa `label → kontekst żądania`, timeouty).
- **Zależy od:** `message-tags` (✅), `batch` (✅).

### 3. `monitor`
- **Dlaczego:** najtańsza widoczna dla użytkownika wygrana — powiadomienia online/offline dla obserwowanych nicków.
- **Proto-impact:** 🟢 — `Command::MONITOR` oraz numeryki `RPL_MONONLINE/MONOFFLINE/MONLIST/ENDOFMONLIST` (730–734) **już są** w forku.
- **Nakład:** S–M — logika listy MONITOR, parsowanie 730–734, UI, limit z ISUPPORT `MONITOR=`.
- **Zależy od:** —

---

## Tier 3 — Nowoczesny UX

### 4. `draft/read-marker`
- **Dlaczego:** synchronizacja „przeczytane do" między urządzeniami/sesjami; naturalnie współgra z chathistory i web-ui.
- **Proto-impact:** 🟡 — verb `MARKREAD` (lub tymczasowo `Raw`).
- **Nakład:** M — przechowywanie markera per-bufor, wysyłka/odbiór, UI.
- **Zależy od:** dobrze współgra z `chathistory`.

### 5. `draft/message-redaction`
- **Dlaczego:** moderacja/kasowanie wiadomości; coraz częściej wdrażane (ergo).
- **Proto-impact:** 🟡 — verb `REDACT` (lub `Raw`).
- **Nakład:** M — zastosowanie redakcji w buforze i w SQLite (oznaczenie/usunięcie po `msgid`/`target`).
- **Zależy od:** `msgid` (poz. 2) — bez niego redakcja jest zawodna.

### 6. `draft/multiline`
- **Dlaczego:** czyste wysyłanie długich wiadomości jako jeden logiczny blok (zamiast łamania na linie).
- **Proto-impact:** 🟢 — używa istniejącego `BATCH`; brak nowego verba.
- **Nakład:** M — concat/split wg `max-bytes`/`max-lines`, parsowanie limitów z CAP.
- **Zależy od:** `batch` (✅).

### 7. `setname`
- **Dlaczego:** zmiana realname w locie; trywialne, niemal darmowe domknięcie.
- **Proto-impact:** 🟡 — verb `SETNAME` (lub `Raw`).
- **Nakład:** S.
- **Zależy od:** —

---

## Tier 4 — Warunkowe / niski priorytet

### 8. `soju.im/bouncer-networks`
- **Dlaczego:** istotne, jeśli mamy/planujemy użytkowników bouncerów; mocno współgra z chathistory. Niszowe poza ekosystemem soju.
- **Proto-impact:** 🟢 — soju-specyficzne tagi/komendy, w razie potrzeby `Raw`.
- **Nakład:** M–L. **Zależy od:** `chathistory` (poz. 1).

### 9. `draft/metadata-2`
- **Dlaczego:** display-name, avatar, pronouns, kolor itd. Rzadko wdrażane na produkcji.
- **Proto-impact:** 🟡 — `Command::METADATA` z subkomendami **już istnieje** (GET/LIST/SET/CLEAR), do dopracowania pod metadata-2.
- **Nakład:** M. **Zależy od:** —

### 10. `no-implicit-names`
- **Dlaczego:** optymalizacja ruchu z ergo/soju (pominięcie NAMES przy JOIN).
- **Proto-impact:** 🟢. **Nakład:** S. **Zależy od:** —

---

## Tier 5 — Ścieżka bezpieczeństwa (niezależna)

### 11. `STS` (Strict Transport Security)
- **Dlaczego:** wymuszenie TLS/upgrade; hardening. Brak po obu stronach (również Halloy go nie ma).
- **Proto-impact:** 🟢 — parsowanie polityki z CAP `sts=`, logika w warstwie połączenia (`irc-repartee`).
- **Nakład:** M. **Zależy od:** —

### (poza zakresem CAP) Transport: WebSocket / SOCKS5 / HTTP-proxy / Tor
- **Dlaczego:** Halloy ma to w swoim crate `irc`; my mamy tylko TLS. Realny wyróżnik, ale duży projekt. Rozważyć tylko przy realnym popycie; Halloy = wzorzec architektury (nie kod).
- **Nakład:** L. **Zależy od:** —

---

## Skrót decyzyjny

| # | Cap | Proto | Nakład | Wartość |
|---|-----|:---:|:---:|:---:|
| 1 | draft/chathistory (+event-playback) | 🟡 | L | ★★★★★ |
| 2 | labeled-response (+msgid) | 🟢 | S–M | ★★★★ |
| 3 | monitor | 🟢 | S–M | ★★★★ |
| 4 | draft/read-marker | 🟡 | M | ★★★ |
| 5 | draft/message-redaction | 🟡 | M | ★★★ |
| 6 | draft/multiline | 🟢 | M | ★★ |
| 7 | setname | 🟡 | S | ★★ |
| 8 | soju.im/bouncer-networks | 🟢 | M–L | ★★ (warunkowo) |
| 9 | draft/metadata-2 | 🟡 | M | ★ |
| 10 | no-implicit-names | 🟢 | S | ★ |
| 11 | STS | 🟢 | M | ★★ (security) |
