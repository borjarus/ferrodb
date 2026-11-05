### FerroDb — English (reference locale). Keep message ids in sync with pl/main.ftl.

app-name = FerroDb
welcome-subtitle = Database IDE for SQL and NoSQL
keys-double-shift = Shift Shift

## Menus
menu-app = FerroDb
menu-file = File
menu-edit = Edit
menu-view = View
menu-console = Console
menu-developer = Developer
menu-help = Help
menu-connect = Connect
menu-disconnect = Disconnect
menu-services = Services

## Action categories
category-application = Application
category-navigation = Navigation
category-layout = Layout
category-data-sources = Data sources
category-console = Console
category-developer = Developer

## Actions
action-find-action = Find Action…
action-search-everywhere = Search Everywhere
action-open-settings = Settings…
action-toggle-theme = Toggle Light/Dark Theme
action-open-project = Open Project…
action-recent-projects = Recent Projects…
action-about = About FerroDb
action-quit = Quit
action-toggle-explorer = Toggle Database Explorer
action-toggle-bottom-panel = Toggle Bottom Panel
action-toggle-right-panel = Toggle Right Panel
action-reset-layout = Reset Layout
action-new-data-source = New Data Source…
action-new-console = New Console
action-execute = Execute
action-stop = Stop Query
action-format = Reformat Code
action-rename = Rename…
action-goto-declaration = Go to Declaration
action-live-templates = Insert Live Template…
action-quick-doc = Quick Documentation
action-dev-scenarios = Mock Scenarios…
action-dev-notification = Show Test Notification

## Panels
panel-welcome = Welcome
panel-explorer = Database Explorer
panel-console = Console
panel-services = Services
panel-output = Output
panel-inspector = Inspector
inspector-empty = Nothing selected

## Connection states
state-disconnected = Disconnected
state-connecting = Connecting…
state-connected = Connected
state-failed = Failed

## Console
console-no-data-source = No data source
console-focus-required = Focus a console to run a query
console-result-summary = Result: { $count } items{ $more ->
        [yes] {" "}(more available)
       *[no] {""}
    }
console-affected = { $count ->
        [one] 1 row affected
       *[other] { $count } rows affected
    }
console-finished = Finished in { $ms } ms
console-cancelled = Cancellation requested
rows-count = { $count ->
        [one] 1 row
       *[other] { $count } rows
    }
not-implemented-phase = Coming in phase { $phase }

## Palettes
palette-actions-placeholder = Type an action name…
palette-search-placeholder = Search tables, collections, keys, data sources and actions…
palette-no-results = Nothing found
search-kind-data-source = Data source

## Object kinds
object-database = Database
object-schema = Schema
object-keyspace = Keyspace
object-bucket = Bucket
object-scope = Scope
object-project = Project
object-dataset = Dataset
object-folder = Folder
object-table = Table
object-view = View
object-materializedview = Materialized view
object-column = Column
object-index = Index
object-primarykey = Primary key
object-foreignkey = Foreign key
object-procedure = Procedure
object-function = Function
object-sequence = Sequence
object-trigger = Trigger
object-collection = Collection
object-field = Field
object-keyprefix = Key prefix
object-key = Key

## Projects and layout
project-opened = Project opened: { $name }
open-project-prompt = Open project folder
recent-projects-empty = No recent projects
reset-layout-title = Reset layout?
reset-layout-description = Panels will return to their default positions.
reset-layout-done = Layout reset

## About / status bar
about-version = Version { $version }
about-backend = Backend: { $backend }
status-backend = Backend: { $backend }
status-connections = Connected: { $connected }/{ $total }

## Settings
settings-general = General
settings-appearance = Appearance
settings-theme = Theme
appearance-system = System
appearance-light = Light
appearance-dark = Dark
settings-ui-font-size = Interface font size
settings-language = Language
settings-language-description = Some texts change after restart (native menus are rebuilt immediately).
language-system = System default
settings-editor = Editor
settings-editor-font-size = Editor font size
settings-editor-font-family = Editor font family
settings-line-numbers = Show line numbers
settings-developer = Developer
settings-developer-mode = Developer mode
settings-developer-mode-description = Shows the Developer menu with mock scenarios.

## Developer
dev-no-scenarios = This backend has no scenarios
dev-scenario-set = Scenario: { $name }
dev-notification-demo = Notifications work
scenario-normal = Normal
scenario-million-rows = 1 million rows
scenario-100k-documents = 100k documents
scenario-million-keys = 1M keys
scenario-5mb-document = 5 MB document
scenario-10k-tables = 10k tables
scenario-slow-network = Slow network
scenario-connection-error = Connection error
scenario-driver-not-downloaded = Driver not downloaded
scenario-jdbc-process-failure = JDBC process failure
