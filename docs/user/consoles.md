# Consoles

A console is an editor bound to one data source. It speaks that data source's language: an SQL dialect, MongoDB shell syntax (MQL), CQL, SQL++, PartiQL or Redis commands.

## Opening a console

| How | Result |
|---|---|
| **File ▸ New Console** or Ctrl+Shift+L (Cmd+Shift+L) | A console for the selected data source |
| **New Console** in the explorer's context menu | A console for that data source |
| A script in the **Files** panel | A console bound to the file (see [Tools](tools.md#project-files)) |
| **Generate …**, **Copy DDL**, the AI assistant, query history | A console with the generated text |

The toolbar shows the data source and its language (click to switch to another data source) and, where the database has them, a schema or database picker. The picker sets the current schema for running statements and for completion.

**Save File** (Ctrl+S / Cmd+S) writes a console bound to a file. A console without a file is saved to `.ferrodb/consoles/` in the project. Every console also gets [Local History](tools.md#local-history) revisions when it runs, is saved, and after 30 seconds without typing.

## Editor features

How much the editor knows depends on the language support level of the database (see [Supported databases](databases/README.md)):

| Level | Databases | Features |
|---|---|---|
| A — full | PostgreSQL, MySQL, MariaDB, SQLite, SQL Server, Azure SQL, Oracle, MongoDB (MQL) | Highlighting, statement splitting, schema-aware completion including `JOIN` conditions on foreign keys, name resolution, inspections, formatter |
| B — standard | Other SQL dialects, CQL, SQL++, PartiQL, Redis commands | Highlighting, statement splitting, completion of objects, columns or fields, keywords and built-in functions, syntax diagnostics, formatter |
| C — basic | Custom JDBC drivers | Generic SQL highlighting, statement splitting, object names from the driver's metadata |

### Completion

Completions appear as you type; **Ctrl+Space** opens them explicitly. Suggestions come from the cached metadata: tables, columns, `JOIN … ON` conditions built from foreign keys, `INSERT` column lists, keywords, functions with their signatures, and live templates. Matching accepts a prefix, an abbreviation (`cu_id` finds `customer_id`), and text typed with a Cyrillic keyboard layout. In MongoDB consoles, field names come from the cached schema and from sampled documents.

### Diagnostics and inspections

Problems are underlined as you type. Level A languages report, among others: unknown names (with suggestions), ambiguous columns, `= NULL`, `UPDATE` or `DELETE` without `WHERE`, unused CTEs, duplicate aliases, and `INSERT` statements whose value count does not match the columns. Errors returned by the server are also shown at their position in the editor.

### Formatter

**Console ▸ Reformat Code** (Ctrl+Alt+L / Cmd+Alt+L) formats the console. Formatting keeps every token and comment, and formatting twice gives the same text. MongoDB statements that contain comments are left as they are.

### Navigation

- **Go to Declaration** (Ctrl+B / Cmd+B): selects the object under the cursor in the explorer.
- **Quick Documentation** (Ctrl+Q / F1 on macOS): shows the object's columns, keys and comments, or its definition, in the Inspector.

## Running statements

| Command | Shortcut | Runs |
|---|---|---|
| **Execute** | Ctrl+Enter / Cmd+Enter | The selection, or the statement under the cursor |
| **Execute Script** | Ctrl+Shift+Enter / Cmd+Shift+Enter | The whole console, statement by statement |
| **Explain Plan** | Ctrl+Shift+E / Cmd+Shift+E | The execution plan of the statement (see [Tools](tools.md#execution-plans)) |
| **Explain Analyze** | Ctrl+Alt+Shift+E / Cmd+Alt+Shift+E | Runs the statement and shows the plan with actual rows and times |
| **Stop Query** | Ctrl+F2 / Cmd+F2 | Cancels the running statement |

The console highlights the statement that **Execute** will run. Statements are split the way the database expects: semicolons, `GO` batches in T-SQL, `/` after PL/SQL blocks in Oracle, `DELIMITER` in MySQL, routine bodies, dollar-quoted strings, and so on.

### Results

Each statement that returns data gets a result tab below the editor; messages, notices, warnings and affected-row counts go to the **Output** tab. A comment on the line above a statement names its result tab:

```sql
-- Orders by month
SELECT date_trunc('month', created_at), count(*) FROM orders GROUP BY 1;
```

The first page (500 rows or documents) is shown as soon as it arrives; the rest stays on the server until you ask for it. Only the last result of a script stays open; earlier results are cut at their first page with a notice.

### Parameters

Statements with parameters ask for values in a **Parameter values** dialog before they run:

| Syntax | Databases |
|---|---|
| `:name` | All SQL dialects |
| `$1`, `$2` | PostgreSQL family |
| `?` | MySQL, MariaDB, SQLite |
| `@name` | SQL Server, Azure SQL |

Values are inserted as SQL literals, escaped for the dialect. Leave a field empty for `NULL`. Error positions reported by the server are mapped back to your original text.

### Confirmation of dangerous statements

Before running, FerroDb checks every statement. A **Confirm execution** dialog lists statements that may change or destroy data: **DELETE without WHERE**, **UPDATE without WHERE**, **DROP**, **TRUNCATE**, and similar operations in other languages (for example `deleteMany({})` or `dropDatabase` in MongoDB, `FLUSHALL` in Redis). On a data source marked **Production**, every statement that modifies data or schema asks.

On a read-only data source, modifying statements are refused before they are sent.

### Query cost

For data sources that can estimate the cost of a query before running it, the console shows a **Query cost** dialog (*This query will process about …*) and the estimate in the toolbar.

### Timeouts

Set `query_timeout=<seconds>` in the data source's **Options** tab to cancel statements that run too long. They are reported as `FERRODB-TIMEOUT`.

## Transactions

| Mode | Behavior |
|---|---|
| Auto-commit (default) | Every statement is committed by the database |
| **Manual commit** (data source option) | FerroDb begins a transaction before the first statement. **Commit** and **Roll Back** appear in the console toolbar |

`BEGIN`, `COMMIT` and `ROLLBACK` typed in a script are tracked as well. The **Services** panel shows which sessions have an open transaction. If the connection is lost, FerroDb reconnects and warns you; an open transaction is reported as lost. Databases without transactions (for example ClickHouse) say so instead of opening one; see the database pages.

## Cancellation

**Stop Query** cancels on the server (a cancel request, `KILL QUERY`, `killOp`, a TDS attention, and so on, depending on the database), not only in FerroDb. The connection stays usable. Running queries can also be stopped from the **Services** panel.

## AI commands

With the AI assistant turned on, the **Console** menu and the editor's context menu have **Ask AI…**, **AI: Explain Query**, **AI: Optimize Query** and **Fix with AI** (after a statement fails). See [AI assistant and MCP](ai-and-mcp.md).

## Not available yet

**Rename…** (Shift+F6) and **Insert Live Template…** (Ctrl+J) are listed in the **Console** menu but show *Coming in phase 5*: the language engine supports them, the console does not use them yet. Quick fixes for inspections are not offered in the editor yet. Live templates do appear in completion.
