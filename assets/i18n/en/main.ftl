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
menu-copy-script = Copy Script
menu-generate-select = Generate SELECT
menu-generate-find = Generate find()
menu-generate-insert = Generate INSERT
menu-generate-update = Generate UPDATE
menu-generate-delete = Generate DELETE
menu-generate-call = Generate Call
menu-generate-drop = Generate DROP
goto-not-found = No object named “{ $name }” in the cached metadata
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
grid-filter-value = Filter by This Value
grid-related = Related: { $label }
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
changes-applied-pending = { $count ->
        [one] 1 change applied in the open transaction; commit to keep it
       *[other] { $count } changes applied in the open transaction; commit to keep them
    }

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
local-history-empty = Revisions appear here when a console runs, is saved or edited
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
wizard-export-done = Exported { $items } items to { $path } ({ $method }, { $elapsed })
wizard-import-done = Imported { $items } items into { $target } ({ $method }, { $elapsed })
wizard-failed = Transfer failed: { $error }
wizard-running = Transferring…
wizard-source = Source
wizard-no-source = Run a query or select a table or collection first
wizard-no-target = Select a data source, schema or table in the explorer first
wizard-create-table = Create the table when it does not exist
wizard-truncate = Remove existing rows first
wizard-mapping-auto = Columns are matched by name:
wizard-mapping-new = (new column)
wizard-mapping-skipped = (not imported)
gallery-grid = Result grid
gallery-documents = Document browser
gallery-keys = Key browser

## Phase 3: confirmation of dangerous statements and script parameters
risk-title = Confirm execution
risk-description = The script contains statements that may change or destroy data:
risk-production = This data source is marked as Production.
risk-run = Execute
risk-delete-all = DELETE without WHERE
risk-update-all = UPDATE without WHERE
risk-drop = DROP
risk-truncate = TRUNCATE
risk-modification = Modifies data or schema
params-title = Parameter values
params-description = Values are inserted as SQL literals. Leave a field empty for NULL.
params-null = NULL
params-run = Execute

## Phase 7 — advanced tools

action-explain-analyze = Explain Analyze
action-save-file = Save File
action-project-files = Project Files
console-saved = Saved to { $file }

explain-tab-analyzed = Plan (analyzed)
explain-planning = planning { $time }
explain-execution = execution { $time }
explain-analyzed = actual rows and times
explain-mode-tree = Tree
explain-mode-table = Table
explain-mode-diagram = Diagram
explain-copy-raw = Copy Raw Plan
explain-col-operation = Operation
explain-col-object = Object
explain-col-cost = Cost
explain-col-self = Self
explain-col-rows = Est. rows
explain-col-actual-rows = Actual rows
explain-col-time = Time
explain-col-loops = Loops

er-loading = Loading the schema…
er-more-columns = … { $count } more
er-reset-layout = Reset Layout

compare-source = Source
compare-target = Target
compare-choose = Choose a schema…
compare-run = Compare
compare-running = Comparing…
compare-pick-both = Choose a source and a target schema
compare-identical = The schemas are identical
compare-summary = { $count ->
    [one] 1 difference
   *[other] { $count } differences
}
compare-drop-removed = Drop objects missing in the source
compare-ignore-comments = Ignore comments
compare-routines = Routines and triggers
compare-script = Migration script (makes the target like the source)
compare-no-script = -- nothing to change
compare-open-console = Open in Console
compare-apply = Apply to Target
compare-applied = { $count ->
    [one] 1 statement applied
   *[other] { $count } statements applied
}

table-editor-columns = Columns
table-editor-indexes = Indexes ({ $count })
table-editor-foreign-keys = Foreign Keys ({ $count })
table-editor-auto = Auto
table-editor-unique = Unique
table-editor-index-name = Index
table-editor-index-columns = Columns (comma-separated)
table-editor-add-index = Add index
table-editor-fk-columns = Columns
table-editor-fk-table = References table
table-editor-fk-ref-columns = Referenced columns
table-editor-fk-on-delete = On delete
table-editor-add-fk = Add foreign key
table-editor-no-schema = Select a schema or table in the explorer first
table-editor-applied = Table changes applied

panel-files = Files
files-folder = Files
files-refresh = Refresh
files-open = Open in Console
files-open-as-source = Open as Data Source
files-opened-as-source = Data source “{ $name }” ready

local-history-saved = saved
local-history-autosaved = autosaved
local-history-before-restore = before restore
local-history-external = changed on disk
local-history-labelled = label
local-history-label-placeholder = Label…
local-history-put-label = Put Label
local-history-delete = Delete revision
local-history-clear = Clear History of Document
local-history-restored = Revision restored
local-history-not-open = Open the document in a console first

## Phase 9: AI assistant, agents and MCP, extension scripts
category-ai = AI and agents
action-ai-assistant = AI Assistant
action-ai-ask = Ask AI…
action-ai-explain = AI: Explain Query
action-ai-optimize = AI: Optimize Query
action-ai-fix = Fix with AI
action-ai-settings = AI Assistant Settings…
action-agents-settings = Agents and MCP Settings…
action-scripts-folder = Open Scripts Folder
action-new-extractor = New Extractor Script…
action-new-aggregator = New Aggregator Script…
action-reload-scripts = Reload Scripts

