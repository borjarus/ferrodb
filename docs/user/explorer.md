# Database Explorer

The Database Explorer (left panel, Alt+1 / Cmd+1) shows your data sources in folders and, below each one, its objects.

## The tree

- Expanding a data source connects to it. The levels below follow the database: for example schemas → tables → columns for PostgreSQL, databases → collections → indexes for MongoDB, keyspaces → tables for Cassandra, databases → key prefixes → keys for Redis.
- Objects are grouped by kind with a count, e.g. "tables 24".
- Data sources show their environment color and a lock when they are read-only.
- Objects load lazily, only when you expand them.
- **Double-click** a table, view or collection to open its data.

Selecting a data source or object shows its details in the **Inspector** (right panel).

### Metadata cache

FerroDb reads each database's catalog once and keeps it in memory and on disk (`metadata/<source>.json` in the data directory). On the next start the tree, completion and Search Everywhere work from the cache before any connection is made.

FerroDb does not detect schema changes made elsewhere. Use **Refresh** (Ctrl+F5 / Cmd+F5, or the context menu) on a data source, schema or table to read it again. Refreshing a table re-reads only that table.

## Filtering

- **Filter objects…** at the top of the panel filters the tree by name and keeps the branches that contain a match.
- To hide schemas permanently, use the **Schemas** tab of the data source dialog (see [Data sources](data-sources.md#schemas-tab)).

## Context menu

The menu depends on the object and the data model.

| Item | Where | What it does |
|---|---|---|
| **Connect**, **Disconnect** | Data source | Opens or closes the connections |
| **New Console** | Data source, objects | Opens a console for the data source |
| **Refresh** | Everywhere | Re-reads metadata |
| **Data Source Properties…** | Data source | Opens the data source dialog |
| **Driver Manager…** | Data source | Opens the Driver Manager |
| **Remove Data Source…** | Data source | Removes it after confirmation |
| **Open Data** | Tables, views, collections | Opens the data in a result tab |
| **Copy Name** | Objects | Copies the qualified name |
| **Copy DDL** | Tabular objects | Copies the object's DDL to the clipboard and opens it in a new console |
| **Copy Script** | Document databases: database, collection, view, index | Copies the script that recreates the object (for MongoDB `db.createCollection(…)` plus `createIndex(…)`) and opens it in a new console |
| **Generate SELECT / INSERT / UPDATE / DELETE / DROP** | Tables | Opens a new console with the statement (see below) |
| **Generate DROP** | Columns, keys, indexes, triggers, sequences | Opens a new console with the `DROP` statement |
| **Generate SELECT / DROP** | Views | Opens a new console with the statement |
| **Generate find() / INSERT / UPDATE / DELETE / DROP** | Collections | The same for documents |
| **Generate Call / DROP** | Procedures and functions | Opens a call with named parameters |
| **Quick Documentation** | Objects | Shows columns, keys, indexes, comments or the definition in the Inspector |
| **Modify Table…** | Tables | Opens the table editor (see [Tools](tools.md#table-editor)) |
| **New Table…** | Schemas | Opens the table editor for a new table |
| **ER Diagram** | Schemas, tables | Opens the ER diagram (see [Tools](tools.md#er-diagrams)) |
| **Compare Schemas** | Schemas | Opens schema comparison |
| **Infer Schema** | Collections | Samples documents and shows their fields and types |
| **Export Data…**, **Import Data…** | Tables, collections | Opens the export or import wizard |

### Generated statements

- `SELECT` lists every column.
- `INSERT` leaves out generated, identity and serial columns.
- `UPDATE` and `DELETE` match the row by its primary key, else a unique key, else all columns (with a warning comment).
- Calls use `:name` parameters, so running them asks for the values.

### DDL

DDL for tables is generated from the catalog, including keys, indexes and comments. Views, routines and triggers use the definition stored on the server. DDL for several tables comes in foreign-key order. Which object kinds are covered depends on the database; see its page.

## Search Everywhere

Press **Shift** twice. Search Everywhere ranks actions, data sources, and the tables, views, routines, sequences and collections of every data source's cached metadata. It works before you connect.

## Go to declaration and quick documentation

In a console, put the cursor on a name and press **Ctrl+B** (Cmd+B) to select that object in the explorer, or **Ctrl+Q** (F1 on macOS) to see its documentation in the Inspector. Names are resolved against the cached metadata, current schema first.
