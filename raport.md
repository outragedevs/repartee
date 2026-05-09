# Raport: weryfikacja ścieżek nicklist / JOIN / PART / QUIT / split

## Wniosek główny

Problem nie jest w renderze prawego sidepanelu. Nicklista po prawej czyta bezpośrednio `active_buffer().users`, więc jeśli nick zostaje na liście, to błąd jest wcześniej, w utrzymaniu `Buffer.users`.

Potwierdzenie:
- `src/ui/nick_list.rs:26-31` renderuje bezpośrednio `buf.users.values()`

## Zweryfikowane ścieżki

### 1. Disconnect / reconnect nie czyści nicklist kanałów

Status: krytyczne

- `src/irc/events.rs:334-365` (`handle_disconnected`) zapisuje kanały do rejoinu, ale nie czyści `buf.users`
- po reconnect `src/irc/events.rs:2524-2541` (`RPL_NAMREPLY`) tylko dodaje albo nadpisuje wpisy
- `src/irc/events.rs:3053-3054` (`RPL_ENDOFNAMES`) jest obecnie no-op

Skutek:
- jeśli przed disconnectem user był w `users`, a po reconnect serwer już go nie zwróci w NAMES, ten nick dalej zostaje lokalnie i w sidepanelu

### 2. Własny JOIN do istniejącego bufora nie resetuje nicklisty

Status: krytyczne

- `src/irc/events.rs:1185-1248` (`handle_join`)
- przy naszym JOIN bufor kanału jest tworzony tylko jeśli go wcześniej nie ma
- jeśli bufor już istnieje, np. po reconnect/autojoin, stara mapa `users` zostaje
- późniejsze `RPL_NAMREPLY` nie robi pełnej synchronizacji, tylko dopisuje użytkowników

Skutek:
- stary skład kanału może przetrwać reconnect albo autojoin mimo że serwer zwrócił już inny stan

### 3. Skrypty mogą zablokować mutację stanu nicklisty

Status: wysokie

- `src/app/irc.rs:613-623`
- event IRC trafia do skryptów przed `handle_irc_message`
- jeśli skrypt suppressuje `irc.part`, `irc.quit`, `irc.kick` albo `irc.nick`, główny handler nie odpala

Skutek:
- nicklista nie zostanie zaktualizowana, mimo że wiadomość przyszła z serwera

Wniosek:
- suppress nie powinien blokować podstawowej synchronizacji stanu
- suppress powinien ewentualnie blokować display/default output, ale nie mutację modelu

### 4. Wygasły IRCv3 batch jest porzucany bez replayu

Status: wysokie

- `src/app/irc.rs:537-576` przechwytuje wiadomości batchowane zanim trafią do zwykłego handlera
- `src/irc/batch.rs:68-83` (`purge_expired`) usuwa wygasłe batch-e
- jeśli nie przyjdzie `BATCH -tag`, wiadomości wewnątrz batcha mogą nigdy nie zostać przetworzone

Skutek:
- QUIT/JOIN ukryte w takim batchu mogą nie zmutować `Buffer.users`
- to daje realną ścieżkę do zostających nicków

### 5. Zwykłe PART / QUIT / KICK / NICK działają poprawnie

Status: OK

- `src/irc/events.rs:1553-1627` (`handle_part`) usuwa nick z kanału
- `src/irc/events.rs:1632-1759` (`handle_quit`) usuwa nick ze wszystkich buforów danego połączenia
- `src/irc/events.rs:1904-1997` (`handle_kick`) usuwa wyrzuconego usera z kanału
- `src/irc/events.rs:1762-1901` (`handle_nick_change`) robi rename we wszystkich buforach

Wniosek:
- same podstawowe ścieżki są poprawne, o ile rzeczywiście dochodzi do ich wykonania

### 6. Ukończony NETSPLIT batch usuwa nicki poprawnie

Status: OK z zastrzeżeniem

- `src/irc/batch.rs:164-234` przetwarza `NETSPLIT`
- podczas finalizacji batcha użytkownicy są zdejmowani z `users`
- istnieje też test dla porównań case-insensitive w tej ścieżce

Zastrzeżenie:
- działa poprawnie tylko wtedy, gdy batch zostanie domknięty i przetworzony
- problem wraca przy wygasłym albo niedomkniętym batchu

### 7. Heurystyczny split tracking ma case-sensitive indeks nicków

Status: średnie

- `src/irc/netsplit.rs:74-120` zapisuje informacje o quitach splitowych
- `src/irc/netsplit.rs:122-173` i `176-225` używa exact-match lookupów

Skutek:
- to bardziej psuje dokładność heurystyki netjoin/netsplit niż samo podstawowe usuwanie userów
- główny problem z zostającymi nickami nadal jest wcześniej: reconnect, reuse starego bufora albo batch expiry

## Miejsca, które nie są winne

- `src/ui/nick_list.rs:26-31`

UI nie utrzymuje osobnego cache nicklisty. Jeśli nick zostaje po prawej, to model kanału jest już zły.

## Rekomendowana naprawa

Najlepsze rozwiązanie:

- zrobić atomową synchronizację NAMES
- `RPL_NAMREPLY` powinien zbierać użytkowników do tymczasowej mapy per kanał
- `RPL_ENDOFNAMES` powinien podmieniać całe `buf.users` na świeży snapshot z serwera

To rozwiązuje klasę błędów, w której lokalna mapa jest tylko częściowo nadpisywana.

## Hotfixi, które warto zrobić niezależnie

1. Czyścić `users` wszystkich buforów kanałowych w `handle_disconnected`.
2. Czyścić `users` przy naszym własnym JOIN do istniejącego już bufora.
3. Rozdzielić suppress skryptów od synchronizacji stanu, żeby core zawsze przetwarzał JOIN/PART/QUIT/KICK/NICK.
4. Przy `purge_expired()` replayować zebrane wiadomości albo przynajmniej przetwarzać częściowy stan batcha zamiast go gubić.

## Podsumowanie

Najmocniejsze i najbardziej prawdopodobne źródła raportowanego problemu są trzy:

1. reconnect bez wyczyszczenia `users`
2. reuse istniejącego bufora kanału przy własnym JOIN
3. batch expiry oraz suppress skryptów omijające normalną ścieżkę mutacji

Jeśli nick zostaje w sidepanelu mimo że nie ma go już na kanale albo na całym IRC, to praktycznie zawsze oznacza to, że `Buffer.users` nie dostał pełnej resynchronizacji albo nie przeszedł przez właściwy handler usuwający usera.
