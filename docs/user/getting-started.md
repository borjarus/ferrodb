# Getting started

This page walks through a first session: open a project, add a data source, run a query and read the result.

## The main window

| Area | Default content |
|---|---|
| Left | **Database Explorer** (toggle with Alt+1, Cmd+1 on macOS) |
| Center | **Welcome** and consoles |
| Bottom | **Services**, **Output**, **Query History**, **Local History** (toggle with Alt+4, Cmd+4 on macOS) |
| Right | **Inspector**, and the **AI Assistant** when opened (hidden by default; **View ▸ Toggle Right Panel**) |

On Windows and Linux the menu bar is inside the window title bar. On macOS FerroDb uses the system menu bar. Panel positions are saved per project; **View ▸ Reset Layout** restores the defaults.

Two commands reach everything else:

- **Find Action…** (Ctrl+Shift+A, Cmd+Shift+A on macOS) runs any command by name.
- **Search Everywhere** (press Shift twice) finds actions, data sources, tables, collections and keys.

## Projects

A project is a folder. FerroDb stores the project's own state in a `.ferrodb/` subfolder:

| Path | Content |
|---|---|
| `.ferrodb/layout.json` | Panel layout |
| `.ferrodb/consoles/` | Consoles that are not bound to a file |
| `.ferrodb/diagrams/` | Saved ER diagram layouts |

- **File ▸ Open Project…** opens a folder as a project. **File ▸ Recent Projects…** lists the last ten.
- Without a project, FerroDb opens the last one you used, or a default project in its data directory (`default-project`).
- You can also start FerroDb with a folder: `ferrodb path/to/folder`.

Data sources are global in 1.0: every project sees the same list.

## Add your first data source

1. Choose **File ▸ New Data Source…**.
2. Pick the database on the left. The list is grouped into **Relational**, **Analytical and cloud** and **NoSQL**; type in **Search databases…** to filter it. **Templates** fill the form for common cases (for example **PostgreSQL (local Docker)** or **SQLite file**).
3. Fill in the **General** tab: name, host, port, database, **Authentication**, **User** and password. The password field says *stored in the system keyring*: it never goes into a configuration file.
4. If the driver is not present, the dialog shows **Not downloaded**. Click **Download**; see [Installation](installation.md#drivers-downloaded-on-demand).
5. Click **Test Connection**. A successful test shows the server version and the time it took.
6. Click **OK**.

The data source appears in the Database Explorer. Expanding it connects and loads its objects. [Data sources](data-sources.md) explains every field.

## Open a console and run a query

1. Select the data source in the explorer and choose **New Console** from its context menu, or press Ctrl+Shift+L (Cmd+Shift+L).
2. Type a query. The console offers completions from the database's metadata (Ctrl+Space) and underlines errors as you type.
3. Press **Ctrl+Enter** (Cmd+Enter) to run the statement under the cursor, or the selection if there is one. **Ctrl+Shift+Enter** runs the whole script.
4. Press **Ctrl+F2** (Cmd+F2) or the stop button to cancel a running query.

The console toolbar shows the data source and its query language. Click it to switch to another data source; the schema picker next to it sets the current schema or database.

If a statement can change or destroy data (for example `DELETE` without `WHERE`, `DROP`, `TRUNCATE`), FerroDb asks for confirmation first. On a data source marked **Production**, every modifying statement asks. More in [Consoles](consoles.md).

## Read the results

Results open in tabs below the editor. The view depends on the database:

| Database type | View |
|---|---|
| Tables (SQL, Cassandra) | Result grid |
| Documents (MongoDB, Couchbase, DynamoDB) | Document browser: **Tree**, **JSON**, **Table** |
| Keys (Redis) | Key browser |

The first 500 rows arrive as soon as the server sends them. **Load more** and **Load all** fetch the rest. You can sort and search the loaded rows, filter on the server with the `WHERE …` and `ORDER BY …` fields, open a row in **Record view**, and edit values. See [Results and editing](results-and-editing.md).

## Next steps

- [Database Explorer](explorer.md): generate `SELECT`, `INSERT` and DDL from objects.
- [Import and export](import-export.md): move data between files and databases.
- [Keyboard shortcuts](keyboard-shortcuts.md).
- The page for [your database](databases/README.md).
