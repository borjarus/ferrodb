### FerroDb — polski. Identyfikatory komunikatów muszą odpowiadać en/main.ftl.

app-name = FerroDb
welcome-subtitle = IDE baz danych SQL i NoSQL
keys-double-shift = Shift Shift

## Menu
menu-app = FerroDb
menu-file = Plik
menu-edit = Edycja
menu-view = Widok
menu-console = Konsola
menu-developer = Deweloper
menu-help = Pomoc
menu-tools = Narzędzia
menu-connect = Połącz
menu-disconnect = Rozłącz
menu-services = Usługi

## Kategorie akcji
category-application = Aplikacja
category-navigation = Nawigacja
category-layout = Układ
category-data-sources = Źródła danych
category-console = Konsola
category-developer = Deweloper

## Akcje
action-find-action = Znajdź akcję…
action-search-everywhere = Szukaj wszędzie
action-open-settings = Ustawienia…
action-toggle-theme = Przełącz motyw jasny/ciemny
action-open-project = Otwórz projekt…
action-recent-projects = Ostatnie projekty…
action-about = O programie FerroDb
action-quit = Zakończ
action-toggle-explorer = Pokaż/ukryj eksplorator baz
action-toggle-bottom-panel = Pokaż/ukryj dolny panel
action-toggle-right-panel = Pokaż/ukryj prawy panel
action-reset-layout = Przywróć domyślny układ
action-new-data-source = Nowe źródło danych…
action-new-console = Nowa konsola
action-execute = Wykonaj
action-stop = Zatrzymaj zapytanie
action-format = Sformatuj kod
action-rename = Zmień nazwę…
action-goto-declaration = Przejdź do deklaracji
action-live-templates = Wstaw szablon…
action-quick-doc = Szybka dokumentacja
action-dev-scenarios = Scenariusze makiety…
action-dev-notification = Pokaż testowe powiadomienie

## Panele
panel-welcome = Start
panel-explorer = Eksplorator baz
panel-console = Konsola
panel-services = Usługi
panel-output = Wyjście
panel-inspector = Inspektor
inspector-empty = Nic nie zaznaczono

## Stany połączeń
state-disconnected = Rozłączono
state-connecting = Łączenie…
state-connected = Połączono
state-failed = Błąd

## Konsola
console-no-data-source = Brak źródła danych
console-focus-required = Ustaw kursor w konsoli, aby wykonać zapytanie
console-result-summary = Wynik: { $count } elementów{ $more ->
        [yes] {" "}(są kolejne)
       *[no] {""}
    }
console-affected = { $count ->
        [one] Zmieniono 1 wiersz
        [few] Zmieniono { $count } wiersze
       *[other] Zmieniono { $count } wierszy
    }
console-finished = Zakończono w { $ms } ms
console-cancelled = Zażądano anulowania
rows-count = { $count ->
        [one] 1 wiersz
        [few] { $count } wiersze
       *[other] { $count } wierszy
    }
not-implemented-phase = Dostępne w fazie { $phase }

## Palety
palette-actions-placeholder = Wpisz nazwę akcji…
palette-search-placeholder = Szukaj tabel, kolekcji, kluczy, źródeł danych i akcji…
palette-no-results = Nic nie znaleziono
search-kind-data-source = Źródło danych

## Rodzaje obiektów
object-database = Baza danych
object-schema = Schemat
object-keyspace = Przestrzeń kluczy
object-bucket = Kubełek
object-scope = Zakres
object-project = Projekt
object-dataset = Zbiór danych
object-folder = Folder
object-table = Tabela
object-view = Widok
object-materializedview = Widok zmaterializowany
object-column = Kolumna
object-index = Indeks
object-primarykey = Klucz główny
object-foreignkey = Klucz obcy
object-procedure = Procedura
object-function = Funkcja
object-sequence = Sekwencja
object-trigger = Wyzwalacz
object-collection = Kolekcja
object-field = Pole
object-keyprefix = Prefiks kluczy
object-key = Klucz

## Projekty i układ
project-opened = Otwarto projekt: { $name }
open-project-prompt = Otwórz folder projektu
recent-projects-empty = Brak ostatnich projektów
reset-layout-title = Przywrócić układ?
reset-layout-description = Panele wrócą na domyślne miejsca.
reset-layout-done = Przywrócono układ

