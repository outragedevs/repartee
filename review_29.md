# Review PR #29 — `fix(e2e): channel notice rendering + DM keying consistency (lurker interop)`

> **STATUS (2026-07-02): NAPRAWIONE.** Wszystkie znaleziska #1–#8 oraz trzy z
> wyciętych (swallow własnego szyfrogramu, luka lenient/strict framing —
> rozwiązana przez observe-przed-parse + lenient dispatch PRIVMSG, indeks
> `e2e_peers(last_nick)`) zostały naprawione w commicie na branchu
> `fix/various-improvements`. Decyzje implementacyjne:
> - **#5**: legacy fallback pozostaje źródłem migracji TYLKO gdy brak źródeł
>   z tej sieci (bufor/cache) — usunięcie go całkiem przywróciłoby plaintext
>   downgrade na ścieżce upgrade'u. Zamiast tego: AutoAccept jest capowany do
>   Normal (obcy handshake dalej prosi o accept) i enable jest komunikowany
>   userowi (`[E2E] ... pre-upgrade nick match ... run /e2e off`).
> - **#2/#7**: naprawione u źródła — `resolve_query_peer_handle` odzyskuje
>   connection z samego buffer_id (`conn_id/nazwa`), więc cache keyringa jest
>   konsultowany także po `/close`; żywa rezolucja wygrywa nad captured.
> - **Nie zrobione** (świadomie): unifikacja resolverów input.rs ↔
>   handlers_e2e.rs (czysty refactor, bez zmiany zachowania — do osobnego PR).
> Weryfikacja: `make clippy` 0 warnings, `make test` 1470 passed (6 nowych
> testów regresyjnych).

> **STATUS 2 (2026-07-06): AUDYT KOMPLETNOŚCI E2E DM — NAPRAWIONE.**
> Pełny czteroagentowy audyt (zgodność ze spec, ścieżki wysyłki, ścieżki
> odbioru, cykl życia kluczy + storage) po domknięciu znalezisk #1–#8.
> Zgodność z `docs/rpe2e-dm-addendum.md`: pełna (13/13 wymagań, złoty wektor
> AAD DM przechodzi). Naprawione w fazach A–E (commity `99a8770`, `1b8c091`,
> `f3510b7`, `cc7c570`, `3375f92`, `d9f44a6`):
>
> - **A (krytyczne, fail-closed):** `/msg`, `/query <nick> <tekst>`, `/me`
>   oraz Lua `say()/action()/ctcp()` szły `send_privmsg` wprost, omijając
>   bramkę E2E — plaintext do peera z włączonym E2E. Bramka przeniesiona na
>   `AppState` (`src/app/e2e_gate.rs`, testowalna), wszystkie ścieżki
>   by-target przez `e2e_send_plan_for_target`/`send_gated_message`.
>   `/notice`, Lua `notice()` i DCC CHAT dostają jawne ostrzeżenie cleartext.
> - **B (odbiór):** uszkodzona linia `+RPE2E01` renderowała się (i logowała)
>   surowa → teraz `[E2E rejected: malformed…]`; ciphertext w NOTICE
>   tłumiony; placeholder `[E2E: awaiting session with …]` był logowany pod
>   prawdziwym @msgid i blokował replay (pierwszy DM sesji tracony na
>   zawsze) → transient+tagless, KEYRSP kolejkuje gap-fill query,
>   splice sprząta placeholder.
> - **C (rotacja):** REKEY bez ochrony przed replay (wrap do klucza
>   długoterminowego = wieczna ważność) → nonce single-use w
>   `e2e_seen_rekeys` (bez zmiany wire — interop zachowany); brak retencji
>   starego klucza → `prev_sk`/`prev_created_at` + fallback deszyfrowania
>   w oknie 300 s (reorder REKEY↔PRIVMSG).
> - **D (higiena):** `~/.repartee/logs` 0700 + `messages.db` 0600 (unix,
>   naprawiane przy każdym starcie); TTL pending handshake'ów (initiator
>   15 min, inbound accept 6 h) — mapy nie rosną bez ograniczeń.
> - **E (izolacja sieci):** konteksty keyringa scope'owane per-sieć
>   (`{network}\x1F{wire}`) — `#rust` na dwóch sieciach nie dzieli już
>   configu/kluczy. Wire/AAD/sygnatury/c= zawsze z części wire → interop
>   z lurkerem bajt-w-bajt. Odczyty z fallbackiem do wierszy legacy
>   (scoped wygrywa), mutacje destrukcyjne (revoke/forget/rotate) trafiają
>   w oba wiersze, autotrust honoruje legacy scope.
>
> Weryfikacja końcowa: `make clippy` 0 warnings, `cargo test` 1493 passed.
> Świadomie odłożone: unifikacja resolverów (refactor, osobny PR);
> `KEYRING_KEY` w `.env` obok bazy (wymaga decyzji o keychain/OS-store).

> **STATUS 3 (2026-07-07): RUNDA ZEWNĘTRZNA #3 — NAPRAWIONE.** Oba
> znaleziska zweryfikowane jako realne i naprawione:
>
> - **[P1] Globalny legacy fallback łamał izolację sieci** (`keyring.rs`):
>   każdy scoped miss spadał do niescope'owanego wiersza pre-upgrade
>   niezależnie od sieci — `#chan` na drugiej sieci przejmował klucze
>   pierwszej. Teraz: (1) startowa migracja `adopt_legacy_contexts()` —
>   przy JEDNEJ skonfigurowanej sieci wszystko przechodzi na jej scope;
>   przy wielu sieciach kontekst DM atrybuowany przez network-keyed
>   `e2e_dm_handle_cache`, kanał przez pary `(network, buffer)` z logu
>   wiadomości (ta sama baza) — migracja tylko przy dokładnie jednym
>   dopasowaniu; kolizja ze scoped wierszem → scoped wygrywa, legacy
>   znika. (2) Read-fallback, uniony list, konsultacja TOFU w
>   `install_incoming_session_strict` i legacy scope autotrustu są
>   ODMAWIANE przy >1 skonfigurowanej sieci (`legacy_fallback()` —
>   fail-closed: świeży handshake zamiast cudzych kluczy). (3)
>   Nieatrybuowalne konteksty zgłaszane głośnym `[E2E] warning` przy
>   starcie. Heal po rename labela bez zmian (czyta scoped siblings,
>   nie legacy). 5 nowych testów integracyjnych.
> - **[P2] Gap-fill po KEYRSP ginął przy zajętym CHATHISTORY**
>   (`backlog.rs`/`app/irc.rs`): jednorazowy wpis `pending_e2e_gapfills`
>   był zużywany nawet gdy `should_request` stłumił żądanie przez
>   in-flight batch dla tego celu — placeholder „awaiting session"
>   zostawał do reconnectu. Teraz `regapfill_conversation_after_session`
>   zwraca wynik; stłumienie przez in-flight (transient) → wpis wraca do
>   kolejki i ponawia się przy następnym evencie IRC (batch END
>   in-flight'a JEST takim eventem, więc retry odpala dokładnie gdy
>   konflikt znika); przyczyny trwałe (brak capa/połączenia) → drop.
>   Enqueue dedupowane.
>
> Weryfikacja: `make clippy` 0 warnings, `make test` 1505 passed.

