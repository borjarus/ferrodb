//! Streaming export: rows (Arrow batches) or documents go in batch by
//! batch and are written immediately, so memory stays constant whatever
//! the size of the result. Formats: CSV, TSV, JSON, JSON Lines, Extended
//! JSON, Markdown, HTML, XLSX (constant-memory worksheets), SQL `INSERT`
//! and Parquet.
//!
//! Documents written to a tabular format get the columns of the first
//! batch (top-level fields); fields first seen later are dropped and
//! counted in a warning.

use std::{
    fs::File,
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

use arrow_array::{
    ArrayRef, RecordBatch,
    builder::{
        BinaryBuilder, BooleanBuilder, Date32Builder, Float64Builder, Int64Builder, StringBuilder,
        Time64MicrosecondBuilder, TimestampMicrosecondBuilder,
    },
};
use arrow_schema::{DataType, Field, Schema, SchemaRef, TimeUnit};
use bson::{Bson, Document, RawDocumentBuf};
use ferrodb_api::{DataFormat, Error, FormatOptions, Result};

use crate::{
    docs::{DocColumn, decode, document_to_row, infer_columns},
    sql::Sql,
    value::{Value, ValueType, value_at},
};

fn io_err(e: impl std::fmt::Display) -> Error {
    Error::Other(format!("export: {e}"))
}

/// A column as the writers see it.
#[derive(Clone, Debug)]
struct Col {
    name: String,
    ty: ValueType,
}

trait Writer: Send {
    fn begin(&mut self, cols: &[Col], schema: Option<&SchemaRef>) -> Result<()>;
    fn row(&mut self, row: &[Value]) -> Result<()>;
    /// Writers with a native batch path (Parquet) override this.
    fn batch(&mut self, batch: &RecordBatch) -> Result<()> {
        let mut row = Vec::with_capacity(batch.num_columns());
        for r in 0..batch.num_rows() {
            row.clear();
            row.extend(batch.columns().iter().map(|c| value_at(c.as_ref(), r)));
            self.row(&row)?;
        }
        Ok(())
    }
    /// Writes a whole document; `false` = not supported (use rows).
    fn document(&mut self, _doc: &Document) -> Result<bool> {
        Ok(false)
    }
    fn finish(self: Box<Self>) -> Result<()>;
}

/// Outcome of an export.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ExportStats {
    pub items: u64,
    pub bytes: u64,
    pub warnings: Vec<String>,
}

pub struct Exporter {
    path: PathBuf,
    writer: Box<dyn Writer>,
    started: bool,
    doc_columns: Option<Vec<DocColumn>>,
    items: u64,
    dropped_fields: u64,
    extra_results: u64,
}

