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
local-history-empty = Migawki pojawiają się tu przy każdym uruchomieniu konsoli
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
table-editor-mock-applied = DDL zapisano w panelu Wyjście (wykonanie pojawi się w fazie 4)
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
wizard-export-done = Przygotowano eksport do { $format }: { $path } (transfer danych pojawi się w fazie 6)
wizard-import-done = Przygotowano import { $path } do { $target } (transfer danych pojawi się w fazie 6)
gallery-grid = Siatka wyników
gallery-documents = Przeglądarka dokumentów
gallery-keys = Przeglądarka kluczy
