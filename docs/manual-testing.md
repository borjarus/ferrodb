# Scenariusz ręcznego przeklikania aplikacji (Faza 10)

Przewodnik weryfikacji manualnej interfejsu użytkownika FerroDb przygotowany dla **Fazy 10 (stabilizacja i wydanie 1.0)**. 
Obejmuje wszystkie nowe mechanizmy: aktualizacje, raportowanie awarii, menu Pomoc, poprawki edycji siatki i źródeł danych oraz lokalizację (język polski).

---

## 0. Przygotowanie środowiska i uruchomienie

Aplikację można uruchomić w dwóch wariantach:
1. **Tryb Mock (`--mock`)** — nie wymaga działających baz danych ani połączeń sieciowych, wszystkie dane demonstracyjne i scenariusze działają w pamięci:
   ```sh
   cargo run -p ferrodb-app -- --mock
   ```
2. **Prawdziwy rdzeń (Core)** — uruchomienie produkcyjne:
   ```sh
   cargo run -p ferrodb-app
   ```
3. *(Opcjonalnie)* **Izolowany profil testowy** (aby nie modyfikować własnych ustawień w `~/Library/Application Support/dev.FerroDb.FerroDb`):
   ```sh
   FERRODB_HOME=/tmp/ferrodb-manual-test cargo run -p ferrodb-app -- --mock
   ```

Sprawdzenie wersji w CLI:
```sh
cargo run -p ferrodb-app -- --version
# Oczekiwany wynik: ferrodb 0.1.0 (API v8, dev channel)
```

---

## 1. Scenariusz: Menu Pomoc (Help) i Paleta poleceń

### A. Główne menu aplikacji
W menu systemowym przejdź do sekcji **Help** (lub **Pomoc**):
- [ ] **Check for Updates… / Sprawdź aktualizacje…**:
  - Wybierz pozycję.
  - Sprawdź, czy pojawia się czytelne powiadomienie lub okno stanu (w buildzie bez skonfigurowanego serwera aktualizacji powinien pojawić się łagodny komunikat informacyjny, a nie awaria aplikacji).
- [ ] **Report a Problem… / Zgłoś problem…**:
  - Wybierz pozycję.
  - Aplikacja powinna skopiować do schowka diagnostykę systemową (wersja, API v8, kanał, system operacyjny) i otworzyć w przeglądarce stronę zgłaszania błędów w repozytorium GitHub (`/issues/new`).
  - Wklej zawartość schowka w dowolnym edytorze i upewnij się, że nie zawiera wrażliwych haseł.
- [ ] **Join Beta Program… / Dołącz do programu beta…**:
  - Wybierz pozycję.
  - Powinno otworzyć się okno ustawień aktualizacji z informacją o kanale Beta lub nastąpić przełączenie na kanał Beta.
- [ ] **About FerroDb / O programie FerroDb**:
  - Sprawdź treść dialogu:
    - Nazwa i wersja: `FerroDb 0.1.0`
    - Wersja kontraktu: `API v8`
    - Kanał aktualizacji: `dev` (lub `stable`/`beta`)
    - Typ backendu: `mock` (w trybie mock) lub `core`
    - Przycisk sprawdzania aktualizacji.

### B. Paleta poleceń (Command Palette)
- [ ] Naciśnij skrót `Cmd+Shift+P` (macOS) lub `Ctrl+Shift+P` (Linux/Windows).
- [ ] Wpisz `Updates`, `Beta`, `Report`, `About` (lub po polsku `Aktualizacje`, `Pomoc`).
- [ ] Upewnij się, że wszystkie akcje są dostępne i dają ten sam rezultat co z poziomu menu.

---

## 2. Scenariusz: Ustawienia „Aktualizacje i prywatność” (Updates and Privacy)

### A. Nawigacja do ustawień
- [ ] Otwórz ustawienia skrótem `Cmd+,` (lub przez menu `FerroDb ▸ Settings` / `Ustawienia`).
- [ ] Przejdź do zakładki **Updates and Privacy** (lub **Aktualizacje i prywatność**).