impl Exporter {
    /// `dialect` is used by SQL `INSERT` (identifier quoting, literals).
    pub fn create(
        path: &Path,
        format: DataFormat,
        opts: &FormatOptions,
        dialect: &str,
    ) -> Result<Self> {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).map_err(io_err)?;
        }
        let file = || -> Result<BufWriter<File>> {
            Ok(BufWriter::with_capacity(
                256 * 1024,
                File::create(path).map_err(io_err)?,
            ))
        };
        let writer: Box<dyn Writer> = match format {
            DataFormat::Csv | DataFormat::Tsv => {
                let delim = if format == DataFormat::Tsv {
                    b'\t'
                } else {
                    opts.delimiter.map_or(b',', |c| c as u8)
                };
                Box::new(CsvWriter {
                    w: csv::WriterBuilder::new()
                        .delimiter(delim)
                        .from_writer(file()?),
                    header: opts.header,
                    null: opts.null_text.clone(),
                    buf: Vec::new(),
                })
            }
            DataFormat::Json | DataFormat::JsonLines | DataFormat::ExtendedJson => {
                Box::new(JsonWriter {
                    out: file()?,
                    mode: match format {
                        DataFormat::Json => JsonMode::Array {
                            pretty: opts.pretty,
                        },
                        DataFormat::JsonLines => JsonMode::Lines,
                        _ => JsonMode::Canonical,
                    },
                    cols: Vec::new(),
                    first: true,
                    line: String::new(),
                })
            }
            DataFormat::Markdown => Box::new(MarkdownWriter {
                out: file()?,
                null: opts.null_text.clone(),
            }),
            DataFormat::Html => Box::new(HtmlWriter {
                out: file()?,
                header: opts.header,
                null: opts.null_text.clone(),
            }),
            DataFormat::Xlsx => Box::new(XlsxWriter::new(path, opts.header)?),
            DataFormat::SqlInsert => Box::new(InsertWriter {
                out: file()?,
                sql: Sql::new(dialect),
                table: opts.table.clone().unwrap_or_else(|| "data".into()),
                cols: Vec::new(),
                rows: Vec::new(),
                per: opts.rows_per_statement.max(1),
            }),
            DataFormat::Parquet => Box::new(ParquetWriter {
                file: Some(File::create(path).map_err(io_err)?),
                writer: None,
                schema: None,
                rows: Vec::new(),
            }),
        };
        Ok(Self {
            path: path.to_owned(),
            writer,
            started: false,
            doc_columns: None,
            items: 0,
            dropped_fields: 0,
            extra_results: 0,
        })
    }

    /// A tabular result starts. Only the first result of a source is
    /// exported; later ones are counted in a warning.
    pub fn begin_table(&mut self, schema: &SchemaRef) -> Result<bool> {
        if self.started {
            self.extra_results += 1;
            return Ok(false);
        }
        self.started = true;
        let cols: Vec<Col> = schema
            .fields()
            .iter()
            .map(|f| Col {
                name: f.name().clone(),
                ty: ValueType::from_arrow(f.data_type()),
            })
            .collect();
        self.writer.begin(&cols, Some(schema))?;
        Ok(true)
    }

    pub fn write_batch(&mut self, batch: &RecordBatch) -> Result<()> {
        self.writer.batch(batch)?;
        self.items += batch.num_rows() as u64;
        Ok(())
    }

    /// Rows with explicit columns (imports, copies, tests).
    pub fn begin_rows(&mut self, columns: &[(String, ValueType)]) -> Result<()> {
        self.started = true;
        let cols: Vec<Col> = columns
            .iter()
            .map(|(n, t)| Col {
                name: n.clone(),
                ty: *t,
            })
            .collect();
        self.writer.begin(&cols, None)
    }

    pub fn write_row(&mut self, row: &[Value]) -> Result<()> {
        self.writer.row(row)?;
        self.items += 1;
        Ok(())
    }

    pub fn write_documents(&mut self, docs: &[RawDocumentBuf]) -> Result<()> {
        let decoded: Vec<Document> = docs.iter().map(decode).collect();
        self.write_decoded(&decoded)
    }

    pub fn write_decoded(&mut self, docs: &[Document]) -> Result<()> {
        if docs.is_empty() {
            return Ok(());
        }
        if self.doc_columns.is_none() {
            let cols = infer_columns(docs);
            let wcols: Vec<Col> = cols
                .iter()
                .map(|c| Col {
                    name: c.path.clone(),
                    ty: c.ty,
                })
                .collect();
            if !self.started {
                self.started = true;
                self.writer.begin(&wcols, None)?;
            }
            self.doc_columns = Some(cols);
        }
        for d in docs {
            if !self.writer.document(d)? {
                let cols = self.doc_columns.as_ref().expect("set above");
                self.dropped_fields += d
                    .keys()
                    .filter(|k| !cols.iter().any(|c| &c.path == *k))
                    .count() as u64;
                let row = document_to_row(d, cols);
                self.writer.row(&row)?;
            }
            self.items += 1;
        }
        Ok(())
    }

    pub fn items(&self) -> u64 {
        self.items
    }

    pub fn finish(mut self) -> Result<ExportStats> {
        if !self.started {
            // Empty result without a shape: an empty file of the format.
            self.writer.begin(&[], None)?;
        }
        self.writer.finish()?;
        let bytes = std::fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0);
        let mut warnings = Vec::new();
        if self.dropped_fields > 0 {
            warnings.push(format!(
                "{} field value(s) not present in the first documents were left out",
                self.dropped_fields
            ));
        }
        if self.extra_results > 0 {
            warnings.push(format!(
                "only the first result was exported ({} more skipped)",
                self.extra_results
            ));
        }
        Ok(ExportStats {
            items: self.items,
            bytes,
            warnings,
        })
    }
}