edit-cut = Cut
edit-copy = Copy
edit-paste = Paste
edit-select-all = Select All

panel-ai-assistant = AI Assistant
ai-unavailable = The AI assistant is not available in this build.
ai-disabled = The AI assistant is turned off. Choose a provider and enable it in the settings.
ai-hint = Ask about the data source, or describe the query you need. Queries are never run for you: insert them into a console and run them there.
ai-prompt-placeholder = Ask AI… (Enter sends, Shift+Enter adds a line)
ai-send = Send
ai-stop = Stop
ai-new-chat = New Chat
ai-no-source = No data source
ai-follow-console = Follow the active console
ai-thinking = Thinking…
ai-stopped = Stopped
ai-context-sent = Schema of { $objects ->
        [one] 1 object
       *[other] { $objects } objects
    } sent, { $chars } chars
ai-modifies = Modifies data
ai-problem = Problem: { $message }
ai-insert = Insert into Console
ai-open-console = Open in New Console
ai-copy = Copy
ai-copied = Query copied
ai-task-explain = Explain
ai-task-optimize = Optimize
ai-task-fix = Fix
ai-prompt-explain = Explain this query.
ai-prompt-optimize = Make this query faster.
ai-prompt-fix = Fix this query.
ai-no-error = No failed statement in this console
ai-no-statement = Place the cursor on a statement or select one

settings-ai = AI Assistant
settings-agents = Agents and MCP
settings-save = Save
settings-saved = Settings saved
ai-enabled = Enable AI assistant
ai-enabled-description = Questions, schema names (if shared) and selected queries are sent to the provider below.
ai-provider-section = Provider
ai-provider = Provider
ai-endpoint = Endpoint
ai-model = Model
ai-load-models = Load models
ai-models-choose = Choose a model
ai-models-none = The provider listed no models
ai-api-key = API key
ai-key-placeholder = Leave empty to keep the stored key
ai-key-stored = stored
ai-key-missing = not set
ai-key-optional = not needed
ai-key-cleared = removed on save
ai-clear-key = Clear
ai-privacy-section = What is shared
ai-share-schema = Share schema (names and types)
ai-share-samples = Share sample values
ai-max-tokens = Max tokens per answer
ai-schema-budget = Schema budget (characters)
ai-test = Test
ai-test-ok = Connected: { $model }

agents-unavailable = Agents and the MCP server are not available in this build.
mcp-section = MCP server
mcp-enabled = Start the MCP server
mcp-port = Port (127.0.0.1)
mcp-running = Running
mcp-stopped = Not running
mcp-no-clients = No connected clients
mcp-clients = Clients: { $names }
mcp-copy-url = Copy URL
mcp-copy-token = Copy Token
mcp-copy-config = Copy Client Config
mcp-regenerate = Regenerate Token
mcp-not-running = The MCP server is not running
mcp-copied = Copied to the clipboard
mcp-token-regenerated = New token generated; clients must reconnect
agent-policy-section = Permissions
agent-default-access = Default access
agent-consent = Ask for consent
agent-max-rows = Max rows per query
agent-timeout = Consent timeout (seconds)
agent-sources = Access per data source
agent-access-hidden = Hidden
agent-access-metadata = Metadata only
agent-access-read-only = Read-only
agent-access-read-write = Read and write
agent-access-default = Default ({ $access })
agent-consent-every-call = For every call
agent-consent-queries = For queries only
agent-consent-writes = For writes only
audit-section = Audit log
audit-refresh = Refresh
audit-empty = No agent calls yet
audit-allowed-policy = allowed by policy
audit-allowed-user = allowed
audit-denied-policy = denied by policy
audit-denied-user = denied
audit-timed-out = timed out
status-mcp = MCP · { $clients ->
        [one] 1 client
       *[other] { $clients } clients
    }

consent-title = An agent wants to use a data source
consent-client = Client
consent-tool = Tool
consent-source = Data source
consent-modifies = This call changes data or schema.
consent-allow-once = Allow Once
consent-allow-session = Allow for This Session
consent-deny = Deny
consent-allowed = Allowed { $tool } for { $client }
consent-denied = Denied { $tool } for { $client }
consent-closed = A consent request was closed (timeout or the client gave up)

scripts-unavailable = Extension scripts are not available in this build.
scripts-no-folder = Scripts are not stored on disk in this configuration
scripts-name = Name
scripts-name-description = The file name of the new script (without .rhai).
scripts-create = Create
scripts-created = Script created: { $path }
scripts-reloaded = Scripts reloaded ({ $count })
grid-copy-as = Copy As
grid-copied-as = Copied to the clipboard
grid-script-failed = Script { $script } failed: { $error }
grid-aggregator = Summary of the selected column
grid-aggregate-builtin = Summary
grid-aggregate-failed = Aggregator failed: { $error }