## O programie / pasek stanu
about-version = Wersja { $version }
about-backend = Backend: { $backend }
status-backend = Backend: { $backend }
status-connections = Połączone: { $connected }/{ $total }

## Ustawienia
settings-general = Ogólne
settings-appearance = Wygląd
settings-theme = Motyw
appearance-system = Systemowy
appearance-light = Jasny
appearance-dark = Ciemny
settings-ui-font-size = Rozmiar czcionki interfejsu
settings-language = Język
settings-language-description = Część tekstów zmieni się po ponownym uruchomieniu (menu systemowe są przebudowywane od razu).
language-system = Domyślny systemowy
settings-editor = Edytor
settings-editor-font-size = Rozmiar czcionki edytora
settings-editor-font-family = Czcionka edytora
settings-line-numbers = Pokazuj numery wierszy
settings-developer = Deweloper
settings-developer-mode = Tryb dewelopera
settings-developer-mode-description = Pokazuje menu Deweloper ze scenariuszami makiety.

## Deweloper
dev-no-scenarios = Ten backend nie ma scenariuszy
dev-scenario-set = Scenariusz: { $name }
dev-notification-demo = Powiadomienia działają
scenario-normal = Normalny
scenario-million-rows = Milion wierszy
scenario-100k-documents = 100 tys. dokumentów
scenario-million-keys = Milion kluczy
scenario-5mb-document = Dokument 5 MB
scenario-10k-tables = 10 tys. tabel
scenario-slow-network = Wolna sieć
scenario-connection-error = Błąd połączenia
scenario-driver-not-downloaded = Brak pobranego sterownika
scenario-jdbc-process-failure = Awaria procesu JDBC

## ---- Faza 2 ----

## Wspólne
loading = Wczytywanie…
button-ok = OK
button-cancel = Anuluj
button-apply = Zastosuj
category-results = Wyniki
category-tools = Narzędzia

## Akcje fazy 2
action-edit-data-source = Właściwości źródła danych…
action-driver-manager = Menedżer sterowników…
action-refresh = Odśwież
action-execute-script = Wykonaj skrypt
action-explain = Plan wykonania
action-commit = Zatwierdź
action-rollback = Wycofaj
action-copy-selection = Kopiuj
action-find-in-results = Szukaj w wynikach
action-submit-changes = Zapisz zmiany
action-revert-changes = Cofnij zmiany
action-load-more = Wczytaj więcej wierszy
action-export-data = Eksportuj dane…
action-import-data = Importuj dane…
action-query-history = Historia zapytań
action-local-history = Historia lokalna
action-new-table = Nowa tabela…
action-er-diagram = Diagram ER
action-compare = Porównaj schematy
action-dev-gallery = Galeria komponentów

## Panele
panel-history = Historia zapytań
panel-local-history = Historia lokalna
panel-gallery = Galeria

## Eksplorator
explorer-filter = Filtruj obiekty…
explorer-select-source = Zaznacz źródło danych w eksploratorze
menu-open-data = Otwórz dane
menu-copy-name = Kopiuj nazwę
menu-copy-ddl = Kopiuj DDL
menu-copy-script = Kopiuj skrypt
menu-generate-select = Generuj SELECT
menu-generate-find = Generuj find()
menu-generate-insert = Generuj INSERT
menu-generate-update = Generuj UPDATE
menu-generate-delete = Generuj DELETE
menu-generate-call = Generuj wywołanie
menu-generate-drop = Generuj DROP
goto-not-found = Brak obiektu „{ $name }” w metadanych w pamięci podręcznej
menu-modify-table = Modyfikuj tabelę…
menu-remove-data-source = Usuń źródło danych…
remove-data-source-title = Usunąć źródło danych?
remove-data-source-description = Źródło danych „{ $id }” zostanie usunięte. Jego sekrety pozostaną w magazynie kluczy.
ddl-copied = DDL skopiowano do schowka
inspector-path = Ścieżka
inspector-detail = Szczegóły
inspector-copy = Kopiuj wartość
group-database = bazy danych
group-schema = schematy
group-keyspace = przestrzenie kluczy
group-bucket = kubełki
group-scope = zakresy
group-project = projekty
group-dataset = zbiory danych
group-folder = foldery
group-table = tabele
group-view = widoki
group-materializedview = widoki zmaterializowane
group-column = kolumny
group-index = indeksy
group-primarykey = klucze główne
group-foreignkey = klucze obce
group-procedure = procedury
group-function = funkcje
group-sequence = sekwencje
group-trigger = wyzwalacze
group-collection = kolekcje
group-field = pola
group-keyprefix = prefiksy kluczy
group-key = klucze