// ------------------------------------------------------------------ CSV

struct CsvWriter {
    w: csv::Writer<BufWriter<File>>,
    header: bool,
    null: String,
    buf: Vec<String>,
}

impl Writer for CsvWriter {
    fn begin(&mut self, cols: &[Col], _: Option<&SchemaRef>) -> Result<()> {
        if self.header && !cols.is_empty() {
            self.w
                .write_record(cols.iter().map(|c| c.name.as_str()))
                .map_err(io_err)?;
        }
        Ok(())
    }

    fn row(&mut self, row: &[Value]) -> Result<()> {
        self.buf.clear();
        self.buf.extend(
            row.iter()
                .map(|v| v.to_text().unwrap_or_else(|| self.null.clone())),
        );
        self.w.write_record(&self.buf).map_err(io_err)
    }

    fn finish(mut self: Box<Self>) -> Result<()> {
        self.w.flush().map_err(io_err)
    }
}

// ------------------------------------------------------------------ JSON

enum JsonMode {
    Array {
        pretty: bool,
    },
    Lines,
    /// Canonical Extended JSON lines.
    Canonical,
}

struct JsonWriter {
    out: BufWriter<File>,
    mode: JsonMode,
    cols: Vec<String>,
    first: bool,
    line: String,
}

impl JsonWriter {
    fn emit(&mut self) -> Result<()> {
        match self.mode {
            JsonMode::Array { pretty } => {
                let sep = if self.first { "\n" } else { ",\n" };
                self.out.write_all(sep.as_bytes()).map_err(io_err)?;
                if pretty {
                    let v: serde_json::Value = serde_json::from_str(&self.line).map_err(io_err)?;
                    let p = serde_json::to_string_pretty(&v).map_err(io_err)?;
                    for (i, l) in p.lines().enumerate() {
                        if i > 0 {
                            self.out.write_all(b"\n").map_err(io_err)?;
                        }
                        self.out.write_all(b"  ").map_err(io_err)?;
                        self.out.write_all(l.as_bytes()).map_err(io_err)?;
                    }
                } else {
                    self.out.write_all(self.line.as_bytes()).map_err(io_err)?;
                }
            }
            JsonMode::Lines | JsonMode::Canonical => {
                self.out.write_all(self.line.as_bytes()).map_err(io_err)?;
                self.out.write_all(b"\n").map_err(io_err)?;
            }
        }
        self.first = false;
        Ok(())
    }
}

impl Writer for JsonWriter {
    fn begin(&mut self, cols: &[Col], _: Option<&SchemaRef>) -> Result<()> {
        self.cols = cols
            .iter()
            .map(|c| serde_json::Value::String(c.name.clone()).to_string())
            .collect();
        if let JsonMode::Array { .. } = self.mode {
            self.out.write_all(b"[").map_err(io_err)?;
        }
        Ok(())
    }

    fn row(&mut self, row: &[Value]) -> Result<()> {
        self.line.clear();
        self.line.push('{');
        for (i, (k, v)) in self.cols.iter().zip(row).enumerate() {
            if i > 0 {
                self.line.push(',');
            }
            self.line.push_str(k);
            self.line.push(':');
            match self.mode {
                JsonMode::Canonical => self
                    .line
                    .push_str(&v.to_bson().into_canonical_extjson().to_string()),
                _ => v.write_json(&mut self.line),
            }
        }
        self.line.push('}');
        self.emit()
    }

    fn document(&mut self, doc: &Document) -> Result<bool> {
        let b = Bson::Document(doc.clone());
        let v = match self.mode {
            JsonMode::Canonical => b.into_canonical_extjson(),
            _ => b.into_relaxed_extjson(),
        };
        self.line = v.to_string();
        self.emit()?;
        Ok(true)
    }

    fn finish(mut self: Box<Self>) -> Result<()> {
        if let JsonMode::Array { .. } = self.mode {
            self.out
                .write_all(if self.first { b"]\n" } else { b"\n]\n" })
                .map_err(io_err)?;
        }
        self.out.flush().map_err(io_err)
    }
}

// ------------------------------------------------------------------ Markdown / HTML

