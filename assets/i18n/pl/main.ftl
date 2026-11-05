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