## Okno źródła danych
ds-edit-title = Źródło danych „{ $name }”
ds-search-drivers = Szukaj bazy danych…
ds-templates = Szablony
ds-category-relational = Relacyjne
ds-category-analytical = Analityczne i chmurowe
ds-category-nosql = NoSQL
ds-choose-driver = Wybierz bazę danych po lewej
ds-name = Nazwa
ds-folder = Folder
ds-folder-placeholder = np. klienci/acme
ds-user = Użytkownik
ds-auth = Uwierzytelnianie
ds-secret-placeholder = zapisywane w systemowym magazynie kluczy
ds-environment = Środowisko
ds-read-only = Tylko do odczytu
ds-manual-tx = Ręczne zatwierdzanie
ds-tab-general = Ogólne
ds-tab-options = Opcje
ds-tab-schemas = Schematy
ds-ssh-enable = Użyj tunelu SSH
ds-ssh-host = Host SSH
ds-ssh-key = Plik klucza prywatnego
ds-ssl-mode = Tryb SSL
ds-options = Opcje sterownika
ds-schema-filter = Pokazuj schematy
ds-schema-filter-hint = Nazwy lub wzorce oddzielone przecinkami; puste pokazuje wszystkie.
ds-test = Testuj połączenie
ds-testing = Testowanie połączenia…
ds-test-ok = Połączono: { $version } ({ $ms } ms)
ds-required = Wymagane: { $fields }
ds-saved = Zapisano źródło danych: { $name }
ds-driver = Sterownik
ds-language = Język
ds-model = Model danych
ds-access = Warstwa dostępu
ds-transactions = Transakcje
env-none = Brak
env-development = Deweloperskie
env-test = Testowe
env-staging = Przedprodukcyjne
env-production = Produkcyjne
auth-none = Brak
auth-password = Hasło
auth-awsiam = AWS IAM
auth-entraid = Entra ID
auth-googleserviceaccount = Konto usługi
auth-keypair = Para kluczy
auth-sso = SSO
auth-kerberos = Kerberos
auth-x509 = X.509
template-pg-docker = PostgreSQL (lokalny Docker)
template-mongo-atlas = MongoDB Atlas
template-redis-local = Redis (localhost)
template-sqlite-file = Plik SQLite

## Sterowniki
drivers-builtin = Wbudowany
drivers-not-downloaded = Niepobrany
drivers-downloading = Pobieranie… { $percent }%
drivers-downloaded = Pobrany, wersja { $version }
drivers-update = Zainstalowana wersja { $installed }, dostępna { $latest }
drivers-custom = Własne pliki ({ $count })
drivers-download = Pobierz
drivers-redownload = Pobierz ponownie
drivers-download-done = Sterownik „{ $id }” pobrany i zweryfikowany
drivers-license = Licencja: { $license }
drivers-custom-files = Użyj własnych plików sterownika…
drivers-choose-files = Wybierz pliki sterownika
bridge-title = Mostek JDBC:
bridge-not-started = nieuruchomiony (startuje przy pierwszym połączeniu JDBC)
bridge-starting = uruchamianie…
bridge-running = działa (PID { $pid }, połączenia: { $count })
bridge-restarting = restart po awarii; sesje zostały rozłączone

## Konsola
console-output-tab = Wyjście
console-result-title = Wynik { $n }
console-statement-ms = Instrukcję wykonano w { $ms } ms
console-affected-ms = { $count ->
        [one] Zmieniono 1 wiersz
        [few] Zmieniono { $count } wiersze
       *[other] Zmieniono { $count } wierszy
    } w { $ms } ms
cost-title = Koszt zapytania
cost-description = To zapytanie przetworzy około { $size }. Uruchomić?
cost-run = Uruchom
cost-label = Koszt: { $size }
cost-processed = Przetworzono: { $size }
explain-tab = Plan
explain-unsupported = To źródło danych nie udostępnia planów wykonania
explain-summary = Łączny koszt { $cost }, szacowana liczba wierszy { $rows }
tx-no-session = Ta konsola nie ma otwartej sesji
tx-committed = Transakcję zatwierdzono
tx-rolled-back = Transakcję wycofano