struct MarkdownWriter {
    out: BufWriter<File>,
    null: String,
}

fn md_cell(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('|', "\\|")
        .replace("\r\n", "<br>")
        .replace('\n', "<br>")
}

impl Writer for MarkdownWriter {
    fn begin(&mut self, cols: &[Col], _: Option<&SchemaRef>) -> Result<()> {
        if cols.is_empty() {
            return Ok(());
        }
        let head: Vec<String> = cols.iter().map(|c| md_cell(&c.name)).collect();
        let align: Vec<&str> = cols
            .iter()
            .map(|c| match c.ty {
                ValueType::Int | ValueType::Float | ValueType::Decimal => "---:",
                _ => "---",
            })
            .collect();
        writeln!(self.out, "| {} |\n|{}|", head.join(" | "), align.join("|")).map_err(io_err)
    }

    fn row(&mut self, row: &[Value]) -> Result<()> {
        let cells: Vec<String> = row
            .iter()
            .map(|v| md_cell(&v.to_text().unwrap_or_else(|| self.null.clone())))
            .collect();
        writeln!(self.out, "| {} |", cells.join(" | ")).map_err(io_err)
    }

    fn finish(mut self: Box<Self>) -> Result<()> {
        self.out.flush().map_err(io_err)
    }
}

struct HtmlWriter {
    out: BufWriter<File>,
    header: bool,
    null: String,
}

fn html(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&#39;"),
            c => o.push(c),
        }
    }
    o
}

impl Writer for HtmlWriter {
    fn begin(&mut self, cols: &[Col], _: Option<&SchemaRef>) -> Result<()> {
        self.out
            .write_all(
                b"<!DOCTYPE html>\n<html>\n<head>\n<meta charset=\"utf-8\">\n<title>Export</title>\n\
<style>table{border-collapse:collapse;font-family:sans-serif;font-size:13px}\
th,td{border:1px solid #ccc;padding:2px 6px}th{background:#f2f2f2}td.n{text-align:right}\
td.null{color:#999;font-style:italic}</style>\n</head>\n<body>\n<table>\n",
            )
            .map_err(io_err)?;
        if self.header && !cols.is_empty() {
            let th: String = cols
                .iter()
                .map(|c| format!("<th>{}</th>", html(&c.name)))
                .collect();
            writeln!(self.out, "<thead><tr>{th}</tr></thead>").map_err(io_err)?;
        }
        self.out.write_all(b"<tbody>\n").map_err(io_err)
    }

    fn row(&mut self, row: &[Value]) -> Result<()> {
        let mut tr = String::from("<tr>");
        for v in row {
            match v {
                Value::Null => {
                    tr.push_str(&format!("<td class=\"null\">{}</td>", html(&self.null)))
                }
                Value::Int(_) | Value::UInt(_) | Value::Float(_) | Value::Decimal(_) => tr
                    .push_str(&format!(
                        "<td class=\"n\">{}</td>",
                        html(&v.to_text().unwrap_or_default())
                    )),
                v => tr.push_str(&format!(
                    "<td>{}</td>",
                    html(&v.to_text().unwrap_or_default())
                )),
            }
        }
        tr.push_str("</tr>\n");
        self.out.write_all(tr.as_bytes()).map_err(io_err)
    }

    fn finish(mut self: Box<Self>) -> Result<()> {
        self.out
            .write_all(b"</tbody>\n</table>\n</body>\n</html>\n")
            .map_err(io_err)?;
        self.out.flush().map_err(io_err)
    }
}

// ------------------------------------------------------------------ SQL INSERT

struct InsertWriter {
    out: BufWriter<File>,
    sql: Sql,
    table: String,
    cols: Vec<String>,
    rows: Vec<Vec<Value>>,
    per: usize,
}

impl InsertWriter {
    fn flush_rows(&mut self) -> Result<()> {
        if self.rows.is_empty() {
            return Ok(());
        }
        let table = self.sql.ident(&self.table);
        let stmt = self.sql.insert(&table, &self.cols, &self.rows);
        self.rows.clear();
        writeln!(self.out, "{stmt};").map_err(io_err)
    }
}

