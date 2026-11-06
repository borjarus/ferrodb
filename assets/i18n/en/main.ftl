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
menu-tools = Tools
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

## ---- Phase 2 ----

## Common
loading = Loading…
button-ok = OK
button-cancel = Cancel
button-apply = Apply
category-results = Results
category-tools = Tools

## Phase 2 actions
action-edit-data-source = Data Source Properties…
action-driver-manager = Driver Manager…
action-refresh = Refresh
action-execute-script = Execute Script
action-explain = Explain Plan
action-commit = Commit
action-rollback = Roll Back
action-copy-selection = Copy
action-find-in-results = Find in Results
action-submit-changes = Submit Changes
action-revert-changes = Revert Changes
action-load-more = Load More Rows
action-export-data = Export Data…
action-import-data = Import Data…
action-query-history = Query History
action-local-history = Local History
action-new-table = New Table…
action-er-diagram = ER Diagram
action-compare = Compare Schemas
action-dev-gallery = Component Gallery

## Panels
panel-history = Query History
panel-local-history = Local History
panel-gallery = Gallery

## Explorer
explorer-filter = Filter objects…
explorer-select-source = Select a data source in the explorer
menu-open-data = Open Data
menu-copy-name = Copy Name
menu-copy-ddl = Copy DDL
menu-modify-table = Modify Table…
menu-remove-data-source = Remove Data Source…
remove-data-source-title = Remove data source?
remove-data-source-description = Data source “{ $id }” will be removed. Its secrets stay in the keyring.
ddl-copied = DDL copied to the clipboard
inspector-path = Path
inspector-detail = Details
inspector-copy = Copy value
group-database = databases
group-schema = schemas
group-keyspace = keyspaces
group-bucket = buckets
group-scope = scopes
group-project = projects
group-dataset = datasets
group-folder = folders
group-table = tables
group-view = views
group-materializedview = materialized views
group-column = columns
group-index = indexes
group-primarykey = primary keys
group-foreignkey = foreign keys
group-procedure = procedures
group-function = functions
group-sequence = sequences
group-trigger = triggers
group-collection = collections
group-field = fields
group-keyprefix = key prefixes
group-key = keys

## Data source dialog
ds-edit-title = Data Source “{ $name }”
ds-search-drivers = Search databases…
ds-templates = Templates
ds-category-relational = Relational
ds-category-analytical = Analytical and cloud
ds-category-nosql = NoSQL
ds-choose-driver = Choose a database on the left
ds-name = Name
ds-folder = Folder
ds-folder-placeholder = e.g. clients/acme
ds-user = User
ds-auth = Authentication
ds-secret-placeholder = stored in the system keyring
ds-environment = Environment
ds-read-only = Read-only
ds-manual-tx = Manual commit
ds-tab-general = General
ds-tab-options = Options
ds-tab-schemas = Schemas
ds-ssh-enable = Use SSH tunnel
ds-ssh-host = SSH host
ds-ssh-key = Private key file
ds-ssl-mode = SSL mode
ds-options = Driver options
ds-schema-filter = Show schemas
ds-schema-filter-hint = Comma-separated names or patterns; empty shows all.
ds-test = Test Connection
ds-testing = Testing connection…
ds-test-ok = Connected: { $version } ({ $ms } ms)
ds-required = Required: { $fields }
ds-saved = Data source saved: { $name }
ds-driver = Driver
ds-language = Language
ds-model = Data model
ds-access = Access layer
ds-transactions = Transactions
env-none = None
env-development = Development
env-test = Test
env-staging = Staging
env-production = Production
auth-none = None
auth-password = Password
auth-awsiam = AWS IAM
auth-entraid = Entra ID
auth-googleserviceaccount = Service account
auth-keypair = Key pair
auth-sso = SSO
auth-kerberos = Kerberos
auth-x509 = X.509
template-pg-docker = PostgreSQL (local Docker)
template-mongo-atlas = MongoDB Atlas
template-redis-local = Redis (localhost)
template-sqlite-file = SQLite file

## Drivers
drivers-builtin = Built in
drivers-not-downloaded = Not downloaded
drivers-downloading = Downloading… { $percent }%
drivers-downloaded = Downloaded, version { $version }
drivers-update = Version { $installed } installed, { $latest } available
drivers-custom = Custom files ({ $count })
drivers-download = Download
drivers-redownload = Download again
drivers-download-done = Driver “{ $id }” downloaded and verified
drivers-license = License: { $license }
drivers-custom-files = Use custom driver files…
drivers-choose-files = Choose driver files
bridge-title = JDBC bridge:
bridge-not-started = not started (starts with the first JDBC connection)
bridge-starting = starting…
bridge-running = running (PID { $pid }, connections: { $count })
bridge-restarting = restarting after a failure; sessions were disconnected