## Siatka wyników
grid-search = Szukaj we wczytanych wierszach
grid-rows = { $count ->
        [one] 1 wiersz
        [few] { $count } wiersze
       *[other] { $count } wierszy
    }{ $more ->
        [yes] {" "}(dostępne kolejne)
       *[no] {""}
    }
grid-shown = widoczne: { $count }
grid-pending = { $count ->
        [one] 1 oczekująca zmiana
        [few] { $count } oczekujące zmiany
       *[other] { $count } oczekujących zmian
    }
grid-page-size = Strona: { $count }
grid-load-more = Wczytaj więcej
grid-load-all = Wczytaj wszystko
grid-add-row = Dodaj wiersz
grid-row-added = Dodano pusty wiersz; zostanie wstawiony przy zapisie zmian
grid-record-view = Widok rekordu
grid-record-title = Wiersz { $row }
grid-row-type = Wiersz { $row } · { $type }
grid-read-only = Źródło danych jest tylko do odczytu
grid-not-editable = Wyników tego źródła danych nie można edytować
grid-copy-cell = Kopiuj wartość
grid-copy-row = Kopiuj wiersz (TSV)
grid-copy-json = Kopiuj wiersz jako JSON
grid-copy-insert = Kopiuj wiersz jako INSERT
grid-edit-cell = Edytuj wartość
grid-set-null = Ustaw NULL
grid-delete-row = Usuń wiersz
grid-filter-value = Filtruj po tej wartości
grid-related = Powiązane: { $label }
changes-preview-title = Przegląd { $count ->
        [one] 1 zmiany
       *[other] { $count } zmian
    }
changes-preview-description = Zostaną wykonane następujące instrukcje:
changes-applied = { $count ->
        [one] Zastosowano 1 zmianę
        [few] Zastosowano { $count } zmiany
       *[other] Zastosowano { $count } zmian
    }
changes-applied-short = Zmiany zastosowano
changes-applied-pending = { $count ->
        [one] Zastosowano 1 zmianę w otwartej transakcji; zatwierdź, aby ją zachować
        [few] Zastosowano { $count } zmiany w otwartej transakcji; zatwierdź, aby je zachować
       *[other] Zastosowano { $count } zmian w otwartej transakcji; zatwierdź, aby je zachować
    }

## Przeglądarka dokumentów
docs-mode-tree = Drzewo
docs-mode-table = Tabela
docs-fields = { $count ->
        [one] 1 pole
        [few] { $count } pola
       *[other] { $count } pól
    }
docs-more-items = i { $count } kolejnych elementów
docs-count = { $count ->
        [one] 1 dokument
        [few] { $count } dokumenty
       *[other] { $count } dokumentów
    }{ $more ->
        [yes] {" "}(dostępne kolejne)
       *[no] {""}
    }
docs-edit = Edytuj wartość…
docs-edit-field = Edycja „{ $path }”
docs-delete = Usuń dokument
docs-copy-json = Kopiuj dokument jako JSON
docs-infer-schema = Wywnioskuj schemat
docs-schema = Wywnioskowany schemat
docs-sampled = Próbka: { $count } dokumentów
docs-indexes = Indeksy

## Przeglądarka kluczy
keys-pattern = Wzorzec, np. app:user:*
keys-count = { $count ->
        [one] 1 klucz
        [few] { $count } klucze
       *[other] { $count } kluczy
    }{ $more ->
        [yes] {" "}(dostępne kolejne)
       *[no] {""}
    }
keys-select = Zaznacz klucz, aby zobaczyć wartość
keys-elements = { $count ->
        [one] 1 element
        [few] { $count } elementy
       *[other] { $count } elementów
    }
keys-ttl-seconds = TTL w sekundach (puste = brak)
keys-set-ttl = Ustaw TTL
keys-ttl-invalid = TTL musi być liczbą sekund
keys-delete = Usuń klucz
keys-save = Zapisz
keys-field = Pole
keys-value = Wartość
keys-index = Indeks
keys-member = Element
keys-score = Wynik
keys-fields = Pola
keys-edit-member = Edycja „{ $member }”

## Historia, historia lokalna, usługi
history-filter = Filtruj historię…
local-history-empty = Rewizje pojawiają się tu po uruchomieniu, zapisaniu lub edycji konsoli
local-history-executed = wykonanie
local-history-snapshot = Migawka
local-history-current = Bieżąca konsola
local-history-compare = Porównaj z bieżącą konsolą
local-history-restore = Przywróć w nowej konsoli
services-session = Sesja { $id }
services-in-transaction = otwarta transakcja