impl Writer for InsertWriter {
    fn begin(&mut self, cols: &[Col], _: Option<&SchemaRef>) -> Result<()> {
        self.cols = cols.iter().map(|c| c.name.clone()).collect();
        Ok(())
    }

    fn row(&mut self, row: &[Value]) -> Result<()> {
        self.rows.push(row.to_vec());
        if self.rows.len() >= self.per {
            self.flush_rows()?;
        }
        Ok(())
    }

    fn finish(mut self: Box<Self>) -> Result<()> {
        self.flush_rows()?;
        self.out.flush().map_err(io_err)
    }
}

// ------------------------------------------------------------------ XLSX

/// Excel's last row (1-based 1,048,576); a new sheet starts after it.
const XLSX_ROWS: u32 = 1_048_576;
/// Excel's limit on cell text.
const XLSX_TEXT: usize = 32_767;
/// Days from 1899-12-30 (Excel's day 0) to 1970-01-01.
const EXCEL_EPOCH: f64 = 25_569.0;

struct XlsxWriter {
    path: PathBuf,
    book: rust_xlsxwriter::Workbook,
    sheets: usize,
    row: u32,
    header: bool,
    names: Vec<String>,
    bold: rust_xlsxwriter::Format,
    date: rust_xlsxwriter::Format,
    datetime: rust_xlsxwriter::Format,
    time: rust_xlsxwriter::Format,
}

fn xerr(e: rust_xlsxwriter::XlsxError) -> Error {
    io_err(e)
}

impl XlsxWriter {
    fn new(path: &Path, header: bool) -> Result<Self> {
        use rust_xlsxwriter::Format;
        Ok(Self {
            path: path.to_owned(),
            book: rust_xlsxwriter::Workbook::new(),
            sheets: 0,
            row: 0,
            header,
            names: Vec::new(),
            bold: Format::new().set_bold(),
            date: Format::new().set_num_format("yyyy-mm-dd"),
            datetime: Format::new().set_num_format("yyyy-mm-dd hh:mm:ss"),
            time: Format::new().set_num_format("hh:mm:ss"),
        })
    }

    fn new_sheet(&mut self) -> Result<()> {
        let ws = self
            .book
            .add_worksheet_with_constant_memory()
            .map_err(xerr)?;
        ws.set_name(format!(
            "Data{}",
            if self.sheets == 0 {
                String::new()
            } else {
                format!(" {}", self.sheets + 1)
            }
        ))
        .map_err(xerr)?;
        self.sheets += 1;
        self.row = 0;
        if self.header && !self.names.is_empty() {
            for (c, n) in self.names.iter().enumerate() {
                ws.write_string_with_format(0, c as u16, n, &self.bold)
                    .map_err(xerr)?;
            }
            ws.set_freeze_panes(1, 0).map_err(xerr)?;
            self.row = 1;
        }
        Ok(())
    }
}

impl Writer for XlsxWriter {
    fn begin(&mut self, cols: &[Col], _: Option<&SchemaRef>) -> Result<()> {
        self.names = cols.iter().map(|c| c.name.clone()).collect();
        self.new_sheet()
    }

    fn row(&mut self, row: &[Value]) -> Result<()> {
        if self.row >= XLSX_ROWS {
            self.new_sheet()?;
        }
        let r = self.row;
        let ws = self
            .book
            .worksheet_from_index(self.sheets - 1)
            .map_err(xerr)?;
        for (c, v) in row.iter().enumerate() {
            let c = c as u16;
            match v {
                Value::Null => {}
                Value::Bool(b) => {
                    ws.write_boolean(r, c, *b).map_err(xerr)?;
                }
                Value::Int(i) if i.unsigned_abs() < (1 << 53) => {
                    ws.write_number(r, c, *i as f64).map_err(xerr)?;
                }
                Value::Float(f) if f.is_finite() => {
                    ws.write_number(r, c, *f).map_err(xerr)?;
                }
                Value::Decimal(d) if d.trim_start_matches('-').replace('.', "").len() <= 15 => {
                    match d.parse::<f64>() {
                        Ok(f) => ws.write_number(r, c, f).map_err(xerr)?,
                        Err(_) => ws.write_string(r, c, d).map_err(xerr)?,
                    };
                }
                Value::Date(d) => {
                    ws.write_number_with_format(r, c, f64::from(*d) + EXCEL_EPOCH, &self.date)
                        .map_err(xerr)?;
                }
                Value::Timestamp { micros, .. } => {
                    let days = *micros as f64 / 86_400_000_000.0 + EXCEL_EPOCH;
                    ws.write_number_with_format(r, c, days, &self.datetime)
                        .map_err(xerr)?;
                }
                Value::Time(t) => {
                    ws.write_number_with_format(r, c, *t as f64 / 86_400_000_000.0, &self.time)
                        .map_err(xerr)?;
                }
                other => {
                    let mut s = other.to_text().unwrap_or_default();
                    if s.len() > XLSX_TEXT {
                        let mut end = XLSX_TEXT;
                        while !s.is_char_boundary(end) {
                            end -= 1;
                        }
                        s.truncate(end);
                    }
                    ws.write_string(r, c, &s).map_err(xerr)?;
                }
            }
        }
        self.row += 1;
        Ok(())
    }