> **STATUS 4 (2026-07-07): RUNDA ZEWNĘTRZNA #4 — NAPRAWIONE.**
>
> - **[P1] NICK osierocał cache handle'a DM → plaintext** (`events.rs`
>   `handle_nick_change` / `e2e_gate.rs:155`): handler NICK zmieniał nazwy
>   buforów query, ale nie przepisywał wiersza `e2e_dm_handle_cache`
>   (klucz `(network, nick)`) ani hintu `e2e_peers.last_nick`. Przy
>   zamkniętym (lub nieotwartym w tej sesji) query `/msg <nowy_nick>`
>   rezolwował handle po nowym nicku → miss → brak configu `@<handle>`
>   → plaintext mimo włączonego E2E. Fix: `Keyring::rename_dm_nick()` —
>   re-key wiersza cache na nowy nick (`UPDATE OR REPLACE`; NICK jest
>   autorytatywny, więc stary wiersz pod nowym nickiem jest zastępowany;
>   inne sieci nietknięte) + odświeżenie network-agnostycznego
>   `last_nick` (semantyka „ostatnio widziany", ścieżka wysyłki jest na
>   nim bezpieczna — najgorszy przypadek to wrong-context, ale wciąż
>   ZASZYFROWANY send). Wywołane w `handle_nick_change` PRZED early
>   returnem ścieżki ignore (ignorowany nick też zmienia nick). Otwarte
>   query było odporne już wcześniej (`rename_query_buffers` przenosi
>   `peer_handle` z buforem). 2 nowe testy (keyring + regresja
>   event-level na pełnym scenariuszu).
>
> Weryfikacja: `make clippy` 0 warnings, `make test` 1507 passed.

> **STATUS 5 (2026-07-07): RUNDA ZEWNĘTRZNA #5 — NAPRAWIONE.** Oba
> findings [P2] potwierdzone i naprawione.
>
> - **[P2] Hardening uprawnień pomijał pliki WAL/SHM** (`storage/mod.rs`):
>   `harden_storage_permissions` chmodował tylko katalog (0700) i
>   `messages.db` (0600). SQLite tworzy `-wal`/`-shm` z uprawnieniami
>   pliku bazy — ale pierwszy start otwiera bazę (i WAL) PRZED
>   hardeningiem, a instalacje sprzed hardeningu nigdy go nie miały, więc
>   istniejące siblingi zostawały z umask (np. 0644) ze świeżymi
>   plaintextowymi stronami logu/keyringa. Fix: pętla hardeningu chmoduje
>   też `messages.db-wal` i `messages.db-shm` (istniejące; przyszłe
>   dziedziczą już 0600 po głównym pliku). Test rozszerzony o oba pliki.
> - **[P2] Atrybucja legacy kluczy z logów czatu — usunięta**
>   (`keyring.rs` `attribute_legacy_context`): wiersz w `messages` dla
>   `#chan` na NetA to dowód AKTYWNOŚCI, nie własności kluczy — legacy
>   wiersz E2E mógł należeć do NetB, której historia jest pusta,
>   wykluczona albo wyczyszczona; migracja na tej podstawie = cross-network
>   key reuse na NetA + utrata fallbacku na NetB. Fix: gałąź `messages`
>   usunięta w całości (wraz z `messages_table_exists`); na multi-network
>   atrybutowalne są wyłącznie konteksty DM (`@handle`) przez
>   `e2e_dm_handle_cache` — stan zapisywany przez samą maszynerię E2E, więc
>   pojedyncza sieć w cache to bezpośredni dowód własności. Konteksty
>   kanałowe na multi-network zostają legacy → ostrzeżenie startowe,
>   read-gate trzyma je martwe, świeże handshaki odtwarzają sesje
>   fail-closed. Single-network bez zmian (skrót `[only]`). Test
>   przepisany: log wskazujący jedną sieć NIE migruje kanału.
>
> Weryfikacja: `make clippy` 0 warnings, `make test` 1507 passed.

