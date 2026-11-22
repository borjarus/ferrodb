//! One compiled script: header, entry points, the streaming extractor
//! writer and the aggregator call.

use std::{path::PathBuf, sync::Arc};

use ferrodb_api::{
    Error, ExtScriptInfo, ExtScriptKind, FormatOptions, Result, ScriptTable, ScriptValue,
};
use rhai::{AST, Array, CallFnOptions, Dynamic, Engine, Map, Scope};

use crate::sandbox::{self, row_array, to_dynamic};

/// Polled while a script runs; `true` stops it with [`Error::Cancelled`].
pub type CancelFn = Arc<dyn Fn() -> bool + Send + Sync>;

/// The `// key: value` comment lines at the top of a script.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Header {
    pub name: Option<String>,
    pub description: Option<String>,
    pub extension: Option<String>,
}

/// Reads the header: leading `//` lines (blank lines allowed) up to the
/// first line of code.
pub fn parse_header(source: &str) -> Header {
    let mut h = Header::default();
    for line in source.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Some(comment) = line.strip_prefix("//") else {
            break;
        };
        let Some((key, value)) = comment.split_once(':') else {
            continue;
        };
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        let slot = match key.trim().to_ascii_lowercase().as_str() {
            "name" => &mut h.name,
            "description" => &mut h.description,
            "extension" => &mut h.extension,
            _ => continue,
        };
        if slot.is_none() {
            *slot = Some(value.to_owned());
        }
    }
    h
}

/// A column handed to an extractor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScriptColumn {
    pub name: String,
    /// Type name (`integer`, `text`, `decimal`…); may be empty.
    pub ty: String,
}

impl ScriptColumn {
    pub fn new(name: impl Into<String>, ty: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ty: ty.into(),
        }
    }
}

/// Arity of the script's entry points (`None` = not defined).
#[derive(Clone, Copy, Debug, Default)]
struct Entry {
    header: Option<usize>,
    row: Option<usize>,
    separator: Option<usize>,
    footer: Option<usize>,
    aggregate: Option<usize>,
}

/// A compiled script, cheap to clone.
#[derive(Clone)]
pub struct CompiledScript {
    pub info: ExtScriptInfo,
    ast: Arc<AST>,
    entry: Entry,
    cancel: Option<CancelFn>,
}

impl std::fmt::Debug for CompiledScript {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompiledScript")
            .field("info", &self.info)
            .finish_non_exhaustive()
    }
}

/// Lower-case extension of letters and digits, default `txt`.
fn clean_extension(ext: Option<&str>) -> String {
    let e: String = ext
        .unwrap_or("")
        .trim()
        .trim_start_matches('.')
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect::<String>()
        .to_ascii_lowercase();
    if e.is_empty() { "txt".into() } else { e }
}

/// Compiles a script. The result always carries the listing info; the
/// error (syntax, missing entry point) is also stored in `info.error`.
pub fn compile(
    kind: ExtScriptKind,
    id: &str,
    source: &str,
    path: Option<PathBuf>,
) -> (ExtScriptInfo, Result<CompiledScript>) {
    let header = parse_header(source);
    let stem = id.strip_prefix("builtin:").unwrap_or(id);
    let mut info = ExtScriptInfo {
        id: id.to_owned(),
        kind,
        name: header.name.unwrap_or_else(|| stem.to_owned()),
        description: header.description.unwrap_or_default(),
        extension: match kind {
            ExtScriptKind::Extractor => clean_extension(header.extension.as_deref()),
            ExtScriptKind::Aggregator => String::new(),
        },
        builtin: path.is_none(),
        path,
        error: None,
    };
    let r = compile_ast(kind, source);
    match r {
        Ok((ast, entry)) => {
            let s = CompiledScript {
                info: info.clone(),
                ast: Arc::new(ast),
                entry,
                cancel: None,
            };
            (info, Ok(s))
        }
        Err(e) => {
            info.error = Some(e.clone());
            (info, Err(Error::Other(format!("script `{id}`: {e}"))))
        }
    }
}

fn compile_ast(kind: ExtScriptKind, source: &str) -> std::result::Result<(AST, Entry), String> {
    let engine = sandbox::engine(Map::new(), None);
    let ast = engine.compile(source).map_err(|e| e.to_string())?;
    let mut entry = Entry::default();
    for f in ast.iter_functions() {
        let n = Some(f.params.len());
        match f.name {
            "header" if f.params.len() <= 1 => entry.header = n,
            "row" if (1..=3).contains(&f.params.len()) => entry.row = n,
            "separator" if f.params.is_empty() => entry.separator = n,
            "footer" if f.params.len() <= 1 => entry.footer = n,
            "aggregate" if (1..=2).contains(&f.params.len()) => entry.aggregate = n,
            _ => {}
        }
    }
    match kind {
        ExtScriptKind::Extractor if entry.row.is_none() => {
            Err("an extractor must define `fn row(values, columns, index)`".into())
        }
        ExtScriptKind::Aggregator if entry.aggregate.is_none() => {
            Err("an aggregator must define `fn aggregate(values, table)`".into())
        }
        _ => Ok((ast, entry)),
    }
}