### B. Opcje aktualizacji
- [ ] **Wybór kanału (Update Channel)**:
  - Przełącz między `Stable` (Stabilny) a `Beta`.
  - Sprawdź, czy opis pod polem aktualizuje się czytelnie.
- [ ] **Sprawdzaj automatycznie (Check automatically)**:
  - Przełącz przełącznik on/off.
- [ ] **Przycisk „Check Now” / „Sprawdź teraz”**:
  - Kliknij przycisk.
  - Zweryfikuj, czy pojawia się wskaźnik sprawdzania, a następnie czytelna informacja o wyniku (brak błędu krytycznego).

### C. Zgoda na raporty awarii (Crash Reporting Consent)
- [ ] Sprawdź dostępne opcje wyboru:
  - `Ask` (Pytaj za każdym razem — domyślne),
  - `Always` (Zawsze wysyłaj automatycznie),
  - `Never` (Nigdy nie wysyłaj).
- [ ] Zmień wartość np. na `Ask` lub `Always`.
- [ ] **Przycisk „Open Reports Folder” / „Otwórz folder raportów”**:
  - Kliknij przycisk — powinien otworzyć się natywny menedżer plików (Finder na macOS, Explorer na Windows).
- [ ] **Przycisk „View Reports” / „Pokaż raporty”**:
  - Kliknij — powinno pojawić się okno/lista dotychczasowych raportów awarii.
- [ ] **Przycisk „Clear” / „Wyczyść”**:
  - Przycisk powinien usunąć stare raporty i zaktualizować widok.

### D. Trwałość ustawień po restarcie
- [ ] Zamknij aplikację (`Cmd+Q`).
- [ ] Sprawdź zawartość pliku konfiguracyjnego `release.toml`:
  - Na macOS: `~/Library/Application Support/dev.FerroDb.FerroDb/release.toml` (lub `$FERRODB_HOME/config/release.toml`).
- [ ] Uruchom aplikację ponownie i sprawdź, czy wybrane opcje (kanał, automatyczne sprawdzanie, poziom zgody) zostały zachowane.

---

## 3. Scenariusz: Okno po awarii (Crash Dialog) i maskowanie danych

Aplikacja FerroDb posiada mechanizm przechwytywania panik (`panic hook`) oraz rejestrowania ostatnich linii logów w bezpiecznym buforze pierścieniowym.

### A. Symulacja raportu awarii
Aby przetestować okno dialogowe bez konieczności crashowania aplikacji, wygeneruj plik raportu w katalogu danych:

```sh
# Dla standardowego uruchomienia:
DATA_DIR="$HOME/Library/Application Support/dev.FerroDb.FerroDb"
# Dla uruchomienia z FERRODB_HOME=/tmp/ferrodb-manual-test:
# DATA_DIR="/tmp/ferrodb-manual-test/data"
# Dla uruchomienia z --mock bez FERRODB_HOME (katalog tymczasowy):
# DATA_DIR="$TMPDIR/ferrodb-mock"

mkdir -p "$DATA_DIR/crashes"
cat << 'EOF' > "$DATA_DIR/crashes/20261124-120000-test.json"
{
  "id": "20261124-120000-test",
  "time": "2026-11-24T12:00:00Z",
  "app_version": "0.1.0",
  "os": "macos aarch64 15.0",
  "thread": "main",
  "message": "connection failed: postgres://user:super_secret_password@db.example.com:5432/prod with token Bearer sk-ant-secret12345",
  "location": "crates/ferrodb-core/src/lib.rs:123:45",
  "backtrace": "stack backtrace:\n   0: ferrodb_core::test_panic\n   1: ferrodb_app::main",
  "log_tail": [
    "INFO connecting to postgres://user:super_secret_password@db.example.com:5432/prod",
    "WARN connection timed out after 30s"
  ],
  "sent": false,
  "comment": ""
}
EOF
```