> **STATUS 6 (2026-07-07): WEWNĘTRZNY SELF-REVIEW ADWERSARYJNY — NAPRAWIONE.**
> Po rundzie #5 przepuściłem cały kod z rund 3–5 przez dwóch niezależnych
> recenzentów z konkretnymi kątami ataku (case-sensitivity, swap nicków,
> atomowość migracji, kompletność hardeningu, ścieżki fail-open bramy).
> Wynik: 4 realne findingi naprawione + 2 mniejsze.
>
> - **[P1] REGRESJA z rundy #4: `rename_dm_nick` czyścił hint `last_nick`
>   peerom na INNYCH sieciach** (`keyring.rs`): `UPDATE e2e_peers SET
>   last_nick=? WHERE last_nick=?` był network-agnostyczny — NICK carol→dave
>   na NetA przenosił hint carol z NetB, jedyną ścieżkę rezolucji jej
>   handle'a przed pierwszym odezwaniem → `/msg carol` na NetB = plaintext.
>   Fix: UPDATE na `e2e_peers` usunięty w całości (rename cache per-network
>   wystarcza; `last_nick` odświeżają realne sightingi). Dodatkowo rename
>   pomija WŁASNĄ zmianę nicka (cache trzyma handle peerów). Test rozszerzony
>   o izolację hinta.
> - **[P1] Konteksty kanałowe case-sensitive** (`e2e_gate.rs`/`keyring.rs`):
>   config zapisany pod `#sec`, `/msg #SEC …` budował kontekst z case'em
>   użytkownika → BINARY miss → plaintext na cały kanał z włączonym E2E,
>   bez żadnego sygnału. Fix: `Keyring::canonical_channel_context` (NOCASE,
>   preferencja exact) — brama kanonizuje kontekst RAZ, więc sesje/odbiorcy
>   niżej zostają spójni; błąd odczytu = odmowa (fail-closed); advisory twin
>   (`e2e_enabled_for_target`) kanonizuje tak samo. Test regresyjny.
> - **[P1] Adopcja legacy przenosiła bare-nickowe wiersze DM poza zasięg
>   odmowy NoPeerHandle** (`keyring.rs`): single-network `[only]` migrował
>   też wiersze typu `bob` (sprzed handle'i) do `net␟bob`, a brama sprawdza
>   je NIESKOPOWANE — po pierwszym starcie po upgradzie odmowa fail-closed
>   zamieniała się w plaintext. Fix: `legacy_context_values` pomija konteksty
>   niebędące ani `#…` ani `@…` (obsługuje je nieskopowana ścieżka bramy +
>   migracja na `@<handle>` przy pierwszym kontakcie); test bramy odtwarza
>   scenariusz z adopcją, test adopcji pilnuje filtra.
> - **[P1→advisory] Bypass botowy `.`/`!` był niewidoczny** (`e2e_gate.rs`):
>   celowy bypass (kompatybilny ze skryptami irssi/weechat) wysyłał
>   plaintext w rozmowie E2E bez śladu — echo wyglądało jak zaszyfrowane.
>   Semantyka bez zmian (bypass zostaje), ale rozmowa E2E dostaje widoczną
>   linię `[E2E] … CLEARTEXT` (jak przy `/notice`); poza E2E cisza. Test.
>   (Ewentualne ograniczenie bypassu do kanałów = decyzja produktowa,
>   odnotowana do dyskusji.)
> - **[P2] Err ≠ brak przy obserwacji zmiany handle'a** (`events.rs`):
>   `cached_dm_handle(...).unwrap_or_default()` traktował błąd odczytu jak
>   „brak poprzedniego handle'a" — migracja configu pomijana po cichu, a
>   bufor i tak przestawiany na `@<new>` → następna wiadomość plaintext.
>   Fix: `track_dm_handle_change` zwraca bool; przy błędzie odczytu CAŁA
>   obserwacja odroczona (wszystkie 3 call sites zostawiają `peer_handle`
>   na starym, wciąż deszyfrowalnym kontekście; kolejny PRIVMSG/CHGHOST
>   ponawia).
> - **[P2] Warunek ostrzeżenia startowego liczył wpisy serwerów, nie sieci**
>   (`app/mod.rs`): dwa wpisy bouncera z tym samym `label` to dla keyringa
>   JEDNA sieć (fallback aktywny), a ostrzeżenie „ignored" kłamało. Fix:
>   licznik po DISTINCT labelach, spójnie z `set_configured_networks`.
>
> Weryfikacja: `make clippy` 0 warnings, `make test` 1510 passed (3 nowe).

> **STATUS 7 (2026-07-07): DECYZJA PRODUKTOWA — bypass botowy `.`/`!`
> ograniczony do KANAŁÓW.** Na polecenie użytkownika („komendy . ! mają
> być ograniczone do kanałów"): w DM-ach linie zaczynające się od `.`/`!`
> przechodzą przez pełną bramę E2E jak każda inna wiadomość (szyfrowane
> przy włączonym E2E, fail-closed przy błędach); na kanałach bypass
> zostaje, z widoczną linią `[E2E] … CLEARTEXT` gdy kanał ma włączone
> E2E. Spójnie zaktualizowane wszystkie trzy implementacje: brama w
> repartee (`e2e_gate.rs`), skrypt weechat (`scripts/weechat/rpe2e.py`)
> i skrypt irssi (`scripts/irssi/rpe2e.pl`). Test bramy przepisany na
> nową semantykę (kanał E2E: bypass+advisory; kanał bez E2E: cisza;
> DM E2E: szyfruje). Weryfikacja: clippy 0, 1510 testów, `perl -c` /
> `py_compile` na skryptach OK.

> **STATUS 8 (2026-07-08): RUNDA ZEWNĘTRZNA #6 — NAPRAWIONE.**
>
> - **[P2] Długie szyfrowane `/me` łamało ramkowanie CTCP** (`e2e_gate.rs`
>   / `manager.rs`): chunki RPE2E01 deszyfrują się i renderują STANDALONE
>   (bez reasemblacji, spec §6), a `/me` szyfrował całą ramkę
>   `\x01ACTION …\x01` generycznym `encrypt_outgoing` — ciało dłuższe niż
>   ~171 B rozpadało się w środku ramki i u peera renderowało jako surowe
>   fragmenty ze znakami kontrolnymi. Fix: nowy
>   `E2eManager::encrypt_outgoing_ctcp` — ramka mieszcząca się w jednym
>   chunku idzie jak dotąd; dłuższy ACTION jest dzielony na NIEZALEŻNE,
>   osobno opakowane ramki `\x01ACTION kawałek\x01` (peer renderuje
>   sekwencję akcji, analogicznie do wieloliniowego plaintextu; limit
>   MAX_CHUNKS obowiązuje dla kawałków); inna za długa ramka CTCP (np.
>   z Lua `ctcp()`) jest ODMAWIANA zamiast cicho wysyłana połamana.
>   Brama kieruje każdy tekst zaczynający się od `\x01` przez ten wariant.
>   Chunker dostał `split_plaintext_budget` (budżet per-kawałek z miejscem
>   na ramkowanie). Test roundtrip: multi-byte ciało ≫ 1 chunk → każdy
>   kawałek deszyfruje się do kompletnej ramki ACTION, bajty bez strat;
>   krótki ACTION = 1 ramka; za długi VERSION = błąd.
>
> Weryfikacja: `make clippy` 0 warnings, `make test` 1511 passed.

> **STATUS 9 (2026-07-08): RUNDA ZEWNĘTRZNA #7 — FINDING OBALONY (test
> regresyjny dodany).**
>
> - **[P2] „By-target send z innym casem gubi REKEY-e" — NIEPRAWDA.**
>   Przesłanka findingu („constructs conn/#SEC") jest błędna:
>   `make_buffer_id` LOWERCASE'UJE nazwę (`state/buffer.rs:214`, test
>   `make_buffer_id_lowercases`), więc `/msg #SEC` daje `conn/#sec` i
>   `self.buffers.get(buffer_id)` trafia w otwarty bufor `#sec`. Rezolucja
>   odbiorców REKEY w kanale idzie po `ident@host` z users-mapy tego
>   bufora (`rekey_notice_target`), a w DM po równości
>   `context_key == wire_context` — case targetu nie uczestniczy w żadnym
>   kroku drain'u. Dowód empiryczny: nowy test
>   `by_target_channel_send_case_variant_still_drains_rekeys` odtwarza
>   scenariusz reviewera 1:1 (bufor `#sec` z userem bob, handshake
>   AutoAccept, `mark_outgoing_pending_rotation`, wysyłka `/msg #SEC`) i
>   przechodzi BEZ zmian produkcyjnych — REKEY NOTICE ląduje w
>   `pending_e2e_sends` z targetem `bob`. Test zostaje jako regresja
>   chroniąca ten łańcuch przy przyszłych refaktorach.
>
> Weryfikacja: `make clippy` 0 warnings, `make test` 1512 passed.

> **STATUS 10 (2026-07-08): RUNDA ZEWNĘTRZNA #8 — NAPRAWIONE.** Oba
> findingi [P2] potwierdzone i naprawione.
>
> TTL dla pending-handshake'ów były egzekwowane WYŁĄCZNIE na ścieżce
> insertu (`prune_expired_pending` wołany z `build_keyreq` /
> `cache_pending_inbound_normal_mode`). W bezczynnej sesji, gdy między
> stworzeniem wpisu a jego konsumpcją nie zdarzył się żaden kolejny
> handshake, przeterminowany wpis przeżywał i konsument go akceptował.
>
> - **[P2] KEYRSP kończył przeterminowany handshake** (`manager.rs:1657`
>   `consume_matching_pending_for_keyrsp`): KEYRSP przychodzący po
>   `PENDING_KEYREQ_TTL_SECS` (900 s) bez insertu w międzyczasie nadal
>   dopasowywał stary wpis i kończył handshake, trzymając sekret
>   efemeryczny ponad TTL. Fix: `prune_expired_pending()` na początku
>   konsumenta — stary kandydat wypada, zanim zostanie choćby spróbowany.
> - **[P2] `/e2e accept` akceptował przeterminowany KEYREQ**
>   (`manager.rs:1495` `accept_pending_inbound`): Normal-mode KEYREQ
>   wiszący ponad `PENDING_INBOUND_TTL_SECS` (21 600 s) był akceptowany
>   przy `/e2e accept` bez insertu wyzwalającego prune. Fix:
>   `prune_expired_pending()` przed `remove` — przeterminowany wpis jest
>   eksmitowany, więc `remove` naturalnie zwraca `Ok(None)` („nic do
>   zaakceptowania"); peer po prostu ponawia handshake.
>
> Istniejące testy TTL (`stale_pending_*_are_evicted`) przechodziły tylko
> dzięki *jawnemu* insertowi po postarzeniu wpisu — dokładnie ta luka.
> Dodane 2 testy egzekwujące TTL po stronie konsumenta BEZ insertu
> (`stale_pending_handshake_rejected_by_keyrsp_consumer_without_insert`,
> `stale_pending_inbound_rejected_by_accept_without_insert`).
>
> Weryfikacja: `make clippy` 0 warnings, `make test` 1514 passed (2 nowe).

> **STATUS 11 (2026-07-08): RUNDA ZEWNĘTRZNA #9 — NAPRAWIONE.** Finding
> [P2] potwierdzony i naprawiony.
>
> - **[P2] Zamiecione placeholdery E2E nie znikały u klienta web**
>   (`state/events.rs:663-666` `surface_history_rows`): transientny
>   placeholder (`AWAITING_OWN_IDENTITY_PLACEHOLDER` /
>   `AWAITING_SESSION_PLACEHOLDER_PREFIX`) był rozgłaszany do żywych
>   klientów web przez `NewMessage` w chwili dostarczenia. Gdy pojawiała
>   się odszyfrowana linia z CHATHISTORY, serwerowy `retain` usuwał
>   placeholder TYLKO z `AppState` — nie było żadnego zdarzenia usuwającego
>   go u klienta. Klient web pokazywał więc DWIE linie na ten sam
>   ciphertext (placeholder + odszyfrowany `InsertMessage`) aż do pełnego
>   resyncu/reconnectu. Fix: nowy wariant `WebEvent::DeleteMessages {
>   buffer_id, message_ids }` (serwer `src/web/protocol.rs` + mirror
>   `web-ui/src/protocol.rs`); `surface_history_rows` zbiera in-memory id
>   zamiatanych placeholderów i emituje `DeleteMessages` PO
>   `InsertMessage`; handler w `web-ui/src/state.rs` usuwa je po `id`
>   (`entry.retain(|m| !message_ids.contains(&m.id))`). Nieaktualny komentarz
>   „no removal event exists to send" poprawiony.
>
> Dodane testy: `swept_placeholder_emits_delete_web_event` (emituje
> `DeleteMessages` z id placeholdera) oraz negatywny guard w
> `unrelated_placeholder_survives_a_replay_of_other_lines` (brak
> `DeleteMessages`, gdy nic nie zamieciono).
>
> Weryfikacja: `make clippy` 0 warnings, `make test` 1515 passed (1 nowy),
> `make wasm` OK (web-ui się kompiluje).

> **STATUS 12 (2026-07-08): RUNDA ZEWNĘTRZNA #10 — NAPRAWIONE.** Oba
> findingi [P2] potwierdzone i naprawione. Kontrakt
> `track_dm_handle_change` = „zwraca `false` ⇒ obserwacja ODŁOŻONA (bufor
> zostaje na starym, wciąż odszyfrowywalnym kontekście)". Dwie wewnętrzne
> ścieżki błędu keyringu zwracały `true` (sukces) zamiast odłożyć.
>
> - **[P2] Błąd odczytu configu ≠ „wyłączone"** (`events.rs:2056`
>   `migrate_dm_e2e_config`): `let Ok(Some(cfg)) = get_channel_config(old)`
>   traktował `Err` identycznie jak brak configu → `false`. Pętla w
>   `track_dm_handle_change` szła dalej, cache'owała `@<new>` i zwracała
>   `true`; enabled config uwięziony pod `@<old>` stawał się nieosiągalny,
>   następny send keyował `@<new>` bez configu → plaintext. Fix: funkcja
>   zwraca teraz `Result<bool>` (`Ok(true)` migrated / `Ok(false)` nic do
>   migracji / `Err` = fault keyringu); `set_channel_config` też
>   propaguje `?` zamiast `.is_ok()` (poprzednio cichy `false`). Caller na
>   `Err` loguje i `return false` (odkłada).
> - **[P2] Błąd zapisu cache handle fail-closed** (`events.rs:2174`
>   `cache_dm_handle`): `let _ = ...` połykał błąd, funkcja zwracała
>   `true`; callerzy przesuwali `peer_handle` na `@<new>`. Po zamknięciu
>   bufora `/msg <nick>` rozwiązywał kontekst z cache (stary/pusty), mijał
>   zmigrowany enabled `@<new>` → plaintext. Fix: `if let Err(e) =
>   cache_dm_handle(...) { warn; return false; }` — odłożenie; `@<old>`
>   zostaje enabled i odszyfrowywalny, następne spostrzeżenie ponawia zapis.
>
> Wszystkie 3 call-sites już poprawnie odkładają na `false` (runda #5), więc
> zmiana domyka lukę bez dotykania callerów. Docstringi zaktualizowane.
> Testy migracji przełączone na `.unwrap()` (typ się zmienił).
>
> Weryfikacja: `make clippy` 0 warnings, `make test` 1515 passed.

> **STATUS 13 (2026-07-08): PROAKTYWNY SWEEP ADWERSARYJNY (3 agenty) —
> NAPRAWIONE 3, ZGŁOSZONE do decyzji.** Zamiast czekać na kolejną rundę
> zewnętrzną, przeczesano CAŁĄ powierzchnię fail-open trzema niezależnymi
> agentami (kąt 1: połykanie błędów keyringu; kąt 2: ścieżki send/render;
> kąt 3: cross-network scoping + skrypty towarzyszące).
>
> **Naprawione (klient Rust):**
> - **[MED] Legacy nick-fallback połykał błąd odczytu** (`events.rs:2151`):
>   trzeci brat w `track_dm_handle_change` — `if let Ok(Some(legacy)) =
>   legacy_handle_for_nick(nick)` traktował `Err` jak „brak legacy handle",
>   podczas gdy dwaj bracia (cached read, cache write) już odkładają na
>   `Err`. W DB po upgrade config mógł zostać uwięziony pod `@<old>`, bufor
>   przesunięty na `@<new>` → plaintext. Fix: `match { Err => warn; return
>   false }`. Teraz WSZYSTKIE 4 ścieżki keyringu w tej funkcji fail-closed.
> - **[MED] Wielolinijkowy paste z wiodącym `.`/`!` obchodził E2E dla
>   CAŁEGO bloku** (`e2e_gate.rs:197`): bramka sprawdzała `starts_with(['.',
>   '!'])` na całym sklejonym tekście; `plain_passthrough` + caller
>   dzieliły go per-linia → każda kolejna (tajna) linia szła cleartextem.
>   Fix: bypass botowy TYLKO dla pojedynczej linii (`!text.contains('\n')`);
>   każdy newline → pełna bramka E2E (szyfruje całość). Pokrywa obie ścieżki
>   (`e2e_encrypt_or_passthrough` i `e2e_send_plan_for_target`). Test:
>   `multiline_paste_with_bot_prefix_does_not_bypass_e2e`.
> - **[MED] Skracarka URL wyciekała treść E2E do usługi trzeciej**
>   (`input.rs:1397`): worker shrink POST-ował surowy URL do zewnętrznego
>   API PRZED bramką E2E — treść chronionej rozmowy trafiała do osoby
>   trzeciej cleartextem. Fix: `if !e2e_enabled_for_target(...)` przed
>   dispatchem — przy włączonym E2E shrink jest pomijany, oryginalny URL
>   szyfrowany na drucie jak reszta.
>
> **Zweryfikowane jako bezpieczne (bez zmian):** rdzeń bramki
> (`e2e_encrypt_or_passthrough`/`e2e_send_plan_for_target`) fail-closed;
> `try_decrypt_e2e` nigdy nie renderuje surowego `+RPE2E01`; scoping
> `{network}\x1F{wire}` szczelny (legacy fallback re-scope'owany do bieżącej
> sieci, adopcja wielosieciowa strzeżona, AutoAccept→Normal + warning);
> reassembly batch/multiline; `.ok()` w `e2e_enabled_for_target` dotyczy
> tylko advisory, nie decyzji szyfrowania.
>
> **Świadome escape-hatche (NIE naprawiane — plaintext z założenia):** Lua
> `raw()`, `/quote`, CTCP `/version` — analogiczne do `/quote`, wysyłają
> surowo bez treści użytkownika lub jako jawna furtka. (Do rozważenia:
> advisory `[E2E]` dla Lua `raw()`.)
>
> **DO DECYZJI UŻYTKOWNIKA — skrypty towarzyszące (Perl/Python), osobny
> workstream:** agent #3 potwierdził, że `scripts/weechat/rpe2e.py` i
> `scripts/irssi/rpe2e.pl` przepuszczają `/me`, `/msg`, `/say` (każda linia
> zaczynająca się od `/`) jako PLAINTEXT do rozmów E2E — jedyna bramka
> wychodząca bailuje na `^/`. To realny cichy wyciek [HIGH] po stronie
> skryptów, ale wymaga dodania hooków wychodzących w obu skryptach (duża,
> osobna zmiana). Dodatkowo: irssi mis-renderuje przychodzące zaszyfrowane
> ACTION-y (dekrypcja po CTCP-split) [MED, PLAUSIBLE]; skrypty nie
> scope'ują kontekstów per-sieć [LOW-MED, PLAUSIBLE]. NIE ruszane bez
> zgody — patrz rozmowa.
>
> Weryfikacja: `make clippy` 0 warnings, `make test` 1516 passed (1 nowy).

- **Data review:** 2026-07-01
- **Zakres:** pełny diff PR #29 (`main...fix/various-improvements`, stan po commicie `96580de`)
- **Metoda:** 8 niezależnych kątów wyszukiwania (line-by-line, removed-behavior, cross-file, reuse, simplification, efficiency, altitude, conventions) → dedup → 12 osobnych weryfikatorów (po jednym na kandydata, verdict CONFIRMED/PLAUSIBLE/REFUTED z cytatami z kodu)
- **Wynik:** 9× CONFIRMED, 3× PLAUSIBLE, 0× w pełni odrzuconych. Konwencje CLAUDE.md: czysto (clippy 0 warnings, tracing, APP_NAME, state/ UI-agnostic — wszystko OK).

**Przegląd:** PR wprowadza recipient-keyed konteksty DM E2E (encrypt → `@<peer_handle>`, decrypt → `@<own_handle>`), śledzenie własnego handle przez one-shot USERHOST, migrację configów przy CHGHOST/PRIVMSG/handshake, cache handle'i `e2e_dm_handle_cache` w SQLite, fail-closed komunikaty `E2eRefusal` oraz naprawę renderowania notek trust-change przez `event_params`. Kierunek dobry i spójny z addendum lurkera (`docs/rpe2e-dm-addendum.md`) — ale weryfikacja potwierdziła **dwie realne dziury fail-open (plaintext na drucie)** i kilka problemów z zaufaniem/kontekstami.

---

## Znaleziska (od najpoważniejszego)

### 1. `src/app/input.rs:1895` — błąd odczytu keyringa przy sprawdzaniu `enabled` nadal wysyła plaintext ⚠️ *(CONFIRMED)*

W `e2e_encrypt_or_passthrough`:

```rust
let enabled = mgr.keyring().get_channel_config(&context).ok().flatten().is_some_and(|c| c.enabled);
if !enabled { return plain_passthrough(); }
```

`.ok().flatten()` mapuje `Err` (fallible SQLite `query_row(...).optional()?` na pliku) na „nie włączone" → `plain_passthrough()` → **cleartext na drucie**. To dokładnie ta sama klasa błędu, którą 30 linii wyżej (`input.rs:1856-1867`) łapiemy jako `Err(E2eRefusal::KeyringRead)` z komentarzem „Never fall through to plaintext on a read error". Ten sam wzorzec swallow przy legacy bare-nick check na `input.rs:1874-1879`.

**Scenariusz:** `/e2e on` dla kanału lub DM (config enabled istnieje, peer_handle już na buforze); transient błąd odczytu SQLite (I/O, disk full, WAL fault) → `enabled=false` → wiadomość idzie plaintextem zamiast odmowy.

**Fix:** zamienić oba `.ok().flatten()` na match → `Err(E2eRefusal::KeyringRead)`. Jednolinijkowy.

---

### 2. `src/app/input.rs:1762` — `/close` bufora podczas shrink-wait omija retry keyringa → plaintext ⚠️ *(CONFIRMED)*

Łańcuch (każde ogniwo zweryfikowane):

1. Capture przy dispatchu (`input.rs:1443-1453`) traktuje `Err` z keyringa jako `None` z komentarzem „authoritative refuse-vs-plaintext decision is re-made at send time".
2. `/close` na Query (`handlers_ui.rs:221-224`) robi tylko `remove_buffer` — **nie czyści** kolejki shrink.
3. `ShrinkDeliver::Outgoing` (`shrink.rs:466-468`) nie sprawdza istnienia bufora — wysyłka odpala się mimo zamknięcia (celowy design dla `/nick`/`/close`).
4. `resolve_query_peer_handle` (`input.rs:1762-1763`) na brakującym buforze zwraca `Ok(None)` **przed** odczytem keyringa — jego doc-comment („Ok(None) ⇒ no `@<handle>` config can exist") jest w tym przypadku fałszywy.
5. Legacy-check na gołym nicku nie znajduje configu (żyje pod `@<handle>`) → `plain_passthrough()` → **PRIVMSG cleartextem**.

**Scenariusz:** E2E-enabled DM, peer milczy w tej sesji (config osiągalny tylko przez cache); wiadomość z długim URL-em idzie przez shrink; odczyt keyringa przy capture transientnie erroruje → `None`; user robi `/close` w trakcie (budżet ~2s); deferred send → plaintext.

**Uwaga:** normalna ścieżka (bufor istnieje) JEST fail-closed — retry keyringa erroruje ponownie → `KeyringRead`. Dziura wisi wyłącznie na buffer-missing early return.

**Fix:** rozróżnić w capture „keyring errored" od „nic nie znaleziono" (odmowa już przy dispatchu, albo poisoned marker w `OutgoingDeliver.peer_handle` traktowany jako `KeyringRead`); i/lub w deferred path traktować buffer-missing + Query + nierozwiązany handle jako odmowę, nie passthrough.

---

### 3. `src/commands/handlers_e2e.rs:691` + `src/irc/events.rs:4525` — `/e2e forget` po cichu pomija kontekst `@<own>` gdy własny handle nieznany *(CONFIRMED, zgłoszone niezależnie przez 4 findery)*

Obie ścieżki (bezpośrednia `perform_e2e_forget` i deferred przez USERHOST) przy `conn.own_handle == None` przekazują `own_channel = None` do `forget_peer_on_dm_contexts`, które wtedy czyści **tylko** `@<peer>` (`manager.rs:759-772`: `None` wpada w `_ => Ok(n)`), a `forget_peer_on_channel` kasuje incoming session tylko dla dokładnej pary `(handle, channel)` — TRUSTED incoming session pod `@<own>` **przeżywa**. UI mimo to raportuje sukces: „forgot {target} ({handle}) — removed N row(s)".

Okno jest realne przy **każdym reconnect**: `own_handle` resetowany na RPL_WELCOME (`app/irc.rs:717`), re-seed dopiero przy async odpowiedzi na self-USERHOST (lub nigdy, jeśli serwer filtruje USERHOST). Własny komentarz kodu przyznaje stawkę: „otherwise the peer's trusted incoming session survives and their messages still decrypt".

**Niespójność:** wszystkie siostrzane komendy hard-errorują w tym samym stanie — revoke (`:566`), unrevoke (`:603`), handshake (`:733`), list (`:785`), verify (`:969`): „own handle not yet known". Forget nie. Ścieżka `all=true` (`forget_peer_everywhere`) pokrywa problem przez `delete_incoming_sessions_for_handle` — dziura dotyczy tylko non-all.

**Scenariusz:** reconnect → RPL_WELCOME resetuje own_handle → user robi `/e2e forget mallory` zanim dojdzie self-USERHOST → `@mallory@host` wyczyszczony, UI mówi sukces → self-USERHOST re-seeduje ten sam handle → wiadomości Mallory dalej deszyfrują się jako trusted, mimo że user myśli, że ją zapomniał.

**Fix:** forget powinien errorować „own handle not yet known" jak reszta (lub minimum: warn, że incoming-trust row nie został wyczyszczony).

---

### 4. `src/irc/events.rs:1570→4229` — echo-message: brak guardu `is_own` przed dispatchem RPE2E *(CONFIRMED)*

`handle_notice` woła `try_dispatch_rpe2e_ctcp` na linii **1570**, a `is_own` liczy dopiero na **1582**. `echo-message` jest w żądanych capach (`src/irc/cap.rs:139`), a handshaki KEYREQ/KEYRSP/REKEY wychodzą jako NOTICE (`src/app/web.rs:363`) — więc serwer echuje je z naszym pełnym prefixem. Skutki:

- `observe_dm_peer_handle(conn, nasz_nick, nasz_handle)` → `cache_dm_handle` zapisuje `(network, nasz_nick) → nasz_handle` do cache'u **peerów** — dokładnie ta pollution, przed którą broni się nowy guard `is_own` w `handle_chghost` (komentarz na `events.rs:2101-2105`).
- Jeśli `last_handle_for_nick(nasz_nick)` trzyma już inny handle → spurious `migrate_dm_e2e_config` dla naszych własnych kontekstów.
- Echowany KEYREQ wpada dalej do `mgr.handle_keyreq_with_nick(...)` — **brak odrzucenia self-handshake** (target echa jest jawnie ignorowany: `let _ = target;` na `:4219`).

**Scenariusz:** na serwerze z echo-message wysyłamy dowolny handshake → cache mapuje nasz nick na nasz handle. Później zmieniamy nick / rozłączamy się, ktoś przejmuje stary nick, otwieramy query i robimy `/e2e on` zanim się odezwie → `last_handle_for_nick` zwraca NASZ stary handle → DM kluczowany pod `@<nasz_stary_handle>` — zły kontekst.

**Fix:** policzyć `is_own` (prefix nick vs `conn.nick`, case-insensitive) przed dispatchem RPE2E w `handle_notice`/`handle_privmsg`, albo skip `observe_dm_peer_handle` + odrzucenie handshake w `try_dispatch_rpe2e_ctcp` gdy nadawca to my.

---

### 5. `src/e2e/keyring.rs:983` — legacy fallback `e2e_peers` bez filtra sieci migruje config do obcego o tym samym nicku *(CONFIRMED)*

Fallback: `SELECT last_handle FROM e2e_peers WHERE last_nick = ?1 COLLATE NOCASE ... ORDER BY last_seen DESC LIMIT 1` — brak kolumny/filtra network (doc-comment to przyznaje). `track_dm_handle_change` (`events.rs:2025-2033`) przy świeżym buforze (prev=None) traktuje wynik jako „poprzedni handle" i woła `migrate_dm_e2e_config(@<netA-handle>, @<netB-stranger>)`, a `migrate_dm_e2e_config` (`events.rs:1976-1996`) kopiuje `enabled=true` **bez żadnego guardu** tożsamości/sieci. Potem `cache_dm_handle` pinuje handle obcego pod NetB — stan trwały, nic go nie samo-leczy.

Tabela `e2e_dm_handle_cache` jest **nowa w tym PR**, więc po upgrade cache jest pusty u wszystkich, a `e2e_peers` trzyma stare handle — trigger realny.

**Scenariusz:** `/e2e on` z bobem na NetA (pre-upgrade keyring). Po upgrade inny „bob" z NetB wysyła pierwszy plaintext DM → config `enabled` kopiowany na kontekst obcego → nasza odpowiedź rusza auto-KEYREQ z niewłaściwą osobą pod politiką, której user nie włączał. Łagodzi: TOFU `last_handle` celowo nie jest bumpowany, więc handshake z obcym sklasyfikuje się jako HandleChanged/new i trafi w reverify gate — ale enable jest nieautoryzowany i trwały do ręcznego `/e2e off`.

**Fix:** nie używać network-agnostic fallbacku jako źródła „prev handle" do migracji (ograniczyć fallback do ścieżki wysyłki, gdzie błąd jest fail-closed), albo wymagać potwierdzenia fingerprintu przed migracją enabled configu.

---

### 6. `src/commands/handlers_e2e.rs:530` — `/e2e accept` po restarcie flipuje zero-key placeholder na Trusted *(CONFIRMED — pre-existing, w funkcji dotkniętej PR-em)*

Normal-mode inbound KEYREQ persystuje Pending incoming session z `sk=[0u8;32]` (`cache_pending_inbound_normal_mode`, `manager.rs:1122-1143`, `INSERT OR REPLACE` — trwałe), a sam KEYREQ trzyma tylko w in-memory `pending_inbound` (Mutex<HashMap>, pusty po starcie; **nic nie repopuluje z DB**). Po restarcie `/e2e accept bob` → `accept_pending_inbound` → `Ok(None)` → fallback `update_incoming_status(..., Trusted)` flipuje zerowy klucz na Trusted → „accepted bob…" → **żaden KEYRSP nie wychodzi**, Bob nie może nas deszyfrować, my jego też nie (zero-key), bez diagnostyki. Gorzej niż przed accept: row nie jest już Pending, więc early-reject i re-cache na świeży KEYREQ przestają działać.

Dziura **pre-datuje PR #29** (PR dotknął tylko głowy funkcji: `current_channel` → `current_e2e_context`) — raportować jako pre-existing, nie regresję.

**Fix:** fallback `Ok(None)` powinien sprawdzić `sk != [0u8;32]` / status Pending-placeholder i odmówić z komunikatem „pending request lost on restart — ask peer to re-handshake", zamiast flipować status.

---

### 7. `src/app/input.rs:1852` — deferred shrink pinuje handle z chwili dispatchu; migracja w oknie ~2s → wiadomość bezpowrotnie niedeszyfrowalna *(CONFIRMED, wąskie okno)*

`match captured_peer_handle { Some(h) => ... }` bezwarunkowo wygrywa — żywa rezolucja (`resolve_query_peer_handle`, która widziałaby zaktualizowany `buf.peer_handle`) nigdy nie jest konsultowana. Jeśli w trakcie shrink-wait (budżet `shrink.outgoing_timeout_ms`, default **2000ms**) peer odezwie się z nowego hosta: migracja kopiuje config na `@<new>`, ale `@<old>` **celowo zostaje enabled** (TOFU rationale, `events.rs:1964-1970`), więc deferred send znajduje `enabled=true` pod `@<old>`, a `encrypt_outgoing` zawsze się uda (wygeneruje świeży klucz) z `@<old>` wpiętym w AAD. Peer deszyfruje pod swoim aktualnym `@<own>=@<new>` → klucz i AAD się nie zgadzają → **utrata jednej wiadomości** (re-handshake jej nie odzyska). Fail-closed — poufność zachowana, bug availability.

**Scenariusz:** Alice ma `/e2e on` z milczącym Bobem (handle z cache `~bob@old.host`); wysyła wiadomość z długim URL-em → shrink defer; w oknie 2s Bob (po reconnect z `new.host`) pisze do niej → migracja; deferred send szyfruje pod `@~bob@old.host` → Bob widzi decrypt-failure i daremny KEYREQ.

**Fix:** przy deliver najpierw żywa rezolucja, captured handle tylko gdy bufor zniknął (zachowuje ochronę z pkt 2, odzyskuje świeżość).

---

### 8. `src/irc/events.rs:1492` — placeholder „[E2E: awaiting our own identity]" nigdy nie znika po decrypted replay *(CONFIRMED, kosmetyczne)*

Placeholder jest transient (bez `@msgid`, nie persystowany — celowo, żeby replay z CHATHISTORY nie zdedupował się z nim i nie zginął; test `decrypted_replay_surfaces_past_a_tagless_placeholder` pilnuje tego kierunku). Ale **nic go nie usuwa**: `surface_history_rows` tylko wstawia, jedyny `retain` w tej ścieżce czyści `backlog_end`. Decrypted replay ląduje tuż **za** placeholderem (ten sam `@time`) — user widzi obie linie do końca sesji. Korekta względem pierwotnego zgłoszenia: **detach/reattach NIE leczy** (session daemon trzyma bufor w pamięci) — leczy dopiero restart albo eviction scrollbacka.

**Fix:** przy splice'owaniu decrypted rows do bufora DM usunąć placeholder (retain po tekście placeholdera scoped do tego bufora, gdy pojawi się row z realnym `@msgid`).

---

## Wycięte przez cap ≤8 (zweryfikowane, niższa waga)

- **`src/irc/events.rs:1164` — leak surowego `+RPE2E01` przy echo własnej wiadomości z nick-only prefix** *(CONFIRMED, low)*. Wymaga jednocześnie: echo/replay własnego szyfrogramu, prefix bez ident@host (niestandardowy — bouncer/relay), `own_handle` jeszcze None. Wtedy `incoming_e2e_context → None`, placeholder-arm gated na `!is_own`, `None => None` → surowy szyfrogram renderowany i logowany. Pre-PR `try_decrypt_e2e` połykał echo (`is_own → Some("")`). Leak szyfrogramu (nie plaintextu). Fix: w `None`-arm połykać także `is_own && text.starts_with("+RPE2E01")`.
- **`src/irc/events.rs:1255` — lenient `is_rpe2e_handshake` vs strict `is_ctcp` przy PRIVMSG** *(PLAUSIBLE)*. Handshake-looking PRIVMSG unframed/half-framed/unparseable nie dostaje ANI generic trackingu (suppressed), ANI `observe_dm_peer_handle` (dispatch tylko przy obu `\x01`; parse-fail wychodzi przed observe na `:4205/:4214`). Nieosiągalne od konformnego peera (handshaki idą NOTICE-ami, ścieżka NOTICE jest lenient i bezwarunkowa na `:1570`); skutek fail-closed (niedeszyfrowalny szyfrogram, self-heal przy pierwszym zwykłym PRIVMSG). Bonus: komentarz na `:1389-1391` twierdzi, że PRIVMSG-fallback obsługuje „stripped trailing framing" — a strict `is_ctcp` właśnie ten przypadek wyklucza. Hardening: `is_rpe2e_handshake = is_ctcp && starts_with(CTCP_TAG)` albo lenient dispatch też dla PRIVMSG.
- **Duplikacja resolverów handle** *(PLAUSIBLE, cleanup)*: `App::resolve_query_peer_handle` (`input.rs:1757`, ścieżka wysyłki) i `resolve_cached_handle_by_nick`+`current_e2e_context` (`handlers_e2e.rs:1269/316`, ścieżka `/e2e`) implementują niezależnie identyczną regułę „żywy `buf.peer_handle`, else network-scoped `last_handle_for_nick`", spójność pilnowana tylko komentarzami. Konkretna dywergencja cross-connection **odrzucona** (wszystkie `/e2e` działają na aktywnym buforze). Realna różnica drugorzędna: `current_e2e_context` połyka błąd keyringa (`.and_then(Result::ok)`) — mylący komunikat, ale fail-closed. Warto zunifikować za jednym helperem — cała premisa PR to identyczne kluczowanie obu warstw.
- **Brak indeksu `e2e_peers(last_nick)`** *(PLAUSIBLE, pomijalne)*: fallback robi full scan na każdy cache-miss (per wysłany DM do milczącego peera, na głównym tasku TUI), ale tabela trzyma tylko E2E-peerów (mikrosekundy). Ewentualnie: `CREATE INDEX ... ON e2e_peers(last_nick COLLATE NOCASE)`. **Uwaga:** backfill-on-fallback-hit do cache'u byłby NIEpoprawny (prałby network-agnostic handle do tabeli traktowanej jako network-scoped autorytatywna) — patrz też znalezisko #5.

## Odrzucone w weryfikacji

- Cross-connection dywergencja resolverów (patrz wyżej) — `/e2e` zawsze działa na aktywnym buforze.
- Wcześniejsze (poprzednia sesja review): „TOFU/policy bypass w `observe_dm_peer_handle`" — migracja nie omija pinów TOFU (`classify_peer_change` kluczuje po fingerprint+handle); addendum lurkera jawnie dopuszcza auto-accept rule w HandleChanged. „Redundant clone `.map(str::to_string)`" — konieczna konwersja `&str`→`String`.

---

## Rekomendowana kolejność napraw

| # | Znalezisko | Waga | Koszt fixu |
|---|-----------|------|-----------|
| 1 | `.ok().flatten()` fail-open (input.rs:1874, 1895) | **wysoka — plaintext** | trywialny |
| 2 | buffer-missing → plaintext w deferred send (input.rs:1762) | **wysoka — plaintext** | mały |
| 3 | cichy partial forget (handlers_e2e.rs:691, events.rs:4525) | wysoka — trust | mały (error jak w revoke) |
| 4 | echo-message self-dispatch (events.rs:1570/4229) | średnio-wysoka | mały (guard is_own) |
| 5 | cross-network migracja z legacy fallbacku (keyring.rs:983 → events.rs:2023) | średnia | średni |
| 6 | accept flipuje zero-key row (handlers_e2e.rs:530) | średnia (pre-existing) | mały |
| 7 | stale pinned handle w shrink (input.rs:1852) | niska-średnia (utrata 1 msg) | mały |
| 8 | placeholder nie znika (events.rs:1492) | niska (kosmetyka) | mały |

Punkty 1+2 łamią dokładnie tę zasadę fail-closed, którą ten branch wprowadza — do naprawy przed merge. Punkty 3+4 to higiena zaufania w duchu addendum lurkera. Reszta może iść follow-upem.