## Widoki (makiety)
table-editor-name = Nazwa tabeli
table-editor-column = Kolumna
table-editor-type = Typ
table-editor-default = Domyślna
table-editor-add-column = Dodaj kolumnę
table-editor-ddl = Podgląd DDL
table-editor-no-changes = -- brak zmian
table-editor-modify = Modyfikacja tabeli „{ $name }”
er-summary = Schemat { $schema }: tabele { $tables }, relacje { $relations }
diff-identical = identyczne
wizard-format = Format
wizard-options = Opcje
wizard-preview = Podgląd
wizard-source-file = Plik źródłowy
wizard-mapping = Format i cel
wizard-target = Tabela docelowa
wizard-file = Plik
wizard-header-row = Wiersz nagłówka
wizard-back = Wstecz
wizard-next = Dalej
wizard-finish = Zakończ
wizard-export-done = Wyeksportowano { $items } elementów do { $path } ({ $method }, { $elapsed })
wizard-import-done = Zaimportowano { $items } elementów do { $target } ({ $method }, { $elapsed })
wizard-failed = Transfer nie powiódł się: { $error }
wizard-running = Trwa transfer…
wizard-source = Źródło
wizard-no-source = Najpierw uruchom zapytanie albo zaznacz tabelę lub kolekcję
wizard-no-target = Najpierw zaznacz w eksploratorze źródło danych, schemat albo tabelę
wizard-create-table = Utwórz tabelę, jeśli nie istnieje
wizard-truncate = Najpierw usuń istniejące wiersze
wizard-mapping-auto = Kolumny są dopasowane po nazwie:
wizard-mapping-new = (nowa kolumna)
wizard-mapping-skipped = (pominięta)
gallery-grid = Siatka wyników
gallery-documents = Przeglądarka dokumentów
gallery-keys = Przeglądarka kluczy

## Faza 3: potwierdzanie niebezpiecznych instrukcji i parametry skryptu
risk-title = Potwierdź wykonanie
risk-description = Skrypt zawiera instrukcje, które mogą zmienić lub usunąć dane:
risk-production = To źródło danych jest oznaczone jako produkcyjne.
risk-run = Wykonaj
risk-delete-all = DELETE bez WHERE
risk-update-all = UPDATE bez WHERE
risk-drop = DROP
risk-truncate = TRUNCATE
risk-modification = Zmienia dane lub schemat
params-title = Wartości parametrów
params-description = Wartości są wstawiane jako literały SQL. Puste pole oznacza NULL.
params-null = NULL
params-run = Wykonaj

## Faza 7 — narzędzia zaawansowane

action-explain-analyze = Explain Analyze
action-save-file = Zapisz plik
action-project-files = Pliki projektu
console-saved = Zapisano w { $file }

explain-tab-analyzed = Plan (z wykonaniem)
explain-planning = planowanie { $time }
explain-execution = wykonanie { $time }
explain-analyzed = rzeczywiste wiersze i czasy
explain-mode-tree = Drzewo
explain-mode-table = Tabela
explain-mode-diagram = Diagram
explain-copy-raw = Kopiuj surowy plan
explain-col-operation = Operacja
explain-col-object = Obiekt
explain-col-cost = Koszt
explain-col-self = Własny
explain-col-rows = Szac. wiersze
explain-col-actual-rows = Rzecz. wiersze
explain-col-time = Czas
explain-col-loops = Pętle

er-loading = Wczytywanie schematu…
er-more-columns = … i { $count } więcej
er-reset-layout = Przywróć układ

compare-source = Źródło
compare-target = Cel
compare-choose = Wybierz schemat…
compare-run = Porównaj
compare-running = Porównywanie…
compare-pick-both = Wybierz schemat źródłowy i docelowy
compare-identical = Schematy są identyczne
compare-summary = { $count ->
    [one] 1 różnica
    [few] { $count } różnice
   *[other] { $count } różnic
}
compare-drop-removed = Usuń obiekty, których nie ma w źródle
compare-ignore-comments = Pomijaj komentarze
compare-routines = Procedury i wyzwalacze
compare-script = Skrypt migracji (cel staje się taki jak źródło)
compare-no-script = -- nic do zmiany
compare-open-console = Otwórz w konsoli
compare-apply = Zastosuj w celu
compare-applied = { $count ->
    [one] Wykonano 1 instrukcję
    [few] Wykonano { $count } instrukcje
   *[other] Wykonano { $count } instrukcji
}

