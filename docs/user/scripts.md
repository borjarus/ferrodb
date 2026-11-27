# Extension scripts

Extension scripts add your own export formats ("extractors") and column summaries ("aggregators"). They are written in [Rhai](https://rhai.rs), a small scripting language with a syntax close to Rust and JavaScript, and run in a sandbox.

| Kind | Used in | Input | Output |
|---|---|---|---|
| Extractor | The export wizard (as a format), **Copy As** in the grid's context menu | Rows, one at a time | Text |
| Aggregator | The **Σ** summary picker in the grid footer | The cells of the selected column over the loaded rows | One value |

## Built-in scripts

| Extractors | Aggregators |
|---|---|
| SQL IN list | Count |
| SQL VALUES rows | Distinct count |
| JSON object keyed by first column | Min – max |
| Markdown (aligned) | Median |
| PHP array | Percentile 90 |
| Pipe-separated | Standard deviation |
| | Comma-joined |

## Your own scripts

Scripts are files in the configuration directory:

```
<config>/scripts/extractors/*.rhai
<config>/scripts/aggregators/*.rhai
```

Commands in the **Tools** menu:

| Command | Does |
|---|---|
| **Open Scripts Folder** | Opens `<config>/scripts` in the file manager |
| **New Extractor Script…**, **New Aggregator Script…** | Asks for a **Name** (the file name, without `.rhai`), then writes a documented template. An existing file is never overwritten; a number is added instead |
| **Reload Scripts** | Reads the folder again (*Scripts reloaded (N)*) |

A script that does not compile is listed with its error and cannot be chosen. If a script fails while running, the export stops or the grid shows *Script … failed: …*.

## Script format

The first comment lines are the header:

```rhai
// name: SQL IN list
// description: The first column as an SQL IN list: (1, 2, 'x')
// extension: sql
```

| Header | Meaning |
|---|---|
| `name` | Shown in menus and the export wizard |
| `description` | Shown next to the name |
| `extension` | Extractors: the file extension suggested for exports |

Only the functions described below are called. Statements outside functions never run.

### Values

| Database value | Script value |
|---|---|
| `NULL` | `()` |
| Boolean | `bool` |
| Integer | `i64` |
| Floating point | `f64` |
| Everything else (text, dates, decimals, JSON, binary) | string |

`columns` is an array of maps `#{name, type}`; `type` is a simple type name such as `integer`, `float`, `decimal` or `text`.

## Extractors

Exports stream: an extractor gets one call per row, and nothing accumulates unless your script keeps it.

| Function | Required | Returns | Called |
|---|---|---|---|
| `header(columns)` | No | Text written before the first row | Once |
| `row(values, columns, index)` | Yes | Text for one row. `index` starts at 0. `columns` and `index` may be left out of the parameter list | Once per row |
| `separator()` | No | Text written between rows | Once (the result is reused) |
| `footer(count)` | No | Text written after the last row; `count` is the number of rows | Once |

`options()` returns the export options as a map:

| Key | Value |
|---|---|
| `table` | Table name for SQL output, or `()` |
| `header` | Whether a header row was requested |
| `null_text` | Text for `NULL` |
| `delimiter` | Field delimiter (default `,`) |

Inside these functions, `this` is a map that lives for the whole export. Use it for state, such as column widths. State grows with the data, so keep it small.

Example: the built-in **SQL IN list**.

```rhai
// name: SQL IN list
// description: The first column as an SQL IN list: (1, 2, 'x')
// extension: sql

fn header() { "(" }

fn row(values) {
    if values.is_empty() { "NULL" } else { sql_literal(values[0]) }
}

fn separator() { ", " }

fn footer() { ")" }
```

## Aggregators

| Function | Returns |
|---|---|
| `aggregate(values, table)` | Anything. Strings are shown as they are, numbers in their shortest form, `()` as empty |

- `values` is the flat array of the selected cells, row by row.
- `table` is `#{columns, rows}`: `columns` as above, `rows` an array of arrays. It may be left out of the parameter list.

Example: the built-in **Median**.

```rhai
// name: Median
// description: Median of the numeric values

fn aggregate(values) {
    let xs = numbers(values);
    let n = xs.len();
    if n == 0 {
        return ();
    }
    xs.sort();
    if n % 2 == 1 { xs[n / 2] } else { round_to((xs[n / 2 - 1] + xs[n / 2]) / 2.0, 10) }
}
```

## Helper functions

| Function | Returns |
|---|---|
| `text(v)`, `text(v, null_text)` | The value as text; `NULL` as empty text or `null_text` |
| `sql_literal(v)` | An SQL literal: `NULL`, a number, or a quoted and escaped string |
| `sql_ident(s)` | A quoted SQL identifier |
| `json(v)` | The value as JSON |
| `csv_field(v)`, `csv_field(v, delimiter)` | A CSV field, quoted when needed |
| `html_escape(v)` | Text with HTML special characters escaped |
| `md_escape(v)` | Text with Markdown table special characters escaped |
| `is_null(v)` | `true` for `NULL` |
| `is_number(v)` | `true` for `i64` and `f64` |
| `to_number(v)` | A number from a number or numeric text, else `()` |
| `numbers(values)` | The numeric values (and numeric text) of an array, as floats |
| `join(array, separator)` | The items joined with the separator |
| `pad(v, width)`, `pad_left(v, width)` | Text padded on the right or the left to `width` |
| `format_number(x, decimals)` | A number formatted with that many decimals |
| `round_to(x, decimals)` | A number rounded to that many decimals |
| `num_text(x)` | A number in its shortest text form |
| `options()` | The export options (extractors) |

Rhai's own functions for strings, arrays and maps (`len`, `push`, `map`, `filter`, `sort`, `contains`, …) are available as well.

## Sandbox and limits

- No access to files, the network or other processes. `import` and `eval` are disabled; `print` and `debug` output goes nowhere.
- Each call may run at most 5 million operations. Strings are limited to 16 MB, arrays to 5 million items, maps to 100,000 entries, and calls to 64 levels of nesting.
- A script that exceeds a limit fails with an error. Cancelling an export stops the script.