### B. Weryfikacja okna dialogowego
- [ ] Uruchom aplikację:
  ```sh
  cargo run -p ferrodb-app
  ```
- [ ] Przy ustawieniu zgody `Ask` natychmiast po uruchomieniu powinno pojawić się okno dialogowe:
  **„FerroDb closed unexpectedly last time” / „FerroDb został nieoczekiwanie zamknięty ostatnim razem”**.
- [ ] **Weryfikacja maskowania danych (Secret Masking)**:
  - Rozwiń szczegóły raportu (Backtrace / Szczegóły / Logi).
  - Upewnij się, że wrażliwe hasła i tokeny zostały zamaskowane:
    - zamiast `super_secret_password` powinno być `***` (np. `postgres://user:***@...`),
    - tokeny typu `Bearer sk-ant-...` powinny być zamaskowane.
- [ ] **Pole komentarza użytkownika**:
  - Wpisz przykładowy komentarz (np. *„Wystąpiło podczas łączenia z bazą testową”*).
- [ ] **Weryfikacja akcji przycisków**:
  - **Przycisk „Send” / „Wyślij”**:
    - W buildzie deweloperskim bez skonfigurowanego endpointu serwera raportów kliknięcie powinno wyświetlić czytelny komunikat o braku serwera oraz zaoferować przyciski **„Copy Report” / „Kopiuj raport”** oraz **„Open Reports Folder”**.
    - Kliknij „Copy Report” i wklej zawartość — upewnij się, że JSON jest kompletny i zamaskowany.
  - **Przycisk „Don't Send” / „Nie wysyłaj”**:
    - Zamknij dialog tym przyciskiem. Raport powinien zostać dopisany do `dismissed.txt` i nie pojawiać się przy kolejnym starcie.
  - **Opcja „Always Send” / „Zawsze wysyłaj”**:
    - Sprawdza automatyczne raportowanie bez pokazywania okna dialogowego.

---

## 4. Scenariusz: Poprawki Fazy 10 w edycji siatki i źródeł danych

### A. Edycja wyników w siatce (PostgreSQL, MySQL, SQLite)
Wcześniejszy problem: siatka wyników błędnie blokowała edycję komunikatem *„cannot be edited”*.
- [ ] Uruchom aplikację w trybie mock lub połącz się z lokalną bazą SQLite:
  ```sh
  cargo run -p ferrodb-app -- --mock
  ```
- [ ] Wybierz scenariusz tabeli lub wykonaj zapytanie `SELECT * FROM ...`.
- [ ] Kliknij dwukrotnie komórkę tabeli wynikowej:
  - [ ] Komórka powinna przejść w tryb edycji (pole tekstowe).
  - [ ] Zmień wartość i naciśnij `Enter` (zatwierdzenie) lub `Esc` (anulowanie).
  - [ ] Zmiana powinna zostać oznaczona w siatce (pasek oczekujących modyfikacji / generowanie DML).

### B. Edycja zapisanego źródła danych (Data Source Dialog)
Wcześniejszy problem: ponowna edycja źródła resetowała konfigurację tunelu SSH, tryb SSL, opcje zaawansowane oraz gubiła ręcznie dopisane klucze (np. `ssl.ca`).
- [ ] Otwórz dialog dodawania/edycji źródła danych (**Add / Edit Data Source**).
- [ ] Skonfiguruj przykładowe parametry:
  - Baza: PostgreSQL lub MySQL.
  - Włącz zakładkę **SSH Tunnel** i uzupełnij host bastion (`bastion.internal`), port (`2222`), użytkownika (`tunnel_user`).
  - W zakładce **SSL** wybierz tryb inny niż domyślny (np. `require` lub `verify-full`).
  - W filtrze schematów wpisz wzorzec (np. `public, analytics_*`).
  - W opcjach zaawansowanych dodaj niestandardowy parametr.