table-editor-columns = Kolumny
table-editor-indexes = Indeksy ({ $count })
table-editor-foreign-keys = Klucze obce ({ $count })
table-editor-auto = Auto
table-editor-unique = Unikalny
table-editor-index-name = Indeks
table-editor-index-columns = Kolumny (po przecinku)
table-editor-add-index = Dodaj indeks
table-editor-fk-columns = Kolumny
table-editor-fk-table = Tabela docelowa
table-editor-fk-ref-columns = Kolumny docelowe
table-editor-fk-on-delete = Przy usunięciu
table-editor-add-fk = Dodaj klucz obcy
table-editor-no-schema = Najpierw zaznacz schemat lub tabelę w eksploratorze
table-editor-applied = Zmiany tabeli zastosowane

panel-files = Pliki
files-folder = Pliki
files-refresh = Odśwież
files-open = Otwórz w konsoli
files-open-as-source = Otwórz jako źródło danych
files-opened-as-source = Źródło danych „{ $name }” gotowe

local-history-saved = zapis
local-history-autosaved = autozapis
local-history-before-restore = przed przywróceniem
local-history-external = zmiana na dysku
local-history-labelled = etykieta
local-history-label-placeholder = Etykieta…
local-history-put-label = Dodaj etykietę
local-history-delete = Usuń rewizję
local-history-clear = Wyczyść historię dokumentu
local-history-restored = Przywrócono rewizję
local-history-not-open = Najpierw otwórz dokument w konsoli

## Faza 9: asystent AI, agenci i MCP, skrypty rozszerzeń
category-ai = AI i agenci
action-ai-assistant = Asystent AI
action-ai-ask = Zapytaj AI…
action-ai-explain = AI: wyjaśnij zapytanie
action-ai-optimize = AI: zoptymalizuj zapytanie
action-ai-fix = Napraw z AI
action-ai-settings = Ustawienia asystenta AI…
action-agents-settings = Ustawienia agentów i MCP…
action-scripts-folder = Otwórz folder skryptów
action-new-extractor = Nowy skrypt eksportu…
action-new-aggregator = Nowy skrypt agregatu…
action-reload-scripts = Wczytaj skrypty ponownie

edit-cut = Wytnij
edit-copy = Kopiuj
edit-paste = Wklej
edit-select-all = Zaznacz wszystko

panel-ai-assistant = Asystent AI
ai-unavailable = Asystent AI nie jest dostępny w tej wersji.
ai-disabled = Asystent AI jest wyłączony. Wybierz dostawcę i włącz go w ustawieniach.
ai-hint = Zapytaj o źródło danych albo opisz potrzebne zapytanie. Zapytania nigdy nie są uruchamiane za Ciebie: wstaw je do konsoli i uruchom tam.
ai-prompt-placeholder = Zapytaj AI… (Enter wysyła, Shift+Enter dodaje wiersz)
ai-send = Wyślij
ai-stop = Zatrzymaj
ai-new-chat = Nowa rozmowa
ai-no-source = Bez źródła danych
ai-follow-console = Jak w aktywnej konsoli
ai-thinking = Myślę…
ai-stopped = Zatrzymano
ai-context-sent = Wysłano schemat { $objects ->
        [one] 1 obiektu
       *[other] { $objects } obiektów
    }, { $chars } znaków
ai-modifies = Zmienia dane
ai-problem = Problem: { $message }
ai-insert = Wstaw do konsoli
ai-open-console = Otwórz w nowej konsoli
ai-copy = Kopiuj
ai-copied = Skopiowano zapytanie
ai-task-explain = Wyjaśnij
ai-task-optimize = Zoptymalizuj
ai-task-fix = Napraw
ai-prompt-explain = Wyjaśnij to zapytanie.
ai-prompt-optimize = Przyspiesz to zapytanie.
ai-prompt-fix = Napraw to zapytanie.
ai-no-error = W tej konsoli nie ma nieudanej instrukcji
ai-no-statement = Ustaw kursor na instrukcji albo ją zaznacz

