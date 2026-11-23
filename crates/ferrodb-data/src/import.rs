//! Import readers: CSV / TSV, XLSX, Parquet, JSON (array or lines),
//! JSON Lines and Extended JSON. A reader yields rows in batches (or
//! documents, for document targets); a [`Mapping`] then converts each row
//! to the columns and types of the target.
//!
//! CSV / TSV, Parquet and JSON Lines stream. A JSON array and an XLSX
//! sheet are read whole (the formats do not allow otherwise without a
//! streaming parser; acceptable for spreadsheet-sized files).

use std::{
    collections::VecDeque,
    fs::File,
    io::{BufRead, BufReader, Read},
    path::Path,
};

use arrow_array::RecordBatch;
use bson::{Bson, Document};
use calamine::Reader as _;
use ferrodb_api::{ColumnMapping, DataFormat, Error, FilePreview, FormatOptions, Result};

use crate::{
    docs::{DocColumn, document_to_row, infer_columns, row_to_document},
    value::{Value, ValueType, guess_type, value_at},
};

/// Rows read ahead to guess column types.
const SAMPLE_ROWS: usize = 1000;

fn err(e: impl std::fmt::Display) -> Error {
    Error::Other(format!("import: {e}"))
}

#[derive(Clone, Debug, PartialEq)]
pub struct SourceColumn {
    pub name: String,
    pub ty: ValueType,
}

pub trait RowReader: Send {
    fn columns(&self) -> &[SourceColumn];
    /// Up to `max` rows; empty = end of input.
    fn next_rows(&mut self, max: usize) -> Result<Vec<Vec<Value>>>;
    /// Up to `max` documents (document targets). Tabular formats build
    /// them from rows (dotted column names nest).
    fn next_documents(&mut self, max: usize) -> Result<Vec<Document>> {
        let names: Vec<String> = self.columns().iter().map(|c| c.name.clone()).collect();
        Ok(self
            .next_rows(max)?
            .iter()
            .map(|r| row_to_document(&names, r))
            .collect())
    }
}

pub fn open(path: &Path, format: DataFormat, opts: &FormatOptions) -> Result<Box<dyn RowReader>> {
    Ok(match format {
        DataFormat::Csv | DataFormat::Tsv => {
            Box::new(CsvReader::new(open_file(path)?, format, opts)?)
        }
        DataFormat::Xlsx => Box::new(XlsxReader::open(path, opts)?),
        DataFormat::Parquet => Box::new(ParquetReader::open(path)?),
        DataFormat::Json | DataFormat::JsonLines | DataFormat::ExtendedJson => {
            Box::new(JsonReader::new(open_file(path)?)?)
        }
        other => {
            return Err(Error::Unsupported(format!(
                "importing {} files",
                other.label()
            )));
        }
    })
}

/// A reader over bytes in memory (pasted text, tests, fuzzing): CSV / TSV
/// and the JSON formats.
pub fn from_bytes(
    data: Vec<u8>,
    format: DataFormat,
    opts: &FormatOptions,
) -> Result<Box<dyn RowReader>> {
    let input: Input = Box::new(std::io::Cursor::new(data));
    Ok(match format {
        DataFormat::Csv | DataFormat::Tsv => Box::new(CsvReader::new(input, format, opts)?),
        DataFormat::Json | DataFormat::JsonLines | DataFormat::ExtendedJson => {
            Box::new(JsonReader::new(input)?)
        }
        other => {
            return Err(Error::Unsupported(format!(
                "importing {} from memory",
                other.label()
            )));
        }
    })
}

/// The byte source of the streaming readers.
type Input = Box<dyn Read + Send>;

fn open_file(path: &Path) -> Result<Input> {
    Ok(Box::new(File::open(path).map_err(err)?))
}

/// The first `rows` rows of a file with guessed column types.
pub fn preview(
    path: &Path,
    format: DataFormat,
    opts: &FormatOptions,
    rows: usize,
) -> Result<FilePreview> {
    let mut r = open(path, format, opts)?;
    let data = r.next_rows(rows)?;
    Ok(FilePreview {
        columns: r.columns().iter().map(|c| c.name.clone()).collect(),
        types: r
            .columns()
            .iter()
            .map(|c| c.ty.label().to_owned())
            .collect(),
        rows: data
            .into_iter()
            .map(|row| row.iter().map(Value::to_text).collect())
            .collect(),
    })
}

// ------------------------------------------------------------------ CSV

struct CsvReader {
    rd: csv::Reader<BufReader<Input>>,
    cols: Vec<SourceColumn>,
    ahead: VecDeque<csv::StringRecord>,
    null: String,
    done: bool,
}