/// `options()` of an extractor.
fn options_map(o: &FormatOptions) -> Map {
    let mut m = Map::new();
    m.insert(
        "table".into(),
        o.table.clone().map_or(Dynamic::UNIT, Dynamic::from),
    );
    m.insert("header".into(), Dynamic::from_bool(o.header));
    m.insert("null_text".into(), Dynamic::from(o.null_text.clone()));
    m.insert(
        "delimiter".into(),
        Dynamic::from(o.delimiter.unwrap_or(',').to_string()),
    );
    m
}

fn columns_array(columns: &[ScriptColumn]) -> Array {
    columns
        .iter()
        .map(|c| {
            let mut m = Map::new();
            m.insert("name".into(), Dynamic::from(c.name.clone()));
            m.insert("type".into(), Dynamic::from(c.ty.clone()));
            Dynamic::from_map(m)
        })
        .collect()
}

fn table_columns(t: &ScriptTable) -> Vec<ScriptColumn> {
    t.columns
        .iter()
        .enumerate()
        .map(|(i, n)| ScriptColumn::new(n, t.types.get(i).cloned().unwrap_or_default()))
        .collect()
}

/// Calls script functions with the run's state bound to `this`.
struct Runner {
    engine: Engine,
    ast: Arc<AST>,
    scope: Scope<'static>,
    state: Dynamic,
    name: String,
}

impl Runner {
    fn new(s: &CompiledScript, options: Map) -> Self {
        Self {
            engine: sandbox::engine(options, s.cancel.clone()),
            ast: s.ast.clone(),
            scope: Scope::new(),
            state: Dynamic::from_map(Map::new()),
            name: s.info.name.clone(),
        }
    }

    fn call(&mut self, f: &str, mut args: Vec<Dynamic>, arity: usize) -> Result<Dynamic> {
        args.truncate(arity);
        let opts = CallFnOptions::new()
            .eval_ast(false)
            .rewind_scope(true)
            .bind_this_ptr(&mut self.state);
        self.engine
            .call_fn_with_options::<Dynamic>(opts, &mut self.scope, &self.ast, f, args)
            .map_err(|e| sandbox::error(&self.name, e))
    }
}

impl CompiledScript {
    /// Stops runs of this script when `cancel` returns `true`.
    pub fn with_cancel(mut self, cancel: impl Fn() -> bool + Send + Sync + 'static) -> Self {
        self.cancel = Some(Arc::new(cancel));
        self
    }

    pub fn kind(&self) -> ExtScriptKind {
        self.info.kind
    }

    fn expect(&self, kind: ExtScriptKind) -> Result<()> {
        if self.info.kind == kind {
            Ok(())
        } else {
            Err(Error::Other(format!(
                "`{}` is not an {}",
                self.info.id,
                match kind {
                    ExtScriptKind::Extractor => "extractor",
                    ExtScriptKind::Aggregator => "aggregator",
                }
            )))
        }
    }

    /// Runs an aggregator over selected cells.
    pub fn aggregate(&self, cells: &ScriptTable) -> Result<String> {
        self.expect(ExtScriptKind::Aggregator)?;
        let mut runner = Runner::new(self, Map::new());
        let values: Array = cells.rows.iter().flatten().map(to_dynamic).collect();
        let mut table = Map::new();
        table.insert(
            "columns".into(),
            Dynamic::from_array(columns_array(&table_columns(cells))),
        );
        table.insert(
            "rows".into(),
            Dynamic::from_array(
                cells
                    .rows
                    .iter()
                    .map(|r| Dynamic::from_array(row_array(r)))
                    .collect(),
            ),
        );
        let arity = self.entry.aggregate.unwrap_or(2);
        let out = runner.call(
            "aggregate",
            vec![Dynamic::from_array(values), Dynamic::from_map(table)],
            arity,
        )?;
        Ok(sandbox::result_text(out))
    }

    /// Runs an extractor over rows held in memory.
    pub fn extract(&self, table: &ScriptTable, options: &FormatOptions) -> Result<String> {
        let mut w = ExtractorWriter::new(self, table_columns(table), options)?;
        let mut out = String::new();
        for r in &table.rows {
            out.push_str(&w.write_row(r)?);
        }
        out.push_str(&w.finish()?);
        Ok(out)
    }
}

/// Streams rows through an extractor: each call returns the text to append
/// (the header comes with the first row, or with `finish` when there are
/// no rows).
pub struct ExtractorWriter {
    runner: Runner,
    entry: Entry,
    columns: Array,
    separator: Option<String>,
    started: bool,
    rows: u64,
}