    fn finish(mut self: Box<Self>) -> Result<()> {
        if self.sheets == 0 {
            self.new_sheet()?;
        }
        let path = self.path.clone();
        self.book.save(&path).map_err(xerr)
    }
}

// ------------------------------------------------------------------ Parquet

/// Rows buffered before a record batch is built (documents → Parquet).
const PARQUET_ROWS: usize = 8192;

struct ParquetWriter {
    file: Option<File>,
    writer: Option<parquet::arrow::ArrowWriter<File>>,
    schema: Option<SchemaRef>,
    rows: Vec<Vec<Value>>,
}

fn arrow_type(ty: ValueType) -> DataType {
    match ty {
        ValueType::Bool => DataType::Boolean,
        ValueType::Int => DataType::Int64,
        ValueType::Float => DataType::Float64,
        ValueType::Bytes => DataType::Binary,
        ValueType::Date => DataType::Date32,
        ValueType::Time => DataType::Time64(TimeUnit::Microsecond),
        ValueType::Timestamp { tz } => {
            DataType::Timestamp(TimeUnit::Microsecond, tz.then(|| Arc::from("UTC")))
        }
        ValueType::Decimal | ValueType::Text | ValueType::Json | ValueType::Any => DataType::Utf8,
    }
}

/// Builds a record batch from rows, converting each value to its column's
/// type (a value that does not convert becomes NULL).
pub fn rows_to_batch(schema: &SchemaRef, rows: &[Vec<Value>]) -> Result<RecordBatch> {
    let mut arrays: Vec<ArrayRef> = Vec::with_capacity(schema.fields().len());
    for (c, f) in schema.fields().iter().enumerate() {
        let ty = ValueType::from_arrow(f.data_type());
        let cell = |r: &Vec<Value>| -> Value {
            r.get(c)
                .cloned()
                .unwrap_or(Value::Null)
                .convert(ty)
                .unwrap_or(Value::Null)
        };
        let arr: ArrayRef = match f.data_type() {
            DataType::Boolean => {
                let mut b = BooleanBuilder::with_capacity(rows.len());
                for r in rows {
                    match cell(r) {
                        Value::Bool(v) => b.append_value(v),
                        _ => b.append_null(),
                    }
                }
                Arc::new(b.finish())
            }
            DataType::Int64 => {
                let mut b = Int64Builder::with_capacity(rows.len());
                for r in rows {
                    match cell(r) {
                        Value::Int(v) => b.append_value(v),
                        _ => b.append_null(),
                    }
                }
                Arc::new(b.finish())
            }
            DataType::Float64 => {
                let mut b = Float64Builder::with_capacity(rows.len());
                for r in rows {
                    match cell(r) {
                        Value::Float(v) => b.append_value(v),
                        _ => b.append_null(),
                    }
                }
                Arc::new(b.finish())
            }
            DataType::Binary => {
                let mut b = BinaryBuilder::new();
                for r in rows {
                    match cell(r) {
                        Value::Bytes(v) => b.append_value(v),
                        _ => b.append_null(),
                    }
                }
                Arc::new(b.finish())
            }
            DataType::Date32 => {
                let mut b = Date32Builder::with_capacity(rows.len());
                for r in rows {
                    match cell(r) {
                        Value::Date(v) => b.append_value(v),
                        _ => b.append_null(),
                    }
                }
                Arc::new(b.finish())
            }
            DataType::Time64(_) => {
                let mut b = Time64MicrosecondBuilder::with_capacity(rows.len());
                for r in rows {
                    match cell(r) {
                        Value::Time(v) => b.append_value(v),
                        _ => b.append_null(),
                    }
                }
                Arc::new(b.finish())
            }
            DataType::Timestamp(_, tz) => {
                let mut b = TimestampMicrosecondBuilder::with_capacity(rows.len());
                for r in rows {
                    match cell(r) {
                        Value::Timestamp { micros, .. } => b.append_value(micros),
                        _ => b.append_null(),
                    }
                }
                let a = b.finish();
                Arc::new(match tz {
                    Some(tz) => a.with_timezone(tz.clone()),
                    None => a,
                })
            }
            _ => {
                let mut b = StringBuilder::new();
                for r in rows {
                    match r.get(c) {
                        None | Some(Value::Null) => b.append_null(),
                        Some(v) => b.append_value(v.to_text().unwrap_or_default()),
                    }
                }
                Arc::new(b.finish())
            }
        };
        arrays.push(arr);
    }
    RecordBatch::try_new(schema.clone(), arrays).map_err(io_err)
}