impl CsvReader {
    fn new(input: Input, format: DataFormat, opts: &FormatOptions) -> Result<Self> {
        let delim = crate::export::csv_delimiter(format, opts)?;
        let mut rd = csv::ReaderBuilder::new()
            .delimiter(delim)
            .has_headers(opts.header)
            .flexible(true)
            .from_reader(BufReader::with_capacity(256 * 1024, input));
        let headers: Vec<String> = if opts.header {
            rd.headers()
                .map_err(err)?
                .iter()
                .map(|h| h.trim_start_matches('\u{feff}').to_owned())
                .collect()
        } else {
            Vec::new()
        };
        let mut ahead = VecDeque::new();
        let mut done = false;
        let mut rec = csv::StringRecord::new();
        while ahead.len() < SAMPLE_ROWS {
            if !rd.read_record(&mut rec).map_err(err)? {
                done = true;
                break;
            }
            ahead.push_back(rec.clone());
        }
        let width = ahead
            .iter()
            .map(csv::StringRecord::len)
            .max()
            .unwrap_or(0)
            .max(headers.len());
        let null = opts.null_text.clone();
        let cols = (0..width)
            .map(|i| SourceColumn {
                name: headers
                    .get(i)
                    .filter(|h| !h.is_empty())
                    .cloned()
                    .unwrap_or_else(|| format!("column{}", i + 1)),
                ty: guess_type(ahead.iter().filter_map(|r| r.get(i)).filter(|s| *s != null)),
            })
            .collect();
        Ok(Self {
            rd,
            cols,
            ahead,
            null,
            done,
        })
    }

    fn convert(&self, rec: &csv::StringRecord) -> Vec<Value> {
        self.cols
            .iter()
            .enumerate()
            .map(|(i, c)| match rec.get(i) {
                None => Value::Null,
                Some(s) if s == self.null || (self.null.is_empty() && s.is_empty()) => Value::Null,
                Some(s) => Value::Text(s.to_owned())
                    .convert(c.ty)
                    .unwrap_or_else(|_| Value::Text(s.to_owned())),
            })
            .collect()
    }
}

impl RowReader for CsvReader {
    fn columns(&self) -> &[SourceColumn] {
        &self.cols
    }

    fn next_rows(&mut self, max: usize) -> Result<Vec<Vec<Value>>> {
        let mut out = Vec::with_capacity(max.min(4096));
        while out.len() < max {
            if let Some(r) = self.ahead.pop_front() {
                out.push(self.convert(&r));
                continue;
            }
            if self.done {
                break;
            }
            let mut rec = csv::StringRecord::new();
            if !self.rd.read_record(&mut rec).map_err(err)? {
                self.done = true;
                break;
            }
            out.push(self.convert(&rec));
        }
        Ok(out)
    }
}

// ------------------------------------------------------------------ XLSX

struct XlsxReader {
    cols: Vec<SourceColumn>,
    rows: VecDeque<Vec<Value>>,
}

/// Days from 1899-12-30 (Excel's day 0) to 1970-01-01.
const EXCEL_EPOCH: f64 = 25_569.0;

fn xlsx_value(d: &calamine::Data) -> Value {
    use calamine::Data;
    match d {
        Data::Empty | Data::Error(_) => Value::Null,
        Data::Int(i) => Value::Int(*i),
        Data::Float(f) if f.fract() == 0.0 && f.abs() < 9.0e15 => Value::Int(*f as i64),
        Data::Float(f) => Value::Float(*f),
        Data::String(s) => Value::Text(s.clone()),
        Data::Bool(b) => Value::Bool(*b),
        Data::DateTime(dt) => {
            let serial = dt.as_f64();
            if dt.is_duration() {
                Value::Time((serial.fract() * 86_400_000_000.0).round() as i64)
            } else if serial.fract() == 0.0 {
                Value::Date((serial - EXCEL_EPOCH) as i32)
            } else {
                Value::Timestamp {
                    micros: ((serial - EXCEL_EPOCH) * 86_400_000_000.0).round() as i64,
                    tz: false,
                }
            }
        }
        Data::DateTimeIso(s) | Data::DurationIso(s) => Value::Text(s.clone()),
    }
}