- [ ] Zapisz źródło danych.
- [ ] Otwórz ponownie edycję tego samego źródła:
  - [ ] Sprawdź, czy parametry SSH nadal są wypełnione.
  - [ ] Sprawdź, czy wybrany tryb SSL nie wrócił do wartości domyślnej.
  - [ ] Sprawdź, czy filtry schematów i opcje zostały zachowane.
- [ ] Sprawdź plik konfiguracyjny źródeł (`data-sources.toml`):
  - Ręcznie dodaj do sekcji danego źródła wpis `ssl.ca = "/etc/ssl/my-custom-ca.pem"`.
  - Otwórz dialog edycji w UI, zmień np. nazwę źródła i zapisz.
  - Sprawdź `data-sources.toml` — wpis `ssl.ca` **nie może zostać usunięty**.

---

## 5. Scenariusz: Język polski (Lokalizacja i18n)

- [ ] Przejdź do **Settings ▸ General ▸ Language** (lub zmień w `settings.toml` na `locale = "pl"`).
- [ ] Sprawdź, czy interfejs natychmiast przełącza się na język polski.
- [ ] Przejrzyj nowe widoki Fazy 10:
  - [ ] Zakładka **Aktualizacje i prywatność**:
    - „Kanał aktualizacji: Stabilny / Beta”,
    - „Sprawdzaj aktualizacje automatycznie”,
    - „Sprawdź teraz”,
    - „Zgoda na raporty awarii: Pytaj za każdym razem / Zawsze wysyłaj / Nigdy nie wysyłaj”,
    - Informacja o braku telemetrii.
  - [ ] Menu **Pomoc**:
    - „Sprawdź aktualizacje…”, „Zgłoś problem…”, „Dołącz do programu beta…”, „O programie FerroDb”.
  - [ ] Okno dialogowe awarii:
    - Prawidłowe formy liczby mnogiej w języku polskim: *1 raport*, *2 raporty*, *5 raportów*.
  - [ ] Upewnij się, że napisy nie są poucinane i mieszczą się w etykietach i przyciskach.

---

## 6. Podsumowująca lista kontrolna (Sign-off)

| Obszar | Przypadek testowy | Status | Uwagi |
|---|---|:---:|---|
| **Help Menu** | Check for Updates… nie powoduje paniki | [ ] | |
| **Help Menu** | Report a Problem… kopiuje diagnostykę i otwiera stronę | [ ] | |
| **Help Menu** | Join Beta Program… działa z menu i palety | [ ] | |
| **Help Menu** | About FerroDb wyświetla 0.1.0, API v8, kanał dev | [ ] | |
| **Settings** | Zmiana kanału Stable / Beta | [ ] | |
| **Settings** | Check automatically toggle | [ ] | |
| **Settings** | Przycisk Check Now z czytelnym wynikiem | [ ] | |
| **Settings** | Zgoda na raporty Ask / Always / Never | [ ] | |
| **Settings** | Przyciski View Reports / Open Reports Folder / Clear | [ ] | |
| **Persistence** | Ustawienia przetrwały restart (`release.toml`) | [ ] | |
| **Crash Dialog** | Okno awarii pojawia się po restarcie przy obecnym raporcie | [ ] | |
| **Crash Dialog** | Poufne dane (hasła URI, tokeny) są zamaskowane gwiazdkami | [ ] | |
| **Crash Dialog** | Opcje Send / Don't Send / Always Send | [ ] | |
| **Grid Edit** | Edycja komórki w siatce wyników (Postgres/MySQL/SQLite) | [ ] | |
| **Data Source**| Zachowanie konfiguracji SSH, SSL, filtrów po ponownym zapisie | [ ] | |
| **Data Source**| Zachowanie ręcznego klucza `ssl.ca` w `data-sources.toml` | [ ] | |
| **i18n** | Polskie tłumaczenia bez ucięć i z poprawną odmianą liczb | [ ] | |

**Podpis testera:** _______________________  
**Data:** _______________________  
**Wersja:** `FerroDb 0.1.0 (API v8)`