impl ParquetWriter {
    fn open(&mut self, schema: SchemaRef) -> Result<()> {
        let file = self
            .file
            .take()
            .ok_or_else(|| io_err("file already used"))?;
        let props = parquet::file::properties::WriterProperties::builder()
            .set_compression(parquet::basic::Compression::SNAPPY)
            .build();
        self.writer = Some(
            parquet::arrow::ArrowWriter::try_new(file, schema.clone(), Some(props))
                .map_err(io_err)?,
        );
        self.schema = Some(schema);
        Ok(())
    }

    fn flush_rows(&mut self) -> Result<()> {
        if self.rows.is_empty() {
            return Ok(());
        }
        let schema = self.schema.clone().ok_or_else(|| io_err("no schema"))?;
        let batch = rows_to_batch(&schema, &self.rows)?;
        self.rows.clear();
        self.writer
            .as_mut()
            .ok_or_else(|| io_err("no writer"))?
            .write(&batch)
            .map_err(io_err)
    }
}

/// Parquet has no unsigned 64-bit decimals or views we can't read back
/// everywhere; drop the driver's field metadata, keep names and types.
fn plain_schema(schema: &SchemaRef) -> SchemaRef {
    Arc::new(Schema::new(
        schema
            .fields()
            .iter()
            .map(|f| Field::new(f.name(), f.data_type().clone(), true))
            .collect::<Vec<_>>(),
    ))
}

impl Writer for ParquetWriter {
    fn begin(&mut self, cols: &[Col], schema: Option<&SchemaRef>) -> Result<()> {
        let schema = match schema {
            Some(s) => plain_schema(s),
            None => Arc::new(Schema::new(
                cols.iter()
                    .map(|c| Field::new(&c.name, arrow_type(c.ty), true))
                    .collect::<Vec<_>>(),
            )),
        };
        self.open(schema)
    }

    fn row(&mut self, row: &[Value]) -> Result<()> {
        self.rows.push(row.to_vec());
        if self.rows.len() >= PARQUET_ROWS {
            self.flush_rows()?;
        }
        Ok(())
    }

    fn batch(&mut self, batch: &RecordBatch) -> Result<()> {
        let schema = self.schema.clone().ok_or_else(|| io_err("no schema"))?;
        let b = RecordBatch::try_new(schema, batch.columns().to_vec()).map_err(io_err)?;
        self.writer
            .as_mut()
            .ok_or_else(|| io_err("no writer"))?
            .write(&b)
            .map_err(io_err)
    }

    fn finish(mut self: Box<Self>) -> Result<()> {
        if self.writer.is_none() {
            self.open(Arc::new(Schema::empty()))?;
        }
        self.flush_rows()?;
        if let Some(w) = self.writer.take() {
            w.close().map_err(io_err)?;
        }
        Ok(())
    }
}