## Console
console-output-tab = Output
console-result-title = Result { $n }
console-statement-ms = Statement executed in { $ms } ms
console-affected-ms = { $count ->
        [one] 1 row affected
       *[other] { $count } rows affected
    } in { $ms } ms
cost-title = Query cost
cost-description = This query will process about { $size }. Run it?
cost-run = Run
cost-label = Cost: { $size }
cost-processed = Processed: { $size }
explain-tab = Plan
explain-unsupported = This data source has no execution plans
explain-summary = Total cost { $cost }, estimated rows { $rows }
tx-no-session = No open session in this console
tx-committed = Transaction committed
tx-rolled-back = Transaction rolled back

## Result grid
grid-search = Search loaded rows
grid-rows = { $count ->
        [one] 1 row
       *[other] { $count } rows
    }{ $more ->
        [yes] {" "}(more available)
       *[no] {""}
    }
grid-shown = shown: { $count }
grid-pending = { $count ->
        [one] 1 pending change
       *[other] { $count } pending changes
    }
grid-page-size = Page: { $count }
grid-load-more = Load more
grid-load-all = Load all
grid-add-row = Add row
grid-row-added = Empty row added; it is inserted when you submit changes
grid-record-view = Record view
grid-record-title = Row { $row }
grid-row-type = Row { $row } · { $type }
grid-read-only = The data source is read-only
grid-not-editable = Results of this data source cannot be edited
grid-copy-cell = Copy Value
grid-copy-row = Copy Row (TSV)
grid-copy-json = Copy Row as JSON
grid-copy-insert = Copy Row as INSERT
grid-edit-cell = Edit Value
grid-set-null = Set NULL
grid-delete-row = Delete Row
changes-preview-title = Review { $count ->
        [one] 1 change
       *[other] { $count } changes
    }
changes-preview-description = The following statements will be executed:
changes-applied = { $count ->
        [one] 1 change applied
       *[other] { $count } changes applied
    }
changes-applied-short = Changes applied

## Document browser
docs-mode-tree = Tree
docs-mode-table = Table
docs-fields = { $count ->
        [one] 1 field
       *[other] { $count } fields
    }
docs-more-items = { $count } more items
docs-count = { $count ->
        [one] 1 document
       *[other] { $count } documents
    }{ $more ->
        [yes] {" "}(more available)
       *[no] {""}
    }
docs-edit = Edit Value…
docs-edit-field = Edit “{ $path }”
docs-delete = Delete Document
docs-copy-json = Copy Document as JSON
docs-infer-schema = Infer Schema
docs-schema = Inferred schema
docs-sampled = Sampled { $count } documents
docs-indexes = Indexes

## Key browser
keys-pattern = Pattern, e.g. app:user:*
keys-count = { $count ->
        [one] 1 key
       *[other] { $count } keys
    }{ $more ->
        [yes] {" "}(more available)
       *[no] {""}
    }
keys-select = Select a key to see its value
keys-elements = { $count ->
        [one] 1 element
       *[other] { $count } elements
    }
keys-ttl-seconds = TTL in seconds (empty = none)
keys-set-ttl = Set TTL
keys-ttl-invalid = TTL must be a number of seconds
keys-delete = Delete Key
keys-save = Save
keys-field = Field
keys-value = Value
keys-index = Index
keys-member = Member
keys-score = Score
keys-fields = Fields
keys-edit-member = Edit “{ $member }”

## History, Local History, Services
history-filter = Filter history…
local-history-empty = Snapshots appear here each time a console runs
local-history-executed = executed
local-history-snapshot = Snapshot
local-history-current = Current console
local-history-compare = Compare with current console
local-history-restore = Restore into a new console
services-session = Session { $id }
services-in-transaction = transaction open

## Views (mockups)
table-editor-name = Table name
table-editor-column = Column
table-editor-type = Type
table-editor-default = Default
table-editor-add-column = Add column
table-editor-ddl = DDL preview
table-editor-no-changes = -- no changes
table-editor-modify = Modify Table “{ $name }”
table-editor-mock-applied = DDL written to the Output panel (execution arrives in phase 4)
er-summary = Schema { $schema }: { $tables } tables, { $relations } relations
diff-identical = identical
wizard-format = Format
wizard-options = Options
wizard-preview = Preview
wizard-source-file = Source file
wizard-mapping = Format and target
wizard-target = Target table
wizard-file = File
wizard-header-row = Header row
wizard-back = Back
wizard-next = Next
wizard-finish = Finish
wizard-export-done = Export to { $format } prepared: { $path } (data transfer arrives in phase 6)
wizard-import-done = Import of { $path } into { $target } prepared (data transfer arrives in phase 6)
gallery-grid = Result grid
gallery-documents = Document browser
gallery-keys = Key browser