settings-ai = Asystent AI
settings-agents = Agenci i MCP
settings-save = Zapisz
settings-saved = Zapisano ustawienia
ai-enabled = Włącz asystenta AI
ai-enabled-description = Pytania, nazwy ze schematu (jeśli udostępniane) i zaznaczone zapytania są wysyłane do poniższego dostawcy.
ai-provider-section = Dostawca
ai-provider = Dostawca
ai-endpoint = Adres
ai-model = Model
ai-load-models = Wczytaj modele
ai-models-choose = Wybierz model
ai-models-none = Dostawca nie podał żadnych modeli
ai-api-key = Klucz API
ai-key-placeholder = Zostaw puste, aby zachować zapisany klucz
ai-key-stored = zapisany
ai-key-missing = brak
ai-key-optional = niepotrzebny
ai-key-cleared = zostanie usunięty przy zapisie
ai-clear-key = Usuń
ai-privacy-section = Co jest udostępniane
ai-share-schema = Udostępniaj schemat (nazwy i typy)
ai-share-samples = Udostępniaj przykładowe wartości
ai-max-tokens = Maks. tokenów odpowiedzi
ai-schema-budget = Limit schematu (znaki)
ai-test = Testuj
ai-test-ok = Połączono: { $model }

agents-unavailable = Agenci i serwer MCP nie są dostępni w tej wersji.
mcp-section = Serwer MCP
mcp-enabled = Uruchamiaj serwer MCP
mcp-port = Port (127.0.0.1)
mcp-running = Działa
mcp-stopped = Nie działa
mcp-no-clients = Brak połączonych klientów
mcp-clients = Klienci: { $names }
mcp-copy-url = Kopiuj adres
mcp-copy-token = Kopiuj token
mcp-copy-config = Kopiuj konfigurację klienta
mcp-regenerate = Wygeneruj nowy token
mcp-not-running = Serwer MCP nie działa
mcp-copied = Skopiowano do schowka
mcp-token-regenerated = Wygenerowano nowy token; klienci muszą połączyć się ponownie
agent-policy-section = Uprawnienia
agent-default-access = Domyślny dostęp
agent-consent = Pytaj o zgodę
agent-max-rows = Maks. wierszy zapytania
agent-timeout = Czas na zgodę (sekundy)
agent-sources = Dostęp do źródeł danych
agent-access-hidden = Ukryte
agent-access-metadata = Tylko metadane
agent-access-read-only = Tylko odczyt
agent-access-read-write = Odczyt i zapis
agent-access-default = Domyślnie ({ $access })
agent-consent-every-call = Przy każdym wywołaniu
agent-consent-queries = Tylko przy zapytaniach
agent-consent-writes = Tylko przy zapisach
audit-section = Dziennik audytu
audit-refresh = Odśwież
audit-empty = Agenci jeszcze nic nie wywołali
audit-allowed-policy = dozwolone przez reguły
audit-allowed-user = dozwolone
audit-denied-policy = zabronione przez reguły
audit-denied-user = odrzucone
audit-timed-out = upłynął czas
status-mcp = MCP · { $clients ->
        [one] 1 klient
        [few] { $clients } klientów
       *[other] { $clients } klientów
    }

consent-title = Agent chce użyć źródła danych
consent-client = Klient
consent-tool = Narzędzie
consent-source = Źródło danych
consent-modifies = To wywołanie zmienia dane lub schemat.
consent-allow-once = Zezwól raz
consent-allow-session = Zezwól w tej sesji
consent-deny = Odmów
consent-allowed = Zezwolono na { $tool } dla { $client }
consent-denied = Odmówiono { $tool } dla { $client }
consent-closed = Prośba o zgodę została zamknięta (upłynął czas albo klient zrezygnował)

scripts-unavailable = Skrypty rozszerzeń nie są dostępne w tej wersji.
scripts-no-folder = W tej konfiguracji skrypty nie są przechowywane na dysku
scripts-name = Nazwa
scripts-name-description = Nazwa pliku nowego skryptu (bez .rhai).
scripts-create = Utwórz
scripts-created = Utworzono skrypt: { $path }
scripts-reloaded = Wczytano skrypty ({ $count })
grid-copy-as = Kopiuj jako
grid-copied-as = Skopiowano do schowka
grid-script-failed = Skrypt { $script } nie powiódł się: { $error }
grid-aggregator = Podsumowanie zaznaczonej kolumny
grid-aggregate-builtin = Podsumowanie
grid-aggregate-failed = Agregat nie powiódł się: { $error }
