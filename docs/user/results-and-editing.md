# Results and editing

Results appear in tabs below the console. Tabular results open in the result grid, documents in the document browser, and Redis keys in the key browser.

## Result grid

The grid shows rows as they arrive. Column headers show the database type (`varchar(20)`, `numeric(10,2)`). Values keep their exact type: decimals are not rounded, and values that cannot be represented become `NULL` with a warning instead of being changed silently.

### Paging

- The first page holds 500 rows. **Load more** fetches the next page and **Load all** the rest; **Page: N** shows the page size.
- The footer shows the row count, *(more available)* while the server has more, and *shown: N* when a search hides rows.

### Sorting, searching and filtering

| Tool | Works on |
|---|---|
| Click a column header | Sorts the loaded rows |
| **Search loaded rows** (Ctrl+F / Cmd+F) | Searches the loaded rows |
| `WHERE …` and `ORDER BY …` fields | Run the statement again on the server as `SELECT * FROM (<your statement>) t WHERE … ORDER BY …` |
| **Filter by This Value** (context menu) | Adds a condition for the clicked value (`city = 'O''Hara'`, `note IS NULL`, or `{ "city": "Kraków" }` for MongoDB) and runs the statement again |

### Context menu

| Item | Action |
|---|---|
| **Copy Value** | Copies the cell (also Ctrl+C / Cmd+C) |
| **Copy Row (TSV)**, **Copy Row as JSON**, **Copy Row as INSERT** | Copies the row in that format |
| **Copy As** ▸ *extractor* | Copies the row through an extractor script (see [Extension scripts](scripts.md)) |
| **Edit Value** | Edits the cell |
| **Set NULL** | Sets the cell to `NULL` |
| **Delete Row** | Marks the row for deletion |
| **Filter by This Value** | See above |
| **Related: …** | One item per foreign key: opens the referenced rows (`customer_id → customers(id)`) or the rows that reference this one (`orders.customer_id → customers`) |

### Record view and summaries

- **Record view** shows one row as a vertical list of fields.
- The footer shows a summary of the selected column over the loaded rows. The **Σ** picker (**Summary of the selected column**) switches between the built-in **Summary** and aggregator scripts such as median, percentile 90 or distinct count.

## Editing tabular results

A result can be edited when FerroDb can map it back to one table:

- a `SELECT` from one table or alias, without `GROUP BY`, `HAVING`, `DISTINCT` or set operations (`UNION` …);
- computed columns stay read-only;
- views cannot be edited;
- the data source is not read-only.

Otherwise the grid says **Results of this data source cannot be edited** or **The data source is read-only**.

To edit:

1. Change cells inline (text, numbers and dates are validated as you type; booleans toggle), use **Set NULL**, **Delete Row**, or **Add row** (empty cells take the column defaults). Changes stay pending; the footer counts them.
2. **Submit Changes** (Ctrl+Enter / Cmd+Enter in the grid) opens **Review N changes**: the exact statements that will run.
3. Confirm to apply, or close the dialog to keep editing. **Revert Changes** (Ctrl+Alt+Z / Cmd+Alt+Z) drops all pending changes.

How changes are applied:

- One `UPDATE` per edited row, with a `WHERE` on the row's original key values. The key is the primary key, else a unique key. Without a key, rows are matched by all their values; the preview starts with a warning comment, and MySQL statements get `LIMIT 1`.
- Every statement must change exactly one row. If one would change more or fewer, everything is undone and the message names the statement.
- On auto-commit data sources all changes run in one transaction. On manual-commit data sources they run inside your open transaction and wait for **Commit** (*N changes applied in the open transaction; commit to keep them*).

## Document browser

Results of MongoDB, Couchbase and DynamoDB open in the document browser with three views:

| View | Shows |
|---|---|
| **Tree** | Fields with their types (`ObjectId`, `ISODate`, `NumberLong`…). Arrays over 1000 items show the first items and *N more items* |
| **JSON** | The documents as text |
| **Table** | One row per document, one column per field |

- Filter, sort and projection fields use MQL for MongoDB, or a `WHERE` condition for SQL++ (Couchbase) and PartiQL (DynamoDB).
- **Edit Value…** changes a field; **Delete Document** deletes a document; **Copy Document as JSON** copies it.
- **Infer Schema** samples the collection and lists fields with their types, share and presence (*Sampled N documents*). **Indexes** lists the collection's indexes.

Edits go through the same **Review N changes** preview, shown in the database's language: `updateOne({_id: …}, {$set: …})` and `deleteOne(…)` in MongoDB, `UPDATE … USE KEYS` in Couchbase, `UPDATE "table" SET … WHERE pk = …` in DynamoDB. All changes run in one transaction where the server supports it.

## Key browser

Redis keys open in the key browser:

- Keys are grouped into a tree by the `:` separator. Columns show type, TTL and size.
- **Pattern, e.g. app:user:*** filters with `SCAN … MATCH`; keys load page by page.
- Select a key to see its value. Each type has its own view: string (text or JSON editor), hash (**Field** / **Value**), list (**Index** / **Value**), set (**Member**), sorted set (**Member** / **Score**), stream.
- Edit a member, **Save** a string value, set the TTL with **Set TTL** (seconds; empty for none), or **Delete Key**.

Every change shows the Redis commands that will run before it is applied.
