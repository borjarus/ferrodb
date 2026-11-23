# Import and export

Exports and imports stream their data: a 10-million-row export does not need 10 million rows of memory. Progress and the result are shown in a notification and in the **Output** panel.

## Formats

| Format | Export | Import | Notes |
|---|---|---|---|
| CSV | Yes | Yes | Header row, delimiter, text for `NULL`; import also handles a BOM |
| TSV | Yes | Yes | |
| JSON | Yes | Yes | An array of objects; optionally indented. Import also accepts JSON Lines |
| JSON Lines | Yes | Yes | One object per line (`.jsonl`, `.ndjson`) |
| Extended JSON | Yes | Yes | MongoDB canonical Extended JSON, one document per line; types are kept (`$oid`, `$date`, `$numberLong`…) |
| Excel (xlsx) | Yes | Yes | Real numbers and dates; a new sheet after 1,048,576 rows. Import reads the first sheet |
| Parquet | Yes | Yes | Snappy-compressed |
| SQL INSERT | Yes | — | `INSERT` statements in the source's dialect, 100 rows per statement |
| Markdown | Yes | — | |
| HTML | Yes | — | |
| Extractor scripts | Yes | — | Your own formats; see [Extension scripts](scripts.md) |

When documents are exported to a tabular format, the columns are taken from the first batch of documents. Fields that appear only later are counted in a warning.

## Export

Open the wizard with **File ▸ Export Data…**, the grid's export button, or **Export Data…** in the explorer's context menu.

What is exported:

- **a console result**: the statement runs again on its own connection, so the export contains every row, not only the loaded ones. Only read-only statements can be exported;
- **a table or collection** selected in the explorer.

Steps:

1. **Format**: choose a format, or an extractor script.
2. **Options**: the target **File**, **Header row**, and the options of the format.
3. **Preview**: check the source, the file and the format, then **Finish**.

When the export finishes, the notification reads *Exported N items to <file> (<method>, <time>)*.

## Import

Open the wizard with **File ▸ Import Data…** or **Import Data…** on a schema or table in the explorer.

1. **Source file**: choose the file. The format is detected from its extension.
2. **Format and target**: the **Target table**, and whether to **Create the table when it does not exist** and **Remove existing rows first**.
3. **Preview**: the first rows with the guessed column types, and the column mapping (*Columns are matched by name:*). Source columns without a target are marked *(not imported)*; new columns of a created table are marked *(new column)*.
4. **Finish**.

Details:

- Columns are matched by name, ignoring case. Values are converted to the target column types; a conversion error names the column.
- CSV types are guessed from the first 1000 rows.
- Imports into a read-only data source are refused.

### Fast paths

| Database | Method |
|---|---|
| PostgreSQL | `COPY … FROM STDIN` |
| MySQL, MariaDB | `LOAD DATA LOCAL INFILE`, streamed from memory. If the server refuses it (`local_infile` is off, the MySQL 8 default), the import continues with multi-row `INSERT` and says so in a warning |
| MongoDB | `insertMany`, 1000 documents per batch |
| SQLite | 500-row `INSERT` statements in one transaction |
| Others | Multi-row `INSERT` |

The method used is part of the final message.

## Known limitations

- The wizard matches columns by name only; it does not let you map columns by hand.
- JSON arrays and XLSX sheets are read whole on import. CSV, JSON Lines and Parquet are streamed.
- BSON dates have millisecond precision, so microseconds of SQL timestamps are lost when data passes through a MongoDB collection.
- Imports into Couchbase and DynamoDB are not supported.