impl ExtractorWriter {
    pub fn new(
        script: &CompiledScript,
        columns: Vec<ScriptColumn>,
        options: &FormatOptions,
    ) -> Result<Self> {
        script.expect(ExtScriptKind::Extractor)?;
        Ok(Self {
            runner: Runner::new(script, options_map(options)),
            entry: script.entry,
            columns: columns_array(&columns),
            separator: None,
            started: false,
            rows: 0,
        })
    }

    /// Rows written so far.
    pub fn rows(&self) -> u64 {
        self.rows
    }

    fn start(&mut self) -> Result<String> {
        self.started = true;
        let mut out = String::new();
        if let Some(n) = self.entry.header {
            let cols = Dynamic::from_array(self.columns.clone());
            out = sandbox::result_text(self.runner.call("header", vec![cols], n)?);
        }
        if self.entry.separator.is_some() {
            let s = self.runner.call("separator", Vec::new(), 0)?;
            self.separator = Some(sandbox::result_text(s));
        }
        Ok(out)
    }

    pub fn write_row(&mut self, row: &[ScriptValue]) -> Result<String> {
        let mut out = if self.started {
            String::new()
        } else {
            self.start()?
        };
        if self.rows > 0
            && let Some(sep) = &self.separator
        {
            out.push_str(sep);
        }
        let arity = self.entry.row.unwrap_or(3);
        let mut args = vec![Dynamic::from_array(row_array(row))];
        if arity >= 2 {
            args.push(Dynamic::from_array(self.columns.clone()));
        }
        if arity >= 3 {
            args.push(Dynamic::from_int(self.rows as i64));
        }
        let r = self.runner.call("row", args, arity)?;
        out.push_str(&sandbox::result_text(r));
        self.rows += 1;
        Ok(out)
    }

    pub fn finish(mut self) -> Result<String> {
        let mut out = if self.started {
            String::new()
        } else {
            self.start()?
        };
        if let Some(n) = self.entry.footer {
            let count = Dynamic::from_int(self.rows as i64);
            out.push_str(&sandbox::result_text(self.runner.call(
                "footer",
                vec![count],
                n,
            )?));
        }
        Ok(out)
    }
}

/// The commented template written by `ScriptService::create`.
pub fn template(kind: ExtScriptKind, name: &str) -> String {
    let name = name.trim().replace(['\r', '\n'], " ");
    match kind {
        ExtScriptKind::Extractor => format!(
            r#"// name: {name}
// description: Custom export format
// extension: txt
//
// An extractor turns rows into text, one call per row (exports stream, so
// nothing accumulates). Values are () for NULL, bool, i64, f64 or a string
// (dates, decimals and JSON arrive as text). `columns` is an array of
// #{{name, type}} maps. Only the functions below are called.
//
// options() returns the export options: #{{table, header, null_text, delimiter}}.
// `this` is a map kept for the whole export, for state you need.
//
// Helpers: text(v), text(v, null_text), sql_literal(v), sql_ident(s), json(v),
// csv_field(v, delimiter), html_escape(v), md_escape(v), is_null(v),
// is_number(v), to_number(v), numbers(values), join(array, sep), pad(v, width),
// pad_left(v, width), format_number(x, decimals), round_to(x, decimals), num_text(x).
//
// The sandbox has no file, network or process access; each call is limited
// in operations and memory.

// Optional: written before the first row.
fn header(columns) {{
    if !options().header {{
        return "";
    }}
    join(columns.map(|c| c.name), "\t") + "\n"
}}

// Required: one row. `index` starts at 0; `columns` and `index` may be left out.
fn row(values, columns, index) {{
    join(values.map(|v| text(v, options().null_text)), "\t")
}}

// Optional: written between rows (called once).
fn separator() {{
    "\n"
}}

// Optional: written after the last row.
fn footer(count) {{
    if count > 0 {{ "\n" }} else {{ "" }}
}}
"#
        ),
        ExtScriptKind::Aggregator => format!(
            r#"// name: {name}
// description: Custom aggregate of the selected cells
//
// An aggregator reduces the selected cells to one value shown in the status
// bar. `values` is the flat array of the selected cells (row by row): () for
// NULL, bool, i64, f64 or a string. `table` is #{{columns, rows}}, with
// `columns` an array of #{{name, type}} maps and `rows` an array of arrays.
// Return a string or a number; () shows nothing.
//
// Helpers: text(v), sql_literal(v), json(v), is_null(v), is_number(v),
// to_number(v), numbers(values) (numeric values and numeric text as floats),
// join(array, sep), format_number(x, decimals), round_to(x, decimals), num_text(x).
//
// The sandbox has no file, network or process access; each call is limited
// in operations and memory.

fn aggregate(values, table) {{
    let xs = numbers(values);
    if xs.is_empty() {{
        return ();
    }}
    let sum = 0.0;
    for x in xs {{
        sum += x;
    }}
    num_text(sum / xs.len())
}}
"#
        ),
    }
}