impl XlsxReader {
    fn open(path: &Path, opts: &FormatOptions) -> Result<Self> {
        let mut book = calamine::open_workbook_auto(path).map_err(err)?;
        let sheet = match &opts.sheet {
            Some(s) => s.clone(),
            None => book
                .sheet_names()
                .first()
                .cloned()
                .ok_or_else(|| err("the workbook has no sheets"))?,
        };
        let range = book.worksheet_range(&sheet).map_err(err)?;
        let mut it = range.rows();
        let headers: Vec<String> = if opts.header {
            it.next()
                .map(|r| {
                    r.iter()
                        .map(|c| xlsx_value(c).to_text().unwrap_or_default())
                        .collect()
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let rows: VecDeque<Vec<Value>> = it.map(|r| r.iter().map(xlsx_value).collect()).collect();
        let width = rows
            .iter()
            .map(Vec::len)
            .max()
            .unwrap_or(0)
            .max(headers.len());
        let cols = (0..width)
            .map(|i| {
                let mut ty: Option<ValueType> = None;
                for r in rows.iter().take(SAMPLE_ROWS) {
                    let t = match r.get(i) {
                        None | Some(Value::Null) => continue,
                        Some(Value::Int(_)) => ValueType::Int,
                        Some(Value::Float(_)) => ValueType::Float,
                        Some(Value::Bool(_)) => ValueType::Bool,
                        Some(Value::Date(_)) => ValueType::Date,
                        Some(Value::Timestamp { .. }) => ValueType::Timestamp { tz: false },
                        Some(Value::Time(_)) => ValueType::Time,
                        Some(_) => ValueType::Text,
                    };
                    ty = Some(match (ty, t) {
                        (None, t) => t,
                        (Some(a), b) if a == b => a,
                        (Some(ValueType::Int), ValueType::Float)
                        | (Some(ValueType::Float), ValueType::Int) => ValueType::Float,
                        (Some(ValueType::Date), ValueType::Timestamp { .. })
                        | (Some(ValueType::Timestamp { .. }), ValueType::Date) => {
                            ValueType::Timestamp { tz: false }
                        }
                        _ => ValueType::Text,
                    });
                }
                SourceColumn {
                    name: headers
                        .get(i)
                        .filter(|h| !h.is_empty())
                        .cloned()
                        .unwrap_or_else(|| format!("column{}", i + 1)),
                    ty: ty.unwrap_or(ValueType::Text),
                }
            })
            .collect();
        Ok(Self { cols, rows })
    }
}

impl RowReader for XlsxReader {
    fn columns(&self) -> &[SourceColumn] {
        &self.cols
    }

    fn next_rows(&mut self, max: usize) -> Result<Vec<Vec<Value>>> {
        let n = max.min(self.rows.len());
        Ok(self
            .rows
            .drain(..n)
            .map(|mut r| {
                r.resize(self.cols.len(), Value::Null);
                r.into_iter()
                    .zip(&self.cols)
                    .map(|(v, c)| match (&v, c.ty) {
                        (Value::Int(_) | Value::Float(_), ValueType::Text) => v,
                        _ => v.clone().convert(c.ty).unwrap_or(v),
                    })
                    .collect()
            })
            .collect())
    }
}

// ------------------------------------------------------------------ Parquet

struct ParquetReader {
    rd: parquet::arrow::arrow_reader::ParquetRecordBatchReader,
    cols: Vec<SourceColumn>,
    current: Option<(RecordBatch, usize)>,
}

impl ParquetReader {
    fn open(path: &Path) -> Result<Self> {
        let file = File::open(path).map_err(err)?;
        let builder = parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(file)
            .map_err(err)?;
        let cols = builder
            .schema()
            .fields()
            .iter()
            .map(|f| SourceColumn {
                name: f.name().clone(),
                ty: ValueType::from_arrow(f.data_type()),
            })
            .collect();
        let rd = builder.with_batch_size(1024).build().map_err(err)?;
        Ok(Self {
            rd,
            cols,
            current: None,
        })
    }
}

impl RowReader for ParquetReader {
    fn columns(&self) -> &[SourceColumn] {
        &self.cols
    }

    fn next_rows(&mut self, max: usize) -> Result<Vec<Vec<Value>>> {
        let mut out = Vec::with_capacity(max.min(4096));
        while out.len() < max {
            let (batch, pos) = match self.current.take() {
                Some(c) => c,
                None => match self.rd.next() {
                    Some(b) => (b.map_err(err)?, 0),
                    None => break,
                },
            };
            let end = batch.num_rows().min(pos + (max - out.len()));
            for r in pos..end {
                out.push(
                    batch
                        .columns()
                        .iter()
                        .map(|c| value_at(c.as_ref(), r))
                        .collect(),
                );
            }
            if end < batch.num_rows() {
                self.current = Some((batch, end));
            }
        }
        Ok(out)
    }
}

// ------------------------------------------------------------------ JSON

struct JsonReader {
    /// JSON Lines input (streamed); `None` for a JSON array.
    lines: Option<std::io::Lines<BufReader<Input>>>,
    ahead: VecDeque<Document>,
    cols: Vec<SourceColumn>,
    doc_cols: Vec<DocColumn>,
    line_no: usize,
}

fn to_document(v: serde_json::Value, at: &str) -> Result<Document> {
    match Bson::try_from(v).map_err(|e| err(format!("{at}: {e}")))? {
        Bson::Document(d) => Ok(d),
        other => {
            let mut d = Document::new();
            d.insert("value", other);
            Ok(d)
        }
    }
}

impl JsonReader {
    fn new(input: Input) -> Result<Self> {
        let mut file = BufReader::new(input);
        // First significant byte decides: `[` = array, else lines.
        let first = loop {
            let buf = file.fill_buf().map_err(err)?;
            if buf.is_empty() {
                break None;
            }
            match buf
                .iter()
                .position(|b| !b.is_ascii_whitespace() && *b != 0xEF && *b != 0xBB && *b != 0xBF)
            {
                Some(i) => break Some(buf[i]),
                None => {
                    let n = buf.len();
                    file.consume(n);
                }
            }
        };
        let mut me = Self {
            lines: None,
            ahead: VecDeque::new(),
            cols: Vec::new(),
            doc_cols: Vec::new(),
            line_no: 0,
        };
        if first == Some(b'[') {
            let mut text = String::new();
            file.read_to_string(&mut text).map_err(err)?;
            let v: serde_json::Value = serde_json::from_str(&text).map_err(err)?;
            let serde_json::Value::Array(items) = v else {
                return Err(err("expected a JSON array"));
            };
            for (i, item) in items.into_iter().enumerate() {
                me.ahead
                    .push_back(to_document(item, &format!("element {}", i + 1))?);
            }
        } else {
            me.lines = Some(file.lines());
            me.fill(SAMPLE_ROWS)?;
        }
        me.doc_cols = infer_columns(me.ahead.iter().take(SAMPLE_ROWS));
        me.cols = me
            .doc_cols
            .iter()
            .map(|c| SourceColumn {
                name: c.path.clone(),
                ty: c.ty,
            })
            .collect();
        Ok(me)
    }

    fn fill(&mut self, want: usize) -> Result<()> {
        let Some(lines) = self.lines.as_mut() else {
            return Ok(());
        };
        while self.ahead.len() < want {
            let Some(line) = lines.next() else { break };
            self.line_no += 1;
            let line = line.map_err(err)?;
            let t = line.trim().trim_start_matches('\u{feff}');
            if t.is_empty() {
                continue;
            }
            let v: serde_json::Value =
                serde_json::from_str(t).map_err(|e| err(format!("line {}: {e}", self.line_no)))?;
            self.ahead
                .push_back(to_document(v, &format!("line {}", self.line_no))?);
        }
        Ok(())
    }
}

impl RowReader for JsonReader {
    fn columns(&self) -> &[SourceColumn] {
        &self.cols
    }

    fn next_rows(&mut self, max: usize) -> Result<Vec<Vec<Value>>> {
        Ok(self
            .next_documents(max)?
            .iter()
            .map(|d| document_to_row(d, &self.doc_cols))
            .collect())
    }

    fn next_documents(&mut self, max: usize) -> Result<Vec<Document>> {
        self.fill(max)?;
        let n = max.min(self.ahead.len());
        Ok(self.ahead.drain(..n).collect())
    }
}

// ------------------------------------------------------------------ mapping

/// Source columns → target columns with conversion to the target types.
#[derive(Clone, Debug, PartialEq)]
pub struct Mapping {
    /// Target columns written (name, type).
    pub targets: Vec<(String, ValueType)>,
    /// Per target column: the source column index.
    pub sources: Vec<usize>,
}

impl Mapping {
    /// `explicit` empty = by name, ignoring case; target columns without a
    /// source are left to their defaults.
    pub fn new(
        source: &[SourceColumn],
        target: &[(String, ValueType)],
        explicit: &[ColumnMapping],
    ) -> Result<Self> {
        let find_src = |name: &str| {
            source.iter().position(|c| c.name == name).or_else(|| {
                source
                    .iter()
                    .position(|c| c.name.eq_ignore_ascii_case(name))
            })
        };
        let find_tgt = |name: &str| {
            target
                .iter()
                .position(|c| c.0 == name)
                .or_else(|| target.iter().position(|c| c.0.eq_ignore_ascii_case(name)))
        };
        let mut m = Mapping {
            targets: Vec::new(),
            sources: Vec::new(),
        };
        if explicit.is_empty() {
            for (name, ty) in target {
                if let Some(s) = find_src(name) {
                    m.targets.push((name.clone(), *ty));
                    m.sources.push(s);
                }
            }
        } else {
            for cm in explicit {
                let Some(src) = &cm.source else { continue };
                let s = find_src(src).ok_or_else(|| err(format!("no source column `{src}`")))?;
                let t = find_tgt(&cm.target)
                    .ok_or_else(|| err(format!("no target column `{}`", cm.target)))?;
                m.targets.push(target[t].clone());
                m.sources.push(s);
            }
        }
        if m.targets.is_empty() {
            return Err(err(
                "no source column matches a target column; map the columns explicitly",
            ));
        }
        Ok(m)
    }

    /// Identity mapping: every source column, its own type.
    pub fn identity(source: &[SourceColumn]) -> Self {
        Self {
            targets: source.iter().map(|c| (c.name.clone(), c.ty)).collect(),
            sources: (0..source.len()).collect(),
        }
    }

    pub fn names(&self) -> Vec<String> {
        self.targets.iter().map(|t| t.0.clone()).collect()
    }

    /// Converts one source row. Errors name the column.
    pub fn row(&self, src: &[Value]) -> std::result::Result<Vec<Value>, String> {
        self.sources
            .iter()
            .zip(&self.targets)
            .map(|(&s, (name, ty))| {
                src.get(s)
                    .cloned()
                    .unwrap_or(Value::Null)
                    .convert(*ty)
                    .map_err(|e| format!("column {name}: {e}"))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::*;

    fn write(dir: &tempfile::TempDir, name: &str, text: &str) -> std::path::PathBuf {
        let p = dir.path().join(name);
        std::fs::File::create(&p)
            .unwrap()
            .write_all(text.as_bytes())
            .unwrap();
        p
    }

    #[test]
    fn csv_with_types() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(
            &dir,
            "a.csv",
            "\u{feff}id,name,born,score\n1,\"Kowalski, Jan\",1990-05-01,4.5\n2,Anna,,\n",
        );
        let opts = FormatOptions::default();
        let mut r = open(&p, DataFormat::Csv, &opts).unwrap();
        let cols: Vec<_> = r
            .columns()
            .iter()
            .map(|c| (c.name.as_str(), c.ty))
            .collect();
        assert_eq!(
            cols,
            vec![
                ("id", ValueType::Int),
                ("name", ValueType::Text),
                ("born", ValueType::Date),
                ("score", ValueType::Float)
            ]
        );
        let rows = r.next_rows(10).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0][1], Value::Text("Kowalski, Jan".into()));
        assert_eq!(rows[1][2], Value::Null);
        assert!(r.next_rows(10).unwrap().is_empty());

        let target = vec![
            ("ID".to_owned(), ValueType::Text),
            ("score".to_owned(), ValueType::Decimal),
            ("other".to_owned(), ValueType::Int),
        ];
        let m = Mapping::new(r.columns(), &target, &[]).unwrap();
        assert_eq!(m.names(), vec!["ID", "score"]);
        assert_eq!(
            m.row(&rows[0]).unwrap(),
            vec![Value::Text("1".into()), Value::Decimal("4.5".into())]
        );
    }

    #[test]
    fn json_lines_and_array() {
        let dir = tempfile::tempdir().unwrap();
        let lines = write(
            &dir,
            "a.jsonl",
            "{\"_id\":{\"$oid\":\"65a1b2c3d4e5f60718293a4b\"},\"n\":1}\n\n{\"n\":2,\"tags\":[\"x\"]}\n",
        );
        let mut r = open(&lines, DataFormat::JsonLines, &FormatOptions::default()).unwrap();
        let docs = r.next_documents(10).unwrap();
        assert_eq!(docs.len(), 2);
        assert!(matches!(docs[0].get("_id"), Some(Bson::ObjectId(_))));
        let arr = write(
            &dir,
            "b.json",
            " [ {\"a\": 1}, {\"a\": null, \"b\": true} ]",
        );
        let p = preview(&arr, DataFormat::Json, &FormatOptions::default(), 5).unwrap();
        assert_eq!(p.columns, vec!["a", "b"]);
        assert_eq!(p.rows[1], vec![None, Some("true".into())]);
    }
}
