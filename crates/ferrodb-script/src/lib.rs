//! # ferrodb-script (plan, phase 9)
//!
//! Extension scripts in [`rhai`]: custom export formats ("extractors") and
//! aggregates of selected cells ("aggregators"), like the data extractors
//! and aggregators of JetBrains IDEs.
//!
//! Scripts live in `<config>/scripts/extractors/*.rhai` and
//! `<config>/scripts/aggregators/*.rhai`; built-ins are compiled into the
//! binary (ids `builtin:<stem>`). [`ScriptLibrary`] lists, compiles, caches
//! and runs them and implements [`ferrodb_api::ScriptService`].
//!
//! ## Script format
//!
//! Leading comment lines are the header:
//!
//! ```text
//! // name: SQL IN list
//! // description: (1, 2, 'x') of the first column
//! // extension: sql
//! ```
//!
//! Only the functions below are called; top-level statements are never
//! run. Values are `()` for NULL, `bool`, `i64`, `f64` or a string (dates,
//! decimals, JSON and binary arrive as text). `columns` is an array of
//! `#{name, type}` maps (`type` = `integer`, `text`, `decimal`…).
//!
//! **Extractor** (streaming: one call per row, nothing accumulates):
//!
//! - `fn header(columns)` → string, optional, before the first row;
//! - `fn row(values, columns, index)` → string, required (`columns` and
//!   `index`, 0-based, may be left out of the parameter list);
//! - `fn separator()` → string between rows, optional (called once);
//! - `fn footer(count)` → string, optional, after the last row.
//!
//! `options()` returns the export options as a map: `table` (string or
//! `()`), `header` (bool), `null_text`, `delimiter` (default `,`). Inside
//! these functions `this` is a map kept for the whole run, for scripts that
//! need state (e.g. column widths); state grows with the data, so use it
//! sparingly.
//!
//! **Aggregator**: `fn aggregate(values, table)` → anything; `values` is the
//! flat array of the selected cells (row-major), `table` is
//! `#{columns, rows}`. Strings are shown as they are, numbers in their
//! shortest form, `()` as an empty text.
//!
//! ## Helpers
//!
//! `text(v)` / `text(v, null_text)`, `sql_literal(v)`, `sql_ident(s)`,
//! `json(v)`, `csv_field(v)` / `csv_field(v, delimiter)`, `html_escape(v)`,
//! `md_escape(v)`, `is_null(v)`, `is_number(v)`, `to_number(v)` (numbers and
//! numeric text, else `()`), `numbers(values)` (the numeric values as
//! floats), `join(array, separator)`, `pad(v, width)`, `pad_left(v, width)`,
//! `format_number(x, decimals)`, `round_to(x, decimals)`, `num_text(x)`,
//! `options()`.
//!
//! ## Sandbox
//!
//! No module resolver (`import` and `eval` are disabled), no file, network
//! or process access (rhai has none; `print`/`debug` go nowhere), bounded
//! operations per call, string / array / map sizes, call depth and
//! expression depth. A runaway script fails with an error; a cancel flag
//! stops it between operations.

mod builtin;
mod library;
mod sandbox;
mod script;

pub use library::ScriptLibrary;
pub use sandbox::{MAX_ARRAY, MAX_OPERATIONS, MAX_STRING};
pub use script::{
    CancelFn, CompiledScript, ExtractorWriter, Header, ScriptColumn, compile, parse_header,
    template,
};

#[cfg(test)]
mod tests;
